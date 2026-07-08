use super::*;

impl<'a> CGenerator<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_actor_spawn_expr(
        &mut self,
        expr: &TExpr,
        mode: &ActorSpawnMode,
        state_arg: &TExpr,
        handler: &TExpr,
        state_ty: &Ty,
        handle_message_ty: &Ty,
        message_ty: &Ty,
        handler_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        match mode {
            ActorSpawnMode::Cloned => self.emit_actor_spawn_cloned_expr(
                expr,
                state_arg,
                handler,
                state_ty,
                handle_message_ty,
                message_ty,
                handler_ty,
                indent,
            ),
            ActorSpawnMode::State => self.emit_actor_spawn_state_expr(
                expr,
                state_arg,
                handler,
                state_ty,
                handle_message_ty,
                message_ty,
                handler_ty,
                indent,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_actor_spawn_cloned_expr(
        &mut self,
        expr: &TExpr,
        initial_state: &TExpr,
        handler: &TExpr,
        state_ty: &Ty,
        handle_message_ty: &Ty,
        message_ty: &Ty,
        handler_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let result_layout = self.result_layout(&expr.ty, expr.span)?;
        let result_temp = self.next_temp("actor_spawn_result");
        let done_label = self.next_temp("actor_spawn_done");
        self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &result_temp)));

        let state_src = self.emit_temp_value("actor_state_src", initial_state, indent)?;
        let state_clone = self.emit_task_boundary_clone_result_from_ptr(
            state_ty,
            &format!("&{state_src}"),
            indent,
            expr.span,
        )?;
        self.emit_clone_error_jump(
            &result_temp,
            &result_layout,
            &state_clone,
            state_ty,
            &done_label,
            indent,
            expr.span,
        )?;
        let state_box = self.next_temp("actor_state_box");
        self.line_indent(
            indent,
            &format!(
                "{} = ciel_alloc(sizeof(*{state_box}));",
                self.c_pointer_decl(state_ty, &state_box)
            ),
        );
        let state_clone_layout = self.result_layout(
            &std_result_ty(
                &self.program.checked.resolved,
                state_ty.clone(),
                std_error_ty(&self.program.checked.resolved),
            ),
            expr.span,
        )?;
        self.emit_value_copy(
            &format!("(*{state_box})"),
            &format!("{state_clone}.as.{}._0", state_clone_layout.ok_name),
            state_ty,
            indent,
        );

        let handler_src = self.emit_temp_value("actor_handler_src", handler, indent)?;
        let handler_clone = self.emit_clone_message_result_from_ptr(
            handler_ty,
            &format!("&{handler_src}"),
            indent,
            expr.span,
        )?;
        self.emit_clone_error_jump(
            &result_temp,
            &result_layout,
            &handler_clone,
            handler_ty,
            &done_label,
            indent,
            expr.span,
        )?;
        let handler_box = self.next_temp("actor_handler_box");
        self.line_indent(
            indent,
            &format!(
                "{} = ciel_alloc(sizeof(*{handler_box}));",
                self.c_pointer_decl(handler_ty, &handler_box)
            ),
        );
        let handler_clone_layout = self.result_layout(
            &std_result_ty(
                &self.program.checked.resolved,
                handler_ty.clone(),
                std_error_ty(&self.program.checked.resolved),
            ),
            expr.span,
        )?;
        self.emit_value_copy(
            &format!("(*{handler_box})"),
            &format!("{handler_clone}.as.{}._0", handler_clone_layout.ok_name),
            handler_ty,
            indent,
        );

        let raw_actor = self.next_temp("actor_raw");
        let rc = self.next_temp("actor_rc");
        let dispatch =
            self.actor_dispatch_name(&ActorSpawnMode::Cloned, state_ty, message_ty, handler_ty);
        self.line_indent(indent, &format!("CielActor *{raw_actor} = NULL;"));
        self.line_indent(
            indent,
            &format!(
                "int32_t {rc} = ciel_actor_spawn(&{raw_actor}, (void *){state_box}, (void *){handler_box}, {dispatch});"
            ),
        );
        self.line_indent(indent, &format!("if ({rc} != 0) {{"));
        self.line_indent(
            indent + 1,
            &format!(
                "{result_temp} = {};",
                self.result_err_from_error_literal(&result_layout, &self.error_code_literal(&rc))
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done_label};"));
        self.line_indent(indent, "}");
        let actor_ty = std_actor_ty(&self.program.checked.resolved, handle_message_ty.clone());
        let actor_value = format!(
            "({}){{ .handle = (void *){raw_actor} }}",
            self.c_type(&actor_ty)
        );
        self.line_indent(
            indent,
            &format!(
                "{result_temp} = {};",
                self.result_ok_literal(&result_layout, Some(&actor_value))
            ),
        );
        self.line_indent(indent, &format!("{done_label}:;"));
        Ok(result_temp)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_actor_spawn_state_expr(
        &mut self,
        expr: &TExpr,
        init: &TExpr,
        handler: &TExpr,
        state_ty: &Ty,
        handle_message_ty: &Ty,
        message_ty: &Ty,
        handler_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let result_layout = self.result_layout(&expr.ty, expr.span)?;
        let result_temp = self.next_temp("actor_spawn_result");
        let done_label = self.next_temp("actor_spawn_done");
        self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &result_temp)));

        let init_src = self.emit_temp_value("actor_state_init", init, indent)?;
        let actor_owner = self.next_temp("actor_owner");
        let actor_owner_rc = self.next_temp("actor_owner_rc");
        self.line_indent(indent, &format!("CielResourceOwner *{actor_owner} = NULL;"));
        self.line_indent(indent, &format!("int32_t {actor_owner_rc} = 0;"));
        self.line_indent(
            indent,
            &format!(
                "{actor_owner} = ciel_resource_owner_new_child(ciel_resource_current_owner_or_root(), ciel_resource_default_limits(), &{actor_owner_rc});"
            ),
        );
        self.line_indent(indent, &format!("if ({actor_owner} == NULL) {{"));
        self.line_indent(
            indent + 1,
            &format!("if ({actor_owner_rc} == 0) {actor_owner_rc} = ENOMEM;"),
        );
        self.line_indent(
            indent + 1,
            &format!(
                "{result_temp} = {};",
                self.result_err_from_error_literal(
                    &result_layout,
                    &self.error_code_literal(&actor_owner_rc)
                )
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done_label};"));
        self.line_indent(indent, "}");
        let init_call = self.callable_call_expr(&init.ty, &init_src, &[])?;
        let init_result_ty = std_result_ty(
            &self.program.checked.resolved,
            state_ty.clone(),
            std_error_ty(&self.program.checked.resolved),
        );
        let init_result_layout = self.result_layout(&init_result_ty, expr.span)?;
        let init_result = self.next_temp("actor_state_init_result");
        let previous_owner = self.next_temp("actor_previous_owner");
        self.line_indent(
            indent,
            &format!(
                "CielResourceOwner *{previous_owner} = ciel_resource_set_current_owner({actor_owner});"
            ),
        );
        self.line_indent(
            indent,
            &format!(
                "{} = {init_call};",
                self.c_decl(&init_result_ty, &init_result)
            ),
        );
        self.line_indent(
            indent,
            &format!("ciel_resource_restore_current_owner({previous_owner});"),
        );
        self.line_indent(
            indent,
            &format!(
                "if ({init_result}.tag == {}) {{",
                init_result_layout.err_index
            ),
        );
        self.line_indent(
            indent + 1,
            &format!("(void)ciel_resource_owner_close({actor_owner});"),
        );
        self.line_indent(
            indent + 1,
            &format!(
                "{result_temp} = {};",
                self.result_err_literal(&result_layout, &init_result_layout, &init_result)
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done_label};"));
        self.line_indent(indent, "}");

        let state_box = self.next_temp("actor_state_box");
        self.line_indent(
            indent,
            &format!(
                "{} = ciel_alloc(sizeof(*{state_box}));",
                self.c_pointer_decl(state_ty, &state_box)
            ),
        );
        self.emit_value_copy(
            &format!("(*{state_box})"),
            &format!("{init_result}.as.{}._0", init_result_layout.ok_name),
            state_ty,
            indent,
        );

        let handler_src = self.emit_temp_value("actor_handler_src", handler, indent)?;
        let handler_clone = self.emit_clone_message_result_from_ptr(
            handler_ty,
            &format!("&{handler_src}"),
            indent,
            expr.span,
        )?;
        let handler_clone_layout = self.result_layout(
            &std_result_ty(
                &self.program.checked.resolved,
                handler_ty.clone(),
                std_error_ty(&self.program.checked.resolved),
            ),
            expr.span,
        )?;
        self.line_indent(
            indent,
            &format!(
                "if ({handler_clone}.tag == {}) {{",
                handler_clone_layout.err_index
            ),
        );
        self.line_indent(
            indent + 1,
            &format!("(void)ciel_resource_owner_close({actor_owner});"),
        );
        self.line_indent(
            indent + 1,
            &format!(
                "{result_temp} = {};",
                self.result_err_literal(&result_layout, &handler_clone_layout, &handler_clone)
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done_label};"));
        self.line_indent(indent, "}");
        let handler_box = self.next_temp("actor_handler_box");
        self.line_indent(
            indent,
            &format!(
                "{} = ciel_alloc(sizeof(*{handler_box}));",
                self.c_pointer_decl(handler_ty, &handler_box)
            ),
        );
        self.emit_value_copy(
            &format!("(*{handler_box})"),
            &format!("{handler_clone}.as.{}._0", handler_clone_layout.ok_name),
            handler_ty,
            indent,
        );

        let actor_owner_detach_rc = self.next_temp("actor_owner_detach_rc");
        self.line_indent(
            indent,
            &format!(
                "int32_t {actor_owner_detach_rc} = ciel_resource_owner_detach({actor_owner});"
            ),
        );
        self.line_indent(indent, &format!("if ({actor_owner_detach_rc} != 0) {{"));
        self.line_indent(
            indent + 1,
            &format!("(void)ciel_resource_owner_close({actor_owner});"),
        );
        self.line_indent(
            indent + 1,
            &format!(
                "{result_temp} = {};",
                self.result_err_from_error_literal(
                    &result_layout,
                    &self.error_code_literal(&actor_owner_detach_rc)
                )
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done_label};"));
        self.line_indent(indent, "}");

        let raw_actor = self.next_temp("actor_raw");
        let rc = self.next_temp("actor_rc");
        let dispatch =
            self.actor_dispatch_name(&ActorSpawnMode::State, state_ty, message_ty, handler_ty);
        self.line_indent(indent, &format!("CielActor *{raw_actor} = NULL;"));
        self.line_indent(
            indent,
            &format!(
                "int32_t {rc} = ciel_actor_spawn_with_owner(&{raw_actor}, (void *){state_box}, (void *){handler_box}, {dispatch}, {actor_owner});"
            ),
        );
        self.line_indent(indent, &format!("if ({rc} != 0) {{"));
        self.line_indent(
            indent + 1,
            &format!("(void)ciel_resource_owner_close({actor_owner});"),
        );
        self.line_indent(
            indent + 1,
            &format!(
                "{result_temp} = {};",
                self.result_err_from_error_literal(&result_layout, &self.error_code_literal(&rc))
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done_label};"));
        self.line_indent(indent, "}");
        let actor_ty = std_actor_ty(&self.program.checked.resolved, handle_message_ty.clone());
        let actor_value = format!(
            "({}){{ .handle = (void *){raw_actor} }}",
            self.c_type(&actor_ty)
        );
        self.line_indent(
            indent,
            &format!(
                "{result_temp} = {};",
                self.result_ok_literal(&result_layout, Some(&actor_value))
            ),
        );
        self.line_indent(indent, &format!("{done_label}:;"));
        Ok(result_temp)
    }

    pub(super) fn emit_actor_send_expr(
        &mut self,
        expr: &TExpr,
        actor: &TExpr,
        value: &TExpr,
        message_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let result_layout = self.result_layout(&expr.ty, expr.span)?;
        let result_temp = self.next_temp("actor_send_result");
        let done_label = self.next_temp("actor_send_done");
        self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &result_temp)));

        let value_src = self.emit_temp_value("actor_msg_src", value, indent)?;
        let clone_result = self.emit_task_boundary_clone_result_from_ptr(
            message_ty,
            &format!("&{value_src}"),
            indent,
            expr.span,
        )?;
        self.emit_clone_error_jump(
            &result_temp,
            &result_layout,
            &clone_result,
            message_ty,
            &done_label,
            indent,
            expr.span,
        )?;
        let clone_layout = self.result_layout(
            &std_result_ty(
                &self.program.checked.resolved,
                message_ty.clone(),
                std_error_ty(&self.program.checked.resolved),
            ),
            expr.span,
        )?;
        let msg_box = self.next_temp("actor_msg_box");
        self.line_indent(
            indent,
            &format!(
                "{} = ciel_actor_message_alloc(sizeof(*{msg_box}));",
                self.c_pointer_decl(message_ty, &msg_box)
            ),
        );
        self.emit_value_copy(
            &format!("(*{msg_box})"),
            &format!("{clone_result}.as.{}._0", clone_layout.ok_name),
            message_ty,
            indent,
        );
        let handle = self.emit_actor_handle(actor, indent)?;
        let rc = self.next_temp("actor_send_rc");
        self.line_indent(
            indent,
            &format!("int32_t {rc} = ciel_actor_send({handle}, (void *){msg_box});"),
        );
        self.emit_runtime_result_from_rc(&result_temp, &result_layout, &rc, &done_label, indent);
        self.line_indent(indent, &format!("{done_label}:;"));
        Ok(result_temp)
    }

    pub(super) fn emit_actor_lifecycle_expr(
        &mut self,
        expr: &TExpr,
        actor: &TExpr,
        runtime_fn: &str,
        indent: usize,
    ) -> DiagResult<String> {
        let result_layout = self.result_layout(&expr.ty, expr.span)?;
        let result_temp = self.next_temp("actor_lifecycle_result");
        let done_label = self.next_temp("actor_lifecycle_done");
        self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &result_temp)));
        let handle = self.emit_actor_handle(actor, indent)?;
        let rc = self.next_temp("actor_lifecycle_rc");
        self.line_indent(indent, &format!("int32_t {rc} = {runtime_fn}({handle});"));
        self.emit_runtime_result_from_rc(&result_temp, &result_layout, &rc, &done_label, indent);
        self.line_indent(indent, &format!("{done_label}:;"));
        Ok(result_temp)
    }

    pub(super) fn emit_clone_error_jump(
        &mut self,
        result_temp: &str,
        result_layout: &ResultLayout,
        clone_result: &str,
        cloned_ty: &Ty,
        done_label: &str,
        indent: usize,
        span: crate::span::Span,
    ) -> DiagResult<()> {
        let clone_layout = self.result_layout(
            &std_result_ty(
                &self.program.checked.resolved,
                cloned_ty.clone(),
                std_error_ty(&self.program.checked.resolved),
            ),
            span,
        )?;
        self.line_indent(
            indent,
            &format!("if ({clone_result}.tag == {}) {{", clone_layout.err_index),
        );
        self.line_indent(
            indent + 1,
            &format!(
                "{result_temp} = {};",
                self.result_err_literal(result_layout, &clone_layout, clone_result)
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done_label};"));
        self.line_indent(indent, "}");
        Ok(())
    }

    pub(in crate::codegen) fn emit_async_clone_error_jump(
        &mut self,
        result_temp: &str,
        result_layout: &ResultLayout,
        clone_result: &str,
        cloned_ty: &Ty,
        done_label: &str,
        indent: usize,
        span: crate::span::Span,
    ) -> DiagResult<()> {
        let clone_layout = self.result_layout(
            &std_result_ty(
                &self.program.checked.resolved,
                cloned_ty.clone(),
                std_error_ty(&self.program.checked.resolved),
            ),
            span,
        )?;
        self.line_indent(
            indent,
            &format!("if ({clone_result}.tag == {}) {{", clone_layout.err_index),
        );
        let clone_error = format!("{clone_result}.as.{}._0", clone_layout.err_name);
        let err_value =
            self.result_err_from_message_clone_literal(result_layout, &clone_error, span)?;
        self.line_indent(indent + 1, &format!("{result_temp} = {err_value};"));
        self.line_indent(indent + 1, &format!("goto {done_label};"));
        self.line_indent(indent, "}");
        Ok(())
    }

    pub(super) fn emit_runtime_result_from_rc(
        &mut self,
        result_temp: &str,
        result_layout: &ResultLayout,
        rc: &str,
        done_label: &str,
        indent: usize,
    ) {
        self.line_indent(indent, &format!("if ({rc} != 0) {{"));
        self.line_indent(
            indent + 1,
            &format!(
                "{result_temp} = {};",
                self.result_err_from_error_literal(result_layout, &self.error_code_literal(rc))
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done_label};"));
        self.line_indent(indent, "}");
        self.line_indent(
            indent,
            &format!(
                "{result_temp} = {};",
                self.result_ok_literal(result_layout, None)
            ),
        );
    }

    pub(super) fn emit_async_runtime_result_from_rc(
        &mut self,
        result_temp: &str,
        result_layout: &ResultLayout,
        rc: &str,
        done_label: &str,
        indent: usize,
        span: crate::span::Span,
    ) -> DiagResult<()> {
        self.line_indent(indent, &format!("if ({rc} != 0) {{"));
        let err_value = self.result_err_from_runtime_literal(result_layout, rc, span)?;
        self.line_indent(indent + 1, &format!("{result_temp} = {err_value};"));
        self.line_indent(indent + 1, &format!("goto {done_label};"));
        self.line_indent(indent, "}");
        self.line_indent(
            indent,
            &format!(
                "{result_temp} = {};",
                self.result_ok_literal(result_layout, None)
            ),
        );
        Ok(())
    }

    pub(super) fn emit_actor_handle(&mut self, actor: &TExpr, indent: usize) -> DiagResult<String> {
        let actor_temp = self.emit_temp_value("actor_ref", actor, indent)?;
        Ok(format!("(CielActor *)({actor_temp}->handle)"))
    }

    pub(in crate::codegen) fn std_resource_scoped_call(
        &self,
        callee: &TExpr,
    ) -> Option<ResourceScopedCall> {
        let TExprKind::Function(def_id, _) = &callee.kind else {
            return None;
        };
        let origin = self.program.generic_origins.get(def_id)?;
        let def = self.program.checked.resolved.def(origin.template_def);
        if std_id::is_std_resource_function(
            &self.program.checked.resolved,
            def.module,
            &origin.template_name,
            "scoped",
        ) {
            return Some(ResourceScopedCall::Default);
        }
        if std_id::is_std_resource_function(
            &self.program.checked.resolved,
            def.module,
            &origin.template_name,
            "scoped_with_limits",
        ) {
            return Some(ResourceScopedCall::WithLimits);
        }
        None
    }

    pub(in crate::codegen) fn std_resource_transfer_before_owner_close_call(
        &self,
        callee: &TExpr,
    ) -> bool {
        let TExprKind::Function(def_id, _) = &callee.kind else {
            return false;
        };
        let Some(origin) = self.program.generic_origins.get(def_id) else {
            return false;
        };
        let def = self.program.checked.resolved.def(origin.template_def);
        std_id::is_std_resource_function(
            &self.program.checked.resolved,
            def.module,
            &origin.template_name,
            "transfer_to_parent_before_owner_close",
        )
    }

    pub(super) fn emit_resource_transfer_before_owner_close_call(
        &mut self,
        expr: &TExpr,
        args: &[TExpr],
        indent: usize,
    ) -> DiagResult<String> {
        if args.len() != 1 {
            return Err(vec![Diagnostic::new(
                expr.span,
                "internal error: resource owner transfer hook has wrong arity",
            )]);
        }
        let layout = self.result_layout(&expr.ty, expr.span)?;
        let Ty::Pointer { inner, .. } = &args[0].ty else {
            return Err(vec![Diagnostic::new(
                args[0].span,
                "internal error: resource owner transfer hook expects pointer argument",
            )]);
        };
        let value_ty = inner.as_ref().clone();
        let value = self.gen_expr_in_stmt(&args[0], indent)?;
        let result = self.next_temp("resource_owner_transfer_result");
        self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &result)));
        self.line_indent(
            indent,
            &format!("{result} = {};", self.result_ok_literal(&layout, None)),
        );
        if self.type_is_affine(&value_ty) {
            let transfer_rc = self.next_temp("resource_owner_transfer_rc");
            let helper = self.resource_transfer_to_parent_name(&value_ty);
            let value = format!("(({})({value}))", self.c_pointer_type(&value_ty));
            self.line_indent(
                indent,
                &format!("int32_t {transfer_rc} = {helper}({value});"),
            );
            self.line_indent(indent, &format!("if ({transfer_rc} != 0) {{"));
            let err_value =
                self.result_err_from_runtime_literal(&layout, &transfer_rc, expr.span)?;
            self.line_indent(indent + 1, &format!("{result} = {err_value};"));
            self.line_indent(indent, "}");
        } else {
            self.line_indent(indent, &format!("(void)({value});"));
        }
        Ok(result)
    }

    pub(super) fn emit_resource_scoped_call(
        &mut self,
        expr: &TExpr,
        args: &[TExpr],
        scoped: ResourceScopedCall,
        indent: usize,
    ) -> DiagResult<String> {
        let Some((ok_ty, scoped_err_ty)) = result_args(&self.program.checked.resolved, &expr.ty)
        else {
            return Err(vec![Diagnostic::new(
                expr.span,
                "internal error: resource scoped call must return Result",
            )]);
        };
        let layout = self.result_layout(&expr.ty, expr.span)?;
        let (limits_arg, body_arg) = match scoped {
            ResourceScopedCall::Default => {
                let Some(body) = args.first() else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "internal error: scoped call missing body",
                    )]);
                };
                (None, body)
            }
            ResourceScopedCall::WithLimits => {
                if args.len() != 2 {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "internal error: scoped_with_limits call has wrong arity",
                    )]);
                }
                (Some(&args[0]), &args[1])
            }
        };

        let result = self.next_temp("resource_scoped_result");
        let body_result = self.next_temp("resource_scoped_body");
        let push_rc = self.next_temp("resource_scoped_push_rc");
        let close_rc = self.next_temp("resource_scoped_close_rc");
        let transfer_rc = self.next_temp("resource_scoped_transfer_rc");
        let done = self.next_temp("resource_scoped_done");
        let body = self.emit_temp_value("resource_scoped_body_fn", body_arg, indent)?;
        let limits = if let Some(limits_arg) = limits_arg {
            Some(self.emit_temp_value("resource_scoped_limits", limits_arg, indent)?)
        } else {
            None
        };
        let Some((body_ret_ty, body_params)) = callable_ret_params_ty(&body_arg.ty) else {
            return Err(vec![Diagnostic::new(
                body_arg.span,
                "internal error: resource scoped body is not callable",
            )]);
        };
        if !body_params.is_empty() {
            return Err(vec![Diagnostic::new(
                body_arg.span,
                "internal error: resource scoped body must not take arguments",
            )]);
        }
        let Some((body_ok_ty, body_err_ty)) =
            result_args(&self.program.checked.resolved, &body_ret_ty)
        else {
            return Err(vec![Diagnostic::new(
                body_arg.span,
                "internal error: resource scoped body must return Result",
            )]);
        };
        if body_ok_ty != ok_ty {
            return Err(vec![Diagnostic::new(
                body_arg.span,
                format!(
                    "internal error: resource scoped body returns `{body_ok_ty}`, but scoped call returns `{ok_ty}`"
                ),
            )]);
        }
        if !self.enum_has_variant_with_payload(
            scoped_err_ty,
            "Resource",
            &[std_resource_error_ty(&self.program.checked.resolved)],
        ) {
            return Err(vec![Diagnostic::new(
                expr.span,
                format!(
                    "internal error: resource scoped error type `{scoped_err_ty}` has no Resource(ResourceError) variant"
                ),
            )]);
        }
        if !self.enum_has_variant_with_payload(
            scoped_err_ty,
            "Body",
            std::slice::from_ref(body_err_ty),
        ) {
            return Err(vec![Diagnostic::new(
                expr.span,
                format!(
                    "internal error: resource scoped error type `{scoped_err_ty}` has no Body({body_err_ty}) variant"
                ),
            )]);
        }
        let body_layout = self.result_layout(&body_ret_ty, body_arg.span)?;

        self.line_indent(
            indent,
            &format!("{} = {{0}};", self.c_decl(&expr.ty, &result)),
        );
        self.line_indent(
            indent,
            &format!("{} = {{0}};", self.c_decl(&body_ret_ty, &body_result)),
        );
        match limits {
            Some(limits) => self.line_indent(
                indent,
                &format!(
                    "int32_t {push_rc} = ciel_resource_scope_push_limits_raw({limits}.max_resources, {limits}.max_child_owners, {limits}.max_pending_ops, {limits}.max_descriptors);"
                ),
            ),
            None => self.line_indent(
                indent,
                &format!("int32_t {push_rc} = ciel_resource_scope_push_default();"),
            ),
        }
        self.line_indent(indent, &format!("if ({push_rc} != 0) {{"));
        let push_error =
            self.resource_scoped_resource_literal(scoped_err_ty, &push_rc, expr.span)?;
        self.line_indent(
            indent + 1,
            &format!(
                "{result} = {};",
                self.result_err_from_error_literal(&layout, &push_error)
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done};"));
        self.line_indent(indent, "}");

        let body_call = self.callable_call_expr(&body_arg.ty, &body, &[])?;
        self.line_indent(indent, &format!("{body_result} = {body_call};"));
        if self.type_is_affine(&body_ret_ty) {
            let transfer_helper = self.resource_transfer_to_parent_name(&body_ret_ty);
            self.line_indent(
                indent,
                &format!("int32_t {transfer_rc} = {transfer_helper}(&{body_result});"),
            );
        } else {
            self.line_indent(indent, &format!("int32_t {transfer_rc} = 0;"));
        }
        self.line_indent(
            indent,
            &format!("int32_t {close_rc} = ciel_resource_scope_close_current();"),
        );
        self.line_indent(indent, &format!("if ({transfer_rc} != 0) {{"));
        self.line_indent(indent + 1, &format!("(void){close_rc};"));
        let body_cleanup = self.resource_cleanup_call(&body_ret_ty, &body_result);
        self.line_indent(indent + 1, &format!("{body_cleanup};"));
        let transfer_error =
            self.resource_scoped_resource_literal(scoped_err_ty, &transfer_rc, expr.span)?;
        self.line_indent(
            indent + 1,
            &format!(
                "{result} = {};",
                self.result_err_from_error_literal(&layout, &transfer_error)
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done};"));
        self.line_indent(indent, "}");
        self.line_indent(
            indent,
            &format!("if ({body_result}.tag == {}) {{", body_layout.ok_index),
        );
        let ok_value = body_layout
            .ok_has_payload
            .then(|| format!("{body_result}.as.{}._0", body_layout.ok_name));
        self.line_indent(indent + 1, &format!("if ({close_rc} != 0) {{"));
        self.line_indent(indent + 2, &format!("{body_cleanup};"));
        let close_error =
            self.resource_scoped_resource_literal(scoped_err_ty, &close_rc, expr.span)?;
        self.line_indent(
            indent + 2,
            &format!(
                "{result} = {};",
                self.result_err_from_error_literal(&layout, &close_error)
            ),
        );
        self.line_indent(indent + 2, &format!("goto {done};"));
        self.line_indent(indent + 1, "}");
        self.line_indent(
            indent + 1,
            &format!(
                "{result} = {};",
                self.result_ok_literal(&layout, ok_value.as_deref())
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done};"));
        self.line_indent(indent, "} else {");
        self.line_indent(indent + 1, &format!("(void){close_rc};"));
        let body_error = if body_layout.err_has_payload {
            let body_payload = format!("{body_result}.as.{}._0", body_layout.err_name);
            self.resource_scoped_body_literal(scoped_err_ty, Some(&body_payload), expr.span)?
        } else {
            self.resource_scoped_body_literal(scoped_err_ty, None, expr.span)?
        };
        self.line_indent(
            indent + 1,
            &format!(
                "{result} = {};",
                self.result_err_from_error_literal(&layout, &body_error)
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done};"));
        self.line_indent(indent, "}");
        self.line_indent(indent, &format!("{done}:;"));
        Ok(result)
    }

    pub(in crate::codegen) fn gen_defer_call(
        &mut self,
        expr: &TExpr,
        indent: usize,
    ) -> DiagResult<String> {
        self.with_temporary_resource_cleanup_scope(|this| {
            let TExprKind::Call { callee, args, .. } = &expr.kind else {
                return this.gen_expr_in_stmt(expr, indent);
            };
            let callee = this.gen_expr_in_stmt(callee, indent)?;
            let mut temp_args = Vec::new();
            for arg in args {
                if arg.ty.is_erased_value() {
                    let value = this.gen_expr_in_stmt(arg, indent)?;
                    this.line_indent(indent, &format!("(void)({value});"));
                    continue;
                }
                if let Some(ctx) = this.current_async_context.clone() {
                    this.current_async_defer_arg_index += 1;
                    let field = format!("{ctx}->defer_arg{}", this.current_async_defer_arg_index);
                    this.emit_expr_store(&field, arg, indent)?;
                    temp_args.push(field);
                } else {
                    let temp = this.emit_temp_value("defer_arg", arg, indent)?;
                    temp_args.push(temp);
                }
            }
            let call = format!("{callee}({})", temp_args.join(", "));
            if this.type_is_affine(&expr.ty) {
                let temp = this.next_temp("defer_return");
                let helper = this.resource_cleanup_name(&expr.ty);
                Ok(format!(
                    "do {{ {} = {call}; {helper}(&{temp}); }} while (0)",
                    this.c_decl(&expr.ty, &temp),
                ))
            } else {
                Ok(call)
            }
        })
    }
}
