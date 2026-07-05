use super::*;

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn function_decl(
        &self,
        function: &CheckedFunction,
        _prototype: bool,
    ) -> String {
        let name = self.c_name(function.def_id);
        let params = function
            .params
            .iter()
            .filter(|(_, _, ty, _)| !ty.is_erased_value())
            .map(|(_, name, ty, _)| self.c_decl(ty, name))
            .collect::<Vec<_>>();
        let params = if params.is_empty() {
            "void".to_string()
        } else {
            params.join(", ")
        };
        let ret_ty = self.function_call_return_ty(function);
        let decl = self.c_return_decl(&ret_ty, &format!("{name}({params})"));
        if self.function_has_internal_linkage(function) {
            format!("static {decl}")
        } else {
            decl
        }
    }

    fn function_has_internal_linkage(&self, function: &CheckedFunction) -> bool {
        function.body.is_some() && !(function.abi.as_deref() == Some("C") && function.exported)
    }

    pub(in crate::codegen) fn function_call_return_ty(&self, function: &CheckedFunction) -> Ty {
        if function.is_async {
            let affine_state = function
                .params
                .iter()
                .any(|(_, _, ty, _)| self.type_is_affine(ty));
            let state = function
                .params
                .iter()
                .map(|(_, name, ty, _)| (name.clone(), ty.clone()))
                .collect();
            generated_future_ty_with_state(
                format!("fn_{}", function.def_id.0),
                function.ret.clone(),
                false,
                true,
                affine_state,
                state,
            )
        } else {
            function.ret.clone()
        }
    }

    pub(in crate::codegen) fn async_function_names(&self, def_id: DefId) -> AsyncFunctionNames {
        let base = self.c_name(def_id);
        AsyncFunctionNames {
            context: format!("CielAsyncCtx_{base}"),
            run: format!("CielAsyncRun_{base}"),
            cleanup: format!("CielAsyncCleanup_{base}"),
        }
    }

    pub(in crate::codegen) fn async_closure_names(
        &self,
        closure: &ClosureDef,
    ) -> AsyncFunctionNames {
        let base = format!("{}_closure_{}", self.c_name(closure.owner), closure.id);
        AsyncFunctionNames {
            context: format!("CielAsyncClosureCtx_{base}"),
            run: format!("CielAsyncClosureRun_{base}"),
            cleanup: format!("CielAsyncClosureCleanup_{base}"),
        }
    }

    pub(in crate::codegen) fn async_sleep_context_name(&self, output_ty: &Ty) -> String {
        format!("CielAsyncSleepFutureCtx_{}", mangle_ty_fragment(output_ty))
    }

    pub(in crate::codegen) fn async_sleep_run_name(&self, output_ty: &Ty) -> String {
        format!("CielAsyncSleepFutureRun_{}", mangle_ty_fragment(output_ty))
    }

    pub(in crate::codegen) fn async_sleep_cleanup_name(&self, output_ty: &Ty) -> String {
        format!(
            "CielAsyncSleepFutureCleanup_{}",
            mangle_ty_fragment(output_ty)
        )
    }

    pub(in crate::codegen) fn awaitable_future_impl_name(
        &self,
        output_ty: &Ty,
        receiver_ty: &Ty,
    ) -> DiagResult<String> {
        let receiver_ty = if let Ty::OpaqueState { base, .. } = receiver_ty {
            base.as_ref()
        } else {
            receiver_ty
        };
        self.program
            .checked
            .impls
            .iter()
            .find(|implementation| {
                impl_matches_interface_receiver(
                    implementation,
                    self.std_async_interface_def(STD_ASYNC_AWAITABLE_FUTURE_INTERFACE),
                    std::slice::from_ref(output_ty),
                    receiver_ty,
                )
            })
            .map(|implementation| self.c_name(implementation.function_def))
            .ok_or_else(|| {
                vec![Diagnostic::new(
                    None,
                    format!(
                        "internal error: missing `{STD_ASYNC_AWAITABLE_FUTURE_INTERFACE}` impl for `{receiver_ty}` yielding `{output_ty}`"
                    ),
                )]
            })
    }

    pub(in crate::codegen) fn future_output_ty_for_codegen(&self, ty: &Ty) -> Option<Ty> {
        if let Some(output_ty) = generated_future_output_ty(ty) {
            return Some(output_ty);
        }
        std_id::std_async_future_output_arg(&self.program.checked.resolved, ty).cloned()
    }

    pub(in crate::codegen) fn task_output_ty_for_codegen(&self, ty: &Ty) -> Option<Ty> {
        std_id::std_async_task_output_arg(&self.program.checked.resolved, ty).cloned()
    }
}
