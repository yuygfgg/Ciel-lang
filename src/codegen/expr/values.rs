use super::*;

impl<'a> CGenerator<'a> {
    pub(super) fn emit_array_to_slice_temp(
        &mut self,
        ty: &Ty,
        array: &TExpr,
        indent: usize,
    ) -> DiagResult<String> {
        let Ty::Slice {
            mutability,
            elem: slice_elem,
        } = ty
        else {
            return Err(vec![Diagnostic::new(
                None,
                "internal error: array-to-slice emitted for non-slice type",
            )]);
        };
        let Ty::Array { len, elem } = &array.ty else {
            return Err(vec![Diagnostic::new(
                array.span,
                "internal error: array-to-slice emitted for non-array source",
            )]);
        };
        if elem != slice_elem {
            return Err(vec![Diagnostic::new(
                array.span,
                "internal error: array-to-slice element type mismatch",
            )]);
        }

        let slice = self.next_temp("slice");
        let slice_ty = self.slice_name(*mutability, elem);
        if elem.is_erased_value() {
            let value = self.gen_expr_in_stmt(array, indent)?;
            self.line_indent(indent, &format!("(void)({value});"));
            self.line_indent(
                indent,
                &format!("{slice_ty} {slice} = ({slice_ty}){{ .ptr = NULL, .len = {len} }};"),
            );
            return Ok(slice);
        }

        let ptr = if self.expr_is_stable_array_lvalue(array) {
            self.gen_expr_in_stmt(array, indent)?
        } else {
            self.emit_heap_array_copy("slice_data", array, elem, *len, indent)?
        };
        self.line_indent(
            indent,
            &format!("{slice_ty} {slice} = ({slice_ty}){{ .ptr = {ptr}, .len = {len} }};"),
        );
        Ok(slice)
    }

    pub(super) fn emit_slice_literal_temp(
        &mut self,
        ty: &Ty,
        elements: &[TExpr],
        indent: usize,
    ) -> DiagResult<String> {
        let Ty::Slice { elem, .. } = ty else {
            return Err(vec![Diagnostic::new(
                None,
                "internal error: slice literal emitted for non-slice type",
            )]);
        };
        let data = self.next_temp("slice_data");
        let slice = self.next_temp("slice");
        if elem.is_erased_value() {
            for element in elements {
                let value = self.gen_expr_in_stmt(element, indent)?;
                self.line_indent(indent, &format!("(void)({value});"));
            }
            self.line_indent(
                indent,
                &format!(
                    "{} {slice} = ({}){{ .ptr = NULL, .len = {} }};",
                    self.c_type(ty),
                    self.c_type(ty),
                    elements.len()
                ),
            );
            return Ok(slice);
        }
        let elem_c = self.c_type(elem);
        let alloc = self.c_array_alloc_expr(elem, &elements.len().to_string());
        self.line_indent(indent, &format!("{elem_c} *{data} = ({elem_c} *){alloc};"));
        for (idx, element) in elements.iter().enumerate() {
            self.emit_expr_store(&format!("{data}[{idx}]"), element, indent)?;
        }
        self.line_indent(
            indent,
            &format!(
                "{} {slice} = ({}){{ .ptr = {data}, .len = {} }};",
                self.c_type(ty),
                self.c_type(ty),
                elements.len()
            ),
        );
        Ok(slice)
    }

    pub(super) fn emit_slice_repeat_temp(
        &mut self,
        ty: &Ty,
        element: &TExpr,
        len: usize,
        indent: usize,
    ) -> DiagResult<String> {
        let Ty::Slice { elem, .. } = ty else {
            return Err(vec![Diagnostic::new(
                None,
                "internal error: slice repeat literal emitted for non-slice type",
            )]);
        };
        let data = self.next_temp("slice_data");
        let slice = self.next_temp("slice");
        if elem.is_erased_value() {
            let value = self.gen_expr_in_stmt(element, indent)?;
            self.line_indent(indent, &format!("(void)({value});"));
            self.line_indent(
                indent,
                &format!(
                    "{} {slice} = ({}){{ .ptr = NULL, .len = {len} }};",
                    self.c_type(ty),
                    self.c_type(ty),
                ),
            );
            return Ok(slice);
        }
        let elem_c = self.c_type(elem);
        let alloc = self.c_array_alloc_expr(elem, &len.to_string());
        self.line_indent(indent, &format!("{elem_c} *{data} = ({elem_c} *){alloc};"));
        self.emit_array_repeat_init(data.as_str(), elem, element, len, indent)?;
        self.line_indent(
            indent,
            &format!(
                "{} {slice} = ({}){{ .ptr = {data}, .len = {len} }};",
                self.c_type(ty),
                self.c_type(ty),
            ),
        );
        Ok(slice)
    }

    pub(super) fn emit_slice_subview_temp(
        &mut self,
        expr: &TExpr,
        base: &TExpr,
        start: Option<&TExpr>,
        end: Option<&TExpr>,
        indent: usize,
    ) -> DiagResult<String> {
        enum SliceBase {
            Slice,
            Array { len: usize, elem: Ty },
        }

        let source = match &base.ty {
            Ty::Slice { .. } => SliceBase::Slice,
            Ty::Array { len, elem } => SliceBase::Array {
                len: *len,
                elem: (**elem).clone(),
            },
            other => {
                return Err(vec![Diagnostic::new(
                    base.span,
                    format!("internal error: cannot emit slice subview for `{other}`"),
                )]);
            }
        };

        let (ptr_code, len_code) = match source {
            SliceBase::Slice => {
                let base_code = self.gen_expr_in_stmt(base, indent)?;
                let base_temp = self.next_temp("slice_base");
                self.line_indent(
                    indent,
                    &format!("{} = {base_code};", self.c_decl(&base.ty, &base_temp)),
                );
                (format!("{base_temp}.ptr"), format!("{base_temp}.len"))
            }
            SliceBase::Array { len, elem } => {
                if elem.is_erased_value() {
                    let base_code = self.gen_expr_in_stmt(base, indent)?;
                    self.line_indent(indent, &format!("(void)({base_code});"));
                    ("NULL".to_string(), len.to_string())
                } else if self.expr_is_decayed_array_parameter(base) {
                    let base_code = self.gen_expr_in_stmt(base, indent)?;
                    let base_temp = self.next_temp("slice_array");
                    self.line_indent(
                        indent,
                        &format!("{} *{base_temp} = {base_code};", self.c_type(&elem)),
                    );
                    (base_temp, len.to_string())
                } else if self.expr_is_stable_array_lvalue(base) {
                    let base_code = self.gen_expr_in_stmt(base, indent)?;
                    let base_temp = self.next_temp("slice_array");
                    let array_ty = Ty::Array {
                        len,
                        elem: Box::new(elem),
                    };
                    self.line_indent(
                        indent,
                        &format!(
                            "{} = &({base_code});",
                            self.c_pointer_decl(&array_ty, &base_temp)
                        ),
                    );
                    (format!("(*{base_temp})"), len.to_string())
                } else {
                    let data = self.emit_heap_array_copy("slice_data", base, &elem, len, indent)?;
                    (data, len.to_string())
                }
            }
        };

        let start_temp = self.next_temp("slice_start");
        let start_code = match start {
            Some(start) => self.gen_expr_in_stmt(start, indent)?,
            None => "0".to_string(),
        };
        self.line_indent(
            indent,
            &format!("size_t {start_temp} = (size_t)({start_code});"),
        );

        let end_temp = self.next_temp("slice_end");
        let end_code = match end {
            Some(end) => self.gen_expr_in_stmt(end, indent)?,
            None => len_code.clone(),
        };
        self.line_indent(
            indent,
            &format!("size_t {end_temp} = (size_t)({end_code});"),
        );

        let offset_temp = self.next_temp("slice_offset");
        let (file, line) = self.location_args(expr.span);
        self.line_indent(
            indent,
            &format!(
                "size_t {offset_temp} = ciel_slice_range_check({start_temp}, {end_temp}, {len_code}, {file}, {line});"
            ),
        );

        let slice_temp = self.next_temp("slice");
        let slice_ty = self.c_type(&expr.ty);
        let ptr_value = match &expr.ty {
            Ty::Slice { elem, .. } if elem.is_erased_value() => "NULL".to_string(),
            _ => format!("({ptr_code}) + {offset_temp}"),
        };
        self.line_indent(
            indent,
            &format!(
                "{} = ({slice_ty}){{ .ptr = {ptr_value}, .len = {end_temp} - {start_temp} }};",
                self.c_decl(&expr.ty, &slice_temp)
            ),
        );
        Ok(slice_temp)
    }

    pub(super) fn expr_is_decayed_array_parameter(&self, expr: &TExpr) -> bool {
        matches!(expr.ty, Ty::Array { .. })
            && matches!(&expr.kind, TExprKind::Local(local_id, _) if self.current_param_locals.contains_key(local_id))
    }

    pub(super) fn expr_is_stable_array_lvalue(&self, expr: &TExpr) -> bool {
        matches!(expr.ty, Ty::Array { .. }) && self.expr_is_stable_lvalue(expr)
    }

    fn expr_is_stable_lvalue(&self, expr: &TExpr) -> bool {
        match &expr.kind {
            TExprKind::Local(..) => true,
            TExprKind::Field { base, .. } => self.expr_is_stable_lvalue(base),
            TExprKind::Arrow { .. } => true,
            TExprKind::Index { base, .. } => match &base.ty {
                Ty::Slice { .. } => true,
                Ty::Array { .. } => self.expr_is_stable_array_lvalue(base),
                _ => false,
            },
            TExprKind::Unary {
                op: UnaryOp::Deref, ..
            } => true,
            _ => false,
        }
    }

    fn emit_heap_array_copy(
        &mut self,
        prefix: &str,
        array: &TExpr,
        elem: &Ty,
        len: usize,
        indent: usize,
    ) -> DiagResult<String> {
        let array = self.array_copy_source(array);
        let data = self.next_temp(prefix);
        if elem.is_erased_value() {
            let value = self.gen_expr_in_stmt(array, indent)?;
            self.line_indent(indent, &format!("(void)({value});"));
            return Ok("NULL".to_string());
        }
        let elem_c = self.c_type(elem);
        let alloc = self.c_array_alloc_expr(elem, &len.to_string());
        self.line_indent(indent, &format!("{elem_c} *{data} = ({elem_c} *){alloc};"));
        match &array.kind {
            TExprKind::ArrayLiteral(elements) => {
                for (idx, element) in elements.iter().enumerate() {
                    self.emit_expr_store(&format!("{data}[{idx}]"), element, indent)?;
                }
            }
            TExprKind::ArrayRepeat {
                element,
                len: repeat_len,
            } if *repeat_len == len => {
                self.emit_array_repeat_init(&data, elem, element, *repeat_len, indent)?;
            }
            _ => {
                let source = self.gen_expr_in_stmt(array, indent)?;
                self.line_indent(
                    indent,
                    &format!(
                        "memcpy({data}, {source}, sizeof({}) * {len});",
                        self.c_sizeof_type(elem)
                    ),
                );
            }
        }
        Ok(data)
    }

    fn array_copy_source<'b>(&self, expr: &'b TExpr) -> &'b TExpr {
        match &expr.kind {
            TExprKind::Cast { expr: inner, ty }
                if expr.ty == *ty && matches!(ty, Ty::Array { .. }) =>
            {
                self.array_copy_source(inner)
            }
            _ => expr,
        }
    }

    pub(super) fn emit_array_literal_init(
        &mut self,
        target: &str,
        elements: &[TExpr],
        indent: usize,
    ) -> DiagResult<()> {
        for (idx, element) in elements.iter().enumerate() {
            self.emit_expr_store(&format!("({target})[{idx}]"), element, indent)?;
        }
        Ok(())
    }

    pub(super) fn emit_array_repeat_init(
        &mut self,
        target: &str,
        elem_ty: &Ty,
        element: &TExpr,
        len: usize,
        indent: usize,
    ) -> DiagResult<()> {
        if elem_ty.is_erased_value() {
            let value = self.gen_expr_in_stmt(element, indent)?;
            self.line_indent(indent, &format!("(void)({value});"));
            return Ok(());
        }
        let value_temp = self.emit_temp_value("repeat_value", element, indent)?;
        let index_temp = self.next_temp("repeat_i");
        self.line_indent(
            indent,
            &format!("for (size_t {index_temp} = 0; {index_temp} < {len}; {index_temp}++) {{"),
        );
        self.emit_value_copy(
            &format!("({target})[{index_temp}]"),
            &value_temp,
            elem_ty,
            indent + 1,
        );
        self.line_indent(indent, "}");
        Ok(())
    }

    pub(super) fn emit_dynamic_interface_value(
        &mut self,
        expr: &TExpr,
        inner: &TExpr,
        concrete_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        if matches!(concrete_ty, Ty::DynamicInterface { .. }) {
            return self.emit_dynamic_interface_reerasure(expr, inner, concrete_ty, indent);
        }
        let data_expr = self.gen_expr_in_stmt(inner, indent)?;
        let data_ptr = if matches!(concrete_ty, Ty::Pointer { .. }) {
            format!("(void *)({data_expr})")
        } else {
            let temp = self.next_temp("dyn_data");
            self.line_indent(
                indent,
                &format!(
                    "{} *{temp} = ({})ciel_alloc(sizeof({}));",
                    self.c_type(concrete_ty),
                    self.c_pointer_type(concrete_ty),
                    self.c_sizeof_type(concrete_ty)
                ),
            );
            self.line_indent(indent, &format!("*{temp} = {data_expr};"));
            format!("(void *){temp}")
        };
        let dyn_c = self.c_type(&expr.ty);
        let vtable = self.dynamic_table_name(&expr.ty, concrete_ty);
        Ok(format!(
            "({dyn_c}){{ .data = {data_ptr}, .vtable = &{vtable} }}"
        ))
    }

    pub(super) fn emit_dynamic_interface_reerasure(
        &mut self,
        expr: &TExpr,
        inner: &TExpr,
        concrete_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let Ty::DynamicInterface { def_id, args, .. } = &expr.ty else {
            return Err(vec![Diagnostic::new(
                expr.span,
                "internal error: dynamic re-erasure target is not dynamic",
            )]);
        };
        let source_code = self.gen_expr_in_stmt(inner, indent)?;
        let source_temp = self.next_temp("dyn_source");
        self.line_indent(
            indent,
            &format!(
                "{} = {source_code};",
                self.c_decl(concrete_ty, &source_temp)
            ),
        );
        let vtable_ty = self.dynamic_vtable_name(&expr.ty);
        let vtable_temp = self.next_temp("dyn_vtable");
        self.line_indent(
            indent,
            &format!(
                "{vtable_ty} *{vtable_temp} = ({vtable_ty} *)ciel_alloc(sizeof({vtable_ty}));"
            ),
        );
        for interface in self.dynamic_view_interfaces(*def_id, args) {
            let field_name = self.dynamic_interface_field_name(&interface);
            self.line_indent(
                indent,
                &format!(
                    "{vtable_temp}->{} = ({source_temp}).vtable->{};",
                    field_name, field_name
                ),
            );
        }
        let dyn_c = self.c_type(&expr.ty);
        Ok(format!(
            "({dyn_c}){{ .data = ({source_temp}).data, .vtable = {vtable_temp} }}"
        ))
    }

    pub(super) fn emit_closure_value(
        &mut self,
        expr: &TExpr,
        id: usize,
        captures: &[TClosureCapture],
        stmt_indent: Option<usize>,
    ) -> DiagResult<String> {
        let owner = self.current_closure_owner.ok_or_else(|| {
            vec![Diagnostic::new(
                expr.span,
                "internal error: closure emitted outside a function",
            )]
        })?;
        if matches!(expr.ty, Ty::Function { .. }) {
            return Ok(self.closure_thunk_name(owner, id));
        }
        let (Ty::Closure { .. } | Ty::ClosureInstance { .. }) = expr.ty else {
            return Err(vec![Diagnostic::new(
                expr.span,
                "internal error: closure literal has non-closure type",
            )]);
        };
        let env = if captures.is_empty() {
            "NULL".to_string()
        } else {
            let Some(indent) = stmt_indent else {
                return Err(vec![Diagnostic::new(
                    expr.span,
                    "capturing closure needs statement lowering",
                )]);
            };
            let env_name = self.closure_env_name(owner, id);
            let temp = self.next_temp("closure_env");
            self.line_indent(
                indent,
                &format!("{env_name} *{temp} = ({env_name} *)ciel_alloc(sizeof({env_name}));"),
            );
            for (idx, capture) in captures.iter().enumerate() {
                if capture.ty.is_erased_value() {
                    continue;
                }
                let value = TExpr {
                    span: expr.span,
                    ty: capture.ty.clone(),
                    kind: TExprKind::Local(capture.local_id, capture.name.clone()),
                };
                let value = self.gen_expr_in_stmt(&value, indent)?;
                self.emit_value_copy(&format!("{temp}->cap{idx}"), &value, &capture.ty, indent);
                if self.type_is_affine(&capture.ty) {
                    self.emit_resource_zero_expr(&capture.ty, &value, indent);
                }
            }
            format!("(void *){temp}")
        };
        Ok(format!(
            "({}){{ .call = {}, .env = {env} }}",
            self.c_type(&expr.ty),
            self.closure_thunk_name(owner, id)
        ))
    }

    pub(super) fn emit_function_to_closure_value(
        &mut self,
        expr: &TExpr,
        inner: &TExpr,
        stmt_indent: Option<usize>,
    ) -> DiagResult<String> {
        let Some(indent) = stmt_indent else {
            return Err(vec![Diagnostic::new(
                expr.span,
                "function-to-closure conversion needs statement lowering",
            )]);
        };
        let function_value = self.gen_expr_in_stmt(inner, indent)?;
        self.emit_closure_value_from_source(&expr.ty, &inner.ty, &function_value, indent)
    }

    pub(super) fn emit_retain_closure_value(
        &mut self,
        expr: &TExpr,
        inner: &TExpr,
        source_ty: &Ty,
        stmt_indent: Option<usize>,
    ) -> DiagResult<String> {
        let Some(indent) = stmt_indent else {
            return Err(vec![Diagnostic::new(
                expr.span,
                "retained closure conversion needs statement lowering",
            )]);
        };
        let source_code = self.gen_expr_in_stmt(inner, indent)?;
        let source_temp = self.next_temp("closure_source");
        self.line_indent(
            indent,
            &format!("{} = {source_code};", self.c_decl(source_ty, &source_temp)),
        );
        self.emit_closure_value_from_source(&expr.ty, source_ty, &source_temp, indent)
    }

    pub(in crate::codegen) fn emit_closure_value_from_source(
        &mut self,
        target_ty: &Ty,
        source_ty: &Ty,
        source_value: &str,
        indent: usize,
    ) -> DiagResult<String> {
        if matches!(
            source_ty,
            Ty::Function {
                is_unsafe: false,
                abi: None,
                ..
            }
        ) {
            return self.emit_function_closure_value_from_source(
                target_ty,
                source_ty,
                source_value,
                indent,
            );
        }
        if retained_closure_needs_wrapper(target_ty, source_ty) {
            return self.emit_wrapped_retained_closure_value_from_source(
                target_ty,
                source_ty,
                source_value,
                indent,
            );
        }
        let mut fields = vec![
            format!(".call = ({source_value}).call"),
            format!(".env = ({source_value}).env"),
        ];
        fields.extend(self.retained_closure_witness_initializers(
            target_ty,
            source_ty,
            source_value,
        ));
        Ok(format!(
            "({}){{ {} }}",
            self.c_type(target_ty),
            fields.join(", ")
        ))
    }

    pub(super) fn emit_function_closure_value_from_source(
        &mut self,
        target_ty: &Ty,
        source_ty: &Ty,
        source_value: &str,
        indent: usize,
    ) -> DiagResult<String> {
        let env_name = self.function_closure_env_name(target_ty, source_ty);
        let temp = self.next_temp("closure_fn_env");
        self.line_indent(
            indent,
            &format!("{env_name} *{temp} = ({env_name} *)ciel_alloc(sizeof({env_name}));"),
        );
        self.line_indent(indent, &format!("{temp}->func = {source_value};"));
        let mut fields = vec![
            format!(
                ".call = {}",
                self.function_closure_thunk_name(target_ty, source_ty)
            ),
            format!(".env = (void *){temp}"),
        ];
        fields.extend(self.retained_closure_witness_initializers(target_ty, source_ty, ""));
        Ok(format!(
            "({}){{ {} }}",
            self.c_type(target_ty),
            fields.join(", ")
        ))
    }

    pub(super) fn emit_wrapped_retained_closure_value_from_source(
        &mut self,
        target_ty: &Ty,
        source_ty: &Ty,
        source_value: &str,
        indent: usize,
    ) -> DiagResult<String> {
        let env_name = self.retained_closure_env_name(target_ty, source_ty);
        let temp = self.next_temp("closure_retain_env");
        self.line_indent(
            indent,
            &format!("{env_name} *{temp} = ({env_name} *)ciel_alloc(sizeof({env_name}));"),
        );
        self.emit_value_copy(&format!("{temp}->source"), source_value, source_ty, indent);
        let mut fields = vec![
            format!(
                ".call = {}",
                self.retained_closure_thunk_name(target_ty, source_ty)
            ),
            format!(".env = (void *){temp}"),
        ];
        fields.extend(self.retained_closure_witness_initializers(
            target_ty,
            source_ty,
            source_value,
        ));
        Ok(format!(
            "({}){{ {} }}",
            self.c_type(target_ty),
            fields.join(", ")
        ))
    }

    pub(super) fn retained_closure_witness_initializers(
        &self,
        target_ty: &Ty,
        source_ty: &Ty,
        source_value: &str,
    ) -> Vec<String> {
        retained_closure_capabilities(target_ty)
            .into_iter()
            .map(|capability| {
                let field = self.retained_closure_witness_field_name(&capability);
                let value = self.retained_closure_witness_value(
                    target_ty,
                    source_ty,
                    &capability,
                    Some(source_value),
                );
                format!(".{field} = {value}")
            })
            .collect()
    }

    pub(super) fn retained_closure_witness_value(
        &self,
        target_ty: &Ty,
        source_ty: &Ty,
        capability: &ConstraintRef,
        source_value: Option<&str>,
    ) -> String {
        if retained_closure_can_reuse_source_witness_field(target_ty, source_ty, capability)
            && let Some(source_value) = source_value
        {
            return format!(
                "({source_value}).{}",
                self.retained_closure_witness_field_name(capability)
            );
        }
        self.retained_closure_witness_name(target_ty, source_ty, capability)
    }

    pub(super) fn emit_closure_call(
        &mut self,
        callee: &TExpr,
        args: &[TExpr],
        stmt_indent: Option<usize>,
    ) -> DiagResult<String> {
        let callee_code = self.gen_expr_with_lowering(callee, stmt_indent)?;
        let receiver = if let Some(indent) = stmt_indent {
            let temp = self.next_temp("closure");
            self.line_indent(
                indent,
                &format!("{} = {callee_code};", self.c_decl(&callee.ty, &temp)),
            );
            temp
        } else {
            callee_code
        };
        let mut call_args = vec![format!("({receiver}).env")];
        call_args.extend(self.gen_call_args(args, stmt_indent)?);
        Ok(format!("({receiver}).call({})", call_args.join(", ")))
    }

    pub(super) fn emit_retained_closure_interface_call(
        &mut self,
        interface_def: DefId,
        interface_name: &str,
        interface_args: &[Ty],
        receiver: &TExpr,
        args: &[TExpr],
        stmt_indent: Option<usize>,
    ) -> DiagResult<String> {
        let capability = ConstraintRef {
            def_id: interface_def,
            name: interface_name.to_string(),
            args: interface_args.to_vec(),
        };
        let receiver_code = self.gen_expr_with_lowering(receiver, stmt_indent)?;
        let (receiver_ref, receiver_value) = match &receiver.ty {
            Ty::Pointer { inner, .. } if matches!(&**inner, Ty::Closure { .. }) => {
                let receiver_ref = if let Some(indent) = stmt_indent {
                    let temp = self.next_temp("retained_recv_ptr");
                    self.line_indent(
                        indent,
                        &format!("{} = {receiver_code};", self.c_decl(&receiver.ty, &temp)),
                    );
                    temp
                } else {
                    receiver_code
                };
                (receiver_ref.clone(), format!("*({receiver_ref})"))
            }
            Ty::Closure { .. } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        receiver.span,
                        "retained closure interface call needs statement lowering",
                    )]);
                };
                let temp = self.next_temp("retained_recv");
                self.line_indent(
                    indent,
                    &format!("{} = {receiver_code};", self.c_decl(&receiver.ty, &temp)),
                );
                (format!("&{temp}"), temp)
            }
            other => {
                return Err(vec![Diagnostic::new(
                    receiver.span,
                    format!(
                        "internal error: retained closure interface receiver has type `{other}`"
                    ),
                )]);
            }
        };
        let mut call_args = vec![format!("(void *)({receiver_ref})")];
        call_args.extend(self.gen_call_args(args, stmt_indent)?);
        Ok(format!(
            "({receiver_value}).{}({})",
            self.retained_closure_witness_field_name(&capability),
            call_args.join(", ")
        ))
    }
}
