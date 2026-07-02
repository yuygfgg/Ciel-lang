use super::*;

impl TypeChecker {
    pub(super) fn instantiate_generic_function_item(
        &mut self,
        span: crate::span::Span,
        sig: &FunctionSig,
        type_args: &[Type],
    ) -> Option<(FunctionSig, Vec<Ty>)> {
        if sig.generics.is_empty() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("function `{}` is not generic", sig.name),
            ));
            return None;
        }
        let explicit_count = Self::explicit_generic_count(&sig.generics);
        if type_args.len() != explicit_count {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "generic function `{}` expects {} explicit type arguments, got {}",
                    sig.name,
                    explicit_count,
                    type_args.len()
                ),
            ));
            return None;
        }

        let mut subst = HashMap::<String, Ty>::new();
        let current_subst = self.current_type_subst();
        let mut arg_idx = 0;
        for generic in &sig.generics {
            if generic.is_hidden {
                continue;
            }
            let concrete = self.lower_type_with_subst(&type_args[arg_idx], &current_subst);
            subst.insert(generic.name.clone(), concrete);
            arg_idx += 1;
        }
        self.infer_generic_constraints_from_known_receivers(&sig.generics, &mut subst, span);
        self.solve_hidden_generics(&sig.name, &sig.generics, &mut subst, span);
        for generic in &sig.generics {
            if !subst.contains_key(&generic.name) {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "could not infer generic parameter `{}` for `{}`",
                        generic.name, sig.name
                    ),
                ));
                return None;
            }
        }

        self.check_generic_constraints(&sig.generics, &subst, span);
        let instance_args = sig
            .generics
            .iter()
            .filter_map(|generic| {
                subst.get(&generic.name).map(|ty| {
                    let ty = self.resolve_type_holes(ty);
                    self.normalize_meta_repr_markers(&ty, span)
                })
            })
            .collect::<Vec<_>>();
        let params = sig
            .params
            .iter()
            .map(|param| {
                let substituted = substitute_ty(param, &subst);
                let ty = self.normalize_meta_repr_markers(&substituted, span);
                self.resolve_type_holes(&ty)
            })
            .collect::<Vec<_>>();
        let ret = {
            let substituted = substitute_ty(&sig.ret, &subst);
            let ty = self.normalize_meta_repr_markers(&substituted, span);
            self.resolve_type_holes(&ty)
        };
        self.ensure_generic_opaque_return_solution(sig, &instance_args, &ret);
        Some((
            FunctionSig {
                def_id: sig.def_id,
                module: sig.module,
                name: sig.name.clone(),
                is_unsafe: sig.is_unsafe,
                is_async: sig.is_async,
                abi: sig.abi.clone(),
                noescape: sig.noescape,
                has_body: sig.has_body,
                ret,
                params,
                generics: Vec::new(),
                exported: sig.exported,
            },
            instance_args,
        ))
    }

    pub(super) fn infer_generic_function_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        sig: &FunctionSig,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
        allow_resource_captures: bool,
    ) -> Option<(FunctionSig, Vec<Ty>)> {
        let mut subst = HashMap::<String, Ty>::new();
        let current_subst = self.current_type_subst();
        for (idx, ty) in type_args.iter().enumerate() {
            let Some(generic) = sig
                .generics
                .iter()
                .filter(|generic| !generic.is_hidden)
                .nth(idx)
            else {
                self.diagnostics.push(Diagnostic::new(
                    ty.span,
                    format!("too many type arguments for `{}`", sig.name),
                ));
                return None;
            };
            let concrete = self.lower_type_with_subst(ty, &current_subst);
            subst.insert(generic.name.clone(), concrete);
        }
        let expected_hints = if let Some(expected) = expected {
            let mut hints = subst.clone();
            self.unify_ty_for_inference(&sig.ret, expected, &mut hints);
            hints
        } else {
            subst.clone()
        };

        if sig.params.len() != args.len() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "call expects {} arguments, got {}",
                    sig.params.len(),
                    args.len()
                ),
            ));
        }

        let mut deferred_closure_args = Vec::new();
        for (idx, arg) in args.iter().enumerate() {
            let Some(param_ty) = sig.params.get(idx) else {
                continue;
            };
            let (expected_arg, expected_for_arg) =
                self.inference_arg_expected(param_ty, &subst, &expected_hints);
            if contains_generic(&expected_arg) && expr_is_closure_literal(arg) {
                if expected_for_arg.is_none() {
                    if let Some(partial_expected) =
                        self.closure_inference_expected(param_ty, &subst, &expected_hints)
                    {
                        let checked = self.check_generic_inference_arg(
                            scopes,
                            arg,
                            Some(&partial_expected),
                            allow_resource_captures,
                        )?;
                        self.unify_ty_for_inference(&expected_arg, &checked.ty, &mut subst);
                        continue;
                    }
                    deferred_closure_args.push(idx);
                    continue;
                }
            }
            let checked = self.check_generic_inference_arg(
                scopes,
                arg,
                expected_for_arg.as_ref(),
                allow_resource_captures,
            )?;
            self.unify_ty_for_inference(&expected_arg, &checked.ty, &mut subst);
        }
        for idx in deferred_closure_args {
            let Some(param_ty) = sig.params.get(idx) else {
                continue;
            };
            let Some(arg) = args.get(idx) else {
                continue;
            };
            let (expected_arg, expected_for_arg) =
                self.inference_arg_expected(param_ty, &subst, &expected_hints);
            let checked = self.check_generic_inference_arg(
                scopes,
                arg,
                expected_for_arg.as_ref(),
                allow_resource_captures,
            )?;
            self.unify_ty_for_inference(&expected_arg, &checked.ty, &mut subst);
        }
        if let Some(expected) = expected {
            self.unify_ty_for_inference(&sig.ret, expected, &mut subst);
        }
        self.infer_generic_constraints_from_known_receivers(&sig.generics, &mut subst, span);
        self.solve_hidden_generics(&sig.name, &sig.generics, &mut subst, span);

        for generic in &sig.generics {
            if !subst.contains_key(&generic.name) {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "could not infer generic parameter `{}` for `{}`",
                        generic.name, sig.name
                    ),
                ));
                return None;
            }
            if subst
                .get(&generic.name)
                .is_some_and(|ty| contains_type_hole(&self.resolve_type_holes(ty)))
            {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "could not infer generic parameter `{}` for `{}`",
                        generic.name, sig.name
                    ),
                ));
                return None;
            }
        }

        self.check_generic_constraints(&sig.generics, &subst, span);
        let instance_args = sig
            .generics
            .iter()
            .filter_map(|generic| {
                subst.get(&generic.name).map(|ty| {
                    let ty = self.resolve_type_holes(ty);
                    self.normalize_meta_repr_markers(&ty, span)
                })
            })
            .collect::<Vec<_>>();
        let params = sig
            .params
            .iter()
            .map(|param| {
                let substituted = substitute_ty(param, &subst);
                let ty = self.normalize_meta_repr_markers(&substituted, span);
                self.resolve_type_holes(&ty)
            })
            .collect::<Vec<_>>();
        let ret = {
            let substituted = substitute_ty(&sig.ret, &subst);
            let ty = self.normalize_meta_repr_markers(&substituted, span);
            self.resolve_type_holes(&ty)
        };
        self.ensure_generic_opaque_return_solution(sig, &instance_args, &ret);
        Some((
            FunctionSig {
                def_id: sig.def_id,
                module: sig.module,
                name: sig.name.clone(),
                is_unsafe: sig.is_unsafe,
                is_async: sig.is_async,
                abi: sig.abi.clone(),
                noescape: sig.noescape,
                has_body: sig.has_body,
                ret,
                params,
                generics: Vec::new(),
                exported: sig.exported,
            },
            instance_args,
        ))
    }

    pub(super) fn ensure_generic_opaque_return_solution(
        &mut self,
        sig: &FunctionSig,
        instance_args: &[Ty],
        ret: &Ty,
    ) {
        let Ty::OpaqueReturn { key, .. } = ret else {
            return;
        };
        if self.opaque_returns.contains_key(key) {
            return;
        }
        if !self.opaque_return_probe_stack.insert(key.clone()) {
            return;
        }
        let Some(template) = self.ctx.generic_functions.get(&sig.def_id).cloned() else {
            self.opaque_return_probe_stack.remove(key);
            return;
        };
        let probe_def = self.alloc_synthetic_def();
        let probe_name = format!(
            "{}__opaque_probe_{}",
            sig.name,
            instance_args
                .iter()
                .map(mangle_ty_fragment)
                .collect::<Vec<_>>()
                .join("_")
        );
        let checked_template = CheckedGenericFunction {
            def_id: sig.def_id,
            module: sig.module,
            name: sig.name.clone(),
            is_unsafe: sig.is_unsafe,
            is_async: sig.is_async,
            abi: sig.abi.clone(),
            noescape: sig.noescape,
            exported: template.exported,
            generics: sig
                .generics
                .iter()
                .map(|generic| CheckedGenericParam {
                    name: generic.name.clone(),
                    is_resource: generic.is_resource,
                    is_hidden: generic.is_hidden,
                    constraint: generic.constraint.clone(),
                })
                .collect(),
            ret: sig.ret.clone(),
            params: sig.params.clone(),
            function: template.function,
        };
        let _ = self.instantiate_generic_template_for_mono(
            &checked_template,
            instance_args,
            probe_def,
            probe_name,
        );
        self.opaque_return_probe_stack.remove(key);
    }

    pub(super) fn infer_generic_constraints_from_known_receivers(
        &mut self,
        generics: &[GenericInfo],
        subst: &mut HashMap<String, Ty>,
        span: crate::span::Span,
    ) {
        for _ in 0..=generics.len() {
            let before = subst.clone();
            for generic in generics {
                let Some(concrete) = subst.get(&generic.name).cloned() else {
                    continue;
                };
                let Some(constraint) = &generic.constraint else {
                    continue;
                };
                let bounds = self.constraint_bounds(constraint, subst);
                for capability in bounds.positive {
                    let Some(actual_args) =
                        self.actual_capability_args_for_inference(&capability, &concrete, span)
                    else {
                        continue;
                    };
                    for (pattern, actual) in capability.args.iter().zip(actual_args.iter()) {
                        self.unify_ty_for_inference(pattern, actual, subst);
                    }
                }
            }
            if *subst == before {
                break;
            }
        }
    }

    pub(super) fn actual_capability_args_for_inference(
        &mut self,
        capability: &ConstraintRef,
        receiver_ty: &Ty,
        span: crate::span::Span,
    ) -> Option<Vec<Ty>> {
        if std_id::is_std_async_interface(
            &self.ctx.resolved,
            capability.def_id,
            STD_ASYNC_AWAITABLE_FUTURE_INTERFACE,
        ) {
            return self
                .awaitable_output_ty(receiver_ty, span)
                .map(|output_ty| vec![output_ty]);
        }
        let generic_names = capability
            .args
            .iter()
            .flat_map(|arg| ty_generic_names(arg).into_iter())
            .collect::<HashSet<_>>();
        if generic_names.is_empty() {
            return None;
        }
        let assumptions = self.hidden_solver_assumptions(receiver_ty);
        match capability_solve::solve_hidden_from_capability(
            &self.ctx,
            receiver_ty,
            capability,
            &generic_names,
            &assumptions,
        ) {
            capability_solve::HiddenSolveResult::Unique(bindings) => {
                let subst = bindings.into_iter().collect::<HashMap<_, _>>();
                Some(
                    capability
                        .args
                        .iter()
                        .map(|arg| substitute_ty(arg, &subst))
                        .collect(),
                )
            }
            capability_solve::HiddenSolveResult::NoSolution
            | capability_solve::HiddenSolveResult::Ambiguous => None,
        }
    }

    pub(in crate::typeck) fn check_generic_constraints(
        &mut self,
        generics: &[GenericInfo],
        subst: &HashMap<String, Ty>,
        span: impl Into<Option<crate::span::Span>>,
    ) {
        self.check_generic_constraints_impl(generics, subst, span.into());
    }

    pub(super) fn check_generic_constraints_impl(
        &mut self,
        generics: &[GenericInfo],
        subst: &HashMap<String, Ty>,
        span: Option<crate::span::Span>,
    ) {
        for generic in generics {
            let Some(concrete) = subst.get(&generic.name) else {
                continue;
            };
            if generic.is_resource && !self.type_is_affine(concrete) {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "generic constraint not satisfied: `{}` is not a resource-affine type",
                        concrete
                    ),
                ));
            }
            let Some(constraint) = &generic.constraint else {
                continue;
            };
            let concrete_for_constraints = self.meta_repr_constraint_receiver_ty(concrete, span);
            let bounds = self.constraint_bounds(constraint, subst);
            for capability in bounds.positive {
                if !self.type_implements_capability_ref(&capability, &concrete_for_constraints) {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!(
                            "generic constraint not satisfied: `{}` does not implement `{}`",
                            concrete, capability.name
                        ),
                    ));
                }
            }
            for capability in bounds.negative {
                if self.type_implements_capability_ref(&capability, &concrete_for_constraints) {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!(
                            "generic constraint not satisfied: `{}` has forbidden capability `{}`",
                            concrete, capability.name
                        ),
                    ));
                }
            }
        }
    }

    pub(in crate::typeck) fn instantiate_generic_template_for_mono(
        &mut self,
        template: &CheckedGenericFunction,
        instance_args: &[Ty],
        def_id: DefId,
        instance_name: String,
    ) -> Option<CheckedFunction> {
        if template.generics.len() != instance_args.len() {
            self.diagnostics.push(Diagnostic::new(
                template.function.signature.name.span,
                format!(
                    "generic function `{}` expects {} type arguments, got {}",
                    template.name,
                    template.generics.len(),
                    instance_args.len()
                ),
            ));
            return None;
        }
        let subst = template
            .generics
            .iter()
            .map(|generic| generic.name.clone())
            .zip(instance_args.iter().cloned())
            .collect::<HashMap<_, _>>();
        let generics = template
            .generics
            .iter()
            .map(|generic| GenericInfo {
                name: generic.name.clone(),
                is_resource: generic.is_resource,
                is_hidden: generic.is_hidden,
                constraint: generic.constraint.clone(),
            })
            .collect::<Vec<_>>();
        self.check_generic_constraints(&generics, &subst, template.function.signature.name.span);
        let params = template
            .function
            .signature
            .params
            .iter()
            .map(|param| {
                (
                    param.local_id,
                    param.name.name.clone(),
                    self.lower_type_with_subst(&param.ty, &subst),
                )
            })
            .collect::<Vec<_>>();
        let body_params = template
            .function
            .signature
            .params
            .iter()
            .zip(params.iter())
            .filter_map(|(param, (_, _, ty))| {
                param.local_id.map(|local_id| {
                    (
                        local_id,
                        param.name.name.clone(),
                        ty.clone(),
                        param.mutability,
                    )
                })
            })
            .collect::<Vec<_>>();
        let ret = self.lower_function_return_type(
            template.def_id,
            &template.function.signature.ret,
            &generics,
            &subst,
        );
        let instance_sig = FunctionSig {
            def_id,
            module: template.module,
            name: instance_name.clone(),
            is_unsafe: template.is_unsafe,
            is_async: template.is_async,
            abi: template.abi.clone(),
            noescape: template.noescape,
            has_body: true,
            ret: ret.clone(),
            params: params.iter().map(|(_, _, ty)| ty.clone()).collect(),
            generics: Vec::new(),
            exported: false,
        };
        self.ctx
            .functions_by_def
            .insert(def_id, instance_sig.clone());

        let body = template.function.body.as_ref().and_then(|body| {
            let previous_module = self.current_module;
            self.current_module = template.module;
            self.type_subst_stack.push(subst.clone());
            let checked_body = self.check_function_body(&instance_sig, &body_params, body);
            self.type_subst_stack.pop();
            self.current_module = previous_module;
            checked_body
        });
        body.map(|body| CheckedFunction {
            def_id,
            name: instance_name,
            is_unsafe: template.is_unsafe,
            is_async: template.is_async,
            async_facts: body.async_facts,
            abi: template.abi.clone(),
            noescape: template.noescape,
            exported: false,
            ret,
            params,
            body: Some(body.block),
        })
    }

    pub(super) fn check_interface_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        def_id: DefId,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        self.check_interface_call_with_receiver_index(
            scopes, span, def_id, type_args, args, expected, 0,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_interface_call_with_receiver_index(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        def_id: DefId,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
        receiver_index: usize,
    ) -> Option<TExpr> {
        let name = self.ctx.resolved.def(def_id).name.clone();
        let Some(interface) = self.ctx.interfaces.get(&def_id).cloned() else {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("interface alias `{name}` is not directly callable"),
            ));
            return None;
        };
        if interface.params.is_empty() || args.get(receiver_index).is_none() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("interface call `{name}` requires a receiver argument"),
            ));
            return None;
        }

        let explicit_args = type_args
            .iter()
            .map(|arg| self.lower_type(arg))
            .collect::<Vec<_>>();
        let receiver_arg = self.check_expr(scopes, &args[receiver_index], None)?;
        if let Ty::DynamicInterface {
            def_id: dyn_def_id,
            args: dyn_args,
            ..
        } = &receiver_arg.ty
            && let Some(interface_ref) = self.dynamic_view_interface(*dyn_def_id, dyn_args, def_id)
        {
            if receiver_index != 0 {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "dynamic interface call `{name}` requires the receiver parameter to be first"
                    ),
                ));
                return None;
            }
            return self.check_dynamic_interface_call(
                scopes,
                span,
                interface,
                &interface_ref.args,
                receiver_arg,
                &args[1..],
            );
        }

        let mut subst = interface
            .generics
            .iter()
            .cloned()
            .map(|name| (name.clone(), Ty::Generic(name)))
            .collect::<HashMap<_, _>>();
        for (idx, ty) in explicit_args.iter().enumerate() {
            let Some(generic) = interface.generics.iter().skip(1).nth(idx) else {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("too many type arguments for interface `{name}`"),
                ));
                return None;
            };
            subst.insert(generic.clone(), ty.clone());
        }
        if let Some(expected) = expected {
            let ret = self.lower_type_with_subst(&interface.ret, &subst);
            self.unify_ty_for_inference(&ret, expected, &mut subst);
        }
        if let Some(param) = interface.params.get(receiver_index) {
            let param_ty = self.lower_type_with_subst(&param.ty, &subst);
            self.unify_receiver_param_for_inference(&param_ty, &receiver_arg.ty, &mut subst);
        }
        let mut checked_args = Vec::new();
        for (idx, arg) in args.iter().enumerate() {
            let Some(param) = interface.params.get(idx) else {
                continue;
            };
            if idx == receiver_index {
                if idx > 0 {
                    self.diagnose_prechecked_affine_expr_moved(scopes, &receiver_arg);
                }
                checked_args.push(self.consume_affine_expr(scopes, receiver_arg.clone(), false));
                continue;
            }
            let param_ty = self.lower_type_with_subst(&param.ty, &subst);
            let checked = if contains_generic(&param_ty) {
                self.check_expr(scopes, arg, None)?
            } else {
                self.check_expr(scopes, arg, Some(&param_ty))?
            };
            self.unify_ty_for_inference(&param_ty, &checked.ty, &mut subst);
            checked_args.push(self.consume_affine_expr(scopes, checked, false));
        }
        if interface.params.len() != args.len() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "interface call `{name}` expects {} arguments, got {}",
                    interface.params.len(),
                    args.len()
                ),
            ));
        }
        self.infer_interface_determined_parameters(&interface, &mut subst);
        for generic in &interface.generics {
            if subst.get(generic).is_none_or(contains_generic)
                || subst
                    .get(generic)
                    .is_some_and(|ty| contains_type_hole(&self.resolve_type_holes(ty)))
            {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("could not infer interface generic parameter `{generic}` for `{name}`"),
                ));
                return None;
            }
        }
        let interface_args = interface
            .generics
            .iter()
            .filter_map(|generic| subst.get(generic).map(|ty| self.resolve_type_holes(ty)))
            .collect::<Vec<_>>();
        let receiver_ty = subst
            .get(&interface.generics[0])
            .map(|ty| self.resolve_type_holes(ty));
        let non_receiver_args = interface_non_receiver_args(&interface_args);
        if let Some(receiver_ty) = receiver_ty.as_ref()
            && retained_closure_proves_capability(receiver_ty, def_id, &non_receiver_args)
        {
            let ret = self.lower_type_with_subst(&interface.ret, &subst);
            let receiver = checked_args.remove(receiver_index);
            let args = checked_args;
            return Some(TExpr {
                span,
                ty: ret,
                kind: TExprKind::RetainedClosureInterfaceCall {
                    interface_def: def_id,
                    interface_name: name.clone(),
                    interface_args: non_receiver_args.to_vec(),
                    receiver: Box::new(receiver),
                    args,
                },
            });
        }
        if std_id::is_std_message_clone_interface(&self.ctx.resolved, def_id)
            && interface_args.len() == 1
            && let Some(message_ty) = receiver_ty.as_ref()
            && let Some(witness_ty) = self.meta_repr_owned_message_witness_ty(message_ty)
            && self.type_implements_capability_by_def(def_id, &name, &[], &witness_ty)
        {
            let value = checked_args.remove(receiver_index);
            let ret = std_result_ty(message_ty.clone(), std_error_ty());
            return Some(TExpr {
                span,
                ty: ret,
                kind: TExprKind::CloneMessage {
                    value: Box::new(value),
                    message_ty: message_ty.clone(),
                },
            });
        }
        if let Some(implementation) = self.find_or_instantiate_impl_by_full_args(
            def_id,
            &name,
            &interface_args,
            receiver_ty.as_ref(),
            span,
        ) {
            let callee = TExpr {
                span,
                ty: Ty::Function {
                    is_unsafe: false,
                    abi: None,
                    ret: Box::new(implementation.ret.clone()),
                    params: implementation.params.clone(),
                },
                kind: TExprKind::Function(implementation.function_def, name.clone()),
            };
            return Some(TExpr {
                span,
                ty: implementation.ret.clone(),
                kind: TExprKind::Call {
                    callee: Box::new(callee),
                    args: checked_args,
                },
            });
        }

        let message = if std_id::is_std_message_clone_interface(&self.ctx.resolved, def_id) {
            receiver_ty
                .as_ref()
                .map(|ty| format!("`{ty}` does not implement `Message`"))
                .unwrap_or_else(|| format!("no impl of `{name}` for this call"))
        } else {
            format!("no impl of `{name}` for this call")
        };
        self.diagnostics.push(Diagnostic::new(span, message));
        None
    }

    pub(super) fn check_dynamic_interface_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        interface: InterfaceSig,
        interface_args: &[Ty],
        receiver: TExpr,
        args: &[Expr],
    ) -> Option<TExpr> {
        let Ty::DynamicInterface { .. } = &receiver.ty else {
            return None;
        };
        let mut subst = HashMap::<String, Ty>::new();
        if let Some(receiver_generic) = interface.generics.first() {
            subst.insert(
                receiver_generic.clone(),
                Ty::Generic(receiver_generic.clone()),
            );
        }
        for (generic, arg) in interface.generics.iter().skip(1).zip(interface_args.iter()) {
            subst.insert(generic.clone(), arg.clone());
        }
        if interface.generics.len() > 1 && interface_args.len() != interface.generics.len() - 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "dynamic interface `{}` requires {} non-receiver type arguments",
                    interface.name,
                    interface.generics.len() - 1
                ),
            ));
            return None;
        }
        if args.len() + 1 != interface.params.len() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "dynamic interface call `{}` expects {} trailing arguments, got {}",
                    interface.name,
                    interface.params.len().saturating_sub(1),
                    args.len()
                ),
            ));
        }
        let mut checked_args = Vec::new();
        for (arg, param) in args.iter().zip(interface.params.iter().skip(1)) {
            let param_ty = self.lower_type_with_subst(&param.ty, &subst);
            let checked = self.check_consumed_expr(scopes, arg, Some(&param_ty), false)?;
            self.require_assignable(&param_ty, &checked.ty, checked.span);
            checked_args.push(checked);
        }
        let ret = self.lower_type_with_subst(&interface.ret, &subst);
        Some(TExpr {
            span,
            ty: ret,
            kind: TExprKind::DynamicInterfaceCall {
                interface_def: interface.def_id,
                interface_name: interface.name,
                receiver: Box::new(receiver),
                args: checked_args,
            },
        })
    }

    pub(super) fn infer_interface_determined_parameters(
        &mut self,
        interface: &InterfaceSig,
        subst: &mut HashMap<String, Ty>,
    ) {
        let Some(determined_start) = interface.determined_start else {
            return;
        };
        let mut hidden_names = HashSet::new();
        for generic in interface.generics.iter().skip(determined_start) {
            if matches!(subst.get(generic), Some(Ty::Generic(name)) if name == generic) {
                hidden_names.insert(generic.clone());
            }
        }
        if hidden_names.is_empty() {
            return;
        }
        let full_args = interface
            .generics
            .iter()
            .map(|generic| {
                subst
                    .get(generic)
                    .cloned()
                    .unwrap_or_else(|| Ty::Generic(generic.clone()))
            })
            .collect::<Vec<_>>();
        let Some(receiver_ty) = full_args.first().cloned() else {
            return;
        };
        let capability = ConstraintRef {
            def_id: interface.def_id,
            name: interface.name.clone(),
            args: full_args.get(1..).unwrap_or(&[]).to_vec(),
        };
        self.solve_hidden_from_capability(&receiver_ty, &capability, &hidden_names, subst);
    }

    pub(super) fn unify_receiver_param_for_inference(
        &mut self,
        pattern: &Ty,
        actual: &Ty,
        subst: &mut HashMap<String, Ty>,
    ) -> bool {
        match pattern {
            Ty::Pointer { inner, .. } => match actual {
                Ty::Pointer {
                    inner: actual_inner,
                    ..
                } => self.unify_ty_for_inference(inner, actual_inner, subst),
                _ => self.unify_ty_for_inference(inner, actual, subst),
            },
            _ => self.unify_ty_for_inference(pattern, actual, subst),
        }
    }
}
