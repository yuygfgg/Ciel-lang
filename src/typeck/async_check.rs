use super::*;

impl TypeChecker {
    pub(super) fn async_block_capabilities(&mut self, block: &TBlock) -> (bool, bool) {
        let mut visitor = AsyncSuspensionCapabilityVisitor {
            checker: self,
            await_count: 0,
            abortable: true,
        };
        visitor.visit_block(block);
        let await_count = visitor.await_count;
        let abortable = visitor.abortable;
        drop(visitor);
        let cancel_safe =
            await_count == 0 || self.async_block_is_transparent_cancel_safe_forwarder(block);
        (cancel_safe, abortable)
    }

    pub(super) fn async_closure_body_capabilities(&mut self, body: &TClosureBody) -> (bool, bool) {
        let mut visitor = AsyncSuspensionCapabilityVisitor {
            checker: self,
            await_count: 0,
            abortable: true,
        };
        visitor.visit_closure_body(body);
        let await_count = visitor.await_count;
        let abortable = visitor.abortable;
        drop(visitor);
        let cancel_safe =
            await_count == 0 || self.async_closure_body_is_transparent_cancel_safe_forwarder(body);
        (cancel_safe, abortable)
    }

    pub(super) fn async_block_is_transparent_cancel_safe_forwarder(
        &mut self,
        block: &TBlock,
    ) -> bool {
        let Some((prefix, future)) = self.transparent_cancel_safe_return_future(&block.statements)
        else {
            return false;
        };
        self.transparent_cancel_safe_future(prefix, future)
    }

    pub(super) fn async_closure_body_is_transparent_cancel_safe_forwarder(
        &mut self,
        body: &TClosureBody,
    ) -> bool {
        match body {
            TClosureBody::Block(block) => {
                self.async_block_is_transparent_cancel_safe_forwarder(block)
            }
            TClosureBody::Expr(expr) => self
                .transparent_cancel_safe_await_future(expr)
                .is_some_and(|future| self.is_cancel_safe_ty(&future.ty)),
        }
    }

    pub(super) fn transparent_cancel_safe_return_future<'a>(
        &self,
        statements: &'a [TStmt],
    ) -> Option<(&'a [TStmt], &'a TExpr)> {
        let (last, prefix) = statements.split_last()?;
        let TStmtKind::Return(Some(expr)) = &last.kind else {
            return None;
        };
        self.transparent_cancel_safe_await_future(expr)
            .map(|future| (prefix, future))
    }

    pub(super) fn transparent_cancel_safe_await_future<'a>(
        &self,
        expr: &'a TExpr,
    ) -> Option<&'a TExpr> {
        match &expr.kind {
            TExprKind::Await { future } => Some(future),
            TExprKind::Try { expr, .. } => self.transparent_cancel_safe_await_future(expr),
            TExprKind::Call { callee, args }
                if args.len() == 1
                    && matches!(
                        callee.kind,
                        TExprKind::Function(_, _) | TExprKind::GenericFunction { .. }
                    ) =>
            {
                self.transparent_cancel_safe_await_future(&args[0])
            }
            _ => None,
        }
    }

    pub(super) fn transparent_cancel_safe_future(
        &mut self,
        prefix: &[TStmt],
        future: &TExpr,
    ) -> bool {
        if !self.is_cancel_safe_ty(&future.ty) {
            return false;
        }
        if prefix.is_empty() {
            return true;
        }
        if prefix.len() != 1 {
            return false;
        }
        let TStmtKind::VarDecl {
            local_id,
            init: Some(_),
            ..
        } = &prefix[0].kind
        else {
            return false;
        };
        self.transparent_cancel_safe_future_consumes_local(future, *local_id)
    }

    fn transparent_cancel_safe_future_consumes_local(
        &self,
        future: &TExpr,
        local_id: LocalId,
    ) -> bool {
        match &future.kind {
            TExprKind::Local(id, _) => *id == local_id,
            TExprKind::Move(inner) => {
                self.transparent_cancel_safe_future_consumes_local(inner, local_id)
            }
            TExprKind::Call { args, .. } => args
                .iter()
                .any(|arg| self.transparent_cancel_safe_future_consumes_local(arg, local_id)),
            _ => false,
        }
    }

    pub(super) fn check_async_frame_safety(
        &mut self,
        block: &TBlock,
        params: &[(LocalId, String, Ty, BindingMutability)],
    ) {
        let mut infos = HashMap::<LocalId, AsyncLocalInfo>::new();
        for (local_id, name, ty, _) in params {
            infos.insert(
                *local_id,
                AsyncLocalInfo {
                    name: name.clone(),
                    ty: ty.clone(),
                    static_const_slice: false,
                },
            );
        }
        self.async_collect_local_infos_block(block, &mut infos);
        let live_after = HashSet::new();
        self.async_live_before_block(block, live_after, &infos);
        self.async_check_defer_arg_frame_safety_block(block);
    }

    pub(super) fn check_async_closure_frame_safety(
        &mut self,
        body: &TClosureBody,
        params: &[(LocalId, String, Ty)],
        captures: &[TClosureCapture],
    ) {
        let mut infos = HashMap::<LocalId, AsyncLocalInfo>::new();
        for (local_id, name, ty) in params {
            infos.insert(
                *local_id,
                AsyncLocalInfo {
                    name: name.clone(),
                    ty: ty.clone(),
                    static_const_slice: false,
                },
            );
        }
        for capture in captures {
            infos.insert(
                capture.local_id,
                AsyncLocalInfo {
                    name: capture.name.clone(),
                    ty: capture.ty.clone(),
                    static_const_slice: false,
                },
            );
        }
        match body {
            TClosureBody::Block(block) => {
                self.async_collect_local_infos_block(block, &mut infos);
                let live_after = HashSet::new();
                self.async_live_before_block(block, live_after, &infos);
                self.async_check_defer_arg_frame_safety_block(block);
            }
            TClosureBody::Expr(expr) => {
                let live_after = HashSet::new();
                self.async_validate_awaits_in_expr(expr, &live_after, &infos);
            }
        }
    }

    pub(super) fn async_facts_for_block(&mut self, block: &TBlock) -> AsyncFacts {
        let mut locals = Vec::<(LocalId, Ty)>::new();
        let mut seen = HashSet::<LocalId>::new();
        let mut live_across_await = HashSet::<LocalId>::new();
        self.collect_async_frame_locals_block(block, &mut locals, &mut seen);
        self.async_live_frame_locals_before_block(block, HashSet::new(), &mut live_across_await);
        let frame_locals = locals
            .into_iter()
            .filter(|(id, ty)| live_across_await.contains(id) || self.type_is_affine(ty))
            .map(|(id, ty)| AsyncFrameLocal {
                id,
                ty,
                field: format!("local{}", id.0),
                heap: false,
            })
            .collect();
        let mut await_output_tys = Vec::new();
        self.collect_async_await_output_tys_block(block, &mut await_output_tys);
        let mut defer_arg_tys = Vec::new();
        self.collect_async_defer_args_block(block, &mut defer_arg_tys);
        let defer_args = defer_arg_tys
            .into_iter()
            .enumerate()
            .map(|(idx, ty)| AsyncDeferArg {
                ty,
                field: format!("defer_arg{}", idx + 1),
            })
            .collect();
        AsyncFacts {
            frame_locals,
            live_across_await,
            await_output_tys,
            defer_args,
        }
    }

    pub(super) fn async_facts_for_closure_body(&mut self, body: &TClosureBody) -> AsyncFacts {
        match body {
            TClosureBody::Block(block) => self.async_facts_for_block(block),
            TClosureBody::Expr(expr) => {
                let mut locals = Vec::<(LocalId, Ty)>::new();
                let mut seen = HashSet::<LocalId>::new();
                let mut live_across_await = HashSet::<LocalId>::new();
                self.collect_async_frame_locals_expr(expr, &mut locals, &mut seen);
                self.async_collect_live_awaits_in_expr(
                    expr,
                    &HashSet::new(),
                    &mut live_across_await,
                );
                let frame_locals = locals
                    .into_iter()
                    .filter(|(id, ty)| live_across_await.contains(id) || self.type_is_affine(ty))
                    .map(|(id, ty)| AsyncFrameLocal {
                        id,
                        ty,
                        field: format!("local{}", id.0),
                        heap: false,
                    })
                    .collect();
                let mut await_output_tys = Vec::new();
                self.collect_async_await_output_tys_expr(expr, &mut await_output_tys);
                let mut defer_arg_tys = Vec::new();
                self.collect_async_defer_args_expr(expr, &mut defer_arg_tys);
                let defer_args = defer_arg_tys
                    .into_iter()
                    .enumerate()
                    .map(|(idx, ty)| AsyncDeferArg {
                        ty,
                        field: format!("defer_arg{}", idx + 1),
                    })
                    .collect();
                AsyncFacts {
                    frame_locals,
                    live_across_await,
                    await_output_tys,
                    defer_args,
                }
            }
        }
    }

    pub(super) fn collect_async_frame_locals_block(
        &self,
        block: &TBlock,
        out: &mut Vec<(LocalId, Ty)>,
        seen: &mut HashSet<LocalId>,
    ) {
        let mut visitor = AsyncFrameLocalCollector { out, seen };
        visitor.visit_block(block);
    }

    pub(super) fn collect_async_frame_locals_expr(
        &self,
        expr: &TExpr,
        out: &mut Vec<(LocalId, Ty)>,
        seen: &mut HashSet<LocalId>,
    ) {
        let mut visitor = AsyncFrameLocalCollector { out, seen };
        visitor.visit_expr(expr);
    }

    pub(super) fn async_live_frame_locals_before_block(
        &self,
        block: &TBlock,
        mut live: HashSet<LocalId>,
        out: &mut HashSet<LocalId>,
    ) -> HashSet<LocalId> {
        for stmt in block.statements.iter().rev() {
            live = self.async_live_frame_locals_before_stmt(stmt, live, out);
        }
        live
    }

    pub(super) fn async_live_frame_locals_before_stmt(
        &self,
        stmt: &TStmt,
        live_after: HashSet<LocalId>,
        out: &mut HashSet<LocalId>,
    ) -> HashSet<LocalId> {
        match &stmt.kind {
            TStmtKind::Block(block) => {
                self.async_live_frame_locals_before_block(block, live_after, out)
            }
            TStmtKind::VarDecl { local_id, init, .. } => {
                let mut live = live_after;
                live.remove(local_id);
                if let Some(init) = init {
                    self.async_collect_live_awaits_in_expr(init, &live, out);
                    live.extend(Self::async_expr_used_locals(init));
                }
                live
            }
            TStmtKind::Assign { target, value } => {
                let mut live = live_after;
                self.async_collect_live_awaits_in_expr(value, &live, out);
                self.async_collect_live_awaits_in_expr(target, &live, out);
                live.extend(Self::async_expr_used_locals(value));
                live.extend(Self::async_expr_used_locals(target));
                live
            }
            TStmtKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                let then_live =
                    self.async_live_frame_locals_before_block(then_block, live_after.clone(), out);
                let else_live = else_branch
                    .as_ref()
                    .map(|stmt| {
                        self.async_live_frame_locals_before_stmt(stmt, live_after.clone(), out)
                    })
                    .unwrap_or_else(|| live_after.clone());
                let mut live = then_live;
                live.extend(else_live);
                self.async_collect_live_awaits_in_expr(cond, &live, out);
                live.extend(Self::async_expr_used_locals(cond));
                live
            }
            TStmtKind::While { cond, body } => {
                let mut loop_live = live_after.clone();
                loop_live.extend(Self::async_expr_used_locals(cond));
                for _ in 0..2 {
                    let body_live =
                        self.async_live_frame_locals_before_block(body, loop_live.clone(), out);
                    let old_len = loop_live.len();
                    loop_live.extend(body_live);
                    loop_live.extend(Self::async_expr_used_locals(cond));
                    if loop_live.len() == old_len {
                        break;
                    }
                }
                self.async_collect_live_awaits_in_expr(cond, &loop_live, out);
                loop_live
            }
            TStmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                let mut live = live_after;
                if let Some(step) = step {
                    live = self.async_live_frame_locals_before_for_init(step, live, out);
                }
                if let Some(cond) = cond {
                    self.async_collect_live_awaits_in_expr(cond, &live, out);
                    live.extend(Self::async_expr_used_locals(cond));
                }
                live = self.async_live_frame_locals_before_block(body, live, out);
                if let Some(init) = init {
                    live = self.async_live_frame_locals_before_for_init(init, live, out);
                }
                live
            }
            TStmtKind::Switch {
                expr,
                cases,
                default,
                ..
            } => {
                let mut live = HashSet::new();
                for case in cases {
                    let mut case_live = live_after.clone();
                    for stmt in case.statements.iter().rev() {
                        case_live = self.async_live_frame_locals_before_stmt(stmt, case_live, out);
                    }
                    let mut bindings = Vec::new();
                    case.pattern.collect_bindings(&mut bindings);
                    for (local_id, _, _, _) in bindings {
                        case_live.remove(local_id);
                    }
                    live.extend(case_live);
                }
                let mut default_live = live_after;
                for stmt in default.iter().rev() {
                    default_live =
                        self.async_live_frame_locals_before_stmt(stmt, default_live, out);
                }
                live.extend(default_live);
                self.async_collect_live_awaits_in_expr(expr, &live, out);
                live.extend(Self::async_expr_used_locals(expr));
                live
            }
            TStmtKind::Defer(expr) | TStmtKind::ResourceCleanup(expr) | TStmtKind::Expr(expr) => {
                let mut live = live_after;
                self.async_collect_live_awaits_in_expr(expr, &live, out);
                live.extend(Self::async_expr_used_locals(expr));
                live
            }
            TStmtKind::Return(Some(expr)) => {
                let live = HashSet::new();
                self.async_collect_live_awaits_in_expr(expr, &live, out);
                Self::async_expr_used_locals(expr)
            }
            TStmtKind::Return(None)
            | TStmtKind::Break
            | TStmtKind::Continue
            | TStmtKind::Unsupported => HashSet::new(),
        }
    }

    pub(super) fn async_live_frame_locals_before_for_init(
        &self,
        init: &TForInit,
        live_after: HashSet<LocalId>,
        out: &mut HashSet<LocalId>,
    ) -> HashSet<LocalId> {
        match init {
            TForInit::VarDecl { local_id, init, .. } => {
                let mut live = live_after;
                live.remove(local_id);
                if let Some(init) = init {
                    self.async_collect_live_awaits_in_expr(init, &live, out);
                    live.extend(Self::async_expr_used_locals(init));
                }
                live
            }
            TForInit::Assign { target, value } => {
                let mut live = live_after;
                self.async_collect_live_awaits_in_expr(value, &live, out);
                self.async_collect_live_awaits_in_expr(target, &live, out);
                live.extend(Self::async_expr_used_locals(value));
                live.extend(Self::async_expr_used_locals(target));
                live
            }
            TForInit::Expr(expr) => {
                let mut live = live_after;
                self.async_collect_live_awaits_in_expr(expr, &live, out);
                live.extend(Self::async_expr_used_locals(expr));
                live
            }
        }
    }

    pub(super) fn async_collect_live_awaits_in_expr(
        &self,
        expr: &TExpr,
        live_after: &HashSet<LocalId>,
        out: &mut HashSet<LocalId>,
    ) {
        let mut live_after = live_after.clone();
        live_after.extend(Self::async_expr_used_locals(expr));
        self.async_collect_live_awaits_in_expr_inner(expr, &live_after, out);
    }

    pub(super) fn async_collect_live_awaits_in_expr_inner(
        &self,
        expr: &TExpr,
        live_after: &HashSet<LocalId>,
        out: &mut HashSet<LocalId>,
    ) {
        match &expr.kind {
            TExprKind::Await { future } => {
                let mut live = live_after.clone();
                live.extend(Self::async_expr_used_locals(future));
                out.extend(live);
                self.async_collect_live_awaits_in_expr_inner(future, live_after, out);
            }
            TExprKind::AsyncSelect { arms, .. } => {
                let mut live = live_after.clone();
                for arm in arms {
                    live.extend(Self::async_expr_used_locals(&arm.future));
                    let mut body_live = Self::async_expr_used_locals(&arm.body);
                    body_live.remove(&arm.binding_local);
                    live.extend(body_live);
                }
                out.extend(live);
                for arm in arms {
                    self.async_collect_live_awaits_in_expr_inner(&arm.future, live_after, out);
                    self.async_collect_live_awaits_in_expr_inner(&arm.body, live_after, out);
                }
            }
            TExprKind::Closure { .. } => {}
            TExprKind::UnsafeBlock { statements, value } => {
                let mut live = live_after.clone();
                if let Some(value) = value {
                    self.async_collect_live_awaits_in_expr_inner(value, &live, out);
                    live.extend(Self::async_expr_used_locals(value));
                }
                for stmt in statements.iter().rev() {
                    live = self.async_live_frame_locals_before_stmt(stmt, live, out);
                }
            }
            _ => walk_expr_children(expr, &mut |child| {
                self.async_collect_live_awaits_in_expr_inner(child, live_after, out);
            }),
        }
    }

    pub(super) fn collect_async_defer_args_block(&self, block: &TBlock, out: &mut Vec<Ty>) {
        let mut visitor = AsyncDeferArgCollector { out };
        visitor.visit_block(block);
    }

    pub(super) fn collect_async_defer_args_expr(&self, expr: &TExpr, out: &mut Vec<Ty>) {
        let mut visitor = AsyncDeferArgCollector { out };
        visitor.visit_expr(expr);
    }

    pub(super) fn collect_async_await_output_tys_block(&self, block: &TBlock, out: &mut Vec<Ty>) {
        let mut visitor = AsyncAwaitOutputCollector { out };
        visitor.visit_block(block);
    }

    pub(super) fn collect_async_await_output_tys_expr(&self, expr: &TExpr, out: &mut Vec<Ty>) {
        let mut visitor = AsyncAwaitOutputCollector { out };
        visitor.visit_expr(expr);
    }

    pub(super) fn async_check_defer_arg_frame_safety_block(&mut self, block: &TBlock) {
        let mut visitor = AsyncDeferArgFrameSafetyWalker { checker: self };
        visitor.visit_block(block);
    }

    pub(super) fn async_collect_local_infos_block(
        &self,
        block: &TBlock,
        infos: &mut HashMap<LocalId, AsyncLocalInfo>,
    ) {
        let mut visitor = AsyncLocalInfoWalker {
            checker: self,
            infos,
        };
        visitor.visit_block(block);
    }

    pub(super) fn async_collect_pattern_infos(
        &self,
        pattern: &TPattern,
        infos: &mut HashMap<LocalId, AsyncLocalInfo>,
    ) {
        let mut bindings = Vec::new();
        pattern.collect_bindings(&mut bindings);
        for (local_id, name, _, ty) in bindings {
            infos.insert(
                *local_id,
                AsyncLocalInfo {
                    name: name.clone(),
                    ty: ty.clone(),
                    static_const_slice: false,
                },
            );
        }
    }

    pub(super) fn async_live_before_block(
        &mut self,
        block: &TBlock,
        mut live: HashSet<LocalId>,
        infos: &HashMap<LocalId, AsyncLocalInfo>,
    ) -> HashSet<LocalId> {
        for stmt in block.statements.iter().rev() {
            live = self.async_live_before_stmt(stmt, live, infos);
        }
        live
    }

    pub(super) fn async_live_before_stmt(
        &mut self,
        stmt: &TStmt,
        live_after: HashSet<LocalId>,
        infos: &HashMap<LocalId, AsyncLocalInfo>,
    ) -> HashSet<LocalId> {
        match &stmt.kind {
            TStmtKind::Block(block) => self.async_live_before_block(block, live_after, infos),
            TStmtKind::VarDecl { local_id, init, .. } => {
                let mut live = live_after;
                live.remove(local_id);
                if let Some(init) = init {
                    self.async_validate_awaits_in_expr(init, &live, infos);
                    live.extend(Self::async_expr_used_locals(init));
                }
                live
            }
            TStmtKind::Assign { target, value } => {
                let mut live = live_after;
                self.async_validate_awaits_in_expr(value, &live, infos);
                self.async_validate_awaits_in_expr(target, &live, infos);
                live.extend(Self::async_expr_used_locals(value));
                live.extend(Self::async_expr_used_locals(target));
                live
            }
            TStmtKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                let then_live = self.async_live_before_block(then_block, live_after.clone(), infos);
                let else_live = else_branch
                    .as_ref()
                    .map(|stmt| self.async_live_before_stmt(stmt, live_after.clone(), infos))
                    .unwrap_or_else(|| live_after.clone());
                let mut live = then_live;
                live.extend(else_live);
                self.async_validate_awaits_in_expr(cond, &live, infos);
                live.extend(Self::async_expr_used_locals(cond));
                live
            }
            TStmtKind::While { cond, body } => {
                let mut loop_live = live_after.clone();
                loop_live.extend(Self::async_expr_used_locals(cond));
                for _ in 0..2 {
                    let body_live = self.async_live_before_block(body, loop_live.clone(), infos);
                    let old_len = loop_live.len();
                    loop_live.extend(body_live);
                    loop_live.extend(Self::async_expr_used_locals(cond));
                    if loop_live.len() == old_len {
                        break;
                    }
                }
                self.async_validate_awaits_in_expr(cond, &loop_live, infos);
                loop_live
            }
            TStmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                let mut live = live_after;
                if let Some(step) = step {
                    live = self.async_live_before_for_init(step, live, infos);
                }
                if let Some(cond) = cond {
                    self.async_validate_awaits_in_expr(cond, &live, infos);
                    live.extend(Self::async_expr_used_locals(cond));
                }
                live = self.async_live_before_block(body, live, infos);
                if let Some(init) = init {
                    live = self.async_live_before_for_init(init, live, infos);
                }
                live
            }
            TStmtKind::Switch {
                expr,
                cases,
                default,
                ..
            } => {
                let mut live = HashSet::new();
                for case in cases {
                    let mut case_live = live_after.clone();
                    for stmt in case.statements.iter().rev() {
                        case_live = self.async_live_before_stmt(stmt, case_live, infos);
                    }
                    let mut bindings = Vec::new();
                    case.pattern.collect_bindings(&mut bindings);
                    for (local_id, _, _, _) in bindings {
                        case_live.remove(local_id);
                    }
                    live.extend(case_live);
                }
                let mut default_live = live_after;
                for stmt in default.iter().rev() {
                    default_live = self.async_live_before_stmt(stmt, default_live, infos);
                }
                live.extend(default_live);
                self.async_validate_awaits_in_expr(expr, &live, infos);
                live.extend(Self::async_expr_used_locals(expr));
                live
            }
            TStmtKind::Defer(expr) | TStmtKind::ResourceCleanup(expr) | TStmtKind::Expr(expr) => {
                let mut live = live_after;
                self.async_validate_awaits_in_expr(expr, &live, infos);
                live.extend(Self::async_expr_used_locals(expr));
                live
            }
            TStmtKind::Return(Some(expr)) => {
                let live = HashSet::new();
                self.async_validate_awaits_in_expr(expr, &live, infos);
                Self::async_expr_used_locals(expr)
            }
            TStmtKind::Return(None)
            | TStmtKind::Break
            | TStmtKind::Continue
            | TStmtKind::Unsupported => HashSet::new(),
        }
    }

    pub(super) fn async_live_before_for_init(
        &mut self,
        init: &TForInit,
        live_after: HashSet<LocalId>,
        infos: &HashMap<LocalId, AsyncLocalInfo>,
    ) -> HashSet<LocalId> {
        match init {
            TForInit::VarDecl { local_id, init, .. } => {
                let mut live = live_after;
                live.remove(local_id);
                if let Some(init) = init {
                    self.async_validate_awaits_in_expr(init, &live, infos);
                    live.extend(Self::async_expr_used_locals(init));
                }
                live
            }
            TForInit::Assign { target, value } => {
                let mut live = live_after;
                self.async_validate_awaits_in_expr(value, &live, infos);
                self.async_validate_awaits_in_expr(target, &live, infos);
                live.extend(Self::async_expr_used_locals(value));
                live.extend(Self::async_expr_used_locals(target));
                live
            }
            TForInit::Expr(expr) => {
                let mut live = live_after;
                self.async_validate_awaits_in_expr(expr, &live, infos);
                live.extend(Self::async_expr_used_locals(expr));
                live
            }
        }
    }

    pub(super) fn async_validate_awaits_in_expr(
        &mut self,
        expr: &TExpr,
        live_after: &HashSet<LocalId>,
        infos: &HashMap<LocalId, AsyncLocalInfo>,
    ) {
        let mut live_after = live_after.clone();
        live_after.extend(Self::async_expr_used_locals(expr));
        let mut validator = AsyncAwaitValidator {
            checker: self,
            infos,
            live_after: &live_after,
        };
        validator.visit_expr(expr);
    }

    pub(super) fn async_check_defer_arg_frame_safety_arg(&mut self, arg: &TExpr) {
        if arg.ty.is_erased_value() {
            return;
        }
        let static_const_slice = self.async_is_static_const_slice_init(&arg.ty, Some(arg));
        let mut visiting = HashSet::new();
        if let Some(reason) = self.async_frame_safety_violation(
            &arg.ty,
            static_const_slice,
            "`defer` argument",
            &mut visiting,
        ) {
            self.diagnostics.push(self.diagnostic_with_reason_note(
                arg.span,
                "`defer` argument is not async-frame-safe",
                reason,
            ));
        }
    }

    pub(super) fn async_check_live_locals_at_await(
        &mut self,
        span: crate::span::Span,
        live: &HashSet<LocalId>,
        infos: &HashMap<LocalId, AsyncLocalInfo>,
    ) {
        let mut checked = HashSet::<LocalId>::new();
        for local_id in live {
            if !checked.insert(*local_id) {
                continue;
            }
            let Some(info) = infos.get(local_id) else {
                continue;
            };
            let mut visiting = HashSet::new();
            if let Some(reason) = self.async_frame_safety_violation(
                &info.ty,
                info.static_const_slice,
                &format!("local `{}`", info.name),
                &mut visiting,
            ) {
                self.diagnostics.push(self.diagnostic_with_reason_note(
                    span,
                    format!("`{}` is not async-frame-safe across `await`", info.name),
                    reason,
                ));
            }
        }
    }

    pub(super) fn async_frame_safety_violation(
        &mut self,
        ty: &Ty,
        static_const_slice: bool,
        path: &str,
        visiting: &mut HashSet<Ty>,
    ) -> Option<String> {
        if matches!(
            ty,
            Ty::Unknown
                | Ty::Hole(_)
                | Ty::Never
                | Ty::Void
                | Ty::Bool
                | Ty::Char
                | Ty::I8
                | Ty::I16
                | Ty::I32
                | Ty::I64
                | Ty::U8
                | Ty::U16
                | Ty::U32
                | Ty::U64
                | Ty::Usize
                | Ty::F32
                | Ty::F64
                | Ty::CSpelling { .. }
        ) {
            return None;
        }
        if matches!(ty, Ty::OpaqueReturn { .. }) {
            let concrete = self.lower_opaque_returns_in_ty(ty);
            if &concrete != ty {
                return self.async_frame_safety_violation(
                    &concrete,
                    static_const_slice,
                    path,
                    visiting,
                );
            }
        }
        if std_id::is_std_async_runtime_handle_ty(&self.ctx.resolved, ty) {
            return None;
        }
        if contains_generic(ty) || contains_type_hole(ty) {
            return Some(format!(
                "{path} has generic type `{ty}` without a proven async-frame-safety policy"
            ));
        }
        if self.type_implements_thread_local(ty) {
            return Some(format!("{path} has ThreadLocal type `{ty}`"));
        }
        if self.type_implements_async_frame_opt_in(ty) {
            return None;
        }
        match ty {
            Ty::Pointer { nullable, .. } => {
                if *nullable {
                    Some(format!("{path} has nullable raw pointer type `{ty}`"))
                } else {
                    Some(format!("{path} has raw pointer type `{ty}`"))
                }
            }
            Ty::Slice { mutability, elem } => {
                if *mutability == ViewMutability::Writable {
                    return Some(format!("{path} has mutable slice type `{ty}`"));
                }
                if static_const_slice && matches!(&**elem, Ty::Char | Ty::U8) {
                    None
                } else {
                    Some(format!(
                        "{path} has non-static borrowed read-only slice type `{ty}`"
                    ))
                }
            }
            Ty::Array { elem, .. } => {
                self.async_frame_safety_violation(elem, false, &format!("{path} element"), visiting)
            }
            Ty::GeneratedFuture { .. } => None,
            Ty::OpaqueState { base, state } => {
                if let Some(reason) = self.async_frame_safety_violation(base, false, path, visiting)
                {
                    return Some(reason);
                }
                for (name, state_ty) in state {
                    let state_path = if name.is_empty() {
                        format!("{path} state")
                    } else {
                        format!("{path} state `{name}`")
                    };
                    if let Some(reason) =
                        self.async_frame_safety_violation(state_ty, false, &state_path, visiting)
                    {
                        return Some(reason);
                    }
                }
                None
            }
            Ty::Named { name, args } => {
                if std_id::is_std_async_runtime_handle_ty(&self.ctx.resolved, ty) {
                    return None;
                }
                if !visiting.insert(ty.clone()) {
                    return None;
                }
                let instance_name = aggregate_instance_name(name, args);
                if let Some(fields) = self.ctx.structs.get(&instance_name).cloned() {
                    for (field, field_ty) in fields {
                        if let Some(reason) = self.async_frame_safety_violation(
                            &field_ty,
                            false,
                            &format!("{path}.{field}"),
                            visiting,
                        ) {
                            visiting.remove(ty);
                            return Some(reason);
                        }
                    }
                    visiting.remove(ty);
                    return None;
                }
                self.ensure_enum_instance(ty);
                if let Some(enm) = self.ctx.checked_enums.get(&instance_name).cloned() {
                    for variant in enm.variants {
                        for (idx, payload_ty) in variant.payload.iter().enumerate() {
                            if let Some(reason) = self.async_frame_safety_violation(
                                payload_ty,
                                false,
                                &format!("{path}.{}[{idx}]", variant.name),
                                visiting,
                            ) {
                                visiting.remove(ty);
                                return Some(reason);
                            }
                        }
                    }
                }
                visiting.remove(ty);
                None
            }
            Ty::ClosureInstance { captures, .. } => {
                for (idx, capture_ty) in captures.iter().enumerate() {
                    if let Some(reason) = self.async_frame_safety_violation(
                        capture_ty,
                        false,
                        &format!("{path} closure capture {idx}"),
                        visiting,
                    ) {
                        return Some(reason);
                    }
                }
                None
            }
            Ty::Closure { .. } => Some(format!("{path} has erased closure type `{ty}`")),
            Ty::DynamicInterface { .. } => {
                Some(format!("{path} has dynamic interface type `{ty}`"))
            }
            Ty::OpaqueReturn { .. } => Some(format!(
                "{path} has opaque return type `{ty}` without an async-frame-safety policy"
            )),
            Ty::Function { .. } => Some(format!("{path} has function pointer type `{ty}`")),
            Ty::Generic(_) => Some(format!(
                "{path} has generic type `{ty}` without a proven async-frame-safety policy"
            )),
            Ty::Hole(_)
            | Ty::Never
            | Ty::Void
            | Ty::Bool
            | Ty::Char
            | Ty::I8
            | Ty::I16
            | Ty::I32
            | Ty::I64
            | Ty::U8
            | Ty::U16
            | Ty::U32
            | Ty::U64
            | Ty::Usize
            | Ty::F32
            | Ty::F64
            | Ty::CSpelling { .. }
            | Ty::Unknown => None,
        }
    }

    pub(super) fn async_is_static_const_slice_init(&self, ty: &Ty, init: Option<&TExpr>) -> bool {
        matches!(
            ty,
            Ty::Slice {
                mutability: ViewMutability::ReadOnly,
                elem
            } if matches!(&**elem, Ty::Char | Ty::U8)
        ) && init.is_some_and(|expr| matches!(expr.kind, TExprKind::Literal(Literal::String(_))))
    }

    pub(super) fn async_expr_used_locals(expr: &TExpr) -> HashSet<LocalId> {
        let mut collector = AsyncLocalUseCollector {
            locals: HashSet::new(),
        };
        collector.visit_expr(expr);
        collector.locals
    }
}

fn grouped_cases_by_variant<'a>(cases: &'a [TCase]) -> BTreeMap<usize, Vec<&'a TCase>> {
    let mut grouped = BTreeMap::<usize, Vec<&TCase>>::new();
    for case in cases {
        grouped.entry(case.variant_index).or_default().push(case);
    }
    grouped
}

struct AsyncFrameLocalCollector<'a> {
    out: &'a mut Vec<(LocalId, Ty)>,
    seen: &'a mut HashSet<LocalId>,
}

impl AsyncFrameLocalCollector<'_> {
    fn record(&mut self, local_id: LocalId, ty: &Ty) {
        if !ty.is_erased_value() && self.seen.insert(local_id) {
            self.out.push((local_id, ty.clone()));
        }
    }
}

impl ThirVisitor for AsyncFrameLocalCollector<'_> {
    fn visit_stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::VarDecl {
                ty, local_id, init, ..
            } => {
                self.record(*local_id, ty);
                if let Some(init) = init {
                    self.visit_expr(init);
                }
            }
            TStmtKind::Switch {
                expr,
                cases,
                default,
                ..
            } => {
                self.visit_expr(expr);
                for case in cases {
                    self.visit_pattern(&case.pattern);
                    for stmt in &case.statements {
                        self.visit_stmt(stmt);
                    }
                }
                for stmt in default {
                    self.visit_stmt(stmt);
                }
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_for_init(&mut self, init: &TForInit) {
        match init {
            TForInit::VarDecl {
                ty, local_id, init, ..
            } => {
                self.record(*local_id, ty);
                if let Some(init) = init {
                    self.visit_expr(init);
                }
            }
            _ => walk_for_init(self, init),
        }
    }

    fn visit_pattern(&mut self, pattern: &TPattern) {
        let mut bindings = Vec::new();
        pattern.collect_bindings(&mut bindings);
        for (local_id, _, _, ty) in bindings {
            self.record(*local_id, ty);
        }
        walk_pattern(self, pattern);
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        if matches!(expr.kind, TExprKind::Closure { .. }) {
            return;
        }
        walk_expr(self, expr);
    }
}

struct AsyncDeferArgCollector<'a> {
    out: &'a mut Vec<Ty>,
}

impl ThirVisitor for AsyncDeferArgCollector<'_> {
    fn visit_stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::Switch {
                expr,
                cases,
                default,
                ..
            } => {
                self.visit_expr(expr);
                for cases in grouped_cases_by_variant(cases).into_values() {
                    for case in cases {
                        for stmt in &case.statements {
                            self.visit_stmt(stmt);
                        }
                    }
                }
                for stmt in default {
                    self.visit_stmt(stmt);
                }
            }
            TStmtKind::Defer(expr) => {
                if let TExprKind::Call { callee, args, .. } = &expr.kind {
                    self.visit_expr(callee);
                    for arg in args {
                        self.visit_expr(arg);
                        if !arg.ty.is_erased_value() {
                            self.out.push(arg.ty.clone());
                        }
                    }
                } else {
                    self.visit_expr(expr);
                }
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        if matches!(expr.kind, TExprKind::Closure { .. }) {
            return;
        }
        walk_expr(self, expr);
    }
}

struct AsyncAwaitOutputCollector<'a> {
    out: &'a mut Vec<Ty>,
}

impl ThirVisitor for AsyncAwaitOutputCollector<'_> {
    fn visit_stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(init) = init {
                    self.visit_for_init(init);
                }
                if let Some(cond) = cond {
                    self.visit_expr(cond);
                }
                self.visit_block(body);
                if let Some(step) = step {
                    self.visit_for_init(step);
                }
            }
            TStmtKind::Switch {
                expr,
                cases,
                default,
                ..
            } => {
                self.visit_expr(expr);
                for cases in grouped_cases_by_variant(cases).into_values() {
                    for case in cases {
                        for stmt in &case.statements {
                            self.visit_stmt(stmt);
                        }
                    }
                }
                for stmt in default {
                    self.visit_stmt(stmt);
                }
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        match &expr.kind {
            TExprKind::Await { future } => {
                self.visit_expr(future);
                self.out.push(expr.ty.clone());
            }
            TExprKind::AsyncSelect { arms, .. } => {
                for arm in arms {
                    self.visit_expr(&arm.future);
                    self.visit_expr(&arm.body);
                }
                self.out.push(Ty::Void);
            }
            TExprKind::Closure { .. } => {}
            _ => walk_expr(self, expr),
        }
    }
}

struct AsyncLocalInfoWalker<'a, 'b> {
    checker: &'a TypeChecker,
    infos: &'b mut HashMap<LocalId, AsyncLocalInfo>,
}

impl AsyncLocalInfoWalker<'_, '_> {
    fn record(&mut self, local_id: LocalId, name: &str, ty: &Ty, init: Option<&TExpr>) {
        self.infos.insert(
            local_id,
            AsyncLocalInfo {
                name: name.to_string(),
                ty: ty.clone(),
                static_const_slice: self.checker.async_is_static_const_slice_init(ty, init),
            },
        );
    }
}

impl ThirVisitor for AsyncLocalInfoWalker<'_, '_> {
    fn visit_stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::VarDecl {
                ty,
                name,
                local_id,
                init,
            } => {
                self.record(*local_id, name, ty, init.as_ref());
                if let Some(init) = init {
                    self.visit_expr(init);
                }
            }
            TStmtKind::Switch {
                expr,
                cases,
                default,
                ..
            } => {
                self.visit_expr(expr);
                for case in cases {
                    self.checker
                        .async_collect_pattern_infos(&case.pattern, self.infos);
                    for stmt in &case.statements {
                        self.visit_stmt(stmt);
                    }
                }
                for stmt in default {
                    self.visit_stmt(stmt);
                }
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_for_init(&mut self, init: &TForInit) {
        match init {
            TForInit::VarDecl {
                ty,
                name,
                local_id,
                init,
            } => {
                self.record(*local_id, name, ty, init.as_ref());
                if let Some(init) = init {
                    self.visit_expr(init);
                }
            }
            _ => walk_for_init(self, init),
        }
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        if matches!(expr.kind, TExprKind::Closure { .. }) {
            return;
        }
        walk_expr(self, expr);
    }
}

struct AsyncDeferArgFrameSafetyWalker<'a> {
    checker: &'a mut TypeChecker,
}

impl ThirVisitor for AsyncDeferArgFrameSafetyWalker<'_> {
    fn visit_stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::Defer(expr) => {
                if let TExprKind::Call { args, .. } = &expr.kind {
                    for arg in args {
                        self.checker.async_check_defer_arg_frame_safety_arg(arg);
                    }
                }
                self.visit_expr(expr);
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        if matches!(expr.kind, TExprKind::Closure { .. }) {
            return;
        }
        walk_expr(self, expr);
    }
}
