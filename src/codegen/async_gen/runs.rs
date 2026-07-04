use super::*;

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn emit_async_sleep_future_runs(&mut self) -> DiagResult<()> {
        for output_ty in self
            .plan
            .async_sleep_output_tys
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            let ctx_name = self.async_sleep_context_name(&output_ty);
            let run_name = self.async_sleep_run_name(&output_ty);
            let cleanup_name = self.async_sleep_cleanup_name(&output_ty);
            let span = crate::span::Span::new(crate::span::FileId(0), 0, 0);
            let layout = self.result_layout(&output_ty, span)?;
            self.line(&format!(
                "static int32_t {run_name}(void *ctx_raw, void *out_raw) {{"
            ));
            self.line_indent(1, &format!("{ctx_name} *ctx = ({ctx_name} *)ctx_raw;"));
            self.line_indent(
                1,
                &format!(
                    "{} *out = ({})out_raw;",
                    layout.c_type,
                    self.c_pointer_type(&output_ty)
                ),
            );
            self.line_indent(
                1,
                "int32_t rc = ciel_future_await_sleep_ms(ctx->future, &ctx->op, ctx->ms);",
            );
            self.line_indent(1, "if (rc == EAGAIN) {");
            self.line_indent(2, "return EAGAIN;");
            self.line_indent(1, "}");
            self.line_indent(1, "if (rc == 0) {");
            self.line_indent(
                2,
                &format!("*out = {};", self.result_ok_literal(&layout, None)),
            );
            self.line_indent(1, "} else {");
            let error_literal = self.async_error_runtime_literal("rc", span)?;
            self.line_indent(
                2,
                &format!(
                    "*out = {};",
                    self.result_err_from_error_literal(&layout, &error_literal)
                ),
            );
            self.line_indent(1, "}");
            self.line_indent(1, "return 0;");
            self.line("}");
            self.line(&format!(
                "static void {cleanup_name}(void *ctx_raw, int32_t reason) {{"
            ));
            self.line_indent(1, &format!("{ctx_name} *ctx = ({ctx_name} *)ctx_raw;"));
            self.line_indent(1, "(void)reason;");
            self.line_indent(1, "if (ctx == NULL || ctx->op == NULL) return;");
            self.line_indent(1, "(void)ciel_async_cancel(ctx->op);");
            self.line_indent(1, "ctx->op = NULL;");
            self.line("}");
        }
        Ok(())
    }

    pub(in crate::codegen) fn emit_async_op_future_runs(&mut self) -> DiagResult<()> {
        for context in self
            .plan
            .async_op_contexts
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            let span = crate::span::Span::new(crate::span::FileId(0), 0, 0);
            let ctx_name = self.async_op_context_name(&context.op_ty, &context.output_ty);
            let run_name = self.async_op_run_name(&context.op_ty, &context.output_ty);
            let cleanup_name = self.async_op_cleanup_name(&context.op_ty, &context.output_ty);
            let result_ty = std_result_ty(context.output_ty.clone(), std_async_error_ty());
            let layout = self.result_layout(&result_ty, span)?;
            let raw_impl = self.async_op_impl_name(
                context.raw_operation_def,
                "raw_operation",
                &[],
                &context.op_ty,
            )?;
            let poll_impl = self.async_op_impl_name(
                context.poll_done_def,
                "poll_done",
                std::slice::from_ref(&context.output_ty),
                &context.op_ty,
            )?;
            let op_cleanup = self.resource_cleanup_call(&context.op_ty, "ctx->op_value");
            self.line(&format!(
                "static int32_t {run_name}(void *ctx_raw, void *out_raw) {{"
            ));
            self.line_indent(1, &format!("{ctx_name} *ctx = ({ctx_name} *)ctx_raw;"));
            self.line_indent(
                1,
                &format!(
                    "{} *out = ({})out_raw;",
                    layout.c_type,
                    self.c_pointer_type(&result_ty)
                ),
            );
            self.line_indent(1, "if (ctx->op == NULL) {");
            self.line_indent(2, &format!("void *raw = {raw_impl}(&ctx->op_value);"));
            self.line_indent(2, "if (raw == NULL) {");
            self.line_indent(3, &format!("{op_cleanup};"));
            self.line_indent(
                3,
                &format!(
                    "*out = {};",
                    self.result_err_from_error_literal(
                        &layout,
                        &self.async_error_runtime_literal("EIO", span)?
                    )
                ),
            );
            self.line_indent(3, "return 0;");
            self.line_indent(2, "}");
            self.line_indent(2, "ctx->op = (CielAsyncOp *)raw;");
            self.line_indent(2, "ciel_future_bind_operation(ctx->future, ctx->op);");
            self.line_indent(1, "}");
            let poll_value = if context.output_ty.is_erased_value() {
                self.line_indent(
                    1,
                    &format!("int32_t rc = {poll_impl}(&ctx->op_value, NULL);"),
                );
                None
            } else {
                let value = self.next_temp("async_op_value");
                self.line_indent(1, &format!("{};", self.c_decl(&context.output_ty, &value)));
                self.line_indent(1, &format!("memset(&{value}, 0, sizeof({value}));"));
                self.line_indent(
                    1,
                    &format!("int32_t rc = {poll_impl}(&ctx->op_value, &{value});"),
                );
                Some(value)
            };
            self.line_indent(1, "if (rc == EAGAIN) {");
            self.line_indent(2, "return EAGAIN;");
            self.line_indent(1, "}");
            self.line_indent(1, "ciel_future_clear_operation(ctx->future, ctx->op);");
            self.line_indent(1, "ctx->op = NULL;");
            self.line_indent(1, &format!("{op_cleanup};"));
            self.line_indent(1, "if (rc == 0) {");
            if let Some(value) = poll_value.as_deref() {
                self.line_indent(
                    2,
                    &format!("*out = {};", self.result_ok_literal(&layout, Some(value))),
                );
            } else {
                self.line_indent(
                    2,
                    &format!("*out = {};", self.result_ok_literal(&layout, None)),
                );
            }
            self.line_indent(1, "} else {");
            let error_literal = self.async_error_runtime_literal("rc", span)?;
            self.line_indent(
                2,
                &format!(
                    "*out = {};",
                    self.result_err_from_error_literal(&layout, &error_literal)
                ),
            );
            self.line_indent(1, "}");
            self.line_indent(1, "return 0;");
            self.line("}");
            self.line(&format!(
                "static void {cleanup_name}(void *ctx_raw, int32_t reason) {{"
            ));
            self.line_indent(1, &format!("{ctx_name} *ctx = ({ctx_name} *)ctx_raw;"));
            self.line_indent(1, "(void)reason;");
            self.line_indent(1, "if (ctx == NULL) return;");
            self.line_indent(1, "if (ctx->op != NULL) {");
            self.line_indent(2, "(void)ciel_async_cancel(ctx->op);");
            self.line_indent(2, "ctx->op = NULL;");
            self.line_indent(1, "}");
            self.line_indent(1, &format!("{op_cleanup};"));
            self.line("}");
        }
        Ok(())
    }

    pub(in crate::codegen) fn emit_async_channel_future_runs(&mut self) -> DiagResult<()> {
        let span = crate::span::Span::new(crate::span::FileId(0), 0, 0);
        for payload_ty in self
            .plan
            .async_channel_send_payload_tys
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            let result_ty = std_result_ty(Ty::Void, std_async_error_ty());
            let layout = self.result_layout(&result_ty, span)?;
            let ctx_name = self.async_channel_send_context_name(&payload_ty);
            let run_name = self.async_channel_send_run_name(&payload_ty);
            let cleanup_name = self.async_channel_send_cleanup_name(&payload_ty);
            self.line(&format!(
                "static int32_t {run_name}(void *ctx_raw, void *out_raw) {{"
            ));
            self.line_indent(1, &format!("{ctx_name} *ctx = ({ctx_name} *)ctx_raw;"));
            self.line_indent(
                1,
                &format!(
                    "{} *out = ({})out_raw;",
                    layout.c_type,
                    self.c_pointer_type(&result_ty)
                ),
            );
            self.line_indent(1, "if (ctx->init_failed) {");
            let init_error = self.async_error_message_clone_literal("ctx->init_error", span)?;
            self.line_indent(
                2,
                &format!(
                    "*out = {};",
                    self.result_err_from_error_literal(&layout, &init_error)
                ),
            );
            self.line_indent(2, "return 0;");
            self.line_indent(1, "}");
            let value_ptr = if payload_ty.is_erased_value() {
                "NULL".to_string()
            } else {
                "&ctx->value".to_string()
            };
            self.line_indent(
                1,
                &format!(
                    "int32_t rc = ciel_async_channel_send_poll(ctx->future, (CielAsyncSender *)ctx->sender, {value_ptr});"
                ),
            );
            self.line_indent(1, "if (rc == EAGAIN) return EAGAIN;");
            self.line_indent(1, "if (rc == 0) {");
            self.line_indent(
                2,
                &format!("*out = {};", self.result_ok_literal(&layout, None)),
            );
            self.line_indent(1, "} else {");
            let error_literal = self.async_error_channel_or_runtime_literal("rc", span)?;
            self.line_indent(
                2,
                &format!(
                    "*out = {};",
                    self.result_err_from_error_literal(&layout, &error_literal)
                ),
            );
            self.line_indent(1, "}");
            self.line_indent(1, "return 0;");
            self.line("}");
            self.line(&format!(
                "static void {cleanup_name}(void *ctx_raw, int32_t reason) {{"
            ));
            self.line_indent(1, "(void)ctx_raw;");
            self.line_indent(1, "(void)reason;");
            self.line("}");
        }

        for payload_ty in self
            .plan
            .async_channel_reserve_payload_tys
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            let permit_ty = std_send_permit_ty(payload_ty.clone());
            let result_ty = std_result_ty(permit_ty.clone(), std_async_error_ty());
            let layout = self.result_layout(&result_ty, span)?;
            let ctx_name = self.async_channel_reserve_context_name(&payload_ty);
            let run_name = self.async_channel_reserve_run_name(&payload_ty);
            let cleanup_name = self.async_channel_reserve_cleanup_name(&payload_ty);
            self.line(&format!(
                "static int32_t {run_name}(void *ctx_raw, void *out_raw) {{"
            ));
            self.line_indent(1, &format!("{ctx_name} *ctx = ({ctx_name} *)ctx_raw;"));
            self.line_indent(
                1,
                &format!(
                    "{} *out = ({})out_raw;",
                    layout.c_type,
                    self.c_pointer_type(&result_ty)
                ),
            );
            self.line_indent(1, "CielAsyncSendPermit *permit = NULL;");
            self.line_indent(
                1,
                "int32_t rc = ciel_async_channel_reserve_poll(ctx->future, (CielAsyncSender *)ctx->sender, &permit);",
            );
            self.line_indent(1, "if (rc == EAGAIN) return EAGAIN;");
            self.line_indent(1, "if (rc == 0) {");
            let permit_value = format!(
                "({}){{ .handle = (void *)permit }}",
                self.c_type(&permit_ty)
            );
            self.line_indent(
                2,
                &format!(
                    "*out = {};",
                    self.result_ok_literal(&layout, Some(&permit_value))
                ),
            );
            self.line_indent(1, "} else {");
            let error_literal = self.async_error_channel_or_runtime_literal("rc", span)?;
            self.line_indent(
                2,
                &format!(
                    "*out = {};",
                    self.result_err_from_error_literal(&layout, &error_literal)
                ),
            );
            self.line_indent(1, "}");
            self.line_indent(1, "return 0;");
            self.line("}");
            self.line(&format!(
                "static void {cleanup_name}(void *ctx_raw, int32_t reason) {{"
            ));
            self.line_indent(1, "(void)ctx_raw;");
            self.line_indent(1, "(void)reason;");
            self.line("}");
        }

        for payload_ty in self
            .plan
            .async_channel_recv_payload_tys
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            let result_ty = std_result_ty(payload_ty.clone(), std_async_error_ty());
            let layout = self.result_layout(&result_ty, span)?;
            let ctx_name = self.async_channel_recv_context_name(&payload_ty);
            let run_name = self.async_channel_recv_run_name(&payload_ty);
            let cleanup_name = self.async_channel_recv_cleanup_name(&payload_ty);
            self.line(&format!(
                "static int32_t {run_name}(void *ctx_raw, void *out_raw) {{"
            ));
            self.line_indent(1, &format!("{ctx_name} *ctx = ({ctx_name} *)ctx_raw;"));
            self.line_indent(
                1,
                &format!(
                    "{} *out = ({})out_raw;",
                    layout.c_type,
                    self.c_pointer_type(&result_ty)
                ),
            );
            let value = if payload_ty.is_erased_value() {
                None
            } else {
                let value = "value";
                self.line_indent(1, &format!("{};", self.c_decl(&payload_ty, value)));
                self.line_indent(1, &format!("memset(&{value}, 0, sizeof({value}));"));
                Some(value)
            };
            let out_ptr = value
                .map(|value| format!("&{value}"))
                .unwrap_or_else(|| "NULL".to_string());
            self.line_indent(
                1,
                &format!(
                    "int32_t rc = ciel_async_channel_recv_poll(ctx->future, (CielAsyncReceiver *)ctx->receiver, {out_ptr});"
                ),
            );
            self.line_indent(1, "if (rc == EAGAIN) return EAGAIN;");
            self.line_indent(1, "if (rc == 0) {");
            if let Some(value) = value {
                self.line_indent(
                    2,
                    &format!("*out = {};", self.result_ok_literal(&layout, Some(value))),
                );
            } else {
                self.line_indent(
                    2,
                    &format!("*out = {};", self.result_ok_literal(&layout, None)),
                );
            }
            self.line_indent(1, "} else {");
            let error_literal = self.async_error_channel_or_runtime_literal("rc", span)?;
            self.line_indent(
                2,
                &format!(
                    "*out = {};",
                    self.result_err_from_error_literal(&layout, &error_literal)
                ),
            );
            self.line_indent(1, "}");
            self.line_indent(1, "return 0;");
            self.line("}");
            self.line(&format!(
                "static void {cleanup_name}(void *ctx_raw, int32_t reason) {{"
            ));
            self.line_indent(1, "(void)ctx_raw;");
            self.line_indent(1, "(void)reason;");
            self.line("}");
        }
        Ok(())
    }

    pub(in crate::codegen) fn emit_async_task_group_future_runs(&mut self) -> DiagResult<()> {
        let span = crate::span::Span::new(crate::span::FileId(0), 0, 0);
        for payload_ty in self
            .plan
            .async_task_group_next_payload_tys
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            let task_ty = std_task_ty(payload_ty.clone());
            let result_ty = std_result_ty(task_ty.clone(), std_async_error_ty());
            let layout = self.result_layout(&result_ty, span)?;
            let ctx_name = self.async_task_group_next_context_name(&payload_ty);
            let run_name = self.async_task_group_next_run_name(&payload_ty);
            let cleanup_name = self.async_task_group_next_cleanup_name(&payload_ty);
            self.line(&format!(
                "static int32_t {run_name}(void *ctx_raw, void *out_raw) {{"
            ));
            self.line_indent(1, &format!("{ctx_name} *ctx = ({ctx_name} *)ctx_raw;"));
            self.line_indent(
                1,
                &format!(
                    "{} *out = ({})out_raw;",
                    layout.c_type,
                    self.c_pointer_type(&result_ty)
                ),
            );
            self.line_indent(1, "void *task = NULL;");
            self.line_indent(
                1,
                "int32_t rc = ciel_task_group_next_task_poll(ctx->future, (CielTaskGroup *)ctx->group, &task);",
            );
            self.line_indent(1, "if (rc == EAGAIN) return EAGAIN;");
            self.line_indent(1, "if (rc == 0) {");
            let task_value = format!("({}){{ .handle = task }}", self.c_type(&task_ty));
            self.line_indent(
                2,
                &format!(
                    "*out = {};",
                    self.result_ok_literal(&layout, Some(&task_value))
                ),
            );
            self.line_indent(1, "} else {");
            let error_literal = self.result_err_from_runtime_literal(&layout, "rc", span)?;
            self.line_indent(2, &format!("*out = {error_literal};"));
            self.line_indent(1, "}");
            self.line_indent(1, "return 0;");
            self.line("}");
            self.line(&format!(
                "static void {cleanup_name}(void *ctx_raw, int32_t reason) {{"
            ));
            self.line_indent(1, "(void)ctx_raw;");
            self.line_indent(1, "(void)reason;");
            self.line("}");
        }
        Ok(())
    }
}
