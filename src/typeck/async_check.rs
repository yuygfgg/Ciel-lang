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
        let liveness = self.async_liveness_for_block(block);
        self.async_check_liveness_frame_safety(&liveness, &infos);
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
                let liveness = self.async_liveness_for_block(block);
                self.async_check_liveness_frame_safety(&liveness, &infos);
                self.async_check_defer_arg_frame_safety_block(block);
            }
            TClosureBody::Expr(expr) => {
                let liveness = self.async_liveness_for_expr(expr);
                self.async_check_liveness_frame_safety(&liveness, &infos);
            }
        }
    }

    pub(super) fn async_facts_for_block(&mut self, block: &TBlock) -> AsyncFacts {
        let mut locals = Vec::<(LocalId, Ty)>::new();
        let mut seen = HashSet::<LocalId>::new();
        let liveness = self.async_liveness_for_block(block);
        let live_across_await = liveness.live_across_await;
        self.collect_async_frame_locals_block(block, &mut locals, &mut seen);
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
                let liveness = self.async_liveness_for_expr(expr);
                let live_across_await = liveness.live_across_await;
                self.collect_async_frame_locals_expr(expr, &mut locals, &mut seen);
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

    fn async_liveness_for_block(&self, block: &TBlock) -> AsyncLiveness {
        AsyncLivenessAnalyzer::new().analyze_block(block)
    }

    fn async_liveness_for_expr(&self, expr: &TExpr) -> AsyncLiveness {
        AsyncLivenessAnalyzer::new().analyze_expr(expr)
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

    fn async_check_liveness_frame_safety(
        &mut self,
        liveness: &AsyncLiveness,
        infos: &HashMap<LocalId, AsyncLocalInfo>,
    ) {
        for site in &liveness.await_sites {
            self.async_check_live_locals_at_await(site.span, &site.live, infos);
        }
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
        if let Some(source_ty) = meta_schema_marker_source(ty) {
            if contains_type_hole(source_ty) {
                return Some(format!(
                    "{path} has generic type `{ty}` without a proven async-frame-safety policy"
                ));
            }
            return None;
        }
        if let Some((borrowed, source_ty)) = meta_repr_marker_source(ty) {
            if borrowed {
                return Some(format!(
                    "{path} has borrowed structural meta representation type `{ty}`"
                ));
            }
            if contains_generic(source_ty) || contains_type_hole(source_ty) {
                return Some(format!(
                    "{path} has generic type `{ty}` without a proven async-frame-safety policy"
                ));
            }
            let Some(sop_ty) = self.meta_repr_marker_sop_ty(ty) else {
                return Some(format!(
                    "{path} has owned structural meta representation type `{ty}` without a proven async-frame-safety policy"
                ));
            };
            return self.async_frame_safety_violation(&sop_ty, false, path, visiting);
        }
        if contains_generic(ty) || contains_type_hole(ty) {
            return Some(format!(
                "{path} has generic type `{ty}` without a proven async-frame-safety policy"
            ));
        }
        if self.type_implements_thread_local(ty) {
            return Some(format!("{path} has ThreadLocal type `{ty}`"));
        }
        match ty {
            Ty::GeneratedFuture { state, .. } => {
                if !visiting.insert(ty.clone()) {
                    return None;
                }
                for (name, state_ty) in state {
                    let state_path = if name.is_empty() {
                        format!("{path} state")
                    } else {
                        format!("{path} state `{name}`")
                    };
                    if let Some(reason) = self.async_future_state_frame_safety_violation(
                        state_ty,
                        &state_path,
                        visiting,
                    ) {
                        visiting.remove(ty);
                        return Some(reason);
                    }
                }
                visiting.remove(ty);
                None
            }
            Ty::OpaqueState { base, state } => {
                if !visiting.insert(ty.clone()) {
                    return None;
                }
                if let Some(reason) = self.async_frame_safety_violation(base, false, path, visiting)
                {
                    visiting.remove(ty);
                    return Some(reason);
                }
                for (name, state_ty) in state {
                    let state_path = if name.is_empty() {
                        format!("{path} state")
                    } else {
                        format!("{path} state `{name}`")
                    };
                    if let Some(reason) = self.async_future_state_frame_safety_violation(
                        state_ty,
                        &state_path,
                        visiting,
                    ) {
                        visiting.remove(ty);
                        return Some(reason);
                    }
                }
                visiting.remove(ty);
                None
            }
            _ if self.type_implements_async_frame_opt_in(ty) => None,
            _ if self.type_is_nominal_resource_frame_boundary(ty) && !self.is_abortable_ty(ty) => {
                Some(format!(
                    "{path} has resource-affine type `{ty}` without an async-frame-safety policy"
                ))
            }
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
            Ty::Named { name, args, .. } => {
                if self.is_canonical_std_meta_sop_node_ty(ty) {
                    return self.async_meta_sop_frame_safety_violation(ty, path, visiting);
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

    fn type_is_nominal_resource_frame_boundary(&mut self, ty: &Ty) -> bool {
        if std_id::is_std_resource_handle_ty(&self.ctx.resolved, ty) {
            return true;
        }
        let Ty::Named { name, args, .. } = ty else {
            return false;
        };
        let instance_name = aggregate_instance_name(name, args);
        if self.ctx.resource_structs.contains(&instance_name) {
            return true;
        }
        self.ensure_struct_instance(ty);
        self.ctx.resource_structs.contains(&instance_name)
    }

    fn is_canonical_std_meta_sop_node_ty(&self, ty: &Ty) -> bool {
        let Ty::Named { def_id, name, .. } = ty else {
            return false;
        };
        self.is_std_meta_frame_sop_node_name(name)
            && def_id.is_some_and(|id| {
                std_id::std_meta_named_def_id(&self.ctx.resolved, name) == Some(id)
            })
    }

    fn is_std_meta_frame_sop_node_name(&self, name: &str) -> bool {
        matches!(
            name,
            "HNil"
                | "HCons"
                | "FieldRef"
                | "Field"
                | "FieldSchema"
                | "PayloadRef"
                | "Payload"
                | "PayloadSchema"
                | "CoNil"
                | "Coproduct"
                | "VariantRef"
                | "Variant"
                | "VariantSchema"
                | "ArrayNil"
                | "ElementSchema"
                | "ArrayCat"
        ) || name
            .strip_prefix("ArrayChunk")
            .and_then(|suffix| suffix.parse::<usize>().ok())
            .is_some_and(|len| (1..=crate::types::META_ARRAY_CHUNK_SIZE).contains(&len))
    }

    fn async_meta_sop_frame_safety_violation(
        &mut self,
        ty: &Ty,
        path: &str,
        visiting: &mut HashSet<Ty>,
    ) -> Option<String> {
        if !visiting.insert(ty.clone()) {
            return None;
        }
        let result = self.async_meta_sop_frame_safety_violation_inner(ty, path, visiting);
        visiting.remove(ty);
        result
    }

    fn async_meta_sop_frame_safety_violation_inner(
        &mut self,
        ty: &Ty,
        path: &str,
        visiting: &mut HashSet<Ty>,
    ) -> Option<String> {
        let Ty::Named { name, args, .. } = ty else {
            return Some(format!(
                "{path} has malformed /std/meta structural representation type `{ty}`"
            ));
        };
        match name.as_str() {
            "HNil" | "CoNil" | "ArrayNil" if args.is_empty() => None,
            "HCons" if args.len() == 2 => self
                .async_frame_safety_violation(&args[0], false, &format!("{path}.head"), visiting)
                .or_else(|| {
                    self.async_frame_safety_violation(
                        &args[1],
                        false,
                        &format!("{path}.tail"),
                        visiting,
                    )
                }),
            "Coproduct" if args.len() == 2 => self
                .async_frame_safety_violation(&args[0], false, &format!("{path}.This"), visiting)
                .or_else(|| {
                    self.async_frame_safety_violation(
                        &args[1],
                        false,
                        &format!("{path}.Next"),
                        visiting,
                    )
                }),
            "FieldRef" | "PayloadRef" | "VariantRef" => Some(format!(
                "{path} has borrowed structural meta representation type `{ty}`"
            )),
            "Field" | "Payload" if args.len() == 1 => self.async_frame_safety_violation(
                &args[0],
                false,
                &format!("{path}.value"),
                visiting,
            ),
            "Variant" if args.len() == 1 => self.async_frame_safety_violation(
                &args[0],
                false,
                &format!("{path}.payload"),
                visiting,
            ),
            "FieldSchema" | "PayloadSchema" | "ElementSchema" if args.len() == 2 => None,
            "VariantSchema" if args.len() == 1 => None,
            "ArrayCat" if args.len() == 2 => self
                .async_frame_safety_violation(&args[0], false, &format!("{path}.left"), visiting)
                .or_else(|| {
                    self.async_frame_safety_violation(
                        &args[1],
                        false,
                        &format!("{path}.right"),
                        visiting,
                    )
                }),
            name if name
                .strip_prefix("ArrayChunk")
                .and_then(|suffix| suffix.parse::<usize>().ok())
                .is_some_and(|len| (1..=crate::types::META_ARRAY_CHUNK_SIZE).contains(&len))
                && args.len() == 1 =>
            {
                self.async_frame_safety_violation(
                    &args[0],
                    false,
                    &format!("{path}.item"),
                    visiting,
                )
            }
            _ => Some(format!(
                "{path} has malformed /std/meta structural representation type `{ty}`"
            )),
        }
    }

    pub(super) fn async_future_state_frame_safety_violation(
        &mut self,
        ty: &Ty,
        path: &str,
        visiting: &mut HashSet<Ty>,
    ) -> Option<String> {
        match ty {
            Ty::GeneratedFuture { state, .. } => {
                if !visiting.insert(ty.clone()) {
                    return None;
                }
                for (name, state_ty) in state {
                    let state_path = if name.is_empty() {
                        format!("{path} state")
                    } else {
                        format!("{path} state `{name}`")
                    };
                    if let Some(reason) = self.async_future_state_frame_safety_violation(
                        state_ty,
                        &state_path,
                        visiting,
                    ) {
                        visiting.remove(ty);
                        return Some(reason);
                    }
                }
                visiting.remove(ty);
                None
            }
            Ty::OpaqueState { base, state } => {
                if !visiting.insert(ty.clone()) {
                    return None;
                }
                if let Some(reason) = self.async_frame_safety_violation(base, false, path, visiting)
                {
                    visiting.remove(ty);
                    return Some(reason);
                }
                for (name, state_ty) in state {
                    let state_path = if name.is_empty() {
                        format!("{path} state")
                    } else {
                        format!("{path} state `{name}`")
                    };
                    if let Some(reason) = self.async_future_state_frame_safety_violation(
                        state_ty,
                        &state_path,
                        visiting,
                    ) {
                        visiting.remove(ty);
                        return Some(reason);
                    }
                }
                visiting.remove(ty);
                None
            }
            _ => self.async_frame_safety_violation(ty, false, path, visiting),
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

#[derive(Clone, Debug, Default)]
struct AsyncLiveness {
    live_across_await: HashSet<LocalId>,
    await_sites: Vec<AsyncAwaitSite>,
}

#[derive(Clone, Debug)]
struct AsyncAwaitSite {
    span: crate::span::Span,
    live: HashSet<LocalId>,
}

#[derive(Clone, Debug)]
struct AsyncControlFrame {
    break_live: HashSet<LocalId>,
    continue_live: Option<HashSet<LocalId>>,
}

#[derive(Clone, Debug)]
struct AsyncLivenessAnalyzer {
    liveness: AsyncLiveness,
    control_stack: Vec<AsyncControlFrame>,
    record_awaits: bool,
}

impl AsyncLivenessAnalyzer {
    fn new() -> Self {
        Self {
            liveness: AsyncLiveness::default(),
            control_stack: Vec::new(),
            record_awaits: true,
        }
    }

    fn analyze_block(mut self, block: &TBlock) -> AsyncLiveness {
        self.live_before_block(block, HashSet::new());
        self.liveness
    }

    fn analyze_expr(mut self, expr: &TExpr) -> AsyncLiveness {
        self.collect_awaits_in_expr(expr, &HashSet::new());
        self.liveness
    }

    fn without_recording<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let old_record_awaits = self.record_awaits;
        self.record_awaits = false;
        let result = f(self);
        self.record_awaits = old_record_awaits;
        result
    }

    fn with_control_frame<T>(
        &mut self,
        frame: AsyncControlFrame,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.control_stack.push(frame);
        let result = f(self);
        self.control_stack.pop();
        result
    }

    fn current_break_live(&self) -> HashSet<LocalId> {
        self.control_stack
            .last()
            .map(|frame| frame.break_live.clone())
            .unwrap_or_default()
    }

    fn current_continue_live(&self) -> HashSet<LocalId> {
        self.control_stack
            .iter()
            .rev()
            .find_map(|frame| frame.continue_live.clone())
            .unwrap_or_default()
    }

    fn record_await(&mut self, span: crate::span::Span, live: HashSet<LocalId>) {
        if !self.record_awaits {
            return;
        }
        self.liveness.live_across_await.extend(live.iter().copied());
        self.liveness
            .await_sites
            .push(AsyncAwaitSite { span, live });
    }

    fn live_before_block(
        &mut self,
        block: &TBlock,
        mut live: HashSet<LocalId>,
    ) -> HashSet<LocalId> {
        for stmt in block.statements.iter().rev() {
            live = self.live_before_stmt(stmt, live);
        }
        live
    }

    fn live_before_stmt(&mut self, stmt: &TStmt, live_after: HashSet<LocalId>) -> HashSet<LocalId> {
        match &stmt.kind {
            TStmtKind::Block(block) => self.live_before_block(block, live_after),
            TStmtKind::VarDecl { local_id, init, .. } => {
                let mut live = live_after;
                live.remove(local_id);
                if let Some(init) = init {
                    self.collect_awaits_in_expr(init, &live);
                    live.extend(TypeChecker::async_expr_used_locals(init));
                }
                live
            }
            TStmtKind::Assign { target, value } => {
                let mut live = live_after;
                if let TExprKind::Local(local_id, _) = &target.kind {
                    live.remove(local_id);
                }
                self.collect_awaits_in_expr(value, &live);
                self.collect_awaits_in_expr(target, &live);
                live.extend(TypeChecker::async_expr_used_locals(value));
                if !matches!(target.kind, TExprKind::Local(..)) {
                    live.extend(TypeChecker::async_expr_used_locals(target));
                }
                live
            }
            TStmtKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                let then_live = self.live_before_block(then_block, live_after.clone());
                let else_live = else_branch
                    .as_ref()
                    .map(|stmt| self.live_before_stmt(stmt, live_after.clone()))
                    .unwrap_or_else(|| live_after.clone());
                let mut live = then_live;
                live.extend(else_live);
                self.collect_awaits_in_expr(cond, &live);
                live.extend(TypeChecker::async_expr_used_locals(cond));
                live
            }
            TStmtKind::While { cond, body } => self.live_before_while(cond, body, live_after),
            TStmtKind::For {
                init,
                cond,
                step,
                body,
            } => self.live_before_for(
                init.as_ref(),
                cond.as_ref(),
                step.as_ref(),
                body,
                live_after,
            ),
            TStmtKind::Switch {
                expr,
                cases,
                default,
                ..
            } => self.live_before_switch(expr, cases, default, live_after),
            TStmtKind::Defer(expr) | TStmtKind::ResourceCleanup(expr) | TStmtKind::Expr(expr) => {
                let mut live = live_after;
                self.collect_awaits_in_expr(expr, &live);
                live.extend(TypeChecker::async_expr_used_locals(expr));
                live
            }
            TStmtKind::Return(Some(expr)) => {
                let live = HashSet::new();
                self.collect_awaits_in_expr(expr, &live);
                TypeChecker::async_expr_used_locals(expr)
            }
            TStmtKind::Break => self.current_break_live(),
            TStmtKind::Continue => self.current_continue_live(),
            TStmtKind::Return(None) | TStmtKind::Unsupported => HashSet::new(),
        }
    }

    fn live_before_for_init(
        &mut self,
        init: &TForInit,
        live_after: HashSet<LocalId>,
    ) -> HashSet<LocalId> {
        match init {
            TForInit::VarDecl { local_id, init, .. } => {
                let mut live = live_after;
                live.remove(local_id);
                if let Some(init) = init {
                    self.collect_awaits_in_expr(init, &live);
                    live.extend(TypeChecker::async_expr_used_locals(init));
                }
                live
            }
            TForInit::Assign { target, value } => {
                let mut live = live_after;
                if let TExprKind::Local(local_id, _) = &target.kind {
                    live.remove(local_id);
                }
                self.collect_awaits_in_expr(value, &live);
                self.collect_awaits_in_expr(target, &live);
                live.extend(TypeChecker::async_expr_used_locals(value));
                if !matches!(target.kind, TExprKind::Local(..)) {
                    live.extend(TypeChecker::async_expr_used_locals(target));
                }
                live
            }
            TForInit::Expr(expr) => {
                let mut live = live_after;
                self.collect_awaits_in_expr(expr, &live);
                live.extend(TypeChecker::async_expr_used_locals(expr));
                live
            }
        }
    }

    fn live_before_while(
        &mut self,
        cond: &TExpr,
        body: &TBlock,
        live_after: HashSet<LocalId>,
    ) -> HashSet<LocalId> {
        let cond_uses = TypeChecker::async_expr_used_locals(cond);
        let mut loop_live = live_after.clone();
        loop_live.extend(cond_uses.iter().copied());

        loop {
            let frame = AsyncControlFrame {
                break_live: live_after.clone(),
                continue_live: Some(loop_live.clone()),
            };
            let body_live = self.without_recording(|this| {
                this.with_control_frame(frame, |this| {
                    this.live_before_block(body, loop_live.clone())
                })
            });
            let mut next_loop_live = live_after.clone();
            next_loop_live.extend(cond_uses.iter().copied());
            next_loop_live.extend(body_live);
            if next_loop_live == loop_live {
                break;
            }
            loop_live = next_loop_live;
        }

        let frame = AsyncControlFrame {
            break_live: live_after,
            continue_live: Some(loop_live.clone()),
        };
        self.with_control_frame(frame, |this| {
            this.live_before_block(body, loop_live.clone());
        });
        self.collect_awaits_in_expr(cond, &loop_live);
        loop_live
    }

    fn live_before_for(
        &mut self,
        init: Option<&TForInit>,
        cond: Option<&TExpr>,
        step: Option<&TForInit>,
        body: &TBlock,
        live_after: HashSet<LocalId>,
    ) -> HashSet<LocalId> {
        let loop_live = self.live_before_for_loop(cond, step, body, live_after);
        if let Some(cond) = cond {
            self.collect_awaits_in_expr(cond, &loop_live);
        }
        if let Some(init) = init {
            self.live_before_for_init(init, loop_live)
        } else {
            loop_live
        }
    }

    fn live_before_for_loop(
        &mut self,
        cond: Option<&TExpr>,
        step: Option<&TForInit>,
        body: &TBlock,
        live_after: HashSet<LocalId>,
    ) -> HashSet<LocalId> {
        let cond_uses = cond
            .map(TypeChecker::async_expr_used_locals)
            .unwrap_or_default();
        let mut loop_live = live_after.clone();
        loop_live.extend(cond_uses.iter().copied());

        loop {
            let live_before_step = self.without_recording(|this| {
                let mut live_before_step = loop_live.clone();
                if let Some(step) = step {
                    live_before_step = this.live_before_for_init(step, live_before_step);
                }
                live_before_step
            });
            let frame = AsyncControlFrame {
                break_live: live_after.clone(),
                continue_live: Some(live_before_step.clone()),
            };
            let body_live = self.without_recording(|this| {
                this.with_control_frame(frame, |this| {
                    this.live_before_block(body, live_before_step)
                })
            });

            let mut next_loop_live = live_after.clone();
            next_loop_live.extend(cond_uses.iter().copied());
            next_loop_live.extend(body_live);
            if next_loop_live == loop_live {
                break;
            }
            loop_live = next_loop_live;
        }

        let mut live_before_step = loop_live.clone();
        if let Some(step) = step {
            live_before_step = self.live_before_for_init(step, live_before_step);
        }
        let frame = AsyncControlFrame {
            break_live: live_after,
            continue_live: Some(live_before_step.clone()),
        };
        self.with_control_frame(frame, |this| {
            this.live_before_block(body, live_before_step);
        });

        loop_live
    }

    fn live_before_switch(
        &mut self,
        expr: &TExpr,
        cases: &[TCase],
        default: &[TStmt],
        live_after: HashSet<LocalId>,
    ) -> HashSet<LocalId> {
        let frame = AsyncControlFrame {
            break_live: live_after.clone(),
            continue_live: None,
        };
        let mut live = self.with_control_frame(frame, |this| {
            let mut live = HashSet::new();
            for case in cases {
                let mut case_live = live_after.clone();
                for stmt in case.statements.iter().rev() {
                    case_live = this.live_before_stmt(stmt, case_live);
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
                default_live = this.live_before_stmt(stmt, default_live);
            }
            live.extend(default_live);
            live
        });
        self.collect_awaits_in_expr(expr, &live);
        live.extend(TypeChecker::async_expr_used_locals(expr));
        live
    }

    fn collect_awaits_in_expr(&mut self, expr: &TExpr, live_after: &HashSet<LocalId>) {
        let mut live_after = live_after.clone();
        live_after.extend(TypeChecker::async_expr_used_locals(expr));
        self.collect_awaits_in_expr_inner(expr, &live_after);
    }

    fn collect_awaits_in_expr_inner(&mut self, expr: &TExpr, live_after: &HashSet<LocalId>) {
        match &expr.kind {
            TExprKind::Await { future } => {
                let mut live = live_after.clone();
                live.extend(TypeChecker::async_expr_used_locals(future));
                self.record_await(expr.span, live);
                self.collect_awaits_in_expr_inner(future, live_after);
            }
            TExprKind::AsyncSelect { arms, .. } => {
                let mut live = live_after.clone();
                for arm in arms {
                    live.extend(TypeChecker::async_expr_used_locals(&arm.future));
                    let mut body_live = TypeChecker::async_expr_used_locals(&arm.body);
                    body_live.remove(&arm.binding_local);
                    live.extend(body_live);
                }
                self.record_await(expr.span, live);
                for arm in arms {
                    self.collect_awaits_in_expr_inner(&arm.future, live_after);
                    self.collect_awaits_in_expr_inner(&arm.body, live_after);
                }
            }
            TExprKind::Closure { .. } => {}
            TExprKind::UnsafeBlock { statements, value } => {
                let mut live = live_after.clone();
                if let Some(value) = value {
                    self.collect_awaits_in_expr_inner(value, &live);
                    live.extend(TypeChecker::async_expr_used_locals(value));
                }
                for stmt in statements.iter().rev() {
                    live = self.live_before_stmt(stmt, live);
                }
            }
            _ => walk_expr_children(expr, &mut |child| {
                self.collect_awaits_in_expr_inner(child, live_after);
            }),
        }
    }
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
