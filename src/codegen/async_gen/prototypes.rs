use super::*;

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn emit_async_function_run_prototypes(&mut self) {
        for function in &self.program.checked.functions {
            if !function.is_async || function.body.is_none() {
                continue;
            }
            let names = self.async_function_names(function.def_id);
            self.line(&format!(
                "static int32_t {}(void *ctx_raw, void *out_raw);",
                names.run
            ));
            self.line(&format!(
                "static void {}(void *ctx_raw, int32_t reason);",
                names.cleanup
            ));
        }
    }

    pub(in crate::codegen) fn emit_async_closure_run_prototypes(&mut self) {
        let closures = self.plan.closure_defs.clone();
        for closure in closures.values().filter(|closure| closure.is_async) {
            let names = self.async_closure_names(closure);
            self.line(&format!(
                "static int32_t {}(void *ctx_raw, void *out_raw);",
                names.run
            ));
            self.line(&format!(
                "static void {}(void *ctx_raw, int32_t reason);",
                names.cleanup
            ));
        }
    }

    pub(in crate::codegen) fn emit_async_sleep_future_prototypes(&mut self) {
        for output_ty in self
            .plan
            .async_sleep_output_tys
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            self.line(&format!(
                "static int32_t {}(void *ctx_raw, void *out_raw);",
                self.async_sleep_run_name(&output_ty)
            ));
            self.line(&format!(
                "static void {}(void *ctx_raw, int32_t reason);",
                self.async_sleep_cleanup_name(&output_ty)
            ));
        }
    }

    pub(in crate::codegen) fn emit_async_op_future_prototypes(&mut self) {
        for context in self
            .plan
            .async_op_contexts
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            self.line(&format!(
                "static int32_t {}(void *ctx_raw, void *out_raw);",
                self.async_op_run_name(&context.op_ty, &context.output_ty)
            ));
            self.line(&format!(
                "static void {}(void *ctx_raw, int32_t reason);",
                self.async_op_cleanup_name(&context.op_ty, &context.output_ty)
            ));
        }
    }

    pub(in crate::codegen) fn emit_async_channel_future_prototypes(&mut self) {
        for payload_ty in self
            .plan
            .async_channel_send_payload_tys
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            self.line(&format!(
                "static int32_t {}(void *ctx_raw, void *out_raw);",
                self.async_channel_send_run_name(&payload_ty)
            ));
            self.line(&format!(
                "static void {}(void *ctx_raw, int32_t reason);",
                self.async_channel_send_cleanup_name(&payload_ty)
            ));
        }
        for payload_ty in self
            .plan
            .async_channel_reserve_payload_tys
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            self.line(&format!(
                "static int32_t {}(void *ctx_raw, void *out_raw);",
                self.async_channel_reserve_run_name(&payload_ty)
            ));
            self.line(&format!(
                "static void {}(void *ctx_raw, int32_t reason);",
                self.async_channel_reserve_cleanup_name(&payload_ty)
            ));
        }
        for payload_ty in self
            .plan
            .async_channel_recv_payload_tys
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            self.line(&format!(
                "static int32_t {}(void *ctx_raw, void *out_raw);",
                self.async_channel_recv_run_name(&payload_ty)
            ));
            self.line(&format!(
                "static void {}(void *ctx_raw, int32_t reason);",
                self.async_channel_recv_cleanup_name(&payload_ty)
            ));
        }
    }
}
