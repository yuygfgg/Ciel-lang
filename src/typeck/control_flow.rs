use super::*;

impl TypeChecker {
    pub(super) fn push_control_context(&mut self, kind: ControlContextKind) {
        self.control_contexts.push(ControlContext {
            kind,
            break_scopes: Vec::new(),
        });
    }

    pub(super) fn pop_control_context(&mut self) -> ControlContext {
        self.control_contexts
            .pop()
            .expect("control context stack is not empty")
    }

    pub(super) fn record_break_scope(&mut self, scopes: &LocalScopes) -> bool {
        if let Some(context) = self.control_contexts.iter_mut().rev().find(|context| {
            matches!(
                context.kind,
                ControlContextKind::Loop | ControlContextKind::Switch
            )
        }) {
            context.break_scopes.push(scopes.clone());
            true
        } else {
            false
        }
    }

    pub(super) fn has_continue_target(&self) -> bool {
        self.control_contexts
            .iter()
            .rev()
            .any(|context| matches!(context.kind, ControlContextKind::Loop))
    }

    pub(super) fn check_block(
        &mut self,
        scopes: &mut LocalScopes,
        block: &Block,
        ret_ty: &Ty,
    ) -> Option<CheckedBlockFlow> {
        scopes.push();
        let result = self.check_block_with_existing_scope(scopes, block, ret_ty);
        scopes.pop();
        result
    }

    pub(super) fn check_block_with_existing_scope(
        &mut self,
        scopes: &mut LocalScopes,
        block: &Block,
        ret_ty: &Ty,
    ) -> Option<CheckedBlockFlow> {
        let (statements, flow) = self.check_statement_sequence(scopes, &block.statements, ret_ty);
        Some(CheckedBlockFlow {
            block: TBlock {
                span: block.span,
                statements,
            },
            flow,
        })
    }

    pub(super) fn check_statement_sequence(
        &mut self,
        scopes: &mut LocalScopes,
        source_statements: &[Stmt],
        ret_ty: &Ty,
    ) -> (Vec<TStmt>, Flow) {
        let mut statements = Vec::new();
        let mut flow = Flow::fallthrough();
        for stmt in source_statements {
            if let Some(checked) = self.check_stmt(scopes, stmt, ret_ty) {
                if flow.can_fallthrough {
                    flow = checked.flow;
                }
                statements.push(checked.stmt);
            }
        }
        (statements, flow)
    }

    pub(super) fn check_unsafe_block_expr(
        &mut self,
        scopes: &mut LocalScopes,
        block: &ExprBlock,
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        scopes.push();
        let previous_unsafe_depth = self.unsafe_depth;
        self.unsafe_depth += 1;
        let ret_ty = self.current_return_ty.clone();
        let (statements, flow) = self.check_statement_sequence(scopes, &block.statements, &ret_ty);
        let value = if flow.can_fallthrough {
            block
                .value
                .as_ref()
                .and_then(|expr| self.check_expr(scopes, expr, expected))
                .map(Box::new)
        } else {
            None
        };
        let ty = if !flow.can_fallthrough {
            Ty::Never
        } else {
            value
                .as_ref()
                .map(|expr| expr.ty.clone())
                .unwrap_or(Ty::Void)
        };
        self.unsafe_depth = previous_unsafe_depth;
        scopes.pop();
        Some(TExpr {
            span: block.span,
            ty,
            kind: TExprKind::UnsafeBlock { statements, value },
        })
    }

    pub(super) fn check_stmt(
        &mut self,
        scopes: &mut LocalScopes,
        stmt: &Stmt,
        ret_ty: &Ty,
    ) -> Option<CheckedStmtFlow> {
        let (kind, flow) = match &stmt.kind {
            StmtKind::Block(block) => {
                let checked = self.check_block(scopes, block, ret_ty)?;
                (TStmtKind::Block(checked.block), checked.flow)
            }
            StmtKind::VarDecl {
                ty,
                name,
                mutability,
                local_id,
                init,
            } => {
                let checked =
                    self.check_local_decl_init(scopes, stmt.span, ty, &name.name, init.as_ref());
                let (binding_ty, flow_ty) = self.storage_and_flow_ty(&checked.ty);
                if let Err(name) = scopes.insert(
                    *local_id,
                    Binding {
                        name: name.name.clone(),
                        ty: binding_ty,
                        flow_ty,
                        narrowed_ty: None,
                        init_state: InitState::from_assigned(checked.assigned),
                        mutability: *mutability,
                        captured: false,
                        declared_loop_depth: self.current_loop_depth,
                    },
                ) {
                    self.diagnostics.push(Diagnostic::new(
                        stmt.span,
                        format!("duplicate local `{name}`"),
                    ));
                }
                (
                    TStmtKind::VarDecl {
                        ty: checked.ty,
                        name: name.name.clone(),
                        local_id: *local_id,
                        init: checked.init,
                    },
                    Flow::fallthrough(),
                )
            }
            StmtKind::Assign { target, value } => {
                let checked_target = self.check_lvalue(scopes, target, false)?;
                let assignment_allowed = self.validate_assignment_target(
                    scopes,
                    &checked_target,
                    checked_target.expr.span,
                );
                let target = checked_target.expr;
                let expected_ty = assignment_expected_ty(scopes, &target);
                let diagnostics_before_value = self.diagnostics.len();
                let value = self.check_consumed_expr(scopes, value, Some(&expected_ty), false)?;
                if expected_ty.is_erased_value() {
                    self.diagnostics.push(Diagnostic::new(
                        stmt.span,
                        "void values are implicit and cannot be explicitly assigned",
                    ));
                }
                if self.diagnostics.len() == diagnostics_before_value {
                    self.require_assignable(&expected_ty, &value.ty, stmt.span);
                }
                if assignment_allowed {
                    self.mark_assignment_complete(scopes, &target, &value.ty);
                }
                (TStmtKind::Assign { target, value }, Flow::fallthrough())
            }
            StmtKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                let cond = self.check_expr(scopes, cond, Some(&Ty::Bool))?;
                self.require_assignable(&Ty::Bool, &cond.ty, cond.span);
                let before = scopes.clone();
                let mut then_scopes = before.clone();
                self.apply_condition_narrowing(&mut then_scopes, &cond, true);
                let checked_then = self.check_block(&mut then_scopes, then_block, ret_ty)?;
                let mut else_scopes = before.clone();
                self.apply_condition_narrowing(&mut else_scopes, &cond, false);
                let checked_else = else_branch
                    .as_ref()
                    .and_then(|stmt| self.check_stmt(&mut else_scopes, stmt, ret_ty));
                let else_flow = checked_else
                    .as_ref()
                    .map(|checked| checked.flow)
                    .unwrap_or_else(Flow::fallthrough);

                let mut reachable = Vec::new();
                if checked_then.flow.can_fallthrough {
                    reachable.push(then_scopes);
                }
                if else_flow.can_fallthrough {
                    reachable.push(else_scopes);
                }
                scopes.merge_reachable_flows(&reachable);
                let flow = Flow {
                    can_fallthrough: !reachable.is_empty(),
                };
                let then_block = checked_then.block;
                let else_branch = checked_else.map(|checked| Box::new(checked.stmt));
                (
                    TStmtKind::If {
                        cond,
                        then_block,
                        else_branch,
                    },
                    flow,
                )
            }
            StmtKind::While { cond, body } => {
                let cond = self.check_expr(scopes, cond, Some(&Ty::Bool))?;
                self.require_assignable(&Ty::Bool, &cond.ty, cond.span);
                let mut body_scopes = scopes.clone();
                self.push_control_context(ControlContextKind::Loop);
                self.current_loop_depth += 1;
                let checked_body = self.check_block(&mut body_scopes, body, ret_ty);
                self.current_loop_depth -= 1;
                let loop_context = self.pop_control_context();
                let checked_body = checked_body?;
                let flow = if bool_literal_is(&cond, true) && loop_context.break_scopes.is_empty() {
                    Flow::no_fallthrough()
                } else {
                    Flow::fallthrough()
                };
                (
                    TStmtKind::While {
                        cond,
                        body: checked_body.block,
                    },
                    flow,
                )
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                scopes.push();
                let init = init
                    .as_ref()
                    .and_then(|init| self.check_for_init(scopes, init));
                let cond = cond
                    .as_ref()
                    .and_then(|expr| self.check_expr(scopes, expr, Some(&Ty::Bool)));
                if let Some(cond) = &cond {
                    self.require_assignable(&Ty::Bool, &cond.ty, cond.span);
                }
                let mut loop_scopes = scopes.clone();
                self.current_loop_depth += 1;
                let step = step
                    .as_ref()
                    .and_then(|step| self.check_for_step(&mut loop_scopes, step));
                self.push_control_context(ControlContextKind::Loop);
                let checked_body = self.check_block(&mut loop_scopes, body, ret_ty);
                self.current_loop_depth -= 1;
                let loop_context = self.pop_control_context();
                let checked_body = checked_body?;
                let condition_always_true = cond
                    .as_ref()
                    .map(|cond| bool_literal_is(cond, true))
                    .unwrap_or(true);
                let flow = if condition_always_true && loop_context.break_scopes.is_empty() {
                    Flow::no_fallthrough()
                } else {
                    Flow::fallthrough()
                };
                scopes.pop();
                (
                    TStmtKind::For {
                        init,
                        cond,
                        step,
                        body: checked_body.block,
                    },
                    flow,
                )
            }
            StmtKind::Switch {
                expr,
                cases,
                has_default,
                default,
            } => self.check_switch_stmt(
                scopes,
                stmt.span,
                expr,
                cases,
                *has_default,
                default,
                ret_ty,
            )?,
            StmtKind::Defer(expr) => {
                let expr = self.check_expr(scopes, expr, None)?;
                if !matches!(expr.kind, TExprKind::Call { .. }) {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        "defer requires a direct function call",
                    ));
                }
                (TStmtKind::Defer(expr), Flow::fallthrough())
            }
            StmtKind::Return(expr) => {
                if ret_ty.is_never() {
                    self.diagnostics.push(Diagnostic::new(
                        stmt.span,
                        "`never` function cannot return normally",
                    ));
                    return Some(CheckedStmtFlow {
                        stmt: TStmt {
                            span: stmt.span,
                            kind: TStmtKind::Return(None),
                        },
                        flow: Flow::no_fallthrough(),
                    });
                }
                if matches!(ret_ty, Ty::OpaqueReturn { .. }) {
                    let expr = match expr {
                        Some(expr) => {
                            let expected = self
                                .current_opaque_return
                                .as_ref()
                                .and_then(|state| state.concrete_ty.clone());
                            let expr =
                                self.check_consumed_expr(scopes, expr, expected.as_ref(), true)?;
                            let concrete_ty = self.normalize_meta_repr_markers(&expr.ty, expr.span);
                            self.record_opaque_return_ty(ret_ty, &concrete_ty, expr.span);
                            Some(expr)
                        }
                        None => {
                            self.diagnostics.push(Diagnostic::new(
                                stmt.span,
                                format!("function must return `{ret_ty}`"),
                            ));
                            None
                        }
                    };
                    return Some(CheckedStmtFlow {
                        stmt: TStmt {
                            span: stmt.span,
                            kind: TStmtKind::Return(expr),
                        },
                        flow: Flow::no_fallthrough(),
                    });
                }
                let expr = match expr {
                    Some(expr) => {
                        let expr = self.check_consumed_expr(scopes, expr, Some(ret_ty), true)?;
                        if ret_ty.is_void() && !expr.ty.is_void() {
                            self.diagnostics.push(Diagnostic::new(
                                expr.span,
                                "void function cannot return a non-void value",
                            ));
                        }
                        self.require_return_assignable(ret_ty, &expr.ty, expr.span);
                        if (matches!(expr.ty, Ty::GeneratedFuture { .. })
                            || ty_has_hidden_state(&expr.ty))
                            && let Some(reason) = self.future_state_escape_violation(
                                &expr.ty,
                                "returned future",
                                &mut HashSet::new(),
                            )
                        {
                            self.diagnostics.push(self.diagnostic_with_reason_note(
                                expr.span,
                                "returned future state cannot safely escape this function",
                                reason,
                            ));
                        }
                        Some(expr)
                    }
                    None => {
                        if !ret_ty.is_erased_value() {
                            self.diagnostics.push(Diagnostic::new(
                                stmt.span,
                                format!("function must return `{ret_ty}`"),
                            ));
                        }
                        None
                    }
                };
                (TStmtKind::Return(expr), Flow::no_fallthrough())
            }
            StmtKind::Break => {
                if !self.record_break_scope(scopes) {
                    self.diagnostics
                        .push(Diagnostic::new(stmt.span, "break outside loop or switch"));
                }
                (TStmtKind::Break, Flow::no_fallthrough())
            }
            StmtKind::Continue => {
                if !self.has_continue_target() {
                    self.diagnostics
                        .push(Diagnostic::new(stmt.span, "continue outside loop"));
                }
                (TStmtKind::Continue, Flow::no_fallthrough())
            }
            StmtKind::Expr(expr) => {
                let expr = self.check_consumed_expr(scopes, expr, None, false)?;
                let flow = if expr.is_never() {
                    Flow::no_fallthrough()
                } else {
                    Flow::fallthrough()
                };
                (TStmtKind::Expr(expr), flow)
            }
        };

        Some(CheckedStmtFlow {
            stmt: TStmt {
                span: stmt.span,
                kind,
            },
            flow,
        })
    }

    pub(super) fn check_for_init(
        &mut self,
        scopes: &mut LocalScopes,
        init: &ForInit,
    ) -> Option<TForInit> {
        match init {
            ForInit::VarDecl {
                ty,
                name,
                mutability,
                local_id,
                init: initializer,
            } => {
                let checked = self.check_local_decl_init(
                    scopes,
                    name.span,
                    ty,
                    &name.name,
                    initializer.as_ref(),
                );
                let local_name = name.name.clone();
                let local_span = name.span;
                let (binding_ty, flow_ty) = self.storage_and_flow_ty(&checked.ty);
                if let Err(duplicate) = scopes.insert(
                    *local_id,
                    Binding {
                        name: local_name.clone(),
                        ty: binding_ty,
                        flow_ty,
                        narrowed_ty: None,
                        init_state: InitState::from_assigned(checked.assigned),
                        mutability: *mutability,
                        captured: false,
                        declared_loop_depth: self.current_loop_depth,
                    },
                ) {
                    self.diagnostics.push(Diagnostic::new(
                        local_span,
                        format!("duplicate local `{duplicate}`"),
                    ));
                }
                Some(TForInit::VarDecl {
                    ty: checked.ty,
                    name: local_name,
                    local_id: *local_id,
                    init: checked.init,
                })
            }
            ForInit::Assign { target, value } => {
                let checked_target = self.check_lvalue(scopes, target, false)?;
                let assignment_allowed = self.validate_assignment_target(
                    scopes,
                    &checked_target,
                    checked_target.expr.span,
                );
                let target = checked_target.expr;
                let expected_ty = assignment_expected_ty(scopes, &target);
                let value = self.check_consumed_expr(scopes, value, Some(&expected_ty), false)?;
                if expected_ty.is_erased_value() {
                    self.diagnostics.push(Diagnostic::new(
                        target.span,
                        "void values are implicit and cannot be explicitly assigned",
                    ));
                }
                self.require_assignable(&expected_ty, &value.ty, value.span);
                if assignment_allowed {
                    self.mark_assignment_complete(scopes, &target, &value.ty);
                }
                Some(TForInit::Assign { target, value })
            }
            ForInit::Expr(expr) => self
                .check_consumed_expr(scopes, expr, None, false)
                .map(TForInit::Expr),
        }
    }

    pub(super) fn check_for_step(
        &mut self,
        scopes: &mut LocalScopes,
        step: &ForInit,
    ) -> Option<TForInit> {
        match step {
            ForInit::Assign { target, value } => {
                let checked_target = self.check_lvalue(scopes, target, false)?;
                let assignment_allowed = self.validate_assignment_target(
                    scopes,
                    &checked_target,
                    checked_target.expr.span,
                );
                let target = checked_target.expr;
                let expected_ty = assignment_expected_ty(scopes, &target);
                let value = self.check_consumed_expr(scopes, value, Some(&expected_ty), false)?;
                if expected_ty.is_erased_value() {
                    self.diagnostics.push(Diagnostic::new(
                        target.span,
                        "void values are implicit and cannot be explicitly assigned",
                    ));
                }
                self.require_assignable(&expected_ty, &value.ty, value.span);
                if assignment_allowed {
                    self.mark_assignment_complete(scopes, &target, &value.ty);
                }
                Some(TForInit::Assign { target, value })
            }
            ForInit::Expr(expr) => self
                .check_consumed_expr(scopes, expr, None, false)
                .map(TForInit::Expr),
            ForInit::VarDecl { ty, name, .. } => {
                self.diagnostics.push(Diagnostic::new(
                    ty.span.merge(name.span),
                    "for step cannot declare a variable",
                ));
                None
            }
        }
    }

    pub(super) fn check_switch_stmt(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        expr: &Expr,
        cases: &[CaseClause],
        has_default: bool,
        default: &[Stmt],
        ret_ty: &Ty,
    ) -> Option<(TStmtKind, Flow)> {
        let expr = self.check_expr(scopes, expr, None)?;
        let switch_ty = if let Ty::OpaqueState { base, .. } = &expr.ty {
            (**base).clone()
        } else {
            expr.ty.clone()
        };
        let Ty::Named { name, args } = &switch_ty else {
            self.diagnostics.push(Diagnostic::new(
                expr.span,
                format!("switch requires enum value, got `{}`", expr.ty),
            ));
            return Some((TStmtKind::Unsupported, Flow::fallthrough()));
        };
        let enum_type_name = enum_instance_name(name, args);
        self.ensure_enum_instance(&switch_ty);
        let Some(checked_enum) = self.ctx.checked_enums.get(&enum_type_name).cloned() else {
            self.diagnostics.push(Diagnostic::new(
                expr.span,
                format!("`{}` is not an enum type", expr.ty),
            ));
            return Some((TStmtKind::Unsupported, Flow::fallthrough()));
        };
        let expr = self.consume_affine_expr(scopes, expr, false);

        let before = scopes.clone();
        let mut top_patterns = Vec::new();
        let mut checked_cases = Vec::new();
        let mut reachable_after_switch = Vec::new();
        self.push_control_context(ControlContextKind::Switch);
        for case in cases {
            let Some((variant_index, pattern)) =
                self.check_case_pattern(&case.pattern, &expr.ty, &checked_enum, true)
            else {
                continue;
            };
            top_patterns.push(pattern.clone());

            let mut case_scopes = before.clone();
            case_scopes.push();
            let mut bindings = Vec::new();
            pattern.collect_bindings(&mut bindings);
            for (local_id, binding_name, mutability, binding_ty) in bindings {
                let (storage_ty, flow_ty) = self.storage_and_flow_ty(&binding_ty);
                if let Err(duplicate) = case_scopes.insert(
                    *local_id,
                    Binding {
                        name: binding_name.clone(),
                        ty: storage_ty,
                        flow_ty,
                        narrowed_ty: None,
                        init_state: InitState::Assigned,
                        mutability,
                        captured: false,
                        declared_loop_depth: self.current_loop_depth,
                    },
                ) {
                    self.diagnostics.push(Diagnostic::new(
                        pattern_span(&case.pattern),
                        format!("duplicate pattern binding `{duplicate}`"),
                    ));
                }
            }
            let (statements, case_flow) =
                self.check_statement_sequence(&mut case_scopes, &case.statements, ret_ty);
            case_scopes.pop();
            if case_flow.can_fallthrough {
                reachable_after_switch.push(case_scopes);
            }
            checked_cases.push(TCase {
                variant_name: checked_enum.variants[variant_index].name.clone(),
                variant_index,
                pattern,
                statements,
            });
        }

        let exhaustive = self.patterns_exhaustive_for_type(&switch_ty, &top_patterns);
        if !has_default && !exhaustive {
            self.diagnostics
                .push(Diagnostic::new(span, "switch is not exhaustive"));
        }

        let mut default_scopes = before.clone();
        default_scopes.push();
        let default_break_start = self
            .control_contexts
            .last()
            .map(|context| context.break_scopes.len())
            .unwrap_or(0);
        let (default, default_flow) =
            self.check_statement_sequence(&mut default_scopes, default, ret_ty);
        default_scopes.pop();
        if has_default && !exhaustive {
            if default_flow.can_fallthrough {
                reachable_after_switch.push(default_scopes);
            }
        } else if has_default && let Some(context) = self.control_contexts.last_mut() {
            context.break_scopes.truncate(default_break_start);
        } else if !has_default && !exhaustive {
            reachable_after_switch.push(before.clone());
        }

        let switch_context = self.pop_control_context();
        reachable_after_switch.extend(switch_context.break_scopes);
        scopes.merge_reachable_flows(&reachable_after_switch);
        let flow = Flow {
            can_fallthrough: !reachable_after_switch.is_empty(),
        };

        Some((
            TStmtKind::Switch {
                expr,
                enum_type_name,
                cases: checked_cases,
                has_default,
                default,
                can_fallthrough: flow.can_fallthrough,
            },
            flow,
        ))
    }

    pub(super) fn check_case_pattern(
        &mut self,
        pattern: &Pattern,
        expected_ty: &Ty,
        checked_enum: &CheckedEnum,
        is_case_head: bool,
    ) -> Option<(usize, TPattern)> {
        let Pattern::Variant(name, _subpatterns) = pattern else {
            self.diagnostics.push(Diagnostic::new(
                pattern_span(pattern),
                "top-level wildcard pattern is not supported; use default",
            ));
            return None;
        };
        let Some(checked_pattern) = self.check_pattern(pattern, expected_ty, is_case_head) else {
            return None;
        };
        let TPattern::Variant {
            variant_index,
            variant_name,
            ..
        } = &checked_pattern
        else {
            self.diagnostics.push(Diagnostic::new(
                pattern_span(pattern),
                "switch case must name an enum variant",
            ));
            return None;
        };
        if !checked_enum
            .variants
            .iter()
            .any(|variant| variant.name == *variant_name)
        {
            let mut diagnostic = Diagnostic::new(
                name.span,
                format!(
                    "`{}` is not a variant of `{}`",
                    name.display, checked_enum.name
                ),
            );
            if let Some(note) = suggest::did_you_mean_note(
                variant_name,
                checked_enum.variants.iter().map(|variant| &variant.name),
            ) {
                diagnostic = diagnostic.note(note);
            }
            self.diagnostics.push(diagnostic);
            return None;
        }
        Some((*variant_index, checked_pattern))
    }

    pub(super) fn check_pattern(
        &mut self,
        pattern: &Pattern,
        expected_ty: &Ty,
        is_case_head: bool,
    ) -> Option<TPattern> {
        match pattern {
            Pattern::Wildcard(span) => {
                if is_case_head {
                    self.diagnostics.push(Diagnostic::new(
                        *span,
                        "top-level wildcard pattern is not supported; use default",
                    ));
                    None
                } else {
                    Some(TPattern::Wildcard {
                        ty: expected_ty.clone(),
                    })
                }
            }
            Pattern::Variant(name, subpatterns) => match name.kind {
                PatternNameKind::Variant(_) | PatternNameKind::VariantCandidates(_) => {
                    self.check_variant_pattern(name, subpatterns, expected_ty)
                }
                PatternNameKind::Binding {
                    local_id,
                    mutability,
                } if !is_case_head && subpatterns.is_empty() => Some(TPattern::Binding {
                    local_id,
                    name: name.display.clone(),
                    mutability,
                    ty: expected_ty.clone(),
                }),
                PatternNameKind::Binding { .. } => {
                    self.diagnostics.push(Diagnostic::new(
                        name.span,
                        "pattern binding cannot have payload patterns",
                    ));
                    None
                }
                PatternNameKind::Error => None,
            },
        }
    }

    pub(super) fn check_variant_pattern(
        &mut self,
        name: &PatternName,
        subpatterns: &[Pattern],
        expected_ty: &Ty,
    ) -> Option<TPattern> {
        let (expected_base_ty, opaque_state) = if let Ty::OpaqueState { base, state } = expected_ty
        {
            (base.as_ref(), Some(state.as_slice()))
        } else {
            (expected_ty, None)
        };
        let Some((_def_id, sig)) = self.lookup_pattern_variant_name(name, expected_base_ty) else {
            let mut diagnostic = Diagnostic::new(
                name.span,
                format!("unknown enum variant `{}`", name.display),
            );
            if let Some(checked_enum) = self.checked_enum_for_type(expected_base_ty)
                && let Some(note) = suggest::did_you_mean_note_with_display(
                    name.display.rsplit("::").next().unwrap_or(&name.display),
                    checked_enum.variants.iter().map(|variant| {
                        (
                            variant.name.clone(),
                            format!("{}::{}", checked_enum.name, variant.name),
                        )
                    }),
                )
            {
                diagnostic = diagnostic.note(note);
            }
            self.diagnostics.push(diagnostic);
            return None;
        };
        let Ty::Named {
            name: enum_name,
            args: enum_args,
        } = expected_base_ty
        else {
            self.diagnostics.push(Diagnostic::new(
                name.span,
                format!(
                    "variant `{}` pattern requires enum value, got `{expected_ty}`",
                    name.display
                ),
            ));
            return None;
        };
        let variant_name = name
            .path
            .last()
            .map(|ident| ident.name.clone())
            .unwrap_or_else(|| name.display.clone());
        if enum_name != &sig.enum_name {
            self.diagnostics.push(Diagnostic::new(
                name.span,
                format!(
                    "variant `{}` belongs to `{}`, not `{expected_ty}`",
                    name.display, sig.enum_name
                ),
            ));
            return None;
        }
        if enum_args.len() != sig.enum_generics.len() {
            self.diagnostics.push(Diagnostic::new(
                name.span,
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
        let use_logical_payload = subpatterns.len() == logical_payload_tys.len();
        let use_physical_payload =
            subpatterns.len() == physical_payload_tys.len() && !use_logical_payload;
        if !use_logical_payload && !use_physical_payload {
            self.diagnostics.push(Diagnostic::new(
                name.span,
                format!(
                    "variant `{}` expects {} pattern fields, got {}",
                    name.display,
                    physical_payload_tys.len(),
                    subpatterns.len()
                ),
            ));
            return None;
        }

        let mut payload = Vec::new();
        if use_logical_payload {
            let mut physical_index = 0usize;
            for (subpattern, payload_ty) in subpatterns.iter().zip(logical_payload_tys.iter()) {
                let payload_ty = if payload_ty.is_erased_value() {
                    payload_ty.clone()
                } else {
                    let projected = opaque_state
                        .map(|state| {
                            self.project_opaque_variant_payload_ty(
                                payload_ty.clone(),
                                state,
                                &variant_name,
                                physical_index,
                            )
                        })
                        .unwrap_or_else(|| payload_ty.clone());
                    physical_index += 1;
                    projected
                };
                payload.push(self.check_pattern(subpattern, &payload_ty, false)?);
            }
        } else {
            for (physical_index, (subpattern, payload_ty)) in subpatterns
                .iter()
                .zip(physical_payload_tys.iter())
                .enumerate()
            {
                let payload_ty = opaque_state
                    .map(|state| {
                        self.project_opaque_variant_payload_ty(
                            payload_ty.clone(),
                            state,
                            &variant_name,
                            physical_index,
                        )
                    })
                    .unwrap_or_else(|| payload_ty.clone());
                payload.push(self.check_pattern(subpattern, &payload_ty, false)?);
            }
        }
        self.ensure_enum_instance(expected_base_ty);
        Some(TPattern::Variant {
            ty: expected_base_ty.clone(),
            enum_type_name: enum_instance_name(enum_name, enum_args),
            variant_name,
            variant_index: sig.variant_index,
            payload,
        })
    }

    pub(super) fn lookup_pattern_variant_name(
        &mut self,
        name: &PatternName,
        expected_ty: &Ty,
    ) -> Option<(DefId, VariantSig)> {
        match &name.kind {
            PatternNameKind::Variant(def_id) => {
                let sig = self.ctx.variants.get(def_id)?.clone();
                Some((*def_id, sig))
            }
            PatternNameKind::VariantCandidates(candidates) => self.select_variant_candidate(
                &name.display,
                name.span,
                candidates,
                Some(expected_ty),
            ),
            PatternNameKind::Binding { .. } | PatternNameKind::Error => None,
        }
    }

    pub(super) fn patterns_exhaustive_for_type(&mut self, ty: &Ty, patterns: &[TPattern]) -> bool {
        let rows = patterns
            .iter()
            .cloned()
            .map(|pattern| vec![pattern])
            .collect::<Vec<_>>();
        self.tuple_patterns_exhaustive(&[ty.clone()], &rows)
    }

    pub(super) fn tuple_patterns_exhaustive(&mut self, tys: &[Ty], rows: &[Vec<TPattern>]) -> bool {
        let Some((first_ty, rest_tys)) = tys.split_first() else {
            return !rows.is_empty();
        };
        if rows.iter().any(|row| row.len() != tys.len()) {
            return false;
        }

        if let Some(checked_enum) = self.checked_enum_for_type(first_ty) {
            for variant in &checked_enum.variants {
                let mut specialized_rows = Vec::new();
                for row in rows {
                    match &row[0] {
                        TPattern::Wildcard { .. } | TPattern::Binding { .. } => {
                            let mut specialized = variant
                                .payload
                                .iter()
                                .cloned()
                                .map(|ty| TPattern::Wildcard { ty })
                                .collect::<Vec<_>>();
                            specialized.extend(row[1..].iter().cloned());
                            specialized_rows.push(specialized);
                        }
                        TPattern::Variant {
                            variant_name,
                            payload,
                            ..
                        } if variant_name == &variant.name => {
                            let mut specialized = payload
                                .iter()
                                .filter(|pattern| !pattern.ty().is_erased_value())
                                .cloned()
                                .collect::<Vec<_>>();
                            specialized.extend(row[1..].iter().cloned());
                            specialized_rows.push(specialized);
                        }
                        TPattern::Variant { .. } => {}
                    }
                }
                let mut specialized_tys = variant.payload.clone();
                specialized_tys.extend_from_slice(rest_tys);
                if !self.tuple_patterns_exhaustive(&specialized_tys, &specialized_rows) {
                    return false;
                }
            }
            true
        } else {
            let rest_rows = rows
                .iter()
                .filter_map(|row| match row[0] {
                    TPattern::Wildcard { .. } | TPattern::Binding { .. } => Some(row[1..].to_vec()),
                    TPattern::Variant { .. } => None,
                })
                .collect::<Vec<_>>();
            self.tuple_patterns_exhaustive(rest_tys, &rest_rows)
        }
    }

    pub(super) fn checked_enum_for_type(&mut self, ty: &Ty) -> Option<CheckedEnum> {
        let Ty::Named { name, args } = ty else {
            return None;
        };
        self.ensure_enum_instance(ty);
        let instance_name = enum_instance_name(name, args);
        self.ctx.checked_enums.get(&instance_name).cloned()
    }

    pub(super) fn with_return_loop_move_context<T>(
        &mut self,
        allow_loop_move: bool,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = self.return_loop_move_depth;
        if allow_loop_move {
            self.return_loop_move_depth += 1;
        }
        let result = f(self);
        self.return_loop_move_depth = previous;
        result
    }

    pub(super) fn without_return_loop_move_context<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = self.return_loop_move_depth;
        self.return_loop_move_depth = 0;
        let result = f(self);
        self.return_loop_move_depth = previous;
        result
    }
}

fn assignment_expected_ty(scopes: &LocalScopes, target: &TExpr) -> Ty {
    if let TExprKind::Local(local_id, _) = &target.kind
        && let Some(binding) = scopes.get(*local_id)
    {
        return binding.ty.clone();
    }
    target.ty.clone()
}
