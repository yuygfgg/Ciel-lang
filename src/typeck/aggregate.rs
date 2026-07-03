use super::*;

impl TypeChecker {
    pub(super) fn ensure_enum_instance(&mut self, ty: &Ty) {
        match ty {
            Ty::Named { name, args } => {
                let Some(template) = self.ctx.enum_templates.get(name).cloned() else {
                    return;
                };
                if args.iter().any(contains_generic) {
                    return;
                }
                if args.len() != template.generics.len() {
                    self.diagnostics.push(Diagnostic::new(
                        None,
                        format!(
                            "enum `{name}` expects {} type arguments, got {}",
                            template.generics.len(),
                            args.len()
                        ),
                    ));
                    return;
                }
                let instance_name = enum_instance_name(name, args);
                if self.ctx.checked_enums.contains_key(&instance_name)
                    || self.visiting_enums.contains(&instance_name)
                {
                    return;
                }
                let subst = template
                    .generics
                    .iter()
                    .map(|generic| generic.name.clone())
                    .zip(args.iter().cloned())
                    .collect::<HashMap<_, _>>();
                self.visiting_enums.insert(instance_name.clone());
                let variants = template
                    .variants
                    .iter()
                    .map(|variant| CheckedVariant {
                        name: variant.name.clone(),
                        payload: variant
                            .payload
                            .iter()
                            .filter_map(|payload| {
                                let ty = self.lower_type_with_subst(payload, &subst);
                                (!ty.is_erased_value()).then_some(ty)
                            })
                            .collect(),
                    })
                    .collect::<Vec<_>>();
                self.visiting_enums.remove(&instance_name);
                self.ctx.checked_enums.insert(
                    instance_name.clone(),
                    CheckedEnum {
                        name: instance_name,
                        variants,
                    },
                );
            }
            Ty::Pointer { inner, .. } => self.ensure_enum_instance(inner),
            Ty::Array { elem, .. } | Ty::Slice { elem, .. } => self.ensure_enum_instance(elem),
            Ty::DynamicInterface { args, .. } => {
                for arg in args {
                    self.ensure_enum_instance(arg);
                }
            }
            Ty::Function { ret, params, .. } => {
                self.ensure_enum_instance(ret);
                for param in params {
                    self.ensure_enum_instance(param);
                }
            }
            Ty::Closure { ret, params, .. } | Ty::ClosureInstance { ret, params, .. } => {
                self.ensure_enum_instance(ret);
                for param in params {
                    self.ensure_enum_instance(param);
                }
            }
            _ => {}
        }
    }

    pub(super) fn ensure_struct_instance(&mut self, ty: &Ty) {
        match ty {
            Ty::Named { name, args } => {
                let Some(template) = self.ctx.struct_templates.get(name).cloned() else {
                    return;
                };
                if args.iter().any(contains_generic) {
                    return;
                }
                if args.len() != template.generics.len() {
                    self.diagnostics.push(Diagnostic::new(
                        None,
                        format!(
                            "struct `{name}` expects {} type arguments, got {}",
                            template.generics.len(),
                            args.len()
                        ),
                    ));
                    return;
                }
                let instance_name = enum_instance_name(name, args);
                if self.ctx.structs.contains_key(&instance_name)
                    || self.visiting_structs.contains(&instance_name)
                {
                    return;
                }
                let subst = template
                    .generics
                    .iter()
                    .map(|generic| generic.name.clone())
                    .zip(args.iter().cloned())
                    .collect::<HashMap<_, _>>();
                self.check_generic_constraints(&template.generics, &subst, None);
                self.visiting_structs.insert(instance_name.clone());
                let fields = template
                    .fields
                    .iter()
                    .map(|field| {
                        let ty = self.lower_type_with_subst(&field.ty, &subst);
                        self.reject_invalid_plain_value_type(&ty, field.ty.span, "struct field");
                        (field.name.name.clone(), ty)
                    })
                    .collect::<Vec<_>>();
                self.visiting_structs.remove(&instance_name);
                let instance_ty = Ty::Named {
                    name: name.clone(),
                    args: args.clone(),
                };
                if template.is_resource {
                    self.ctx.resource_structs.insert(instance_name.clone());
                }
                self.ctx
                    .structs
                    .insert(instance_name.clone(), fields.clone());
                self.validate_resource_struct_fields(
                    &instance_ty,
                    template.is_resource,
                    &fields,
                    None,
                );
                if template.is_unsafe {
                    self.ctx
                        .unsafe_structs
                        .insert(enum_instance_name(name, args));
                }
            }
            Ty::Pointer { inner, .. } => self.ensure_struct_instance(inner),
            Ty::Array { elem, .. } | Ty::Slice { elem, .. } => self.ensure_struct_instance(elem),
            Ty::DynamicInterface { args, .. } => {
                for arg in args {
                    self.ensure_struct_instance(arg);
                }
            }
            Ty::Function { ret, params, .. } => {
                self.ensure_struct_instance(ret);
                for param in params {
                    self.ensure_struct_instance(param);
                }
            }
            Ty::Closure { ret, params, .. } | Ty::ClosureInstance { ret, params, .. } => {
                self.ensure_struct_instance(ret);
                for param in params {
                    self.ensure_struct_instance(param);
                }
            }
            _ => {}
        }
    }

    pub(super) fn function_sig_for(&self, module: ModuleId, name: &str) -> Option<&FunctionSig> {
        self.ctx.functions_by_name.get(name).and_then(|defs| {
            defs.iter().find_map(|def_id| {
                let sig = self.ctx.functions_by_def.get(def_id)?;
                (sig.module == module).then_some(sig)
            })
        })
    }

    pub(super) fn resolve_function_name(&mut self, name: &NameRef) -> Option<FunctionSig> {
        let def_id = self.name_def_of_kind(
            name,
            &[DefKind::Function, DefKind::ExternFunction],
            "function",
        )?;
        self.ctx.functions_by_def.get(&def_id).cloned()
    }

    pub(super) fn lookup_variant_name(
        &mut self,
        name: &NameRef,
        expected: Option<&Ty>,
    ) -> Option<(DefId, VariantSig)> {
        match &name.kind {
            NameRefKind::Def(def_id)
                if self.ctx.resolved.def(*def_id).kind == DefKind::EnumVariant =>
            {
                let sig = self.ctx.variants.get(def_id)?.clone();
                Some((*def_id, sig))
            }
            NameRefKind::VariantCandidates(candidates) => {
                self.select_variant_candidate(&name.display, name.span, candidates, expected)
            }
            _ => None,
        }
    }

    pub(super) fn select_variant_candidate(
        &mut self,
        display: &str,
        span: crate::span::Span,
        candidates: &[DefId],
        expected: Option<&Ty>,
    ) -> Option<(DefId, VariantSig)> {
        let candidates = candidates
            .iter()
            .filter_map(|def_id| {
                self.ctx
                    .variants
                    .get(def_id)
                    .cloned()
                    .map(|sig| (*def_id, sig))
            })
            .collect::<Vec<_>>();
        if let Some(Ty::Named { name, .. }) = expected {
            let matching = candidates
                .iter()
                .filter(|(_, sig)| &sig.enum_name == name)
                .cloned()
                .collect::<Vec<_>>();
            return match matching.len() {
                1 => Some(matching[0].clone()),
                0 if candidates.len() == 1 => Some(candidates[0].clone()),
                0 => {
                    self.diagnostics.push(
                        Diagnostic::new(
                            span,
                            format!("no visible variant `{display}` belongs to `{name}`"),
                        )
                        .note(self.variant_candidate_note(&candidates)),
                    );
                    None
                }
                _ => {
                    self.diagnostics.push(
                        Diagnostic::new(
                            span,
                            format!(
                                "ambiguous enum variant `{display}` for expected `{name}` ({} candidates)",
                                matching.len()
                            ),
                        )
                        .note(self.variant_candidate_note(&matching)),
                    );
                    None
                }
            };
        }
        match candidates.len() {
            0 => None,
            1 => Some(candidates[0].clone()),
            _ => {
                self.diagnostics.push(
                    Diagnostic::new(
                        span,
                        format!(
                            "ambiguous enum variant `{display}` ({} candidates); use `Enum::{}` or an expected enum type",
                            candidates.len(),
                            display.rsplit("::").next().unwrap_or(display)
                        ),
                    )
                    .note(self.variant_candidate_note(&candidates)),
                );
                None
            }
        }
    }

    fn variant_candidate_note(&self, candidates: &[(DefId, VariantSig)]) -> String {
        if candidates.is_empty() {
            return "no visible variant candidates were found".to_string();
        }
        let mut parts = candidates
            .iter()
            .take(5)
            .map(|(def_id, sig)| {
                let def = self.ctx.resolved.def(*def_id);
                format!("`{}::{}`", sig.enum_name, def.name)
            })
            .collect::<Vec<_>>();
        if candidates.len() > parts.len() {
            parts.push(format!("and {} more", candidates.len() - parts.len()));
        }
        format!("visible variant candidates: {}", parts.join(", "))
    }

    pub(super) fn lookup_interface_name(&self, name: &NameRef) -> Option<DefId> {
        self.name_def_of_kind_ref(name, &[DefKind::Interface, DefKind::InterfaceAlias])
    }

    pub(super) fn name_def_of_kind(
        &mut self,
        name: &NameRef,
        kinds: &[DefKind],
        kind_name: &str,
    ) -> Option<DefId> {
        let def_id = self.name_def_of_kind_ref(name, kinds)?;
        Some(def_id).or_else(|| {
            self.diagnostics.push(Diagnostic::new(
                name.span,
                format!("`{}` is not a {kind_name}", name.display),
            ));
            None
        })
    }

    pub(super) fn name_def_of_kind_ref(&self, name: &NameRef, kinds: &[DefKind]) -> Option<DefId> {
        let NameRefKind::Def(def_id) = name.kind else {
            return None;
        };
        let def = self.ctx.resolved.def(def_id);
        if kinds.iter().any(|kind| *kind == def.kind) {
            Some(def_id)
        } else {
            None
        }
    }

    pub(super) fn resolved_local_id(&self, name: &NameRef) -> Option<LocalId> {
        match name.kind {
            NameRefKind::Local(local_id) => Some(local_id),
            _ => None,
        }
    }

    pub(super) fn interface_sig_by_def(&self, def_id: DefId) -> Option<&InterfaceSig> {
        self.ctx.interfaces.get(&def_id)
    }
}
