use super::*;
use crate::ciel_display::format_typed_binding;

impl TypeChecker {
    pub(super) fn check_async_select_expr(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        biased: bool,
        arms: &[SelectArm],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        let future = self.check_select_future_expr(scopes, span, biased, arms, expected)?;
        let output_ty = generated_future_output_ty(&future.ty).unwrap_or(Ty::Unknown);
        Some(TExpr {
            span,
            ty: output_ty,
            kind: TExprKind::AsyncSelect {
                biased,
                arms: match future.kind {
                    TExprKind::AsyncSelect { arms, .. } => arms,
                    _ => unreachable!("select future expression has select kind"),
                },
            },
        })
    }

    pub(super) fn check_select_future_expr(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        biased: bool,
        arms: &[SelectArm],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        if arms.is_empty() {
            self.diagnostics
                .push(Diagnostic::new(span, "`select` requires at least one case"));
        }

        let mut checked_arms = Vec::new();
        let mut output_ty = expected.cloned();
        let mut affine_state = false;
        for arm in arms {
            let future = self.check_expr(scopes, &arm.future, None)?;
            if self.expr_contains_async_suspension(&future) {
                self.diagnostics.push(Diagnostic::new(
                    arm.future.span,
                    "select arm future cannot contain `await` or nested `select`",
                ));
            }
            let selectable_output = self.selectable_future_output_ty(&future.ty, arm.future.span);
            let future_output_ty = if let Some(output_ty) = selectable_output {
                output_ty
            } else if let Some(output_ty) = self.awaitable_output_ty(&future.ty, arm.future.span) {
                if !self.is_cancel_safe_ty(&future.ty) {
                    self.diagnostics.push(self.named_capability_diagnostic(
                        arm.future.span,
                        &future.ty,
                        STD_ASYNC_CANCEL_SAFE_INTERFACE,
                        "`select` may drop losing arms, so each arm future must be cancel-safe",
                    ));
                }
                if !self.is_abortable_ty(&future.ty) {
                    self.diagnostics.push(self.named_capability_diagnostic(
                        arm.future.span,
                        &future.ty,
                        STD_ASYNC_ABORT_FUTURE_INTERFACE,
                        "`select` may abort losing arms during cleanup",
                    ));
                }
                output_ty
            } else {
                self.diagnostics.push(self.named_capability_diagnostic(
                    arm.future.span,
                    &future.ty,
                    STD_ASYNC_AWAITABLE_FUTURE_INTERFACE,
                    "`select` arm futures must be awaitable",
                ));
                if !self.is_cancel_safe_ty(&future.ty) {
                    self.diagnostics.push(self.named_capability_diagnostic(
                        arm.future.span,
                        &future.ty,
                        STD_ASYNC_CANCEL_SAFE_INTERFACE,
                        "`select` may drop losing arms, so each arm future must be cancel-safe",
                    ));
                }
                if !self.is_abortable_ty(&future.ty) {
                    self.diagnostics.push(self.named_capability_diagnostic(
                        arm.future.span,
                        &future.ty,
                        STD_ASYNC_ABORT_FUTURE_INTERFACE,
                        "`select` may abort losing arms during cleanup",
                    ));
                }
                Ty::Unknown
            };
            let future = self.consume_affine_expr(scopes, future, false);
            affine_state |= self.type_is_affine(&future.ty);

            let mut arm_scopes = scopes.clone();
            arm_scopes.push();
            if let Err(name) = arm_scopes.insert(
                arm.binding_local,
                Binding {
                    name: arm.binding.name.clone(),
                    ty: future_output_ty.clone(),
                    narrowed_ty: None,
                    init_state: InitState::Assigned,
                    mutability: BindingMutability::Immutable,
                    captured: false,
                    declared_loop_depth: self.current_loop_depth,
                },
            ) {
                self.diagnostics.push(Diagnostic::new(
                    arm.binding.span,
                    format!("duplicate select binding `{name}`"),
                ));
            }
            let body_expected = output_ty.as_ref();
            let body =
                self.check_consumed_expr(&mut arm_scopes, &arm.body, body_expected, false)?;
            if self.expr_contains_async_suspension(&body) {
                self.diagnostics.push(Diagnostic::new(
                    arm.body.span,
                    "select arm body cannot contain `await` or nested `select`",
                ));
            }
            if let Some(expected) = output_ty.as_ref() {
                self.require_assignable(expected, &body.ty, body.span);
            } else {
                output_ty = Some(body.ty.clone());
            }
            arm_scopes.pop();

            checked_arms.push(TSelectArm {
                binding_local: arm.binding_local,
                binding_name: arm.binding.name.clone(),
                future,
                future_output_ty,
                body,
            });
        }

        let output_ty = output_ty.unwrap_or(Ty::Unknown);
        Some(TExpr {
            span,
            ty: generated_future_ty_with_affine_state(
                format!("select_{}", mangle_ty_fragment(&output_ty)),
                output_ty,
                false,
                true,
                affine_state,
            ),
            kind: TExprKind::AsyncSelect {
                biased,
                arms: checked_arms,
            },
        })
    }

    pub(super) fn expr_contains_async_suspension(&self, expr: &TExpr) -> bool {
        struct Visitor {
            found: bool,
        }
        impl ThirVisitor for Visitor {
            fn visit_expr(&mut self, expr: &TExpr) {
                match &expr.kind {
                    TExprKind::Await { .. } | TExprKind::AsyncSelect { .. } => {
                        self.found = true;
                    }
                    TExprKind::Closure { .. } => {}
                    _ => walk_expr(self, expr),
                }
            }
        }
        let mut visitor = Visitor { found: false };
        visitor.visit_expr(expr);
        visitor.found
    }

    pub(super) fn check_actor_spawn_cloned_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        if args.len() != 2 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("spawn_actor_cloned expects 2 arguments, got {}", args.len()),
            ));
            return None;
        }
        let current_subst = self.current_type_subst();
        let explicit_args = type_args
            .iter()
            .map(|arg| self.lower_type_with_subst(arg, &current_subst))
            .collect::<Vec<_>>();
        if explicit_args.len() > 2 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "spawn_actor_cloned accepts at most S and M type arguments",
            ));
            return None;
        }

        let explicit_handle_state_ty = explicit_args.first().cloned();
        let explicit_state_ty = explicit_handle_state_ty
            .as_ref()
            .map(|ty| self.meta_repr_storage_ty(ty, span));
        let explicit_handle_message_ty = explicit_args.get(1).cloned();
        let explicit_message_ty = explicit_handle_message_ty
            .as_ref()
            .map(|ty| self.meta_repr_storage_ty(ty, span));

        let expected_initial_state_ty = explicit_handle_state_ty
            .as_ref()
            .or(explicit_state_ty.as_ref());
        let initial_state = self.check_expr(scopes, &args[0], expected_initial_state_ty)?;
        let state_ty = explicit_state_ty.unwrap_or_else(|| initial_state.ty.clone());
        self.require_assignable(&state_ty, &initial_state.ty, initial_state.span);

        let mut prechecked_handler = None;
        let mut handle_message_ty = explicit_handle_message_ty
            .clone()
            .or_else(|| self.actor_message_ty_from_spawn_expected(expected))
            .or_else(|| self.actor_message_ty_from_closure_literal(&args[1]));
        if handle_message_ty.is_none() && !expr_is_closure_literal(&args[1]) {
            let handler = self.check_expr(scopes, &args[1], None)?;
            handle_message_ty =
                callable_ret_params_ty(&handler.ty).and_then(|(_, params)| params.get(1).cloned());
            prechecked_handler = Some(handler);
        }
        let Some(handle_message_ty) = handle_message_ty else {
            self.diagnostics.push(Diagnostic::new(
                span,
                "could not infer actor message type; add spawn_actor_cloned<S, M> type arguments, an expected Actor<M>, or handler parameter types",
            ));
            return None;
        };
        let message_ty = explicit_message_ty
            .unwrap_or_else(|| self.meta_repr_storage_ty(&handle_message_ty, span));
        let handler_state_ty = explicit_handle_state_ty.unwrap_or_else(|| state_ty.clone());
        let handler_message_ty = handle_message_ty.clone();
        let storage_state_ty = self.meta_repr_storage_ty(&state_ty, span);
        let handler_ret = std_result_ty(handler_state_ty.clone(), std_error_ty());
        let message_view = self.std_message_view("Message");
        let expected_handler_ty = Ty::Closure {
            ret: Box::new(handler_ret.clone()),
            params: vec![handler_state_ty.clone(), handler_message_ty.clone()],
            constraints: ConstraintBounds {
                positive: message_view.positive,
                negative: message_view.negative,
            },
        };
        let handler = if let Some(handler) = prechecked_handler {
            self.coerce_expr_to_expected(scopes, handler, Some(&expected_handler_ty))
        } else if let ExprKind::Closure {
            is_async: false,
            params,
            body,
        } = &args[1].kind
        {
            let handler = self.check_closure_expr(
                scopes,
                args[1].span,
                false,
                params,
                body,
                Some(&expected_handler_ty),
                false,
            )?;
            self.coerce_expr_to_expected(scopes, handler, Some(&expected_handler_ty))
        } else {
            self.check_expr(scopes, &args[1], Some(&expected_handler_ty))?
        };
        self.require_actor_handler_callable(
            &handler.ty,
            &handler_state_ty,
            &handler_message_ty,
            &handler_ret,
            handler.span,
        );

        if !self.type_implements_message(&storage_state_ty) {
            self.diagnostics.push(Diagnostic::new(
                initial_state.span,
                format!("actor state type `{state_ty}` does not implement `Message`"),
            ));
        }
        if !self.type_implements_message(&message_ty) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("actor message type `{message_ty}` does not implement `Message`"),
            ));
        }
        let ret = std_result_ty(std_actor_ty(handle_message_ty.clone()), std_error_ty());
        self.ensure_struct_instance(&ret);
        self.ensure_enum_instance(&ret);
        Some(TExpr {
            span,
            ty: ret,
            kind: TExprKind::ActorSpawn {
                mode: ActorSpawnMode::Cloned,
                state_arg: Box::new(initial_state),
                handler_ty: handler.ty.clone(),
                handler: Box::new(handler),
                state_ty: storage_state_ty,
                handle_message_ty,
                message_ty,
            },
        })
    }

    pub(super) fn check_actor_spawn_state_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        if args.len() != 2 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("spawn_actor_state expects 2 arguments, got {}", args.len()),
            ));
            return None;
        }
        let current_subst = self.current_type_subst();
        let explicit_args = type_args
            .iter()
            .map(|arg| self.lower_type_with_subst(arg, &current_subst))
            .collect::<Vec<_>>();
        if explicit_args.len() > 2 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "spawn_actor_state accepts at most S and M type arguments",
            ));
            return None;
        }

        let explicit_handle_state_ty = explicit_args.first().cloned();
        let explicit_state_ty = explicit_handle_state_ty
            .as_ref()
            .map(|ty| self.meta_repr_storage_ty(ty, span));
        let explicit_handle_message_ty = explicit_args.get(1).cloned();
        let explicit_message_ty = explicit_handle_message_ty
            .as_ref()
            .map(|ty| self.meta_repr_storage_ty(ty, span));

        let mut prechecked_init = None;
        let state_ty = if let Some(state_ty) = explicit_state_ty {
            state_ty
        } else {
            let init = self.check_expr(scopes, &args[0], None)?;
            let Some((ret, params)) = callable_ret_params_ty(&init.ty) else {
                self.diagnostics.push(Diagnostic::new(
                    init.span,
                    format!("actor state initializer `{}` is not callable", init.ty),
                ));
                return None;
            };
            if !params.is_empty() {
                self.diagnostics.push(Diagnostic::new(
                    init.span,
                    format!(
                        "actor state initializer expects 0 parameters, got {}",
                        params.len()
                    ),
                ));
                return None;
            }
            let Some((ok_ty, err_ty)) = self.result_ok_err_tys(&ret) else {
                self.diagnostics.push(Diagnostic::new(
                    init.span,
                    format!("actor state initializer must return `Result<S, Error>`, got `{ret}`"),
                ));
                return None;
            };
            if err_ty != std_error_ty() {
                self.diagnostics.push(Diagnostic::new(
                    init.span,
                    format!("actor state initializer error type must be `Error`, got `{err_ty}`"),
                ));
                return None;
            }
            prechecked_init = Some(init);
            self.meta_repr_storage_ty(&ok_ty, span)
        };

        let mut prechecked_handler = None;
        let mut handle_message_ty = explicit_handle_message_ty
            .clone()
            .or_else(|| self.actor_message_ty_from_spawn_expected(expected))
            .or_else(|| self.actor_message_ty_from_closure_literal_at(&args[1], 2));
        if handle_message_ty.is_none() && !expr_is_closure_literal(&args[1]) {
            let handler = self.check_expr(scopes, &args[1], None)?;
            handle_message_ty =
                callable_ret_params_ty(&handler.ty).and_then(|(_, params)| params.get(2).cloned());
            prechecked_handler = Some(handler);
        }
        let Some(handle_message_ty) = handle_message_ty else {
            self.diagnostics.push(Diagnostic::new(
                span,
                "could not infer actor message type; add spawn_actor_state<S, M> type arguments, an expected Actor<M>, or handler parameter types",
            ));
            return None;
        };
        let message_ty = explicit_message_ty
            .unwrap_or_else(|| self.meta_repr_storage_ty(&handle_message_ty, span));
        let handler_state_ty = explicit_handle_state_ty.unwrap_or_else(|| state_ty.clone());
        let handler_message_ty = handle_message_ty.clone();
        let storage_state_ty = self.meta_repr_storage_ty(&state_ty, span);
        let state_ptr_ty = Ty::Pointer {
            nullable: false,
            mutability: ViewMutability::Writable,
            inner: Box::new(handler_state_ty.clone()),
        };
        let actor_self_ty = std_actor_ty(handle_message_ty.clone());
        let init_ret = std_result_ty(storage_state_ty.clone(), std_error_ty());
        let handler_ret = std_result_ty(Ty::Void, std_error_ty());
        let message_view = self.std_message_view("Message");
        let expected_init_ty = Ty::Closure {
            ret: Box::new(init_ret.clone()),
            params: vec![],
            constraints: ConstraintBounds {
                positive: message_view.positive.clone(),
                negative: message_view.negative.clone(),
            },
        };
        let expected_handler_ty = Ty::Closure {
            ret: Box::new(handler_ret.clone()),
            params: vec![
                state_ptr_ty.clone(),
                actor_self_ty.clone(),
                handler_message_ty.clone(),
            ],
            constraints: ConstraintBounds {
                positive: message_view.positive,
                negative: message_view.negative,
            },
        };
        let init = if let Some(init) = prechecked_init {
            self.coerce_expr_to_expected(scopes, init, Some(&expected_init_ty))
        } else {
            let init = self.check_expr(scopes, &args[0], Some(&expected_init_ty))?;
            self.coerce_expr_to_expected(scopes, init, Some(&expected_init_ty))
        };
        self.require_actor_callable(
            &init.ty,
            &[],
            &init_ret,
            "actor state initializer",
            init.span,
        );

        let handler = if let Some(handler) = prechecked_handler {
            self.coerce_expr_to_expected(scopes, handler, Some(&expected_handler_ty))
        } else if let ExprKind::Closure {
            is_async: false,
            params,
            body,
        } = &args[1].kind
        {
            let handler = self.check_closure_expr(
                scopes,
                args[1].span,
                false,
                params,
                body,
                Some(&expected_handler_ty),
                false,
            )?;
            self.coerce_expr_to_expected(scopes, handler, Some(&expected_handler_ty))
        } else {
            let handler = self.check_expr(scopes, &args[1], Some(&expected_handler_ty))?;
            self.coerce_expr_to_expected(scopes, handler, Some(&expected_handler_ty))
        };
        self.require_actor_callable(
            &handler.ty,
            &[state_ptr_ty, actor_self_ty, handler_message_ty.clone()],
            &handler_ret,
            "actor state handler",
            handler.span,
        );

        if !self.type_implements_message(&message_ty) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("actor message type `{message_ty}` does not implement `Message`"),
            ));
        }
        let ret = std_result_ty(std_actor_ty(handle_message_ty.clone()), std_error_ty());
        self.ensure_struct_instance(&ret);
        self.ensure_enum_instance(&ret);
        Some(TExpr {
            span,
            ty: ret,
            kind: TExprKind::ActorSpawn {
                mode: ActorSpawnMode::State,
                state_arg: Box::new(init),
                handler_ty: handler.ty.clone(),
                handler: Box::new(handler),
                state_ty: storage_state_ty,
                handle_message_ty,
                message_ty,
            },
        })
    }

    pub(super) fn check_actor_send_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        if args.len() != 2 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("send expects 2 arguments, got {}", args.len()),
            ));
            return None;
        }
        let current_subst = self.current_type_subst();
        let explicit_args = type_args
            .iter()
            .map(|arg| self.lower_type_with_subst(arg, &current_subst))
            .collect::<Vec<_>>();
        if explicit_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "send accepts at most one message type argument",
            ));
            return None;
        }
        let actor = self.check_expr(scopes, &args[0], None)?;
        let inferred_message_ty = self.actor_message_ty_from_pointer(&actor.ty, actor.span);
        let handle_message_ty = explicit_args
            .first()
            .cloned()
            .or(inferred_message_ty)
            .unwrap_or(Ty::Unknown);
        let value = self.check_expr(scopes, &args[1], Some(&handle_message_ty))?;
        self.require_assignable(&handle_message_ty, &value.ty, value.span);
        let message_ty = if self.meta_repr_marker_matches_concrete(&handle_message_ty, &value.ty) {
            value.ty.clone()
        } else {
            self.normalize_meta_repr_markers(&handle_message_ty, span)
        };
        if !self.type_implements_message(&message_ty) {
            self.diagnostics.push(Diagnostic::new(
                value.span,
                format!("actor message type `{message_ty}` does not implement `Message`"),
            ));
        }
        let ret = std_result_ty(Ty::Void, std_error_ty());
        self.ensure_enum_instance(&ret);
        Some(TExpr {
            span,
            ty: ret,
            kind: TExprKind::ActorSend {
                actor: Box::new(actor),
                value: Box::new(value),
                message_ty,
            },
        })
    }

    pub(super) fn check_actor_lifecycle_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
        op: ActorLifecycleOp,
    ) -> Option<TExpr> {
        if args.len() != 1 {
            let name = match op {
                ActorLifecycleOp::Stop => "stop",
                ActorLifecycleOp::Join => "join",
            };
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{name} expects 1 argument, got {}", args.len()),
            ));
            return None;
        }
        if type_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "actor lifecycle calls accept at most one message type argument",
            ));
            return None;
        }
        let current_subst = self.current_type_subst();
        let explicit_message_ty = type_args
            .first()
            .map(|arg| self.lower_type_with_subst(arg, &current_subst));
        let actor = self.check_expr(scopes, &args[0], None)?;
        let message_ty = explicit_message_ty
            .or_else(|| self.actor_message_ty_from_pointer(&actor.ty, actor.span))
            .unwrap_or(Ty::Unknown);
        let ret = std_result_ty(Ty::Void, std_error_ty());
        self.ensure_enum_instance(&ret);
        let kind = match op {
            ActorLifecycleOp::Stop => TExprKind::ActorStop {
                actor: Box::new(actor),
                message_ty,
            },
            ActorLifecycleOp::Join => TExprKind::ActorJoin {
                actor: Box::new(actor),
                message_ty,
            },
        };
        Some(TExpr {
            span,
            ty: ret,
            kind,
        })
    }

    pub(super) fn check_meta_repr_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        name: &str,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        match name {
            "as_ref_repr" => self.check_meta_as_ref_repr_call(scopes, span, type_args, args),
            "into_repr" => self.check_meta_into_repr_call(scopes, span, type_args, args),
            "from_repr" => self.check_meta_from_repr_call(scopes, span, type_args, args, expected),
            "schema" => self.check_meta_schema_call(span, type_args, args),
            _ => None,
        }
    }

    pub(super) fn check_meta_schema_call(
        &mut self,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        if !args.is_empty() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("schema expects 0 arguments, got {}", args.len()),
            ));
            return None;
        }
        if type_args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "schema requires exactly one type argument",
            ));
            return None;
        }
        let subst = self.current_type_subst();
        let source_ty = self.lower_type_with_subst(&type_args[0], &subst);
        if !contains_generic(&source_ty)
            && !contains_type_hole(&source_ty)
            && !self.meta_repr_source_visible_from_current_module(&source_ty)
        {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "meta schema reflection cannot inspect private shape of `{source_ty}` from this module"
                ),
            ));
            return Some(TExpr {
                span,
                ty: Ty::Unknown,
                kind: TExprKind::MetaSchema { source_ty },
            });
        }
        let ret = self.meta_schema_ty(span, &source_ty);
        Some(TExpr {
            span,
            ty: ret,
            kind: TExprKind::MetaSchema { source_ty },
        })
    }

    pub(super) fn check_meta_as_ref_repr_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("as_ref_repr expects 1 argument, got {}", args.len()),
            ));
            return None;
        }
        if type_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "as_ref_repr accepts at most one type argument",
            ));
            return None;
        }
        let explicit = type_args.first().map(|ty| self.lower_type(ty));
        let expected_arg = explicit.clone().map(Ty::const_pointer_to);
        let value = self.check_expr(scopes, &args[0], expected_arg.as_ref())?;
        let source_ty = if let Some(source_ty) = explicit {
            source_ty
        } else {
            match &value.ty {
                Ty::Pointer {
                    nullable: false,
                    inner,
                    ..
                } => (**inner).clone(),
                Ty::Pointer { nullable: true, .. } => {
                    self.diagnostics.push(Diagnostic::new(
                        value.span,
                        "as_ref_repr requires a non-null pointer",
                    ));
                    Ty::Unknown
                }
                other => {
                    self.diagnostics.push(Diagnostic::new(
                        value.span,
                        format!("as_ref_repr requires `*const T`, got `{other}`"),
                    ));
                    Ty::Unknown
                }
            }
        };
        if let Some(expected_arg) = expected_arg.as_ref() {
            self.require_assignable(expected_arg, &value.ty, value.span);
        }
        self.reject_meta_ref_repr_erased_fields(span, &source_ty);
        let ret = self.meta_repr_ty(span, &source_ty, true);
        Some(TExpr {
            span,
            ty: ret,
            kind: TExprKind::MetaAsRefRepr {
                value: Box::new(value),
                source_ty,
            },
        })
    }

    pub(super) fn check_meta_into_repr_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("into_repr expects 1 argument, got {}", args.len()),
            ));
            return None;
        }
        if type_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "into_repr accepts at most one type argument",
            ));
            return None;
        }
        let explicit = type_args.first().map(|ty| self.lower_type(ty));
        let expected_arg = explicit.clone().map(Ty::const_pointer_to);
        let value = self.check_expr(scopes, &args[0], expected_arg.as_ref())?;
        let source_ty = if let Some(source_ty) = explicit {
            source_ty
        } else {
            match &value.ty {
                Ty::Pointer {
                    nullable: false,
                    inner,
                    ..
                } => (**inner).clone(),
                Ty::Pointer { nullable: true, .. } => {
                    self.diagnostics.push(Diagnostic::new(
                        value.span,
                        "into_repr requires a non-null pointer",
                    ));
                    Ty::Unknown
                }
                other => {
                    self.diagnostics.push(Diagnostic::new(
                        value.span,
                        format!("into_repr requires `*const T`, got `{other}`"),
                    ));
                    Ty::Unknown
                }
            }
        };
        if let Some(expected_arg) = expected_arg.as_ref() {
            self.require_assignable(expected_arg, &value.ty, value.span);
        }
        let ret = self.meta_repr_ty(span, &source_ty, false);
        Some(TExpr {
            span,
            ty: ret,
            kind: TExprKind::MetaIntoRepr {
                value: Box::new(value),
                source_ty,
            },
        })
    }

    pub(super) fn check_meta_from_repr_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("from_repr expects 1 argument, got {}", args.len()),
            ));
            return None;
        }
        if type_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "from_repr accepts at most one type argument",
            ));
            return None;
        }
        let target_ty = if let Some(ty) = type_args.first() {
            self.lower_type(ty)
        } else if let Some(expected) = expected {
            expected.clone()
        } else {
            self.diagnostics.push(Diagnostic::new(
                span,
                "from_repr requires an explicit type argument or expected result type",
            ));
            Ty::Unknown
        };
        let repr_ty = self.meta_repr_ty(span, &target_ty, false);
        let storage_repr_ty = std_meta_repr_marker_ty(false, target_ty.clone());
        let value = self.check_expr(scopes, &args[0], Some(&storage_repr_ty))?;
        if value.ty == storage_repr_ty {
            // Source-level storage keeps `meta::Repr<T>` as the safe-envelope type,
            // while representation operations lower through the concrete SOP layout.
        } else {
            self.require_assignable(&repr_ty, &value.ty, value.span);
        }
        Some(TExpr {
            span,
            ty: target_ty.clone(),
            kind: TExprKind::MetaFromRepr {
                value: Box::new(value),
                target_ty,
            },
        })
    }

    pub(super) fn reject_meta_ref_repr_erased_fields(
        &mut self,
        span: crate::span::Span,
        source_ty: &Ty,
    ) {
        let Ty::Named { name, args } = source_ty else {
            return;
        };
        let instance_name = enum_instance_name(name, args);
        let Some(fields) = self.ctx.structs.get(&instance_name).cloned() else {
            return;
        };
        for (field, ty) in &fields {
            if ty.is_erased_value() {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("as_ref_repr cannot borrow erased field `{field}` of `{source_ty}`"),
                ));
            }
        }
    }

    pub(super) fn check_type_metadata_call(
        &mut self,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
        name: &str,
    ) -> Option<TExpr> {
        if !args.is_empty() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{name} expects 0 arguments, got {}", args.len()),
            ));
            return None;
        }
        if type_args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{name} requires exactly one type argument"),
            ));
            return None;
        }
        let subst = self.current_type_subst();
        let lowered = self.lower_type_with_subst(&type_args[0], &subst);
        let ty = self.meta_repr_storage_ty(&lowered, type_args[0].span);
        self.ensure_struct_instance(&ty);
        self.ensure_enum_instance(&ty);
        let (ret_ty, kind) = match name {
            "type_size" => (Ty::Usize, TExprKind::TypeSize { ty }),
            "type_align" => (Ty::Usize, TExprKind::TypeAlign { ty }),
            "type_needs_gc_scan" => (Ty::Bool, TExprKind::TypeNeedsGcScan { ty }),
            _ => return None,
        };
        Some(TExpr {
            span,
            ty: ret_ty,
            kind,
        })
    }

    pub(super) fn actor_message_ty_from_spawn_expected(&self, expected: Option<&Ty>) -> Option<Ty> {
        let (ok_ty, _) = self.result_ok_err_tys(expected?)?;
        self.actor_message_ty_from_actor_ty(&ok_ty)
    }

    pub(super) fn actor_message_ty_from_actor_ty(&self, actor_ty: &Ty) -> Option<Ty> {
        let Ty::Named { name, args } = actor_ty else {
            return None;
        };
        if args.len() != 1 {
            return None;
        }
        self.ctx.nominal_type_defs.get(name).and_then(|def_id| {
            std_id::is_std_actor_type(&self.ctx.resolved, *def_id).then(|| args[0].clone())
        })
    }

    pub(super) fn actor_message_ty_from_closure_literal(&mut self, expr: &Expr) -> Option<Ty> {
        self.actor_message_ty_from_closure_literal_at(expr, 1)
    }

    pub(super) fn actor_message_ty_from_closure_literal_at(
        &mut self,
        expr: &Expr,
        param_index: usize,
    ) -> Option<Ty> {
        match &expr.kind {
            ExprKind::Closure { params, .. } => params
                .get(param_index)
                .and_then(|param| param.ty.as_ref())
                .map(|ty| self.lower_type(ty)),
            ExprKind::Cast { expr, .. } => {
                self.actor_message_ty_from_closure_literal_at(expr, param_index)
            }
            _ => None,
        }
    }

    pub(super) fn actor_message_ty_from_pointer(
        &mut self,
        actor_ty: &Ty,
        span: crate::span::Span,
    ) -> Option<Ty> {
        let Ty::Pointer {
            nullable: false,
            inner,
            ..
        } = actor_ty
        else {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("actor handle argument must be `*Actor<M>`, got `{actor_ty}`"),
            ));
            return None;
        };
        let Some(message_ty) = self.actor_message_ty_from_actor_ty(inner) else {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("actor handle argument must be `*Actor<M>`, got `{actor_ty}`"),
            ));
            return None;
        };
        Some(message_ty)
    }

    pub(super) fn require_actor_handler_callable(
        &mut self,
        handler_ty: &Ty,
        state_ty: &Ty,
        message_ty: &Ty,
        expected_ret: &Ty,
        span: crate::span::Span,
    ) {
        self.require_actor_callable(
            handler_ty,
            &[state_ty.clone(), message_ty.clone()],
            expected_ret,
            "actor handler",
            span,
        );
    }

    pub(super) fn require_actor_callable(
        &mut self,
        callable_ty: &Ty,
        expected_params: &[Ty],
        expected_ret: &Ty,
        label: &str,
        span: crate::span::Span,
    ) {
        let Some((ret, params)) = callable_ret_params_ty(callable_ty) else {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{label} `{callable_ty}` is not callable"),
            ));
            return;
        };
        if params.len() != expected_params.len() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "{label} expects {} parameters, got {}",
                    expected_params.len(),
                    params.len()
                ),
            ));
            return;
        }
        for (index, (actual, expected)) in params.iter().zip(expected_params.iter()).enumerate() {
            if actual != expected {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "{label} parameter {index} mismatch: expected `{expected}`, got `{actual}`",
                    ),
                ));
            }
        }
        if !self.ty_can_assign_from(expected_ret, &ret) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{label} must return `{expected_ret}`, got `{ret}`"),
            ));
        }
    }

    pub(super) fn check_raw_storage_from_ptr_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        sig: &FunctionSig,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        self.require_unsafe(
            span,
            format!(
                "call to unsafe function `{}` requires unsafe block",
                sig.name
            ),
        );
        if type_args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "raw_from_ptr requires exactly one type argument",
            ));
            return None;
        }
        if args.len() != 2 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("raw_from_ptr expects 2 arguments, got {}", args.len()),
            ));
            return None;
        }

        let subst = self.current_type_subst();
        let elem_ty = self.lower_type_with_subst(&type_args[0], &subst);
        let elem_ty = self.normalize_meta_repr_markers(&elem_ty, type_args[0].span);
        let elem_ty = self.resolve_type_holes(&elem_ty);
        self.ensure_struct_instance(&elem_ty);
        self.ensure_enum_instance(&elem_ty);

        let ptr_ty = Ty::Pointer {
            nullable: false,
            mutability: ViewMutability::Writable,
            inner: Box::new(Ty::Void),
        };
        let ptr = self.check_consumed_expr(scopes, &args[0], Some(&ptr_ty), false)?;
        self.require_assignable(&ptr_ty, &ptr.ty, args[0].span);
        let len = self.check_consumed_expr(scopes, &args[1], Some(&Ty::Usize), false)?;
        self.require_assignable(&Ty::Usize, &len.ty, args[1].span);

        Some(TExpr {
            span,
            ty: Ty::Slice {
                mutability: ViewMutability::Writable,
                elem: Box::new(elem_ty.clone()),
            },
            kind: TExprKind::RawSliceFromPtr {
                ptr: Box::new(ptr),
                len: Box::new(len),
                elem_ty,
            },
        })
    }

    pub(super) fn check_direct_function_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        callee_span: crate::span::Span,
        sig: FunctionSig,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        if std_id::is_std_actor_function(
            &self.ctx.resolved,
            sig.module,
            &sig.name,
            "spawn_actor_cloned",
        ) {
            return self.check_actor_spawn_cloned_call(scopes, span, type_args, args, expected);
        }
        if std_id::is_std_actor_function(
            &self.ctx.resolved,
            sig.module,
            &sig.name,
            "spawn_actor_state",
        ) {
            return self.check_actor_spawn_state_call(scopes, span, type_args, args, expected);
        }
        if std_id::is_std_actor_function(&self.ctx.resolved, sig.module, &sig.name, "send") {
            return self.check_actor_send_call(scopes, span, type_args, args);
        }
        if std_id::is_std_actor_function(&self.ctx.resolved, sig.module, &sig.name, "stop") {
            return self.check_actor_lifecycle_call(
                scopes,
                span,
                type_args,
                args,
                ActorLifecycleOp::Stop,
            );
        }
        if std_id::is_std_actor_function(&self.ctx.resolved, sig.module, &sig.name, "join") {
            return self.check_actor_lifecycle_call(
                scopes,
                span,
                type_args,
                args,
                ActorLifecycleOp::Join,
            );
        }
        if std_id::is_std_meta_function(&self.ctx.resolved, sig.module, &sig.name, "as_ref_repr")
            || std_id::is_std_meta_function(&self.ctx.resolved, sig.module, &sig.name, "into_repr")
            || std_id::is_std_meta_function(&self.ctx.resolved, sig.module, &sig.name, "from_repr")
            || std_id::is_std_meta_function(&self.ctx.resolved, sig.module, &sig.name, "schema")
        {
            return self.check_meta_repr_call(scopes, span, &sig.name, type_args, args, expected);
        }
        if std_id::is_std_meta_function(&self.ctx.resolved, sig.module, &sig.name, "type_size")
            || std_id::is_std_meta_function(&self.ctx.resolved, sig.module, &sig.name, "type_align")
            || std_id::is_std_meta_function(
                &self.ctx.resolved,
                sig.module,
                &sig.name,
                "type_needs_gc_scan",
            )
        {
            return self.check_type_metadata_call(span, type_args, args, &sig.name);
        }
        if std_id::is_std_storage_function(
            &self.ctx.resolved,
            sig.module,
            &sig.name,
            "raw_from_ptr",
        ) {
            return self.check_raw_storage_from_ptr_call(scopes, span, &sig, type_args, args);
        }
        if std_id::is_std_async_function(&self.ctx.resolved, sig.module, &sig.name, "spawn") {
            return self.check_async_spawn_call(scopes, span, type_args, args, expected);
        }
        if std_id::is_std_async_function(&self.ctx.resolved, sig.module, &sig.name, "cancel") {
            return self.check_async_task_cancel_call(scopes, span, type_args, args);
        }
        if std_id::is_std_async_function(&self.ctx.resolved, sig.module, &sig.name, "is_finished") {
            return self.check_async_task_is_finished_call(scopes, span, type_args, args);
        }
        if std_id::is_std_async_function(&self.ctx.resolved, sig.module, &sig.name, "block_on") {
            return self.check_async_block_on_call(scopes, span, type_args, args);
        }
        if std_id::is_std_async_function(
            &self.ctx.resolved,
            sig.module,
            &sig.name,
            "future_from_op",
        ) {
            return self.check_async_future_from_op_call(scopes, span, type_args, args);
        }
        if std_id::is_std_async_function(&self.ctx.resolved, sig.module, &sig.name, "send") {
            return self.check_async_channel_send_call(scopes, span, type_args, args);
        }
        if std_id::is_std_async_function(&self.ctx.resolved, sig.module, &sig.name, "try_send") {
            return self.check_async_channel_try_send_call(scopes, span, type_args, args);
        }
        if std_id::is_std_async_function(&self.ctx.resolved, sig.module, &sig.name, "reserve") {
            return self.check_async_channel_reserve_call(scopes, span, type_args, args);
        }
        if std_id::is_std_async_function(&self.ctx.resolved, sig.module, &sig.name, "permit_send") {
            return self.check_async_channel_permit_send_call(scopes, span, type_args, args);
        }
        if std_id::is_std_async_function(&self.ctx.resolved, sig.module, &sig.name, "recv") {
            return self.check_async_channel_recv_call(scopes, span, type_args, args);
        }
        if std_id::is_std_async_function(
            &self.ctx.resolved,
            sig.module,
            &sig.name,
            "group_next_task",
        ) {
            return self.check_async_task_group_next_task_call(scopes, span, type_args, args);
        }
        if std_id::is_std_async_time_function(&self.ctx.resolved, sig.module, &sig.name, "sleep_ms")
        {
            return self.check_async_sleep_ms_call(scopes, span, type_args, args);
        }

        let allow_resource_captures =
            std_id::is_std_resource_function(&self.ctx.resolved, sig.module, &sig.name, "scoped")
                || std_id::is_std_resource_function(
                    &self.ctx.resolved,
                    sig.module,
                    &sig.name,
                    "scoped_with_limits",
                );

        let (call_sig, generic_args) = if sig.generics.is_empty() {
            if !type_args.is_empty() {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("function `{}` is not generic", sig.name),
                ));
                return None;
            }
            (sig, None)
        } else {
            let (call_sig, instance_args) = self.infer_generic_function_call(
                scopes,
                span,
                &sig,
                type_args,
                args,
                expected,
                allow_resource_captures,
            )?;
            (call_sig, Some(instance_args))
        };
        if call_sig.is_unsafe {
            self.require_unsafe(
                span,
                format!(
                    "call to unsafe function `{}` requires unsafe block",
                    call_sig.name
                ),
            );
        }
        let call_ret = if call_sig.is_async {
            self.async_function_future_ty(call_sig.def_id, call_sig.ret.clone(), &call_sig.params)
        } else {
            call_sig.ret.clone()
        };
        let callee = TExpr {
            span: callee_span,
            ty: Ty::Function {
                is_unsafe: call_sig.is_unsafe,
                abi: call_sig.abi.clone(),
                ret: Box::new(call_ret.clone()),
                params: call_sig.params.clone(),
            },
            kind: if let Some(type_args) = generic_args {
                TExprKind::GenericFunction {
                    def_id: call_sig.def_id,
                    name: call_sig.name.clone(),
                    type_args,
                }
            } else {
                TExprKind::Function(call_sig.def_id, call_sig.name.clone())
            },
        };
        self.check_call_with_sig(
            scopes,
            span,
            callee,
            &call_ret,
            &call_sig.params,
            Some(&call_sig.param_names),
            Some(&call_sig.param_mutabilities),
            args,
            allow_resource_captures,
        )
    }

    pub(super) fn check_closure_cast_allowed(&mut self, target: &Ty, span: crate::span::Span) {
        match target {
            Ty::Closure { .. } => {}
            Ty::Function {
                is_unsafe: false,
                abi: None,
                ..
            } => {}
            Ty::Function { abi: Some(_), .. } => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    "closure expressions cannot produce extern C function pointers",
                ));
            }
            Ty::Function {
                is_unsafe: true, ..
            } => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    "closure expressions cannot produce unsafe function pointers",
                ));
            }
            _ => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "closure annotation must be a closure or Ciel ABI function type, got `{target}`"
                    ),
                ));
            }
        }
    }

    pub(super) fn check_closure_expr(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        is_async: bool,
        params: &[ClosureParam],
        body: &ClosureBody,
        expected: Option<&Ty>,
        allow_resource_captures: bool,
    ) -> Option<TExpr> {
        let expected_closure_instance_id = match expected {
            Some(Ty::ClosureInstance { id, .. }) => Some(*id),
            _ => None,
        };
        let expected_sig = match expected {
            Some(Ty::Closure { ret, params, .. })
            | Some(Ty::ClosureInstance { ret, params, .. }) => {
                Some(((**ret).clone(), params.clone(), false))
            }
            Some(Ty::Function {
                is_unsafe: false,
                abi: None,
                ret,
                params,
            }) => Some(((**ret).clone(), params.clone(), true)),
            Some(Ty::Function { abi: Some(_), .. }) => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    "closure expressions cannot produce extern C function pointers",
                ));
                None
            }
            Some(other) => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("closure requires expected callable type, got `{other}`"),
                ));
                None
            }
            None => None,
        };
        let expected_body_sig = expected_sig
            .as_ref()
            .map(|(expected_ret, expected_params, target_fn)| {
                let body_ret = if is_async {
                    self.future_output_ty(expected_ret).unwrap_or_else(|| {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            format!(
                                "async closure requires expected callable return type `Future<T>`, got `{expected_ret}`"
                            ),
                        ));
                        Ty::Unknown
                    })
                } else {
                    expected_ret.clone()
                };
                (body_ret, expected_params.clone(), *target_fn)
            });

        if let Some((_, expected_params, _)) = &expected_body_sig
            && expected_params.len() != params.len()
        {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "closure expects {} parameters, got {}",
                    expected_params.len(),
                    params.len()
                ),
            ));
        }

        let mut checked_params = Vec::new();
        let mut closure_scopes = scopes.clone();
        closure_scopes.mark_all_captured();
        closure_scopes.push();
        for (idx, param) in params.iter().enumerate() {
            let param_ty = if let Some(ty) = &param.ty {
                let ty = self.lower_type(ty);
                if let Some((_, expected_params, _)) = &expected_body_sig
                    && let Some(expected_ty) = expected_params.get(idx)
                {
                    if contains_type_hole(expected_ty) {
                        self.unify_type_holes(expected_ty, &ty);
                    } else {
                        let expected_ty = self.resolve_type_holes(expected_ty);
                        if !matches!(expected_ty, Ty::Unknown)
                            && !contains_generic(&expected_ty)
                            && ty != expected_ty
                        {
                            self.diagnostics.push(Diagnostic::new(
                                param.name.span,
                                format!(
                                    "closure parameter `{}` expected `{expected_ty}`, got `{ty}`",
                                    param.name.name
                                ),
                            ));
                        }
                    }
                }
                ty
            } else if let Some((_, expected_params, _)) = &expected_body_sig {
                expected_params
                    .get(idx)
                    .map(|ty| self.resolve_type_holes(ty))
                    .unwrap_or(Ty::Unknown)
            } else {
                self.diagnostics.push(Diagnostic::new(
                    param.name.span,
                    format!(
                        "closure parameter `{}` requires an explicit type or expected callable type",
                        param.name.name
                    ),
                ));
                Ty::Unknown
            };
            self.reject_invalid_plain_value_type(&param_ty, param.name.span, "closure parameter");
            if let Err(name) = closure_scopes.insert(
                param.local_id,
                Binding {
                    name: param.name.name.clone(),
                    ty: param_ty.clone(),
                    narrowed_ty: None,
                    init_state: InitState::Assigned,
                    mutability: param.mutability,
                    captured: false,
                    declared_loop_depth: self.current_loop_depth,
                },
            ) {
                self.diagnostics.push(Diagnostic::new(
                    param.name.span,
                    format!("duplicate closure parameter `{name}`"),
                ));
            }
            checked_params.push((param.local_id, param.name.name.clone(), param_ty));
        }

        let previous_return_ty = self.current_return_ty.clone();
        let previous_control_contexts = std::mem::take(&mut self.control_contexts);
        let previous_unsafe_depth = std::mem::replace(&mut self.unsafe_depth, 0);
        let previous_async_depth =
            std::mem::replace(&mut self.current_async_depth, if is_async { 1 } else { 0 });
        let body_result = self.without_return_loop_move_context(|this| match body {
            ClosureBody::Expr(body_expr) => {
                if let Some((expected_ret, _, _)) = &expected_body_sig {
                    let expected_ret = this.resolve_type_holes(expected_ret);
                    this.current_return_ty = expected_ret.clone();
                    let checked = this.check_consumed_expr(
                        &mut closure_scopes,
                        body_expr,
                        Some(&expected_ret),
                        true,
                    )?;
                    this.require_assignable(&expected_ret, &checked.ty, checked.span);
                    Some((expected_ret, TClosureBody::Expr(Box::new(checked))))
                } else {
                    this.current_return_ty = Ty::Unknown;
                    let checked =
                        this.check_consumed_expr(&mut closure_scopes, body_expr, None, true)?;
                    let ret_ty = checked.ty.clone();
                    Some((ret_ty, TClosureBody::Expr(Box::new(checked))))
                }
            }
            ClosureBody::Block(block) => {
                let Some((expected_ret, _, _)) = &expected_body_sig else {
                    this.diagnostics.push(Diagnostic::new(
                        block.span,
                        "block-bodied closure requires an expected callable return type",
                    ));
                    return None;
                };
                let expected_ret = this.resolve_type_holes(expected_ret);
                this.current_return_ty = expected_ret.clone();
                let checked = this.check_block_with_existing_scope(
                    &mut closure_scopes,
                    block,
                    &expected_ret,
                )?;
                if expected_ret.is_never() && checked.flow.can_fallthrough {
                    this.diagnostics.push(Diagnostic::new(
                        block.span,
                        "closure with return type `never` can fall through",
                    ));
                } else if !expected_ret.is_erased_value() && checked.flow.can_fallthrough {
                    this.diagnostics.push(Diagnostic::new(
                        block.span,
                        format!("closure must return `{expected_ret}` on every path"),
                    ));
                }
                Some((expected_ret, TClosureBody::Block(checked.block)))
            }
        });
        let Some((ret_ty, checked_body)) = body_result else {
            self.current_return_ty = previous_return_ty;
            self.control_contexts = previous_control_contexts;
            self.unsafe_depth = previous_unsafe_depth;
            self.current_async_depth = previous_async_depth;
            return None;
        };
        self.current_return_ty = previous_return_ty;
        self.control_contexts = previous_control_contexts;
        self.unsafe_depth = previous_unsafe_depth;
        self.current_async_depth = previous_async_depth;

        let capture_ids = collect_closure_capture_ids(&checked_params, &checked_body);
        let mut captures = Vec::new();
        for local_id in capture_ids {
            let Some(binding) = scopes.get(local_id) else {
                continue;
            };
            if !binding.init_state.is_assigned() {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "captured local `{}` is not definitely assigned at closure creation",
                        binding.name
                    ),
                ));
            }
            captures.push(TClosureCapture {
                local_id,
                name: binding.name.clone(),
                ty: scopes
                    .effective_ty(local_id)
                    .unwrap_or_else(|| binding.ty.clone()),
            });
        }
        for capture in &captures {
            if self.type_is_affine(&capture.ty) {
                if allow_resource_captures {
                    if let Some(binding) = scopes.get(capture.local_id)
                        && binding.declared_loop_depth < self.current_loop_depth
                    {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            format!(
                                "resource `{}` is declared outside this loop and cannot be moved into a closure inside it",
                                capture.name
                            ),
                        ));
                    }
                    if let Some(binding) = scopes.get_mut(capture.local_id) {
                        binding.init_state = InitState::Moved;
                        binding.narrowed_ty = None;
                    }
                } else {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!(
                            "closure cannot capture resource `{}` of type `{}`",
                            capture.name, capture.ty
                        ),
                    ));
                }
            }
        }

        let id = if let Some(id) = expected_closure_instance_id {
            id
        } else {
            let id = self.next_closure_id;
            self.next_closure_id += 1;
            id
        };
        let capture_tys = captures
            .iter()
            .map(|capture| capture.ty.clone())
            .collect::<Vec<_>>();
        let closure_affine_state = captures
            .iter()
            .any(|capture| self.type_is_affine(&capture.ty))
            || checked_params
                .iter()
                .any(|(_, _, ty)| self.type_is_affine(ty));
        if is_async {
            self.check_async_closure_frame_safety(&checked_body, &checked_params, &captures);
        }
        let async_facts = if is_async {
            Some(self.async_facts_for_closure_body(&checked_body))
        } else {
            None
        };
        let (closure_cancel_safe, closure_abortable) = if is_async {
            self.async_closure_body_capabilities(&checked_body)
        } else {
            (false, false)
        };

        let result_ty = if let Some((expected_ret, expected_params, target_fn)) = expected_sig {
            let expected_ret = self.resolve_type_holes(&expected_ret);
            let expected_params = expected_params
                .iter()
                .map(|param| self.resolve_type_holes(param))
                .collect::<Vec<_>>();
            let closure_ret = if is_async {
                self.async_closure_future_ty(
                    id,
                    ret_ty.clone(),
                    closure_cancel_safe,
                    closure_abortable,
                    closure_affine_state,
                )
            } else {
                expected_ret.clone()
            };
            if target_fn {
                if !captures.is_empty() {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        "capturing closure cannot convert to `fn`",
                    ));
                }
                Ty::Function {
                    is_unsafe: false,
                    abi: None,
                    ret: Box::new(closure_ret),
                    params: expected_params,
                }
            } else {
                Ty::ClosureInstance {
                    id,
                    ret: Box::new(closure_ret),
                    params: expected_params,
                    captures: capture_tys,
                }
            }
        } else {
            let ret_ty = if is_async {
                self.async_closure_future_ty(
                    id,
                    ret_ty.clone(),
                    closure_cancel_safe,
                    closure_abortable,
                    closure_affine_state,
                )
            } else {
                ret_ty.clone()
            };
            Ty::ClosureInstance {
                id,
                ret: Box::new(ret_ty),
                params: checked_params.iter().map(|(_, _, ty)| ty.clone()).collect(),
                captures: capture_tys,
            }
        };
        Some(TExpr {
            span,
            ty: result_ty,
            kind: TExprKind::Closure {
                is_async,
                id,
                params: checked_params,
                captures,
                body: checked_body,
                async_facts,
            },
        })
    }

    pub(super) fn check_call_with_sig(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        callee: TExpr,
        ret: &Ty,
        params: &[Ty],
        param_names: Option<&[String]>,
        param_mutabilities: Option<&[BindingMutability]>,
        args: &[Expr],
        allow_resource_captures: bool,
    ) -> Option<TExpr> {
        if params.len() != args.len() {
            let mut diagnostic = Diagnostic::new(
                span,
                format!(
                    "call expects {} arguments, got {}",
                    params.len(),
                    args.len()
                ),
            );
            if let Some(note) =
                parameter_names_note(params, param_names, param_mutabilities, params.len())
            {
                diagnostic = diagnostic.note(note);
            }
            self.diagnostics.push(diagnostic);
        }
        let mut checked_args = Vec::new();
        for (idx, arg) in args.iter().enumerate() {
            let expected = params.get(idx);
            let checked = if allow_resource_captures && expr_is_closure_literal(arg) {
                let checked = self.check_closure_literal_preserving_instance(
                    scopes,
                    arg,
                    expected,
                    allow_resource_captures,
                )?;
                self.consume_affine_expr(scopes, checked, false)
            } else {
                self.check_consumed_expr(scopes, arg, expected, false)?
            };
            if let Some(expected) = expected {
                let param_display = param_names
                    .and_then(|names| names.get(idx))
                    .zip(param_mutabilities.and_then(|mutabilities| mutabilities.get(idx)))
                    .map(|(name, mutability)| format_typed_binding(expected, name, *mutability));
                self.require_assignable_argument(
                    expected,
                    &checked.ty,
                    arg.span,
                    param_display.as_deref(),
                );
            }
            checked_args.push(checked);
        }
        Some(TExpr {
            span,
            ty: ret.clone(),
            kind: TExprKind::Call {
                callee: Box::new(callee),
                args: checked_args,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_field_or_receiver_selector_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        base: &Expr,
        field: &crate::ast::Ident,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        let checked_base = self.check_expr(scopes, base, None)?;
        if let Some(field_ty) = self.field_ty_silent(&checked_base.ty, &field.name, field.span)
            && callable_ret_params_ty(&field_ty).is_some()
        {
            if !type_args.is_empty() {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    "type arguments can only be used on generic function or interface calls",
                ));
                return None;
            }
            let field_ty = self
                .field_ty(&checked_base.ty, &field.name, field.span)
                .unwrap_or(field_ty);
            let callee = TExpr {
                span: checked_base.span.merge(field.span),
                ty: field_ty,
                kind: TExprKind::Field {
                    base: Box::new(checked_base),
                    field: field.name.clone(),
                },
            };
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
            let Some((ret, params)) = callable_ret_params_ty(&callee.ty) else {
                unreachable!("callable field type was checked above");
            };
            return self
                .check_call_with_sig(scopes, span, callee, &ret, &params, None, None, args, false);
        }

        let selector = vec![field.clone()];
        self.check_receiver_selector_call(scopes, span, base, &selector, type_args, args, expected)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_receiver_selector_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        receiver: &Expr,
        selector_path: &[crate::ast::Ident],
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        let Some(selector_name) = selector_path.last().map(|name| name.name.clone()) else {
            return None;
        };
        let checked_receiver = self.check_expr(scopes, receiver, None)?;
        let visible = self.visible_receiver_selector_candidates(self.current_module, selector_path);
        let mut matches = Vec::new();
        for candidate in visible {
            let Some(param_ty) = self.receiver_selector_pattern_ty(&candidate) else {
                continue;
            };
            if let Some(adaptation) = receiver_selector_adaptation(&param_ty, &checked_receiver.ty)
            {
                matches.push((candidate, adaptation));
            }
        }
        match matches.len() {
            0 => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "no selector `.{selector_name}` for receiver type `{}`",
                        checked_receiver.ty
                    ),
                ));
                None
            }
            1 => {
                let (selector, adaptation) = matches.into_iter().next().unwrap();
                let selector_span = selector_path_span(selector_path).unwrap_or(span);
                self.check_receiver_selector_target_call(
                    scopes,
                    span,
                    selector_span,
                    receiver,
                    selector,
                    adaptation,
                    type_args,
                    args,
                    expected,
                )
            }
            count => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "ambiguous selector `.{selector_name}` for receiver type `{}` ({count} candidates)",
                        checked_receiver.ty
                    ),
                ));
                None
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_receiver_selector_target_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        callee_span: crate::span::Span,
        receiver: &Expr,
        selector: ReceiverSelectorSig,
        adaptation: ReceiverAdaptation,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        match selector.callable {
            ReceiverSelectorCallable::Function(def_id) => {
                let sig = self.ctx.functions_by_def.get(&def_id).cloned()?;
                let call_args = receiver_selector_desugared_args(
                    receiver,
                    args,
                    sig.params.len(),
                    selector.receiver_index,
                    adaptation,
                );
                self.check_direct_function_call(
                    scopes,
                    span,
                    callee_span,
                    sig,
                    type_args,
                    &call_args,
                    expected,
                )
            }
            ReceiverSelectorCallable::Interface(def_id) => {
                let interface = self.ctx.interfaces.get(&def_id).cloned()?;
                let call_args = receiver_selector_desugared_args(
                    receiver,
                    args,
                    interface.params.len(),
                    selector.receiver_index,
                    adaptation,
                );
                self.check_interface_call_with_receiver_index(
                    scopes,
                    span,
                    def_id,
                    type_args,
                    &call_args,
                    expected,
                    selector.receiver_index,
                )
            }
        }
    }

    pub(super) fn visible_receiver_selector_candidates(
        &self,
        module: ModuleId,
        selector_path: &[crate::ast::Ident],
    ) -> Vec<ReceiverSelectorSig> {
        let Some(selector_name) = selector_path.last().map(|name| name.name.as_str()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match selector_path {
            [_name] => {
                out.extend(
                    self.ctx
                        .receiver_selectors
                        .iter()
                        .filter(|selector| {
                            selector.module == module && selector.selector == selector_name
                        })
                        .cloned(),
                );
                let mut visited = HashSet::new();
                for import in &self.ctx.resolved.modules[module.0].imports {
                    if import.alias.is_some() {
                        continue;
                    }
                    if let Some(target) = import.target {
                        self.exported_receiver_selectors_from_module(
                            target,
                            selector_name,
                            &mut visited,
                            &mut out,
                        );
                    }
                }
            }
            [alias, _name] => {
                for target in self.receiver_selector_alias_targets(module, &alias.name) {
                    let mut visited = HashSet::new();
                    self.exported_receiver_selectors_from_module(
                        target,
                        selector_name,
                        &mut visited,
                        &mut out,
                    );
                }
            }
            _ => {}
        }
        dedup_receiver_selectors(&mut out);
        out
    }

    pub(super) fn exported_receiver_selectors_from_module(
        &self,
        module: ModuleId,
        selector_name: &str,
        visited: &mut HashSet<ModuleId>,
        out: &mut Vec<ReceiverSelectorSig>,
    ) {
        if !visited.insert(module) {
            return;
        }
        out.extend(
            self.ctx
                .receiver_selectors
                .iter()
                .filter(|selector| {
                    selector.module == module
                        && selector.exported
                        && selector.selector == selector_name
                })
                .cloned(),
        );
        for import in &self.ctx.resolved.modules[module.0].imports {
            if !import.exported || import.alias.is_some() {
                continue;
            }
            if let Some(target) = import.target {
                self.exported_receiver_selectors_from_module(target, selector_name, visited, out);
            }
        }
    }

    pub(super) fn receiver_selector_alias_targets(
        &self,
        module: ModuleId,
        alias: &str,
    ) -> Vec<ModuleId> {
        let mut targets = Vec::new();
        for import in &self.ctx.resolved.modules[module.0].imports {
            if import.alias.as_deref() == Some(alias)
                && let Some(target) = import.target
            {
                targets.push(target);
            }
        }
        let mut visited = HashSet::new();
        for import in &self.ctx.resolved.modules[module.0].imports {
            if import.alias.is_some() {
                continue;
            }
            if let Some(target) = import.target {
                self.exported_receiver_selector_alias_targets(
                    target,
                    alias,
                    &mut visited,
                    &mut targets,
                );
            }
        }
        dedup_modules(&mut targets);
        targets
    }

    pub(super) fn exported_receiver_selector_alias_targets(
        &self,
        module: ModuleId,
        alias: &str,
        visited: &mut HashSet<ModuleId>,
        out: &mut Vec<ModuleId>,
    ) {
        if !visited.insert(module) {
            return;
        }
        for import in &self.ctx.resolved.modules[module.0].imports {
            if !import.exported {
                continue;
            }
            if import.alias.as_deref() == Some(alias) {
                if let Some(target) = import.target {
                    out.push(target);
                }
            } else if import.alias.is_none()
                && let Some(target) = import.target
            {
                self.exported_receiver_selector_alias_targets(target, alias, visited, out);
            }
        }
    }

    pub(super) fn check_async_block_on_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        if type_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                type_args[1].span,
                "too many type arguments for `block_on`",
            ));
            return None;
        }
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("call expects 1 arguments, got {}", args.len()),
            ));
        }
        let explicit_output = type_args.first().map(|ty| self.lower_type(ty));
        let Some(arg) = args.first() else {
            return None;
        };
        let future = self.check_expr(scopes, arg, None)?;
        let awaitable = self.awaitable_ty(&future.ty, future.span);
        let output_ty = explicit_output.clone().or_else(|| {
            awaitable
                .as_ref()
                .map(|awaitable| awaitable.output_ty.clone())
        });
        let Some(output_ty) = output_ty else {
            self.diagnostics.push(self.named_capability_diagnostic(
                future.span,
                &future.ty,
                STD_ASYNC_AWAITABLE_FUTURE_INTERFACE,
                "`async::block_on` requires an awaitable future value",
            ));
            return Some(TExpr {
                span,
                ty: Ty::Unknown,
                kind: TExprKind::AsyncBlockOn {
                    future: Box::new(future),
                },
            });
        };
        if let Some(awaitable) = awaitable {
            self.require_assignable(&output_ty, &awaitable.output_ty, future.span);
        } else {
            self.diagnostics.push(self.named_capability_diagnostic(
                future.span,
                &future.ty,
                STD_ASYNC_AWAITABLE_FUTURE_INTERFACE,
                "`async::block_on` requires an awaitable future value",
            ));
        }
        if !self.is_abortable_ty(&future.ty) {
            self.diagnostics.push(self.named_capability_diagnostic(
                future.span,
                &future.ty,
                STD_ASYNC_ABORT_FUTURE_INTERFACE,
                "`async::block_on` needs an abortable future for cleanup on failure",
            ));
        }
        let future = self.consume_affine_expr(scopes, future, false);
        Some(TExpr {
            span,
            ty: output_ty,
            kind: TExprKind::AsyncBlockOn {
                future: Box::new(future),
            },
        })
    }

    pub(super) fn check_async_sleep_ms_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        if !type_args.is_empty() {
            self.diagnostics
                .push(Diagnostic::new(span, "function `sleep_ms` is not generic"));
            return None;
        }
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("call expects 1 arguments, got {}", args.len()),
            ));
        }
        let Some(ms_arg) = args.first() else {
            return None;
        };
        let ms = self.check_expr(scopes, ms_arg, Some(&Ty::U64))?;
        self.require_assignable(&Ty::U64, &ms.ty, ms.span);
        let output_ty = std_result_ty(Ty::Void, std_async_error_ty());
        Some(TExpr {
            span,
            ty: self.async_sleep_future_ty(output_ty.clone()),
            kind: TExprKind::AsyncSleep {
                ms: Box::new(ms),
                output_ty,
            },
        })
    }

    pub(super) fn check_async_future_from_op_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        if type_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "`future_from_op` expects at most 1 type argument, got {}",
                    type_args.len()
                ),
            ));
            return None;
        }
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("call expects 1 arguments, got {}", args.len()),
            ));
        }
        let explicit_op_ty = type_args.first().map(|ty| self.lower_type(ty));
        let Some(arg) = args.first() else {
            return None;
        };
        let op_expr = self.check_consumed_expr(scopes, arg, explicit_op_ty.as_ref(), false)?;
        if let Some(op_ty) = explicit_op_ty.as_ref() {
            self.require_assignable(op_ty, &op_expr.ty, op_expr.span);
        }
        let op_ty = explicit_op_ty.clone().unwrap_or_else(|| op_expr.ty.clone());
        let Some(raw_operation_def) = self.std_async_interface_def("raw_operation") else {
            self.diagnostics.push(Diagnostic::new(
                span,
                "internal error: missing std async interface `raw_operation`",
            ));
            return None;
        };
        let Some(poll_done_def) = self.std_async_interface_def("poll_done") else {
            self.diagnostics.push(Diagnostic::new(
                span,
                "internal error: missing std async interface `poll_done`",
            ));
            return None;
        };
        let output_ty = {
            let diagnostic_count = self.diagnostics.len();
            match self.capability_determined_arg(
                poll_done_def,
                "poll_done",
                "poll_done::Out",
                &op_ty,
                span,
            ) {
                Some(output_ty) => output_ty,
                None => {
                    if self.diagnostics.len() == diagnostic_count {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            format!(
                                "`future_from_op` operation type `{op_ty}` does not determine `poll_done::Out`"
                            ),
                        ));
                    }
                    Ty::Unknown
                }
            }
        };
        if !self.type_implements_capability_by_def(raw_operation_def, "raw_operation", &[], &op_ty)
        {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "`future_from_op` operation type `{op_ty}` does not implement `raw_operation`"
                ),
            ));
        }
        if !self.type_implements_capability_by_def(
            poll_done_def,
            "poll_done",
            std::slice::from_ref(&output_ty),
            &op_ty,
        ) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("`future_from_op` operation type `{op_ty}` does not implement `poll_done<{output_ty}>`"),
            ));
        }
        let result_ty = std_result_ty(output_ty.clone(), std_async_error_ty());
        let future_ty = self.async_op_future_ty(&op_expr.ty, result_ty.clone(), span);
        Some(TExpr {
            span,
            ty: future_ty,
            kind: TExprKind::AsyncOpFuture {
                op: Box::new(op_expr),
                output_ty,
                raw_operation_def,
                poll_done_def,
            },
        })
    }

    pub(super) fn explicit_async_payload_ty(
        &mut self,
        label: &str,
        _span: crate::span::Span,
        type_args: &[Type],
    ) -> Option<Ty> {
        if type_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                type_args[1].span,
                format!("too many type arguments for `{label}`"),
            ));
            return Some(Ty::Unknown);
        }
        type_args.first().map(|ty| self.lower_type(ty))
    }

    pub(super) fn check_channel_payload_message(
        &mut self,
        payload_ty: &Ty,
        span: crate::span::Span,
    ) {
        if let Some(reason) =
            self.task_boundary_message_violation(payload_ty, "channel payload", &mut HashSet::new())
        {
            self.diagnostics.push(self.diagnostic_with_reason_note(
                span,
                format!("async channel payload type `{payload_ty}` does not implement `Message`"),
                reason,
            ));
        }
    }

    pub(super) fn check_async_channel_sender_arg(
        &mut self,
        scopes: &mut LocalScopes,
        label: &str,
        arg: &Expr,
        explicit_payload: Option<&Ty>,
    ) -> Option<(TExpr, Ty)> {
        let expected = explicit_payload.map(|ty| std_sender_ty(ty.clone()));
        let sender = self.check_expr(scopes, arg, expected.as_ref())?;
        if let Some(expected) = expected.as_ref() {
            self.require_assignable(expected, &sender.ty, sender.span);
        }
        let payload_ty = explicit_payload.cloned().or_else(|| {
            std_id::std_async_sender_payload_arg(&self.ctx.resolved, &sender.ty).cloned()
        });
        let Some(payload_ty) = payload_ty else {
            self.diagnostics.push(Diagnostic::new(
                sender.span,
                format!("`async::{label}` requires `Sender<T>`, got `{}`", sender.ty),
            ));
            return Some((sender, Ty::Unknown));
        };
        Some((sender, payload_ty))
    }

    pub(super) fn check_async_channel_send_like_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
        label: &str,
        as_future: bool,
    ) -> Option<TExpr> {
        let explicit_payload = self.explicit_async_payload_ty(label, span, type_args);
        if args.len() != 2 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("`async::{label}` expects 2 arguments, got {}", args.len()),
            ));
        }
        let Some(sender_arg) = args.first() else {
            return None;
        };
        let (sender, payload_ty) = self.check_async_channel_sender_arg(
            scopes,
            label,
            sender_arg,
            explicit_payload.as_ref(),
        )?;
        let Some(value_arg) = args.get(1) else {
            return None;
        };
        let value = self.check_expr(scopes, value_arg, Some(&payload_ty))?;
        self.require_assignable(&payload_ty, &value.ty, value.span);
        self.check_channel_payload_message(&payload_ty, value.span);
        let result_ty = std_result_ty(Ty::Void, std_async_error_ty());
        if as_future {
            Some(TExpr {
                span,
                ty: generated_future_ty(
                    format!("channel_send_{}", mangle_ty_fragment(&payload_ty)),
                    result_ty,
                    false,
                    true,
                ),
                kind: TExprKind::AsyncChannelSend {
                    sender: Box::new(sender),
                    value: Box::new(value),
                    payload_ty,
                },
            })
        } else {
            Some(TExpr {
                span,
                ty: result_ty,
                kind: TExprKind::AsyncChannelTrySend {
                    sender: Box::new(sender),
                    value: Box::new(value),
                    payload_ty,
                },
            })
        }
    }

    pub(super) fn check_async_channel_send_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        self.check_async_channel_send_like_call(scopes, span, type_args, args, "send", true)
    }

    pub(super) fn check_async_channel_try_send_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        self.check_async_channel_send_like_call(scopes, span, type_args, args, "try_send", false)
    }

    pub(super) fn check_async_channel_reserve_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        let explicit_payload = self.explicit_async_payload_ty("reserve", span, type_args);
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("`async::reserve` expects 1 argument, got {}", args.len()),
            ));
        }
        let Some(sender_arg) = args.first() else {
            return None;
        };
        let (sender, payload_ty) = self.check_async_channel_sender_arg(
            scopes,
            "reserve",
            sender_arg,
            explicit_payload.as_ref(),
        )?;
        let permit_ty = std_send_permit_ty(payload_ty.clone());
        let result_ty = std_result_ty(permit_ty, std_async_error_ty());
        Some(TExpr {
            span,
            ty: generated_future_ty(
                format!("channel_reserve_{}", mangle_ty_fragment(&payload_ty)),
                result_ty,
                true,
                true,
            ),
            kind: TExprKind::AsyncChannelReserve {
                sender: Box::new(sender),
                payload_ty,
            },
        })
    }

    pub(super) fn check_async_channel_recv_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        let explicit_payload = self.explicit_async_payload_ty("recv", span, type_args);
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("`async::recv` expects 1 argument, got {}", args.len()),
            ));
        }
        let expected = explicit_payload
            .as_ref()
            .map(|ty| std_receiver_ty(ty.clone()));
        let Some(receiver_arg) = args.first() else {
            return None;
        };
        let receiver = self.check_expr(scopes, receiver_arg, expected.as_ref())?;
        if let Some(expected) = expected.as_ref() {
            self.require_assignable(expected, &receiver.ty, receiver.span);
        }
        let payload_ty = explicit_payload
            .or_else(|| {
                std_id::std_async_receiver_payload_arg(&self.ctx.resolved, &receiver.ty).cloned()
            })
            .unwrap_or_else(|| {
                self.diagnostics.push(Diagnostic::new(
                    receiver.span,
                    format!(
                        "`async::recv` requires `Receiver<T>`, got `{}`",
                        receiver.ty
                    ),
                ));
                Ty::Unknown
            });
        self.check_channel_payload_message(&payload_ty, receiver.span);
        let result_ty = std_result_ty(payload_ty.clone(), std_async_error_ty());
        Some(TExpr {
            span,
            ty: generated_future_ty(
                format!("channel_recv_{}", mangle_ty_fragment(&payload_ty)),
                result_ty,
                true,
                true,
            ),
            kind: TExprKind::AsyncChannelRecv {
                receiver: Box::new(receiver),
                payload_ty,
            },
        })
    }

    pub(super) fn check_async_task_group_next_task_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        let explicit_payload = self.explicit_async_payload_ty("group_next_task", span, type_args);
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "`async::group_next_task` expects 1 argument, got {}",
                    args.len()
                ),
            ));
        }
        let expected = explicit_payload
            .as_ref()
            .map(|ty| Ty::const_pointer_to(std_task_group_ty(ty.clone())));
        let Some(group_arg) = args.first() else {
            return None;
        };
        let group = self.check_expr(scopes, group_arg, expected.as_ref())?;
        if let Some(expected) = expected.as_ref() {
            self.require_assignable(expected, &group.ty, group.span);
        }
        let payload_ty = explicit_payload
            .or_else(|| {
                let Ty::Pointer {
                    nullable: false,
                    inner,
                    ..
                } = &group.ty
                else {
                    return None;
                };
                std_id::std_async_task_group_payload_arg(&self.ctx.resolved, inner).cloned()
            })
            .unwrap_or_else(|| {
                self.diagnostics.push(Diagnostic::new(
                    group.span,
                    format!(
                        "`async::group_next_task` requires `*TaskGroup<T>`, got `{}`",
                        group.ty
                    ),
                ));
                Ty::Unknown
            });
        let task_ty = std_task_ty(payload_ty.clone());
        let result_ty = std_result_ty(task_ty, std_async_error_ty());
        Some(TExpr {
            span,
            ty: generated_future_ty(
                format!("task_group_next_{}", mangle_ty_fragment(&payload_ty)),
                result_ty,
                true,
                true,
            ),
            kind: TExprKind::AsyncTaskGroupNext {
                group: Box::new(group),
                payload_ty,
            },
        })
    }

    pub(super) fn check_async_channel_permit_send_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        let explicit_payload = self.explicit_async_payload_ty("permit_send", span, type_args);
        if args.len() != 2 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "`async::permit_send` expects 2 arguments, got {}",
                    args.len()
                ),
            ));
        }
        let expected = explicit_payload
            .as_ref()
            .map(|ty| std_send_permit_ty(ty.clone()));
        let Some(permit_arg) = args.first() else {
            return None;
        };
        let permit = self.check_expr(scopes, permit_arg, expected.as_ref())?;
        if let Some(expected) = expected.as_ref() {
            self.require_assignable(expected, &permit.ty, permit.span);
        }
        let payload_ty = explicit_payload
            .or_else(|| {
                std_id::std_async_send_permit_payload_arg(&self.ctx.resolved, &permit.ty).cloned()
            })
            .unwrap_or_else(|| {
                self.diagnostics.push(Diagnostic::new(
                    permit.span,
                    format!(
                        "`async::permit_send` requires `SendPermit<T>`, got `{}`",
                        permit.ty
                    ),
                ));
                Ty::Unknown
            });
        let Some(value_arg) = args.get(1) else {
            return None;
        };
        let value = self.check_expr(scopes, value_arg, Some(&payload_ty))?;
        self.require_assignable(&payload_ty, &value.ty, value.span);
        self.check_channel_payload_message(&payload_ty, value.span);
        Some(TExpr {
            span,
            ty: std_result_ty(Ty::Void, std_async_error_ty()),
            kind: TExprKind::AsyncChannelPermitSend {
                permit: Box::new(permit),
                value: Box::new(value),
                payload_ty,
            },
        })
    }

    pub(super) fn check_generic_inference_arg(
        &mut self,
        scopes: &mut LocalScopes,
        arg: &Expr,
        expected: Option<&Ty>,
        allow_resource_captures: bool,
    ) -> Option<TExpr> {
        let saved_scopes = scopes.clone();
        if expr_is_closure_literal(arg) {
            let checked = self.check_closure_literal_preserving_instance(
                scopes,
                arg,
                expected,
                allow_resource_captures,
            );
            *scopes = saved_scopes;
            return checked;
        }
        let checked = self.check_expr(scopes, arg, expected);
        *scopes = saved_scopes;
        checked
    }

    pub(super) fn check_closure_literal_preserving_instance(
        &mut self,
        scopes: &mut LocalScopes,
        arg: &Expr,
        expected: Option<&Ty>,
        allow_resource_captures: bool,
    ) -> Option<TExpr> {
        match &arg.kind {
            ExprKind::Closure {
                is_async,
                params,
                body,
            } => self.check_closure_expr(
                scopes,
                arg.span,
                *is_async,
                params,
                body,
                expected,
                allow_resource_captures,
            ),
            ExprKind::Cast { expr, ty } => {
                let ExprKind::Closure {
                    is_async,
                    params,
                    body,
                } = &expr.kind
                else {
                    return self.check_expr(scopes, arg, expected);
                };
                let target = self.lower_type(ty);
                self.check_closure_cast_allowed(&target, arg.span);
                let checked = self.check_closure_expr(
                    scopes,
                    expr.span,
                    *is_async,
                    params,
                    body,
                    Some(&target),
                    allow_resource_captures,
                )?;
                let checked = TExpr {
                    span: arg.span,
                    ..checked
                };
                Some(self.coerce_expr_to_expected(scopes, checked, Some(&target)))
            }
            _ => self.check_expr(scopes, arg, expected),
        }
    }

    pub(super) fn check_async_spawn_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        if type_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                type_args[1].span,
                "too many type arguments for `spawn`",
            ));
            return None;
        }
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("spawn expects 1 argument, got {}", args.len()),
            ));
        }
        let explicit_output = type_args.first().map(|ty| self.lower_type(ty));
        let expected_output = explicit_output
            .clone()
            .or_else(|| self.task_output_from_spawn_expected(expected));
        let Some(arg) = args.first() else {
            return None;
        };

        let expected_result = expected_output
            .as_ref()
            .map(|ty| std_result_ty(ty.clone(), std_error_ty()));
        let expected_future = expected_result
            .as_ref()
            .map(|result_ty| std_future_ty(result_ty.clone()));
        let expected_closure = expected_future.as_ref().map(|future_ty| Ty::Closure {
            ret: Box::new(future_ty.clone()),
            params: Vec::new(),
            constraints: ConstraintBounds::default(),
        });

        let body = if expr_is_closure_literal(arg) {
            self.check_closure_literal_preserving_instance(
                scopes,
                arg,
                expected_closure.as_ref(),
                false,
            )?
        } else {
            self.check_expr(scopes, arg, None)?
        };

        let mut body_future_ty = None::<Ty>;
        let body_output_ty = match &body.kind {
            TExprKind::Closure {
                is_async,
                params,
                captures,
                ..
            } => {
                if !*is_async {
                    self.diagnostics.push(Diagnostic::new(
                        body.span,
                        "`async::spawn` requires a direct async closure or Awaitable<Result<T, Error>>",
                    ));
                    None
                } else if !params.is_empty() {
                    self.diagnostics.push(Diagnostic::new(
                        body.span,
                        "`async::spawn` async closure must not take parameters",
                    ));
                    None
                } else {
                    for capture in captures {
                        if capture.ty.is_erased_value() {
                            continue;
                        }
                        if let Some(reason) = self.task_boundary_message_violation(
                            &capture.ty,
                            &capture.name,
                            &mut HashSet::new(),
                        ) {
                            self.diagnostics.push(self.diagnostic_with_reason_note(
                                body.span,
                                format!(
                                    "`async::spawn` capture `{}` of type `{}` does not implement `Message`",
                                    capture.name, capture.ty
                                ),
                                reason,
                            ));
                        }
                    }
                    callable_ret_params_ty(&body.ty).and_then(|(ret, _)| {
                        body_future_ty = Some(ret.clone());
                        self.awaitable_ty(&ret, body.span)
                            .map(|awaitable| awaitable.output_ty)
                    })
                }
            }
            _ => {
                body_future_ty = Some(body.ty.clone());
                self.awaitable_ty(&body.ty, body.span)
                    .map(|awaitable| awaitable.output_ty)
            }
        };

        let Some(result_ty) = body_output_ty else {
            self.diagnostics.push(Diagnostic::new(
                body.span,
                format!(
                    "`async::spawn` requires `Awaitable<Result<T, Error>>`, got `{}`",
                    body.ty
                ),
            ));
            return Some(TExpr {
                span,
                ty: Ty::Unknown,
                kind: TExprKind::AsyncSpawn {
                    body: Box::new(body),
                    task_output_ty: Ty::Unknown,
                },
            });
        };
        let Some((task_output_ty, err_ty)) = self.result_ok_err_tys(&result_ty) else {
            self.diagnostics.push(Diagnostic::new(
                body.span,
                format!(
                    "`async::spawn` requires `Awaitable<Result<T, Error>>`, got awaitable yielding `{result_ty}`"
                ),
            ));
            return Some(TExpr {
                span,
                ty: Ty::Unknown,
                kind: TExprKind::AsyncSpawn {
                    body: Box::new(body),
                    task_output_ty: Ty::Unknown,
                },
            });
        };
        if !self.is_std_error_ty(&err_ty) {
            self.diagnostics.push(Diagnostic::new(
                body.span,
                format!("`async::spawn` task error type must be `Error`, got `{err_ty}`"),
            ));
        }
        if let Some(future_ty) = &body_future_ty
            && !self.is_abortable_ty(future_ty)
        {
            self.diagnostics.push(self.named_capability_diagnostic(
                body.span,
                future_ty,
                STD_ASYNC_ABORT_FUTURE_INTERFACE,
                "`async::spawn` needs an abortable future for task cancellation",
            ));
        }
        if let Some(expected_output) = &expected_output {
            self.require_assignable(expected_output, &task_output_ty, body.span);
        }
        if let Some(reason) = self.task_boundary_message_violation(
            &task_output_ty,
            "task result",
            &mut HashSet::new(),
        ) {
            self.diagnostics.push(self.diagnostic_with_reason_note(
                body.span,
                format!(
                    "`async::spawn` task result type `{task_output_ty}` does not implement `Message`"
                ),
                reason,
            ));
        }

        let body = self.consume_affine_expr(scopes, body, false);
        let task_ty = std_task_ty(task_output_ty.clone());
        let result_ty = std_result_ty(task_ty, std_async_error_ty());
        Some(TExpr {
            span,
            ty: result_ty,
            kind: TExprKind::AsyncSpawn {
                body: Box::new(body),
                task_output_ty,
            },
        })
    }

    pub(super) fn task_output_from_spawn_expected(&self, expected: Option<&Ty>) -> Option<Ty> {
        let Some((ok_ty, err_ty)) = expected.and_then(|ty| self.result_ok_err_tys(ty)) else {
            return None;
        };
        if err_ty != std_async_error_ty() {
            return None;
        }
        self.task_output_ty(&ok_ty)
    }

    pub(super) fn check_async_task_cancel_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        self.check_async_task_control_call(scopes, span, type_args, args, AsyncTaskControl::Cancel)
    }

    pub(super) fn check_async_task_is_finished_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        self.check_async_task_control_call(
            scopes,
            span,
            type_args,
            args,
            AsyncTaskControl::IsFinished,
        )
    }

    pub(super) fn check_async_task_control_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
        op: AsyncTaskControl,
    ) -> Option<TExpr> {
        let label = match op {
            AsyncTaskControl::Cancel => "cancel",
            AsyncTaskControl::IsFinished => "is_finished",
        };
        if type_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                type_args[1].span,
                format!("too many type arguments for `{label}`"),
            ));
            return None;
        }
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{label} expects 1 argument, got {}", args.len()),
            ));
        }
        let explicit_output = type_args.first().map(|ty| self.lower_type(ty));
        let expected_task = explicit_output
            .as_ref()
            .map(|ty| Ty::const_pointer_to(std_task_ty(ty.clone())));
        let Some(arg) = args.first() else {
            return None;
        };
        let task = self.check_expr(scopes, arg, expected_task.as_ref())?;
        if let Some(expected) = expected_task.as_ref() {
            self.require_assignable(expected, &task.ty, task.span);
        }
        let Some(task_output_ty) = self.task_output_from_pointer_ty(&task.ty) else {
            self.diagnostics.push(Diagnostic::new(
                task.span,
                format!("`async::{label}` requires `*Task<T>`, got `{}`", task.ty),
            ));
            return Some(TExpr {
                span,
                ty: Ty::Unknown,
                kind: match op {
                    AsyncTaskControl::Cancel => TExprKind::AsyncTaskCancel {
                        task: Box::new(task),
                        task_output_ty: Ty::Unknown,
                    },
                    AsyncTaskControl::IsFinished => TExprKind::AsyncTaskIsFinished {
                        task: Box::new(task),
                        task_output_ty: Ty::Unknown,
                    },
                },
            });
        };
        let ret_ok = match op {
            AsyncTaskControl::Cancel => Ty::Void,
            AsyncTaskControl::IsFinished => Ty::Bool,
        };
        let ret_ty = std_result_ty(ret_ok, std_async_error_ty());
        Some(TExpr {
            span,
            ty: ret_ty,
            kind: match op {
                AsyncTaskControl::Cancel => TExprKind::AsyncTaskCancel {
                    task: Box::new(task),
                    task_output_ty,
                },
                AsyncTaskControl::IsFinished => TExprKind::AsyncTaskIsFinished {
                    task: Box::new(task),
                    task_output_ty,
                },
            },
        })
    }
}

fn parameter_names_note(
    params: &[Ty],
    param_names: Option<&[String]>,
    param_mutabilities: Option<&[BindingMutability]>,
    expected_len: usize,
) -> Option<String> {
    let names = param_names?;
    let mutabilities = param_mutabilities?;
    if expected_len == 0 || names.is_empty() || mutabilities.is_empty() {
        return None;
    }
    let display = names
        .iter()
        .zip(mutabilities.iter())
        .zip(params.iter())
        .take(expected_len)
        .map(|((name, mutability), ty)| {
            format!("`{}`", format_typed_binding(ty, name, *mutability))
        })
        .collect::<Vec<_>>()
        .join(", ");
    (!display.is_empty()).then(|| format!("parameters: {display}"))
}

fn selector_path_span(path: &[crate::ast::Ident]) -> Option<crate::span::Span> {
    Some(path.first()?.span.merge(path.last()?.span))
}
