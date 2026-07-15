use super::*;

impl TypeChecker {
    pub(in crate::typeck) fn check_lvalue(
        &mut self,
        scopes: &mut LocalScopes,
        expr: &Expr,
        require_assigned: bool,
    ) -> Option<CheckedLvalue> {
        match &expr.kind {
            ExprKind::Name(name_ref) => {
                let Some(local_id) = self.resolved_local_id(name_ref) else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!("unresolved local `{}`", name_ref.display),
                    ));
                    return None;
                };
                let Some(binding) = scopes.get(local_id) else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!("unresolved local `{}`", name_ref.display),
                    ));
                    return None;
                };
                let name = binding.name.clone();
                let init_state = binding.init_state;
                let binding_ty = binding
                    .flow_ty
                    .clone()
                    .unwrap_or_else(|| binding.ty.clone());
                if require_assigned && !binding.init_state.is_assigned() {
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
                let expr = TExpr {
                    span: expr.span,
                    ty: binding_ty,
                    kind: TExprKind::Local(local_id, name),
                };
                if binding.captured {
                    Some(CheckedLvalue::read_only(
                        expr,
                        ReadOnlyReason::CapturedBinding(binding.name.clone()),
                    ))
                } else if binding.mutability == BindingMutability::Mutable {
                    Some(CheckedLvalue::writable(expr))
                } else {
                    Some(CheckedLvalue::read_only(
                        expr,
                        ReadOnlyReason::ImmutableBinding(binding.name.clone()),
                    ))
                }
            }
            ExprKind::Field { base, field } => {
                let base = self.check_lvalue(scopes, base, true)?;
                let ty = self.field_ty(&base.expr.ty, &field.name, field.span)?;
                let expr = TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Field {
                        base: Box::new(base.expr),
                        field: field.name.clone(),
                    },
                };
                Some(CheckedLvalue {
                    expr,
                    access: base.access,
                    read_only_reason: base.read_only_reason,
                })
            }
            ExprKind::Arrow { base, field } => {
                let base = self.check_expr(scopes, base, None)?;
                let (mutability, ty) = {
                    let Ty::Pointer {
                        nullable: false,
                        mutability,
                        inner,
                    } = &base.ty
                    else {
                        self.diagnostics.push(Diagnostic::new(
                            base.span,
                            format!("`->` requires non-null pointer, got `{}`", base.ty),
                        ));
                        return None;
                    };
                    (*mutability, self.field_ty(inner, &field.name, field.span)?)
                };
                let expr = TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Arrow {
                        base: Box::new(base),
                        field: field.name.clone(),
                    },
                };
                Some(CheckedLvalue::from_view(
                    expr,
                    mutability,
                    ReadOnlyReason::ReadOnlyPointer,
                ))
            }
            ExprKind::Index { base, index } => {
                let base_expr = self.check_expr(scopes, base, None)?;
                let index = self.check_expr(scopes, index, Some(&Ty::Usize))?;
                if !index.ty.is_integer() {
                    self.diagnostics
                        .push(Diagnostic::new(index.span, "index must be integer"));
                }
                match base_expr.ty.clone() {
                    Ty::OpaqueState {
                        base: opaque_base,
                        state,
                    } => match *opaque_base {
                        Ty::Slice { mutability, elem } => {
                            let elem_ty = self.project_opaque_index_ty((*elem).clone(), &state);
                            let expr = TExpr {
                                span: expr.span,
                                ty: elem_ty,
                                kind: TExprKind::Index {
                                    base: Box::new(base_expr),
                                    index: Box::new(index),
                                },
                            };
                            Some(CheckedLvalue::from_view(
                                expr,
                                mutability,
                                ReadOnlyReason::ReadOnlySlice,
                            ))
                        }
                        Ty::Array { elem, .. } => {
                            let base = self.check_lvalue(scopes, base, true)?;
                            let elem_ty = self.project_opaque_index_ty((*elem).clone(), &state);
                            let expr = TExpr {
                                span: expr.span,
                                ty: elem_ty,
                                kind: TExprKind::Index {
                                    base: Box::new(base.expr),
                                    index: Box::new(index),
                                },
                            };
                            Some(CheckedLvalue {
                                expr,
                                access: base.access,
                                read_only_reason: base.read_only_reason,
                            })
                        }
                        other => {
                            self.diagnostics.push(Diagnostic::new(
                                base_expr.span,
                                format!("cannot index `{}`", other),
                            ));
                            None
                        }
                    },
                    Ty::Slice { mutability, elem } => {
                        let expr = TExpr {
                            span: expr.span,
                            ty: (*elem).clone(),
                            kind: TExprKind::Index {
                                base: Box::new(base_expr),
                                index: Box::new(index),
                            },
                        };
                        Some(CheckedLvalue::from_view(
                            expr,
                            mutability,
                            ReadOnlyReason::ReadOnlySlice,
                        ))
                    }
                    Ty::Array { elem, .. } => {
                        let base = self.check_lvalue(scopes, base, true)?;
                        let expr = TExpr {
                            span: expr.span,
                            ty: (*elem).clone(),
                            kind: TExprKind::Index {
                                base: Box::new(base.expr),
                                index: Box::new(index),
                            },
                        };
                        Some(CheckedLvalue {
                            expr,
                            access: base.access,
                            read_only_reason: base.read_only_reason,
                        })
                    }
                    _ => {
                        self.diagnostics.push(Diagnostic::new(
                            base_expr.span,
                            format!("cannot index `{}`", base_expr.ty),
                        ));
                        None
                    }
                }
            }
            ExprKind::Unary {
                op: UnaryOp::Deref,
                expr: inner,
            } => {
                let inner = self.check_expr(scopes, inner, None)?;
                let (ty, mutability) = match &inner.ty {
                    Ty::Pointer {
                        nullable: false,
                        mutability,
                        inner,
                        ..
                    } => ((**inner).clone(), *mutability),
                    Ty::Pointer { nullable: true, .. } => {
                        self.diagnostics.push(Diagnostic::new(
                            inner.span,
                            format!("`*` requires non-null pointer, got `{}`", inner.ty),
                        ));
                        (Ty::Unknown, ViewMutability::ReadOnly)
                    }
                    _ => {
                        self.diagnostics.push(Diagnostic::new(
                            inner.span,
                            format!("cannot dereference `{}`", inner.ty),
                        ));
                        (Ty::Unknown, ViewMutability::ReadOnly)
                    }
                };
                let expr = TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Unary {
                        op: UnaryOp::Deref,
                        expr: Box::new(inner),
                    },
                };
                Some(CheckedLvalue::from_view(
                    expr,
                    mutability,
                    ReadOnlyReason::ReadOnlyPointer,
                ))
            }
            _ => {
                self.diagnostics
                    .push(Diagnostic::new(expr.span, "expression is not assignable"));
                None
            }
        }
    }

    pub(in crate::typeck) fn validate_assignment_target(
        &mut self,
        scopes: &LocalScopes,
        target: &CheckedLvalue,
        span: crate::span::Span,
    ) -> bool {
        if self.type_is_affine(&target.expr.ty) && !matches!(target.expr.kind, TExprKind::Local(..))
        {
            if self.unsafe_depth == 0 {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "cannot replace resource subvalue of type `{}`",
                        target.expr.ty
                    ),
                ));
                return false;
            }
        }
        if target.access.is_writable() {
            return true;
        }
        if let TExprKind::Local(local_id, name) = &target.expr.kind {
            let Some(binding) = scopes.get(*local_id) else {
                return false;
            };
            if binding.captured {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("cannot mutate captured binding `{name}`"),
                ));
                return false;
            }
            if binding.mutability == BindingMutability::Immutable {
                match binding.init_state {
                    InitState::Unassigned => {
                        if binding.declared_loop_depth < self.current_loop_depth {
                            self.diagnostics.push(Diagnostic::new(
                                span,
                                format!(
                                    "cannot initialize immutable binding `{name}` from a loop body"
                                ),
                            ));
                            return false;
                        }
                        return true;
                    }
                    InitState::Assigned => {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            format!("cannot initialize immutable binding `{name}` more than once"),
                        ));
                        return false;
                    }
                    InitState::Moved => {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            format!("cannot reinitialize moved immutable binding `{name}`"),
                        ));
                        return false;
                    }
                    InitState::MaybeAssigned => {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            format!("cannot initialize maybe-assigned immutable binding `{name}`"),
                        ));
                        return false;
                    }
                    InitState::MaybeMoved => {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            format!("cannot reinitialize maybe-moved immutable binding `{name}`"),
                        ));
                        return false;
                    }
                }
            }
        }

        match target.read_only_reason.as_ref() {
            Some(ReadOnlyReason::CapturedBinding(name)) => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("cannot mutate captured binding `{name}`"),
                ));
            }
            Some(ReadOnlyReason::ImmutableBinding(name)) => {
                if let Some((local_id, _)) = lvalue_root_local(&target.expr)
                    && let Some(binding) = scopes.get(local_id)
                    && binding.init_state == InitState::Unassigned
                {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!("cannot partially initialize immutable binding `{name}`"),
                    ));
                    return false;
                }
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("cannot mutate immutable binding `{name}`"),
                ));
            }
            Some(ReadOnlyReason::ReadOnlyPointer) => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    "cannot write through read-only pointer",
                ));
            }
            Some(ReadOnlyReason::ReadOnlySlice) => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    "cannot write through read-only slice",
                ));
            }
            None => {
                self.diagnostics
                    .push(Diagnostic::new(span, "expression is not writable"));
            }
        }
        false
    }

    pub(in crate::typeck) fn mark_assignment_complete(
        &mut self,
        scopes: &mut LocalScopes,
        target: &TExpr,
        value_ty: &Ty,
    ) {
        if let TExprKind::Local(local_id, _) = &target.kind
            && let Some(binding) = scopes.get_mut(*local_id)
        {
            let flow_ty = self.assignment_flow_ty(&binding.ty, value_ty);
            binding.init_state = InitState::Assigned;
            binding.flow_ty = flow_ty;
        }
    }

    fn assignment_flow_ty(&self, storage_ty: &Ty, value_ty: &Ty) -> Option<Ty> {
        let storage_base = match storage_ty {
            Ty::OpaqueState { base, .. } => base.as_ref(),
            _ => storage_ty,
        };
        match value_ty {
            Ty::OpaqueState { base, state } => {
                if storage_base.can_assign_from(base)
                    || std_id::std_async_future_accepts_generated(
                        &self.ctx.resolved,
                        storage_base,
                        base,
                    )
                {
                    Some(opaque_state_ty(storage_base.clone(), state.clone()))
                } else if storage_ty.can_assign_from(base) {
                    Some(opaque_state_ty(storage_ty.clone(), state.clone()))
                } else {
                    None
                }
            }
            Ty::GeneratedFuture { .. }
                if std_id::std_async_future_accepts_generated(
                    &self.ctx.resolved,
                    storage_base,
                    value_ty,
                ) =>
            {
                Some(value_ty.clone())
            }
            _ => None,
        }
    }

    pub(super) fn texpr_lvalue_access(
        &self,
        scopes: &LocalScopes,
        expr: &TExpr,
    ) -> Option<LvalueAccess> {
        match &expr.kind {
            TExprKind::Local(local_id, _) => scopes.get(*local_id).map(|binding| {
                if !binding.captured && binding.mutability == BindingMutability::Mutable {
                    LvalueAccess::Writable
                } else {
                    LvalueAccess::ReadOnly
                }
            }),
            TExprKind::Field { base, .. } => self.texpr_lvalue_access(scopes, base),
            TExprKind::Arrow { base, .. } => match &base.ty {
                Ty::Pointer { mutability, .. } => Some(LvalueAccess::from_view(*mutability)),
                _ => None,
            },
            TExprKind::Index { base, .. } => match &base.ty {
                Ty::Slice { mutability, .. } => Some(LvalueAccess::from_view(*mutability)),
                Ty::Array { .. } => self.texpr_lvalue_access(scopes, base),
                Ty::OpaqueState {
                    base: opaque_base, ..
                } => match opaque_base.as_ref() {
                    Ty::Slice { mutability, .. } => Some(LvalueAccess::from_view(*mutability)),
                    Ty::Array { .. } => self.texpr_lvalue_access(scopes, base),
                    _ => None,
                },
                _ => None,
            },
            TExprKind::Unary {
                op: UnaryOp::Deref,
                expr,
            } => match &expr.ty {
                Ty::Pointer { mutability, .. } => Some(LvalueAccess::from_view(*mutability)),
                _ => None,
            },
            _ => None,
        }
    }

    pub(super) fn check_variant_literal(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        variant_name: &str,
        sig: VariantSig,
        args: Vec<Expr>,
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        let enum_ty = match expected {
            Some(Ty::Named { def_id, name, args })
                if def_id == &Some(sig.enum_def_id)
                    || (def_id.is_none() && name == &sig.enum_name) =>
            {
                named_ty(*def_id, name.clone(), args.clone())
            }
            Some(other)
                if (self.is_std_error_ty(other) || self.is_std_report_ty(other))
                    && sig.enum_generics.is_empty() =>
            {
                named_ty(Some(sig.enum_def_id), sig.enum_name.clone(), Vec::new())
            }
            Some(other) => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "variant `{variant_name}` constructs `{}`, not `{other}`",
                        sig.enum_name
                    ),
                ));
                return None;
            }
            None if sig.enum_generics.is_empty() => {
                named_ty(Some(sig.enum_def_id), sig.enum_name.clone(), Vec::new())
            }
            None => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("generic variant `{variant_name}` requires an expected enum type"),
                ));
                return None;
            }
        };

        let Ty::Named {
            name: enum_name,
            args: enum_args,
            ..
        } = &enum_ty
        else {
            unreachable!("variant enum type is always named");
        };
        if enum_args.len() != sig.enum_generics.len() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "enum `{enum_name}` expects {} type arguments, got {}",
                    sig.enum_generics.len(),
                    enum_args.len()
                ),
            ));
            return None;
        }
        let subst = sig
            .enum_generics
            .iter()
            .cloned()
            .zip(enum_args.iter().cloned())
            .collect::<HashMap<_, _>>();
        let logical_payload_tys = sig
            .payload
            .iter()
            .map(|ty| self.lower_type_with_subst(ty, &subst))
            .collect::<Vec<_>>();
        let physical_payload_tys = logical_payload_tys
            .iter()
            .filter(|ty| !ty.is_erased_value())
            .cloned()
            .collect::<Vec<_>>();
        let use_logical_payload = args.len() == logical_payload_tys.len();
        let use_physical_payload = args.len() == physical_payload_tys.len() && !use_logical_payload;
        if !use_logical_payload && !use_physical_payload {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "variant `{variant_name}` expects {} payload values, got {}",
                    physical_payload_tys.len(),
                    args.len()
                ),
            ));
            return None;
        }

        let mut payload = Vec::new();
        let payload_inputs = if use_logical_payload {
            args.iter()
                .zip(logical_payload_tys.iter())
                .collect::<Vec<_>>()
        } else {
            args.iter()
                .zip(physical_payload_tys.iter())
                .collect::<Vec<_>>()
        };
        for (arg, expected_ty) in payload_inputs {
            let expected_ty = self.resolve_type_holes(expected_ty);
            let checked = self.check_consumed_expr(scopes, arg, Some(&expected_ty), false)?;
            self.require_assignable(&expected_ty, &checked.ty, checked.span);
            if use_logical_payload || !expected_ty.is_erased_value() {
                payload.push(checked);
            }
        }

        let enum_ty = self.resolve_type_holes(&enum_ty);
        self.ensure_enum_instance(&enum_ty);
        let Ty::Named {
            name: enum_name,
            args: enum_args,
            ..
        } = &enum_ty
        else {
            unreachable!("variant enum type is always named");
        };
        let type_name = enum_instance_name(enum_name, enum_args);
        let hidden_state = payload
            .iter()
            .enumerate()
            .filter(|(_, value)| ty_has_hidden_state(&value.ty))
            .map(|(idx, value)| (format!("{variant_name}#{idx}"), value.ty.clone()))
            .collect::<Vec<_>>();
        let enum_ty = opaque_state_ty(enum_ty, hidden_state);
        Some(TExpr {
            span,
            ty: enum_ty,
            kind: TExprKind::EnumLiteral {
                type_name,
                variant_name: variant_name.to_string(),
                variant_index: sig.variant_index,
                payload,
            },
        })
    }

    pub(super) fn check_literal(
        &mut self,
        span: crate::span::Span,
        literal: &Literal,
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        let ty = match literal {
            Literal::Integer(raw) => {
                let ty = expected
                    .filter(|ty| ty.is_integer() || matches!(ty, Ty::Char | Ty::CSpelling { .. }))
                    .cloned()
                    .unwrap_or(Ty::I64);
                if ty.is_integer() || matches!(ty, Ty::Char) {
                    self.check_integer_literal_range(span, raw, &ty, false);
                }
                ty
            }
            Literal::Float(raw) => {
                let ty = expected
                    .filter(|ty| matches!(ty, Ty::F32 | Ty::F64 | Ty::CSpelling { .. }))
                    .cloned()
                    .unwrap_or(Ty::F64);
                if matches!(ty, Ty::F32 | Ty::F64) {
                    self.check_float_literal_range(span, raw, &ty);
                }
                ty
            }
            Literal::Char(raw) => {
                self.check_char_literal_range(span, raw);
                Ty::Char
            }
            Literal::String(_) => match expected {
                Some(Ty::Slice {
                    mutability: ViewMutability::ReadOnly,
                    elem,
                }) if matches!(&**elem, Ty::Char | Ty::U8) => Ty::Slice {
                    mutability: ViewMutability::ReadOnly,
                    elem: elem.clone(),
                },
                _ => Ty::Slice {
                    mutability: ViewMutability::ReadOnly,
                    elem: Box::new(Ty::Char),
                },
            },
            Literal::Bool(_) => Ty::Bool,
            Literal::Null => match expected {
                Some(Ty::Pointer {
                    inner, mutability, ..
                }) => Ty::Pointer {
                    nullable: true,
                    mutability: *mutability,
                    inner: inner.clone(),
                },
                _ => {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        "`null` requires an expected nullable pointer type",
                    ));
                    Ty::Unknown
                }
            },
        };
        Some(TExpr {
            span,
            ty,
            kind: TExprKind::Literal(literal.clone()),
        })
    }

    pub(super) fn check_integer_literal_range(
        &mut self,
        span: crate::span::Span,
        raw: &str,
        ty: &Ty,
        negated: bool,
    ) {
        let Some(value) = parse_integer_literal_u128(raw) else {
            self.diagnostics
                .push(Diagnostic::new(span, "integer literal is out of range"));
            return;
        };
        let Some((min_abs, max)) = integer_abs_limits(ty) else {
            return;
        };
        let limit = if negated { min_abs } else { max };
        if value > limit {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("integer literal `{raw}` is out of range for `{ty}`"),
            ));
        }
    }

    pub(super) fn check_float_literal_range(
        &mut self,
        span: crate::span::Span,
        raw: &str,
        ty: &Ty,
    ) {
        let normalized = raw.replace('_', "");
        let Ok(value) = normalized.parse::<f64>() else {
            self.diagnostics
                .push(Diagnostic::new(span, "float literal is invalid"));
            return;
        };
        if matches!(ty, Ty::F32) && value.is_finite() && value.abs() > f32::MAX as f64 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("float literal `{raw}` is out of range for `f32`"),
            ));
        }
    }

    pub(super) fn check_char_literal_range(&mut self, span: crate::span::Span, raw: &str) {
        if decode_char_literal_byte(raw).is_none() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("char literal `{raw}` is not a single byte"),
            ));
        }
    }

    pub(super) fn check_binary(
        &mut self,
        op: BinaryOp,
        left: &TExpr,
        right: &TExpr,
        span: crate::span::Span,
    ) -> Ty {
        use BinaryOp::*;
        match op {
            Or | And => {
                self.require_assignable(&Ty::Bool, &left.ty, left.span);
                self.require_assignable(&Ty::Bool, &right.ty, right.span);
                Ty::Bool
            }
            Eq | Ne => {
                if !self.ty_can_assign_from(&left.ty, &right.ty)
                    && !self.ty_can_assign_from(&right.ty, &left.ty)
                {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!("cannot compare `{}` and `{}`", left.ty, right.ty),
                    ));
                }
                if self.is_c_aggregate_value(&left.ty) || self.is_c_aggregate_value(&right.ty) {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        "struct, enum, slice, and dynamic interface values cannot be compared directly",
                    ));
                }
                if matches!(left.ty, Ty::Closure { .. } | Ty::ClosureInstance { .. })
                    || matches!(right.ty, Ty::Closure { .. } | Ty::ClosureInstance { .. })
                {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        "closure values cannot be compared directly",
                    ));
                }
                Ty::Bool
            }
            Lt | Le | Gt | Ge => {
                if !left.ty.is_numeric() && !matches!(left.ty, Ty::Char) {
                    self.diagnostics.push(Diagnostic::new(
                        left.span,
                        format!("relational operator does not accept `{}`", left.ty),
                    ));
                }
                self.require_assignable(&left.ty, &right.ty, right.span);
                Ty::Bool
            }
            Add | Sub | Mul | Div | Rem => {
                if !left.ty.is_numeric() {
                    self.diagnostics.push(Diagnostic::new(
                        left.span,
                        format!("arithmetic operator does not accept `{}`", left.ty),
                    ));
                }
                if matches!(op, Rem) && !left.ty.is_integer() {
                    self.diagnostics
                        .push(Diagnostic::new(left.span, "`%` requires integer operands"));
                }
                self.require_assignable(&left.ty, &right.ty, right.span);
                left.ty.clone()
            }
            BitOr | BitXor | BitAnd => {
                if !left.ty.is_integer() {
                    self.diagnostics.push(Diagnostic::new(
                        left.span,
                        format!("bitwise operator does not accept `{}`", left.ty),
                    ));
                }
                self.require_same_integer_type("bitwise operator", left, right, span);
                left.ty.clone()
            }
            Shl | Shr => {
                if !left.ty.is_integer() {
                    self.diagnostics.push(Diagnostic::new(
                        left.span,
                        format!("shift operator does not accept `{}`", left.ty),
                    ));
                }
                if !right.ty.is_integer() {
                    self.diagnostics.push(Diagnostic::new(
                        right.span,
                        format!("shift count must be an integer, got `{}`", right.ty),
                    ));
                }
                self.check_constant_shift_count(left, right);
                left.ty.clone()
            }
        }
    }

    pub(super) fn require_same_integer_type(
        &mut self,
        context: &str,
        left: &TExpr,
        right: &TExpr,
        span: crate::span::Span,
    ) {
        if matches!(left.ty, Ty::Unknown) || matches!(right.ty, Ty::Unknown) {
            return;
        }
        if !left.ty.is_integer() || !right.ty.is_integer() {
            return;
        }
        if left.ty != right.ty {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "{context} requires matching integer types, got `{}` and `{}`",
                    left.ty, right.ty
                ),
            ));
        }
    }

    pub(super) fn check_constant_shift_count(&mut self, left: &TExpr, right: &TExpr) {
        let Some(width) = left.ty.integer_bit_width() else {
            return;
        };
        let count = match &right.kind {
            TExprKind::Literal(Literal::Integer(raw)) => {
                let Some(count) = parse_integer_literal_u128(raw) else {
                    return;
                };
                (raw.clone(), Some(count))
            }
            TExprKind::Unary {
                op: UnaryOp::Neg,
                expr,
            } => {
                let TExprKind::Literal(Literal::Integer(raw)) = &expr.kind else {
                    return;
                };
                (format!("-{raw}"), None)
            }
            _ => return,
        };
        if count.1.is_none_or(|value| value >= u128::from(width)) {
            self.diagnostics.push(Diagnostic::new(
                right.span,
                format!(
                    "constant shift count `{}` is out of range for `{}`; expected 0..{}",
                    count.0,
                    left.ty,
                    width - 1
                ),
            ));
        }
    }

    pub(super) fn field_ty(
        &mut self,
        base: &Ty,
        field: &str,
        span: crate::span::Span,
    ) -> Option<Ty> {
        let view = self.meta_repr_field_view_ty(base, span);
        match &view {
            Ty::OpaqueState { base, state } => {
                let field_ty = self.field_ty(base, field, span)?;
                Some(self.project_opaque_field_ty(field_ty, state, field))
            }
            Ty::Slice { mutability, elem } if field == "ptr" => Some(Ty::Pointer {
                nullable: false,
                mutability: *mutability,
                inner: Box::new((**elem).clone()),
            }),
            Ty::Slice { .. } if field == "len" => Some(Ty::Usize),
            Ty::Named { name, args, .. } => {
                let instance_name = enum_instance_name(name, args);
                let fields = if let Some(fields) = self.ctx.structs.get(&instance_name).cloned() {
                    fields
                } else if let Some(template) = self.ctx.struct_templates.get(name).cloned() {
                    if template.generics.len() != args.len() {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            format!(
                                "struct `{name}` expects {} type arguments, got {}",
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
                    self.check_generic_constraints(&template.generics, &subst, span);
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
                        span,
                        format!("type `{view}` has no field `{field}`"),
                    ));
                    return None;
                };
                if let Some((_, ty)) = fields.iter().find(|(candidate, _)| candidate == field) {
                    let ty = ty.clone();
                    if self.is_unsafe_struct_instance(name, args) {
                        self.require_unsafe(
                            span,
                            format!("field access on unsafe struct `{name}` requires unsafe block"),
                        );
                    }
                    Some(ty)
                } else {
                    let mut diagnostic =
                        Diagnostic::new(span, format!("unknown field `{field}` on `{view}`"));
                    if field.is_empty() {
                        if let Some(note) = suggest::available_names_note(
                            "available fields",
                            fields.iter().map(|(name, _)| name),
                        ) {
                            diagnostic = diagnostic.note(note);
                        }
                    } else if let Some(note) =
                        suggest::did_you_mean_note(field, fields.iter().map(|(name, _)| name))
                    {
                        diagnostic = diagnostic.note(note);
                    }
                    self.diagnostics.push(diagnostic);
                    None
                }
            }
            _ => {
                let mut diagnostic =
                    Diagnostic::new(span, format!("type `{view}` has no field `{field}`"));
                if let Ty::Slice { .. } = view
                    && let Some(note) = suggest::did_you_mean_note(field, ["ptr", "len"])
                {
                    diagnostic = diagnostic.note(note);
                }
                self.diagnostics.push(diagnostic);
                None
            }
        }
    }

    pub(super) fn field_ty_silent(
        &mut self,
        base: &Ty,
        field: &str,
        span: crate::span::Span,
    ) -> Option<Ty> {
        let diagnostic_count = self.diagnostics.len();
        let ty = self.field_ty(base, field, span);
        self.diagnostics.truncate(diagnostic_count);
        ty
    }

    pub(super) fn ty_can_assign_from(&self, expected: &Ty, actual: &Ty) -> bool {
        expected.can_assign_from(actual)
            || std_id::std_async_future_accepts_generated(&self.ctx.resolved, expected, actual)
    }

    pub(super) fn closure_shape_satisfies(
        &self,
        expected_ret: &Ty,
        expected_params: &[Ty],
        actual: &Ty,
    ) -> bool {
        match actual {
            Ty::Closure {
                ret: actual_ret,
                params: actual_params,
                ..
            }
            | Ty::ClosureInstance {
                ret: actual_ret,
                params: actual_params,
                ..
            } => {
                expected_params == actual_params
                    && self.ty_can_assign_from(expected_ret, actual_ret)
            }
            _ => false,
        }
    }

    pub(in crate::typeck) fn require_assignable(
        &mut self,
        expected: &Ty,
        actual: &Ty,
        span: crate::span::Span,
    ) {
        self.require_assignable_with_context(expected, actual, span, None);
    }

    pub(in crate::typeck) fn require_assignable_argument(
        &mut self,
        expected: &Ty,
        actual: &Ty,
        span: crate::span::Span,
        param_display: Option<&str>,
    ) {
        let context = param_display.map(|display| format!("argument `{display}`"));
        self.require_assignable_with_context(expected, actual, span, context.as_deref());
    }

    fn require_assignable_with_context(
        &mut self,
        expected: &Ty,
        actual: &Ty,
        span: crate::span::Span,
        context: Option<&str>,
    ) {
        if contains_type_hole(expected) || contains_type_hole(actual) {
            self.unify_type_holes(expected, actual);
        }
        let expected = self.resolve_type_holes(expected);
        let actual = self.resolve_type_holes(actual);
        let expected = self.meta_repr_storage_ty(&expected, span);
        let actual = self.meta_repr_storage_ty(&actual, span);
        if contains_generic(&expected) || contains_generic(&actual) {
            return;
        }
        if matches!(expected, Ty::Unknown) || matches!(actual, Ty::Unknown) {
            return;
        }
        if self.meta_repr_storage_equivalent(&expected, &actual) {
            return;
        }
        if let Ty::Closure {
            ret,
            params,
            constraints,
        } = &expected
            && self.closure_shape_satisfies(ret, params, &actual)
        {
            self.closure_constraints_satisfied_by_ty(constraints, &actual, span, false);
            return;
        }
        if !self.ty_can_assign_from(&expected, &actual) {
            let message = match context {
                Some(context) => format!(
                    "{context}: {}",
                    self.type_mismatch_message(&expected, &actual)
                ),
                None => self.type_mismatch_message(&expected, &actual),
            };
            self.diagnostics.push(Diagnostic::new(span, message));
        }
    }

    pub(super) fn type_mismatch_message(&self, expected: &Ty, actual: &Ty) -> String {
        if let (
            Ty::OpaqueReturn {
                key: expected_key, ..
            },
            Ty::OpaqueReturn {
                key: actual_key, ..
            },
        ) = (expected, actual)
        {
            return format!(
                "cannot assign opaque return type from {} to opaque return type from {}; opaque return identities are distinct (expected `{expected}`, got `{actual}`)",
                self.opaque_return_origin_label(actual_key),
                self.opaque_return_origin_label(expected_key),
            );
        }
        format!("expected `{expected}`, got `{actual}`")
    }

    pub(super) fn opaque_return_origin_label(&self, key: &OpaqueReturnKey) -> String {
        let name = &self.ctx.resolved.def(key.def_id).name;
        if key.args.is_empty() {
            return format!("function `{name}`");
        }
        let args = key
            .args
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        format!("function `{name}<{args}>`")
    }

    pub(in crate::typeck) fn record_opaque_return_ty(
        &mut self,
        opaque_ty: &Ty,
        concrete_ty: &Ty,
        span: crate::span::Span,
    ) {
        if concrete_ty.is_never() || matches!(concrete_ty, Ty::Unknown) {
            return;
        }
        let concrete_ty = self.resolve_type_holes(concrete_ty);
        let Some(current_opaque_ty) = self
            .current_opaque_return
            .as_ref()
            .map(|state| state.opaque_ty.clone())
        else {
            return;
        };
        if self.opaque_return_concrete_ty_is_recursive(&current_opaque_ty, &concrete_ty) {
            if let Ty::OpaqueReturn { key, .. } = &current_opaque_ty {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "opaque return {} cannot use an opaque return type that resolves back to itself",
                        self.opaque_return_origin_label(key),
                    ),
                ));
            }
            if let Some(state) = self.current_opaque_return.as_mut() {
                state.saw_recursive_concrete_ty = true;
            }
            return;
        }
        self.check_opaque_return_bounds(opaque_ty, &concrete_ty, span);
        let Some(state) = self.current_opaque_return.as_mut() else {
            return;
        };
        match &state.concrete_ty {
            Some(existing) if existing != &concrete_ty => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("opaque return function returns both `{existing}` and `{concrete_ty}`"),
                ));
            }
            Some(_) => {}
            None => {
                state.concrete_ty = Some(concrete_ty);
            }
        }
    }

    pub(in crate::typeck) fn opaque_return_concrete_ty_is_recursive(
        &self,
        opaque_ty: &Ty,
        concrete_ty: &Ty,
    ) -> bool {
        opaque_return_concrete_ty_is_recursive(opaque_ty, concrete_ty, &self.opaque_returns)
    }

    pub(super) fn check_opaque_return_bounds(
        &mut self,
        opaque_ty: &Ty,
        concrete_ty: &Ty,
        span: crate::span::Span,
    ) {
        let Ty::OpaqueReturn { bounds, .. } = opaque_ty else {
            return;
        };
        for capability in &bounds.positive {
            if !self.type_implements_capability_ref(capability, concrete_ty) {
                self.diagnostics.push(
                    Diagnostic::new(
                        span,
                        format!(
                            "opaque return type requires `{concrete_ty}` to implement `{}`",
                            capability.name
                        ),
                    )
                    .note(format!(
                        "required capability: `{}`",
                        display_constraint_ref(capability)
                    ))
                    .note(format!(
                        "opaque return bounds: `{}`",
                        display_constraint_bounds(bounds)
                    )),
                );
            }
        }
        for capability in &bounds.negative {
            if self.type_implements_capability_ref(capability, concrete_ty) {
                self.diagnostics.push(
                    Diagnostic::new(
                        span,
                        format!(
                            "opaque return type forbids `{concrete_ty}` from implementing `{}`",
                            capability.name
                        ),
                    )
                    .note(format!(
                        "forbidden capability: `{}`",
                        display_constraint_ref(capability)
                    ))
                    .note(format!(
                        "opaque return bounds: `{}`",
                        display_constraint_bounds(bounds)
                    )),
                );
            }
        }
    }

    pub(super) fn opaque_return_concrete_ty(&self, ty: &Ty) -> Option<Ty> {
        let Ty::OpaqueReturn { key, .. } = ty else {
            return None;
        };
        self.opaque_returns.get(key).cloned()
    }

    pub(in crate::typeck) fn lower_opaque_returns_in_ty(&self, ty: &Ty) -> Ty {
        match ty {
            Ty::OpaqueReturn { .. } => {
                let Some(concrete) = self.opaque_return_concrete_ty(ty) else {
                    return ty.clone();
                };
                self.lower_opaque_returns_in_ty(&concrete)
            }
            _ => map_ty_children(ty, |child| self.lower_opaque_returns_in_ty(child)),
        }
    }

    pub(super) fn check_cast_allowed(&mut self, source: &Ty, target: &Ty, span: crate::span::Span) {
        let source = source;
        let target = target;
        if matches!(source, Ty::Unknown) || matches!(target, Ty::Unknown) || source == target {
            return;
        }
        if matches!(source, Ty::Bool) || matches!(target, Ty::Bool) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("cannot cast between `{source}` and `{target}`"),
            ));
            return;
        }
        if (source.is_numeric() || matches!(source, Ty::Char))
            && (target.is_numeric() || matches!(target, Ty::Char))
        {
            return;
        }
        if (source.is_numeric() || matches!(source, Ty::Char | Ty::CSpelling { .. }))
            && (target.is_numeric() || matches!(target, Ty::Char | Ty::CSpelling { .. }))
        {
            return;
        }
        if let (
            Ty::Pointer {
                nullable: source_nullable,
                mutability: source_mutability,
                inner: source_inner,
            },
            Ty::Pointer {
                nullable: target_nullable,
                mutability: target_mutability,
                inner: target_inner,
            },
        ) = (source, target)
        {
            if *source_nullable && !*target_nullable {
                if source_inner == target_inner
                    && pointer_view_can_weaken(*target_mutability, *source_mutability)
                {
                    self.require_unsafe(
                        span,
                        "nullable-to-non-null pointer casts require unsafe block",
                    );
                    return;
                }
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("cannot cast `{source}` to `{target}`"),
                ));
                return;
            }
            if source_mutability.is_read_only() && target_mutability.is_writable() {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("cannot cast `{source}` to `{target}`"),
                ));
                return;
            }
            if matches!(&**source_inner, Ty::Void) || matches!(&**target_inner, Ty::Void) {
                return;
            }
            self.diagnostics.push(Diagnostic::new(
                span,
                "pointer casts must go through `*void` or `?*void`",
            ));
            return;
        }
        self.diagnostics.push(Diagnostic::new(
            span,
            format!("cannot cast `{source}` to `{target}`"),
        ));
    }

    pub(in crate::typeck) fn validate_resource_struct_fields(
        &mut self,
        ty: &Ty,
        is_resource_decl: bool,
        is_unsafe_decl: bool,
        fields: &[(String, Ty)],
        span: impl Into<Option<crate::span::Span>>,
    ) {
        let span = span.into();
        let has_affine_field = fields
            .iter()
            .any(|(_, field_ty)| self.type_is_affine(field_ty));
        if is_resource_decl {
            if !has_affine_field && !is_unsafe_decl && !self.type_is_resource_handle_leaf(ty) {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("resource struct `{ty}` must contain an owning resource field"),
                ));
            }
        } else if has_affine_field && !contains_generic(ty) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("non-resource struct `{ty}` cannot store a resource field"),
            ));
        }
    }

    pub(super) fn require_unsafe_pointer_cast_through_void(
        &mut self,
        source: &Ty,
        target: &Ty,
        span: crate::span::Span,
    ) {
        let (
            Ty::Pointer {
                inner: source_inner,
                ..
            },
            Ty::Pointer {
                inner: target_inner,
                ..
            },
        ) = (source, target)
        else {
            return;
        };
        if source == target {
            return;
        }
        if matches!((&**source_inner, &**target_inner), (Ty::Void, target) if !matches!(target, Ty::Void))
        {
            self.require_unsafe(span, "raw pointer casts from `*void` require unsafe block");
        }
    }

    pub(super) fn require_unsafe(&mut self, span: crate::span::Span, message: impl Into<String>) {
        if self.unsafe_depth == 0 {
            self.diagnostics.push(Diagnostic::new(span, message.into()));
        }
    }

    pub(in crate::typeck) fn reject_invalid_plain_value_type(
        &mut self,
        ty: &Ty,
        span: crate::span::Span,
        context: &str,
    ) {
        if ty.is_never() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{context} cannot have type `never`"),
            ));
            return;
        }
        if self.is_opaque_by_value(ty) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{context} cannot use opaque struct `{ty}` by value"),
            ));
        }
        if type_contains_plain_never_value(ty) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{context} cannot contain `never` by value"),
            ));
        }
    }

    pub(in crate::typeck) fn reject_invalid_return_type(
        &mut self,
        ty: &Ty,
        span: crate::span::Span,
    ) {
        if self.is_opaque_by_value(ty) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("function cannot return opaque struct `{ty}` by value"),
            ));
        }
        match ty {
            Ty::Array { elem, .. } | Ty::Slice { elem, .. }
                if type_contains_plain_never_value(elem) =>
            {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    "function return type cannot contain `never` by value",
                ));
            }
            _ => {}
        }
    }

    pub(super) fn is_opaque_by_value(&self, ty: &Ty) -> bool {
        matches!(ty, Ty::Named { name, args, .. } if args.is_empty() && self.ctx.opaque_structs.contains(name))
    }

    pub(in crate::typeck) fn is_unsafe_struct_instance(&self, name: &str, args: &[Ty]) -> bool {
        let instance_name = enum_instance_name(name, args);
        self.ctx.unsafe_structs.contains(&instance_name)
            || self
                .ctx
                .struct_templates
                .get(name)
                .is_some_and(|template| template.is_unsafe)
    }

    pub(super) fn is_c_aggregate_value(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Named { name, args, .. } => {
                let instance_name = enum_instance_name(name, args);
                self.ctx.structs.contains_key(&instance_name)
                    || self.ctx.checked_enums.contains_key(&instance_name)
            }
            Ty::Slice { .. } | Ty::DynamicInterface { .. } => true,
            _ => false,
        }
    }
}
