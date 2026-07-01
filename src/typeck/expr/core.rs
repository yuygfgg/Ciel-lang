use super::*;

impl TypeChecker {
    pub(in crate::typeck) fn check_expr(
        &mut self,
        scopes: &mut LocalScopes,
        expr: &Expr,
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        let result = self.check_expr_uncoerced(scopes, expr, expected)?;
        Some(self.coerce_expr_to_expected(scopes, result, expected))
    }

    pub(in crate::typeck) fn consume_affine_expr(
        &mut self,
        scopes: &mut LocalScopes,
        expr: TExpr,
        allow_loop_move: bool,
    ) -> TExpr {
        if let TExprKind::ErrorBox {
            expr: inner,
            concrete_ty,
        } = expr.kind.clone()
        {
            let inner = self.consume_affine_expr(scopes, *inner, allow_loop_move);
            return TExpr {
                span: expr.span,
                ty: expr.ty,
                kind: TExprKind::ErrorBox {
                    expr: Box::new(inner),
                    concrete_ty,
                },
            };
        }
        if !self.type_is_affine(&expr.ty) {
            return expr;
        }
        if let TExprKind::UnsafeBlock {
            statements,
            value: Some(value),
        } = expr.kind.clone()
        {
            let previous_unsafe_depth = self.unsafe_depth;
            self.unsafe_depth += 1;
            let value = self.consume_affine_expr(scopes, *value, allow_loop_move);
            self.unsafe_depth = previous_unsafe_depth;
            return TExpr {
                span: expr.span,
                ty: expr.ty,
                kind: TExprKind::UnsafeBlock {
                    statements,
                    value: Some(Box::new(value)),
                },
            };
        }
        if let TExprKind::Cast { expr: inner, ty } = expr.kind.clone()
            && inner.ty == ty
        {
            let inner = self.consume_affine_expr(scopes, *inner, allow_loop_move);
            return TExpr {
                span: expr.span,
                ty: expr.ty,
                kind: TExprKind::Cast {
                    expr: Box::new(inner),
                    ty,
                },
            };
        }
        match &expr.kind {
            TExprKind::Local(local_id, name) => {
                let mut can_move = true;
                if let Some(binding) = scopes.get(*local_id) {
                    if binding.declared_loop_depth < self.current_loop_depth && !allow_loop_move {
                        self.diagnostics.push(Diagnostic::new(
                            expr.span,
                            format!(
                                "resource `{name}` is declared outside this loop and cannot be moved inside it"
                            ),
                        ));
                        can_move = false;
                    }
                }
                if can_move && let Some(binding) = scopes.get_mut(*local_id) {
                    binding.init_state = InitState::Moved;
                    binding.narrowed_ty = None;
                }
                TExpr {
                    span: expr.span,
                    ty: expr.ty.clone(),
                    kind: TExprKind::Move(Box::new(expr)),
                }
            }
            TExprKind::Field { .. } | TExprKind::Arrow { .. } | TExprKind::Index { .. } => {
                if self.unsafe_depth == 0 {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!("cannot move resource subvalue of type `{}`", expr.ty),
                    ));
                    expr
                } else {
                    TExpr {
                        span: expr.span,
                        ty: expr.ty.clone(),
                        kind: TExprKind::Move(Box::new(expr)),
                    }
                }
            }
            TExprKind::Move(_) => expr,
            _ => expr,
        }
    }

    pub(super) fn diagnose_prechecked_affine_expr_moved(
        &mut self,
        scopes: &LocalScopes,
        expr: &TExpr,
    ) {
        if !self.type_is_affine(&expr.ty) {
            return;
        }
        let Some((local_id, name)) = lvalue_root_local(expr) else {
            return;
        };
        let Some(binding) = scopes.get(local_id) else {
            return;
        };
        if binding.init_state.is_assigned() {
            return;
        }
        if binding.init_state.is_moved() {
            self.diagnostics.push(Diagnostic::new(
                expr.span,
                format!("use of moved resource `{name}`"),
            ));
        } else {
            self.diagnostics.push(Diagnostic::new(
                expr.span,
                format!("local `{name}` is not definitely assigned"),
            ));
        }
    }

    pub(in crate::typeck) fn check_consumed_expr(
        &mut self,
        scopes: &mut LocalScopes,
        expr: &Expr,
        expected: Option<&Ty>,
        allow_loop_move: bool,
    ) -> Option<TExpr> {
        let inherited_loop_move = self.return_loop_move_depth > 0;
        let checked = self.with_return_loop_move_context(allow_loop_move, |this| {
            this.check_expr(scopes, expr, expected)
        })?;
        Some(self.consume_affine_expr(scopes, checked, allow_loop_move || inherited_loop_move))
    }

    pub(super) fn check_expr_uncoerced(
        &mut self,
        scopes: &mut LocalScopes,
        expr: &Expr,
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        let result = match &expr.kind {
            ExprKind::Name(name_ref) => {
                if let Some(local_id) = self.resolved_local_id(name_ref)
                    && let Some(binding) = scopes.get(local_id)
                {
                    let name = binding.name.clone();
                    let init_state = binding.init_state;
                    let binding_ty = binding.ty.clone();
                    if !binding.init_state.is_assigned() {
                        if init_state.is_moved() && self.type_is_affine(&binding_ty) {
                            self.diagnostics.push(Diagnostic::new(
                                expr.span,
                                format!("use of moved resource `{name}`"),
                            ));
                        } else {
                            self.diagnostics.push(Diagnostic::new(
                                expr.span,
                                format!("local `{name}` is not definitely assigned"),
                            ));
                        }
                    }
                    TExpr {
                        span: expr.span,
                        ty: scopes
                            .effective_ty(local_id)
                            .unwrap_or_else(|| binding.ty.clone()),
                        kind: TExprKind::Local(local_id, name),
                    }
                } else if let Some(sig) = self.resolve_function_name(name_ref) {
                    if sig.is_unsafe {
                        self.require_unsafe(
                            expr.span,
                            format!(
                                "use of unsafe function `{}` as a value requires unsafe block",
                                sig.name
                            ),
                        );
                    }
                    TExpr {
                        span: expr.span,
                        ty: Ty::Function {
                            is_unsafe: sig.is_unsafe,
                            abi: sig.abi.clone(),
                            ret: Box::new(sig.ret.clone()),
                            params: sig.params.clone(),
                        },
                        kind: TExprKind::Function(sig.def_id, sig.name.clone()),
                    }
                } else if let Some((def_id, sig)) = self.lookup_variant_name(name_ref, expected) {
                    let variant_name = self.ctx.resolved.def(def_id).name.clone();
                    self.check_variant_literal(
                        scopes,
                        expr.span,
                        &variant_name,
                        sig,
                        Vec::new(),
                        expected,
                    )?
                } else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!("unresolved name `{}`", name_ref.display),
                    ));
                    return None;
                }
            }
            ExprKind::Literal(literal) => self.check_literal(expr.span, literal, expected)?,
            ExprKind::StructLiteral(fields) => {
                let Some(Ty::Named {
                    name: type_name,
                    args,
                }) = expected
                else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        "struct literal requires an expected struct type",
                    ));
                    return None;
                };
                let instance_name = enum_instance_name(type_name, args);
                let struct_fields = if let Some(fields) =
                    self.ctx.structs.get(&instance_name).cloned()
                {
                    fields
                } else if let Some(template) = self.ctx.struct_templates.get(type_name).cloned() {
                    if template.generics.len() != args.len() {
                        self.diagnostics.push(Diagnostic::new(
                            expr.span,
                            format!(
                                "struct `{type_name}` expects {} type arguments, got {}",
                                template.generics.len(),
                                args.len()
                            ),
                        ));
                        return None;
                    }
                    let subst = template
                        .generics
                        .iter()
                        .map(|generic| generic.name.clone())
                        .zip(args.iter().cloned())
                        .collect::<HashMap<_, _>>();
                    self.check_generic_constraints(&template.generics, &subst, expr.span);
                    template
                        .fields
                        .iter()
                        .map(|field| {
                            (
                                field.name.name.clone(),
                                self.lower_type_with_subst_allowing_holes(&field.ty, &subst),
                            )
                        })
                        .collect()
                } else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!("`{}` is not a known struct", expected.unwrap()),
                    ));
                    return None;
                };
                if self.is_unsafe_struct_instance(type_name, args) {
                    self.require_unsafe(
                        expr.span,
                        format!("constructing unsafe struct `{type_name}` requires unsafe block"),
                    );
                }
                let mut seen = HashMap::<String, ()>::new();
                let mut checked_fields = Vec::new();
                for init in fields {
                    if seen.insert(init.name.name.clone(), ()).is_some() {
                        self.diagnostics.push(Diagnostic::new(
                            init.name.span,
                            format!("duplicate field `{}`", init.name.name),
                        ));
                    }
                    let Some((_, field_ty)) = struct_fields
                        .iter()
                        .find(|(field_name, _)| field_name == &init.name.name)
                    else {
                        self.diagnostics.push(Diagnostic::new(
                            init.name.span,
                            format!("unknown field `{}` on `{type_name}`", init.name.name),
                        ));
                        continue;
                    };
                    let field_ty = self.resolve_type_holes(field_ty);
                    if field_ty.is_erased_value() {
                        self.diagnostics.push(Diagnostic::new(
                            init.name.span,
                            "void fields are implicit and cannot be explicitly initialized",
                        ));
                        continue;
                    }
                    let value =
                        self.check_consumed_expr(scopes, &init.expr, Some(&field_ty), false)?;
                    self.require_assignable(&field_ty, &value.ty, init.expr.span);
                    checked_fields.push((init.name.name.clone(), value));
                }
                for (field_name, field_ty) in &struct_fields {
                    if !field_ty.is_erased_value() && !seen.contains_key(field_name) {
                        self.diagnostics.push(Diagnostic::new(
                            expr.span,
                            format!("missing field `{field_name}` in `{type_name}` literal"),
                        ));
                    }
                }
                let ty = self.resolve_type_holes(&Ty::Named {
                    name: type_name.clone(),
                    args: args.clone(),
                });
                self.ensure_struct_instance(&ty);
                let Ty::Named {
                    name: concrete_name,
                    args: concrete_args,
                } = &ty
                else {
                    unreachable!("struct literal expected type is named");
                };
                let instance_name = enum_instance_name(concrete_name, concrete_args);
                TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::StructLiteral {
                        type_name: instance_name,
                        fields: checked_fields,
                    },
                }
            }
            ExprKind::ArrayLiteral(elements) => {
                let (elem_ty, result_ty) = match expected {
                    Some(Ty::Array { len, elem }) => {
                        if *len != elements.len() {
                            self.diagnostics.push(Diagnostic::new(
                                expr.span,
                                format!(
                                    "array literal has {} elements, expected {len}",
                                    elements.len()
                                ),
                            ));
                        }
                        ((**elem).clone(), expected.cloned().unwrap())
                    }
                    Some(Ty::Slice { mutability, elem }) => (
                        (**elem).clone(),
                        Ty::Slice {
                            mutability: *mutability,
                            elem: elem.clone(),
                        },
                    ),
                    _ => {
                        self.diagnostics.push(Diagnostic::new(
                            expr.span,
                            "array literal requires an expected array or slice type",
                        ));
                        return None;
                    }
                };
                let checked_elements = elements
                    .iter()
                    .filter_map(|element| {
                        self.check_consumed_expr(scopes, element, Some(&elem_ty), false)
                    })
                    .collect::<Vec<_>>();
                for element in &checked_elements {
                    self.require_assignable(&elem_ty, &element.ty, element.span);
                }
                TExpr {
                    span: expr.span,
                    ty: result_ty,
                    kind: TExprKind::ArrayLiteral(checked_elements),
                }
            }
            ExprKind::ArrayRepeat { element, len } => {
                let (elem_ty, result_ty, resolved_len) = match expected {
                    Some(Ty::Array {
                        len: expected_len,
                        elem,
                    }) => {
                        if len.is_some() {
                            self.diagnostics.push(Diagnostic::new(
                                expr.span,
                                "array repeat literal in fixed array context must omit the length",
                            ));
                        }
                        ((**elem).clone(), expected.cloned().unwrap(), *expected_len)
                    }
                    Some(Ty::Slice { mutability, elem }) => {
                        let Some(len) = len else {
                            self.diagnostics.push(Diagnostic::new(
                                expr.span,
                                "array repeat literal with omitted length requires an expected array type",
                            ));
                            return None;
                        };
                        (
                            (**elem).clone(),
                            Ty::Slice {
                                mutability: *mutability,
                                elem: elem.clone(),
                            },
                            *len,
                        )
                    }
                    _ => {
                        self.diagnostics.push(Diagnostic::new(
                            expr.span,
                            "array repeat literal requires an expected array or slice type",
                        ));
                        return None;
                    }
                };
                if self.type_is_affine(&elem_ty) {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!("array repeat cannot copy resource type `{elem_ty}`"),
                    ));
                }
                let checked_element =
                    self.check_consumed_expr(scopes, element, Some(&elem_ty), false)?;
                self.require_assignable(&elem_ty, &checked_element.ty, checked_element.span);
                TExpr {
                    span: expr.span,
                    ty: result_ty,
                    kind: TExprKind::ArrayRepeat {
                        element: Box::new(checked_element),
                        len: resolved_len,
                    },
                }
            }
            ExprKind::Closure {
                is_async,
                params,
                body,
            } => self
                .check_closure_expr(scopes, expr.span, *is_async, params, body, expected, false)?,
            ExprKind::Unary { op, expr: inner } => {
                if matches!(op, UnaryOp::Neg)
                    && let ExprKind::Literal(Literal::Integer(raw)) = &inner.kind
                    && let Some(expected_ty) = expected
                    && expected_ty.is_signed_integer()
                {
                    self.check_integer_literal_range(inner.span, raw, expected_ty, true);
                    let inner = TExpr {
                        span: inner.span,
                        ty: expected_ty.clone(),
                        kind: TExprKind::Literal(Literal::Integer(raw.clone())),
                    };
                    return Some(TExpr {
                        span: expr.span,
                        ty: expected_ty.clone(),
                        kind: TExprKind::Unary {
                            op: *op,
                            expr: Box::new(inner),
                        },
                    });
                }
                let inner = match op {
                    UnaryOp::Addr => {
                        let inner = self.check_lvalue(scopes, inner, true)?;
                        if let Some(ReadOnlyReason::CapturedBinding(name)) =
                            inner.read_only_reason.as_ref()
                        {
                            self.diagnostics.push(Diagnostic::new(
                                inner.expr.span,
                                format!("cannot take address of captured binding `{name}`"),
                            ));
                        }
                        if inner.expr.ty.is_erased_value() {
                            self.diagnostics.push(Diagnostic::new(
                                inner.expr.span,
                                "cannot take the address of a void value",
                            ));
                        }
                        if let TExprKind::Local(local_id, _) = &inner.expr.kind {
                            scopes.clear_narrowing(*local_id);
                        }
                        inner
                    }
                    UnaryOp::Neg => CheckedLvalue::writable(self.check_expr(
                        scopes,
                        inner,
                        expected.filter(|ty| ty.is_numeric()),
                    )?),
                    UnaryOp::BitNot => CheckedLvalue::writable(self.check_expr(
                        scopes,
                        inner,
                        expected.filter(|ty| ty.is_integer()),
                    )?),
                    _ => CheckedLvalue::writable(self.check_expr(scopes, inner, None)?),
                };
                let ty = match op {
                    UnaryOp::Not => {
                        self.require_assignable(&Ty::Bool, &inner.expr.ty, inner.expr.span);
                        Ty::Bool
                    }
                    UnaryOp::Neg => {
                        if !(inner.expr.ty.is_signed_integer()
                            || matches!(inner.expr.ty, Ty::F32 | Ty::F64))
                        {
                            self.diagnostics.push(Diagnostic::new(
                                inner.expr.span,
                                format!("cannot negate `{}`", inner.expr.ty),
                            ));
                        }
                        inner.expr.ty.clone()
                    }
                    UnaryOp::BitNot => {
                        if !inner.expr.ty.is_integer() {
                            self.diagnostics.push(Diagnostic::new(
                                inner.expr.span,
                                format!("bitwise not does not accept `{}`", inner.expr.ty),
                            ));
                        }
                        inner.expr.ty.clone()
                    }
                    UnaryOp::Addr => inner.access.pointer_ty(inner.expr.ty.clone()),
                    UnaryOp::Deref => match &inner.expr.ty {
                        Ty::Pointer {
                            nullable: false,
                            inner,
                            ..
                        } => (**inner).clone(),
                        Ty::Pointer { nullable: true, .. } => {
                            self.diagnostics.push(Diagnostic::new(
                                inner.expr.span,
                                "cannot dereference nullable pointer without narrowing",
                            ));
                            Ty::Unknown
                        }
                        _ => {
                            self.diagnostics.push(Diagnostic::new(
                                inner.expr.span,
                                format!("cannot dereference `{}`", inner.expr.ty),
                            ));
                            Ty::Unknown
                        }
                    },
                };
                TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Unary {
                        op: *op,
                        expr: Box::new(inner.expr),
                    },
                }
            }
            ExprKind::Binary { op, left, right } => {
                let (left, right) = if matches!(op, BinaryOp::And) {
                    let left = self.check_expr(scopes, left, Some(&Ty::Bool))?;
                    self.require_assignable(&Ty::Bool, &left.ty, left.span);
                    let mut right_scopes = scopes.clone();
                    self.apply_condition_narrowing(&mut right_scopes, &left, true);
                    let right = self.check_expr(&mut right_scopes, right, Some(&Ty::Bool))?;
                    (left, right)
                } else if op.is_equality() && matches!(left.kind, ExprKind::Literal(Literal::Null))
                {
                    let right = self.check_expr(scopes, right, None)?;
                    let left = self.check_expr(scopes, left, Some(&right.ty))?;
                    (left, right)
                } else {
                    let left = self.check_expr(scopes, left, None)?;
                    let right_expected = if op.is_shift() { None } else { Some(&left.ty) };
                    let right = self.check_expr(scopes, right, right_expected)?;
                    (left, right)
                };
                let ty = self.check_binary(*op, &left, &right, expr.span);
                TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Binary {
                        op: *op,
                        left: Box::new(left),
                        right: Box::new(right),
                    },
                }
            }
            ExprKind::Cast { expr: inner, ty } => {
                let target = self.lower_type(ty);
                if let ExprKind::Closure {
                    is_async,
                    params,
                    body,
                } = &inner.kind
                {
                    self.check_closure_cast_allowed(&target, expr.span);
                    let checked = self.check_closure_expr(
                        scopes,
                        inner.span,
                        *is_async,
                        params,
                        body,
                        Some(&target),
                        false,
                    )?;
                    let checked = TExpr {
                        span: expr.span,
                        ..checked
                    };
                    return Some(self.coerce_expr_to_expected(scopes, checked, Some(&target)));
                }
                let literal_expected = match (&inner.kind, &target) {
                    (ExprKind::Literal(Literal::Integer(_)), ty)
                        if ty.is_integer() || matches!(ty, Ty::Char | Ty::CSpelling { .. }) =>
                    {
                        true
                    }
                    (
                        ExprKind::StructLiteral(_)
                        | ExprKind::ArrayLiteral(_)
                        | ExprKind::ArrayRepeat { .. }
                        | ExprKind::Literal(Literal::Null),
                        _,
                    ) => true,
                    _ => false,
                };
                let inner = self.check_expr(scopes, inner, literal_expected.then_some(&target))?;
                self.check_cast_allowed(&inner.ty, &target, expr.span);
                self.require_unsafe_pointer_cast_through_void(&inner.ty, &target, expr.span);
                TExpr {
                    span: expr.span,
                    ty: target.clone(),
                    kind: TExprKind::Cast {
                        expr: Box::new(inner),
                        ty: target,
                    },
                }
            }
            ExprKind::UnsafeBlock(block) => {
                self.check_unsafe_block_expr(scopes, block, expected)?
            }
            ExprKind::Call {
                callee,
                type_args,
                args,
            } => {
                if let ExprKind::Field { base, field } = &callee.kind {
                    return self.check_field_or_receiver_selector_call(
                        scopes, expr.span, base, field, type_args, args, expected,
                    );
                }
                if let ExprKind::ReceiverSelector { base, selector } = &callee.kind {
                    return self.check_receiver_selector_call(
                        scopes, expr.span, base, selector, type_args, args, expected,
                    );
                }
                if let ExprKind::Name(name_ref) = &callee.kind
                    && !matches!(name_ref.kind, NameRefKind::Local(_))
                    && let Some((def_id, sig)) = self.lookup_variant_name(name_ref, expected)
                {
                    let variant_name = self.ctx.resolved.def(def_id).name.clone();
                    return self.check_variant_literal(
                        scopes,
                        expr.span,
                        &variant_name,
                        sig,
                        args.clone(),
                        expected,
                    );
                }
                if let ExprKind::Name(name_ref) = &callee.kind
                    && !matches!(name_ref.kind, NameRefKind::Local(_))
                    && let Some(def_id) = self.lookup_interface_name(name_ref)
                {
                    return self.check_interface_call(
                        scopes, expr.span, def_id, type_args, args, expected,
                    );
                }
                if let ExprKind::Name(name_ref) = &callee.kind
                    && !matches!(name_ref.kind, NameRefKind::Local(_))
                    && let Some(sig) = self.resolve_function_name(name_ref)
                {
                    return self.check_direct_function_call(
                        scopes, expr.span, sig, type_args, args, expected,
                    );
                }
                if !type_args.is_empty() {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        "type arguments can only be used on generic function or interface calls",
                    ));
                    return None;
                }
                let callee = self.check_expr(scopes, callee, None)?;
                if matches!(
                    &callee.ty,
                    Ty::Function {
                        is_unsafe: true,
                        ..
                    }
                ) {
                    self.require_unsafe(
                        callee.span,
                        "call to unsafe function value requires unsafe block",
                    );
                }
                let (ret, params) = match &callee.ty {
                    Ty::Function { ret, params, .. }
                    | Ty::Closure { ret, params, .. }
                    | Ty::ClosureInstance { ret, params, .. } => ((**ret).clone(), params.clone()),
                    _ => {
                        self.diagnostics.push(Diagnostic::new(
                            callee.span,
                            format!("`{}` is not callable", callee.ty),
                        ));
                        return None;
                    }
                };
                if params.len() != args.len() {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!(
                            "call expects {} arguments, got {}",
                            params.len(),
                            args.len()
                        ),
                    ));
                }
                let mut checked_args = Vec::new();
                for (idx, arg) in args.iter().enumerate() {
                    let expected = params.get(idx);
                    let checked = self.check_consumed_expr(scopes, arg, expected, false)?;
                    if let Some(expected) = expected {
                        self.require_assignable(expected, &checked.ty, arg.span);
                    }
                    checked_args.push(checked);
                }
                TExpr {
                    span: expr.span,
                    ty: ret,
                    kind: TExprKind::Call {
                        callee: Box::new(callee),
                        args: checked_args,
                    },
                }
            }
            ExprKind::Field { base, field } => {
                let base = self.check_expr(scopes, base, None)?;
                let ty = self.field_ty(&base.ty, &field.name, field.span)?;
                TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Field {
                        base: Box::new(base),
                        field: field.name.clone(),
                    },
                }
            }
            ExprKind::ReceiverSelector { selector, .. } => {
                self.diagnostics.push(Diagnostic::new(
                    expr.span,
                    format!(
                        "receiver selector `{}` must be called",
                        receiver_selector_path_display(selector)
                    ),
                ));
                return None;
            }
            ExprKind::Arrow { base, field } => {
                let base = self.check_expr(scopes, base, None)?;
                let Ty::Pointer {
                    nullable: false,
                    inner,
                    ..
                } = &base.ty
                else {
                    self.diagnostics.push(Diagnostic::new(
                        base.span,
                        format!("`->` requires non-null pointer, got `{}`", base.ty),
                    ));
                    return None;
                };
                let ty = self.field_ty(inner, &field.name, field.span)?;
                TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Arrow {
                        base: Box::new(base),
                        field: field.name.clone(),
                    },
                }
            }
            ExprKind::Index { base, index } => {
                let base = self.check_expr(scopes, base, None)?;
                let index = self.check_expr(scopes, index, Some(&Ty::Usize))?;
                if !index.ty.is_integer() {
                    self.diagnostics
                        .push(Diagnostic::new(index.span, "index must be integer"));
                }
                let ty = match &base.ty {
                    Ty::Array { elem, .. } | Ty::Slice { elem, .. } => (**elem).clone(),
                    _ => {
                        self.diagnostics.push(Diagnostic::new(
                            base.span,
                            format!("cannot index `{}`", base.ty),
                        ));
                        Ty::Unknown
                    }
                };
                TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Index {
                        base: Box::new(base),
                        index: Box::new(index),
                    },
                }
            }
            ExprKind::Slice { base, start, end } => {
                let base = self.check_expr(scopes, base, None)?;
                let start = match start {
                    Some(start) => {
                        let start = self.check_expr(scopes, start, Some(&Ty::Usize))?;
                        if !start.ty.is_integer() {
                            self.diagnostics
                                .push(Diagnostic::new(start.span, "slice start must be integer"));
                        }
                        Some(Box::new(start))
                    }
                    None => None,
                };
                let end = match end {
                    Some(end) => {
                        let end = self.check_expr(scopes, end, Some(&Ty::Usize))?;
                        if !end.ty.is_integer() {
                            self.diagnostics
                                .push(Diagnostic::new(end.span, "slice end must be integer"));
                        }
                        Some(Box::new(end))
                    }
                    None => None,
                };
                let ty = match &base.ty {
                    Ty::Array { elem, .. } => Ty::Slice {
                        mutability: match self.texpr_lvalue_access(scopes, &base) {
                            Some(LvalueAccess::Writable) => ViewMutability::Writable,
                            _ => ViewMutability::ReadOnly,
                        },
                        elem: elem.clone(),
                    },
                    Ty::Slice { mutability, elem } => Ty::Slice {
                        mutability: *mutability,
                        elem: elem.clone(),
                    },
                    _ => {
                        self.diagnostics.push(Diagnostic::new(
                            base.span,
                            format!("cannot slice `{}`", base.ty),
                        ));
                        Ty::Unknown
                    }
                };
                TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Slice {
                        base: Box::new(base),
                        start,
                        end,
                    },
                }
            }
            ExprKind::Try(inner) => {
                let inner = self.check_expr(scopes, inner, None)?;
                let Some((ok_ty, err_ty)) = self.result_ok_err_tys(&inner.ty) else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!(
                            "`?` requires `/std/result` Result<T, E>, got `{}`",
                            inner.ty
                        ),
                    ));
                    return Some(TExpr {
                        span: expr.span,
                        ty: Ty::Unknown,
                        kind: TExprKind::Try {
                            expr: Box::new(inner),
                            propagation: TryPropagation::Exact,
                        },
                    });
                };
                let Some((_, return_err_ty)) = self.result_ok_err_tys(&self.current_return_ty)
                else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!(
                            "`?` requires enclosing function to return `/std/result` Result<_, {}>",
                            err_ty
                        ),
                    ));
                    return Some(TExpr {
                        span: expr.span,
                        ty: Ty::Unknown,
                        kind: TExprKind::Try {
                            expr: Box::new(inner),
                            propagation: TryPropagation::Exact,
                        },
                    });
                };
                let return_err_ty = if contains_type_hole(&return_err_ty) {
                    if self.unify_type_holes(&return_err_ty, &err_ty) {
                        self.resolve_type_holes(&return_err_ty)
                    } else {
                        return_err_ty
                    }
                } else {
                    return_err_ty
                };
                let propagation = if err_ty == return_err_ty {
                    TryPropagation::Exact
                } else if self.is_std_error_ty(&return_err_ty)
                    && self.type_implements_std_error_trait(&err_ty)
                {
                    if self.reject_affine_error_erasure(expr.span, &err_ty, &return_err_ty) {
                        TryPropagation::Exact
                    } else {
                        TryPropagation::ErrorBox
                    }
                } else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        if self.is_std_error_ty(&return_err_ty) {
                            format!(
                                "`?` cannot convert error type `{err_ty}` to `{return_err_ty}` because `{err_ty}` does not implement `{STD_ERROR_FORMAT_INTERFACE}`"
                            )
                        } else {
                            format!(
                                "`?` error type mismatch: expected `{return_err_ty}`, got `{err_ty}`"
                            )
                        },
                    ));
                    TryPropagation::Exact
                };
                let inner = self.consume_affine_expr(scopes, inner, false);
                TExpr {
                    span: expr.span,
                    ty: ok_ty,
                    kind: TExprKind::Try {
                        expr: Box::new(inner),
                        propagation,
                    },
                }
            }
            ExprKind::Await(inner) => {
                if self.current_async_depth == 0 {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        "`await` is allowed only inside async functions or async closures",
                    ));
                }
                if let ExprKind::Select { biased, arms } = &inner.kind {
                    return self
                        .check_async_select_expr(scopes, expr.span, *biased, arms, expected);
                }
                let future = self.check_expr(scopes, inner, None)?;
                let Some(awaitable) = self.awaitable_ty(&future.ty, expr.span) else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!(
                            "generic constraint not satisfied: `{}` does not implement `{}`",
                            future.ty, STD_ASYNC_AWAITABLE_FUTURE_INTERFACE
                        ),
                    ));
                    return Some(TExpr {
                        span: expr.span,
                        ty: Ty::Unknown,
                        kind: TExprKind::Await {
                            future: Box::new(future),
                        },
                    });
                };
                let future = self.consume_affine_expr(scopes, future, false);
                TExpr {
                    span: expr.span,
                    ty: awaitable.output_ty,
                    kind: TExprKind::Await {
                        future: Box::new(future),
                    },
                }
            }
            ExprKind::Select { biased, arms } => {
                self.diagnostics.push(Diagnostic::new(
                    expr.span,
                    "`select` expression must be awaited",
                ));
                self.check_select_future_expr(scopes, expr.span, *biased, arms, expected)?
            }
        };

        Some(result)
    }

    pub(in crate::typeck) fn coerce_expr_to_expected(
        &mut self,
        scopes: &LocalScopes,
        expr: TExpr,
        expected: Option<&Ty>,
    ) -> TExpr {
        let Some(expected) = expected else {
            return expr;
        };
        if contains_type_hole(expected) || contains_type_hole(&expr.ty) {
            self.unify_type_holes(expected, &expr.ty);
        }
        let expected = self.resolve_type_holes(expected);
        let expr_ty = self.resolve_type_holes(&expr.ty);
        if let Ty::Closure {
            ret: expected_ret,
            params: expected_params,
            constraints: expected_constraints,
        } = &expected
            && self.closure_shape_satisfies(expected_ret, expected_params, &expr_ty)
        {
            if self.closure_constraints_satisfied_by_ty(
                expected_constraints,
                &expr_ty,
                expr.span,
                true,
            ) {
                let needs_retain = match &expr_ty {
                    Ty::Closure {
                        constraints: actual_constraints,
                        ..
                    } => actual_constraints != expected_constraints,
                    Ty::ClosureInstance { .. } => !expected_constraints.is_empty(),
                    _ => false,
                };
                if needs_retain {
                    return TExpr {
                        span: expr.span,
                        ty: expected,
                        kind: TExprKind::RetainClosure {
                            expr: Box::new(expr),
                            source_ty: expr_ty,
                        },
                    };
                }
                return TExpr {
                    span: expr.span,
                    ty: expected,
                    kind: expr.kind,
                };
            }
            return expr;
        }
        if closure_instance_satisfies_signature(&expected, &expr_ty) {
            return TExpr {
                span: expr.span,
                ty: expected,
                kind: expr.kind,
            };
        }
        if let (
            Ty::Slice {
                mutability: expected_mutability,
                elem: expected_elem,
            },
            Ty::Slice {
                mutability: actual_mutability,
                elem: actual_elem,
            },
        ) = (&expected, &expr_ty)
            && *expected_mutability == ViewMutability::ReadOnly
            && *actual_mutability == ViewMutability::Writable
            && expected_elem == actual_elem
        {
            return TExpr {
                span: expr.span,
                ty: expected,
                kind: TExprKind::SliceToConst(Box::new(expr)),
            };
        }
        if let (
            Ty::Slice {
                mutability: expected_mutability,
                elem: expected_elem,
            },
            Ty::Array {
                elem: actual_elem, ..
            },
        ) = (&expected, &expr_ty)
            && expected_elem == actual_elem
        {
            let access = self.texpr_lvalue_access(scopes, &expr);
            if *expected_mutability == ViewMutability::Writable
                && !matches!(access, Some(LvalueAccess::Writable))
            {
                self.diagnostics.push(Diagnostic::new(
                    expr.span,
                    self.type_mismatch_message(&expected, &expr_ty),
                ));
                return expr;
            }
            return TExpr {
                span: expr.span,
                ty: Ty::Slice {
                    mutability: *expected_mutability,
                    elem: expected_elem.clone(),
                },
                kind: TExprKind::ArrayToSlice(Box::new(expr)),
            };
        }
        if self.ty_can_assign_from(&expected, &expr_ty)
            || self.meta_repr_storage_equivalent(&expected, &expr_ty)
            || contains_generic(&expected)
            || matches!(expr_ty, Ty::Unknown)
        {
            return TExpr {
                span: expr.span,
                ty: if contains_type_hole(&expr.ty) {
                    expected
                } else {
                    expr.ty
                },
                kind: expr.kind,
            };
        }
        if self.is_std_error_ty(&expected) && self.type_implements_std_error_trait(&expr_ty) {
            if self.reject_affine_error_erasure(expr.span, &expr_ty, &expected) {
                return expr;
            }
            return TExpr {
                span: expr.span,
                ty: expected,
                kind: TExprKind::ErrorBox {
                    concrete_ty: expr_ty,
                    expr: Box::new(expr),
                },
            };
        }
        if let (
            Ty::Pointer {
                nullable: false,
                mutability: expected_mutability,
                inner: expected_inner,
            },
            Ty::Pointer {
                nullable: true,
                mutability: actual_mutability,
                inner: actual_inner,
            },
        ) = (&expected, &expr_ty)
            && expected_inner == actual_inner
            && expected_mutability == actual_mutability
            && matches!(expr.kind, TExprKind::Literal(Literal::Null))
        {
            return expr;
        }
        if let Ty::DynamicInterface { name, args } = &expected
            && self.type_satisfies_dynamic_view(name, args, &expr_ty)
        {
            if self.type_is_affine(&expr_ty) {
                self.diagnostics.push(Diagnostic::new(
                    expr.span,
                    format!(
                        "cannot erase resource-affine type `{expr_ty}` into dynamic interface `{expected}`"
                    ),
                ));
                return expr;
            }
            return TExpr {
                span: expr.span,
                ty: expected,
                kind: TExprKind::MakeDynamicInterface {
                    concrete_ty: expr_ty,
                    expr: Box::new(expr),
                },
            };
        }
        if let Ty::Closure {
            ret: expected_ret,
            params: expected_params,
            constraints: expected_constraints,
        } = &expected
            && let Ty::Function {
                is_unsafe: false,
                abi: None,
                ret: actual_ret,
                params: actual_params,
            } = &expr_ty
            && expected_params == actual_params
            && self.ty_can_assign_from(expected_ret, actual_ret)
            && self.closure_constraints_satisfied_by_ty(
                expected_constraints,
                &expr_ty,
                expr.span,
                true,
            )
        {
            return TExpr {
                span: expr.span,
                ty: expected,
                kind: TExprKind::FunctionToClosure(Box::new(expr)),
            };
        }
        self.diagnostics.push(Diagnostic::new(
            expr.span,
            self.type_mismatch_message(&expected, &expr_ty),
        ));
        expr
    }
}
