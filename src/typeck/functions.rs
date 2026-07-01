use super::*;

impl TypeChecker {
    pub(super) fn collect_functions(&mut self) {
        let modules = self.ctx.hir_modules.clone();
        for module in &modules {
            self.current_module = module.id;
            for item in &module.items {
                match &item.kind {
                    ItemKind::Function(function) => {
                        let Some(def_id) = item.def_ids.iter().copied().find(|def_id| {
                            let def = self.ctx.resolved.def(*def_id);
                            def.name == function.signature.name.name
                                && matches!(def.kind, DefKind::Function | DefKind::ExternFunction)
                        }) else {
                            continue;
                        };
                        let exported = self.ctx.resolved.def(def_id).exported;
                        let is_generic = !function.signature.generics.is_empty();
                        self.insert_function_sig(
                            def_id,
                            module.id,
                            &function.signature,
                            function.is_unsafe,
                            function.is_async,
                            function.abi.clone(),
                            false,
                            function.body.is_some(),
                            exported,
                        );
                        if is_generic {
                            self.ctx.generic_functions.insert(
                                def_id,
                                GenericFunctionTemplate {
                                    function: function.clone(),
                                    exported,
                                },
                            );
                        }
                    }
                    ItemKind::ExternBlock(block) => {
                        for extern_item in &block.items {
                            if let ExternItem::Function {
                                noescape,
                                signature,
                            } = extern_item
                            {
                                let Some(def_id) = item.def_ids.iter().copied().find(|def_id| {
                                    let def = self.ctx.resolved.def(*def_id);
                                    def.name == signature.name.name
                                        && def.kind == DefKind::ExternFunction
                                }) else {
                                    continue;
                                };
                                let exported = self.ctx.resolved.def(def_id).exported;
                                self.insert_function_sig(
                                    def_id,
                                    module.id,
                                    signature,
                                    block.is_unsafe,
                                    false,
                                    Some(block.abi.clone()),
                                    *noescape,
                                    false,
                                    exported,
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    pub(super) fn collect_receiver_selectors(&mut self) {
        self.ctx.receiver_selectors.clear();
        let modules = self.ctx.hir_modules.clone();
        for module in &modules {
            for item in &module.items {
                match &item.kind {
                    ItemKind::Interface(decl) => {
                        let Some(selector) = decl.signature.receiver_selector.as_ref() else {
                            continue;
                        };
                        let Some(def_id) = self.ctx.resolved.local_def(
                            module.id,
                            &decl.signature.name.name,
                            &[DefKind::Interface],
                        ) else {
                            continue;
                        };
                        let exported = self.ctx.resolved.def(def_id).exported;
                        self.collect_receiver_selector_for_signature(
                            module.id,
                            &decl.signature,
                            selector,
                            exported,
                            ReceiverSelectorCallable::Interface(def_id),
                        );
                    }
                    ItemKind::Function(function) => {
                        let Some(selector) = function.signature.receiver_selector.as_ref() else {
                            continue;
                        };
                        let Some(def_id) = item.def_ids.iter().copied().find(|def_id| {
                            let def = self.ctx.resolved.def(*def_id);
                            def.name == function.signature.name.name
                                && matches!(def.kind, DefKind::Function | DefKind::ExternFunction)
                        }) else {
                            continue;
                        };
                        if function.abi.as_deref() == Some("C") && function.body.is_none() {
                            self.diagnostics.push(Diagnostic::new(
                                selector.span,
                                "imported C function declarations cannot attach receiver selectors",
                            ));
                            continue;
                        }
                        let exported = self.ctx.resolved.def(def_id).exported;
                        self.collect_receiver_selector_for_signature(
                            module.id,
                            &function.signature,
                            selector,
                            exported,
                            ReceiverSelectorCallable::Function(def_id),
                        );
                    }
                    ItemKind::ExternBlock(block) => {
                        for extern_item in &block.items {
                            let ExternItem::Function { signature, .. } = extern_item else {
                                continue;
                            };
                            if let Some(selector) = signature.receiver_selector.as_ref() {
                                self.diagnostics.push(Diagnostic::new(
                                    selector.span,
                                    "imported C function declarations cannot attach receiver selectors",
                                ));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    pub(super) fn collect_receiver_selector_for_signature(
        &mut self,
        module: ModuleId,
        signature: &FunctionSignature,
        selector: &ReceiverSelector,
        exported: bool,
        callable: ReceiverSelectorCallable,
    ) {
        if signature.params.is_empty() {
            self.diagnostics.push(Diagnostic::new(
                selector.span,
                format!(
                    "receiver selector `.{}` requires at least one parameter",
                    selector.name.name
                ),
            ));
            return;
        }
        let receiver_index = if let Some(receiver_param) = &selector.receiver_param {
            let Some(index) = signature
                .params
                .iter()
                .position(|param| param.name.name == receiver_param.name)
            else {
                self.diagnostics.push(Diagnostic::new(
                    receiver_param.span,
                    format!(
                        "unknown receiver parameter `{}` for selector `.{}`",
                        receiver_param.name, selector.name.name
                    ),
                ));
                return;
            };
            index
        } else {
            0
        };
        self.ctx.receiver_selectors.push(ReceiverSelectorSig {
            selector: selector.name.name.clone(),
            module,
            exported,
            receiver_index,
            span: selector.span,
            callable,
        });
    }

    pub(super) fn validate_receiver_selector_conflicts(&mut self) {
        let selectors = self.ctx.receiver_selectors.clone();
        for (idx, left) in selectors.iter().enumerate() {
            for right in selectors.iter().skip(idx + 1) {
                if left.module != right.module || left.selector != right.selector {
                    continue;
                }
                if self.receiver_selector_patterns_overlap(left, right) {
                    let receiver = self
                        .receiver_selector_pattern_ty(left)
                        .map(|ty| receiver_selector_root_ty(&ty).to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    self.diagnostics.push(Diagnostic::new(
                        right.span,
                        format!(
                            "conflicting selector `.{}` for receiver `{receiver}`; selector declarations are not overloaded by non-receiver arguments",
                            right.selector
                        ),
                    ));
                }
            }
        }
    }

    pub(super) fn receiver_selector_patterns_overlap(
        &mut self,
        left: &ReceiverSelectorSig,
        right: &ReceiverSelectorSig,
    ) -> bool {
        let Some(left_ty) = self.receiver_selector_pattern_ty(left) else {
            return false;
        };
        let Some(right_ty) = self.receiver_selector_pattern_ty(right) else {
            return false;
        };
        let left_root = receiver_selector_root_ty(&left_ty);
        let right_root = receiver_selector_root_ty(&right_ty);
        let mut subst = HashMap::new();
        if unify_ty(&left_root, &right_root, &mut subst) {
            return true;
        }
        let mut subst = HashMap::new();
        unify_ty(&right_root, &left_root, &mut subst)
    }

    pub(super) fn receiver_selector_pattern_ty(
        &mut self,
        selector: &ReceiverSelectorSig,
    ) -> Option<Ty> {
        match selector.callable {
            ReceiverSelectorCallable::Function(def_id) => self
                .ctx
                .functions_by_def
                .get(&def_id)
                .and_then(|sig| sig.params.get(selector.receiver_index).cloned()),
            ReceiverSelectorCallable::Interface(def_id) => {
                let interface = self.ctx.interfaces.get(&def_id).cloned()?;
                let subst = interface
                    .generics
                    .iter()
                    .cloned()
                    .map(|name| (name.clone(), Ty::Generic(name)))
                    .collect::<HashMap<_, _>>();
                interface
                    .params
                    .get(selector.receiver_index)
                    .map(|param| self.lower_type_with_subst(&param.ty, &subst))
            }
        }
    }

    pub(super) fn collect_impl_signatures(&mut self) {
        let mut modules = self.ctx.hir_modules.clone();
        modules.sort_by_key(|module| {
            (
                !std_id::is_std_module(&self.ctx.resolved, module.id),
                module.id.0,
            )
        });
        for module in &modules {
            for item in &module.items {
                let ItemKind::Impl(decl) = &item.kind else {
                    continue;
                };
                self.current_module = module.id;
                let Some(interface_def) =
                    self.name_def_of_kind(&decl.name, &[DefKind::Interface], "interface")
                else {
                    self.diagnostics.push(Diagnostic::new(
                        decl.name.span,
                        format!("unknown interface `{}` in impl", decl.name.display),
                    ));
                    continue;
                };
                let Some(interface) = self.ctx.interfaces.get(&interface_def).cloned() else {
                    continue;
                };
                if interface.is_unsafe && !decl.is_unsafe {
                    self.diagnostics.push(Diagnostic::new(
                        decl.name.span,
                        format!(
                            "impl `{}` requires `unsafe impl` because the interface is unsafe",
                            interface.name
                        ),
                    ));
                    continue;
                }
                if !interface.is_unsafe && decl.is_unsafe {
                    self.diagnostics.push(Diagnostic::new(
                        decl.name.span,
                        format!(
                            "`unsafe impl` cannot implement safe interface `{}`",
                            interface.name
                        ),
                    ));
                    continue;
                }
                if self.is_compiler_provided_meta_marker_def(interface_def) {
                    self.diagnostics.push(Diagnostic::new(
                        decl.name.span,
                        format!(
                            "`{}` is a compiler-provided marker and cannot be implemented in source",
                            interface.name
                        ),
                    ));
                    continue;
                }

                let Some(analysis) = self.analyze_impl_signature(item.span, decl, &interface)
                else {
                    continue;
                };
                if self.resource_handle_message_impl_forbidden(decl.name.span, &analysis) {
                    continue;
                }
                if self.generic_marker_impl_overlaps_existing(item.span, &analysis) {
                    continue;
                }
                if analysis.generics.is_empty() {
                    if self
                        .find_impl_by_full_args(
                            analysis.interface_def,
                            &analysis.interface_name,
                            &analysis.interface_args,
                            analysis.receiver_ty.as_ref(),
                        )
                        .is_some()
                    {
                        self.diagnostics.push(Diagnostic::new(
                            decl.name.span,
                            format!(
                                "conflicting impl of `{}` for this receiver",
                                analysis.interface_name
                            ),
                        ));
                        continue;
                    }
                    if let Some(pending) = self.register_impl_signature(
                        module.id,
                        decl,
                        analysis.interface_def,
                        &analysis.interface_name,
                        analysis.interface_args,
                        analysis.receiver_ty,
                        analysis.ret,
                        analysis.params,
                    ) {
                        self.queue_impl_body(pending, HashMap::new());
                    }
                } else {
                    self.ctx.generic_impls.push(GenericImplTemplate {
                        module: module.id,
                        item_span: item.span,
                        interface_def: analysis.interface_def,
                        interface_name: analysis.interface_name,
                        generics: analysis.generics,
                        generic_constraints: analysis.generic_constraints,
                        interface_args: analysis.interface_args,
                        receiver_ty: analysis.receiver_ty,
                        ret: analysis.ret,
                        params: analysis.params,
                        decl: decl.clone(),
                    });
                }
            }
        }
    }

    pub(super) fn generic_marker_impl_overlaps_existing(
        &mut self,
        span: crate::span::Span,
        analysis: &ImplAnalysis,
    ) -> bool {
        if analysis.generics.is_empty()
            || !self.is_std_message_capability_interface_def(analysis.interface_def)
        {
            return false;
        }
        let current_domain =
            self.compiler_marker_domain_for_impl(&analysis.generics, analysis.receiver_ty.as_ref());
        let templates = self.ctx.generic_impls.clone();
        for template in &templates {
            if template.interface_def != analysis.interface_def {
                continue;
            }
            let template_domain = self
                .compiler_marker_domain_for_impl(&template.generics, template.receiver_ty.as_ref());
            if capability::marker_impl_domains_disjoint(
                current_domain,
                analysis.receiver_ty.as_ref(),
                template_domain,
                template.receiver_ty.as_ref(),
            ) {
                continue;
            }
            if capability::marker_impl_patterns_overlap(
                &template.interface_args,
                template.receiver_ty.as_ref(),
                &analysis.interface_args,
                analysis.receiver_ty.as_ref(),
            ) {
                self.fatal_impl_coherence_error = true;
                self.push_diagnostic_once(
                    span,
                    format!(
                        "ambiguous generic impls for marker interface `{}`",
                        analysis.interface_name
                    ),
                );
                return true;
            }
        }
        for existing in &self.ctx.impls {
            if existing.interface_def != analysis.interface_def {
                continue;
            }
            if capability::marker_impl_domains_disjoint(
                current_domain,
                analysis.receiver_ty.as_ref(),
                None,
                existing.receiver_ty.as_ref(),
            ) {
                continue;
            }
            if capability::marker_impl_patterns_overlap(
                &existing.interface_args,
                existing.receiver_ty.as_ref(),
                &analysis.interface_args,
                analysis.receiver_ty.as_ref(),
            ) {
                self.fatal_impl_coherence_error = true;
                self.push_diagnostic_once(
                    span,
                    format!(
                        "generic marker impl for `{}` conflicts with an existing concrete impl",
                        analysis.interface_name
                    ),
                );
                return true;
            }
        }
        false
    }

    pub(super) fn resource_handle_message_impl_forbidden(
        &mut self,
        span: crate::span::Span,
        analysis: &ImplAnalysis,
    ) -> bool {
        let is_forbidden_interface = self
            .is_std_message_clone_interface_def(analysis.interface_def)
            || self.is_std_message_share_handle_marker_def(analysis.interface_def);
        if !is_forbidden_interface {
            return false;
        }
        let Some(receiver_ty) = analysis.receiver_ty.as_ref() else {
            return false;
        };
        if !self.type_is_affine(receiver_ty) {
            return false;
        }
        self.diagnostics.push(Diagnostic::new(
            span,
            format!(
                "resource type `{receiver_ty}` cannot implement `{}`",
                analysis.interface_name
            ),
        ));
        true
    }

    pub(super) fn push_diagnostic_once(
        &mut self,
        span: impl Into<Option<crate::span::Span>>,
        message: impl Into<String>,
    ) {
        let message = message.into();
        if self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message == message)
        {
            return;
        }
        self.diagnostics.push(Diagnostic::new(span, message));
    }

    pub(super) fn compiler_marker_domain_for_impl(
        &mut self,
        generics: &[GenericInfo],
        receiver_ty: Option<&Ty>,
    ) -> Option<CompilerMarkerDomain> {
        let Ty::Generic(receiver_name) = receiver_ty? else {
            return None;
        };
        let generic = generics
            .iter()
            .find(|generic| generic.name == *receiver_name)?;
        let constraint = generic.constraint.as_ref()?;
        let subst = generics
            .iter()
            .map(|generic| (generic.name.clone(), Ty::Generic(generic.name.clone())))
            .collect::<HashMap<_, _>>();
        let bounds = self.constraint_bounds(constraint, &subst);
        let has_ciel_fn = bounds.positive.iter().any(|entry| {
            self.is_std_meta_ciel_fn_value_marker_def(entry.def_id) && entry.args.is_empty()
        });
        let has_closure = bounds.positive.iter().any(|entry| {
            self.is_std_meta_closure_value_marker_def(entry.def_id) && entry.args.is_empty()
        });
        match (has_ciel_fn, has_closure) {
            (true, false) => Some(CompilerMarkerDomain::CielFnValue),
            (false, true) => Some(CompilerMarkerDomain::ClosureValue),
            _ => None,
        }
    }

    pub(super) fn analyze_impl_signature(
        &mut self,
        span: crate::span::Span,
        decl: &ImplDecl,
        interface: &InterfaceSig,
    ) -> Option<ImplAnalysis> {
        if decl.params.len() != interface.params.len() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "impl `{}` expects {} parameters, got {}",
                    interface.name,
                    interface.params.len(),
                    decl.params.len()
                ),
            ));
            return None;
        }

        let generics = decl
            .generics
            .iter()
            .map(|param| GenericInfo {
                name: param.name.name.clone(),
                is_resource: param.is_resource,
                is_hidden: param.is_hidden,
                constraint: param.constraint.clone(),
            })
            .collect::<Vec<_>>();
        self.validate_generic_bindings(&format!("impl {}", interface.name), &generics);
        self.with_generic_env(&generics, |checker| {
            checker.analyze_impl_signature_with_generics(span, decl, interface, generics.clone())
        })
    }

    pub(super) fn analyze_impl_signature_with_generics(
        &mut self,
        span: crate::span::Span,
        decl: &ImplDecl,
        interface: &InterfaceSig,
        generics: Vec<GenericInfo>,
    ) -> Option<ImplAnalysis> {
        let impl_subst = generics
            .iter()
            .map(|param| (param.name.clone(), Ty::Generic(param.name.clone())))
            .collect::<HashMap<_, _>>();
        let interface_placeholders = interface
            .generics
            .iter()
            .map(|name| {
                (
                    name.clone(),
                    interface_generic_placeholder(&interface.name, name),
                )
            })
            .collect::<HashMap<_, _>>();
        let interface_lower_subst = interface_placeholders
            .iter()
            .map(|(name, placeholder)| (name.clone(), Ty::Generic(placeholder.clone())))
            .collect::<HashMap<_, _>>();
        let mut inferred = interface_placeholders
            .values()
            .cloned()
            .map(|placeholder| (placeholder.clone(), Ty::Generic(placeholder)))
            .collect::<HashMap<_, _>>();

        for (idx, arg) in decl.args.iter().enumerate() {
            let Some(generic_name) = interface.generics.iter().skip(1).nth(idx) else {
                self.diagnostics.push(Diagnostic::new(
                    arg.span,
                    format!("too many type arguments for impl `{}`", interface.name),
                ));
                return None;
            };
            let placeholder = interface_placeholders
                .get(generic_name)
                .expect("interface generic has placeholder");
            let concrete = self.lower_type_with_subst(arg, &impl_subst);
            inferred.insert(placeholder.clone(), concrete);
        }

        let impl_params = decl
            .params
            .iter()
            .map(|param| {
                let ty = self.lower_type_with_subst(&param.ty, &impl_subst);
                self.reject_invalid_plain_value_type(&ty, param.ty.span, "impl parameter");
                ty
            })
            .collect::<Vec<_>>();
        for (interface_param, impl_param) in interface.params.iter().zip(impl_params.iter()) {
            let expected = self.lower_type_with_subst(&interface_param.ty, &interface_lower_subst);
            unify_ty(&expected, impl_param, &mut inferred);
        }
        let lowered_ret = self.lower_type_with_subst(&interface.ret, &interface_lower_subst);
        let ret = self.substitute_ty_normalized(&lowered_ret, &inferred, span);
        let expected_params = interface
            .params
            .iter()
            .map(|param| {
                let ty = self.lower_type_with_subst(&param.ty, &interface_lower_subst);
                self.substitute_ty_normalized(&ty, &inferred, param.ty.span)
            })
            .collect::<Vec<_>>();
        let placeholder_names = interface_placeholders
            .values()
            .cloned()
            .collect::<HashSet<_>>();
        if contains_any_generic_name(&ret, &placeholder_names)
            || expected_params
                .iter()
                .any(|ty| contains_any_generic_name(ty, &placeholder_names))
        {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "impl `{}` leaves interface generic parameters unresolved",
                    interface.name
                ),
            ));
            return None;
        }
        for (expected, actual) in expected_params.iter().zip(impl_params.iter()) {
            if expected != actual {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "impl `{}` parameter mismatch: expected `{expected}`, got `{actual}`",
                        interface.name
                    ),
                ));
            }
        }
        let interface_args = interface
            .generics
            .iter()
            .map(|name| {
                let placeholder = interface_placeholders
                    .get(name)
                    .expect("interface generic has placeholder");
                inferred.get(placeholder).cloned().unwrap_or(Ty::Unknown)
            })
            .collect::<Vec<_>>();
        let receiver_ty = interface.generics.first().and_then(|name| {
            let placeholder = interface_placeholders.get(name)?;
            inferred.get(placeholder).cloned()
        });
        let generic_constraints = generics
            .iter()
            .map(|generic| GenericConstraintBounds {
                name: generic.name.clone(),
                is_resource: generic.is_resource,
                bounds: generic
                    .constraint
                    .as_ref()
                    .map(|constraint| self.constraint_bounds(constraint, &impl_subst))
                    .unwrap_or_default(),
            })
            .collect();
        Some(ImplAnalysis {
            interface_def: interface.def_id,
            interface_name: interface.name.clone(),
            generics,
            generic_constraints,
            interface_args,
            receiver_ty,
            ret,
            params: impl_params,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn instantiate_impl_body(
        &mut self,
        module: ModuleId,
        decl: &ImplDecl,
        interface_def: DefId,
        interface_name: &str,
        interface_args: Vec<Ty>,
        receiver_ty: Option<Ty>,
        ret: Ty,
        params_ty: Vec<Ty>,
        subst: &HashMap<String, Ty>,
    ) -> Option<ImplSig> {
        if let Some(existing) = self.find_impl_by_full_args(
            interface_def,
            interface_name,
            &interface_args,
            receiver_ty.as_ref(),
        ) {
            if subst.is_empty() {
                self.diagnostics.push(Diagnostic::new(
                    decl.name.span,
                    format!("conflicting impl of `{interface_name}` for this receiver"),
                ));
            }
            return Some(existing);
        }
        let pending = self.register_impl_signature(
            module,
            decl,
            interface_def,
            interface_name,
            interface_args,
            receiver_ty,
            ret,
            params_ty,
        )?;
        let implementation = pending.implementation.clone();
        self.queue_impl_body(pending, subst.clone());
        Some(implementation)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn register_impl_signature(
        &mut self,
        module: ModuleId,
        decl: &ImplDecl,
        interface_def: DefId,
        interface_name: &str,
        interface_args: Vec<Ty>,
        receiver_ty: Option<Ty>,
        ret: Ty,
        params_ty: Vec<Ty>,
    ) -> Option<PendingImplBody> {
        if self
            .find_impl_by_full_args(
                interface_def,
                interface_name,
                &interface_args,
                receiver_ty.as_ref(),
            )
            .is_some()
        {
            return None;
        }

        let function_def = self.alloc_synthetic_def();
        let function_name = impl_function_name(interface_name, &params_ty);
        let sig = FunctionSig {
            def_id: function_def,
            module,
            name: function_name.clone(),
            is_unsafe: false,
            is_async: false,
            abi: None,
            noescape: false,
            has_body: true,
            ret: ret.clone(),
            params: params_ty.clone(),
            generics: Vec::new(),
            exported: false,
        };
        self.ctx.functions_by_def.insert(function_def, sig.clone());
        self.ensure_struct_instance(&ret);
        self.ensure_enum_instance(&ret);
        for param in &params_ty {
            self.ensure_struct_instance(param);
            self.ensure_enum_instance(param);
        }
        let implementation = ImplSig {
            interface_def,
            interface_name: interface_name.to_string(),
            interface_args,
            receiver_ty,
            function_def,
            ret,
            params: params_ty,
        };
        self.ctx.impls.push(implementation.clone());
        Some(PendingImplBody {
            decl: decl.clone(),
            module,
            function_name,
            function_sig: sig,
            implementation,
        })
    }

    pub(super) fn check_registered_impl_body(
        &mut self,
        pending: &PendingImplBody,
        subst: &HashMap<String, Ty>,
    ) {
        let params = pending
            .decl
            .params
            .iter()
            .zip(pending.function_sig.params.iter())
            .map(|(param, ty)| (param.local_id, param.name.name.clone(), ty.clone()))
            .collect::<Vec<_>>();
        let body_params = pending
            .decl
            .params
            .iter()
            .zip(pending.function_sig.params.iter())
            .filter_map(|(param, ty)| {
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
        let previous_module = self.current_module;
        self.current_module = pending.module;
        self.type_subst_stack.push(subst.clone());
        let body =
            self.check_function_body(&pending.function_sig, &body_params, &pending.decl.body);
        self.type_subst_stack.pop();
        self.current_module = previous_module;
        if let Some(body) = body {
            self.generated_functions.push(CheckedFunction {
                def_id: pending.function_sig.def_id,
                name: pending.function_name.clone(),
                is_unsafe: false,
                is_async: false,
                async_facts: body.async_facts,
                abi: None,
                noescape: false,
                exported: false,
                ret: pending.function_sig.ret.clone(),
                params,
                body: Some(body.block),
            });
        }
    }

    pub(super) fn queue_impl_body(&mut self, pending: PendingImplBody, subst: HashMap<String, Ty>) {
        if self
            .generated_functions
            .iter()
            .any(|function| function.def_id == pending.function_sig.def_id)
            || self
                .pending_impl_bodies
                .iter()
                .any(|queued| queued.pending.function_sig.def_id == pending.function_sig.def_id)
        {
            return;
        }
        self.pending_impl_bodies
            .push(QueuedImplBody { pending, subst });
    }

    pub(super) fn drain_pending_impl_bodies(&mut self) {
        while let Some(queued) = self.pending_impl_bodies.pop() {
            if self
                .generated_functions
                .iter()
                .any(|function| function.def_id == queued.pending.function_sig.def_id)
            {
                continue;
            }
            self.check_registered_impl_body(&queued.pending, &queued.subst);
        }
    }

    pub(super) fn insert_function_sig(
        &mut self,
        def_id: DefId,
        module: ModuleId,
        signature: &FunctionSignature,
        is_unsafe: bool,
        is_async: bool,
        abi: Option<String>,
        noescape: bool,
        has_body: bool,
        exported: bool,
    ) {
        let generics = signature
            .generics
            .iter()
            .map(|param| GenericInfo {
                name: param.name.name.clone(),
                is_resource: param.is_resource,
                is_hidden: param.is_hidden,
                constraint: param.constraint.clone(),
            })
            .collect::<Vec<_>>();
        self.validate_generic_bindings(&signature.name.name, &generics);
        if let FunctionReturnType::OpaqueConstraint {
            marker_span,
            constraint,
        } = &signature.ret
        {
            self.validate_constraint_bindings_forbidden(constraint, "opaque return constraints");
            if !has_body {
                self.diagnostics.push(Diagnostic::new(
                    *marker_span,
                    "opaque return type requires a function body",
                ));
            }
            if abi.is_some() {
                self.diagnostics.push(Diagnostic::new(
                    *marker_span,
                    "opaque return type cannot be used on an extern function",
                ));
            }
            if exported && abi.as_deref() == Some("C") {
                self.diagnostics.push(Diagnostic::new(
                    *marker_span,
                    "opaque return type cannot be used on an exported C ABI function",
                ));
            }
        }
        let subst = generics
            .iter()
            .map(|param| (param.name.clone(), Ty::Generic(param.name.clone())))
            .collect::<HashMap<_, _>>();
        let previous_defer_meta_repr_expansion =
            std::mem::replace(&mut self.defer_meta_repr_expansion, true);
        let sig = self.with_generic_env(&generics, |checker| FunctionSig {
            def_id,
            module,
            name: signature.name.name.clone(),
            is_unsafe,
            is_async,
            abi,
            noescape,
            has_body,
            ret: checker.lower_function_return_type(def_id, &signature.ret, &generics, &subst),
            params: signature
                .params
                .iter()
                .map(|param| {
                    let ty = checker.lower_type_with_subst(&param.ty, &subst);
                    checker.reject_invalid_plain_value_type(
                        &ty,
                        param.ty.span,
                        "function parameter",
                    );
                    ty
                })
                .collect(),
            generics: generics.clone(),
            exported,
        });
        self.defer_meta_repr_expansion = previous_defer_meta_repr_expansion;
        self.reject_invalid_return_type(&sig.ret, signature.ret.span());
        self.ctx
            .functions_by_name
            .entry(signature.name.name.clone())
            .or_default()
            .push(def_id);
        self.ctx.functions_by_def.insert(def_id, sig);
    }

    pub(super) fn normalize_function_sigs(&mut self) {
        let mut normalized = HashMap::new();
        let sigs = self
            .ctx
            .functions_by_def
            .iter()
            .map(|(def_id, sig)| (*def_id, sig.clone()))
            .collect::<Vec<_>>();
        for (def_id, mut sig) in sigs {
            let span = self.ctx.resolved.defs.get(def_id.0).map(|def| def.span);
            sig.ret = self.normalize_meta_repr_markers(&sig.ret, span);
            sig.params = sig
                .params
                .iter()
                .map(|param| self.normalize_meta_repr_markers(param, span))
                .collect();
            normalized.insert(def_id, sig);
        }
        self.ctx.functions_by_def = normalized;
    }

    pub(super) fn validate_c_abi_functions(&mut self) {
        let mut by_symbol: HashMap<String, Vec<FunctionSig>> = HashMap::new();
        let sigs = self
            .ctx
            .functions_by_def
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for sig in &sigs {
            if sig.abi.as_deref() != Some("C") {
                continue;
            }
            if !sig.has_body && !sig.is_unsafe {
                self.diagnostics.push(Diagnostic::new(
                    self.ctx.resolved.def(sig.def_id).span,
                    "imported C function declarations must be in `unsafe extern \"C\"` blocks",
                ));
            }
            if sig.is_async {
                self.diagnostics.push(Diagnostic::new(
                    self.ctx.resolved.def(sig.def_id).span,
                    "`extern \"C\"` functions cannot be async",
                ));
            }
            if sig.has_body && !sig.exported {
                self.diagnostics.push(Diagnostic::new(
                    self.ctx.resolved.def(sig.def_id).span,
                    "`extern \"C\"` function bodies must be declared with `export`",
                ));
            }
            if !sig.generics.is_empty() {
                self.diagnostics.push(Diagnostic::new(
                    self.ctx.resolved.def(sig.def_id).span,
                    "`extern \"C\"` functions cannot be generic",
                ));
            }
            if type_contains_closure(&sig.ret) || sig.params.iter().any(type_contains_closure) {
                self.diagnostics.push(Diagnostic::new(
                    self.ctx.resolved.def(sig.def_id).span,
                    "closure types are not allowed in extern C declarations",
                ));
            }
            if sig.params.iter().any(Ty::is_erased_value) {
                self.diagnostics.push(Diagnostic::new(
                    self.ctx.resolved.def(sig.def_id).span,
                    "`extern \"C\"` parameters cannot have type `void` by value",
                ));
            }
            by_symbol
                .entry(sig.name.clone())
                .or_default()
                .push(sig.clone());
        }

        for (symbol, mut sigs) in by_symbol {
            sigs.sort_by_key(|sig| sig.def_id.0);
            let Some(first) = sigs.first() else {
                continue;
            };
            for sig in sigs.iter().skip(1) {
                if sig.ret != first.ret || sig.params != first.params {
                    self.diagnostics.push(Diagnostic::new(
                        self.ctx.resolved.def(sig.def_id).span,
                        format!("conflicting `extern \"C\"` declarations for symbol `{symbol}`"),
                    ));
                }
            }
            let definitions = sigs.iter().filter(|sig| sig.has_body).collect::<Vec<_>>();
            if definitions.len() > 1 {
                for sig in definitions.iter().skip(1) {
                    self.diagnostics.push(Diagnostic::new(
                        self.ctx.resolved.def(sig.def_id).span,
                        format!("multiple definitions of C ABI symbol `{symbol}`"),
                    ));
                }
            }
        }
    }

    pub(super) fn check_function_item(
        &mut self,
        function: &FunctionDecl,
        exported: bool,
    ) -> Option<CheckedFunction> {
        let signature = &function.signature;
        if !signature.generics.is_empty() {
            return None;
        }
        let sig = self
            .function_sig_for(self.current_module, &signature.name.name)?
            .clone();
        let params = signature
            .params
            .iter()
            .zip(sig.params.iter())
            .map(|param| {
                let (param, ty) = param;
                (param.local_id, param.name.name.clone(), ty.clone())
            })
            .collect::<Vec<_>>();
        let body_params = signature
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
        let checked_body = function
            .body
            .as_ref()
            .and_then(|body| self.check_function_body(&sig, &body_params, body));
        let (body, async_facts) = checked_body
            .map(|checked| (Some(checked.block), checked.async_facts))
            .unwrap_or((None, None));

        Some(CheckedFunction {
            def_id: sig.def_id,
            name: sig.name,
            is_unsafe: sig.is_unsafe,
            is_async: sig.is_async,
            async_facts,
            abi: sig.abi,
            noescape: sig.noescape,
            exported,
            ret: sig.ret,
            params,
            body,
        })
    }

    pub(super) fn check_function_body(
        &mut self,
        sig: &FunctionSig,
        params: &[(LocalId, String, Ty, BindingMutability)],
        body: &Block,
    ) -> Option<CheckedFunctionBody> {
        let previous_return_ty = std::mem::replace(&mut self.current_return_ty, sig.ret.clone());
        let previous_opaque_return = std::mem::replace(
            &mut self.current_opaque_return,
            matches!(sig.ret, Ty::OpaqueReturn { .. }).then(|| OpaqueReturnState {
                opaque_ty: sig.ret.clone(),
                concrete_ty: None,
                saw_recursive_concrete_ty: false,
            }),
        );
        let previous_control_contexts = std::mem::take(&mut self.control_contexts);
        let previous_unsafe_depth = std::mem::replace(&mut self.unsafe_depth, 0);
        let previous_async_depth = std::mem::replace(
            &mut self.current_async_depth,
            if sig.is_async { 1 } else { 0 },
        );
        let resource_generics = Self::resource_generic_scope(&sig.generics);
        self.resource_generic_stack.push(resource_generics);
        let mut scopes = LocalScopes::default();
        scopes.push();
        for (local_id, name, ty, mutability) in params {
            if let Err(name) = scopes.insert(
                *local_id,
                Binding {
                    name: name.clone(),
                    ty: ty.clone(),
                    narrowed_ty: None,
                    init_state: InitState::Assigned,
                    mutability: *mutability,
                    captured: false,
                    declared_loop_depth: self.current_loop_depth,
                },
            ) {
                self.diagnostics.push(Diagnostic::new(
                    body.span,
                    format!("duplicate parameter `{name}`"),
                ));
            }
        }
        let checked = self.check_block_with_existing_scope(&mut scopes, body, &sig.ret);
        if sig.ret.is_never()
            && checked
                .as_ref()
                .is_some_and(|checked| checked.flow.can_fallthrough)
        {
            self.diagnostics.push(Diagnostic::new(
                body.span,
                format!(
                    "function `{}` with return type `never` can fall through",
                    sig.name
                ),
            ));
        } else if !sig.ret.is_erased_value()
            && !checked
                .as_ref()
                .is_some_and(|checked| !checked.flow.can_fallthrough)
        {
            self.diagnostics.push(Diagnostic::new(
                body.span,
                format!(
                    "function `{}` must return `{}` on every path",
                    sig.name, sig.ret
                ),
            ));
        }
        if matches!(sig.ret, Ty::OpaqueReturn { .. })
            && self.current_opaque_return.as_ref().is_some_and(|state| {
                state.concrete_ty.is_none() && !state.saw_recursive_concrete_ty
            })
            && checked
                .as_ref()
                .is_some_and(|checked| !checked.flow.can_fallthrough)
        {
            self.diagnostics.push(Diagnostic::new(
                body.span,
                format!(
                    "opaque return function `{}` does not return a concrete value",
                    sig.name
                ),
            ));
        }
        if let Some(state) = self.current_opaque_return.as_ref()
            && let Some(concrete_ty) = &state.concrete_ty
            && let Ty::OpaqueReturn { key, .. } = &state.opaque_ty
        {
            self.opaque_returns.insert(key.clone(), concrete_ty.clone());
        }
        let mut async_facts = None;
        if sig.is_async
            && let Some(checked) = checked.as_ref()
        {
            self.check_async_frame_safety(&checked.block, params);
            async_facts = Some(self.async_facts_for_block(&checked.block));
            let (cancel_safe, abortable) = self.async_block_capabilities(&checked.block);
            self.ctx
                .async_function_cancel_safety
                .insert(sig.def_id, cancel_safe);
            self.ctx
                .async_function_abortability
                .insert(sig.def_id, abortable);
        }
        self.current_return_ty = previous_return_ty;
        self.current_opaque_return = previous_opaque_return;
        self.control_contexts = previous_control_contexts;
        self.unsafe_depth = previous_unsafe_depth;
        self.current_async_depth = previous_async_depth;
        self.resource_generic_stack.pop();
        checked.map(|checked| CheckedFunctionBody {
            block: checked.block,
            async_facts,
        })
    }
}
