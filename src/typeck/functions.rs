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
                let decl = match &item.kind {
                    ItemKind::Impl(decl) => decl,
                    ItemKind::DerivableImpl(decl) => {
                        self.collect_derivable_impl_template(module.id, item.span, decl);
                        continue;
                    }
                    _ => continue,
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
                        self.queue_impl_body(pending, HashMap::new(), None);
                    }
                } else {
                    self.ctx.generic_impls.push(GenericImplTemplate {
                        module: module.id,
                        body_reflection_module: None,
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
                        body_subst: HashMap::new(),
                    });
                }
            }
        }
        for module in &modules {
            for item in &module.items {
                let ItemKind::Derive(decl) = &item.kind else {
                    continue;
                };
                self.current_module = module.id;
                self.collect_derive_decl(module.id, item.span, decl);
            }
        }
        self.drain_pending_derived_template_constraint_checks();
        self.drain_pending_derived_negative_checks();
    }

    fn collect_derivable_impl_template(
        &mut self,
        module: ModuleId,
        item_span: crate::span::Span,
        decl: &DerivableImplDecl,
    ) {
        self.current_module = module;
        let impl_decl = &decl.impl_decl;
        let Some(interface_def) =
            self.name_def_of_kind(&impl_decl.name, &[DefKind::Interface], "interface")
        else {
            self.diagnostics.push(Diagnostic::new(
                impl_decl.name.span,
                format!(
                    "unknown interface `{}` in derivable impl",
                    impl_decl.name.display
                ),
            ));
            return;
        };
        let Some(interface) = self.ctx.interfaces.get(&interface_def).cloned() else {
            return;
        };
        if interface.is_unsafe && !impl_decl.is_unsafe {
            self.diagnostics.push(Diagnostic::new(
                impl_decl.name.span,
                format!(
                    "derivable impl `{}` requires `unsafe impl` because the interface is unsafe",
                    interface.name
                ),
            ));
            return;
        }
        if !interface.is_unsafe && impl_decl.is_unsafe {
            self.diagnostics.push(Diagnostic::new(
                impl_decl.name.span,
                format!(
                    "`unsafe impl` cannot implement safe interface `{}`",
                    interface.name
                ),
            ));
            return;
        }
        if self.is_compiler_provided_meta_marker_def(interface_def) {
            self.diagnostics.push(Diagnostic::new(
                impl_decl.name.span,
                format!(
                    "`{}` is a compiler-provided marker and cannot be derived in source",
                    interface.name
                ),
            ));
            return;
        }
        let Some(analysis) = self.analyze_impl_signature(item_span, impl_decl, &interface) else {
            return;
        };
        if self.resource_handle_message_impl_forbidden(impl_decl.name.span, &analysis) {
            return;
        }
        let generic_names = analysis
            .generics
            .iter()
            .map(|generic| generic.name.clone())
            .collect::<HashSet<_>>();
        if interface_non_receiver_args(&analysis.interface_args)
            .iter()
            .any(|arg| contains_any_generic_name(arg, &generic_names))
        {
            self.diagnostics.push(Diagnostic::new(
                impl_decl.name.span,
                format!(
                    "derivable impl `{}` must fix all non-receiver interface arguments",
                    analysis.interface_name
                ),
            ));
            return;
        }
        if self.ctx.derivable_impls.iter().any(|existing| {
            existing.interface_def == analysis.interface_def
                && interface_non_receiver_args(&existing.interface_args)
                    == interface_non_receiver_args(&analysis.interface_args)
        }) {
            self.diagnostics.push(Diagnostic::new(
                impl_decl.name.span,
                format!(
                    "duplicate derivable impl for interface `{}`",
                    analysis.interface_name
                ),
            ));
            return;
        }
        self.ctx.derivable_impls.push(DerivableImplTemplate {
            module,
            requires_unsafe: decl.requires_unsafe,
            interface_def: analysis.interface_def,
            interface_name: analysis.interface_name,
            generics: analysis.generics,
            interface_args: analysis.interface_args,
            receiver_ty: analysis.receiver_ty,
            ret: analysis.ret,
            params: analysis.params,
            decl: impl_decl.clone(),
        });
    }

    fn collect_derive_decl(
        &mut self,
        module: ModuleId,
        item_span: crate::span::Span,
        decl: &DeriveDecl,
    ) {
        let derive_generics = decl
            .generics
            .iter()
            .map(|param| GenericInfo {
                name: param.name.name.clone(),
                is_resource: param.is_resource,
                is_hidden: param.is_hidden,
                constraint: param.constraint.clone(),
            })
            .collect::<Vec<_>>();
        self.validate_generic_bindings("derive declaration", &derive_generics);
        let resource_generics = Self::resource_generic_scope(&derive_generics);
        self.resource_generic_stack.push(resource_generics);
        self.with_generic_env(&derive_generics, |checker| {
            checker.collect_derive_decl_with_generics(
                module,
                item_span,
                decl,
                derive_generics.clone(),
            );
        });
        self.resource_generic_stack.pop();
    }

    fn collect_derive_decl_with_generics(
        &mut self,
        module: ModuleId,
        item_span: crate::span::Span,
        decl: &DeriveDecl,
        derive_generics: Vec<GenericInfo>,
    ) {
        if decl.args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                decl.name.span,
                format!(
                    "derive target `{}` expects exactly one receiver type argument, got {}",
                    decl.name.display,
                    decl.args.len()
                ),
            ));
            return;
        }
        let derive_subst = derive_generics
            .iter()
            .map(|generic| (generic.name.clone(), Ty::Generic(generic.name.clone())))
            .collect::<HashMap<_, _>>();
        let receiver_ty = self.lower_type_with_subst(&decl.args[0], &derive_subst);
        let Some(target_def) = self.name_def_of_kind(
            &decl.name,
            &[DefKind::Interface, DefKind::InterfaceAlias],
            "interface or alias",
        ) else {
            self.diagnostics.push(Diagnostic::new(
                decl.name.span,
                format!(
                    "unknown interface or alias `{}` in derive",
                    decl.name.display
                ),
            ));
            return;
        };
        let target_def_kind = self.ctx.resolved.def(target_def).kind.clone();
        let view = match target_def_kind {
            DefKind::Interface => {
                let Some(interface) = self.ctx.interfaces.get(&target_def).cloned() else {
                    return;
                };
                if interface.generics.len() != 1 {
                    self.diagnostics.push(Diagnostic::new(
                        decl.name.span,
                        format!(
                            "derive target `{}` must not have unfixed non-receiver type parameters",
                            interface.name
                        ),
                    ));
                    return;
                }
                InterfaceView {
                    positive: vec![InterfaceRefTy {
                        def_id: target_def,
                        name: interface.name,
                        args: Vec::new(),
                    }],
                    negative: Vec::new(),
                }
            }
            DefKind::InterfaceAlias => {
                let Some(alias) = self.ctx.interface_aliases.get(&target_def) else {
                    return;
                };
                if !alias.generics.is_empty() {
                    let name = self.ctx.resolved.def(target_def).name.clone();
                    self.diagnostics.push(Diagnostic::new(
                        decl.name.span,
                        format!(
                            "derive target `{name}` must be a one-argument alias with no extra type parameters"
                        ),
                    ));
                    return;
                }
                self.interface_view_for_def(target_def, &[])
            }
            _ => return,
        };

        let generic_constraints =
            self.derived_impl_generic_constraints(&derive_generics, &derive_subst);
        for capability in &view.positive {
            self.derive_positive_capability(
                module,
                item_span,
                decl,
                &derive_generics,
                &generic_constraints,
                &receiver_ty,
                capability,
            );
        }
        self.pending_derived_negative_checks
            .extend(view.negative.into_iter().map(|capability| {
                PendingDerivedNegativeCapabilityCheck {
                    derive_display: decl.name.display.clone(),
                    derive_span: decl.name.span,
                    derive_generics: derive_generics.clone(),
                    generic_constraints: generic_constraints.clone(),
                    receiver_ty: receiver_ty.clone(),
                    capability,
                }
            }));
    }

    #[allow(clippy::too_many_arguments)]
    fn derive_positive_capability(
        &mut self,
        module: ModuleId,
        item_span: crate::span::Span,
        decl: &DeriveDecl,
        derive_generics: &[GenericInfo],
        generic_constraints: &[GenericConstraintBounds],
        receiver_ty: &Ty,
        capability: &InterfaceRefTy,
    ) {
        if self.is_compiler_provided_meta_marker_def(capability.def_id) {
            self.diagnostics.push(Diagnostic::new(
                decl.name.span,
                format!(
                    "`{}` is a compiler-provided marker and cannot be derived in source",
                    capability.name
                ),
            ));
            return;
        }
        let full_args = std::iter::once(receiver_ty.clone())
            .chain(capability.args.iter().cloned())
            .collect::<Vec<_>>();
        if self
            .find_impl_by_full_args(
                capability.def_id,
                &capability.name,
                &full_args,
                Some(receiver_ty),
            )
            .is_some()
        {
            self.diagnostics.push(Diagnostic::new(
                decl.name.span,
                format!(
                    "derive `{}` conflicts with an existing impl of `{}` for `{}`",
                    decl.name.display, capability.name, receiver_ty
                ),
            ));
            return;
        }
        let Some(template) = self
            .ctx
            .derivable_impls
            .iter()
            .find(|template| {
                template.interface_def == capability.def_id
                    && interface_non_receiver_args(&template.interface_args) == capability.args
            })
            .cloned()
        else {
            self.diagnostics.push(Diagnostic::new(
                decl.name.span,
                format!("no derivable impl is available for `{}`", capability.name),
            ));
            return;
        };
        if template.requires_unsafe && !decl.is_unsafe {
            self.diagnostics.push(Diagnostic::new(
                decl.name.span,
                format!(
                    "derive `{}` requires `unsafe derive` because `{}` has an unsafe derivation template",
                    decl.name.display, capability.name
                ),
            ));
            return;
        }
        let body_subst =
            self.derivable_template_subst(&template, &full_args, Some(receiver_ty), decl.name.span);
        let Some(body_subst) = body_subst else {
            return;
        };
        let params = template
            .params
            .iter()
            .map(|ty| self.substitute_derived_template_ty(ty, &body_subst, item_span))
            .collect::<Vec<_>>();
        let ret = self.substitute_derived_template_ty(&template.ret, &body_subst, item_span);
        let interface_args = template
            .interface_args
            .iter()
            .map(|ty| self.substitute_derived_template_ty(ty, &body_subst, item_span))
            .collect::<Vec<_>>();
        let concrete_receiver = template
            .receiver_ty
            .as_ref()
            .map(|ty| self.substitute_derived_template_ty(ty, &body_subst, item_span));
        let analysis = ImplAnalysis {
            interface_def: template.interface_def,
            interface_name: template.interface_name.clone(),
            generics: derive_generics.to_vec(),
            generic_constraints: generic_constraints.to_vec(),
            interface_args,
            receiver_ty: concrete_receiver,
            ret,
            params,
        };
        if self.reject_uninferable_derive_generics(decl.name.span, &analysis) {
            return;
        }
        if self.resource_handle_message_impl_forbidden(decl.name.span, &analysis) {
            return;
        }
        if self.generic_marker_impl_overlaps_existing(item_span, &analysis) {
            return;
        }
        if derive_generics.is_empty() {
            if let Some(pending) = self.register_impl_signature(
                template.module,
                &template.decl,
                analysis.interface_def,
                &analysis.interface_name,
                analysis.interface_args,
                analysis.receiver_ty,
                analysis.ret,
                analysis.params,
            ) {
                self.pending_derived_template_constraint_checks.push(
                    PendingDerivedTemplateConstraintCheck {
                        derive_span: decl.name.span,
                        derive_generics: derive_generics.to_vec(),
                        generic_constraints: generic_constraints.to_vec(),
                        template_generics: template.generics.clone(),
                        body_subst: body_subst.clone(),
                        implementation: pending.implementation.clone(),
                    },
                );
                self.queue_impl_body(pending, body_subst, Some(module));
            }
        } else {
            self.pending_derived_generic_impls
                .push(PendingDerivedGenericImpl {
                    derive_display: decl.name.display.clone(),
                    derive_span: decl.name.span,
                    validation_module: template.module,
                    reflection_module: module,
                    template_generics: template.generics.clone(),
                    template: GenericImplTemplate {
                        module: template.module,
                        body_reflection_module: Some(module),
                        item_span,
                        interface_def: analysis.interface_def,
                        interface_name: analysis.interface_name,
                        generics: analysis.generics,
                        generic_constraints: analysis.generic_constraints,
                        interface_args: analysis.interface_args,
                        receiver_ty: analysis.receiver_ty,
                        ret: analysis.ret,
                        params: analysis.params,
                        decl: template.decl,
                        body_subst,
                    },
                });
        }
    }

    fn reject_uninferable_derive_generics(
        &mut self,
        span: crate::span::Span,
        analysis: &ImplAnalysis,
    ) -> bool {
        if analysis.generics.is_empty() {
            return false;
        }
        let mut rejected = false;
        for generic in &analysis.generics {
            let name = std::slice::from_ref(&generic.name)
                .iter()
                .cloned()
                .collect::<HashSet<_>>();
            let inferable = analysis
                .interface_args
                .iter()
                .any(|arg| contains_any_generic_name(arg, &name))
                || analysis
                    .receiver_ty
                    .as_ref()
                    .is_some_and(|receiver| contains_any_generic_name(receiver, &name));
            if !inferable {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "derive generic parameter `{}` cannot be inferred from the derived impl receiver or interface arguments",
                        generic.name
                    ),
                ));
                rejected = true;
            }
        }
        rejected
    }

    fn derive_receiver_implements_forbidden_capability(
        &mut self,
        derive_generics: &[GenericInfo],
        generic_constraints: &[GenericConstraintBounds],
        capability: &InterfaceRefTy,
        receiver_ty: &Ty,
    ) -> bool {
        let resource_generics = Self::resource_generic_scope(derive_generics);
        self.resource_generic_stack.push(resource_generics);
        self.generic_env_stack.push(derive_generics.to_vec());
        let implements = {
            if !derive_generics.is_empty() {
                let interface_args = std::iter::once(receiver_ty.clone())
                    .chain(capability.args.iter().cloned())
                    .collect::<Vec<_>>();
                let analysis = ImplAnalysis {
                    interface_def: capability.def_id,
                    interface_name: capability.name.clone(),
                    generics: derive_generics.to_vec(),
                    generic_constraints: generic_constraints.to_vec(),
                    interface_args,
                    receiver_ty: Some(receiver_ty.clone()),
                    ret: Ty::Void,
                    params: Vec::new(),
                };
                if self.marker_capability_impl_overlaps_existing(&analysis) {
                    true
                } else {
                    self.derive_receiver_implements_capability_ref(
                        derive_generics,
                        generic_constraints,
                        capability,
                        receiver_ty,
                    )
                }
            } else {
                self.derive_receiver_implements_capability_ref(
                    derive_generics,
                    generic_constraints,
                    capability,
                    receiver_ty,
                )
            }
        };
        self.generic_env_stack.pop();
        self.resource_generic_stack.pop();
        implements
    }

    fn derive_receiver_implements_capability_ref(
        &mut self,
        derive_generics: &[GenericInfo],
        generic_constraints: &[GenericConstraintBounds],
        capability: &InterfaceRefTy,
        receiver_ty: &Ty,
    ) -> bool {
        if !derive_generics.is_empty() {
            self.symbolic_constraint_env_stack
                .push(generic_constraints.to_vec());
        }
        let implements = self.with_symbolic_impl_resolution(|checker| {
            checker.type_implements_capability_ref(capability, receiver_ty)
        });
        if !derive_generics.is_empty() {
            self.symbolic_constraint_env_stack.pop();
        }
        implements
    }

    fn substitute_derived_template_ty(
        &mut self,
        ty: &Ty,
        subst: &HashMap<String, Ty>,
        span: crate::span::Span,
    ) -> Ty {
        let substituted = substitute_ty(ty, subst);
        self.normalize_meta_repr_markers(&substituted, span)
    }

    fn derivable_template_subst(
        &mut self,
        template: &DerivableImplTemplate,
        interface_args: &[Ty],
        receiver_ty: Option<&Ty>,
        span: crate::span::Span,
    ) -> Option<HashMap<String, Ty>> {
        if template.interface_args.len() != interface_args.len() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "derivable impl `{}` does not match requested interface arguments",
                    template.interface_name
                ),
            ));
            return None;
        }
        let scoped_names = template
            .generics
            .iter()
            .map(|generic| {
                (
                    generic.name.clone(),
                    format!(
                        "$derivable_template${}${}",
                        template.interface_name, generic.name
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        let scoped_subst = scoped_names
            .iter()
            .map(|(name, scoped)| (name.clone(), Ty::Generic(scoped.clone())))
            .collect::<HashMap<_, _>>();
        let mut subst = scoped_names
            .values()
            .map(|name| (name.clone(), Ty::Generic(name.clone())))
            .collect::<HashMap<_, _>>();
        for (pattern, actual) in template.interface_args.iter().zip(interface_args.iter()) {
            let pattern = substitute_ty(pattern, &scoped_subst);
            if !unify_ty(&pattern, actual, &mut subst) {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "derivable impl `{}` does not match requested receiver",
                        template.interface_name
                    ),
                ));
                return None;
            }
        }
        if let (Some(pattern), Some(actual)) = (template.receiver_ty.as_ref(), receiver_ty) {
            let pattern = substitute_ty(pattern, &scoped_subst);
            if !unify_ty(&pattern, actual, &mut subst) {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "derivable impl `{}` does not match requested receiver",
                        template.interface_name
                    ),
                ));
                return None;
            }
        }
        let scoped_name_set = scoped_names.values().cloned().collect::<HashSet<_>>();
        let mut body_subst = HashMap::new();
        let mut unresolved = false;
        for generic in &template.generics {
            let Some(scoped_name) = scoped_names.get(&generic.name) else {
                unresolved = true;
                continue;
            };
            let Some(value) = subst.get(scoped_name).cloned() else {
                unresolved = true;
                continue;
            };
            let value = self.substitute_ty_normalized_silent(&value, &subst);
            if contains_any_generic_name(&value, &scoped_name_set) {
                unresolved = true;
                continue;
            }
            body_subst.insert(generic.name.clone(), value);
        }
        if unresolved {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "derivable impl `{}` leaves template generic parameters unresolved",
                    template.interface_name
                ),
            ));
            return None;
        }
        Some(body_subst)
    }

    fn validate_derived_template_constraints(
        &mut self,
        derive_generics: &[GenericInfo],
        generic_constraints: &[GenericConstraintBounds],
        template_generics: &[GenericInfo],
        body_subst: &HashMap<String, Ty>,
        span: crate::span::Span,
    ) -> bool {
        let diagnostic_count = self.diagnostics.len();
        let resource_generics = Self::resource_generic_scope(derive_generics);
        self.resource_generic_stack.push(resource_generics);
        self.generic_env_stack.push(derive_generics.to_vec());
        self.symbolic_constraint_env_stack
            .push(generic_constraints.to_vec());
        self.with_symbolic_impl_resolution(|checker| {
            checker.check_generic_constraints(template_generics, body_subst, span);
        });
        self.symbolic_constraint_env_stack.pop();
        self.generic_env_stack.pop();
        self.resource_generic_stack.pop();
        self.diagnostics.len() == diagnostic_count
    }

    fn drain_pending_derived_template_constraint_checks(&mut self) {
        let mut candidates = std::mem::take(&mut self.pending_derived_template_constraint_checks);
        if candidates.is_empty() {
            return;
        }

        let pending_defs = candidates
            .iter()
            .map(|check| check.implementation.function_def)
            .collect::<HashSet<_>>();
        self.ctx
            .impls
            .retain(|implementation| !pending_defs.contains(&implementation.function_def));

        while !candidates.is_empty() {
            let mut next = Vec::new();
            let mut progressed = false;
            for check in candidates {
                let diagnostic_count = self.diagnostics.len();
                if self.validate_derived_template_constraints(
                    &check.derive_generics,
                    &check.generic_constraints,
                    &check.template_generics,
                    &check.body_subst,
                    check.derive_span,
                ) {
                    self.restore_derived_concrete_impl(check.implementation);
                    progressed = true;
                } else {
                    let diagnostics = self.diagnostics.split_off(diagnostic_count);
                    next.push((check, diagnostics));
                }
            }
            if next.is_empty() {
                return;
            }
            if !progressed {
                for (check, diagnostics) in next {
                    self.restore_derived_concrete_impl(check.implementation);
                    self.diagnostics.extend(diagnostics);
                }
                return;
            }
            candidates = next.into_iter().map(|(check, _)| check).collect();
        }
    }

    fn restore_derived_concrete_impl(&mut self, implementation: ImplSig) {
        if self
            .find_impl_by_full_args(
                implementation.interface_def,
                &implementation.interface_name,
                &implementation.interface_args,
                implementation.receiver_ty.as_ref(),
            )
            .is_none()
        {
            self.ctx.impls.push(implementation);
        }
    }

    fn drain_pending_derived_negative_checks(&mut self) {
        let checks = std::mem::take(&mut self.pending_derived_negative_checks);
        for check in checks {
            if self.derive_receiver_implements_forbidden_capability(
                &check.derive_generics,
                &check.generic_constraints,
                &check.capability,
                &check.receiver_ty,
            ) {
                self.diagnostics.push(Diagnostic::new(
                    check.derive_span,
                    format!(
                        "cannot derive `{}` for `{}` because it already implements forbidden capability `{}`",
                        check.derive_display, check.receiver_ty, check.capability.name
                    ),
                ));
            }
        }
    }

    pub(super) fn derived_impl_generic_constraints(
        &mut self,
        derive_generics: &[GenericInfo],
        derive_subst: &HashMap<String, Ty>,
    ) -> Vec<GenericConstraintBounds> {
        derive_generics
            .iter()
            .map(|generic| GenericConstraintBounds {
                name: generic.name.clone(),
                is_resource: generic.is_resource,
                bounds: generic
                    .constraint
                    .as_ref()
                    .map(|constraint| self.constraint_bounds(constraint, derive_subst))
                    .unwrap_or_default(),
            })
            .collect()
    }

    pub(super) fn validate_derived_generic_impl_body(
        &mut self,
        module: ModuleId,
        derive_display: &str,
        derive_span: crate::span::Span,
        reflection_module: Option<ModuleId>,
        analysis: &ImplAnalysis,
        template_decl: &ImplDecl,
        body_subst: &HashMap<String, Ty>,
    ) -> bool {
        let validation_names = analysis
            .generics
            .iter()
            .map(|generic| {
                (
                    generic.name.clone(),
                    format!(
                        "$derive_validation${}${}",
                        analysis.interface_name, generic.name
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        let validation_subst = validation_names
            .iter()
            .map(|(name, scoped)| (name.clone(), Ty::Generic(scoped.clone())))
            .collect::<HashMap<_, _>>();
        let validation_generics = analysis
            .generics
            .iter()
            .map(|generic| GenericInfo {
                name: validation_names
                    .get(&generic.name)
                    .cloned()
                    .unwrap_or_else(|| generic.name.clone()),
                is_resource: generic.is_resource,
                is_hidden: generic.is_hidden,
                constraint: None,
            })
            .collect::<Vec<_>>();
        let validation_constraints = analysis
            .generic_constraints
            .iter()
            .map(|constraint| GenericConstraintBounds {
                name: validation_names
                    .get(&constraint.name)
                    .cloned()
                    .unwrap_or_else(|| constraint.name.clone()),
                is_resource: constraint.is_resource,
                bounds: substitute_constraint_bounds(&constraint.bounds, &validation_subst),
            })
            .collect::<Vec<_>>();
        let validation_body_subst = body_subst
            .iter()
            .map(|(name, ty)| {
                (
                    name.clone(),
                    self.substitute_ty_normalized_silent(ty, &validation_subst),
                )
            })
            .collect::<HashMap<_, _>>();
        let validation_params = analysis
            .params
            .iter()
            .map(|ty| self.substitute_ty_normalized_silent(ty, &validation_subst))
            .collect::<Vec<_>>();
        let validation_ret = self.substitute_ty_normalized_silent(&analysis.ret, &validation_subst);
        let function_sig = FunctionSig {
            def_id: self.alloc_synthetic_def(),
            module,
            name: impl_function_name(&analysis.interface_name, &validation_params),
            is_unsafe: false,
            is_async: false,
            abi: None,
            noescape: false,
            has_body: true,
            ret: validation_ret,
            params: validation_params,
            generics: validation_generics.clone(),
            exported: false,
        };
        let body_params = template_decl
            .params
            .iter()
            .zip(function_sig.params.iter())
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
        let diagnostic_count = self.diagnostics.len();
        let previous_module = self.current_module;
        self.current_module = module;
        self.type_subst_stack.push(validation_body_subst);
        if let Some(module) = reflection_module {
            self.meta_reflection_module_stack.push(module);
        }
        self.symbolic_constraint_env_stack
            .push(validation_constraints);
        self.with_generic_env(&validation_generics, |checker| {
            checker.with_symbolic_impl_resolution(|checker| {
                checker.check_function_body(&function_sig, &body_params, &template_decl.body);
            });
        });
        self.symbolic_constraint_env_stack.pop();
        if reflection_module.is_some() {
            self.meta_reflection_module_stack.pop();
        }
        self.type_subst_stack.pop();
        self.current_module = previous_module;
        if self.diagnostics.len() == diagnostic_count {
            return true;
        }
        let details = self.diagnostics.split_off(diagnostic_count);
        let detail = details
            .first()
            .map(|diagnostic| diagnostic.message.clone())
            .unwrap_or_else(|| "generated impl body does not type-check".to_string());
        let receiver = analysis
            .receiver_ty
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "<none>".to_string());
        self.diagnostics.push(Diagnostic::new(
            derive_span,
            format!(
                "derive `{}` for `{receiver}` is not valid under its generic constraints: {detail}",
                derive_display
            ),
        ));
        false
    }

    pub(super) fn drain_pending_derived_generic_impls(&mut self) {
        let mut candidates = std::mem::take(&mut self.pending_derived_generic_impls);
        let mut rejected_diagnostics = Vec::new();
        while !candidates.is_empty() {
            self.pending_derived_generic_impls = candidates.clone();
            let mut validated = Vec::new();
            let mut rejected = Vec::new();
            for pending in candidates {
                let template = pending.template.clone();
                let analysis = ImplAnalysis {
                    interface_def: template.interface_def,
                    interface_name: template.interface_name.clone(),
                    generics: template.generics.clone(),
                    generic_constraints: template.generic_constraints.clone(),
                    interface_args: template.interface_args.clone(),
                    receiver_ty: template.receiver_ty.clone(),
                    ret: template.ret.clone(),
                    params: template.params.clone(),
                };
                let diagnostic_count = self.diagnostics.len();
                let valid = self.validate_derived_template_constraints(
                    &template.generics,
                    &template.generic_constraints,
                    &pending.template_generics,
                    &template.body_subst,
                    pending.derive_span,
                ) && self.validate_derived_generic_impl_body(
                    pending.validation_module,
                    &pending.derive_display,
                    pending.derive_span,
                    Some(pending.reflection_module),
                    &analysis,
                    &template.decl,
                    &template.body_subst,
                );
                if valid {
                    debug_assert_eq!(self.diagnostics.len(), diagnostic_count);
                    validated.push(pending);
                } else {
                    rejected.extend(self.diagnostics.split_off(diagnostic_count));
                }
            }
            if rejected.is_empty() {
                self.pending_derived_generic_impls.clear();
                self.ctx
                    .generic_impls
                    .extend(validated.into_iter().map(|pending| pending.template));
                self.diagnostics.extend(rejected_diagnostics);
                return;
            }
            rejected_diagnostics.extend(rejected);
            candidates = validated;
        }
        self.pending_derived_generic_impls.clear();
        self.diagnostics.extend(rejected_diagnostics);
    }

    pub(super) fn visible_generic_impl_templates(&self) -> Vec<GenericImplTemplate> {
        self.ctx
            .generic_impls
            .iter()
            .cloned()
            .chain(
                self.pending_derived_generic_impls
                    .iter()
                    .map(|pending| pending.template.clone()),
            )
            .collect()
    }

    pub(super) fn generic_marker_impl_overlaps_existing(
        &mut self,
        span: crate::span::Span,
        analysis: &ImplAnalysis,
    ) -> bool {
        if !self.is_std_message_capability_interface_def(analysis.interface_def) {
            return false;
        }
        if let Some(conflict) = self.marker_capability_impl_overlap_kind(analysis) {
            self.fatal_impl_coherence_error = true;
            let message = match conflict {
                MarkerImplOverlapKind::GenericTemplate if analysis.generics.is_empty() => {
                    format!(
                        "marker impl for `{}` conflicts with an existing generic impl",
                        analysis.interface_name
                    )
                }
                MarkerImplOverlapKind::GenericTemplate => {
                    format!(
                        "ambiguous generic impls for marker interface `{}`",
                        analysis.interface_name
                    )
                }
                MarkerImplOverlapKind::ConcreteImpl => {
                    format!(
                        "generic marker impl for `{}` conflicts with an existing concrete impl",
                        analysis.interface_name
                    )
                }
            };
            self.push_diagnostic_once(span, message);
            return true;
        }
        false
    }

    fn marker_capability_impl_overlaps_existing(&mut self, analysis: &ImplAnalysis) -> bool {
        self.marker_capability_impl_overlap_kind(analysis).is_some()
    }

    fn marker_capability_impl_overlap_kind(
        &mut self,
        analysis: &ImplAnalysis,
    ) -> Option<MarkerImplOverlapKind> {
        if !self.is_std_message_capability_interface_def(analysis.interface_def) {
            return None;
        }
        let current_domain =
            self.compiler_marker_domain_for_impl(&analysis.generics, analysis.receiver_ty.as_ref());
        let templates = self.visible_generic_impl_templates();
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
                return Some(MarkerImplOverlapKind::GenericTemplate);
            }
        }
        if analysis.generics.is_empty() {
            return None;
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
                return Some(MarkerImplOverlapKind::ConcreteImpl);
            }
        }
        None
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
        reflection_module: Option<ModuleId>,
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
        self.queue_impl_body(pending, subst.clone(), reflection_module);
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
        reflection_module: Option<ModuleId>,
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
        if let Some(module) = reflection_module {
            self.meta_reflection_module_stack.push(module);
        }
        let body = self.check_function_body_with_return_context(
            &pending.function_sig,
            &body_params,
            &pending.decl.body,
            impl_return_context(&pending.implementation),
        );
        if reflection_module.is_some() {
            self.meta_reflection_module_stack.pop();
        }
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

    pub(super) fn queue_impl_body(
        &mut self,
        pending: PendingImplBody,
        subst: HashMap<String, Ty>,
        reflection_module: Option<ModuleId>,
    ) {
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
        self.pending_impl_bodies.push(QueuedImplBody {
            pending,
            subst,
            reflection_module,
        });
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
            self.check_registered_impl_body(
                &queued.pending,
                &queued.subst,
                queued.reflection_module,
            );
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
        self.check_function_body_with_return_context(
            sig,
            params,
            body,
            format!("function `{}`", sig.name),
        )
    }

    fn check_function_body_with_return_context(
        &mut self,
        sig: &FunctionSig,
        params: &[(LocalId, String, Ty, BindingMutability)],
        body: &Block,
        return_context: String,
    ) -> Option<CheckedFunctionBody> {
        let previous_return_ty = std::mem::replace(&mut self.current_return_ty, sig.ret.clone());
        let previous_return_context =
            std::mem::replace(&mut self.current_return_context, Some(return_context));
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
        let return_context = self
            .current_return_context
            .as_deref()
            .unwrap_or("current function");
        if sig.ret.is_never()
            && checked
                .as_ref()
                .is_some_and(|checked| checked.flow.can_fallthrough)
        {
            self.diagnostics.push(Diagnostic::new(
                body.span,
                format!("{return_context} with return type `never` can fall through"),
            ));
        } else if !sig.ret.is_erased_value()
            && !checked
                .as_ref()
                .is_some_and(|checked| !checked.flow.can_fallthrough)
        {
            self.diagnostics.push(Diagnostic::new(
                body.span,
                format!("{return_context} must return `{}` on every path", sig.ret),
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
        self.current_return_context = previous_return_context;
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
