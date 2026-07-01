use super::*;

impl TypeChecker {
    pub(super) fn collect_interfaces(&mut self) {
        let modules = self.ctx.hir_modules.clone();
        for module in &modules {
            self.current_module = module.id;
            for item in &module.items {
                match &item.kind {
                    ItemKind::Interface(decl) => {
                        let Some(def_id) = self.ctx.resolved.local_def(
                            module.id,
                            &decl.signature.name.name,
                            &[DefKind::Interface],
                        ) else {
                            continue;
                        };
                        for generic in &decl.generics {
                            if let Some(constraint) = &generic.constraint {
                                self.validate_constraint_bindings_forbidden(
                                    constraint,
                                    "interface declarations",
                                );
                            }
                        }
                        let generics = decl
                            .generics
                            .iter()
                            .map(|param| param.name.name.clone())
                            .collect::<Vec<_>>();
                        let ret = match &decl.signature.ret {
                            FunctionReturnType::Type(ty) => ty.clone(),
                            FunctionReturnType::OpaqueConstraint { marker_span, .. } => {
                                self.diagnostics.push(Diagnostic::new(
                                    *marker_span,
                                    "opaque return type cannot be used in interface declarations",
                                ));
                                continue;
                            }
                        };
                        self.ctx.interfaces.insert(
                            def_id,
                            InterfaceSig {
                                name: decl.signature.name.name.clone(),
                                is_unsafe: decl.is_unsafe,
                                generics,
                                determined_start: decl.determined_start,
                                ret,
                                params: decl.signature.params.clone(),
                            },
                        );
                        self.ctx
                            .interface_names
                            .insert(decl.signature.name.name.clone(), def_id);
                    }
                    ItemKind::InterfaceAlias(decl) => {
                        let Some(def_id) = self.ctx.resolved.local_def(
                            module.id,
                            &decl.name.name,
                            &[DefKind::InterfaceAlias],
                        ) else {
                            continue;
                        };
                        for generic in &decl.generics {
                            if generic.constraint.is_some() {
                                self.diagnostics.push(Diagnostic::new(
                                    generic.name.span,
                                    "interface alias generic parameters cannot have constraints",
                                ));
                            }
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
                        self.ctx.interface_aliases.insert(
                            def_id,
                            InterfaceAliasTemplate {
                                generics,
                                expr: decl.expr.clone(),
                            },
                        );
                        self.ctx
                            .interface_alias_names
                            .insert(decl.name.name.clone(), def_id);
                    }
                    _ => {}
                }
            }
        }
    }

    pub(super) fn collect_type_aliases_and_opaque_structs(&mut self) {
        let modules = self.ctx.hir_modules.clone();
        for module in &modules {
            self.current_module = module.id;
            for item in &module.items {
                match &item.kind {
                    ItemKind::TypeAlias(decl) => {
                        let Some(def_id) = self.ctx.resolved.local_def(
                            module.id,
                            &decl.name.name,
                            &[DefKind::TypeAlias],
                        ) else {
                            continue;
                        };
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
                        self.validate_generic_bindings(&decl.name.name, &generics);
                        self.ctx.type_aliases.insert(
                            def_id,
                            TypeAliasTemplate {
                                generics,
                                target: decl.target.clone(),
                            },
                        );
                    }
                    ItemKind::ExternBlock(block) => {
                        for extern_item in &block.items {
                            match extern_item {
                                ExternItem::OpaqueStruct(name) => {
                                    let Some(def_id) = self.ctx.resolved.local_def(
                                        module.id,
                                        &name.name,
                                        &[DefKind::OpaqueStruct],
                                    ) else {
                                        continue;
                                    };
                                    let nominal_name =
                                        nominal_type_name(&self.ctx.resolved, def_id);
                                    self.ctx
                                        .nominal_type_defs
                                        .insert(nominal_name.clone(), def_id);
                                    self.ctx.opaque_structs.insert(nominal_name);
                                }
                                ExternItem::TypeAlias(decl) => {
                                    let Some(def_id) = self.ctx.resolved.local_def(
                                        module.id,
                                        &decl.name.name,
                                        &[DefKind::TypeAlias],
                                    ) else {
                                        continue;
                                    };
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
                                    self.validate_generic_bindings(&decl.name.name, &generics);
                                    self.ctx.type_aliases.insert(
                                        def_id,
                                        TypeAliasTemplate {
                                            generics,
                                            target: decl.target.clone(),
                                        },
                                    );
                                }
                                ExternItem::Function { .. } => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    pub(super) fn check_by_value_layout_cycles(&mut self) {
        let structs = self
            .ctx
            .structs
            .iter()
            .map(|(name, fields)| CheckedStruct {
                name: name.clone(),
                is_resource: self.ctx.resource_structs.contains(name),
                fields: fields.clone(),
            })
            .collect::<Vec<_>>();
        let enums = self.ctx.checked_enums.values().cloned().collect::<Vec<_>>();
        self.diagnostics
            .extend(check_checked_aggregate_layouts(&structs, &enums));
    }

    pub(super) fn collect_structs(&mut self) {
        let modules = self.ctx.hir_modules.clone();
        for module in &modules {
            self.current_module = module.id;
            for item in &module.items {
                if let ItemKind::Struct(decl) = &item.kind {
                    let Some(def_id) =
                        self.ctx
                            .resolved
                            .local_def(module.id, &decl.name.name, &[DefKind::Struct])
                    else {
                        continue;
                    };
                    let nominal_name = nominal_type_name(&self.ctx.resolved, def_id);
                    self.ctx
                        .nominal_type_defs
                        .insert(nominal_name.clone(), def_id);
                    if decl.generics.is_empty() {
                        if decl.is_resource {
                            self.ctx.resource_structs.insert(nominal_name.clone());
                        }
                        if decl.is_unsafe {
                            self.ctx.unsafe_structs.insert(nominal_name.clone());
                        }
                    }
                }
            }
        }
        for module in &modules {
            self.current_module = module.id;
            for item in &module.items {
                if let ItemKind::Struct(decl) = &item.kind {
                    let Some(def_id) =
                        self.ctx
                            .resolved
                            .local_def(module.id, &decl.name.name, &[DefKind::Struct])
                    else {
                        continue;
                    };
                    let nominal_name = nominal_type_name(&self.ctx.resolved, def_id);
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
                    self.validate_generic_bindings(&decl.name.name, &generics);
                    self.ctx.struct_templates.insert(
                        nominal_name,
                        StructTemplate {
                            is_resource: decl.is_resource,
                            is_unsafe: decl.is_unsafe,
                            generics,
                            fields: decl.fields.clone(),
                        },
                    );
                }
            }
        }
    }

    pub(super) fn collect_enums(&mut self) {
        let modules = self.ctx.hir_modules.clone();
        for module in &modules {
            self.current_module = module.id;
            for item in &module.items {
                if let ItemKind::Enum(decl) = &item.kind {
                    let Some(enum_def_id) =
                        self.ctx
                            .resolved
                            .local_def(module.id, &decl.name.name, &[DefKind::Enum])
                    else {
                        continue;
                    };
                    let enum_name = nominal_type_name(&self.ctx.resolved, enum_def_id);
                    self.ctx
                        .nominal_type_defs
                        .insert(enum_name.clone(), enum_def_id);
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
                    self.validate_generic_bindings(&decl.name.name, &generics);
                    let generic_names = generics
                        .iter()
                        .map(|generic| generic.name.clone())
                        .collect::<Vec<_>>();
                    let variants = decl
                        .variants
                        .iter()
                        .map(|variant| EnumVariantTemplate {
                            name: variant.name.name.clone(),
                            payload: variant.payload.clone(),
                        })
                        .collect::<Vec<_>>();
                    for (variant_index, variant) in decl.variants.iter().enumerate() {
                        let Some(def_id) = self.ctx.resolved.local_enum_variant_def(
                            module.id,
                            enum_def_id,
                            &variant.name.name,
                        ) else {
                            continue;
                        };
                        self.ctx.variants.insert(
                            def_id,
                            VariantSig {
                                enum_name: enum_name.clone(),
                                enum_generics: generic_names.clone(),
                                variant_index,
                                payload: variant.payload.clone(),
                            },
                        );
                    }
                    self.ctx
                        .enum_templates
                        .insert(enum_name.clone(), EnumTemplate { generics, variants });
                }
            }
        }
    }

    pub(super) fn instantiate_declared_aggregate_instances(&mut self) {
        let structs = Self::zero_arg_aggregate_instances(&self.ctx.struct_templates, |template| {
            &template.generics
        });
        for ty in structs {
            self.ensure_struct_instance(&ty);
        }

        let enums = Self::zero_arg_aggregate_instances(&self.ctx.enum_templates, |template| {
            &template.generics
        });
        for ty in enums {
            self.ensure_enum_instance(&ty);
        }
    }

    pub(super) fn zero_arg_aggregate_instances<T>(
        templates: &HashMap<String, T>,
        generics: impl Fn(&T) -> &[GenericInfo],
    ) -> Vec<Ty> {
        templates
            .iter()
            .filter_map(|(name, template)| {
                generics(template).is_empty().then(|| Ty::Named {
                    name: name.clone(),
                    args: Vec::new(),
                })
            })
            .collect()
    }
}
