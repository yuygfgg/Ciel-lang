use super::*;

impl TypeChecker {
    pub(super) fn lower_type(&mut self, ty: &Type) -> Ty {
        let subst = self.current_type_subst();
        let lowered = self.lower_type_with_subst_inner(ty, &subst, false);
        self.ensure_enum_instance(&lowered);
        self.ensure_struct_instance(&lowered);
        lowered
    }

    pub(super) fn lower_type_allowing_holes(&mut self, ty: &Type) -> Ty {
        let subst = self.current_type_subst();
        let lowered = self.lower_type_with_subst_inner(ty, &subst, true);
        if !contains_type_hole(&lowered) {
            self.ensure_enum_instance(&lowered);
            self.ensure_struct_instance(&lowered);
        }
        lowered
    }

    pub(super) fn lower_type_with_subst(&mut self, ty: &Type, subst: &HashMap<String, Ty>) -> Ty {
        self.lower_type_with_subst_inner(ty, subst, false)
    }

    pub(super) fn lower_type_with_subst_allowing_holes(
        &mut self,
        ty: &Type,
        subst: &HashMap<String, Ty>,
    ) -> Ty {
        self.lower_type_with_subst_inner(ty, subst, true)
    }

    pub(super) fn lower_type_with_subst_no_normalize(
        &mut self,
        ty: &Type,
        subst: &HashMap<String, Ty>,
    ) -> Ty {
        self.lower_type_with_subst_inner_mode(ty, subst, false, false)
    }

    pub(super) fn lower_function_return_type(
        &mut self,
        def_id: DefId,
        ret: &FunctionReturnType,
        generics: &[GenericInfo],
        subst: &HashMap<String, Ty>,
    ) -> Ty {
        match ret {
            FunctionReturnType::Type(ty) => self.lower_type_with_subst(ty, subst),
            FunctionReturnType::OpaqueConstraint { constraint, .. } => {
                let bounds = self.constraint_bounds(constraint, subst);
                let args = generics
                    .iter()
                    .map(|generic| {
                        subst
                            .get(&generic.name)
                            .cloned()
                            .unwrap_or_else(|| Ty::Generic(generic.name.clone()))
                    })
                    .collect();
                Ty::OpaqueReturn {
                    key: OpaqueReturnKey { def_id, args },
                    bounds,
                }
            }
        }
    }

    pub(super) fn lower_source_generic_args(
        &mut self,
        kind: &str,
        name: &str,
        generics: &[GenericInfo],
        args: &[Type],
        outer_subst: &HashMap<String, Ty>,
        allow_holes: bool,
        span: crate::span::Span,
    ) -> Option<Vec<Ty>> {
        let explicit_count = Self::explicit_generic_count(generics);
        if args.len() != explicit_count {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "{kind} `{name}` expects {explicit_count} type arguments, got {}",
                    args.len()
                ),
            ));
            return None;
        }

        let mut subst = outer_subst.clone();
        for (generic, arg) in generics
            .iter()
            .filter(|generic| !generic.is_hidden)
            .zip(args.iter())
        {
            let concrete = self.lower_type_with_subst_inner(arg, outer_subst, allow_holes);
            subst.insert(generic.name.clone(), concrete);
        }
        if !self.solve_hidden_generics(name, generics, &mut subst, span) {
            return None;
        }
        let result = generics
            .iter()
            .map(|generic| {
                subst
                    .get(&generic.name)
                    .cloned()
                    .unwrap_or_else(|| Ty::Generic(generic.name.clone()))
            })
            .collect();
        Some(result)
    }

    pub(super) fn solve_hidden_generics(
        &mut self,
        owner_name: &str,
        generics: &[GenericInfo],
        subst: &mut HashMap<String, Ty>,
        span: crate::span::Span,
    ) -> bool {
        let hidden_names = generics
            .iter()
            .filter(|generic| generic.is_hidden)
            .map(|generic| generic.name.clone())
            .collect::<HashSet<_>>();
        if hidden_names.is_empty() {
            return true;
        }

        for _ in 0..=hidden_names.len() {
            let before = subst.clone();
            for generic in generics {
                let Some(receiver_ty) = subst.get(&generic.name).cloned() else {
                    continue;
                };
                let Some(constraint) = &generic.constraint else {
                    continue;
                };
                let bounds = self.constraint_bounds(constraint, subst);
                for capability in bounds.positive {
                    self.solve_hidden_from_capability(
                        &receiver_ty,
                        &capability,
                        &hidden_names,
                        subst,
                    );
                }
            }
            if *subst == before {
                break;
            }
        }

        let mut ok = true;
        for generic in generics.iter().filter(|generic| generic.is_hidden) {
            if !subst.contains_key(&generic.name) {
                ok = false;
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "cannot instantiate `{owner_name}`: could not infer hidden parameter `{}`",
                        generic.name
                    ),
                ));
            }
        }
        ok
    }

    pub(super) fn solve_hidden_from_capability(
        &mut self,
        receiver_ty: &Ty,
        capability: &ConstraintRef,
        hidden_names: &HashSet<String>,
        subst: &mut HashMap<String, Ty>,
    ) {
        let assumptions = self.hidden_solver_assumptions(receiver_ty);
        match capability_solve::solve_hidden_from_capability(
            &self.ctx,
            receiver_ty,
            capability,
            hidden_names,
            &assumptions,
        ) {
            capability_solve::HiddenSolveResult::Unique(bindings) => {
                for (name, ty) in bindings {
                    if hidden_names.contains(&name) {
                        subst.insert(name, ty);
                    }
                }
            }
            capability_solve::HiddenSolveResult::Ambiguous => {
                self.diagnostics.push(Diagnostic::new(
                    None,
                    format!(
                        "ambiguous hidden parameter inference for capability `{}`",
                        capability.name
                    ),
                ));
            }
            capability_solve::HiddenSolveResult::NoSolution => {}
        }
    }

    pub(super) fn hidden_solver_assumptions(
        &mut self,
        receiver_ty: &Ty,
    ) -> Vec<capability_solve::HiddenConstraint> {
        let mut assumptions = Vec::new();
        if let Ty::OpaqueReturn { bounds, .. } = receiver_ty {
            assumptions.extend(bounds.positive.iter().cloned().map(|capability| {
                capability_solve::HiddenConstraint {
                    receiver: receiver_ty.clone(),
                    capability,
                }
            }));
        }
        let envs = self.generic_env_stack.clone();
        for env in envs {
            let env_subst = Self::initial_generic_subst(&env);
            for generic in &env {
                let Some(constraint) = &generic.constraint else {
                    continue;
                };
                let bounds = self.constraint_bounds(constraint, &env_subst);
                for capability in bounds.positive {
                    assumptions.push(capability_solve::HiddenConstraint {
                        receiver: Ty::Generic(generic.name.clone()),
                        capability,
                    });
                }
            }
        }
        assumptions
    }

    pub(super) fn validate_generic_bindings(&mut self, owner: &str, generics: &[GenericInfo]) {
        for generic in generics {
            if let Some(constraint) = &generic.constraint {
                self.validate_constraint_bindings_in_positive_terms(constraint);
            }
        }
        self.validate_hidden_derivability(owner, generics);
    }

    pub(super) fn validate_constraint_bindings_in_positive_terms(
        &mut self,
        constraint: &ConstraintExpr,
    ) {
        for term in &constraint.terms {
            for arg in &term.args {
                self.validate_constraint_arg_binding_position(arg, term.negated || term.removed);
            }
        }
    }

    pub(super) fn validate_constraint_arg_binding_position(
        &mut self,
        arg: &ConstraintArg,
        forbidden: bool,
    ) {
        match arg {
            ConstraintArg::Binding {
                name, constraint, ..
            } => {
                if forbidden {
                    self.diagnostics.push(Diagnostic::new(
                        name.span,
                        format!(
                            "named constraint binding `{} = _` is not allowed in negative or removed capability constraints",
                            name.name
                        ),
                    ));
                }
                if let Some(constraint) = constraint {
                    self.validate_constraint_bindings_in_positive_terms(constraint);
                }
            }
            ConstraintArg::Type(_) => {}
        }
    }

    pub(super) fn validate_constraint_bindings_forbidden(
        &mut self,
        constraint: &ConstraintExpr,
        context: &str,
    ) {
        for term in &constraint.terms {
            for arg in &term.args {
                self.validate_constraint_arg_bindings_forbidden(arg, context);
            }
        }
    }

    pub(super) fn validate_constraint_arg_bindings_forbidden(
        &mut self,
        arg: &ConstraintArg,
        context: &str,
    ) {
        match arg {
            ConstraintArg::Binding {
                name, constraint, ..
            } => {
                self.diagnostics.push(Diagnostic::new(
                    name.span,
                    format!(
                        "named constraint binding `{} = _` is not allowed in {context}",
                        name.name
                    ),
                ));
                if let Some(constraint) = constraint {
                    self.validate_constraint_bindings_forbidden(constraint, context);
                }
            }
            ConstraintArg::Type(_) => {}
        }
    }

    pub(super) fn validate_hidden_derivability(&mut self, owner: &str, generics: &[GenericInfo]) {
        let hidden_names = generics
            .iter()
            .filter(|generic| generic.is_hidden)
            .map(|generic| generic.name.clone())
            .collect::<HashSet<_>>();
        if hidden_names.is_empty() {
            return;
        }
        let mut known = generics
            .iter()
            .filter(|generic| !generic.is_hidden)
            .map(|generic| generic.name.clone())
            .collect::<HashSet<_>>();
        let subst = Self::initial_generic_subst(generics);
        for _ in 0..=hidden_names.len() {
            let before = known.clone();
            for generic in generics {
                if !known.contains(&generic.name) {
                    continue;
                }
                let Some(constraint) = &generic.constraint else {
                    continue;
                };
                let bounds = self.constraint_bounds(constraint, &subst);
                for capability in bounds.positive {
                    let Some(interface) = self.ctx.interfaces.get(&capability.def_id) else {
                        continue;
                    };
                    let Some(determined_start) = interface.determined_start else {
                        continue;
                    };
                    let full_args = std::iter::once(Ty::Generic(generic.name.clone()))
                        .chain(capability.args.into_iter())
                        .collect::<Vec<_>>();
                    if full_args.len() != interface.generics.len() {
                        continue;
                    }
                    if !full_args
                        .iter()
                        .take(determined_start)
                        .all(|ty| ty_generic_names(ty).is_subset(&known))
                    {
                        continue;
                    }
                    for ty in full_args.iter().skip(determined_start) {
                        known.extend(
                            ty_generic_names(ty)
                                .into_iter()
                                .filter(|name| hidden_names.contains(name)),
                        );
                    }
                }
            }
            if known == before {
                break;
            }
        }
        for hidden in hidden_names.difference(&known) {
            self.diagnostics.push(Diagnostic::new(
                None,
                format!("hidden parameter `{hidden}` in `{owner}` is not determined by explicit parameters"),
            ));
        }
    }

    pub(super) fn lower_type_with_subst_inner(
        &mut self,
        ty: &Type,
        subst: &HashMap<String, Ty>,
        allow_holes: bool,
    ) -> Ty {
        self.lower_type_with_subst_inner_mode(ty, subst, allow_holes, true)
    }

    pub(super) fn lower_type_with_subst_inner_mode(
        &mut self,
        ty: &Type,
        subst: &HashMap<String, Ty>,
        allow_holes: bool,
        normalize_meta_repr: bool,
    ) -> Ty {
        let lowered = match &ty.kind {
            TypeKind::Hole => {
                if allow_holes {
                    let id = self.next_type_hole_id;
                    self.next_type_hole_id += 1;
                    Ty::Hole(id)
                } else {
                    self.diagnostics.push(Diagnostic::new(
                        ty.span,
                        "type hole is only allowed in initialized local declarations",
                    ));
                    Ty::Unknown
                }
            }
            TypeKind::Never => Ty::Never,
            TypeKind::Void => Ty::Void,
            TypeKind::Primitive(primitive) => ty_from_primitive(primitive),
            TypeKind::Named(name, args) => {
                let display_name = &name.display;
                if matches!(name.kind, TypeNameKind::Error) {
                    Ty::Unknown
                } else if args.is_empty()
                    && let TypeNameKind::Generic(generic_name) = &name.kind
                    && let Some(replacement) = subst.get(generic_name)
                {
                    let replacement = replacement.clone();
                    if !contains_type_hole(&replacement) {
                        self.ensure_enum_instance(&replacement);
                        self.ensure_struct_instance(&replacement);
                    }
                    return replacement;
                } else if args.is_empty()
                    && let TypeNameKind::Generic(generic_name) = &name.kind
                {
                    Ty::Generic(generic_name.clone())
                } else if let TypeNameKind::Def(def_id) = &name.kind {
                    let def_id = *def_id;
                    let def = self.ctx.resolved.def(def_id).clone();
                    if let Some(normalized) =
                        self.lower_std_meta_repr_type(ty.span, def_id, args, subst, allow_holes)
                    {
                        normalized
                    } else if def.kind == DefKind::TypeAlias {
                        self.expand_type_alias(ty.span, def_id, args, subst, allow_holes)
                    } else if let Some(interface) = self.ctx.interfaces.get(&def_id).cloned() {
                        let required_args = interface.generics.len().saturating_sub(1);
                        if args.len() != required_args {
                            self.diagnostics.push(Diagnostic::new(
                                ty.span,
                                format!(
                                    "dynamic interface `{}` requires {required_args} non-receiver type arguments",
                                    display_name
                                ),
                            ));
                        }
                        if !interface_receiver_is_input(&interface) {
                            self.diagnostics.push(Diagnostic::new(
                                ty.span,
                                format!(
                                    "interface `{}` cannot be used dynamically because its receiver is not an input parameter",
                                    display_name
                                ),
                            ));
                        }
                        Ty::DynamicInterface {
                            def_id,
                            name: def.name,
                            args: args
                                .iter()
                                .map(|arg| {
                                    self.lower_type_with_subst_inner_mode(
                                        arg,
                                        subst,
                                        allow_holes,
                                        normalize_meta_repr,
                                    )
                                })
                                .collect(),
                        }
                    } else if self.ctx.interface_aliases.contains_key(&def_id) {
                        let alias_args = args
                            .iter()
                            .map(|arg| {
                                self.lower_type_with_subst_inner_mode(
                                    arg,
                                    subst,
                                    allow_holes,
                                    normalize_meta_repr,
                                )
                            })
                            .collect::<Vec<_>>();
                        let view = self.interface_view_for_def(def_id, &alias_args);
                        for entry in view.positive.iter().chain(view.negative.iter()) {
                            if let Some(interface) =
                                self.interface_sig_by_def(entry.def_id).cloned()
                            {
                                let required_args = interface.generics.len().saturating_sub(1);
                                if entry.args.len() != required_args {
                                    self.diagnostics.push(Diagnostic::new(
                                        ty.span,
                                        format!(
                                            "dynamic interface alias `{}` leaves `{}` without {required_args} non-receiver type arguments",
                                            display_name, entry.name
                                        ),
                                    ));
                                }
                                if !interface_receiver_is_input(&interface) {
                                    self.diagnostics.push(Diagnostic::new(
                                        ty.span,
                                        format!(
                                            "interface alias `{}` cannot be used dynamically because `{}` has no input receiver",
                                            display_name, entry.name
                                        ),
                                    ));
                                }
                            }
                        }
                        Ty::DynamicInterface {
                            def_id,
                            name: def.name,
                            args: alias_args,
                        }
                    } else {
                        let nominal_name = nominal_type_name(&self.ctx.resolved, def_id);
                        let canonical_args = if let Some(template) =
                            self.ctx.struct_templates.get(&nominal_name).cloned()
                        {
                            self.lower_source_generic_args(
                                "struct",
                                &nominal_name,
                                &template.generics,
                                args,
                                subst,
                                allow_holes,
                                ty.span,
                            )
                        } else if let Some(template) =
                            self.ctx.enum_templates.get(&nominal_name).cloned()
                        {
                            self.lower_source_generic_args(
                                "enum",
                                &nominal_name,
                                &template.generics,
                                args,
                                subst,
                                allow_holes,
                                ty.span,
                            )
                        } else {
                            Some(
                                args.iter()
                                    .map(|arg| {
                                        self.lower_type_with_subst_inner_mode(
                                            arg,
                                            subst,
                                            allow_holes,
                                            normalize_meta_repr,
                                        )
                                    })
                                    .collect(),
                            )
                        };
                        Ty::Named {
                            name: nominal_name,
                            args: canonical_args.unwrap_or_default(),
                        }
                    }
                } else {
                    self.diagnostics.push(Diagnostic::new(
                        ty.span,
                        format!("unknown type `{display_name}`"),
                    ));
                    Ty::Unknown
                }
            }
            TypeKind::Pointer {
                nullable,
                mutability,
                inner,
            } => Ty::Pointer {
                nullable: *nullable,
                mutability: *mutability,
                inner: Box::new(self.lower_type_with_subst_inner_mode(
                    inner,
                    subst,
                    allow_holes,
                    normalize_meta_repr,
                )),
            },
            TypeKind::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(self.lower_type_with_subst_inner_mode(
                    elem,
                    subst,
                    allow_holes,
                    normalize_meta_repr,
                )),
            },
            TypeKind::Slice { mutability, elem } => Ty::Slice {
                mutability: *mutability,
                elem: Box::new(self.lower_type_with_subst_inner_mode(
                    elem,
                    subst,
                    allow_holes,
                    normalize_meta_repr,
                )),
            },
            TypeKind::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => Ty::Function {
                is_unsafe: *is_unsafe,
                abi: abi.clone(),
                ret: Box::new(self.lower_type_with_subst_inner_mode(
                    ret,
                    subst,
                    allow_holes,
                    normalize_meta_repr,
                )),
                params: params
                    .iter()
                    .map(|param| {
                        self.lower_type_with_subst_inner_mode(
                            param,
                            subst,
                            allow_holes,
                            normalize_meta_repr,
                        )
                    })
                    .collect(),
            },
            TypeKind::Closure {
                ret,
                params,
                constraint,
            } => {
                if let Some(constraint) = constraint {
                    self.validate_constraint_bindings_forbidden(
                        constraint,
                        "retained closure types",
                    );
                }
                Ty::Closure {
                    ret: Box::new(self.lower_type_with_subst_inner_mode(
                        ret,
                        subst,
                        allow_holes,
                        normalize_meta_repr,
                    )),
                    params: params
                        .iter()
                        .map(|param| {
                            self.lower_type_with_subst_inner_mode(
                                param,
                                subst,
                                allow_holes,
                                normalize_meta_repr,
                            )
                        })
                        .collect(),
                    constraints: constraint
                        .as_ref()
                        .map(|constraint| self.constraint_bounds(constraint, subst))
                        .unwrap_or_default(),
                }
            }
        };
        if normalize_meta_repr {
            let lowered = self.normalize_meta_repr_markers(&lowered, ty.span);
            if !contains_type_hole(&lowered) {
                self.ensure_enum_instance(&lowered);
                self.ensure_struct_instance(&lowered);
            }
            return lowered;
        }
        lowered
    }

    pub(super) fn current_type_subst(&self) -> HashMap<String, Ty> {
        self.type_subst_stack.last().cloned().unwrap_or_default()
    }

    pub(super) fn explicit_generic_count(generics: &[GenericInfo]) -> usize {
        generics.iter().filter(|generic| !generic.is_hidden).count()
    }

    pub(super) fn initial_generic_subst(generics: &[GenericInfo]) -> HashMap<String, Ty> {
        generics
            .iter()
            .map(|generic| (generic.name.clone(), Ty::Generic(generic.name.clone())))
            .collect()
    }

    pub(super) fn with_generic_env<T>(
        &mut self,
        generics: &[GenericInfo],
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.generic_env_stack.push(generics.to_vec());
        let result = f(self);
        self.generic_env_stack.pop();
        result
    }

    pub(super) fn resolve_type_holes(&self, ty: &Ty) -> Ty {
        match ty {
            Ty::Hole(id) => self
                .type_hole_solutions
                .get(id)
                .map(|solution| self.resolve_type_holes(solution))
                .unwrap_or(Ty::Hole(*id)),
            Ty::Pointer {
                nullable,
                mutability,
                inner,
            } => Ty::Pointer {
                nullable: *nullable,
                mutability: *mutability,
                inner: Box::new(self.resolve_type_holes(inner)),
            },
            Ty::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(self.resolve_type_holes(elem)),
            },
            Ty::Slice { mutability, elem } => Ty::Slice {
                mutability: *mutability,
                elem: Box::new(self.resolve_type_holes(elem)),
            },
            Ty::Named { name, args } => Ty::Named {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.resolve_type_holes(arg))
                    .collect(),
            },
            Ty::DynamicInterface { def_id, name, args } => Ty::DynamicInterface {
                def_id: *def_id,
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.resolve_type_holes(arg))
                    .collect(),
            },
            Ty::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => Ty::Function {
                is_unsafe: *is_unsafe,
                abi: abi.clone(),
                ret: Box::new(self.resolve_type_holes(ret)),
                params: params
                    .iter()
                    .map(|param| self.resolve_type_holes(param))
                    .collect(),
            },
            Ty::Closure {
                ret,
                params,
                constraints,
            } => Ty::Closure {
                ret: Box::new(self.resolve_type_holes(ret)),
                params: params
                    .iter()
                    .map(|param| self.resolve_type_holes(param))
                    .collect(),
                constraints: self.resolve_constraint_bounds_type_holes(constraints),
            },
            Ty::ClosureInstance {
                id,
                ret,
                params,
                captures,
            } => Ty::ClosureInstance {
                id: *id,
                ret: Box::new(self.resolve_type_holes(ret)),
                params: params
                    .iter()
                    .map(|param| self.resolve_type_holes(param))
                    .collect(),
                captures: captures
                    .iter()
                    .map(|capture| self.resolve_type_holes(capture))
                    .collect(),
            },
            other => other.clone(),
        }
    }

    pub(super) fn bind_type_hole(&mut self, id: usize, ty: &Ty) -> bool {
        let ty = self.resolve_type_holes(ty);
        if matches!(ty, Ty::Hole(other) if other == id) || matches!(ty, Ty::Unknown) {
            return true;
        }
        if let Some(existing) = self.type_hole_solutions.get(&id).cloned() {
            return self.unify_type_holes(&existing, &ty);
        }
        self.type_hole_solutions.insert(id, ty);
        true
    }

    pub(super) fn unify_type_holes(&mut self, expected: &Ty, actual: &Ty) -> bool {
        let expected = self.resolve_type_holes(expected);
        let actual = self.resolve_type_holes(actual);
        if self.meta_repr_storage_equivalent(&expected, &actual) {
            return true;
        }
        match (&expected, &actual) {
            (Ty::Hole(id), _) => self.bind_type_hole(*id, &actual),
            (_, Ty::Hole(id)) => self.bind_type_hole(*id, &expected),
            (
                Ty::Pointer {
                    nullable,
                    mutability,
                    inner: expected_inner,
                },
                Ty::Pointer {
                    nullable: actual_nullable,
                    mutability: actual_mutability,
                    inner: actual_inner,
                },
            ) if nullable == actual_nullable && mutability == actual_mutability => {
                self.unify_type_holes(expected_inner, actual_inner)
            }
            (
                Ty::Array {
                    len,
                    elem: expected_elem,
                },
                Ty::Array {
                    len: actual_len,
                    elem: actual_elem,
                },
            ) if len == actual_len => self.unify_type_holes(expected_elem, actual_elem),
            (
                Ty::Slice {
                    mutability,
                    elem: expected_elem,
                },
                Ty::Slice {
                    mutability: actual_mutability,
                    elem: actual_elem,
                },
            ) if mutability == actual_mutability => {
                self.unify_type_holes(expected_elem, actual_elem)
            }
            (
                Ty::Slice {
                    elem: expected_elem,
                    ..
                },
                Ty::Array {
                    elem: actual_elem, ..
                },
            ) => self.unify_type_holes(expected_elem, actual_elem),
            (
                Ty::Named { name, args },
                Ty::Named {
                    name: actual_name,
                    args: actual_args,
                },
            ) if name == actual_name && args.len() == actual_args.len() => args
                .iter()
                .zip(actual_args.iter())
                .all(|(expected, actual)| self.unify_type_holes(expected, actual)),
            (
                Ty::DynamicInterface { def_id, args, .. },
                Ty::DynamicInterface {
                    def_id: actual_def_id,
                    args: actual_args,
                    ..
                },
            ) if def_id == actual_def_id && args.len() == actual_args.len() => args
                .iter()
                .zip(actual_args.iter())
                .all(|(expected, actual)| self.unify_type_holes(expected, actual)),
            (
                Ty::Function {
                    is_unsafe,
                    abi,
                    ret,
                    params,
                },
                Ty::Function {
                    is_unsafe: actual_is_unsafe,
                    abi: actual_abi,
                    ret: actual_ret,
                    params: actual_params,
                },
            ) if is_unsafe == actual_is_unsafe
                && abi == actual_abi
                && params.len() == actual_params.len() =>
            {
                self.unify_type_holes(ret, actual_ret)
                    && params
                        .iter()
                        .zip(actual_params.iter())
                        .all(|(expected, actual)| self.unify_type_holes(expected, actual))
            }
            (
                Ty::Closure { ret, params, .. },
                Ty::Closure {
                    ret: actual_ret,
                    params: actual_params,
                    ..
                },
            )
            | (
                Ty::Closure { ret, params, .. },
                Ty::ClosureInstance {
                    ret: actual_ret,
                    params: actual_params,
                    ..
                },
            ) if params.len() == actual_params.len() => {
                self.unify_type_holes(ret, actual_ret)
                    && params
                        .iter()
                        .zip(actual_params.iter())
                        .all(|(expected, actual)| self.unify_type_holes(expected, actual))
            }
            (
                Ty::ClosureInstance {
                    id,
                    ret,
                    params,
                    captures,
                },
                Ty::ClosureInstance {
                    id: actual_id,
                    ret: actual_ret,
                    params: actual_params,
                    captures: actual_captures,
                },
            ) if id == actual_id
                && params.len() == actual_params.len()
                && captures.len() == actual_captures.len() =>
            {
                self.unify_type_holes(ret, actual_ret)
                    && params
                        .iter()
                        .zip(actual_params.iter())
                        .all(|(expected, actual)| self.unify_type_holes(expected, actual))
                    && captures
                        .iter()
                        .zip(actual_captures.iter())
                        .all(|(expected, actual)| self.unify_type_holes(expected, actual))
            }
            _ => expected == actual,
        }
    }

    pub(super) fn unify_ty_for_inference(
        &mut self,
        pattern: &Ty,
        actual: &Ty,
        subst: &mut HashMap<String, Ty>,
    ) -> bool {
        let pattern = self.substitute_ty_normalized_silent(pattern, subst);
        let pattern = self.resolve_type_holes(&pattern);
        let actual = self.resolve_type_holes(actual);
        if self.meta_repr_storage_equivalent(&pattern, &actual) {
            return true;
        }
        if let Ty::Generic(name) = &pattern
            && let Some(existing) = subst.get(name).cloned()
            && self.meta_repr_storage_equivalent(&existing, &actual)
        {
            return true;
        }
        match &pattern {
            Ty::Hole(id) => self.bind_type_hole(*id, &actual),
            Ty::Generic(name) => match subst.get(name).cloned() {
                Some(Ty::Generic(existing)) if existing == *name => {
                    subst.insert(name.clone(), actual);
                    true
                }
                Some(existing) => {
                    let ok = self.unify_ty_for_inference(&existing, &actual, subst);
                    if ok {
                        subst.insert(name.clone(), self.resolve_type_holes(&existing));
                    }
                    ok
                }
                None => {
                    subst.insert(name.clone(), actual);
                    true
                }
            },
            Ty::Pointer {
                nullable,
                mutability,
                inner: pattern_inner,
            } => match &actual {
                Ty::Pointer {
                    nullable: actual_nullable,
                    mutability: actual_mutability,
                    inner: actual_inner,
                } if (*nullable || !*actual_nullable)
                    && pointer_view_can_weaken(*mutability, *actual_mutability) =>
                {
                    self.unify_ty_for_inference(pattern_inner, actual_inner, subst)
                }
                _ => false,
            },
            Ty::Array {
                len,
                elem: pattern_elem,
            } => match &actual {
                Ty::Array {
                    len: actual_len,
                    elem: actual_elem,
                } if len == actual_len => {
                    self.unify_ty_for_inference(pattern_elem, actual_elem, subst)
                }
                _ => false,
            },
            Ty::Slice {
                mutability,
                elem: pattern_elem,
            } => match &actual {
                Ty::Slice {
                    mutability: actual_mutability,
                    elem: actual_elem,
                } if pointer_view_can_weaken(*mutability, *actual_mutability) => {
                    self.unify_ty_for_inference(pattern_elem, actual_elem, subst)
                }
                Ty::Array {
                    elem: actual_elem, ..
                } => self.unify_ty_for_inference(pattern_elem, actual_elem, subst),
                _ => false,
            },
            Ty::Named { name, args } => match &actual {
                Ty::Named {
                    name: actual_name,
                    args: actual_args,
                } if name == actual_name && args.len() == actual_args.len() => args
                    .iter()
                    .zip(actual_args.iter())
                    .all(|(pattern, actual)| self.unify_ty_for_inference(pattern, actual, subst)),
                Ty::GeneratedFuture { .. } => std_id::unify_std_async_future_with_generated(
                    &self.ctx.resolved,
                    &pattern,
                    &actual,
                    subst,
                ),
                _ => false,
            },
            Ty::DynamicInterface { def_id, args, .. } => match &actual {
                Ty::DynamicInterface {
                    def_id: actual_def_id,
                    args: actual_args,
                    ..
                } if def_id == actual_def_id && args.len() == actual_args.len() => args
                    .iter()
                    .zip(actual_args.iter())
                    .all(|(pattern, actual)| self.unify_ty_for_inference(pattern, actual, subst)),
                _ => false,
            },
            Ty::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => match &actual {
                Ty::Function {
                    is_unsafe: actual_is_unsafe,
                    abi: actual_abi,
                    ret: actual_ret,
                    params: actual_params,
                } if is_unsafe == actual_is_unsafe
                    && abi == actual_abi
                    && params.len() == actual_params.len() =>
                {
                    self.unify_ty_for_inference(ret, actual_ret, subst)
                        && params
                            .iter()
                            .zip(actual_params.iter())
                            .all(|(pattern, actual)| {
                                self.unify_ty_for_inference(pattern, actual, subst)
                            })
                }
                _ => false,
            },
            Ty::Closure {
                ret,
                params,
                constraints,
            } => match &actual {
                Ty::Closure {
                    ret: actual_ret,
                    params: actual_params,
                    constraints: actual_constraints,
                } if params.len() == actual_params.len() => {
                    self.unify_ty_for_inference(ret, actual_ret, subst)
                        && params
                            .iter()
                            .zip(actual_params.iter())
                            .all(|(pattern, actual)| {
                                self.unify_ty_for_inference(pattern, actual, subst)
                            })
                        && self.unify_constraint_bounds_for_inference(
                            constraints,
                            actual_constraints,
                            subst,
                        )
                }
                Ty::ClosureInstance {
                    ret: actual_ret,
                    params: actual_params,
                    ..
                } if params.len() == actual_params.len() => {
                    self.unify_ty_for_inference(ret, actual_ret, subst)
                        && params
                            .iter()
                            .zip(actual_params.iter())
                            .all(|(pattern, actual)| {
                                self.unify_ty_for_inference(pattern, actual, subst)
                            })
                }
                _ => false,
            },
            Ty::ClosureInstance {
                id,
                ret,
                params,
                captures,
            } => match &actual {
                Ty::ClosureInstance {
                    id: actual_id,
                    ret: actual_ret,
                    params: actual_params,
                    captures: actual_captures,
                } if id == actual_id
                    && params.len() == actual_params.len()
                    && captures.len() == actual_captures.len() =>
                {
                    self.unify_ty_for_inference(ret, actual_ret, subst)
                        && params
                            .iter()
                            .zip(actual_params.iter())
                            .all(|(pattern, actual)| {
                                self.unify_ty_for_inference(pattern, actual, subst)
                            })
                        && captures
                            .iter()
                            .zip(actual_captures.iter())
                            .all(|(pattern, actual)| {
                                self.unify_ty_for_inference(pattern, actual, subst)
                            })
                }
                _ => false,
            },
            other => other == &actual,
        }
    }

    pub(super) fn unify_constraint_bounds_for_inference(
        &mut self,
        pattern: &ConstraintBounds,
        actual: &ConstraintBounds,
        subst: &mut HashMap<String, Ty>,
    ) -> bool {
        let mut trial = subst.clone();
        if !self.unify_constraint_refs_for_inference(
            &pattern.positive,
            &actual.positive,
            &mut trial,
        ) {
            return false;
        }
        if !self.unify_constraint_refs_for_inference(
            &pattern.negative,
            &actual.negative,
            &mut trial,
        ) {
            return false;
        }
        *subst = trial;
        true
    }

    pub(super) fn unify_constraint_refs_for_inference(
        &mut self,
        pattern: &[ConstraintRef],
        actual: &[ConstraintRef],
        subst: &mut HashMap<String, Ty>,
    ) -> bool {
        let Some((first, rest)) = pattern.split_first() else {
            return true;
        };
        for candidate in actual {
            if first.name != candidate.name || first.args.len() != candidate.args.len() {
                continue;
            }
            let mut trial = subst.clone();
            if !first
                .args
                .iter()
                .zip(candidate.args.iter())
                .all(|(pattern_arg, actual_arg)| {
                    self.unify_ty_for_inference(pattern_arg, actual_arg, &mut trial)
                })
            {
                continue;
            }
            if self.unify_constraint_refs_for_inference(rest, actual, &mut trial) {
                *subst = trial;
                return true;
            }
        }
        false
    }

    pub(super) fn check_local_decl_init(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        ty: &Type,
        local_name: &str,
        init: Option<&Expr>,
    ) -> CheckedLocalInit {
        if !hir_type_contains_hole(ty) {
            let erased_after_generic_subst = hir_type_contains_generic(ty);
            let ty = self.lower_type(ty);
            self.reject_invalid_plain_value_type(&ty, span, "local variable");
            let init = if ty.is_erased_value() {
                if let Some(expr) = init {
                    if erased_after_generic_subst {
                        self.check_expr(scopes, expr, Some(&ty))
                    } else {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            "void values are implicit and cannot be explicitly initialized",
                        ));
                        None
                    }
                } else {
                    None
                }
            } else {
                init.and_then(|expr| self.check_consumed_expr(scopes, expr, Some(&ty), false))
            };
            let preserve_generated_future = init.as_ref().is_some_and(|init| {
                std_id::std_async_future_accepts_generated(&self.ctx.resolved, &ty, &init.ty)
            });
            if let Some(init) = &init
                && !preserve_generated_future
            {
                self.require_assignable(&ty, &init.ty, span);
            }
            let ty = if preserve_generated_future {
                let init = init.as_ref().expect("preserved generated future has init");
                init.ty.clone()
            } else {
                ty
            };
            return CheckedLocalInit {
                assigned: init.as_ref().is_some_and(|init| !init.is_never())
                    || ty.is_erased_value(),
                ty,
                init,
            };
        }

        let Some(init_expr) = init else {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("type hole in local `{local_name}` requires an initializer"),
            ));
            return CheckedLocalInit {
                ty: Ty::Unknown,
                init: None,
                assigned: false,
            };
        };

        let declared_ty = self.lower_type_allowing_holes(ty);
        let expected = (!matches!(declared_ty, Ty::Hole(_))).then_some(&declared_ty);
        let checked_init = self.check_expr(scopes, init_expr, expected);
        if let Some(init) = &checked_init {
            self.unify_type_holes(&declared_ty, &init.ty);
        }

        let mut solved_ty = self.resolve_type_holes(&declared_ty);
        if contains_type_hole(&solved_ty) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("cannot infer type hole in local `{local_name}` from initializer"),
            ));
            solved_ty = Ty::Unknown;
        } else {
            self.ensure_enum_instance(&solved_ty);
            self.ensure_struct_instance(&solved_ty);
            self.reject_invalid_plain_value_type(&solved_ty, span, "local variable");
        }

        let init = checked_init.map(|init| {
            let init = if matches!(solved_ty, Ty::Unknown) {
                init
            } else {
                self.coerce_expr_to_expected(scopes, init, Some(&solved_ty))
            };
            self.consume_affine_expr(scopes, init, false)
        });
        if let Some(init) = &init {
            self.require_assignable(&solved_ty, &init.ty, span);
        }
        if solved_ty.is_erased_value() {
            self.diagnostics.push(Diagnostic::new(
                span,
                "void values are implicit and cannot be explicitly initialized",
            ));
            return CheckedLocalInit {
                ty: solved_ty,
                init: None,
                assigned: true,
            };
        }

        CheckedLocalInit {
            assigned: init.as_ref().is_some_and(|init| !init.is_never()),
            ty: solved_ty,
            init,
        }
    }
}
