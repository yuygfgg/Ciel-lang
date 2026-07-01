use super::*;

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn gen_expr(&mut self, expr: &TExpr) -> DiagResult<String> {
        self.gen_expr_with_lowering(expr, None)
    }

    pub(in crate::codegen) fn gen_expr_in_stmt(
        &mut self,
        expr: &TExpr,
        indent: usize,
    ) -> DiagResult<String> {
        self.with_temporary_resource_cleanup_scope(|this| {
            this.gen_expr_with_lowering(expr, Some(indent))
        })
    }

    pub(super) fn gen_call_args(
        &mut self,
        args: &[TExpr],
        stmt_indent: Option<usize>,
    ) -> DiagResult<Vec<String>> {
        if args.iter().any(|arg| arg.ty.is_erased_value()) {
            let Some(indent) = stmt_indent else {
                return Err(vec![Diagnostic::new(
                    args.iter()
                        .find(|arg| arg.ty.is_erased_value())
                        .map(|arg| arg.span),
                    "erased void argument needs statement lowering",
                )]);
            };
            let mut out = Vec::new();
            for arg in args {
                let value = self.gen_expr_in_stmt(arg, indent)?;
                if arg.ty.is_erased_value() {
                    self.line_indent(indent, &format!("(void)({value});"));
                } else {
                    let temp = self.emit_temp_value("call_arg", arg, indent)?;
                    out.push(temp);
                }
            }
            return Ok(out);
        }

        let mut out = Vec::new();
        for arg in args {
            let value = self.gen_expr_with_lowering(arg, stmt_indent)?;
            out.push(value);
        }
        Ok(out)
    }

    pub(super) fn gen_expr_with_lowering(
        &mut self,
        expr: &TExpr,
        stmt_indent: Option<usize>,
    ) -> DiagResult<String> {
        let code = match &expr.kind {
            TExprKind::Local(local_id, name) => {
                if expr.ty.is_erased_value() {
                    return Ok("((void)0)".to_string());
                }
                if let Some(captured) = self.current_capture_locals.get(local_id) {
                    captured.clone()
                } else {
                    let cname = self.local_c_name(*local_id, name);
                    if self.local_is_heap(*local_id) {
                        format!("(*{cname})")
                    } else {
                        cname
                    }
                }
            }
            TExprKind::Move(inner) => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "resource move needs statement lowering",
                    )]);
                };
                if inner.ty.is_erased_value() {
                    let value = self.gen_expr_in_stmt(inner, indent)?;
                    self.line_indent(indent, &format!("(void)({value});"));
                    return Ok("((void)0)".to_string());
                }
                let source = self.gen_expr_in_stmt(inner, indent)?;
                let temp = self.next_temp("resource_move");
                self.line_indent(indent, &format!("{};", self.c_decl(&inner.ty, &temp)));
                self.emit_value_copy(&temp, &source, &inner.ty, indent);
                self.emit_resource_zero_expr(&inner.ty, &source, indent);
                self.push_temporary_resource_cleanup_defer(&inner.ty, &temp);
                temp
            }
            TExprKind::Function(def_id, name) => self
                .plan
                .name_map
                .get(def_id)
                .cloned()
                .unwrap_or_else(|| name.clone()),
            TExprKind::GenericFunction { name, .. } => {
                return Err(vec![Diagnostic::new(
                    expr.span,
                    format!(
                        "internal error: unmonomorphized generic function `{name}` reached C codegen"
                    ),
                )]);
            }
            TExprKind::Literal(literal) => self.gen_literal(expr.span, literal, &expr.ty),
            TExprKind::StructLiteral { type_name, fields } => {
                let mut emitted_fields = Vec::new();
                for (name, value) in fields {
                    let value_code = self.value_initializer_for_checked_expr(value, stmt_indent)?;
                    if value.ty.is_erased_value() {
                        if let Some(indent) = stmt_indent {
                            self.line_indent(indent, &format!("(void)({value_code});"));
                        }
                        continue;
                    }
                    emitted_fields.push(format!(".{} = {}", name, value_code));
                }
                if emitted_fields.is_empty() {
                    format!("({type_name}){{0}}")
                } else {
                    format!("({type_name}){{ {} }}", emitted_fields.join(", "))
                }
            }
            TExprKind::EnumLiteral {
                type_name,
                variant_name,
                variant_index,
                payload,
            } => {
                let physical_payload =
                    self.checked_enum_variant_payload(type_name, *variant_index)?;
                let mut payload_fields = Vec::new();
                let mut physical_idx = 0usize;
                for value in payload {
                    let value_code = self.value_initializer_for_checked_expr(value, stmt_indent)?;
                    if value.ty.is_erased_value() {
                        if let Some(indent) = stmt_indent {
                            self.line_indent(indent, &format!("(void)({value_code});"));
                        }
                        continue;
                    }
                    let Some(target_ty) = physical_payload.get(physical_idx) else {
                        return Err(vec![Diagnostic::new(
                            expr.span,
                            format!(
                                "internal error: enum `{type_name}` payload layout is missing field {physical_idx}"
                            ),
                        )]);
                    };
                    physical_idx += 1;
                    let value_code = if &value.ty == target_ty {
                        value_code
                    } else {
                        self.value_initializer_for_type(
                            &value.ty,
                            target_ty,
                            &value_code,
                            Some(expr.span),
                        )?
                    };
                    let idx = payload_fields.len();
                    payload_fields.push(format!("._{} = {}", idx, value_code));
                }
                let payload = payload_fields.join(", ");
                if payload.is_empty() {
                    format!("({type_name}){{ .tag = {variant_index} }}")
                } else {
                    format!(
                        "({type_name}){{ .tag = {variant_index}, .as.{variant_name} = {{ {payload} }} }}"
                    )
                }
            }
            TExprKind::ArrayLiteral(elements) => {
                if matches!(expr.ty, Ty::Slice { .. }) {
                    if let Some(indent) = stmt_indent {
                        return self.emit_slice_literal_temp(&expr.ty, elements, indent);
                    }
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "slice array literal needs statement lowering in this compiler slice",
                    )]);
                }
                if expr.ty.is_erased_value() {
                    if let Some(indent) = stmt_indent {
                        for element in elements {
                            let value = self.gen_expr_in_stmt(element, indent)?;
                            self.line_indent(indent, &format!("(void)({value});"));
                        }
                    }
                    return Ok("((void)0)".to_string());
                }
                let elements = elements
                    .iter()
                    .map(|element| self.gen_expr_with_lowering(element, stmt_indent))
                    .collect::<DiagResult<Vec<_>>>()?
                    .join(", ");
                format!("{{ {elements} }}")
            }
            TExprKind::ArrayRepeat { element, len } => {
                if matches!(expr.ty, Ty::Slice { .. }) {
                    if let Some(indent) = stmt_indent {
                        return self.emit_slice_repeat_temp(&expr.ty, element, *len, indent);
                    }
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "slice array repeat literal needs statement lowering in this compiler slice",
                    )]);
                }
                if expr.ty.is_erased_value() {
                    if let Some(indent) = stmt_indent {
                        let value = self.gen_expr_in_stmt(element, indent)?;
                        self.line_indent(indent, &format!("(void)({value});"));
                    }
                    return Ok("((void)0)".to_string());
                }
                if let Ty::Array { .. } = &expr.ty
                    && let Some(indent) = stmt_indent
                {
                    self.emit_temp_value("array_repeat", expr, indent)?
                } else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "array repeat literal needs statement lowering in this context",
                    )]);
                }
            }
            TExprKind::Closure { id, captures, .. } => {
                self.emit_closure_value(expr, *id, captures, stmt_indent)?
            }
            TExprKind::FunctionToClosure(inner) => {
                self.emit_function_to_closure_value(expr, inner, stmt_indent)?
            }
            TExprKind::RetainClosure {
                expr: inner,
                source_ty,
            } => self.emit_retain_closure_value(expr, inner, source_ty, stmt_indent)?,
            TExprKind::Unary { op, expr } => {
                let inner = self.gen_expr_with_lowering(expr, stmt_indent)?;
                match op {
                    UnaryOp::Not => format!("(!{inner})"),
                    UnaryOp::BitNot => integer_result_cast(&expr.ty, format!("~{inner}")),
                    UnaryOp::Neg => {
                        if matches!(expr.kind, TExprKind::Literal(Literal::Integer(_))) {
                            format!("(-{inner})")
                        } else if expr.ty.is_integer()
                            && let Some(helper) = checked_integer_unary_helper(&expr.ty)
                        {
                            let (file, line) = self.location_args(expr.span);
                            format!("{helper}({inner}, {file}, {line})")
                        } else {
                            format!("(-{inner})")
                        }
                    }
                    UnaryOp::Addr => {
                        if let TExprKind::Local(local_id, name) = &expr.kind
                            && self.local_is_heap(*local_id)
                        {
                            self.local_c_name(*local_id, name)
                        } else {
                            format!("(&{inner})")
                        }
                    }
                    UnaryOp::Deref => format!("(*{inner})"),
                }
            }
            TExprKind::Binary { op, left, right } => {
                if matches!(op, BinaryOp::And | BinaryOp::Or) && expr_needs_stmt_lowering(right) {
                    let Some(indent) = stmt_indent else {
                        return Err(vec![Diagnostic::new(
                            expr.span,
                            "short-circuit expression needs statement lowering",
                        )]);
                    };
                    return self.emit_short_circuit_expr(expr, *op, left, right, indent);
                }
                let op_str = match op {
                    BinaryOp::Or => "||",
                    BinaryOp::And => "&&",
                    BinaryOp::Eq => "==",
                    BinaryOp::Ne => "!=",
                    BinaryOp::Lt => "<",
                    BinaryOp::Le => "<=",
                    BinaryOp::Gt => ">",
                    BinaryOp::Ge => ">=",
                    BinaryOp::BitOr => "|",
                    BinaryOp::BitXor => "^",
                    BinaryOp::BitAnd => "&",
                    BinaryOp::Shl => "<<",
                    BinaryOp::Shr => ">>",
                    BinaryOp::Add => "+",
                    BinaryOp::Sub => "-",
                    BinaryOp::Mul => "*",
                    BinaryOp::Div => "/",
                    BinaryOp::Rem => "%",
                };
                let left_code = self.gen_expr_with_lowering(left, stmt_indent)?;
                let right_code = self.gen_expr_with_lowering(right, stmt_indent)?;
                if left.ty.is_integer()
                    && let Some(helper) = checked_integer_op_helper(op_str, &left.ty)
                {
                    let (file, line) = self.location_args(expr.span);
                    format!("{helper}({left_code}, {right_code}, {file}, {line})")
                } else if matches!(op_str, "/" | "%") && left.ty.is_integer() {
                    let helper = checked_integer_op_helper(op_str, &left.ty).ok_or_else(|| {
                        vec![Diagnostic::new(
                            left.span,
                            format!("no checked integer helper for `{}`", left.ty),
                        )]
                    })?;
                    let (file, line) = self.location_args(expr.span);
                    format!("{helper}({left_code}, {right_code}, {file}, {line})")
                } else if op.is_shift() && left.ty.is_integer() {
                    let helper = shift_integer_op_helper(*op, &left.ty).ok_or_else(|| {
                        vec![Diagnostic::new(
                            left.span,
                            format!("no shift helper for `{}`", left.ty),
                        )]
                    })?;
                    let (file, line) = self.location_args(expr.span);
                    format!("{helper}({left_code}, {right_code}, {file}, {line})")
                } else if op.is_bitwise() && left.ty.is_integer() {
                    integer_result_cast(&left.ty, format!("{left_code} {op_str} {right_code}"))
                } else {
                    format!("({left_code} {op_str} {right_code})")
                }
            }
            TExprKind::Cast { expr, ty } => {
                if expr.ty == *ty && !matches!(expr.kind, TExprKind::ArrayLiteral(_)) {
                    return self.gen_expr_with_lowering(expr, stmt_indent);
                }
                format!(
                    "(({}){})",
                    self.c_type(ty),
                    self.gen_expr_with_lowering(expr, stmt_indent)?
                )
            }
            TExprKind::Call { callee, args, .. } => {
                if self.std_resource_transfer_before_owner_close_call(callee) {
                    let Some(indent) = stmt_indent else {
                        return Err(vec![Diagnostic::new(
                            expr.span,
                            "resource owner transfer hook needs statement lowering",
                        )]);
                    };
                    return self.emit_resource_transfer_before_owner_close_call(expr, args, indent);
                }
                if matches!(&callee.kind, TExprKind::Function(_, name) if name == "ciel_panic")
                    && args.len() == 2
                {
                    let args = args
                        .iter()
                        .map(|arg| self.gen_expr_with_lowering(arg, stmt_indent))
                        .collect::<DiagResult<Vec<_>>>()?;
                    let (file, line) = self.location_args(expr.span);
                    return Ok(format!(
                        "ciel_panic_at({}, {}, {file}, {line})",
                        args[0], args[1]
                    ));
                }
                if matches!(callee.ty, Ty::Closure { .. } | Ty::ClosureInstance { .. }) {
                    let call = self.emit_closure_call(callee, args, stmt_indent)?;
                    return self.emit_array_call_value(expr, call, stmt_indent);
                }
                if let Some(scoped) = self.std_resource_scoped_call(callee)
                    && result_args(&self.program.checked.resolved, &expr.ty).is_some_and(
                        |(ok_ty, scoped_err_ty)| {
                            self.type_is_affine(ok_ty) || self.type_is_affine(scoped_err_ty)
                        },
                    )
                {
                    let Some(indent) = stmt_indent else {
                        return Err(vec![Diagnostic::new(
                            expr.span,
                            "resource scoped call needs statement lowering",
                        )]);
                    };
                    return self.emit_resource_scoped_call(expr, args, scoped, indent);
                }
                let callee = self.gen_expr_with_lowering(callee, stmt_indent)?;
                let args = self.gen_call_args(args, stmt_indent)?.join(", ");
                self.emit_array_call_value(expr, format!("{callee}({args})"), stmt_indent)?
            }
            TExprKind::UnsafeBlock { statements, value } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "unsafe block expression needs statement lowering",
                    )]);
                };
                return self.emit_unsafe_block_expr(expr, statements, value.as_deref(), indent);
            }
            TExprKind::ArrayToSlice(inner) => {
                let Ty::Slice { mutability, elem } = &expr.ty else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "internal error: array-to-slice conversion has non-slice type",
                    )]);
                };
                let Ty::Array { len, .. } = &inner.ty else {
                    return Err(vec![Diagnostic::new(
                        inner.span,
                        "internal error: array-to-slice conversion has non-array source",
                    )]);
                };
                if elem.is_erased_value() {
                    return Ok(format!(
                        "({}){{ .ptr = NULL, .len = {len} }}",
                        self.slice_name(*mutability, elem)
                    ));
                }
                let inner_code = self.gen_expr_with_lowering(inner, stmt_indent)?;
                format!(
                    "({}){{ .ptr = {inner_code}, .len = {len} }}",
                    self.slice_name(*mutability, elem)
                )
            }
            TExprKind::SliceToConst(inner) => {
                let Ty::Slice {
                    mutability: ViewMutability::ReadOnly,
                    ..
                } = &expr.ty
                else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "internal error: slice weakening has non-read-only slice type",
                    )]);
                };
                let source = if let Some(indent) = stmt_indent {
                    self.emit_temp_value("slice_view", inner, indent)?
                } else {
                    self.gen_expr_with_lowering(inner, stmt_indent)?
                };
                format!(
                    "({}){{ .ptr = {source}.ptr, .len = {source}.len }}",
                    self.c_type(&expr.ty)
                )
            }
            TExprKind::RawSliceFromPtr { ptr, len, elem_ty } => {
                let len_code = self.gen_expr_with_lowering(len, stmt_indent)?;
                if elem_ty.is_erased_value() {
                    return Ok(format!(
                        "({}){{ .ptr = NULL, .len = {len_code} }}",
                        self.c_type(&expr.ty)
                    ));
                }
                let ptr_code = self.gen_expr_with_lowering(ptr, stmt_indent)?;
                let elem_ptr_ty = self.c_pointer_type(elem_ty);
                format!(
                    "({}){{ .ptr = ({elem_ptr_ty})({ptr_code}), .len = {len_code} }}",
                    self.c_type(&expr.ty)
                )
            }
            TExprKind::MakeDynamicInterface {
                expr: inner,
                concrete_ty,
            } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "dynamic interface conversion needs statement lowering",
                    )]);
                };
                self.emit_dynamic_interface_value(expr, inner, concrete_ty, indent)?
            }
            TExprKind::ErrorBox {
                expr: inner,
                concrete_ty,
            } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "error boxing needs statement lowering",
                    )]);
                };
                let value = self.gen_expr_in_stmt(inner, indent)?;
                self.emit_error_boxed_value(&value, concrete_ty, indent, expr.span)?
            }
            TExprKind::DynamicInterfaceCall {
                interface_def,
                interface_name,
                receiver,
                args,
            } => {
                let receiver_code = self.gen_expr_with_lowering(receiver, stmt_indent)?;
                let receiver_code = if let Some(indent) = stmt_indent {
                    let temp = self.next_temp("dyn_recv");
                    self.line_indent(
                        indent,
                        &format!("{} = {receiver_code};", self.c_decl(&receiver.ty, &temp)),
                    );
                    temp
                } else {
                    receiver_code
                };
                let mut call_args = vec![format!("({receiver_code}).data")];
                call_args.extend(self.gen_call_args(args, stmt_indent)?);
                let field_name = self.dynamic_interface_field_name(&CheckedInterfaceRef {
                    def_id: *interface_def,
                    name: interface_name.clone(),
                    args: Vec::new(),
                });
                let call = format!(
                    "({receiver_code}).vtable->{}({})",
                    field_name,
                    call_args.join(", ")
                );
                self.emit_array_call_value(expr, call, stmt_indent)?
            }
            TExprKind::RetainedClosureInterfaceCall {
                interface_def,
                interface_name,
                interface_args,
                receiver,
                args,
            } => {
                let call = self.emit_retained_closure_interface_call(
                    *interface_def,
                    interface_name,
                    interface_args,
                    receiver,
                    args,
                    stmt_indent,
                )?;
                self.emit_array_call_value(expr, call, stmt_indent)?
            }
            TExprKind::CloneMessage { value, message_ty } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "clone_message needs statement lowering",
                    )]);
                };
                let source_ptr = self.gen_expr_with_lowering(value, Some(indent))?;
                self.emit_task_boundary_clone_result_from_ptr(
                    message_ty,
                    &source_ptr,
                    indent,
                    expr.span,
                )?
            }
            TExprKind::Field { base, field } => {
                if expr.ty.is_erased_value() {
                    let base = self.gen_expr_with_lowering(base, stmt_indent)?;
                    if let Some(indent) = stmt_indent {
                        self.line_indent(indent, &format!("(void)({base});"));
                    }
                    return Ok("((void)0)".to_string());
                }
                format!(
                    "({}).{}",
                    self.gen_expr_with_lowering(base, stmt_indent)?,
                    field
                )
            }
            TExprKind::Arrow { base, field } => {
                if expr.ty.is_erased_value() {
                    let base = self.gen_expr_with_lowering(base, stmt_indent)?;
                    if let Some(indent) = stmt_indent {
                        self.line_indent(indent, &format!("(void)({base});"));
                    }
                    return Ok("((void)0)".to_string());
                }
                format!(
                    "({})->{}",
                    self.gen_expr_with_lowering(base, stmt_indent)?,
                    field
                )
            }
            TExprKind::Index { base, index } => {
                let base_code = self.gen_expr_with_lowering(base, stmt_indent)?;
                let index_code = self.gen_expr_with_lowering(index, stmt_indent)?;
                match &base.ty {
                    Ty::Slice { .. } => {
                        let (file, line) = self.location_args(expr.span);
                        if expr.ty.is_erased_value() {
                            format!(
                                "((void)({base_code}), (void)ciel_bounds_check((size_t)({index_code}), ({base_code}).len, {file}, {line}), (void)0)"
                            )
                        } else {
                            format!(
                                "({base_code}).ptr[ciel_bounds_check((size_t)({index_code}), ({base_code}).len, {file}, {line})]"
                            )
                        }
                    }
                    Ty::Array { len, .. } => {
                        let (file, line) = self.location_args(expr.span);
                        if expr.ty.is_erased_value() {
                            format!(
                                "((void)({base_code}), (void)ciel_bounds_check((size_t)({index_code}), {len}, {file}, {line}), (void)0)"
                            )
                        } else {
                            format!(
                                "({base_code})[ciel_bounds_check((size_t)({index_code}), {len}, {file}, {line})]"
                            )
                        }
                    }
                    _ => format!("({base_code})[{index_code}]"),
                }
            }
            TExprKind::Slice { base, start, end } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "slice subview needs statement lowering in this context",
                    )]);
                };
                self.emit_slice_subview_temp(expr, base, start.as_deref(), end.as_deref(), indent)?
            }
            TExprKind::Try {
                expr: inner,
                propagation,
            } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "`?` needs statement lowering in this context",
                    )]);
                };
                self.emit_try_expr(expr, inner, propagation, indent)?
            }
            TExprKind::Await { future } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "`await` needs statement lowering in this context",
                    )]);
                };
                self.emit_await_expr(expr, future, indent)?
            }
            TExprKind::AsyncSelect { biased, arms } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "`select` needs statement lowering in this context",
                    )]);
                };
                self.emit_async_select_expr(expr, *biased, arms, indent)?
            }
            TExprKind::AsyncBlockOn { future } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "`block_on` needs statement lowering in this context",
                    )]);
                };
                self.emit_async_block_on_expr(expr, future, indent)?
            }
            TExprKind::AsyncSleep { ms, output_ty } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "`sleep_ms` needs statement lowering in this context",
                    )]);
                };
                self.emit_async_sleep_expr(expr, ms, output_ty, indent)?
            }
            TExprKind::AsyncOpFuture { op, output_ty, .. } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "async operation needs statement lowering in this context",
                    )]);
                };
                self.emit_async_op_expr(expr, op, output_ty, indent)?
            }
            TExprKind::AsyncChannelSend {
                sender,
                value,
                payload_ty,
            } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "`send` needs statement lowering in this context",
                    )]);
                };
                self.emit_async_channel_send_expr(expr, sender, value, payload_ty, indent)?
            }
            TExprKind::AsyncChannelReserve { sender, payload_ty } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "`reserve` needs statement lowering in this context",
                    )]);
                };
                self.emit_async_channel_reserve_expr(expr, sender, payload_ty, indent)?
            }
            TExprKind::AsyncChannelRecv {
                receiver,
                payload_ty,
            } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "`recv` needs statement lowering in this context",
                    )]);
                };
                self.emit_async_channel_recv_expr(expr, receiver, payload_ty, indent)?
            }
            TExprKind::AsyncChannelTrySend {
                sender,
                value,
                payload_ty,
            } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "`try_send` needs statement lowering in this context",
                    )]);
                };
                self.emit_async_channel_try_send_expr(expr, sender, value, payload_ty, indent)?
            }
            TExprKind::AsyncChannelPermitSend {
                permit,
                value,
                payload_ty,
            } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "`permit_send` needs statement lowering in this context",
                    )]);
                };
                self.emit_async_channel_permit_send_expr(expr, permit, value, payload_ty, indent)?
            }
            TExprKind::AsyncSpawn {
                body,
                task_output_ty,
            } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "`spawn` needs statement lowering in this context",
                    )]);
                };
                self.emit_async_spawn_expr(expr, body, task_output_ty, indent)?
            }
            TExprKind::AsyncTaskCancel { task, .. } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "`cancel` needs statement lowering in this context",
                    )]);
                };
                self.emit_async_task_cancel_expr(expr, task, indent)?
            }
            TExprKind::AsyncTaskIsFinished { task, .. } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "`is_finished` needs statement lowering in this context",
                    )]);
                };
                self.emit_async_task_is_finished_expr(expr, task, indent)?
            }
            TExprKind::MetaAsRefRepr { value, source_ty } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "as_ref_repr needs statement lowering in this context",
                    )]);
                };
                self.emit_meta_as_ref_repr_expr(expr, value, source_ty, indent)?
            }
            TExprKind::MetaIntoRepr { value, source_ty } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "into_repr needs statement lowering in this context",
                    )]);
                };
                self.emit_meta_into_repr_expr(expr, value, source_ty, indent)?
            }
            TExprKind::MetaFromRepr { value, target_ty } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "from_repr needs statement lowering in this context",
                    )]);
                };
                self.emit_meta_from_repr_expr(expr, value, target_ty, indent)?
            }
            TExprKind::ActorSpawn {
                mode,
                state_arg,
                handler,
                state_ty,
                handle_message_ty,
                message_ty,
                handler_ty,
            } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "actor spawn needs statement lowering in this context",
                    )]);
                };
                self.emit_actor_spawn_expr(
                    expr,
                    mode,
                    state_arg,
                    handler,
                    state_ty,
                    handle_message_ty,
                    message_ty,
                    handler_ty,
                    indent,
                )?
            }
            TExprKind::ActorSend {
                actor,
                value,
                message_ty,
            } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "send needs statement lowering in this context",
                    )]);
                };
                self.emit_actor_send_expr(expr, actor, value, message_ty, indent)?
            }
            TExprKind::ActorStop { actor, .. } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "stop needs statement lowering in this context",
                    )]);
                };
                self.emit_actor_lifecycle_expr(expr, actor, "ciel_actor_stop", indent)?
            }
            TExprKind::ActorJoin { actor, .. } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "join needs statement lowering in this context",
                    )]);
                };
                self.emit_actor_lifecycle_expr(expr, actor, "ciel_actor_join", indent)?
            }
            TExprKind::TypeSize { ty } => {
                if ty.is_erased_value() {
                    "0".to_string()
                } else {
                    format!("sizeof({})", self.c_sizeof_type(ty))
                }
            }
            TExprKind::TypeAlign { ty } => {
                if ty.is_erased_value() {
                    "1".to_string()
                } else {
                    format!("CIEL_ALIGNOF({})", self.c_sizeof_type(ty))
                }
            }
            TExprKind::TypeNeedsGcScan { ty } => {
                if self.ty_can_carry_gc_pointer(ty) {
                    "true".to_string()
                } else {
                    "false".to_string()
                }
            }
        };
        Ok(code)
    }

    pub(super) fn emit_short_circuit_expr(
        &mut self,
        expr: &TExpr,
        op: BinaryOp,
        left: &TExpr,
        right: &TExpr,
        indent: usize,
    ) -> DiagResult<String> {
        let result = self.next_temp("short_circuit");
        let left_code = self.gen_expr_with_lowering(left, Some(indent))?;
        self.line_indent(
            indent,
            &format!("{} = {left_code};", self.c_decl(&expr.ty, &result)),
        );
        let should_eval_right = match op {
            BinaryOp::And => result.clone(),
            BinaryOp::Or => format!("!{result}"),
            _ => unreachable!("short-circuit lowering only accepts && and ||"),
        };
        self.line_indent(indent, &format!("if ({should_eval_right}) {{"));
        let right_code = self.gen_expr_in_stmt(right, indent + 1)?;
        self.line_indent(indent + 1, &format!("{result} = {right_code};"));
        self.line_indent(indent, "}");
        Ok(result)
    }

    pub(super) fn emit_unsafe_block_expr(
        &mut self,
        expr: &TExpr,
        statements: &[TStmt],
        value: Option<&TExpr>,
        indent: usize,
    ) -> DiagResult<String> {
        let result = if expr.ty.is_erased_value() || expr.ty.is_never() {
            None
        } else {
            let temp = self.next_temp("unsafe_block");
            self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &temp)));
            Some(temp)
        };

        self.line_indent(indent, "{");
        self.defer_stack.push(Vec::new());
        let mut falls_through = true;
        for stmt in statements {
            if !self.gen_stmt(stmt, indent + 1)? {
                falls_through = false;
                break;
            }
        }
        if falls_through {
            if let Some(value) = value {
                if value.ty.is_erased_value() || expr.ty.is_erased_value() || value.is_never() {
                    let value_code = self.gen_expr_in_stmt(value, indent + 1)?;
                    self.line_indent(indent + 1, &format!("(void)({value_code});"));
                    falls_through = !value.is_never();
                } else if let Some(result) = &result {
                    self.emit_expr_store(result, value, indent + 1)?;
                }
            }
            if falls_through {
                self.emit_current_defers(indent + 1);
            }
        }
        self.defer_stack.pop();
        self.line_indent(indent, "}");

        Ok(result.unwrap_or_else(|| "((void)0)".to_string()))
    }

    pub(super) fn emit_meta_as_ref_repr_expr(
        &mut self,
        expr: &TExpr,
        value: &TExpr,
        source_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let value_temp = self.emit_temp_value("meta_ref_src", value, indent)?;
        if let Ty::Array { len, elem } = source_ty {
            let (_, literal) = self.emit_meta_array_ref_repr_literal(*len, elem, &value_temp, 0)?;
            return Ok(literal);
        }
        if let Ok(fields) = self.struct_fields_for_ty(expr.span, source_ty) {
            let fields = fields
                .into_iter()
                .map(|(name, ty)| MetaProductField {
                    value_expr: format!("&({value_temp})->{name}"),
                    name,
                    ty,
                })
                .collect::<Vec<_>>();
            let (_, literal) = self.meta_named_product_literal(&fields, "FieldRef")?;
            return Ok(literal);
        }
        if let Ok(variants) = self.enum_variants_for_ty(expr.span, source_ty) {
            return self.emit_meta_enum_ref_repr(expr, &value_temp, &variants, indent);
        }
        if matches!(source_ty, Ty::ClosureInstance { .. }) {
            return self.emit_meta_closure_ref_repr(expr, &value_temp, source_ty, indent);
        }
        Err(vec![Diagnostic::new(
            expr.span,
            format!("internal error: unsupported as_ref_repr source `{source_ty}`"),
        )])
    }

    pub(super) fn emit_meta_into_repr_expr(
        &mut self,
        expr: &TExpr,
        value: &TExpr,
        source_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let value_temp = self.emit_temp_value("meta_owned_src", value, indent)?;
        if matches!(
            source_ty,
            Ty::Array { .. } | Ty::Named { .. } | Ty::ClosureInstance { .. }
        ) {
            let source_expr = format!("(*{value_temp})");
            let (_, literal) = self.emit_meta_owned_leaf_repr_expr(
                expr.span,
                source_ty,
                &source_expr,
                source_ty,
                indent,
            )?;
            return Ok(literal);
        }
        Err(vec![Diagnostic::new(
            expr.span,
            format!("internal error: unsupported into_repr source `{source_ty}`"),
        )])
    }

    pub(super) fn emit_meta_from_repr_expr(
        &mut self,
        expr: &TExpr,
        value: &TExpr,
        target_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let value_temp = self.emit_temp_value("meta_repr_src", value, indent)?;
        if matches!(
            target_ty,
            Ty::Array { .. } | Ty::Named { .. } | Ty::ClosureInstance { .. }
        ) {
            let result = self.next_temp("meta_value");
            self.line_indent(indent, &format!("{};", self.c_decl(target_ty, &result)));
            self.emit_meta_value_from_repr_into(
                expr.span,
                &result,
                target_ty,
                &value_temp,
                target_ty,
                indent,
            )?;
            return Ok(result);
        }
        Err(vec![Diagnostic::new(
            expr.span,
            format!("internal error: unsupported from_repr target `{target_ty}`"),
        )])
    }

    pub(super) fn value_initializer_from_expr(&self, ty: &Ty, expr: &str) -> String {
        match ty {
            Ty::Array { len, elem } => {
                let elements = (0..*len)
                    .map(|idx| self.value_initializer_from_expr(elem, &format!("({expr})[{idx}]")))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{ {elements} }}")
            }
            _ => expr.to_string(),
        }
    }

    pub(in crate::codegen) fn emit_value_copy(
        &mut self,
        target: &str,
        source: &str,
        ty: &Ty,
        indent: usize,
    ) {
        if matches!(ty, Ty::Array { .. }) {
            self.line_indent(
                indent,
                &format!("memcpy({target}, {source}, sizeof({target}));"),
            );
        } else {
            self.line_indent(indent, &format!("{target} = {source};"));
        }
    }

    pub(super) fn value_or_initializer_from_expr(&self, ty: &Ty, expr: &str) -> String {
        if matches!(ty, Ty::Array { .. }) {
            self.value_initializer_from_expr(ty, expr)
        } else {
            expr.to_string()
        }
    }

    pub(super) fn value_initializer_for_checked_expr(
        &mut self,
        expr: &TExpr,
        stmt_indent: Option<usize>,
    ) -> DiagResult<String> {
        let value = self.gen_expr_with_lowering(expr, stmt_indent)?;
        if matches!(expr.ty, Ty::Array { .. }) && !matches!(expr.kind, TExprKind::ArrayLiteral(_)) {
            Ok(self.value_initializer_from_expr(&expr.ty, &value))
        } else {
            Ok(value)
        }
    }

    pub(in crate::codegen) fn value_initializer_for_type(
        &mut self,
        source_ty: &Ty,
        target_ty: &Ty,
        source_expr: &str,
        span: Option<crate::span::Span>,
    ) -> DiagResult<String> {
        if source_ty == target_ty {
            return Ok(self.value_or_initializer_from_expr(target_ty, source_expr));
        }
        if let Some(value) =
            self.policy_leaf_value_initializer(source_ty, target_ty, source_expr, span)?
        {
            return Ok(value);
        }
        if let Some(value) =
            self.storage_equivalent_value_initializer(source_ty, target_ty, source_expr, span)?
        {
            return Ok(value);
        }
        Err(vec![Diagnostic::new(
            span,
            format!("internal error: cannot adapt value `{source_ty}` to `{target_ty}`"),
        )])
    }

    pub(super) fn storage_types_equivalent(&self, left: &Ty, right: &Ty) -> bool {
        if left == right || self.c_type(left) == self.c_type(right) {
            return true;
        }
        match (left, right) {
            (
                Ty::Named {
                    name: left_name,
                    args: left_args,
                },
                Ty::Named {
                    name: right_name,
                    args: right_args,
                },
            ) => {
                left_name == right_name
                    && left_args.len() == right_args.len()
                    && left_args
                        .iter()
                        .zip(right_args.iter())
                        .all(|(left, right)| self.storage_types_equivalent(left, right))
            }
            (
                Ty::Array {
                    len: left_len,
                    elem: left_elem,
                },
                Ty::Array {
                    len: right_len,
                    elem: right_elem,
                },
            ) => left_len == right_len && self.storage_types_equivalent(left_elem, right_elem),
            (
                Ty::Pointer {
                    nullable: left_nullable,
                    mutability: left_mutability,
                    inner: left_inner,
                },
                Ty::Pointer {
                    nullable: right_nullable,
                    mutability: right_mutability,
                    inner: right_inner,
                },
            ) => {
                left_nullable == right_nullable
                    && left_mutability == right_mutability
                    && self.storage_types_equivalent(left_inner, right_inner)
            }
            _ => false,
        }
    }

    pub(super) fn storage_equivalent_value_initializer(
        &mut self,
        source_ty: &Ty,
        target_ty: &Ty,
        source_expr: &str,
        span: Option<crate::span::Span>,
    ) -> DiagResult<Option<String>> {
        if !self.storage_types_equivalent(source_ty, target_ty) {
            return Ok(None);
        }
        if source_ty == target_ty || self.c_type(source_ty) == self.c_type(target_ty) {
            return Ok(Some(
                self.value_or_initializer_from_expr(target_ty, source_expr),
            ));
        }
        let (
            Ty::Named {
                name: source_name, ..
            },
            Ty::Named {
                name: target_name, ..
            },
        ) = (source_ty, target_ty)
        else {
            return Ok(None);
        };
        if source_name != target_name {
            return Ok(None);
        }
        let fallback_span =
            span.unwrap_or_else(|| crate::span::Span::new(crate::span::FileId(0), 0, 0));
        let source_fields = match self.struct_fields_for_ty(fallback_span, source_ty) {
            Ok(fields) => fields,
            Err(_) => return Ok(None),
        };
        let target_fields = match self.struct_fields_for_ty(fallback_span, target_ty) {
            Ok(fields) => fields,
            Err(_) => return Ok(None),
        };
        if source_fields.len() != target_fields.len() {
            return Ok(None);
        }
        let mut fields = Vec::new();
        for ((source_field, source_field_ty), (target_field, target_field_ty)) in
            source_fields.iter().zip(target_fields.iter())
        {
            if source_field != target_field {
                return Ok(None);
            }
            if target_field_ty.is_erased_value() {
                continue;
            }
            if source_field_ty.is_erased_value() {
                return Ok(None);
            }
            let source_field_expr = format!("({source_expr}).{source_field}");
            let value = self.value_initializer_for_type(
                source_field_ty,
                target_field_ty,
                &source_field_expr,
                span,
            )?;
            fields.push(format!(".{target_field} = {value}"));
        }
        let c_type = self.c_type(target_ty);
        Ok(Some(if fields.is_empty() {
            format!("({c_type}){{0}}")
        } else {
            format!("({c_type}){{ {} }}", fields.join(", "))
        }))
    }

    pub(super) fn policy_leaf_value_initializer(
        &mut self,
        source_ty: &Ty,
        target_ty: &Ty,
        source_expr: &str,
        span: Option<crate::span::Span>,
    ) -> DiagResult<Option<String>> {
        let (
            Ty::Named {
                name: source_name,
                args: source_args,
            },
            Ty::Named {
                name: target_name,
                args: target_args,
            },
        ) = (source_ty, target_ty)
        else {
            return Ok(None);
        };
        if source_name != target_name
            || source_args.len() != target_args.len()
            || !self.type_matches_meta_policy_marker(source_ty)
            || !self.type_matches_meta_policy_marker(target_ty)
        {
            return Ok(None);
        }
        let source_fields = match self.struct_fields_for_ty(
            span.unwrap_or_else(|| crate::span::Span::new(crate::span::FileId(0), 0, 0)),
            source_ty,
        ) {
            Ok(fields) => fields,
            Err(_) => return Ok(None),
        };
        let target_fields = match self.struct_fields_for_ty(
            span.unwrap_or_else(|| crate::span::Span::new(crate::span::FileId(0), 0, 0)),
            target_ty,
        ) {
            Ok(fields) => fields,
            Err(_) => return Ok(None),
        };
        if source_fields.len() != target_fields.len() {
            return Ok(None);
        }
        let mut fields = Vec::new();
        for ((source_field, source_field_ty), (target_field, target_field_ty)) in
            source_fields.iter().zip(target_fields.iter())
        {
            if source_field != target_field {
                return Ok(None);
            }
            if target_field_ty.is_erased_value() {
                continue;
            }
            if source_field_ty.is_erased_value() {
                return Ok(None);
            }
            let source_field_expr = format!("({source_expr}).{source_field}");
            let value = self.value_initializer_for_type(
                source_field_ty,
                target_field_ty,
                &source_field_expr,
                span,
            )?;
            fields.push(format!(".{target_field} = {value}"));
        }
        let c_type = self.c_type(target_ty);
        Ok(Some(if fields.is_empty() {
            format!("({c_type}){{0}}")
        } else {
            format!("({c_type}){{ {} }}", fields.join(", "))
        }))
    }

    pub(in crate::codegen) fn emit_expr_store(
        &mut self,
        target: &str,
        value: &TExpr,
        indent: usize,
    ) -> DiagResult<()> {
        if value.ty.is_erased_value() {
            let value = self.gen_expr_in_stmt(value, indent)?;
            self.line_indent(indent, &format!("(void)({value});"));
            return Ok(());
        }
        if let Ty::Array { .. } = &value.ty
            && let TExprKind::ArrayLiteral(elements) = &value.kind
        {
            self.emit_array_literal_init(target, elements, indent)?;
            return Ok(());
        }
        if let Ty::Array { elem, .. } = &value.ty
            && let TExprKind::ArrayRepeat { element, len } = &value.kind
        {
            self.emit_array_repeat_init(target, elem, element, *len, indent)?;
            return Ok(());
        }
        let source = self.gen_expr_in_stmt(value, indent)?;
        self.emit_value_copy(target, &source, &value.ty, indent);
        Ok(())
    }

    pub(in crate::codegen) fn emit_assignment(
        &mut self,
        target: &TExpr,
        value: &TExpr,
        indent: usize,
    ) -> DiagResult<()> {
        if self.type_is_affine(&target.ty) {
            return self.with_temporary_resource_cleanup_scope(|this| {
                let source = this.emit_temp_value("resource_assign", value, indent)?;
                let target_code = this.gen_expr_in_stmt(target, indent)?;
                let cleanup = this.resource_cleanup_call(&target.ty, &target_code);
                this.line_indent(indent, &format!("{cleanup};"));
                this.emit_value_copy(&target_code, &source, &target.ty, indent);
                Ok(())
            });
        }
        let target_code = self.gen_expr_in_stmt(target, indent)?;
        self.emit_expr_store(&target_code, value, indent)
    }

    pub(in crate::codegen) fn emit_expr_statement(
        &mut self,
        expr: &TExpr,
        indent: usize,
    ) -> DiagResult<()> {
        if self.type_is_affine(&expr.ty) {
            let value = match &expr.kind {
                TExprKind::Move(_)
                | TExprKind::Local(..)
                | TExprKind::Field { .. }
                | TExprKind::Arrow { .. }
                | TExprKind::Index { .. }
                | TExprKind::Unary {
                    op: UnaryOp::Deref, ..
                } => self.gen_expr_in_stmt(expr, indent)?,
                _ => self.emit_temp_value("resource_expr", expr, indent)?,
            };
            let cleanup = self.resource_cleanup_call(&expr.ty, &value);
            self.line_indent(indent, &format!("{cleanup};"));
            return Ok(());
        }
        let value = self.gen_expr_in_stmt(expr, indent)?;
        self.line_indent(indent, &format!("(void)({value});"));
        Ok(())
    }

    pub(in crate::codegen) fn emit_local_decl_with_init(
        &mut self,
        ty: &Ty,
        name: &str,
        init: &TExpr,
        indent: usize,
    ) -> DiagResult<()> {
        if self.type_is_affine(ty) {
            self.line_indent(indent, &format!("{} = {{0}};", self.c_decl(ty, name)));
            self.push_temporary_resource_cleanup_defer(ty, name);
            self.emit_expr_store(name, init, indent)?;
            return Ok(());
        }
        if matches!(ty, Ty::Array { .. }) {
            self.line_indent(indent, &format!("{};", self.c_decl(ty, name)));
            self.emit_expr_store(name, init, indent)?;
            return Ok(());
        }
        let value = self.gen_expr_in_stmt(init, indent)?;
        self.line_indent(indent, &format!("{} = {value};", self.c_decl(ty, name)));
        Ok(())
    }

    pub(in crate::codegen) fn emit_temp_value(
        &mut self,
        prefix: &str,
        expr: &TExpr,
        indent: usize,
    ) -> DiagResult<String> {
        self.emit_temp_value_with_tracking(prefix, expr, indent, true)
    }

    pub(super) fn emit_untracked_temp_value(
        &mut self,
        prefix: &str,
        expr: &TExpr,
        indent: usize,
    ) -> DiagResult<String> {
        self.emit_temp_value_with_tracking(prefix, expr, indent, false)
    }

    pub(super) fn emit_temp_value_with_tracking(
        &mut self,
        prefix: &str,
        expr: &TExpr,
        indent: usize,
        track_result: bool,
    ) -> DiagResult<String> {
        let temp = self.next_temp(prefix);
        self.with_temporary_resource_cleanup_scope(|this| {
            this.emit_local_decl_with_init(&expr.ty, &temp, expr, indent)
        })?;
        if track_result {
            self.push_temporary_resource_cleanup_defer(&expr.ty, &temp);
        }
        Ok(temp)
    }

    pub(super) fn emit_array_return_value(
        &mut self,
        prefix: &str,
        ty: &Ty,
        source: &str,
        indent: usize,
    ) -> String {
        let temp = self.next_temp(prefix);
        self.line_indent(
            indent,
            &format!("{} {temp};", self.array_return_type_name(ty)),
        );
        self.emit_value_copy(&format!("{temp}.value"), source, ty, indent);
        temp
    }

    pub(in crate::codegen) fn emit_return_value(
        &mut self,
        ty: &Ty,
        source: &str,
        indent: usize,
    ) -> String {
        if self.ty_needs_array_return_wrapper(ty) {
            self.emit_array_return_value("array_return", ty, source, indent)
        } else {
            source.to_string()
        }
    }

    pub(in crate::codegen) fn emit_async_output_store(
        &mut self,
        ty: &Ty,
        out_raw: &str,
        source: &str,
        indent: usize,
    ) {
        if ty.is_erased_value() {
            return;
        }
        let out = format!("(({}){out_raw})", self.c_pointer_type(ty));
        if matches!(ty, Ty::Array { .. }) {
            self.line_indent(indent, &format!("memcpy({out}, {source}, sizeof(*{out}));"));
        } else {
            self.line_indent(indent, &format!("*{out} = {source};"));
        }
    }

    pub(in crate::codegen) fn future_result_layout_args(&self, output_ty: &Ty) -> (String, String) {
        if output_ty.is_erased_value() {
            ("0".to_string(), "1".to_string())
        } else {
            let c_ty = self.c_sizeof_type(output_ty);
            (format!("sizeof({c_ty})"), format!("CIEL_ALIGNOF({c_ty})"))
        }
    }

    pub(in crate::codegen) fn zero_return_value(&self, ty: &Ty) -> String {
        if self.ty_needs_array_return_wrapper(ty) {
            format!("({}){{0}}", self.array_return_type_name(ty))
        } else {
            self.zero_value(ty)
        }
    }

    pub(super) fn emit_array_call_value(
        &mut self,
        expr: &TExpr,
        call: String,
        stmt_indent: Option<usize>,
    ) -> DiagResult<String> {
        if !self.ty_needs_array_return_wrapper(&expr.ty) {
            return Ok(call);
        }
        let Some(indent) = stmt_indent else {
            return Err(vec![Diagnostic::new(
                expr.span,
                "array-returning call needs statement lowering",
            )]);
        };
        let temp = self.next_temp("array_call");
        self.line_indent(
            indent,
            &format!("{} {temp} = {call};", self.array_return_type_name(&expr.ty)),
        );
        Ok(format!("{temp}.value"))
    }

    pub(super) fn gen_literal(
        &mut self,
        span: crate::span::Span,
        literal: &Literal,
        ty: &Ty,
    ) -> String {
        match literal {
            Literal::Integer(raw) | Literal::Float(raw) => raw.replace('_', ""),
            Literal::Char(raw) => raw.clone(),
            Literal::Bool(value) => {
                if *value {
                    "true".to_string()
                } else {
                    "false".to_string()
                }
            }
            Literal::Null => "NULL".to_string(),
            Literal::String(raw) => {
                let len = string_literal_len(raw);
                let (mutability, elem) = match ty {
                    Ty::Slice { mutability, elem } => (*mutability, elem.as_ref()),
                    _ => (ViewMutability::ReadOnly, &Ty::Char),
                };
                let slice = self.slice_name(mutability, elem);
                let name = self
                    .plan
                    .string_literal_names
                    .get(&span_key(span))
                    .cloned()
                    .unwrap_or_else(|| raw.clone());
                let ptr = if matches!(elem, Ty::U8) {
                    format!("(uint8_t const *){name}")
                } else {
                    name
                };
                format!("({slice}){{ .ptr = {ptr}, .len = {len} }}")
            }
        }
    }
}
