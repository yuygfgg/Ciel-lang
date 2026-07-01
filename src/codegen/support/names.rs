use super::*;

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn c_name(&self, def_id: DefId) -> String {
        self.plan
            .name_map
            .get(&def_id)
            .cloned()
            .unwrap_or_else(|| format!("ciel_missing_{}", def_id.0))
    }

    pub(in crate::codegen) fn slice_name(&self, mutability: ViewMutability, elem: &Ty) -> String {
        let prefix = match mutability {
            ViewMutability::Writable => "CielSlice",
            ViewMutability::ReadOnly => "CielConstSlice",
        };
        format!("{prefix}_{}", mangle_ty_fragment(elem))
    }

    pub(in crate::codegen) fn dynamic_type_name(&self, ty: &Ty) -> String {
        match ty {
            Ty::DynamicInterface { name, args } => {
                if args.is_empty() {
                    format!("CielDyn_{name}")
                } else {
                    format!(
                        "CielDyn_{}_{}",
                        name,
                        args.iter()
                            .map(mangle_ty_fragment)
                            .collect::<Vec<_>>()
                            .join("_")
                    )
                }
            }
            _ => "CielDyn_unknown".to_string(),
        }
    }

    pub(in crate::codegen) fn dynamic_vtable_name(&self, ty: &Ty) -> String {
        format!("{}VTable", self.dynamic_type_name(ty))
    }

    pub(in crate::codegen) fn dynamic_impl_key(&self, dyn_ty: &Ty, concrete_ty: &Ty) -> String {
        format!(
            "{}__{}",
            self.dynamic_type_name(dyn_ty),
            mangle_ty_fragment(concrete_ty)
        )
    }

    pub(in crate::codegen) fn closure_type_name(&self, ty: &Ty) -> String {
        match ty {
            Ty::Closure {
                ret,
                params,
                constraints,
            } => {
                let sig_ty = Ty::Closure {
                    ret: ret.clone(),
                    params: params.clone(),
                    constraints: constraints.clone(),
                };
                format!("CielClosure_{}", mangle_ty_fragment(&sig_ty))
            }
            Ty::ClosureInstance { ret, params, .. } => {
                let sig_ty = Ty::Closure {
                    ret: ret.clone(),
                    params: params.clone(),
                    constraints: ConstraintBounds::default(),
                };
                format!("CielClosure_{}", mangle_ty_fragment(&sig_ty))
            }
            _ => "CielClosure_unknown".to_string(),
        }
    }

    pub(in crate::codegen) fn retained_closure_witness_field_name(
        &self,
        capability: &ConstraintRef,
    ) -> String {
        format!("cap_{}", mangle_constraint_ref(capability))
    }

    pub(in crate::codegen) fn retained_closure_witness_name(
        &self,
        target_ty: &Ty,
        source_ty: &Ty,
        capability: &ConstraintRef,
    ) -> String {
        format!(
            "ciel_retained_closure_witness_{}_{}_{}",
            mangle_constraint_ref(capability),
            mangle_ty_fragment(target_ty),
            mangle_ty_fragment(source_ty)
        )
    }

    pub(in crate::codegen) fn closure_env_name(&self, owner: DefId, id: usize) -> String {
        format!("CielClosureEnv_{}_{}", owner.0, id)
    }

    pub(in crate::codegen) fn closure_thunk_name(&self, owner: DefId, id: usize) -> String {
        format!("ciel_closure_thunk_{}_{}", owner.0, id)
    }

    pub(in crate::codegen) fn function_closure_wrapper_key(
        &self,
        closure_ty: &Ty,
        function_ty: &Ty,
    ) -> String {
        format!(
            "{}__{}",
            mangle_ty_fragment(closure_ty),
            mangle_ty_fragment(function_ty)
        )
    }

    pub(in crate::codegen) fn function_closure_env_name(
        &self,
        closure_ty: &Ty,
        function_ty: &Ty,
    ) -> String {
        format!(
            "CielClosureFnEnv_{}_{}",
            mangle_ty_fragment(closure_ty),
            mangle_ty_fragment(function_ty)
        )
    }

    pub(in crate::codegen) fn function_closure_thunk_name(
        &self,
        closure_ty: &Ty,
        function_ty: &Ty,
    ) -> String {
        format!(
            "ciel_function_to_closure_{}_{}",
            mangle_ty_fragment(closure_ty),
            mangle_ty_fragment(function_ty)
        )
    }

    pub(in crate::codegen) fn retained_closure_wrapper_key(
        &self,
        target_ty: &Ty,
        source_ty: &Ty,
    ) -> String {
        format!(
            "{}__{}",
            mangle_ty_fragment(target_ty),
            mangle_ty_fragment(source_ty)
        )
    }

    pub(in crate::codegen) fn retained_closure_env_name(
        &self,
        target_ty: &Ty,
        source_ty: &Ty,
    ) -> String {
        format!(
            "CielRetainedClosureEnv_{}_{}",
            mangle_ty_fragment(target_ty),
            mangle_ty_fragment(source_ty)
        )
    }

    pub(in crate::codegen) fn retained_closure_thunk_name(
        &self,
        target_ty: &Ty,
        source_ty: &Ty,
    ) -> String {
        format!(
            "ciel_retained_closure_to_closure_{}_{}",
            mangle_ty_fragment(target_ty),
            mangle_ty_fragment(source_ty)
        )
    }

    pub(in crate::codegen) fn retained_closure_source_pointer_expr(
        &self,
        witness: &RetainedClosureWitness,
    ) -> String {
        let target_ptr = format!("({})arg0", self.c_pointer_type(&witness.target_ty));
        self.retained_closure_source_pointer_expr_from_target_ptr(witness, &target_ptr)
    }

    pub(in crate::codegen) fn retained_closure_source_pointer_expr_from_target_ptr(
        &self,
        witness: &RetainedClosureWitness,
        target_ptr: &str,
    ) -> String {
        match witness.source_ty {
            Ty::Function {
                is_unsafe: false,
                abi: None,
                ..
            } => {
                let env_name =
                    self.function_closure_env_name(&witness.target_ty, &witness.source_ty);
                format!("&((({env_name} *)(({target_ptr})->env))->func)")
            }
            Ty::Closure { .. }
                if retained_closure_needs_wrapper(&witness.target_ty, &witness.source_ty) =>
            {
                let env_name =
                    self.retained_closure_env_name(&witness.target_ty, &witness.source_ty);
                format!("&((({env_name} *)(({target_ptr})->env))->source)")
            }
            _ => format!(
                "({})({target_ptr})",
                self.c_pointer_type(&witness.source_ty)
            ),
        }
    }

    pub(in crate::codegen) fn actor_dispatch_name(
        &self,
        mode: &ActorSpawnMode,
        state_ty: &Ty,
        message_ty: &Ty,
        handler_ty: &Ty,
    ) -> String {
        let mode_name = match mode {
            ActorSpawnMode::Cloned => "cloned",
            ActorSpawnMode::State => "state",
        };
        format!(
            "ciel_actor_dispatch_{}_{}_{}_{}",
            mode_name,
            mangle_ty_fragment(state_ty),
            mangle_ty_fragment(message_ty),
            mangle_ty_fragment(handler_ty)
        )
    }

    pub(in crate::codegen) fn callable_call_expr(
        &self,
        callable_ty: &Ty,
        callable: &str,
        args: &[&str],
    ) -> DiagResult<String> {
        match callable_ty {
            Ty::Function { .. } => Ok(format!("({callable})({})", args.join(", "))),
            Ty::Closure { .. } | Ty::ClosureInstance { .. } => {
                let mut call_args = vec![format!("({callable}).env")];
                call_args.extend(args.iter().map(|arg| (*arg).to_string()));
                Ok(format!("({callable}).call({})", call_args.join(", ")))
            }
            other => Err(vec![Diagnostic::new(
                None,
                format!("internal error: actor callable `{other}` is not callable"),
            )]),
        }
    }

    pub(in crate::codegen) fn dynamic_table_name(&self, dyn_ty: &Ty, concrete_ty: &Ty) -> String {
        format!(
            "ciel_vtable_{}_{}",
            mangle_ty_fragment(dyn_ty),
            mangle_ty_fragment(concrete_ty)
        )
    }

    pub(in crate::codegen) fn dynamic_shim_name(
        &self,
        dyn_ty: &Ty,
        concrete_ty: &Ty,
        interface_name: &str,
    ) -> String {
        format!(
            "ciel_dyn_shim_{}_{}_{}",
            interface_name,
            mangle_ty_fragment(dyn_ty),
            mangle_ty_fragment(concrete_ty)
        )
    }

    pub(in crate::codegen) fn dynamic_use_interfaces(
        &self,
        dynamic_use: &DynamicImplUse,
    ) -> Vec<CheckedInterfaceRef> {
        let Ty::DynamicInterface { name, args } = &dynamic_use.dyn_ty else {
            return Vec::new();
        };
        self.dynamic_view_interfaces(name, args)
    }

    pub(in crate::codegen) fn dynamic_view_interfaces(
        &self,
        name: &str,
        args: &[Ty],
    ) -> Vec<CheckedInterfaceRef> {
        checked_interface_view(
            &self.program.checked.interfaces,
            &self.program.checked.interface_aliases,
            name,
            args,
        )
    }

    pub(in crate::codegen) fn impl_for_dynamic(
        &self,
        interface: &CheckedInterfaceRef,
        concrete_ty: &Ty,
    ) -> Option<&CheckedImpl> {
        self.program.checked.impls.iter().find(|implementation| {
            impl_matches_dynamic_interface(implementation, interface, concrete_ty)
        })
    }

    pub(in crate::codegen) fn impl_for_retained_closure_witness(
        &self,
        capability: &ConstraintRef,
        source_ty: &Ty,
    ) -> Option<&CheckedImpl> {
        self.program.checked.impls.iter().find(|implementation| {
            impl_matches_interface_receiver(
                implementation,
                &capability.name,
                &capability.args,
                source_ty,
            )
        })
    }

    pub(in crate::codegen) fn dynamic_interface_ret(
        &self,
        interface_ref: &CheckedInterfaceRef,
    ) -> Ty {
        dynamic_interface_signature(&self.program.checked.interfaces, interface_ref)
            .map(|signature| signature.ret)
            .unwrap_or(Ty::Unknown)
    }

    pub(in crate::codegen) fn dynamic_interface_params(
        &self,
        interface_ref: &CheckedInterfaceRef,
    ) -> Vec<Ty> {
        dynamic_interface_signature(&self.program.checked.interfaces, interface_ref)
            .map(|signature| signature.params)
            .unwrap_or_else(|| vec![Ty::pointer_to(Ty::Void)])
    }

    pub(in crate::codegen) fn find_ciel_main(&self) -> Option<&CheckedFunction> {
        self.program
            .checked
            .functions
            .iter()
            .find(|function| function.name == "main" && function.body.is_some())
    }
}
