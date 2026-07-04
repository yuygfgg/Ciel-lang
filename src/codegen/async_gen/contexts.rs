use super::*;

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn emit_async_function_contexts(&mut self) {
        for function in &self.program.checked.functions {
            if !function.is_async || function.body.is_none() {
                continue;
            }
            let names = self.async_function_names(function.def_id);
            self.line(&format!("typedef struct {} {{", names.context));
            self.line("    CielFuture *future;");
            self.line("    uint32_t pc;");
            self.line("    uint32_t cleanup_state;");
            self.line("    CielFuture *active_future;");
            let mut emitted = false;
            for (idx, (_, _, ty, _)) in function
                .params
                .iter()
                .filter(|(_, _, ty, _)| !ty.is_erased_value())
                .enumerate()
            {
                self.line(&format!("    {};", self.c_decl(ty, &format!("arg{idx}"))));
                emitted = true;
            }
            for local in self.async_frame_locals_with_escape_info_for_function(function) {
                if local.heap {
                    self.line(&format!(
                        "    {};",
                        self.c_pointer_decl(&local.ty, &local.field)
                    ));
                } else {
                    self.line(&format!("    {};", self.c_decl(&local.ty, &local.field)));
                }
                emitted = true;
            }
            let facts = self.async_facts_for_function(function);
            for (idx, output_ty) in facts.await_output_tys.iter().enumerate() {
                if output_ty.is_erased_value() {
                    continue;
                }
                self.line(&format!(
                    "    {};",
                    self.c_decl(&output_ty, &format!("await_out{}", idx + 1))
                ));
                emitted = true;
            }
            for arg in &facts.defer_args {
                self.line(&format!("    {};", self.c_decl(&arg.ty, &arg.field)));
                emitted = true;
            }
            if !emitted {
                self.line("    int _unused;");
            }
            self.line(&format!("}} {};", names.context));
        }
        if self
            .program
            .checked
            .functions
            .iter()
            .any(|function| function.is_async && function.body.is_some())
        {
            self.line("");
        }
    }

    pub(in crate::codegen) fn emit_async_closure_contexts(&mut self) {
        let closures = self.plan.closure_defs.clone();
        for closure in closures.values().filter(|closure| closure.is_async) {
            let names = self.async_closure_names(closure);
            self.line(&format!("typedef struct {} {{", names.context));
            self.line("    CielFuture *future;");
            self.line("    uint32_t pc;");
            self.line("    uint32_t cleanup_state;");
            self.line("    CielFuture *active_future;");
            let mut emitted = false;
            for (idx, capture) in closure.captures.iter().enumerate() {
                if capture.ty.is_erased_value() {
                    continue;
                }
                self.line(&format!(
                    "    {};",
                    self.c_decl(&capture.ty, &format!("cap{idx}"))
                ));
                emitted = true;
            }
            for (idx, (_, _, ty)) in closure
                .params
                .iter()
                .filter(|(_, _, ty)| !ty.is_erased_value())
                .enumerate()
            {
                self.line(&format!("    {};", self.c_decl(ty, &format!("arg{idx}"))));
                emitted = true;
            }
            for local in self.async_frame_locals_with_escape_info_for_closure(closure) {
                if local.heap {
                    self.line(&format!(
                        "    {};",
                        self.c_pointer_decl(&local.ty, &local.field)
                    ));
                } else {
                    self.line(&format!("    {};", self.c_decl(&local.ty, &local.field)));
                }
                emitted = true;
            }
            let facts = self.async_facts_for_closure(closure);
            for (idx, output_ty) in facts.await_output_tys.iter().enumerate() {
                if output_ty.is_erased_value() {
                    continue;
                }
                self.line(&format!(
                    "    {};",
                    self.c_decl(&output_ty, &format!("await_out{}", idx + 1))
                ));
                emitted = true;
            }
            for arg in &facts.defer_args {
                self.line(&format!("    {};", self.c_decl(&arg.ty, &arg.field)));
                emitted = true;
            }
            if !emitted {
                self.line("    int _unused;");
            }
            self.line(&format!("}} {};", names.context));
        }
        if self
            .plan
            .closure_defs
            .values()
            .any(|closure| closure.is_async)
        {
            self.line("");
        }
    }

    pub(in crate::codegen) fn emit_async_sleep_future_contexts(&mut self) {
        for output_ty in self
            .plan
            .async_sleep_output_tys
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            let name = self.async_sleep_context_name(&output_ty);
            self.line(&format!("typedef struct {name} {{"));
            self.line("    CielFuture *future;");
            self.line("    CielAsyncOp *op;");
            self.line("    uint64_t ms;");
            self.line(&format!("}} {name};"));
        }
        if !self.plan.async_sleep_output_tys.is_empty() {
            self.line("");
        }
    }

    pub(in crate::codegen) fn emit_async_op_future_contexts(&mut self) {
        for context in self
            .plan
            .async_op_contexts
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            let name = self.async_op_context_name(&context.op_ty, &context.output_ty);
            self.line(&format!("typedef struct {name} {{"));
            self.line("    CielFuture *future;");
            self.line("    CielAsyncOp *op;");
            self.line(&format!("    {};", self.c_decl(&context.op_ty, "op_value")));
            self.line(&format!("}} {name};"));
        }
        if !self.plan.async_op_contexts.is_empty() {
            self.line("");
        }
    }

    pub(in crate::codegen) fn emit_async_channel_future_contexts(&mut self) {
        for payload_ty in self
            .plan
            .async_channel_send_payload_tys
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            let name = self.async_channel_send_context_name(&payload_ty);
            self.line(&format!("typedef struct {name} {{"));
            self.line("    CielFuture *future;");
            self.line("    void *sender;");
            self.line("    int init_failed;");
            self.line(&format!(
                "    {};",
                self.c_decl(&std_error_ty(), "init_error")
            ));
            if !payload_ty.is_erased_value() {
                self.line(&format!("    {};", self.c_decl(&payload_ty, "value")));
            }
            self.line(&format!("}} {name};"));
        }
        for payload_ty in self
            .plan
            .async_channel_reserve_payload_tys
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            let name = self.async_channel_reserve_context_name(&payload_ty);
            self.line(&format!("typedef struct {name} {{"));
            self.line("    CielFuture *future;");
            self.line("    void *sender;");
            self.line(&format!("}} {name};"));
        }
        for payload_ty in self
            .plan
            .async_channel_recv_payload_tys
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            let name = self.async_channel_recv_context_name(&payload_ty);
            self.line(&format!("typedef struct {name} {{"));
            self.line("    CielFuture *future;");
            self.line("    void *receiver;");
            self.line(&format!("}} {name};"));
        }
        if !self.plan.async_channel_send_payload_tys.is_empty()
            || !self.plan.async_channel_reserve_payload_tys.is_empty()
            || !self.plan.async_channel_recv_payload_tys.is_empty()
        {
            self.line("");
        }
    }

    pub(in crate::codegen) fn emit_async_task_group_future_contexts(&mut self) {
        for payload_ty in self
            .plan
            .async_task_group_next_payload_tys
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            let name = self.async_task_group_next_context_name(&payload_ty);
            self.line(&format!("typedef struct {name} {{"));
            self.line("    CielFuture *future;");
            self.line("    void *group;");
            self.line(&format!("}} {name};"));
        }
        if !self.plan.async_task_group_next_payload_tys.is_empty() {
            self.line("");
        }
    }
}
