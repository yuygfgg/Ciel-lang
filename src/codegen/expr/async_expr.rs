use super::*;

impl<'a> CGenerator<'a> {
    pub(super) fn emit_try_expr(
        &mut self,
        expr: &TExpr,
        inner: &TExpr,
        propagation: &TryPropagation,
        indent: usize,
    ) -> DiagResult<String> {
        let inner_layout = self.result_layout(&inner.ty, inner.span)?;
        let return_ty = self.current_return_ty.clone();
        let return_layout = self.result_layout(&return_ty, expr.span)?;
        let value = self.gen_expr_in_stmt(inner, indent)?;
        let temp = self.next_temp("try");
        self.line_indent(
            indent,
            &format!("{} {temp} = {value};", inner_layout.c_type),
        );
        self.line_indent(
            indent,
            &format!("if ({temp}.tag == {}) {{", inner_layout.err_index),
        );
        self.emit_all_defers(indent + 1);
        let err_payload = if matches!(propagation, TryPropagation::ErrorBox) {
            Some(self.emit_error_boxed_value(
                &format!("{temp}.as.{}._0", inner_layout.err_name),
                inner_layout.err_payload_ty.as_ref().ok_or_else(|| {
                    vec![Diagnostic::new(
                        expr.span,
                        "internal error: error-box `?` requires an Err payload",
                    )]
                })?,
                indent + 1,
                expr.span,
            )?)
        } else {
            None
        };
        let err_value = if let Some(err_payload) = err_payload.as_deref() {
            self.result_err_from_error_literal(&return_layout, err_payload)
        } else {
            self.result_err_literal(&return_layout, &inner_layout, &temp)
        };
        if let Some(out_raw) = self.current_async_output.clone() {
            self.emit_async_output_store(&return_ty, &out_raw, &err_value, indent + 1);
            self.line_indent(indent + 1, "return 0;");
        } else {
            self.line_indent(indent + 1, &format!("return {err_value};"));
        }
        self.line_indent(indent, "}");
        if expr.ty.is_erased_value() || !inner_layout.ok_has_payload {
            Ok("((void)0)".to_string())
        } else if self.type_is_affine(&expr.ty) {
            let ok_source = format!("{temp}.as.{}._0", inner_layout.ok_name);
            let ok_temp = self.next_temp("try_ok_move");
            self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &ok_temp)));
            self.emit_value_copy(&ok_temp, &ok_source, &expr.ty, indent);
            self.emit_resource_zero_expr(&expr.ty, &ok_source, indent);
            self.push_temporary_resource_cleanup_defer(&expr.ty, &ok_temp);
            Ok(ok_temp)
        } else {
            Ok(format!("{temp}.as.{}._0", inner_layout.ok_name))
        }
    }

    pub(super) fn emit_future_run(
        &mut self,
        future: &TExpr,
        output_ty: &Ty,
        prefix: &str,
        indent: usize,
    ) -> DiagResult<(String, String)> {
        let raw = self.next_temp(&format!("{prefix}_raw"));
        let await_index = if prefix == "await" && self.current_async_context.is_some() {
            self.current_async_await_index += 1;
            Some(self.current_async_await_index)
        } else {
            None
        };
        let async_ctx = self.current_async_context.clone();
        let async_await = async_ctx.is_some() && await_index.is_some();
        if let Some(await_index) = await_index {
            let cleanup_frames = self.async_cleanup_defer_stack();
            if let Some(case) = self.current_async_cleanup_cases.get_mut(await_index - 1) {
                *case = cleanup_frames;
            }
            self.line_indent(indent, &format!("ciel_async_resume_{await_index}:;"));
            self.line_indent(indent, &format!("CielFuture *{raw} = NULL;"));
            let ctx = async_ctx.as_ref().expect("async ctx exists");
            self.line_indent(
                indent,
                &format!("if ({ctx}->pc == {await_index} && {ctx}->active_future != NULL) {{"),
            );
            self.line_indent(indent + 1, &format!("{raw} = {ctx}->active_future;"));
            self.line_indent(indent, "} else {");
            let future_temp =
                self.emit_untracked_temp_value(&format!("{prefix}_future"), future, indent + 1)?;
            self.emit_awaitable_future_raw(&raw, &future_temp, &future.ty, output_ty, indent + 1)?;
            self.line_indent(indent + 1, &format!("{ctx}->pc = {await_index};"));
            self.line_indent(indent + 1, &format!("{ctx}->cleanup_state = {ctx}->pc;"));
            self.line_indent(indent + 1, &format!("{ctx}->active_future = {raw};"));
            self.line_indent(indent, "}");
        } else {
            let future_temp = self.emit_temp_value(&format!("{prefix}_future"), future, indent)?;
            self.line_indent(indent, &format!("CielFuture *{raw} = NULL;"));
            self.emit_awaitable_future_raw(&raw, &future_temp, &future.ty, output_ty, indent)?;
        }

        let output = if output_ty.is_erased_value() {
            None
        } else if let Some(await_index) = await_index {
            self.current_async_await_outputs
                .get(await_index - 1)
                .and_then(|slot| slot.as_ref().map(|(field, _)| field.clone()))
        } else {
            let output = self.next_temp(&format!("{prefix}_out"));
            self.line_indent(indent, &format!("{};", self.c_decl(output_ty, &output)));
            self.line_indent(indent, &format!("memset(&{output}, 0, sizeof({output}));"));
            Some(output)
        };
        let out_arg = output
            .as_ref()
            .map(|name| format!("&{name}"))
            .unwrap_or_else(|| "NULL".to_string());
        let rc = self.next_temp(&format!("{prefix}_rc"));
        let run_fn = if async_await {
            "ciel_future_poll_trampoline"
        } else {
            "ciel_future_run_to_completion_trampoline"
        };
        self.line_indent(
            indent,
            &format!("int32_t {rc} = {run_fn}({raw}, {out_arg});"),
        );
        if let Some(ctx) = async_ctx
            && prefix == "await"
        {
            self.line_indent(indent, &format!("if ({rc} == EAGAIN) {{"));
            self.line_indent(
                indent + 1,
                &format!("ciel_future_adopt_pending_operation(future, {raw});"),
            );
            self.line_indent(indent + 1, "return EAGAIN;");
            self.line_indent(indent, "}");
            self.line_indent(indent, &format!("{ctx}->active_future = NULL;"));
            self.line_indent(indent, &format!("{ctx}->pc = 0;"));
            self.line_indent(indent, &format!("{ctx}->cleanup_state = 0;"));
            self.line_indent(indent, "ciel_future_clear_pending_operation(future);");
        }
        Ok((output.unwrap_or_else(|| "((void)0)".to_string()), rc))
    }

    pub(super) fn emit_awaitable_future_raw(
        &mut self,
        raw: &str,
        future_temp: &str,
        future_ty: &Ty,
        output_ty: &Ty,
        indent: usize,
    ) -> DiagResult<()> {
        if generated_future_output_ty(future_ty)
            .as_ref()
            .is_some_and(|actual_output| actual_output == output_ty)
        {
            self.line_indent(
                indent,
                &format!("{raw} = (CielFuture *){future_temp}.handle;"),
            );
            return Ok(());
        }
        let impl_name = self.awaitable_future_impl_name(output_ty, future_ty)?;
        self.line_indent(
            indent,
            &format!("{raw} = (CielFuture *){impl_name}(&{future_temp});"),
        );
        Ok(())
    }

    pub(super) fn emit_task_await_output_clone(
        &mut self,
        output: &str,
        output_ty: &Ty,
        task_output_ty: &Ty,
        task_error_ty: &Ty,
        rc: &str,
        indent: usize,
        span: crate::span::Span,
    ) -> DiagResult<()> {
        let layout = self.result_layout(output_ty, span)?;
        let clone_ok = layout.ok_has_payload && !task_output_ty.is_erased_value();
        let clone_err = layout.err_has_payload && !task_error_ty.is_erased_value();
        if !clone_ok && !clone_err {
            return Ok(());
        }
        self.line_indent(indent, &format!("if ({rc} == 0) {{"));
        if clone_ok {
            self.line_indent(
                indent + 1,
                &format!("if ({output}.tag == {}) {{", layout.ok_index),
            );
            let ok_field = format!("{output}.as.{}._0", layout.ok_name);
            self.emit_task_await_payload_clone(
                output,
                &layout,
                task_output_ty,
                &ok_field,
                indent + 2,
                span,
            )?;
            if clone_err {
                self.line_indent(
                    indent + 1,
                    &format!("}} else if ({output}.tag == {}) {{", layout.err_index),
                );
            } else {
                self.line_indent(indent + 1, "}");
            }
        } else if clone_err {
            self.line_indent(
                indent + 1,
                &format!("if ({output}.tag == {}) {{", layout.err_index),
            );
        }
        if clone_err {
            let err_field = format!("{output}.as.{}._0", layout.err_name);
            self.emit_task_await_payload_clone(
                output,
                &layout,
                task_error_ty,
                &err_field,
                indent + 2,
                span,
            )?;
            self.line_indent(indent + 1, "}");
        }
        self.line_indent(indent, "}");
        Ok(())
    }

    fn emit_task_await_payload_clone(
        &mut self,
        output: &str,
        output_layout: &ResultLayout,
        payload_ty: &Ty,
        payload_field: &str,
        indent: usize,
        span: crate::span::Span,
    ) -> DiagResult<()> {
        let cloned = self.emit_task_boundary_clone_result_from_ptr(
            payload_ty,
            &format!("&{payload_field}"),
            indent,
            span,
        )?;
        let clone_layout = self.result_layout(
            &std_result_ty(
                &self.program.checked.resolved,
                payload_ty.clone(),
                std_error_ty(&self.program.checked.resolved),
            ),
            span,
        )?;
        self.line_indent(
            indent,
            &format!("if ({cloned}.tag == {}) {{", clone_layout.err_index),
        );
        let clone_error = format!("{cloned}.as.{}._0", clone_layout.err_name);
        let err_value =
            self.result_err_from_message_clone_literal(output_layout, &clone_error, span)?;
        self.line_indent(indent + 1, &format!("{output} = {err_value};"));
        self.line_indent(indent, "} else {");
        let cloned_payload = format!("{cloned}.as.{}._0", clone_layout.ok_name);
        self.emit_value_copy(payload_field, &cloned_payload, payload_ty, indent + 1);
        self.line_indent(indent, "}");
        Ok(())
    }

    pub(super) fn emit_future_failure_panic(
        &mut self,
        rc: &str,
        span: crate::span::Span,
        indent: usize,
    ) {
        let (file, line) = self.location_args(span);
        self.line_indent(indent, &format!("if ({rc} != 0) {{"));
        self.line_indent(
            indent + 1,
            &format!(
                "ciel_panic_at(\"future failed\", sizeof(\"future failed\") - 1, {file}, {line});"
            ),
        );
        self.line_indent(indent, "}");
    }

    pub(super) fn emit_async_error_return_from_rc(
        &mut self,
        rc: &str,
        span: crate::span::Span,
        indent: usize,
    ) -> DiagResult<()> {
        let return_ty = self.current_return_ty.clone();
        if let Some(out_raw) = self.current_async_output.clone() {
            self.line_indent(indent, &format!("if ({rc} != 0) {{"));
            self.emit_all_defers(indent + 1);
            if result_args(&self.program.checked.resolved, &return_ty).is_some() {
                let layout = self.result_layout(&return_ty, span)?;
                let err_value = self.result_err_from_runtime_literal(&layout, rc, span)?;
                self.emit_async_output_store(&return_ty, &out_raw, &err_value, indent + 1);
                self.line_indent(indent + 1, "return 0;");
            } else {
                self.line_indent(indent + 1, &format!("return {rc};"));
            }
            self.line_indent(indent, "}");
        } else {
            self.emit_future_failure_panic(rc, span, indent);
        }
        Ok(())
    }

    pub(super) fn emit_await_expr(
        &mut self,
        expr: &TExpr,
        future: &TExpr,
        indent: usize,
    ) -> DiagResult<String> {
        let (output, rc) = self.emit_future_run(future, &expr.ty, "await", indent)?;
        self.emit_async_error_return_from_rc(&rc, expr.span, indent)?;
        if let Some((task_output_ty, task_error_ty)) = self.task_tys_for_codegen(&future.ty) {
            self.emit_task_await_output_clone(
                &output,
                &expr.ty,
                &task_output_ty,
                &task_error_ty,
                &rc,
                indent,
                expr.span,
            )?;
        }
        Ok(output)
    }

    pub(super) fn emit_select_future_setup(
        &mut self,
        expr: &TExpr,
        raw: &str,
        set: &str,
        biased: bool,
        arms: &[TSelectArm],
        track_future_temps: bool,
        indent: usize,
    ) -> DiagResult<()> {
        let (file, line) = self.location_args(expr.span);
        self.line_indent(
            indent,
            &format!(
                "{set} = ciel_select_set_new({}, {});",
                arms.len(),
                if biased { 1 } else { 0 }
            ),
        );
        self.line_indent(indent, &format!("if ({set} == NULL) {{"));
        self.line_indent(
            indent + 1,
            &format!(
                "ciel_panic_at(\"select set allocation failed\", sizeof(\"select set allocation failed\") - 1, {file}, {line});"
            ),
        );
        self.line_indent(indent, "}");
        for arm in arms {
            let future_temp = if track_future_temps {
                self.emit_temp_value("select_arm_future", &arm.future, indent)?
            } else {
                self.emit_untracked_temp_value("select_arm_future", &arm.future, indent)?
            };
            let arm_raw = self.next_temp("select_arm_raw");
            self.line_indent(indent, &format!("CielFuture *{arm_raw} = NULL;"));
            self.emit_awaitable_future_raw(
                &arm_raw,
                &future_temp,
                &arm.future.ty,
                &arm.future_output_ty,
                indent,
            )?;
            let (size_expr, align_expr) = self.future_result_layout_args(&arm.future_output_ty);
            let push_rc = self.next_temp("select_push_rc");
            self.line_indent(
                indent,
                &format!(
                    "int32_t {push_rc} = ciel_select_set_push({set}, {arm_raw}, {size_expr}, {align_expr});"
                ),
            );
            self.line_indent(indent, &format!("if ({push_rc} != 0) {{"));
            self.line_indent(
                indent + 1,
                &format!(
                    "ciel_panic_at(\"select arm registration failed\", sizeof(\"select arm registration failed\") - 1, {file}, {line});"
                ),
            );
            self.line_indent(indent, "}");
        }
        self.line_indent(indent, &format!("{raw} = ciel_select_future_new({set});"));
        self.line_indent(indent, &format!("if ({raw} == NULL) {{"));
        self.line_indent(
            indent + 1,
            &format!(
                "ciel_panic_at(\"select future allocation failed\", sizeof(\"select future allocation failed\") - 1, {file}, {line});"
            ),
        );
        self.line_indent(indent, "}");
        Ok(())
    }

    pub(super) fn emit_select_arm_binding(
        &mut self,
        arm: &TSelectArm,
        set: &str,
        index: usize,
        rc: &str,
        indent: usize,
        span: crate::span::Span,
    ) -> DiagResult<()> {
        if arm.future_output_ty.is_erased_value() {
            return Ok(());
        }
        let cname = self.local_c_name(arm.binding_local, &arm.binding_name);
        let value_ptr = self.next_temp("select_arm_value");
        self.line_indent(
            indent,
            &format!(
                "{} {value_ptr} = ({})ciel_select_winner_value({set}, {index});",
                self.c_pointer_type(&arm.future_output_ty),
                self.c_pointer_type(&arm.future_output_ty)
            ),
        );
        let (file, line) = self.location_args(span);
        self.line_indent(indent, &format!("if ({value_ptr} == NULL) {{"));
        self.line_indent(
            indent + 1,
            &format!(
                "ciel_panic_at(\"select winner value missing\", sizeof(\"select winner value missing\") - 1, {file}, {line});"
            ),
        );
        self.line_indent(indent, "}");
        let binding_expr = if self.local_is_async_frame(arm.binding_local) {
            if self.local_is_heap(arm.binding_local) {
                self.line_indent(
                    indent,
                    &format!(
                        "{cname} = ({}){};",
                        self.c_pointer_type(&arm.future_output_ty),
                        self.c_object_alloc_expr(&arm.future_output_ty)
                    ),
                );
                format!("(*{cname})")
            } else {
                cname.clone()
            }
        } else if self.local_is_heap(arm.binding_local) {
            self.line_indent(
                indent,
                &format!(
                    "{} = ({}){};",
                    self.c_pointer_decl(&arm.future_output_ty, &cname),
                    self.c_pointer_type(&arm.future_output_ty),
                    self.c_object_alloc_expr(&arm.future_output_ty)
                ),
            );
            format!("(*{cname})")
        } else {
            self.line_indent(
                indent,
                &format!("{};", self.c_decl(&arm.future_output_ty, &cname)),
            );
            cname.clone()
        };
        self.emit_value_copy(
            &binding_expr,
            &format!("*{value_ptr}"),
            &arm.future_output_ty,
            indent,
        );
        if self.type_is_affine(&arm.future_output_ty) {
            self.emit_resource_zero_expr(&arm.future_output_ty, &format!("*{value_ptr}"), indent);
        }
        self.push_resource_cleanup_defer(&arm.future_output_ty, &binding_expr);
        if let Some((task_output_ty, task_error_ty)) = self.task_tys_for_codegen(&arm.future.ty) {
            self.emit_task_await_output_clone(
                &binding_expr,
                &arm.future_output_ty,
                &task_output_ty,
                &task_error_ty,
                rc,
                indent,
                span,
            )?;
        }
        Ok(())
    }

    pub(super) fn emit_async_select_expr(
        &mut self,
        expr: &TExpr,
        biased: bool,
        arms: &[TSelectArm],
        indent: usize,
    ) -> DiagResult<String> {
        let result_temp = if expr.ty.is_erased_value() {
            None
        } else {
            let temp = self.next_temp("select_result");
            self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &temp)));
            self.line_indent(indent, &format!("memset(&{temp}, 0, sizeof({temp}));"));
            Some(temp)
        };
        let raw = self.next_temp("select_raw");
        let set = self.next_temp("select_set");
        let await_index = if self.current_async_context.is_some() {
            self.current_async_await_index += 1;
            Some(self.current_async_await_index)
        } else {
            None
        };
        let async_ctx = self.current_async_context.clone();
        let async_await = async_ctx.is_some() && await_index.is_some();
        if let Some(await_index) = await_index {
            let cleanup_frames = self.async_cleanup_defer_stack();
            if let Some(case) = self.current_async_cleanup_cases.get_mut(await_index - 1) {
                *case = cleanup_frames;
            }
            self.line_indent(indent, &format!("ciel_async_resume_{await_index}:;"));
            self.line_indent(indent, &format!("CielFuture *{raw} = NULL;"));
            self.line_indent(indent, &format!("CielSelectSet *{set} = NULL;"));
            let ctx = async_ctx.as_ref().expect("async ctx exists");
            self.line_indent(
                indent,
                &format!("if ({ctx}->pc == {await_index} && {ctx}->active_future != NULL) {{"),
            );
            self.line_indent(indent + 1, &format!("{raw} = {ctx}->active_future;"));
            self.line_indent(
                indent + 1,
                &format!("{set} = ciel_select_future_set({raw});"),
            );
            self.line_indent(indent, "} else {");
            self.emit_select_future_setup(expr, &raw, &set, biased, arms, false, indent + 1)?;
            self.line_indent(indent + 1, &format!("{ctx}->pc = {await_index};"));
            self.line_indent(indent + 1, &format!("{ctx}->cleanup_state = {ctx}->pc;"));
            self.line_indent(indent + 1, &format!("{ctx}->active_future = {raw};"));
            self.line_indent(indent, "}");
        } else {
            self.line_indent(indent, &format!("CielFuture *{raw} = NULL;"));
            self.line_indent(indent, &format!("CielSelectSet *{set} = NULL;"));
            self.emit_select_future_setup(expr, &raw, &set, biased, arms, true, indent)?;
        }

        let select_out = self.next_temp("select_out");
        self.line_indent(
            indent,
            &format!("CielSelectResult {select_out} = (CielSelectResult){{0}};"),
        );
        let rc = self.next_temp("select_rc");
        let run_fn = if async_await {
            "ciel_future_poll_trampoline"
        } else {
            "ciel_future_run_to_completion_trampoline"
        };
        self.line_indent(
            indent,
            &format!("int32_t {rc} = {run_fn}({raw}, &{select_out});"),
        );
        if let Some(ctx) = async_ctx {
            self.line_indent(indent, &format!("if ({rc} == EAGAIN) {{"));
            self.line_indent(
                indent + 1,
                &format!("ciel_future_adopt_pending_operation(future, {raw});"),
            );
            self.line_indent(indent + 1, "return EAGAIN;");
            self.line_indent(indent, "}");
            self.line_indent(indent, &format!("{ctx}->active_future = NULL;"));
            self.line_indent(indent, &format!("{ctx}->pc = 0;"));
            self.line_indent(indent, &format!("{ctx}->cleanup_state = 0;"));
            self.line_indent(indent, "ciel_future_clear_pending_operation(future);");
        }
        self.emit_async_error_return_from_rc(&rc, expr.span, indent)?;
        self.line_indent(indent, &format!("switch ({select_out}.index) {{"));
        for (index, arm) in arms.iter().enumerate() {
            self.line_indent(indent + 1, &format!("case {index}: {{"));
            self.defer_stack.push(Vec::new());
            self.emit_select_arm_binding(arm, &set, index, &rc, indent + 2, expr.span)?;
            if let Some(result_temp) = result_temp.as_ref() {
                self.emit_expr_store(result_temp, &arm.body, indent + 2)?;
            } else {
                let value = self.gen_expr_in_stmt(&arm.body, indent + 2)?;
                self.line_indent(indent + 2, &format!("(void)({value});"));
            }
            self.emit_current_defers(indent + 2);
            self.defer_stack.pop();
            self.line_indent(indent + 2, "break;");
            self.line_indent(indent + 1, "}");
        }
        let (file, line) = self.location_args(expr.span);
        self.line_indent(indent + 1, "default:");
        self.line_indent(
            indent + 2,
            &format!(
                "ciel_panic_at(\"invalid select winner\", sizeof(\"invalid select winner\") - 1, {file}, {line});"
            ),
        );
        self.line_indent(indent + 2, "break;");
        self.line_indent(indent, "}");
        Ok(result_temp.unwrap_or_else(|| "((void)0)".to_string()))
    }

    pub(super) fn emit_async_block_on_expr(
        &mut self,
        expr: &TExpr,
        future: &TExpr,
        indent: usize,
    ) -> DiagResult<String> {
        let (output, rc) = self.emit_future_run(future, &expr.ty, "block_on", indent)?;
        if result_args(&self.program.checked.resolved, &expr.ty).is_some()
            && !expr.ty.is_erased_value()
        {
            self.line_indent(indent, &format!("if ({rc} != 0) {{"));
            let layout = self.result_layout(&expr.ty, expr.span)?;
            let err_value = self.result_err_from_runtime_literal(&layout, &rc, expr.span)?;
            self.line_indent(indent + 1, &format!("{output} = {err_value};"));
            self.line_indent(indent, "}");
        } else {
            self.emit_future_failure_panic(&rc, expr.span, indent);
        }
        if let Some((task_output_ty, task_error_ty)) = self.task_tys_for_codegen(&future.ty) {
            self.emit_task_await_output_clone(
                &output,
                &expr.ty,
                &task_output_ty,
                &task_error_ty,
                &rc,
                indent,
                expr.span,
            )?;
        }
        Ok(output)
    }

    pub(super) fn emit_async_sleep_expr(
        &mut self,
        expr: &TExpr,
        ms: &TExpr,
        output_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let ms_value = self.gen_expr_in_stmt(ms, indent)?;
        let ctx_name = self.async_sleep_context_name(output_ty);
        let run_name = self.async_sleep_run_name(output_ty);
        let cleanup_name = self.async_sleep_cleanup_name(output_ty);
        let ctx = self.next_temp("sleep_ctx");
        self.line_indent(
            indent,
            &format!("{ctx_name} *{ctx} = ({ctx_name} *)ciel_alloc(sizeof({ctx_name}));"),
        );
        self.line_indent(indent, &format!("memset({ctx}, 0, sizeof(*{ctx}));"));
        self.line_indent(indent, &format!("{ctx}->op = NULL;"));
        self.line_indent(indent, &format!("{ctx}->ms = (uint64_t)({ms_value});"));
        let raw = self.next_temp("sleep_future");
        let (size_expr, align_expr) = self.future_result_layout_args(output_ty);
        self.line_indent(
            indent,
            &format!(
                "CielFuture *{raw} = ciel_future_new({size_expr}, {align_expr}, {run_name}, {ctx}, {cleanup_name});"
            ),
        );
        let (file, line) = self.location_args(expr.span);
        self.line_indent(indent, &format!("if ({raw} == NULL) {{"));
        self.line_indent(
            indent + 1,
            &format!(
                "ciel_panic_at(\"future allocation failed\", sizeof(\"future allocation failed\") - 1, {file}, {line});"
            ),
        );
        self.line_indent(indent, "}");
        Ok(format!(
            "({}){{ .handle = (void *){raw} }}",
            self.c_type(&expr.ty)
        ))
    }

    pub(super) fn emit_async_spawn_expr(
        &mut self,
        expr: &TExpr,
        body: &TExpr,
        task_output_ty: &Ty,
        task_error_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let result_layout = self.result_layout(&expr.ty, expr.span)?;
        let result_temp = self.next_temp("task_spawn_result");
        let done_label = self.next_temp("task_spawn_done");
        self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &result_temp)));
        let result_output_ty = std_result_ty(
            &self.program.checked.resolved,
            task_output_ty.clone(),
            task_error_ty.clone(),
        );

        let raw_future = if let TExprKind::Closure { is_async, .. } = &body.kind {
            if !*is_async {
                return Err(vec![Diagnostic::new(
                    body.span,
                    "internal error: task spawn closure is not async",
                )]);
            }
            let closure = self.async_closure_def_for_expr(body)?;
            self.emit_async_closure_future_from_parts(
                &closure,
                None,
                AsyncClosureCaptureInit::CloneForTask,
                Some(&result_temp),
                Some(&result_layout),
                Some(&done_label),
                &result_output_ty,
                indent,
            )?
        } else {
            let future_temp = self.emit_temp_value("task_body_future", body, indent)?;
            let raw = self.next_temp("task_body_raw");
            self.line_indent(indent, &format!("CielFuture *{raw} = NULL;"));
            self.emit_awaitable_future_raw(
                &raw,
                &future_temp,
                &body.ty,
                &result_output_ty,
                indent,
            )?;
            raw
        };

        let raw_task = self.next_temp("task_raw");
        self.line_indent(
            indent,
            &format!("void *{raw_task} = ciel_task_spawn({raw_future});"),
        );
        self.line_indent(indent, &format!("if ({raw_task} == NULL) {{"));
        self.line_indent(
            indent + 1,
            &format!(
                "{result_temp} = {};",
                self.result_err_from_runtime_literal(
                    &result_layout,
                    "errno == 0 ? EIO : errno",
                    expr.span
                )?
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done_label};"));
        self.line_indent(indent, "}");
        let task_ty = std_task_ty(
            &self.program.checked.resolved,
            task_output_ty.clone(),
            task_error_ty.clone(),
        );
        let task_value = format!(
            "({}){{ .handle = (void *){raw_task} }}",
            self.c_type(&task_ty)
        );
        self.line_indent(
            indent,
            &format!(
                "{result_temp} = {};",
                self.result_ok_literal(&result_layout, Some(&task_value))
            ),
        );
        self.line_indent(indent, &format!("{done_label}:;"));
        Ok(result_temp)
    }

    pub(super) fn async_closure_def_for_expr(&self, expr: &TExpr) -> DiagResult<ClosureDef> {
        let TExprKind::Closure { id, .. } = &expr.kind else {
            return Err(vec![Diagnostic::new(
                expr.span,
                "internal error: expected async closure expression",
            )]);
        };
        let owner = self.current_closure_owner.ok_or_else(|| {
            vec![Diagnostic::new(
                expr.span,
                "internal error: async closure emitted outside a function",
            )]
        })?;
        self.plan
            .closure_defs
            .get(&(owner.0, *id))
            .cloned()
            .ok_or_else(|| {
                vec![Diagnostic::new(
                    expr.span,
                    "internal error: async closure definition was not planned",
                )]
            })
    }

    pub(super) fn emit_async_task_cancel_expr(
        &mut self,
        expr: &TExpr,
        task: &TExpr,
        indent: usize,
    ) -> DiagResult<String> {
        let result_layout = self.result_layout(&expr.ty, expr.span)?;
        let result_temp = self.next_temp("task_cancel_result");
        let done_label = self.next_temp("task_cancel_done");
        self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &result_temp)));
        let task_ptr = self.gen_expr_in_stmt(task, indent)?;
        let rc = self.next_temp("task_cancel_rc");
        self.line_indent(
            indent,
            &format!("int32_t {rc} = ciel_task_cancel(({task_ptr})->handle);"),
        );
        self.emit_async_runtime_result_from_rc(
            &result_temp,
            &result_layout,
            &rc,
            &done_label,
            indent,
            expr.span,
        )?;
        self.line_indent(
            indent,
            &format!(
                "{result_temp} = {};",
                self.result_ok_literal(&result_layout, None)
            ),
        );
        self.line_indent(indent, &format!("{done_label}:;"));
        Ok(result_temp)
    }

    pub(super) fn emit_async_task_is_finished_expr(
        &mut self,
        expr: &TExpr,
        task: &TExpr,
        indent: usize,
    ) -> DiagResult<String> {
        let result_layout = self.result_layout(&expr.ty, expr.span)?;
        let result_temp = self.next_temp("task_finished_result");
        let done_label = self.next_temp("task_finished_done");
        let finished = self.next_temp("task_finished");
        self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &result_temp)));
        self.line_indent(indent, &format!("bool {finished} = false;"));
        let task_ptr = self.gen_expr_in_stmt(task, indent)?;
        let rc = self.next_temp("task_finished_rc");
        self.line_indent(
            indent,
            &format!("int32_t {rc} = ciel_task_is_finished(({task_ptr})->handle, &{finished});"),
        );
        self.emit_async_runtime_result_from_rc(
            &result_temp,
            &result_layout,
            &rc,
            &done_label,
            indent,
            expr.span,
        )?;
        self.line_indent(
            indent,
            &format!(
                "{result_temp} = {};",
                self.result_ok_literal(&result_layout, Some(&finished))
            ),
        );
        self.line_indent(indent, &format!("{done_label}:;"));
        Ok(result_temp)
    }
}
