use super::*;

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn emit_closure_value_layouts(&mut self) {
        let closure_types = self.plan.closure_types.clone();
        for (name, ty) in closure_types {
            self.line(&format!("struct {name} {{"));
            self.line(&format!("    {};", self.closure_call_field_decl(&ty)));
            self.line("    void *env;");
            for capability in retained_closure_capabilities(&ty) {
                self.line(&format!(
                    "    {};",
                    self.retained_closure_witness_field_decl(&ty, &capability)
                ));
            }
            self.line("};");
            self.line("");
        }
    }

    pub(in crate::codegen) fn emit_closure_environment_layouts(&mut self) {
        let closure_defs = self.plan.closure_defs.clone();
        for closure in closure_defs.values() {
            if closure.captures.is_empty() {
                continue;
            }
            let env_name = self.closure_env_name(closure.owner, closure.id);
            self.line(&format!("struct {env_name} {{"));
            let mut emitted_capture = false;
            for (idx, capture) in closure.captures.iter().enumerate() {
                if capture.ty.is_erased_value() {
                    continue;
                }
                emitted_capture = true;
                self.line(&format!(
                    "    {};",
                    self.c_decl(&capture.ty, &format!("cap{idx}"))
                ));
            }
            if !emitted_capture {
                self.line("    char _ciel_empty;");
            }
            self.line("};");
            self.line("");
        }

        let wrappers = self.plan.function_closure_wrappers.clone();
        for wrapper in wrappers.values() {
            let env_name =
                self.function_closure_env_name(&wrapper.closure_ty, &wrapper.function_ty);
            self.line(&format!("struct {env_name} {{"));
            self.line(&format!(
                "    {};",
                self.c_decl(&wrapper.function_ty, "func")
            ));
            self.line("};");
            self.line("");
        }

        let wrappers = self.plan.retained_closure_wrappers.clone();
        for wrapper in wrappers.values() {
            let env_name = self.retained_closure_env_name(&wrapper.target_ty, &wrapper.source_ty);
            self.line(&format!("struct {env_name} {{"));
            self.line(&format!(
                "    {};",
                self.c_decl(&wrapper.source_ty, "source")
            ));
            self.line("};");
            self.line("");
        }
    }

    pub(in crate::codegen) fn emit_closure_prototypes(&mut self) {
        let closures = self.plan.closure_defs.clone();
        for closure in closures.values() {
            self.line(&format!("{};", self.closure_thunk_decl(closure)));
        }
        let wrappers = self.plan.function_closure_wrappers.clone();
        for wrapper in wrappers.values() {
            self.line(&format!(
                "{};",
                self.function_closure_thunk_decl(&wrapper.closure_ty, &wrapper.function_ty)
            ));
        }
        let wrappers = self.plan.retained_closure_wrappers.clone();
        for wrapper in wrappers.values() {
            self.line(&format!(
                "{};",
                self.retained_closure_thunk_decl(&wrapper.target_ty, &wrapper.source_ty)
            ));
        }
    }

    fn closure_call_field_decl(&self, ty: &Ty) -> String {
        let (ret, params) = self
            .callable_ret_params(ty)
            .expect("closure value type is callable");
        let mut decls = vec!["void *env".to_string()];
        decls.extend(
            params
                .iter()
                .filter(|ty| !ty.is_erased_value())
                .enumerate()
                .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}"))),
        );
        self.c_return_decl(&ret, &format!("(*call)({})", decls.join(", ")))
    }

    fn retained_closure_witness_field_decl(&self, ty: &Ty, capability: &ConstraintRef) -> String {
        let ret = self.retained_closure_interface_ret(ty, capability);
        let params = self.retained_closure_interface_params(ty, capability);
        let params = params
            .iter()
            .filter(|ty| !ty.is_erased_value())
            .enumerate()
            .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}")))
            .collect::<Vec<_>>()
            .join(", ");
        self.c_return_decl(
            &ret,
            &format!(
                "(*{})({})",
                self.retained_closure_witness_field_name(capability),
                params
            ),
        )
    }

    pub(super) fn retained_closure_interface_ret(
        &self,
        receiver_ty: &Ty,
        capability: &ConstraintRef,
    ) -> Ty {
        retained_closure_interface_signature(
            &self.program.checked.interfaces,
            receiver_ty,
            capability,
        )
        .map(|signature| signature.ret)
        .unwrap_or(Ty::Unknown)
    }

    pub(super) fn retained_closure_interface_params(
        &self,
        receiver_ty: &Ty,
        capability: &ConstraintRef,
    ) -> Vec<Ty> {
        retained_closure_interface_signature(
            &self.program.checked.interfaces,
            receiver_ty,
            capability,
        )
        .map(|signature| signature.params)
        .unwrap_or_else(|| vec![Ty::pointer_to(Ty::Void)])
    }

    pub(super) fn closure_thunk_decl(&self, closure: &ClosureDef) -> String {
        let (ret, params) = self
            .callable_ret_params(&closure.ty)
            .expect("closure thunk type is callable");
        let mut decls = Vec::new();
        if matches!(closure.ty, Ty::Closure { .. } | Ty::ClosureInstance { .. }) {
            decls.push("void *env_raw".to_string());
        }
        decls.extend(
            params
                .iter()
                .filter(|ty| !ty.is_erased_value())
                .enumerate()
                .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}"))),
        );
        let params = if decls.is_empty() {
            "void".to_string()
        } else {
            decls.join(", ")
        };
        self.c_static_return_decl(
            &ret,
            &format!(
                "{}({params})",
                self.closure_thunk_name(closure.owner, closure.id)
            ),
        )
    }

    pub(super) fn function_closure_thunk_decl(&self, closure_ty: &Ty, function_ty: &Ty) -> String {
        let (ret, params) = self
            .callable_ret_params(closure_ty)
            .expect("function wrapper closure type is callable");
        let mut decls = vec!["void *env_raw".to_string()];
        decls.extend(
            params
                .iter()
                .filter(|ty| !ty.is_erased_value())
                .enumerate()
                .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}"))),
        );
        self.c_static_return_decl(
            &ret,
            &format!(
                "{}({})",
                self.function_closure_thunk_name(closure_ty, function_ty),
                decls.join(", ")
            ),
        )
    }

    pub(super) fn retained_closure_thunk_decl(&self, target_ty: &Ty, source_ty: &Ty) -> String {
        let (ret, params) = self
            .callable_ret_params(target_ty)
            .expect("retained wrapper closure type is callable");
        let mut decls = vec!["void *env_raw".to_string()];
        decls.extend(
            params
                .iter()
                .filter(|ty| !ty.is_erased_value())
                .enumerate()
                .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}"))),
        );
        self.c_static_return_decl(
            &ret,
            &format!(
                "{}({})",
                self.retained_closure_thunk_name(target_ty, source_ty),
                decls.join(", ")
            ),
        )
    }

    pub(super) fn callable_ret_params(&self, ty: &Ty) -> DiagResult<(Ty, Vec<Ty>)> {
        match ty {
            Ty::Closure { ret, params, .. }
            | Ty::ClosureInstance { ret, params, .. }
            | Ty::Function { ret, params, .. } => Ok(((**ret).clone(), params.clone())),
            other => Err(vec![Diagnostic::new(
                None,
                format!("internal error: `{other}` is not callable"),
            )]),
        }
    }
}
