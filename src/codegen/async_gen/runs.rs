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
                "static int32_t {run_name}(CielFuture *future, void *ctx_raw, void *out_raw) {{"
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
                "int32_t rc = ciel_future_await_sleep_ms(future, &ctx->op, ctx->ms);",
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
                "static void {cleanup_name}(CielFuture *future, void *ctx_raw, int32_t reason) {{"
            ));
            self.line_indent(1, &format!("{ctx_name} *ctx = ({ctx_name} *)ctx_raw;"));
            self.line_indent(1, "(void)future;");
            self.line_indent(1, "(void)reason;");
            self.line_indent(1, "if (ctx == NULL || ctx->op == NULL) return;");
            self.line_indent(1, "(void)ciel_async_cancel(ctx->op);");
            self.line_indent(1, "ctx->op = NULL;");
            self.line("}");
        }
        Ok(())
    }
}
