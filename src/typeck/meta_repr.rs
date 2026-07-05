use super::*;
use crate::typeck::meta_repr_safety::{
    MetaReprSafetyEnv, meta_structural_repr_unsafe_struct_name, owned_meta_repr_affine_message,
    owned_meta_repr_contains_affine,
};

impl TypeChecker {
    pub(super) fn expand_type_alias(
        &mut self,
        span: crate::span::Span,
        def_id: DefId,
        args: &[Type],
        outer_subst: &HashMap<String, Ty>,
        allow_holes: bool,
    ) -> Ty {
        if self.alias_expansion_stack.contains(&def_id) {
            let name = self.ctx.resolved.def(def_id).name.clone();
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("recursive type alias `{name}`"),
            ));
            return Ty::Unknown;
        }
        let Some(template) = self.ctx.type_aliases.get(&def_id).cloned() else {
            let name = self.ctx.resolved.def(def_id).name.clone();
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("unknown type alias `{name}`"),
            ));
            return Ty::Unknown;
        };
        let name = self.ctx.resolved.def(def_id).name.clone();
        let Some(canonical_args) = self.lower_source_generic_args(
            "type alias",
            &name,
            &template.generics,
            args,
            outer_subst,
            allow_holes,
            span,
        ) else {
            return Ty::Unknown;
        };
        let mut subst = outer_subst.clone();
        for (generic, arg) in template.generics.iter().zip(canonical_args) {
            subst.insert(generic.name.clone(), arg);
        }
        self.alias_expansion_stack.push(def_id);
        let ty = match &template.target {
            TypeAliasTarget::Type(ty) => self.lower_type_with_subst_inner(ty, &subst, allow_holes),
            TypeAliasTarget::CSpelling { abi, spelling } => Ty::CSpelling {
                abi: abi.clone(),
                spelling: spelling.clone(),
            },
        };
        self.alias_expansion_stack.pop();
        ty
    }

    pub(super) fn lower_std_meta_repr_type(
        &mut self,
        span: crate::span::Span,
        def_id: DefId,
        args: &[Type],
        subst: &HashMap<String, Ty>,
        allow_holes: bool,
    ) -> Option<Ty> {
        if std_id::is_std_meta_type(&self.ctx.resolved, def_id, "Schema") {
            if args.len() != 1 {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    "meta::Schema requires exactly one type argument",
                ));
                return Some(Ty::Unknown);
            }
            let source_ty = self.lower_type_with_subst_inner(&args[0], subst, allow_holes);
            if let Ty::Array { len, elem } = &source_ty {
                self.check_meta_schema_array_budget(Some(span), &source_ty, *len, elem, true);
            }
            return Some(std_meta_schema_marker_ty(source_ty));
        }

        let borrowed = if std_id::is_std_meta_type(&self.ctx.resolved, def_id, "RefRepr") {
            true
        } else if std_id::is_std_meta_type(&self.ctx.resolved, def_id, "Repr") {
            false
        } else {
            return None;
        };
        if args.len() != 1 {
            let name = if borrowed { "RefRepr" } else { "Repr" };
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("meta::{name} requires exactly one type argument"),
            ));
            return Some(Ty::Unknown);
        }
        let source_ty = self.lower_type_with_subst_inner(&args[0], subst, allow_holes);
        if let Ty::Array { len, elem } = &source_ty {
            self.check_meta_array_budget(Some(span), &source_ty, *len, elem, true);
        }
        if !borrowed {
            self.reject_owned_meta_repr_affine_source(span, &source_ty);
        }
        Some(std_meta_repr_marker_ty(borrowed, source_ty))
    }

    pub(super) fn should_preserve_meta_repr_marker_source(&self, source_ty: &Ty) -> bool {
        self.defer_meta_repr_expansion
            || self
                .deferred_meta_repr_roots
                .iter()
                .any(|root| root == source_ty)
            || self.is_visiting_aggregate_instance(source_ty)
            || !self.meta_repr_source_visible_from_current_module(source_ty)
            || contains_generic(source_ty)
            || contains_type_hole(source_ty)
    }

    pub(super) fn meta_repr_source_visible_from_current_module(&self, source_ty: &Ty) -> bool {
        match source_ty {
            Ty::Named { name, args: _ } => {
                let current_module = self
                    .meta_reflection_module_stack
                    .last()
                    .copied()
                    .unwrap_or(self.current_module);
                let Some(def_id) = self.ctx.nominal_type_defs.get(name) else {
                    return false;
                };
                let def = self.ctx.resolved.def(*def_id);
                if !matches!(def.kind, DefKind::Struct | DefKind::Enum) {
                    return true;
                }
                if def.module == current_module {
                    return true;
                }
                if std_id::is_std_module(&self.ctx.resolved, def.module)
                    && std_id::is_std_module(&self.ctx.resolved, current_module)
                {
                    return true;
                }
                matches!(
                    self.ctx.resolved
                        .lookup_bare(current_module, &def.name, std::slice::from_ref(&def.kind)),
                    Ok(Some(visible_def_id)) if visible_def_id == *def_id
                )
            }
            Ty::Array { elem, .. } | Ty::Slice { elem, .. } => {
                self.meta_repr_source_visible_from_current_module(elem)
            }
            Ty::Pointer { inner, .. } => self.meta_repr_source_visible_from_current_module(inner),
            Ty::DynamicInterface { args, .. } => args
                .iter()
                .all(|arg| self.meta_repr_source_visible_from_current_module(arg)),
            Ty::Function { ret, params, .. } | Ty::Closure { ret, params, .. } => {
                self.meta_repr_source_visible_from_current_module(ret)
                    && params
                        .iter()
                        .all(|param| self.meta_repr_source_visible_from_current_module(param))
            }
            Ty::ClosureInstance {
                ret,
                params,
                captures,
                ..
            } => {
                self.meta_repr_source_visible_from_current_module(ret)
                    && params
                        .iter()
                        .all(|param| self.meta_repr_source_visible_from_current_module(param))
                    && captures
                        .iter()
                        .all(|capture| self.meta_repr_source_visible_from_current_module(capture))
            }
            Ty::GeneratedFuture { output, .. } => {
                self.meta_repr_source_visible_from_current_module(output)
            }
            _ => true,
        }
    }

    pub(super) fn is_visiting_aggregate_instance(&self, ty: &Ty) -> bool {
        let Ty::Named { name, args } = ty else {
            return false;
        };
        let instance_name = enum_instance_name(name, args);
        self.visiting_structs.contains(&instance_name)
            || self.visiting_enums.contains(&instance_name)
    }

    pub(super) fn normalize_meta_repr_markers(
        &mut self,
        ty: &Ty,
        span: impl Into<Option<crate::span::Span>>,
    ) -> Ty {
        self.normalize_meta_repr_markers_inner(ty, span.into(), true, false)
    }

    pub(super) fn meta_repr_storage_ty(
        &mut self,
        ty: &Ty,
        span: impl Into<Option<crate::span::Span>>,
    ) -> Ty {
        self.meta_repr_storage_ty_inner(ty, span.into(), false)
    }

    pub(super) fn meta_repr_marker_storage_ty(
        &mut self,
        marker: &Ty,
        span: Option<crate::span::Span>,
    ) -> Option<Ty> {
        let (borrowed, source_ty) = meta_repr_marker_source(marker)?;
        if self.should_preserve_meta_repr_marker_source(source_ty) {
            return Some(marker.clone());
        }
        Some(self.meta_repr_ty(span, source_ty, borrowed))
    }

    pub(super) fn meta_schema_marker_storage_ty(
        &mut self,
        marker: &Ty,
        span: Option<crate::span::Span>,
    ) -> Option<Ty> {
        let source_ty = meta_schema_marker_source(marker)?;
        if self.should_preserve_meta_repr_marker_source(source_ty) {
            return Some(marker.clone());
        }
        Some(self.meta_schema_ty(span, source_ty))
    }

    pub(super) fn meta_repr_marker_sop_ty(&mut self, marker: &Ty) -> Option<Ty> {
        let (borrowed, source_ty) = meta_repr_marker_source(marker)?;
        if contains_generic(source_ty) || contains_type_hole(source_ty) {
            return None;
        }
        self.try_meta_repr_ty(source_ty, borrowed)
    }

    pub(super) fn meta_schema_marker_sop_ty(&mut self, marker: &Ty) -> Option<Ty> {
        let source_ty = meta_schema_marker_source(marker)?;
        if contains_generic(source_ty) || contains_type_hole(source_ty) {
            return None;
        }
        self.try_meta_schema_ty(source_ty)
    }

    pub(super) fn meta_repr_field_view_ty(
        &mut self,
        ty: &Ty,
        span: impl Into<Option<crate::span::Span>>,
    ) -> Ty {
        let span = span.into();
        let Some((borrowed, source_ty)) = meta_repr_marker_source(ty) else {
            if let Some(source_ty) = meta_schema_marker_source(ty) {
                if contains_generic(source_ty) || contains_type_hole(source_ty) {
                    return ty.clone();
                }
                return self
                    .meta_schema_marker_sop_ty(ty)
                    .unwrap_or_else(|| self.meta_schema_ty(span, source_ty));
            }
            return ty.clone();
        };
        if contains_generic(source_ty) || contains_type_hole(source_ty) {
            return ty.clone();
        }
        self.meta_repr_marker_sop_ty(ty)
            .unwrap_or_else(|| self.meta_repr_ty(span, source_ty, borrowed))
    }

    pub(super) fn meta_repr_constraint_receiver_ty(
        &mut self,
        ty: &Ty,
        span: impl Into<Option<crate::span::Span>>,
    ) -> Ty {
        if let Some((_, source_ty)) = meta_repr_marker_source(ty)
            && (contains_generic(source_ty) || contains_type_hole(source_ty))
        {
            return ty.clone();
        }
        if meta_repr_marker_source(ty).is_some()
            && let Some(sop_ty) = self.meta_repr_marker_sop_ty(ty)
        {
            return sop_ty;
        }
        if let Some(source_ty) = meta_schema_marker_source(ty)
            && (contains_generic(source_ty) || contains_type_hole(source_ty))
        {
            return ty.clone();
        }
        if meta_schema_marker_source(ty).is_some()
            && let Some(sop_ty) = self.meta_schema_marker_sop_ty(ty)
        {
            return sop_ty;
        }
        self.meta_repr_storage_ty(ty, span)
    }

    pub(super) fn meta_repr_symbolic_constraint_receiver_ty(
        &mut self,
        ty: &Ty,
        span: impl Into<Option<crate::span::Span>>,
    ) -> Ty {
        let span = span.into();
        if let Some((borrowed, source_ty)) = meta_repr_marker_source(ty)
            && contains_generic(source_ty)
            && !contains_type_hole(source_ty)
        {
            let root = (!borrowed).then(|| source_ty.clone());
            let mut expanding = HashSet::new();
            return self
                .symbolic_meta_repr_ty_inner_rec(
                    span,
                    source_ty,
                    borrowed,
                    root.as_ref(),
                    &mut expanding,
                )
                .unwrap_or_else(|| ty.clone());
        }
        if let Some(source_ty) = meta_schema_marker_source(ty)
            && contains_generic(source_ty)
            && !contains_type_hole(source_ty)
        {
            let root = source_ty.clone();
            let mut expanding = HashSet::new();
            return self
                .symbolic_meta_schema_ty_inner_rec(span, source_ty, Some(&root), &mut expanding)
                .unwrap_or_else(|| ty.clone());
        }
        self.meta_repr_constraint_receiver_ty(ty, span)
    }

    pub(super) fn symbolic_meta_repr_ty_inner_rec(
        &mut self,
        span: Option<crate::span::Span>,
        source_ty: &Ty,
        borrowed: bool,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Option<Ty> {
        if contains_type_hole(source_ty) || matches!(source_ty, Ty::Generic(_)) {
            return Some(std_meta_repr_marker_ty(borrowed, source_ty.clone()));
        }
        match source_ty {
            Ty::Array { len, elem } => {
                self.check_meta_array_budget(span, source_ty, *len, elem, false)?;
                Some(if borrowed {
                    meta_ref_array_repr_ty(*len, elem)
                } else {
                    self.symbolic_meta_array_repr_ty_inner(span, *len, elem, root, expanding)?
                })
            }
            Ty::Named { name, args } => {
                if let Some(marker_borrowed) = meta_repr_marker_name(name) {
                    if args.len() != 1 {
                        return None;
                    }
                    return self.symbolic_meta_repr_ty_inner_rec(
                        span,
                        &args[0],
                        marker_borrowed,
                        root,
                        expanding,
                    );
                }
                let instance_ty = Ty::Named {
                    name: name.clone(),
                    args: args.clone(),
                };
                if !contains_generic(&instance_ty)
                    && !borrowed
                    && self.is_owned_meta_policy_leaf(&instance_ty, root)
                {
                    return Some(self.meta_repr_policy_leaf_ty(&instance_ty, root));
                }
                if !expanding.insert(instance_ty.clone()) {
                    return None;
                }
                if let Some(fields) = self.symbolic_struct_fields(name, args) {
                    let mut field_tys = Vec::new();
                    for (_, ty) in fields {
                        let Some(field_ty) =
                            self.symbolic_meta_repr_field_ty(span, &ty, borrowed, root, expanding)
                        else {
                            expanding.remove(&instance_ty);
                            return None;
                        };
                        field_tys.push(field_ty);
                    }
                    expanding.remove(&instance_ty);
                    return Some(meta_product_ty(
                        field_tys,
                        if borrowed { "FieldRef" } else { "Field" },
                    ));
                }
                if let Some(variants) = self.symbolic_enum_variants(name, args) {
                    let mut variant_tys = Vec::new();
                    for payload in variants {
                        let mut payload_tys = Vec::new();
                        for payload_ty in payload {
                            let Some(field_ty) = self.symbolic_meta_repr_field_ty(
                                span,
                                &payload_ty,
                                borrowed,
                                root,
                                expanding,
                            ) else {
                                expanding.remove(&instance_ty);
                                return None;
                            };
                            payload_tys.push(field_ty);
                        }
                        variant_tys.push(payload_tys);
                    }
                    expanding.remove(&instance_ty);
                    return Some(meta_sum_ty(variant_tys, borrowed));
                }
                expanding.remove(&instance_ty);
                Some(std_meta_repr_marker_ty(borrowed, source_ty.clone()))
            }
            _ => Some(std_meta_repr_marker_ty(borrowed, source_ty.clone())),
        }
    }

    pub(super) fn symbolic_meta_repr_field_ty(
        &mut self,
        span: Option<crate::span::Span>,
        ty: &Ty,
        borrowed: bool,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Option<Ty> {
        if borrowed {
            return Some(meta_repr_borrowed_array_leaf_ty(ty));
        }
        match ty {
            Ty::Array { .. } | Ty::Named { .. } => {
                self.symbolic_meta_repr_ty_inner_rec(span, ty, false, root, expanding)
            }
            other => Some(other.clone()),
        }
    }

    pub(super) fn symbolic_meta_array_repr_ty_inner(
        &mut self,
        span: Option<crate::span::Span>,
        len: usize,
        elem: &Ty,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Option<Ty> {
        if len == 0 {
            return Some(meta_named("ArrayNil", Vec::new()));
        }
        if len <= crate::types::META_ARRAY_CHUNK_SIZE {
            let elem_ty = self.symbolic_meta_repr_field_ty(span, elem, false, root, expanding)?;
            return Some(meta_named(&format!("ArrayChunk{len}"), vec![elem_ty]));
        }
        let split = crate::types::meta_array_split_len(len);
        Some(meta_named(
            "ArrayCat",
            vec![
                self.symbolic_meta_array_repr_ty_inner(span, split, elem, root, expanding)?,
                self.symbolic_meta_array_repr_ty_inner(span, len - split, elem, root, expanding)?,
            ],
        ))
    }

    pub(super) fn symbolic_meta_schema_ty_inner_rec(
        &mut self,
        span: Option<crate::span::Span>,
        source_ty: &Ty,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Option<Ty> {
        if contains_type_hole(source_ty) || matches!(source_ty, Ty::Generic(_)) {
            return Some(std_meta_schema_marker_ty(source_ty.clone()));
        }
        match source_ty {
            Ty::Array { len, elem } => {
                self.symbolic_meta_schema_array_ty_inner(span, *len, elem, root, expanding)
            }
            Ty::Named { name, args } => {
                if meta_schema_marker_name(name) {
                    if args.len() != 1 {
                        return None;
                    }
                    return self.symbolic_meta_schema_ty_inner_rec(span, &args[0], root, expanding);
                }
                let instance_ty = Ty::Named {
                    name: name.clone(),
                    args: args.clone(),
                };
                if !expanding.insert(instance_ty.clone()) {
                    return None;
                }
                if let Some(fields) = self.symbolic_struct_fields(name, args) {
                    let mut field_tys = Vec::new();
                    for (_, ty) in fields {
                        let Some(repr_ty) =
                            self.symbolic_meta_repr_field_ty(span, &ty, false, root, expanding)
                        else {
                            expanding.remove(&instance_ty);
                            return None;
                        };
                        field_tys.push((ty, repr_ty));
                    }
                    expanding.remove(&instance_ty);
                    return Some(meta_schema_product_ty(field_tys));
                }
                if let Some(variants) = self.symbolic_enum_variants(name, args) {
                    let mut variant_tys = Vec::new();
                    for payload in variants {
                        let mut payload_tys = Vec::new();
                        for payload_ty in payload {
                            let Some(repr_ty) = self.symbolic_meta_repr_field_ty(
                                span,
                                &payload_ty,
                                false,
                                root,
                                expanding,
                            ) else {
                                expanding.remove(&instance_ty);
                                return None;
                            };
                            payload_tys.push((payload_ty, repr_ty));
                        }
                        variant_tys.push(payload_tys);
                    }
                    expanding.remove(&instance_ty);
                    return Some(meta_schema_sum_ty(variant_tys));
                }
                expanding.remove(&instance_ty);
                Some(std_meta_schema_marker_ty(source_ty.clone()))
            }
            _ => Some(std_meta_schema_marker_ty(source_ty.clone())),
        }
    }

    pub(super) fn symbolic_meta_schema_array_ty_inner(
        &mut self,
        span: Option<crate::span::Span>,
        len: usize,
        elem: &Ty,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Option<Ty> {
        let source_ty = Ty::Array {
            len,
            elem: Box::new(elem.clone()),
        };
        self.check_meta_schema_array_budget(span, &source_ty, len, elem, false)?;
        if len == 0 {
            return Some(meta_named("ArrayNil", Vec::new()));
        }
        if len <= crate::types::META_ARRAY_CHUNK_SIZE {
            let repr_ty = self.symbolic_meta_repr_field_ty(span, elem, false, root, expanding)?;
            let elem_ty = meta_named("ElementSchema", vec![elem.clone(), repr_ty]);
            return Some(meta_named(&format!("ArrayChunk{len}"), vec![elem_ty]));
        }
        let split = crate::types::meta_array_split_len(len);
        Some(meta_named(
            "ArrayCat",
            vec![
                self.symbolic_meta_schema_array_ty_inner(span, split, elem, root, expanding)?,
                self.symbolic_meta_schema_array_ty_inner(span, len - split, elem, root, expanding)?,
            ],
        ))
    }

    pub(super) fn symbolic_struct_fields(
        &mut self,
        name: &str,
        args: &[Ty],
    ) -> Option<Vec<(String, Ty)>> {
        let instance_name = enum_instance_name(name, args);
        if let Some(fields) = self.ctx.structs.get(&instance_name).cloned() {
            return Some(fields);
        }
        let template = self.ctx.struct_templates.get(name).cloned()?;
        if args.len() != template.generics.len() {
            return None;
        }
        let subst = template
            .generics
            .iter()
            .map(|generic| generic.name.clone())
            .zip(args.iter().cloned())
            .collect::<HashMap<_, _>>();
        Some(
            template
                .fields
                .iter()
                .map(|field| {
                    (
                        field.name.name.clone(),
                        self.lower_type_with_subst_no_normalize(&field.ty, &subst),
                    )
                })
                .collect(),
        )
    }

    pub(super) fn symbolic_enum_variants(
        &mut self,
        name: &str,
        args: &[Ty],
    ) -> Option<Vec<Vec<Ty>>> {
        let instance_name = enum_instance_name(name, args);
        if let Some(enm) = self.ctx.checked_enums.get(&instance_name).cloned() {
            return Some(
                enm.variants
                    .into_iter()
                    .map(|variant| variant.payload)
                    .collect(),
            );
        }
        let template = self.ctx.enum_templates.get(name).cloned()?;
        if args.len() != template.generics.len() {
            return None;
        }
        let subst = template
            .generics
            .iter()
            .map(|generic| generic.name.clone())
            .zip(args.iter().cloned())
            .collect::<HashMap<_, _>>();
        Some(
            template
                .variants
                .iter()
                .map(|variant| {
                    variant
                        .payload
                        .iter()
                        .map(|payload| self.lower_type_with_subst_no_normalize(payload, &subst))
                        .collect()
                })
                .collect(),
        )
    }

    pub(super) fn meta_repr_storage_ty_inner(
        &mut self,
        ty: &Ty,
        span: Option<crate::span::Span>,
        in_meta_sop: bool,
    ) -> Ty {
        match ty {
            Ty::Named { name, args } => {
                if meta_repr_marker_name(name).is_some() {
                    return self
                        .meta_repr_marker_storage_ty(ty, span)
                        .unwrap_or(Ty::Unknown);
                }
                if meta_schema_marker_name(name) {
                    return self
                        .meta_schema_marker_storage_ty(ty, span)
                        .unwrap_or(Ty::Unknown);
                }
                if name == "Type" && args.len() == 1 {
                    return meta_named(
                        "Type",
                        vec![self.meta_repr_storage_ty_inner(&args[0], span, false)],
                    );
                }
                let original = Ty::Named {
                    name: name.clone(),
                    args: args.clone(),
                };
                if self.type_implements_meta_policy_marker(&original) {
                    if in_meta_sop {
                        return original;
                    }
                    return self.meta_repr_policy_leaf_ty(&original, None);
                }
                let in_meta_sop = in_meta_sop || std_id::is_std_meta_sop_node_name(name);
                map_ty_children(ty, |arg| {
                    self.meta_repr_storage_ty_inner(arg, span, in_meta_sop)
                })
            }
            Ty::GeneratedFuture { .. } => ty.clone(),
            _ => map_ty_children(ty, |arg| {
                self.meta_repr_storage_ty_inner(arg, span, in_meta_sop)
            }),
        }
    }

    pub(super) fn normalize_meta_repr_markers_inner(
        &mut self,
        ty: &Ty,
        span: Option<crate::span::Span>,
        emit_diagnostics: bool,
        in_meta_sop: bool,
    ) -> Ty {
        match ty {
            Ty::Named { name, args } => {
                if meta_repr_marker_name(name).is_some() || meta_schema_marker_name(name) {
                    if args.len() != 1 {
                        return Ty::Unknown;
                    }
                    return Ty::Named {
                        name: name.clone(),
                        args: args
                            .iter()
                            .map(|arg| {
                                self.normalize_meta_repr_markers_inner(
                                    arg,
                                    span,
                                    emit_diagnostics,
                                    in_meta_sop,
                                )
                            })
                            .collect(),
                    };
                }
                if name == "Type" && args.len() == 1 {
                    return meta_named(
                        "Type",
                        vec![self.meta_repr_storage_ty_inner(&args[0], span, false)],
                    );
                }
                let original = Ty::Named {
                    name: name.clone(),
                    args: args.clone(),
                };
                if self.type_implements_meta_policy_marker(&original) {
                    if in_meta_sop {
                        return original;
                    }
                    return self.meta_repr_policy_leaf_ty(&original, None);
                }
                let in_meta_sop = in_meta_sop || std_id::is_std_meta_sop_node_name(name);
                map_ty_children(ty, |arg| {
                    self.normalize_meta_repr_markers_inner(arg, span, emit_diagnostics, in_meta_sop)
                })
            }
            _ => map_ty_children(ty, |arg| {
                self.normalize_meta_repr_markers_inner(arg, span, emit_diagnostics, in_meta_sop)
            }),
        }
    }

    pub(super) fn substitute_ty_normalized(
        &mut self,
        ty: &Ty,
        subst: &HashMap<String, Ty>,
        span: impl Into<Option<crate::span::Span>>,
    ) -> Ty {
        self.substitute_ty_normalized_inner(ty, subst, span.into(), true, false)
            .ty
    }

    pub(super) fn substitute_ty_normalized_silent(
        &mut self,
        ty: &Ty,
        subst: &HashMap<String, Ty>,
    ) -> Ty {
        self.substitute_ty_normalized_inner(ty, subst, None, false, false)
            .ty
    }

    pub(super) fn substitute_ty_normalized_list(
        &mut self,
        tys: &[Ty],
        subst: &HashMap<String, Ty>,
        span: Option<crate::span::Span>,
        emit_diagnostics: bool,
        in_meta_sop: bool,
    ) -> (Vec<Ty>, bool) {
        let mut has_replacement = false;
        let tys = tys
            .iter()
            .map(|ty| {
                let substituted = self.substitute_ty_normalized_inner(
                    ty,
                    subst,
                    span,
                    emit_diagnostics,
                    in_meta_sop,
                );
                has_replacement |= substituted.from_replacement;
                substituted.ty
            })
            .collect::<Vec<_>>();
        (tys, has_replacement)
    }

    pub(super) fn substitute_ty_normalized_inner(
        &mut self,
        ty: &Ty,
        subst: &HashMap<String, Ty>,
        span: Option<crate::span::Span>,
        emit_diagnostics: bool,
        in_meta_sop: bool,
    ) -> SubstitutedTy {
        match ty {
            Ty::Generic(name) => SubstitutedTy {
                ty: subst
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| Ty::Generic(name.clone())),
                from_replacement: subst.contains_key(name),
            },
            Ty::Named { name, args } => {
                if meta_repr_marker_name(name).is_some() || meta_schema_marker_name(name) {
                    let args = args
                        .iter()
                        .map(|arg| {
                            self.substitute_ty_normalized_inner(
                                arg,
                                subst,
                                span,
                                emit_diagnostics,
                                in_meta_sop,
                            )
                            .ty
                        })
                        .collect::<Vec<_>>();
                    if args.len() != 1 {
                        return SubstitutedTy {
                            ty: Ty::Unknown,
                            from_replacement: false,
                        };
                    }
                    return SubstitutedTy {
                        ty: Ty::Named {
                            name: name.clone(),
                            args,
                        },
                        from_replacement: false,
                    };
                }
                let child_in_meta_sop = in_meta_sop || std_id::is_std_meta_sop_node_name(name);
                let (args, has_replacement_arg) = self.substitute_ty_normalized_list(
                    args,
                    subst,
                    span,
                    emit_diagnostics,
                    child_in_meta_sop,
                );
                if name == "Type" && args.len() == 1 {
                    return SubstitutedTy {
                        ty: meta_named(
                            "Type",
                            vec![self.meta_repr_storage_ty_inner(&args[0], span, false)],
                        ),
                        from_replacement: has_replacement_arg,
                    };
                }
                let original = Ty::Named {
                    name: name.clone(),
                    args,
                };
                if !has_replacement_arg && self.type_implements_meta_policy_marker(&original) {
                    let ty = if in_meta_sop {
                        original
                    } else {
                        self.meta_repr_policy_leaf_ty(&original, None)
                    };
                    return SubstitutedTy {
                        ty,
                        from_replacement: false,
                    };
                }
                SubstitutedTy {
                    ty: original,
                    from_replacement: has_replacement_arg,
                }
            }
            Ty::Pointer {
                nullable,
                mutability,
                inner,
            } => {
                let inner = self.substitute_ty_normalized_inner(
                    inner,
                    subst,
                    span,
                    emit_diagnostics,
                    in_meta_sop,
                );
                SubstitutedTy {
                    ty: Ty::Pointer {
                        nullable: *nullable,
                        mutability: *mutability,
                        inner: Box::new(inner.ty),
                    },
                    from_replacement: inner.from_replacement,
                }
            }
            Ty::Array { len, elem } => {
                let elem = self.substitute_ty_normalized_inner(
                    elem,
                    subst,
                    span,
                    emit_diagnostics,
                    in_meta_sop,
                );
                SubstitutedTy {
                    ty: Ty::Array {
                        len: *len,
                        elem: Box::new(elem.ty),
                    },
                    from_replacement: elem.from_replacement,
                }
            }
            Ty::Slice { mutability, elem } => {
                let elem = self.substitute_ty_normalized_inner(
                    elem,
                    subst,
                    span,
                    emit_diagnostics,
                    in_meta_sop,
                );
                SubstitutedTy {
                    ty: Ty::Slice {
                        mutability: *mutability,
                        elem: Box::new(elem.ty),
                    },
                    from_replacement: elem.from_replacement,
                }
            }
            Ty::GeneratedFuture {
                name,
                output,
                cancel_safe,
                abortable,
                affine_state,
                state,
            } => {
                let output = self.substitute_ty_normalized_inner(
                    output,
                    subst,
                    span,
                    emit_diagnostics,
                    in_meta_sop,
                );
                let mut any_replacement = output.from_replacement;
                let state = state
                    .iter()
                    .map(|(name, ty)| {
                        let ty = self.substitute_ty_normalized_inner(
                            ty,
                            subst,
                            span,
                            emit_diagnostics,
                            in_meta_sop,
                        );
                        any_replacement |= ty.from_replacement;
                        (name.clone(), ty.ty)
                    })
                    .collect();
                SubstitutedTy {
                    ty: Ty::GeneratedFuture {
                        name: name.clone(),
                        output: Box::new(output.ty),
                        cancel_safe: *cancel_safe,
                        abortable: *abortable,
                        affine_state: *affine_state,
                        state,
                    },
                    from_replacement: any_replacement,
                }
            }
            Ty::DynamicInterface { def_id, name, args } => {
                let (args, has_replacement_arg) = self.substitute_ty_normalized_list(
                    args,
                    subst,
                    span,
                    emit_diagnostics,
                    in_meta_sop,
                );
                SubstitutedTy {
                    ty: Ty::DynamicInterface {
                        def_id: *def_id,
                        name: name.clone(),
                        args,
                    },
                    from_replacement: has_replacement_arg,
                }
            }
            Ty::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => {
                let ret = self.substitute_ty_normalized_inner(
                    ret,
                    subst,
                    span,
                    emit_diagnostics,
                    in_meta_sop,
                );
                let (params, has_replacement_params) = self.substitute_ty_normalized_list(
                    params,
                    subst,
                    span,
                    emit_diagnostics,
                    in_meta_sop,
                );
                SubstitutedTy {
                    ty: Ty::Function {
                        is_unsafe: *is_unsafe,
                        abi: abi.clone(),
                        ret: Box::new(ret.ty),
                        params,
                    },
                    from_replacement: ret.from_replacement || has_replacement_params,
                }
            }
            Ty::Closure {
                ret,
                params,
                constraints,
            } => {
                let ret = self.substitute_ty_normalized_inner(
                    ret,
                    subst,
                    span,
                    emit_diagnostics,
                    in_meta_sop,
                );
                let (params, has_replacement_params) = self.substitute_ty_normalized_list(
                    params,
                    subst,
                    span,
                    emit_diagnostics,
                    in_meta_sop,
                );
                SubstitutedTy {
                    ty: Ty::Closure {
                        ret: Box::new(ret.ty),
                        params,
                        constraints: substitute_constraint_bounds(constraints, subst),
                    },
                    from_replacement: ret.from_replacement || has_replacement_params,
                }
            }
            Ty::ClosureInstance {
                id,
                ret,
                params,
                captures,
            } => {
                let ret = self.substitute_ty_normalized_inner(
                    ret,
                    subst,
                    span,
                    emit_diagnostics,
                    in_meta_sop,
                );
                let (params, has_replacement_params) = self.substitute_ty_normalized_list(
                    params,
                    subst,
                    span,
                    emit_diagnostics,
                    in_meta_sop,
                );
                let (captures, has_replacement_captures) = self.substitute_ty_normalized_list(
                    captures,
                    subst,
                    span,
                    emit_diagnostics,
                    in_meta_sop,
                );
                SubstitutedTy {
                    ty: Ty::ClosureInstance {
                        id: *id,
                        ret: Box::new(ret.ty),
                        params,
                        captures,
                    },
                    from_replacement: ret.from_replacement
                        || has_replacement_params
                        || has_replacement_captures,
                }
            }
            _ => SubstitutedTy {
                ty: ty.clone(),
                from_replacement: false,
            },
        }
    }

    pub(super) fn inference_arg_expected(
        &mut self,
        param_ty: &Ty,
        subst: &HashMap<String, Ty>,
        expected_hints: &HashMap<String, Ty>,
    ) -> (Ty, Option<Ty>) {
        let expected_arg = self.substitute_ty_normalized_silent(param_ty, subst);
        if !contains_generic(&expected_arg) {
            return (expected_arg.clone(), Some(expected_arg));
        }

        let mut hinted_subst = expected_hints.clone();
        for (name, ty) in subst {
            hinted_subst.insert(name.clone(), ty.clone());
        }
        let hinted_arg = self.substitute_ty_normalized_silent(param_ty, &hinted_subst);
        let expected_for_arg = if contains_generic(&hinted_arg) {
            None
        } else {
            Some(hinted_arg)
        };
        (expected_arg, expected_for_arg)
    }

    pub(super) fn closure_inference_expected(
        &mut self,
        param_ty: &Ty,
        subst: &HashMap<String, Ty>,
        expected_hints: &HashMap<String, Ty>,
    ) -> Option<Ty> {
        let hinted = expected_hints
            .iter()
            .chain(subst.iter())
            .map(|(name, ty)| (name.clone(), ty.clone()))
            .collect::<HashMap<_, _>>();
        let candidate = self.substitute_ty_normalized_silent(param_ty, &hinted);
        let mut inference_holes = HashMap::new();
        match candidate {
            Ty::Closure {
                ret,
                params,
                constraints,
            } => Some(Ty::Closure {
                ret: Box::new(self.partial_inference_ty(&ret, &mut inference_holes)),
                params: params
                    .iter()
                    .map(|param| self.partial_inference_ty(param, &mut inference_holes))
                    .collect(),
                constraints: self
                    .partial_inference_constraint_bounds(&constraints, &mut inference_holes),
            }),
            Ty::ClosureInstance {
                id,
                ret,
                params,
                captures,
            } => Some(Ty::ClosureInstance {
                id,
                ret: Box::new(self.partial_inference_ty(&ret, &mut inference_holes)),
                params: params
                    .iter()
                    .map(|param| self.partial_inference_ty(param, &mut inference_holes))
                    .collect(),
                captures: captures
                    .iter()
                    .map(|capture| self.partial_inference_ty(capture, &mut inference_holes))
                    .collect(),
            }),
            Ty::Function {
                is_unsafe: false,
                abi: None,
                ret,
                params,
            } => Some(Ty::Function {
                is_unsafe: false,
                abi: None,
                ret: Box::new(self.partial_inference_ty(&ret, &mut inference_holes)),
                params: params
                    .iter()
                    .map(|param| self.partial_inference_ty(param, &mut inference_holes))
                    .collect(),
            }),
            _ => None,
        }
    }

    pub(super) fn partial_inference_ty(&mut self, ty: &Ty, holes: &mut HashMap<String, Ty>) -> Ty {
        match ty {
            Ty::Generic(name) => {
                if let Some(hole) = holes.get(name) {
                    return hole.clone();
                }
                let hole = Ty::Hole(self.next_type_hole_id);
                self.next_type_hole_id += 1;
                holes.insert(name.clone(), hole.clone());
                hole
            }
            Ty::Pointer {
                nullable,
                mutability,
                inner,
            } => Ty::Pointer {
                nullable: *nullable,
                mutability: *mutability,
                inner: Box::new(self.partial_inference_ty(inner, holes)),
            },
            Ty::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(self.partial_inference_ty(elem, holes)),
            },
            Ty::Slice { mutability, elem } => Ty::Slice {
                mutability: *mutability,
                elem: Box::new(self.partial_inference_ty(elem, holes)),
            },
            Ty::Named { name, args } => Ty::Named {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.partial_inference_ty(arg, holes))
                    .collect(),
            },
            Ty::DynamicInterface { def_id, name, args } => Ty::DynamicInterface {
                def_id: *def_id,
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.partial_inference_ty(arg, holes))
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
                ret: Box::new(self.partial_inference_ty(ret, holes)),
                params: params
                    .iter()
                    .map(|param| self.partial_inference_ty(param, holes))
                    .collect(),
            },
            Ty::Closure {
                ret,
                params,
                constraints,
            } => Ty::Closure {
                ret: Box::new(self.partial_inference_ty(ret, holes)),
                params: params
                    .iter()
                    .map(|param| self.partial_inference_ty(param, holes))
                    .collect(),
                constraints: self.partial_inference_constraint_bounds(constraints, holes),
            },
            Ty::ClosureInstance {
                id,
                ret,
                params,
                captures,
            } => Ty::ClosureInstance {
                id: *id,
                ret: Box::new(self.partial_inference_ty(ret, holes)),
                params: params
                    .iter()
                    .map(|param| self.partial_inference_ty(param, holes))
                    .collect(),
                captures: captures
                    .iter()
                    .map(|capture| self.partial_inference_ty(capture, holes))
                    .collect(),
            },
            other => other.clone(),
        }
    }

    pub(super) fn partial_inference_constraint_bounds(
        &mut self,
        bounds: &ConstraintBounds,
        holes: &mut HashMap<String, Ty>,
    ) -> ConstraintBounds {
        ConstraintBounds {
            positive: bounds
                .positive
                .iter()
                .map(|entry| self.partial_inference_constraint_ref(entry, holes))
                .collect(),
            negative: bounds
                .negative
                .iter()
                .map(|entry| self.partial_inference_constraint_ref(entry, holes))
                .collect(),
        }
    }

    pub(super) fn partial_inference_constraint_ref(
        &mut self,
        entry: &ConstraintRef,
        holes: &mut HashMap<String, Ty>,
    ) -> ConstraintRef {
        ConstraintRef {
            def_id: entry.def_id,
            name: entry.name.clone(),
            args: entry
                .args
                .iter()
                .map(|arg| self.partial_inference_ty(arg, holes))
                .collect(),
        }
    }

    pub(super) fn meta_repr_ty(
        &mut self,
        span: impl Into<Option<crate::span::Span>>,
        source_ty: &Ty,
        borrowed: bool,
    ) -> Ty {
        self.meta_repr_ty_inner(span.into(), source_ty, borrowed, true)
            .unwrap_or(Ty::Unknown)
    }

    pub(super) fn try_meta_repr_ty(&mut self, source_ty: &Ty, borrowed: bool) -> Option<Ty> {
        self.meta_repr_ty_inner(None, source_ty, borrowed, false)
    }

    pub(super) fn meta_repr_ty_inner(
        &mut self,
        span: Option<crate::span::Span>,
        source_ty: &Ty,
        borrowed: bool,
        emit_diagnostics: bool,
    ) -> Option<Ty> {
        let root = (!borrowed).then(|| source_ty.clone());
        let mut expanding = HashSet::new();
        self.meta_repr_ty_inner_rec(
            span,
            source_ty,
            borrowed,
            emit_diagnostics,
            root.as_ref(),
            &mut expanding,
        )
    }

    pub(super) fn meta_repr_ty_inner_rec(
        &mut self,
        span: Option<crate::span::Span>,
        source_ty: &Ty,
        borrowed: bool,
        emit_diagnostics: bool,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Option<Ty> {
        if contains_generic(source_ty) || contains_type_hole(source_ty) {
            return Some(std_meta_repr_marker_ty(borrowed, source_ty.clone()));
        }
        match source_ty {
            Ty::Array { len, elem } => {
                self.check_meta_array_budget(span, source_ty, *len, elem, emit_diagnostics)?;
                Some(if borrowed {
                    meta_ref_array_repr_ty(*len, elem)
                } else {
                    self.meta_array_repr_ty_inner(
                        span,
                        *len,
                        elem,
                        false,
                        emit_diagnostics,
                        root,
                        expanding,
                    )?
                })
            }
            Ty::Named { name, args } => {
                if let Some(marker_borrowed) = meta_repr_marker_name(name) {
                    if args.len() != 1 {
                        return None;
                    }
                    return self.meta_repr_ty_inner_rec(
                        span,
                        &args[0],
                        marker_borrowed,
                        emit_diagnostics,
                        root,
                        expanding,
                    );
                }
                let instance_ty = Ty::Named {
                    name: name.clone(),
                    args: args.clone(),
                };
                if !borrowed && self.is_owned_meta_policy_leaf(&instance_ty, root) {
                    return Some(self.meta_repr_policy_leaf_ty(&instance_ty, root));
                }
                if !expanding.insert(instance_ty.clone()) {
                    if emit_diagnostics {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            format!(
                                "meta structural representation is recursive through `{source_ty}`"
                            ),
                        ));
                    }
                    return None;
                }
                let instance_name = enum_instance_name(name, args);
                self.deferred_meta_repr_roots.push(instance_ty.clone());
                self.ensure_struct_instance(&instance_ty);
                if let Some(fields) = self.ctx.structs.get(&instance_name).cloned() {
                    let mut field_tys = Vec::new();
                    for (_, ty) in fields {
                        let Some(field_ty) = self.meta_repr_field_ty(
                            span,
                            &ty,
                            borrowed,
                            emit_diagnostics,
                            root,
                            expanding,
                        ) else {
                            self.deferred_meta_repr_roots.pop();
                            expanding.remove(&instance_ty);
                            return None;
                        };
                        field_tys.push(field_ty);
                    }
                    self.deferred_meta_repr_roots.pop();
                    expanding.remove(&instance_ty);
                    return Some(meta_product_ty(
                        field_tys,
                        if borrowed { "FieldRef" } else { "Field" },
                    ));
                }
                self.ensure_enum_instance(&instance_ty);
                if let Some(enm) = self.ctx.checked_enums.get(&instance_name).cloned() {
                    let mut variants = Vec::new();
                    for variant in enm.variants {
                        let mut payloads = Vec::new();
                        for payload in variant.payload {
                            let Some(payload_ty) = self.meta_repr_field_ty(
                                span,
                                &payload,
                                borrowed,
                                emit_diagnostics,
                                root,
                                expanding,
                            ) else {
                                self.deferred_meta_repr_roots.pop();
                                expanding.remove(&instance_ty);
                                return None;
                            };
                            payloads.push(payload_ty);
                        }
                        variants.push(payloads);
                    }
                    self.deferred_meta_repr_roots.pop();
                    expanding.remove(&instance_ty);
                    return Some(meta_sum_ty(variants, borrowed));
                }
                self.deferred_meta_repr_roots.pop();
                expanding.remove(&instance_ty);
                if emit_diagnostics {
                    self.push_meta_unsupported_repr(span, source_ty);
                }
                None
            }
            Ty::ClosureInstance { captures, .. } => {
                let mut capture_tys = Vec::new();
                for ty in captures.iter().filter(|ty| !ty.is_erased_value()) {
                    capture_tys.push(self.meta_repr_field_ty(
                        span,
                        ty,
                        borrowed,
                        emit_diagnostics,
                        root,
                        expanding,
                    )?);
                }
                Some(meta_product_ty(
                    capture_tys,
                    if borrowed { "FieldRef" } else { "Field" },
                ))
            }
            Ty::Closure { .. } => {
                if emit_diagnostics {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!(
                            "meta structural representation requires a concrete closure value, got erased closure `{source_ty}`"
                        ),
                    ));
                }
                None
            }
            _ => {
                if emit_diagnostics {
                    self.push_meta_unsupported_repr(span, source_ty);
                }
                None
            }
        }
    }

    pub(super) fn meta_repr_field_ty(
        &mut self,
        span: Option<crate::span::Span>,
        ty: &Ty,
        borrowed: bool,
        emit_diagnostics: bool,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Option<Ty> {
        if borrowed {
            return Some(meta_repr_borrowed_array_leaf_ty(ty));
        }
        self.meta_repr_owned_leaf_ty_inner(span, ty, emit_diagnostics, root, expanding)
    }

    pub(super) fn meta_repr_policy_leaf_ty(&mut self, ty: &Ty, root: Option<&Ty>) -> Ty {
        match ty {
            Ty::Named { name, args } => Ty::Named {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.meta_repr_policy_leaf_arg_ty(arg, root, false))
                    .collect(),
            },
            _ => ty.clone(),
        }
    }

    pub(super) fn meta_repr_policy_leaf_arg_ty(
        &mut self,
        ty: &Ty,
        root: Option<&Ty>,
        in_meta_sop: bool,
    ) -> Ty {
        match ty {
            Ty::Named { name, args } => {
                if meta_repr_marker_name(name).is_some() || meta_schema_marker_name(name) {
                    if args.len() != 1 {
                        return Ty::Unknown;
                    }
                    return Ty::Named {
                        name: name.clone(),
                        args: args.clone(),
                    };
                }
                let original = Ty::Named {
                    name: name.clone(),
                    args: args.clone(),
                };
                if in_meta_sop && self.type_implements_meta_policy_marker(&original) {
                    return original;
                }
                let in_meta_sop = in_meta_sop || std_id::is_std_meta_sop_node_name(name);
                Ty::Named {
                    name: name.clone(),
                    args: args
                        .iter()
                        .map(|arg| self.meta_repr_policy_leaf_arg_ty(arg, root, in_meta_sop))
                        .collect(),
                }
            }
            _ => map_ty_children(ty, |arg| {
                self.meta_repr_policy_leaf_arg_ty(arg, root, in_meta_sop)
            }),
        }
    }

    pub(super) fn meta_repr_owned_leaf_ty_inner(
        &mut self,
        span: Option<crate::span::Span>,
        ty: &Ty,
        emit_diagnostics: bool,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Option<Ty> {
        if self.is_owned_meta_policy_leaf(ty, root) {
            return Some(self.meta_repr_policy_leaf_ty(ty, root));
        }
        match ty {
            Ty::Array { len, elem } => {
                self.check_meta_array_budget(span, ty, *len, elem, emit_diagnostics)?;
                self.meta_array_repr_ty_inner(
                    span,
                    *len,
                    elem,
                    false,
                    emit_diagnostics,
                    root,
                    expanding,
                )
            }
            Ty::Named { .. } | Ty::ClosureInstance { .. } => {
                self.meta_repr_ty_inner_rec(span, ty, false, emit_diagnostics, root, expanding)
            }
            other => Some(other.clone()),
        }
    }

    pub(super) fn meta_schema_ty(
        &mut self,
        span: impl Into<Option<crate::span::Span>>,
        source_ty: &Ty,
    ) -> Ty {
        self.meta_schema_ty_inner(span.into(), source_ty, true)
            .unwrap_or(Ty::Unknown)
    }

    pub(super) fn try_meta_schema_ty(&mut self, source_ty: &Ty) -> Option<Ty> {
        self.meta_schema_ty_inner(None, source_ty, false)
    }

    pub(super) fn meta_schema_ty_inner(
        &mut self,
        span: Option<crate::span::Span>,
        source_ty: &Ty,
        emit_diagnostics: bool,
    ) -> Option<Ty> {
        let root = source_ty.clone();
        let mut expanding = HashSet::new();
        self.meta_schema_ty_inner_rec(
            span,
            source_ty,
            emit_diagnostics,
            Some(&root),
            &mut expanding,
        )
    }

    pub(super) fn meta_schema_ty_inner_rec(
        &mut self,
        span: Option<crate::span::Span>,
        source_ty: &Ty,
        emit_diagnostics: bool,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Option<Ty> {
        if contains_generic(source_ty) || contains_type_hole(source_ty) {
            return Some(std_meta_schema_marker_ty(source_ty.clone()));
        }
        match source_ty {
            Ty::Array { len, elem } => {
                self.meta_schema_array_ty_inner(span, *len, elem, emit_diagnostics, root, expanding)
            }
            Ty::Named { name, args } => {
                if meta_schema_marker_name(name) {
                    if args.len() != 1 {
                        return None;
                    }
                    return self.meta_schema_ty_inner_rec(
                        span,
                        &args[0],
                        emit_diagnostics,
                        root,
                        expanding,
                    );
                }
                let instance_ty = Ty::Named {
                    name: name.clone(),
                    args: args.clone(),
                };
                if !expanding.insert(instance_ty.clone()) {
                    if emit_diagnostics {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            format!("meta schema reflection is recursive through `{source_ty}`"),
                        ));
                    }
                    return None;
                }
                let instance_name = enum_instance_name(name, args);
                self.deferred_meta_repr_roots.push(instance_ty.clone());
                self.ensure_struct_instance(&instance_ty);
                if let Some(fields) = self.ctx.structs.get(&instance_name).cloned() {
                    let mut field_tys = Vec::new();
                    for (_, ty) in fields {
                        let Some(repr_ty) = self.meta_repr_field_ty(
                            span,
                            &ty,
                            false,
                            emit_diagnostics,
                            root,
                            expanding,
                        ) else {
                            self.deferred_meta_repr_roots.pop();
                            expanding.remove(&instance_ty);
                            return None;
                        };
                        field_tys.push((ty, repr_ty));
                    }
                    self.deferred_meta_repr_roots.pop();
                    expanding.remove(&instance_ty);
                    return Some(meta_schema_product_ty(field_tys));
                }
                self.ensure_enum_instance(&instance_ty);
                if let Some(enm) = self.ctx.checked_enums.get(&instance_name).cloned() {
                    let mut variants = Vec::new();
                    for variant in enm.variants {
                        let mut payloads = Vec::new();
                        for payload in variant.payload {
                            let Some(repr_ty) = self.meta_repr_field_ty(
                                span,
                                &payload,
                                false,
                                emit_diagnostics,
                                root,
                                expanding,
                            ) else {
                                self.deferred_meta_repr_roots.pop();
                                expanding.remove(&instance_ty);
                                return None;
                            };
                            payloads.push((payload, repr_ty));
                        }
                        variants.push(payloads);
                    }
                    self.deferred_meta_repr_roots.pop();
                    expanding.remove(&instance_ty);
                    return Some(meta_schema_sum_ty(variants));
                }
                self.deferred_meta_repr_roots.pop();
                expanding.remove(&instance_ty);
                if emit_diagnostics {
                    self.push_meta_unsupported_schema(span, source_ty);
                }
                None
            }
            Ty::ClosureInstance { captures, .. } => {
                let mut capture_tys = Vec::new();
                for ty in captures.iter().filter(|ty| !ty.is_erased_value()) {
                    let repr_ty = self.meta_repr_field_ty(
                        span,
                        ty,
                        false,
                        emit_diagnostics,
                        root,
                        expanding,
                    )?;
                    capture_tys.push((ty.clone(), repr_ty));
                }
                Some(meta_schema_product_ty(capture_tys))
            }
            Ty::Closure { .. } => {
                if emit_diagnostics {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!(
                            "meta schema reflection requires a concrete closure value, got erased closure `{source_ty}`"
                        ),
                    ));
                }
                None
            }
            _ => {
                if emit_diagnostics {
                    self.push_meta_unsupported_schema(span, source_ty);
                }
                None
            }
        }
    }

    pub(super) fn meta_schema_array_ty_inner(
        &mut self,
        span: Option<crate::span::Span>,
        len: usize,
        elem: &Ty,
        emit_diagnostics: bool,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Option<Ty> {
        let source_ty = Ty::Array {
            len,
            elem: Box::new(elem.clone()),
        };
        self.check_meta_schema_array_budget(span, &source_ty, len, elem, emit_diagnostics)?;
        if len == 0 {
            return Some(meta_named("ArrayNil", Vec::new()));
        }
        if len <= crate::types::META_ARRAY_CHUNK_SIZE {
            let repr_ty =
                self.meta_repr_owned_leaf_ty_inner(span, elem, emit_diagnostics, root, expanding)?;
            let elem_ty = meta_named("ElementSchema", vec![elem.clone(), repr_ty]);
            return Some(meta_named(&format!("ArrayChunk{len}"), vec![elem_ty]));
        }
        let split = crate::types::meta_array_split_len(len);
        Some(meta_named(
            "ArrayCat",
            vec![
                self.meta_schema_array_ty_inner(
                    span,
                    split,
                    elem,
                    emit_diagnostics,
                    root,
                    expanding,
                )?,
                self.meta_schema_array_ty_inner(
                    span,
                    len - split,
                    elem,
                    emit_diagnostics,
                    root,
                    expanding,
                )?,
            ],
        ))
    }

    pub(super) fn is_owned_meta_policy_leaf(&mut self, ty: &Ty, root: Option<&Ty>) -> bool {
        if contains_generic(ty) || contains_type_hole(ty) {
            return false;
        }
        let leaf_ty = self.meta_repr_policy_leaf_ty(ty, root);
        if self.type_is_affine(&leaf_ty) {
            return false;
        }
        let is_thread_local = self.type_implements_thread_local(&leaf_ty);
        if root.is_some_and(|root| ty == root) && !is_thread_local {
            return false;
        }
        matches!(ty, Ty::Named { .. })
            && (is_thread_local
                || self.type_implements_share_handle(&leaf_ty)
                || self.type_implements_message(&leaf_ty))
    }

    pub(super) fn reject_owned_meta_repr_affine_source(
        &mut self,
        span: crate::span::Span,
        source_ty: &Ty,
    ) {
        if owned_meta_repr_contains_affine(self, source_ty) {
            self.diagnostics.push(Diagnostic::new(
                span,
                owned_meta_repr_affine_message(source_ty),
            ));
        }
    }

    pub(super) fn meta_structural_repr_unsafe_struct_name(
        &mut self,
        source_ty: &Ty,
        borrowed: bool,
    ) -> Option<String> {
        meta_structural_repr_unsafe_struct_name(self, source_ty, borrowed)
    }

    pub(super) fn meta_array_repr_ty_inner(
        &mut self,
        span: Option<crate::span::Span>,
        len: usize,
        elem: &Ty,
        borrowed: bool,
        emit_diagnostics: bool,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Option<Ty> {
        if len == 0 {
            return Some(meta_named("ArrayNil", Vec::new()));
        }
        if len <= crate::types::META_ARRAY_CHUNK_SIZE {
            let elem_ty = if borrowed {
                meta_repr_borrowed_array_leaf_ty(elem)
            } else {
                self.meta_repr_owned_leaf_ty_inner(span, elem, emit_diagnostics, root, expanding)?
            };
            return Some(meta_named(&format!("ArrayChunk{len}"), vec![elem_ty]));
        }
        let split = crate::types::meta_array_split_len(len);
        Some(meta_named(
            "ArrayCat",
            vec![
                self.meta_array_repr_ty_inner(
                    span,
                    split,
                    elem,
                    borrowed,
                    emit_diagnostics,
                    root,
                    expanding,
                )?,
                self.meta_array_repr_ty_inner(
                    span,
                    len - split,
                    elem,
                    borrowed,
                    emit_diagnostics,
                    root,
                    expanding,
                )?,
            ],
        ))
    }

    pub(super) fn check_meta_array_budget(
        &mut self,
        span: Option<crate::span::Span>,
        source_ty: &Ty,
        len: usize,
        elem: &Ty,
        emit_diagnostics: bool,
    ) -> Option<()> {
        let cost = crate::types::meta_array_expansion_cost(len, elem)?;
        if cost <= META_ARRAY_EXPANSION_BUDGET {
            return Some(());
        }
        if emit_diagnostics {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "meta::Repr<{source_ty}> expands too many structural array nodes; use an explicit Message wrapper or an owned buffer type"
                ),
            ));
        }
        None
    }

    pub(super) fn check_meta_schema_array_budget(
        &mut self,
        span: Option<crate::span::Span>,
        source_ty: &Ty,
        len: usize,
        elem: &Ty,
        emit_diagnostics: bool,
    ) -> Option<()> {
        let cost = crate::types::meta_array_expansion_cost(len, elem)?;
        if cost <= META_ARRAY_EXPANSION_BUDGET {
            return Some(());
        }
        if emit_diagnostics {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "meta::Schema<{source_ty}> expands too many structural array nodes; use an explicit schema wrapper or an owned buffer type"
                ),
            ));
        }
        None
    }

    pub(super) fn push_meta_unsupported_repr(
        &mut self,
        span: impl Into<Option<crate::span::Span>>,
        source_ty: &Ty,
    ) {
        self.diagnostics.push(Diagnostic::new(
            span,
            format!(
                "meta structural representation supports visible structs, enums, and concrete closure values, got `{source_ty}`"
            ),
        ));
    }

    pub(super) fn push_meta_unsupported_schema(
        &mut self,
        span: impl Into<Option<crate::span::Span>>,
        source_ty: &Ty,
    ) {
        self.diagnostics.push(Diagnostic::new(
            span,
            format!(
                "meta schema reflection supports visible structs, enums, concrete closure values, and fixed-size arrays, got `{source_ty}`"
            ),
        ));
    }

    pub(super) fn alloc_synthetic_def(&mut self) -> DefId {
        self.ctx.alloc_synthetic_def()
    }
}

impl MetaReprSafetyEnv for TypeChecker {
    fn meta_safety_type_is_affine(&mut self, ty: &Ty) -> bool {
        self.type_is_affine(ty)
    }

    fn meta_safety_is_owned_policy_leaf(&mut self, ty: &Ty, root: Option<&Ty>) -> bool {
        self.is_owned_meta_policy_leaf(ty, root)
    }

    fn meta_safety_is_unsafe_struct_instance(&mut self, name: &str, args: &[Ty]) -> bool {
        self.is_unsafe_struct_instance(name, args)
    }

    fn meta_safety_struct_fields(
        &mut self,
        instance_ty: &Ty,
        name: &str,
        args: &[Ty],
    ) -> Option<Vec<(String, Ty)>> {
        self.ensure_struct_instance(instance_ty);
        self.ctx
            .structs
            .get(&enum_instance_name(name, args))
            .cloned()
    }

    fn meta_safety_enum_payloads(
        &mut self,
        instance_ty: &Ty,
        name: &str,
        args: &[Ty],
    ) -> Option<Vec<Vec<Ty>>> {
        self.ensure_enum_instance(instance_ty);
        self.ctx
            .checked_enums
            .get(&enum_instance_name(name, args))
            .map(|enm| {
                enm.variants
                    .iter()
                    .map(|variant| variant.payload.clone())
                    .collect()
            })
    }
}
