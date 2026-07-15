use super::*;
use crate::affine::{self, AffineStructInfo, AffineTypeEnv};

impl AffineTypeEnv for TypeChecker {
    fn is_resource_handle_leaf(&self, ty: &Ty) -> bool {
        self.type_is_resource_handle_leaf(ty)
    }

    fn named_type_is_async_future(&self, ty: &Ty) -> bool {
        std_id::std_async_future_output_arg(&self.ctx.resolved, ty).is_some()
    }

    fn opaque_return_concrete(&self, ty: &Ty) -> Option<Ty> {
        let concrete = self.lower_opaque_returns_in_ty(ty);
        (&concrete != ty).then_some(concrete)
    }

    fn generic_is_resource_only(&self, name: &str) -> bool {
        TypeChecker::generic_is_resource_only(self, name)
    }

    fn named_struct_info(&mut self, ty: &Ty) -> Option<AffineStructInfo> {
        let Ty::Named { name, args, .. } = ty else {
            return None;
        };
        let instance_name = enum_instance_name(name, args);
        if self.ctx.resource_structs.contains(&instance_name) {
            return Some(AffineStructInfo {
                is_resource: true,
                fields: Vec::new(),
            });
        }
        self.ensure_struct_instance(ty);
        if let Some(fields) = self.ctx.structs.get(&instance_name).cloned() {
            return Some(AffineStructInfo {
                is_resource: false,
                fields: fields.into_iter().map(|(_, field_ty)| field_ty).collect(),
            });
        }
        if args.iter().any(contains_generic)
            && let Some(template) = self.ctx.struct_templates.get(name).cloned()
            && args.len() == template.generics.len()
        {
            if template.is_resource {
                return Some(AffineStructInfo {
                    is_resource: true,
                    fields: Vec::new(),
                });
            }
            let subst = template
                .generics
                .iter()
                .map(|generic| generic.name.clone())
                .zip(args.iter().cloned())
                .collect::<HashMap<_, _>>();
            let fields = template
                .fields
                .iter()
                .map(|field| self.lower_type_with_subst_no_normalize(&field.ty, &subst))
                .collect();
            return Some(AffineStructInfo {
                is_resource: false,
                fields,
            });
        }
        None
    }

    fn named_enum_payloads(&mut self, ty: &Ty) -> Option<Vec<Ty>> {
        let Ty::Named { name, args, .. } = ty else {
            return None;
        };
        let instance_name = enum_instance_name(name, args);
        self.ensure_enum_instance(ty);
        if let Some(enm) = self.ctx.checked_enums.get(&instance_name).cloned() {
            return Some(
                enm.variants
                    .into_iter()
                    .flat_map(|variant| variant.payload)
                    .collect(),
            );
        }
        if args.iter().any(contains_generic)
            && let Some(template) = self.ctx.enum_templates.get(name).cloned()
            && args.len() == template.generics.len()
        {
            let subst = template
                .generics
                .iter()
                .map(|generic| generic.name.clone())
                .zip(args.iter().cloned())
                .collect::<HashMap<_, _>>();
            let mut payloads = Vec::new();
            for variant in &template.variants {
                for payload in &variant.payload {
                    payloads.push(self.lower_type_with_subst_no_normalize(payload, &subst));
                }
            }
            return Some(payloads);
        }
        None
    }
}

impl TypeChecker {
    pub(in crate::typeck) fn find_impl(
        &self,
        interface_def: DefId,
        interface_name: &str,
        args: &[Ty],
        receiver_ty: &Ty,
    ) -> Option<&ImplSig> {
        let _ = interface_name;
        CapabilityTable::new(&self.ctx).find_impl(interface_def, args, receiver_ty)
    }

    pub(in crate::typeck) fn type_implements_capability_ref(
        &mut self,
        capability: &ConstraintRef,
        receiver_ty: &Ty,
    ) -> bool {
        self.type_implements_capability_by_def(
            capability.def_id,
            &capability.name,
            &capability.args,
            receiver_ty,
        )
    }

    pub(in crate::typeck) fn type_implements_capability_by_def(
        &mut self,
        interface_def: DefId,
        interface_name: &str,
        args: &[Ty],
        receiver_ty: &Ty,
    ) -> bool {
        let cache_key =
            self.capability_resolution_cache_key(interface_def, interface_name, args, receiver_ty);
        if let Some(key) = cache_key.as_ref()
            && let Some(cached) = self.capability_resolution_cache.get(key)
        {
            return *cached;
        }
        let diagnostic_count = self.diagnostics.len();
        let implements = self.type_implements_capability_by_def_uncached(
            interface_def,
            interface_name,
            args,
            receiver_ty,
        );
        if let Some(key) = cache_key
            && self.diagnostics.len() == diagnostic_count
        {
            self.capability_resolution_cache.insert(key, implements);
        }
        implements
    }

    fn capability_resolution_cache_key(
        &self,
        interface_def: DefId,
        interface_name: &str,
        args: &[Ty],
        receiver_ty: &Ty,
    ) -> Option<CapabilityResolutionCacheKey> {
        if self.symbolic_impl_resolution_depth > 0
            || contains_generic(receiver_ty)
            || contains_type_hole(receiver_ty)
            || args
                .iter()
                .any(|arg| contains_generic(arg) || contains_type_hole(arg))
        {
            return None;
        }
        Some(CapabilityResolutionCacheKey {
            epoch: self.capability_resolution_epoch,
            resolution: CapabilityResolutionKey {
                interface_def,
                interface_name: interface_name.to_string(),
                args: args.to_vec(),
                receiver_ty: receiver_ty.clone(),
            },
        })
    }

    pub(in crate::typeck) fn bump_capability_resolution_epoch(&mut self) {
        self.capability_resolution_epoch = self.capability_resolution_epoch.wrapping_add(1);
        self.capability_resolution_cache.clear();
        self.meta_repr_storage_cache.clear();
    }

    fn type_implements_capability_by_def_uncached(
        &mut self,
        interface_def: DefId,
        interface_name: &str,
        args: &[Ty],
        receiver_ty: &Ty,
    ) -> bool {
        if let Ty::OpaqueReturn { .. } = receiver_ty {
            if self.opaque_return_bounds_forbid(receiver_ty, interface_def, args) {
                return false;
            }
            if self.opaque_return_bounds_prove(receiver_ty, interface_def, args) {
                return true;
            }
        }
        if self.is_std_message_clone_interface_def(interface_def)
            && args.is_empty()
            && let Some(sop_ty) = self.meta_repr_owned_message_witness_ty(receiver_ty)
        {
            return self.type_implements_capability_by_def(
                interface_def,
                interface_name,
                args,
                &sop_ty,
            );
        }
        if self.type_implements_capability_for_receiver(
            interface_def,
            interface_name,
            args,
            receiver_ty,
        ) {
            return true;
        }
        let storage_receiver_ty = self.meta_repr_constraint_receiver_ty(receiver_ty, None);
        if &storage_receiver_ty == receiver_ty {
            return false;
        }
        self.type_implements_capability_for_receiver(
            interface_def,
            interface_name,
            args,
            &storage_receiver_ty,
        )
    }

    pub(super) fn opaque_return_bounds_prove(
        &self,
        ty: &Ty,
        interface_def: DefId,
        args: &[Ty],
    ) -> bool {
        let Ty::OpaqueReturn { bounds, .. } = ty else {
            return false;
        };
        bounds
            .positive
            .iter()
            .any(|entry| entry.def_id == interface_def && entry.args == args)
    }

    pub(super) fn opaque_return_bounds_forbid(
        &self,
        ty: &Ty,
        interface_def: DefId,
        args: &[Ty],
    ) -> bool {
        let Ty::OpaqueReturn { bounds, .. } = ty else {
            return false;
        };
        bounds
            .negative
            .iter()
            .any(|entry| entry.def_id == interface_def && entry.args == args)
    }

    pub(super) fn type_implements_capability_for_receiver(
        &mut self,
        interface_def: DefId,
        interface_name: &str,
        args: &[Ty],
        receiver_ty: &Ty,
    ) -> bool {
        let key = CapabilityResolutionKey {
            interface_def,
            interface_name: interface_name.to_string(),
            args: args.to_vec(),
            receiver_ty: receiver_ty.clone(),
        };
        if !self.capability_resolution_stack.insert(key.clone()) {
            return false;
        }
        let implements =
            self.type_implements_capability_inner(interface_def, interface_name, args, receiver_ty);
        self.capability_resolution_stack.remove(&key);
        implements
    }

    pub(super) fn type_implements_capability_inner(
        &mut self,
        interface_def: DefId,
        interface_name: &str,
        args: &[Ty],
        receiver_ty: &Ty,
    ) -> bool {
        if let Ty::OpaqueState { base, .. } = receiver_ty
            && !self.is_std_message_capability_interface_def(interface_def)
            && !self.is_std_message_clone_interface_def(interface_def)
            && !self.is_std_message_share_handle_marker_def(interface_def)
            && !self.is_std_async_spawnable_future_marker_def(interface_def)
        {
            return self.type_implements_capability_inner(
                interface_def,
                interface_name,
                args,
                base,
            );
        }
        if (self.is_std_message_clone_interface_def(interface_def)
            || self.is_std_message_share_handle_marker_def(interface_def))
            && args.is_empty()
            && self.type_is_affine(receiver_ty)
        {
            return false;
        }
        if self.is_std_message_capability_interface_def(interface_def)
            && args.is_empty()
            && self.type_is_affine(receiver_ty)
        {
            return false;
        }
        if self.is_std_message_capability_interface_def(interface_def)
            && args.is_empty()
            && let Ty::ClosureInstance { captures, .. } = receiver_ty
            && !captures.iter().all(|capture| {
                self.type_implements_capability_by_def(interface_def, interface_name, args, capture)
            })
        {
            return false;
        }
        if self.symbolic_impl_resolution_depth > 0
            && self.symbolically_proves_capability_by_def(
                interface_def,
                interface_name,
                args,
                receiver_ty,
            )
        {
            return true;
        }
        if self.is_std_async_spawnable_future_marker_def(interface_def) && args.is_empty() {
            return self.type_implements_spawnable_future_marker(
                interface_def,
                interface_name,
                receiver_ty,
            );
        }
        if self.type_implements_compiler_provided_interface(interface_def, args, receiver_ty) {
            return true;
        }
        if let Ty::GeneratedFuture {
            output,
            cancel_safe,
            abortable,
            ..
        } = receiver_ty
        {
            return (std_id::is_std_async_interface(
                &self.ctx.resolved,
                interface_def,
                STD_ASYNC_AWAITABLE_FUTURE_INTERFACE,
            ) && args.len() == 1
                && args.first() == Some(output))
                || (std_id::is_std_async_interface(
                    &self.ctx.resolved,
                    interface_def,
                    STD_ASYNC_CANCEL_SAFE_INTERFACE,
                ) && args.is_empty()
                    && *cancel_safe)
                || (std_id::is_std_async_interface(
                    &self.ctx.resolved,
                    interface_def,
                    STD_ASYNC_ABORT_FUTURE_INTERFACE,
                ) && args.is_empty()
                    && *abortable);
        }
        if retained_closure_proves_capability(receiver_ty, interface_def, args) {
            return true;
        }
        self.find_or_instantiate_impl_by_full_args_optional_span(
            interface_def,
            interface_name,
            &std::iter::once(receiver_ty.clone())
                .chain(args.iter().cloned())
                .collect::<Vec<_>>(),
            Some(receiver_ty),
            None,
        )
        .is_some()
            || ((std_id::is_std_async_interface(
                &self.ctx.resolved,
                interface_def,
                STD_ASYNC_CANCEL_SAFE_INTERFACE,
            ) || std_id::is_std_async_interface(
                &self.ctx.resolved,
                interface_def,
                STD_ASYNC_ABORT_FUTURE_INTERFACE,
            )) && self.generic_impl_matches_without_constraints(
                interface_def,
                interface_name,
                args,
                receiver_ty,
            ))
    }

    pub(super) fn type_implements_compiler_provided_interface(
        &self,
        interface_def: DefId,
        args: &[Ty],
        receiver_ty: &Ty,
    ) -> bool {
        if !args.is_empty() {
            return false;
        }
        if self.is_compiler_provided_error_witness_def(interface_def) {
            return !matches!(receiver_ty, Ty::Unknown | Ty::Hole(_));
        }
        (self.is_std_meta_ciel_fn_value_marker_def(interface_def)
            && matches!(
                receiver_ty,
                Ty::Function {
                    is_unsafe: false,
                    abi: None,
                    ..
                }
            ))
            || (self.is_std_meta_closure_value_marker_def(interface_def)
                && matches!(receiver_ty, Ty::ClosureInstance { .. }))
    }

    pub(super) fn closure_constraints_satisfied_by_ty(
        &mut self,
        constraints: &ConstraintBounds,
        source_ty: &Ty,
        span: crate::span::Span,
        emit_diagnostics: bool,
    ) -> bool {
        let mut ok = true;
        for capability in &constraints.positive {
            if !self.type_implements_capability_ref(capability, source_ty) {
                ok = false;
                if emit_diagnostics {
                    self.diagnostics.push(
                        Diagnostic::new(
                            span,
                            format!(
                                "closure conversion requires `{}` to implement `{}`",
                                source_ty, capability.name
                            ),
                        )
                        .note(format!(
                            "required capability: `{}`",
                            display_constraint_ref(capability)
                        ))
                        .note(format!(
                            "closure target constraints: `{}`",
                            display_constraint_bounds(constraints)
                        )),
                    );
                }
            }
        }
        for capability in &constraints.negative {
            if self.type_implements_capability_ref(capability, source_ty) {
                ok = false;
                if emit_diagnostics {
                    self.diagnostics.push(
                        Diagnostic::new(
                            span,
                            format!(
                                "closure conversion forbids `{}` from implementing `{}`",
                                source_ty, capability.name
                            ),
                        )
                        .note(format!(
                            "forbidden capability: `{}`",
                            display_constraint_ref(capability)
                        ))
                        .note(format!(
                            "closure target constraints: `{}`",
                            display_constraint_bounds(constraints)
                        )),
                    );
                }
            }
        }
        ok
    }

    pub(in crate::typeck) fn type_implements_message(&mut self, ty: &Ty) -> bool {
        if self.type_is_affine(ty) {
            return false;
        }
        if let Some(repr_ty) = self.meta_repr_owned_message_witness_ty(ty) {
            return self.type_implements_message(&repr_ty);
        }
        let Some(interface_def) = self.std_message_interface_def(STD_MESSAGE_CLONE_INTERFACE)
        else {
            return false;
        };
        self.type_implements_capability_by_def(interface_def, STD_MESSAGE_CLONE_INTERFACE, &[], ty)
    }

    pub(super) fn meta_repr_owned_message_witness_ty(&mut self, ty: &Ty) -> Option<Ty> {
        let (borrowed, _) = meta_repr_marker_source(ty)?;
        if borrowed {
            return None;
        }
        self.meta_repr_marker_sop_ty(ty)
    }

    fn type_implements_structural_message_clone(&mut self, ty: &Ty) -> bool {
        let repr_marker = self.std_meta_repr_marker_ty(false, ty.clone());
        self.type_implements_message(&repr_marker)
    }

    pub(super) fn type_is_resource_handle_leaf(&self, ty: &Ty) -> bool {
        std_id::is_std_resource_handle_ty(&self.ctx.resolved, ty)
    }

    pub(super) fn generic_is_resource_only(&self, name: &str) -> bool {
        self.resource_generic_stack
            .iter()
            .rev()
            .any(|scope| scope.contains(name))
    }

    pub(in crate::typeck) fn resource_generic_scope(generics: &[GenericInfo]) -> HashSet<String> {
        generics
            .iter()
            .filter(|generic| generic.is_resource)
            .map(|generic| generic.name.clone())
            .collect()
    }

    pub(in crate::typeck) fn type_is_affine(&mut self, ty: &Ty) -> bool {
        affine::type_is_affine(self, ty)
    }

    pub(super) fn task_boundary_message_violation(
        &mut self,
        ty: &Ty,
        path: &str,
        visiting: &mut HashSet<Ty>,
    ) -> Option<String> {
        self.task_boundary_message_violation_inner(ty, path, visiting, false)
    }

    pub(in crate::typeck) fn task_boundary_future_state_violation(
        &mut self,
        ty: &Ty,
        path: &str,
        visiting: &mut HashSet<Ty>,
    ) -> Option<String> {
        self.task_boundary_message_violation_inner(ty, path, visiting, true)
    }

    pub(in crate::typeck) fn future_state_escape_violation(
        &mut self,
        ty: &Ty,
        path: &str,
        visiting: &mut HashSet<Ty>,
    ) -> Option<String> {
        self.future_state_escape_violation_inner(ty, path, visiting)
    }

    pub(in crate::typeck) fn spawnable_future_state_violation(
        &mut self,
        ty: &Ty,
        path: &str,
    ) -> Option<String> {
        if matches!(ty, Ty::GeneratedFuture { .. }) || ty_has_hidden_state(ty) {
            return self.task_boundary_future_state_violation(ty, path, &mut HashSet::new());
        }
        None
    }

    pub(in crate::typeck) fn is_spawnable_future_ty(&mut self, ty: &Ty) -> bool {
        let Some(interface_def) =
            self.std_async_interface_def(STD_ASYNC_SPAWNABLE_FUTURE_INTERFACE)
        else {
            return false;
        };
        self.type_implements_capability_by_def(
            interface_def,
            STD_ASYNC_SPAWNABLE_FUTURE_INTERFACE,
            &[],
            ty,
        )
    }

    fn type_implements_spawnable_future_marker(
        &mut self,
        interface_def: DefId,
        interface_name: &str,
        receiver_ty: &Ty,
    ) -> bool {
        if self
            .spawnable_future_state_violation(receiver_ty, "future")
            .is_some()
        {
            return false;
        }
        let base_ty = strip_opaque_state_for_lookup(receiver_ty);
        if matches!(base_ty, Ty::GeneratedFuture { .. })
            || std_id::std_async_future_output_arg(&self.ctx.resolved, &base_ty).is_some()
        {
            return true;
        }
        self.find_impl(interface_def, interface_name, &[], &base_ty)
            .is_some()
            || self
                .instantiate_generic_impl_for_receiver(
                    interface_def,
                    interface_name,
                    &[],
                    &base_ty,
                    None,
                )
                .is_some()
    }

    fn future_state_escape_violation_inner(
        &mut self,
        ty: &Ty,
        path: &str,
        visiting: &mut HashSet<Ty>,
    ) -> Option<String> {
        if ty.is_erased_value() || is_opaque_future_state_marker_ty(ty) {
            return None;
        }
        if self.type_implements_async_frame_opt_in(ty) {
            return None;
        }
        match ty {
            Ty::OpaqueState { state, .. } | Ty::GeneratedFuture { state, .. } => {
                if !visiting.insert(ty.clone()) {
                    return Some(format!("{path} is recursive through `{ty}`"));
                }
                for (name, state_ty) in state {
                    let state_path = if name.is_empty() {
                        format!("{path} state")
                    } else {
                        format!("{path} state `{name}`")
                    };
                    if let Some(reason) =
                        self.future_state_escape_violation_inner(state_ty, &state_path, visiting)
                    {
                        visiting.remove(ty);
                        return Some(reason);
                    }
                }
                visiting.remove(ty);
                None
            }
            _ => self.task_boundary_message_violation_inner(ty, path, visiting, true),
        }
    }

    fn task_boundary_message_violation_inner(
        &mut self,
        ty: &Ty,
        path: &str,
        visiting: &mut HashSet<Ty>,
        allow_affine_move: bool,
    ) -> Option<String> {
        if ty.is_erased_value() {
            return None;
        }
        if is_opaque_future_state_marker_ty(ty) {
            return Some(format!(
                "{path} has opaque future state without a proven `Message` boundary"
            ));
        }
        if let Ty::OpaqueState { base, state } = ty {
            if !allow_affine_move
                && let Some(reason) =
                    self.task_boundary_message_violation_inner(base, path, visiting, false)
            {
                return Some(reason);
            }
            if !visiting.insert(ty.clone()) {
                return Some(format!("{path} is recursive through `{ty}`"));
            }
            for (name, state_ty) in state {
                let state_path = if name.is_empty() {
                    format!("{path} state")
                } else {
                    format!("{path} state `{name}`")
                };
                if let Some(reason) = self.task_boundary_message_violation_inner(
                    state_ty,
                    &state_path,
                    visiting,
                    allow_affine_move,
                ) {
                    visiting.remove(ty);
                    return Some(reason);
                }
            }
            visiting.remove(ty);
            return None;
        }
        if self.type_implements_message(ty) {
            return None;
        }
        if self.is_std_error_ty(ty) {
            return Some(format!(
                "{path} has downcastable erased error type `Error`; downcastable erased errors are local-only, use `Report` for transfer"
            ));
        }
        if matches!(ty, Ty::OpaqueReturn { .. }) {
            let concrete = self.lower_opaque_returns_in_ty(ty);
            if &concrete != ty {
                return self.task_boundary_message_violation_inner(
                    &concrete,
                    path,
                    visiting,
                    allow_affine_move,
                );
            }
        }
        if self.type_implements_thread_local(ty) {
            return Some(format!("{path} has ThreadLocal type `{ty}`"));
        }
        if self.type_is_affine(ty) {
            if allow_affine_move {
                if matches!(ty, Ty::GeneratedFuture { .. }) {
                    // The future object itself is affine and consumed by spawn; only its
                    // stored frame state needs to cross the task boundary.
                } else if self.task_boundary_affine_move_allowed(ty, &mut HashSet::new()) {
                    return None;
                } else {
                    return Some(format!(
                        "{path} has resource-affine type `{ty}` without an ownership-transfer policy"
                    ));
                }
            } else {
                return Some(format!(
                    "{path} has resource-affine type `{ty}` without a `Message` policy"
                ));
            }
        }
        if !(allow_affine_move && matches!(ty, Ty::GeneratedFuture { .. }))
            && (contains_generic(ty) || contains_type_hole(ty))
        {
            return Some(format!(
                "{path} has generic type `{ty}` without a proven `Message` boundary"
            ));
        }
        match ty {
            Ty::Unknown
            | Ty::Hole(_)
            | Ty::Never
            | Ty::Void
            | Ty::Bool
            | Ty::Char
            | Ty::I8
            | Ty::I16
            | Ty::I32
            | Ty::I64
            | Ty::U8
            | Ty::U16
            | Ty::U32
            | Ty::U64
            | Ty::Usize
            | Ty::F32
            | Ty::F64 => None,
            Ty::OpaqueState { .. } => unreachable!("opaque state handled before message checks"),
            Ty::CSpelling { .. } => Some(format!(
                "{path} has opaque C spelling type `{ty}` without a `Message` policy"
            )),
            Ty::Pointer { nullable, .. } => {
                if *nullable {
                    Some(format!("{path} has nullable raw pointer type `{ty}`"))
                } else {
                    Some(format!("{path} has raw pointer type `{ty}`"))
                }
            }
            Ty::Slice { mutability, .. } => Some(format!(
                "{path} has borrowed {}slice type `{ty}`",
                if *mutability == ViewMutability::Writable {
                    "mutable "
                } else {
                    "read-only "
                }
            )),
            Ty::Array { elem, .. } => {
                if let Some(reason) = self.task_boundary_message_violation_inner(
                    elem,
                    &format!("{path} element"),
                    visiting,
                    allow_affine_move,
                ) {
                    return Some(reason);
                }
                if self.type_implements_structural_message_clone(ty) {
                    None
                } else {
                    Some(format!(
                        "{path} has fixed-size array type `{ty}` without a `Message` capability"
                    ))
                }
            }
            Ty::GeneratedFuture { state, .. } => {
                if !visiting.insert(ty.clone()) {
                    return Some(format!("{path} is recursive through `{ty}`"));
                }
                for (name, state_ty) in state {
                    let state_path = if name.is_empty() {
                        format!("{path} state")
                    } else {
                        format!("{path} state `{name}`")
                    };
                    if let Some(reason) = self.task_boundary_message_violation_inner(
                        state_ty,
                        &state_path,
                        visiting,
                        allow_affine_move,
                    ) {
                        visiting.remove(ty);
                        return Some(reason);
                    }
                }
                visiting.remove(ty);
                None
            }
            Ty::Named { name, args, .. } => {
                if !visiting.insert(ty.clone()) {
                    return Some(format!("{path} is recursive through `{ty}`"));
                }
                self.ensure_struct_instance(ty);
                let instance_name = enum_instance_name(name, args);
                if let Some(fields) = self.ctx.structs.get(&instance_name).cloned() {
                    for (field, field_ty) in fields {
                        if let Some(reason) = self.task_boundary_message_violation_inner(
                            &field_ty,
                            &format!("{path}.{field}"),
                            visiting,
                            allow_affine_move,
                        ) {
                            visiting.remove(ty);
                            return Some(reason);
                        }
                    }
                    visiting.remove(ty);
                    if self.type_implements_structural_message_clone(ty) {
                        return None;
                    }
                    return Some(format!(
                        "{path} has struct type `{ty}` without a `Message` capability"
                    ));
                }
                self.ensure_enum_instance(ty);
                if let Some(enm) = self.ctx.checked_enums.get(&instance_name).cloned() {
                    for variant in enm.variants {
                        for (idx, payload_ty) in variant.payload.iter().enumerate() {
                            if let Some(reason) = self.task_boundary_message_violation_inner(
                                payload_ty,
                                &format!("{path}.{}#{idx}", variant.name),
                                visiting,
                                allow_affine_move,
                            ) {
                                visiting.remove(ty);
                                return Some(reason);
                            }
                        }
                    }
                    visiting.remove(ty);
                    if self.type_implements_structural_message_clone(ty) {
                        return None;
                    }
                    return Some(format!(
                        "{path} has enum type `{ty}` without a `Message` capability"
                    ));
                }
                visiting.remove(ty);
                Some(format!(
                    "{path} has nominal type `{ty}` without a `Message` capability"
                ))
            }
            Ty::DynamicInterface { .. } => Some(format!(
                "{path} has dynamic interface type `{ty}` without a `Message` policy"
            )),
            Ty::OpaqueReturn { .. } => Some(format!(
                "{path} has opaque return type `{ty}` without a `Message` policy"
            )),
            Ty::Function { .. } => Some(format!(
                "{path} has function pointer type `{ty}` without a `Message` policy"
            )),
            Ty::Closure { .. } | Ty::ClosureInstance { .. } => Some(format!(
                "{path} has closure type `{ty}` without a `Message` policy"
            )),
            Ty::Generic(_) => Some(format!(
                "{path} has generic type `{ty}` without a proven `Message` boundary"
            )),
        }
    }

    pub(super) fn task_boundary_affine_move_allowed(
        &mut self,
        ty: &Ty,
        visiting: &mut HashSet<Ty>,
    ) -> bool {
        if ty.is_erased_value() || self.type_is_resource_handle_leaf(ty) {
            return true;
        }
        if self.type_implements_async_frame_opt_in(ty) {
            return true;
        }
        match ty {
            Ty::Array { elem, .. } => self.task_boundary_affine_move_allowed(elem, visiting),
            Ty::OpaqueState { base, state } => {
                (!self.type_is_affine(base)
                    || self.task_boundary_affine_move_allowed(base, visiting))
                    && state
                        .iter()
                        .all(|(_, ty)| self.task_boundary_component_move_allowed(ty, visiting))
            }
            Ty::OpaqueReturn { .. } => {
                let concrete = self.lower_opaque_returns_in_ty(ty);
                &concrete != ty && self.task_boundary_affine_move_allowed(&concrete, visiting)
            }
            Ty::Generic(_) => false,
            Ty::Named { def_id, name, args } => {
                let named_ty = named_ty(*def_id, name.clone(), args.clone());
                if std_id::std_async_future_output_arg(&self.ctx.resolved, &named_ty).is_some() {
                    return false;
                }
                self.ensure_struct_instance(&named_ty);
                let instance_name = enum_instance_name(name, args);
                if !visiting.insert(ty.clone()) {
                    return false;
                }
                let allowed = self
                    .ctx
                    .structs
                    .get(&instance_name)
                    .cloned()
                    .is_some_and(|fields| {
                        fields.iter().all(|(_, field_ty)| {
                            self.task_boundary_component_move_allowed(field_ty, visiting)
                        })
                    });
                if allowed {
                    visiting.remove(ty);
                    return true;
                }
                let allowed = args.iter().any(contains_generic)
                    && self
                        .ctx
                        .struct_templates
                        .get(name)
                        .cloned()
                        .is_some_and(|template| {
                            if args.len() != template.generics.len() {
                                return false;
                            }
                            let subst = template
                                .generics
                                .iter()
                                .map(|generic| generic.name.clone())
                                .zip(args.iter().cloned())
                                .collect::<HashMap<_, _>>();
                            template.fields.iter().all(|field| {
                                let field_ty =
                                    self.lower_type_with_subst_no_normalize(&field.ty, &subst);
                                self.task_boundary_component_move_allowed(&field_ty, visiting)
                            })
                        });
                if allowed {
                    visiting.remove(ty);
                    return true;
                }
                self.ensure_enum_instance(&named_ty);
                let allowed = self
                    .ctx
                    .checked_enums
                    .get(&instance_name)
                    .cloned()
                    .is_some_and(|enm| {
                        enm.variants.iter().all(|variant| {
                            variant.payload.iter().all(|payload_ty| {
                                self.task_boundary_component_move_allowed(payload_ty, visiting)
                            })
                        })
                    });
                if allowed {
                    visiting.remove(ty);
                    return true;
                }
                let allowed = args.iter().any(contains_generic)
                    && self
                        .ctx
                        .enum_templates
                        .get(name)
                        .cloned()
                        .is_some_and(|template| {
                            if args.len() != template.generics.len() {
                                return false;
                            }
                            let subst = template
                                .generics
                                .iter()
                                .map(|generic| generic.name.clone())
                                .zip(args.iter().cloned())
                                .collect::<HashMap<_, _>>();
                            template.variants.iter().all(|variant| {
                                variant.payload.iter().all(|payload| {
                                    let payload_ty =
                                        self.lower_type_with_subst_no_normalize(payload, &subst);
                                    self.task_boundary_component_move_allowed(&payload_ty, visiting)
                                })
                            })
                        });
                visiting.remove(ty);
                allowed
            }
            _ => false,
        }
    }

    fn task_boundary_component_move_allowed(
        &mut self,
        ty: &Ty,
        visiting: &mut HashSet<Ty>,
    ) -> bool {
        if self.type_is_affine(ty) {
            self.task_boundary_affine_move_allowed(ty, visiting)
        } else {
            self.task_boundary_message_violation_inner(
                ty,
                "future state",
                &mut HashSet::new(),
                false,
            )
            .is_none()
        }
    }

    pub(in crate::typeck) fn external_future_flow_ty(
        &mut self,
        ty: &Ty,
        label: &str,
    ) -> Option<Ty> {
        let state = vec![(label.to_string(), opaque_future_state_marker_ty())];
        let mut visiting = HashSet::new();
        let wrapped = self.ty_with_future_state_entries(ty, &state, &mut visiting);
        (&wrapped != ty).then_some(wrapped)
    }

    pub(in crate::typeck) fn storage_and_flow_ty(&self, ty: &Ty) -> (Ty, Option<Ty>) {
        match ty {
            Ty::GeneratedFuture { output, .. } => (
                std_future_ty(&self.ctx.resolved, (**output).clone()),
                Some(ty.clone()),
            ),
            Ty::OpaqueState { base, .. } => ((**base).clone(), Some(ty.clone())),
            _ => (ty.clone(), None),
        }
    }

    pub(in crate::typeck) fn ty_preserving_hidden_state_for_expected(
        &mut self,
        expected: &Ty,
        actual: &Ty,
    ) -> Ty {
        if let Ty::OpaqueState { base, state } = actual
            && (self.ty_can_assign_from(expected, base)
                || std_id::std_async_future_accepts_generated(&self.ctx.resolved, expected, base)
                || self.meta_repr_storage_equivalent(expected, base))
        {
            return opaque_state_ty(expected.clone(), state.clone());
        }
        if std_id::std_async_future_accepts_generated(&self.ctx.resolved, expected, actual) {
            return actual.clone();
        }
        expected.clone()
    }

    pub(in crate::typeck) fn external_storage_and_flow_ty(
        &mut self,
        ty: &Ty,
        label: &str,
    ) -> (Ty, Option<Ty>) {
        let (storage_ty, known_flow_ty) = self.storage_and_flow_ty(ty);
        let flow_ty = known_flow_ty.or_else(|| self.external_future_flow_ty(&storage_ty, label));
        (storage_ty, flow_ty)
    }

    pub(in crate::typeck) fn call_return_future_state_ty(
        &mut self,
        ret: &Ty,
        checked_args: &[TExpr],
    ) -> Ty {
        if checked_args.is_empty() {
            return ret.clone();
        }
        let state = checked_args
            .iter()
            .enumerate()
            .filter(|(_, arg)| !matches!(arg.ty, Ty::Function { .. }))
            .map(|(idx, arg)| (format!("argument {idx}"), arg.ty.clone()))
            .collect::<Vec<_>>();
        let mut visiting = HashSet::new();
        self.ty_with_future_state_entries(ret, &state, &mut visiting)
    }

    pub(in crate::typeck) fn awaited_output_future_state_ty(
        &mut self,
        future_ty: &Ty,
        output_ty: Ty,
    ) -> Ty {
        let state = match future_ty {
            Ty::GeneratedFuture { state, .. } | Ty::OpaqueState { state, .. } => state.clone(),
            _ => Vec::new(),
        };
        if state.is_empty() {
            return output_ty;
        }
        let mut visiting = HashSet::new();
        self.ty_with_future_state_entries(&output_ty, &state, &mut visiting)
    }

    fn ty_with_future_state_entries(
        &mut self,
        ty: &Ty,
        state: &[(String, Ty)],
        visiting: &mut HashSet<Ty>,
    ) -> Ty {
        if state.is_empty() {
            return ty.clone();
        }
        if let Ty::OpaqueState {
            base,
            state: existing,
        } = ty
        {
            let wrapped_base = self.ty_with_future_state_entries(base, state, visiting);
            return opaque_state_ty(wrapped_base, existing.clone());
        }
        if std_id::std_async_future_output_arg(&self.ctx.resolved, ty).is_some() {
            return opaque_state_ty(ty.clone(), state.to_vec());
        }
        match ty {
            Ty::Array { elem, .. } => {
                let elem_ty = self.ty_with_future_state_entries(elem, state, visiting);
                if &elem_ty == elem.as_ref() {
                    ty.clone()
                } else {
                    opaque_state_ty(ty.clone(), vec![("element *".to_string(), elem_ty)])
                }
            }
            Ty::Slice { elem, .. } => {
                let elem_ty = self.ty_with_future_state_entries(elem, state, visiting);
                if &elem_ty == elem.as_ref() {
                    ty.clone()
                } else {
                    opaque_state_ty(ty.clone(), vec![("element *".to_string(), elem_ty)])
                }
            }
            Ty::Named { def_id, name, args } => {
                let named_ty = named_ty(*def_id, name.clone(), args.clone());
                if !visiting.insert(named_ty.clone()) {
                    return ty.clone();
                }
                self.ensure_struct_instance(&named_ty);
                let instance_name = enum_instance_name(name, args);
                if let Some(fields) = self.ctx.structs.get(&instance_name).cloned() {
                    let mut hidden = Vec::new();
                    for (field, field_ty) in fields {
                        let wrapped = self.ty_with_future_state_entries(&field_ty, state, visiting);
                        if wrapped != field_ty {
                            hidden.push((field, wrapped));
                        }
                    }
                    visiting.remove(&named_ty);
                    return opaque_state_ty(ty.clone(), hidden);
                }
                self.ensure_enum_instance(&named_ty);
                if let Some(enm) = self.ctx.checked_enums.get(&instance_name).cloned() {
                    let mut hidden = Vec::new();
                    for variant in enm.variants {
                        for (idx, payload_ty) in variant.payload.into_iter().enumerate() {
                            let wrapped =
                                self.ty_with_future_state_entries(&payload_ty, state, visiting);
                            if wrapped != payload_ty {
                                hidden.push((format!("{}#{idx}", variant.name), wrapped));
                            }
                        }
                    }
                    visiting.remove(&named_ty);
                    return opaque_state_ty(ty.clone(), hidden);
                }
                visiting.remove(&named_ty);
                ty.clone()
            }
            _ => ty.clone(),
        }
    }

    pub(in crate::typeck) fn project_opaque_field_ty(
        &self,
        field_ty: Ty,
        state: &[(String, Ty)],
        field: &str,
    ) -> Ty {
        let selected = state
            .iter()
            .filter(|(name, _)| name == field)
            .cloned()
            .collect::<Vec<_>>();
        self.project_opaque_state_ty(field_ty, selected)
    }

    pub(in crate::typeck) fn project_opaque_index_ty(
        &self,
        elem_ty: Ty,
        state: &[(String, Ty)],
    ) -> Ty {
        let selected = state
            .iter()
            .filter(|(name, _)| name.starts_with("element "))
            .cloned()
            .collect::<Vec<_>>();
        self.project_opaque_state_ty(elem_ty, selected)
    }

    pub(in crate::typeck) fn project_opaque_variant_payload_ty(
        &self,
        payload_ty: Ty,
        state: &[(String, Ty)],
        variant_name: &str,
        physical_index: usize,
    ) -> Ty {
        let key = format!("{variant_name}#{physical_index}");
        let selected = state
            .iter()
            .filter(|(name, _)| name == &key)
            .cloned()
            .collect::<Vec<_>>();
        self.project_opaque_state_ty(payload_ty, selected)
    }

    fn project_opaque_state_ty(&self, projected_ty: Ty, selected: Vec<(String, Ty)>) -> Ty {
        if selected.is_empty() {
            return projected_ty;
        }
        if selected.len() == 1 {
            let source_ty = &selected[0].1;
            if let Ty::OpaqueState { base, state } = source_ty
                && projected_ty.can_assign_from(base)
            {
                return opaque_state_ty(projected_ty, state.clone());
            }
            if std_id::std_async_future_accepts_generated(
                &self.ctx.resolved,
                &projected_ty,
                source_ty,
            ) || projected_ty.can_assign_from(source_ty)
            {
                return source_ty.clone();
            }
        }
        opaque_state_ty(projected_ty, selected)
    }

    pub(super) fn meta_repr_marker_matches_concrete(&mut self, marker: &Ty, concrete: &Ty) -> bool {
        self.meta_repr_marker_sop_ty(marker)
            .or_else(|| self.meta_schema_marker_sop_ty(marker))
            .is_some_and(|repr_ty| self.meta_repr_storage_equivalent_inner(&repr_ty, concrete))
    }

    pub(in crate::typeck) fn meta_repr_storage_equivalent(
        &mut self,
        left: &Ty,
        right: &Ty,
    ) -> bool {
        let left = self.resolve_type_holes(left);
        let right = self.resolve_type_holes(right);
        self.meta_repr_storage_equivalent_inner(&left, &right)
    }

    pub(super) fn meta_repr_storage_equivalent_inner(&mut self, left: &Ty, right: &Ty) -> bool {
        if left == right {
            return true;
        }
        if let Ty::OpaqueState { base, .. } = left {
            return self.meta_repr_storage_equivalent_inner(base, right);
        }
        if let Ty::OpaqueState { base, .. } = right {
            return self.meta_repr_storage_equivalent_inner(left, base);
        }
        if self.meta_repr_marker_matches_concrete(left, right)
            || self.meta_repr_marker_matches_concrete(right, left)
        {
            return true;
        }
        match (left, right) {
            (
                Ty::Pointer {
                    nullable: left_nullable,
                    mutability: left_mutability,
                    inner: left_inner,
                },
                Ty::Pointer {
                    nullable: right_nullable,
                    mutability: right_mutability,
                    inner: right_inner,
                },
            ) => {
                left_nullable == right_nullable
                    && left_mutability == right_mutability
                    && self.meta_repr_storage_equivalent_inner(left_inner, right_inner)
            }
            (
                Ty::Array {
                    len: left_len,
                    elem: left_elem,
                },
                Ty::Array {
                    len: right_len,
                    elem: right_elem,
                },
            ) => {
                left_len == right_len
                    && self.meta_repr_storage_equivalent_inner(left_elem, right_elem)
            }
            (
                Ty::Slice {
                    mutability: left_mutability,
                    elem: left_elem,
                },
                Ty::Slice {
                    mutability: right_mutability,
                    elem: right_elem,
                },
            ) => {
                left_mutability == right_mutability
                    && self.meta_repr_storage_equivalent_inner(left_elem, right_elem)
            }
            (
                Ty::Named {
                    def_id: left_def_id,
                    name: left_name,
                    args: left_args,
                },
                Ty::Named {
                    def_id: right_def_id,
                    name: right_name,
                    args: right_args,
                },
            ) => {
                named_ty_identity_eq(*left_def_id, left_name, *right_def_id, right_name)
                    && left_args.len() == right_args.len()
                    && left_args
                        .iter()
                        .zip(right_args.iter())
                        .all(|(left, right)| self.meta_repr_storage_equivalent_inner(left, right))
            }
            (
                Ty::DynamicInterface {
                    def_id: left_def_id,
                    args: left_args,
                    ..
                },
                Ty::DynamicInterface {
                    def_id: right_def_id,
                    args: right_args,
                    ..
                },
            ) => {
                left_def_id == right_def_id
                    && left_args.len() == right_args.len()
                    && left_args
                        .iter()
                        .zip(right_args.iter())
                        .all(|(left, right)| self.meta_repr_storage_equivalent_inner(left, right))
            }
            (
                Ty::GeneratedFuture {
                    name: left_name,
                    output: left_output,
                    cancel_safe: left_cancel_safe,
                    abortable: left_abortable,
                    affine_state: left_affine_state,
                    state: left_state,
                },
                Ty::GeneratedFuture {
                    name: right_name,
                    output: right_output,
                    cancel_safe: right_cancel_safe,
                    abortable: right_abortable,
                    affine_state: right_affine_state,
                    state: right_state,
                },
            ) => {
                left_name == right_name
                    && left_cancel_safe == right_cancel_safe
                    && left_abortable == right_abortable
                    && left_affine_state == right_affine_state
                    && left_state.len() == right_state.len()
                    && self.meta_repr_storage_equivalent_inner(left_output, right_output)
                    && left_state.iter().zip(right_state.iter()).all(
                        |((left_name, left_ty), (right_name, right_ty))| {
                            left_name == right_name
                                && self.meta_repr_storage_equivalent_inner(left_ty, right_ty)
                        },
                    )
            }
            (
                Ty::Function {
                    is_unsafe: left_is_unsafe,
                    abi: left_abi,
                    ret: left_ret,
                    params: left_params,
                },
                Ty::Function {
                    is_unsafe: right_is_unsafe,
                    abi: right_abi,
                    ret: right_ret,
                    params: right_params,
                },
            ) => {
                left_is_unsafe == right_is_unsafe
                    && left_abi == right_abi
                    && left_params.len() == right_params.len()
                    && self.meta_repr_storage_equivalent_inner(left_ret, right_ret)
                    && left_params
                        .iter()
                        .zip(right_params.iter())
                        .all(|(left, right)| self.meta_repr_storage_equivalent_inner(left, right))
            }
            (
                Ty::Closure {
                    ret: left_ret,
                    params: left_params,
                    constraints: left_constraints,
                },
                Ty::Closure {
                    ret: right_ret,
                    params: right_params,
                    constraints: right_constraints,
                },
            ) => {
                left_constraints == right_constraints
                    && left_params.len() == right_params.len()
                    && self.meta_repr_storage_equivalent_inner(left_ret, right_ret)
                    && left_params
                        .iter()
                        .zip(right_params.iter())
                        .all(|(left, right)| self.meta_repr_storage_equivalent_inner(left, right))
            }
            (
                Ty::ClosureInstance {
                    id: left_id,
                    ret: left_ret,
                    params: left_params,
                    captures: left_captures,
                },
                Ty::ClosureInstance {
                    id: right_id,
                    ret: right_ret,
                    params: right_params,
                    captures: right_captures,
                },
            ) => {
                left_id == right_id
                    && left_params.len() == right_params.len()
                    && left_captures.len() == right_captures.len()
                    && self.meta_repr_storage_equivalent_inner(left_ret, right_ret)
                    && left_params
                        .iter()
                        .zip(right_params.iter())
                        .all(|(left, right)| self.meta_repr_storage_equivalent_inner(left, right))
                    && left_captures
                        .iter()
                        .zip(right_captures.iter())
                        .all(|(left, right)| self.meta_repr_storage_equivalent_inner(left, right))
            }
            _ => false,
        }
    }

    pub(in crate::typeck) fn type_implements_share_handle(&mut self, ty: &Ty) -> bool {
        let Some(interface_def) =
            self.std_message_interface_def(STD_MESSAGE_SHARE_HANDLE_INTERFACE)
        else {
            return false;
        };
        if self.type_is_affine(ty) {
            return false;
        }
        self.find_impl(interface_def, STD_MESSAGE_SHARE_HANDLE_INTERFACE, &[], ty)
            .is_some()
            || self.generic_impl_matches_without_constraints(
                interface_def,
                STD_MESSAGE_SHARE_HANDLE_INTERFACE,
                &[],
                ty,
            )
    }

    pub(in crate::typeck) fn type_implements_async_frame_opt_in(&mut self, ty: &Ty) -> bool {
        let Some(interface_def) =
            self.std_message_interface_def(STD_MESSAGE_ASYNC_FRAME_OPT_IN_INTERFACE)
        else {
            return false;
        };
        self.type_implements_capability_by_def(
            interface_def,
            STD_MESSAGE_ASYNC_FRAME_OPT_IN_INTERFACE,
            &[],
            ty,
        )
    }

    pub(in crate::typeck) fn type_implements_meta_policy_marker(&mut self, ty: &Ty) -> bool {
        // Marker normalization must preserve nominal resource/future boundaries so
        // generic impl identity is stable, but owned meta representation still
        // rejects affine values in `is_owned_meta_policy_leaf`.
        self.type_implements_share_handle(ty)
            || self.type_implements_thread_local(ty)
            || self.type_is_affine(ty)
    }

    pub(in crate::typeck) fn type_implements_thread_local(&mut self, ty: &Ty) -> bool {
        let Some(interface_def) =
            self.std_message_interface_def(STD_MESSAGE_THREAD_LOCAL_INTERFACE)
        else {
            return false;
        };
        self.find_impl(interface_def, STD_MESSAGE_THREAD_LOCAL_INTERFACE, &[], ty)
            .is_some()
            || self.generic_impl_matches_without_constraints(
                interface_def,
                STD_MESSAGE_THREAD_LOCAL_INTERFACE,
                &[],
                ty,
            )
    }

    pub(in crate::typeck) fn generic_impl_matches_without_constraints(
        &self,
        interface_def: DefId,
        interface_name: &str,
        non_receiver_args: &[Ty],
        receiver_ty: &Ty,
    ) -> bool {
        let _ = interface_name;
        CapabilityTable::new(&self.ctx).generic_impl_matches_without_constraints(
            interface_def,
            non_receiver_args,
            receiver_ty,
        )
    }

    pub(super) fn std_message_view(&mut self, name: &str) -> InterfaceView {
        self.std_message_interface_or_alias_def(name)
            .map(|def_id| self.interface_view_for_def(def_id, &[]))
            .unwrap_or_default()
    }

    pub(in crate::typeck) fn std_message_interface_def(&self, name: &str) -> Option<DefId> {
        self.ctx
            .interfaces
            .keys()
            .copied()
            .find(|def_id| std_id::is_std_message_interface(&self.ctx.resolved, *def_id, name))
    }

    pub(in crate::typeck) fn std_message_interface_alias_def(&self, name: &str) -> Option<DefId> {
        self.ctx.interface_aliases.keys().copied().find(|def_id| {
            std_id::is_std_message_interface_alias(&self.ctx.resolved, *def_id, name)
        })
    }

    pub(in crate::typeck) fn std_message_interface_or_alias_def(
        &self,
        name: &str,
    ) -> Option<DefId> {
        self.std_message_interface_def(name)
            .or_else(|| self.std_message_interface_alias_def(name))
    }

    pub(super) fn std_error_interface_def(&self, name: &str) -> Option<DefId> {
        self.ctx
            .interfaces
            .keys()
            .copied()
            .find(|def_id| std_id::is_std_error_interface(&self.ctx.resolved, *def_id, name))
    }

    pub(super) fn std_error_interface_alias_def(&self, name: &str) -> Option<DefId> {
        self.ctx
            .interface_aliases
            .keys()
            .copied()
            .find(|def_id| std_id::is_std_error_interface_alias(&self.ctx.resolved, *def_id, name))
    }

    pub(in crate::typeck) fn find_impl_by_full_args(
        &self,
        interface_def: DefId,
        interface_name: &str,
        interface_args: &[Ty],
        receiver_ty: Option<&Ty>,
    ) -> Option<ImplSig> {
        let _ = interface_name;
        CapabilityTable::new(&self.ctx)
            .find_impl_by_full_args(interface_def, interface_args, receiver_ty)
            .cloned()
    }

    pub(super) fn find_or_instantiate_impl_by_full_args(
        &mut self,
        interface_def: DefId,
        interface_name: &str,
        interface_args: &[Ty],
        receiver_ty: Option<&Ty>,
        span: crate::span::Span,
    ) -> Option<ImplSig> {
        self.find_or_instantiate_impl_by_full_args_optional_span(
            interface_def,
            interface_name,
            interface_args,
            receiver_ty,
            Some(span),
        )
    }

    pub(super) fn find_or_instantiate_impl_by_full_args_optional_span(
        &mut self,
        interface_def: DefId,
        interface_name: &str,
        interface_args: &[Ty],
        receiver_ty: Option<&Ty>,
        span: Option<crate::span::Span>,
    ) -> Option<ImplSig> {
        let stripped_interface_args = interface_args
            .iter()
            .map(strip_opaque_state_for_lookup)
            .collect::<Vec<_>>();
        let stripped_receiver_ty = receiver_ty.map(strip_opaque_state_for_lookup);
        if stripped_interface_args.as_slice() != interface_args
            || stripped_receiver_ty.as_ref() != receiver_ty
        {
            return self.find_or_instantiate_impl_by_full_args_optional_span(
                interface_def,
                interface_name,
                &stripped_interface_args,
                stripped_receiver_ty.as_ref(),
                span,
            );
        }
        if let Some(receiver_ty) = receiver_ty
            && matches!(receiver_ty, Ty::OpaqueReturn { .. })
        {
            let non_receiver_args = interface_non_receiver_args(interface_args);
            if !self.opaque_return_bounds_prove(receiver_ty, interface_def, non_receiver_args) {
                return None;
            }
            let concrete_receiver = self.lower_opaque_returns_in_ty(receiver_ty);
            if &concrete_receiver != receiver_ty {
                let concrete_args = interface_args
                    .iter()
                    .map(|arg| self.lower_opaque_returns_in_ty(arg))
                    .collect::<Vec<_>>();
                return self.find_or_instantiate_impl_by_full_args_optional_span(
                    interface_def,
                    interface_name,
                    &concrete_args,
                    Some(&concrete_receiver),
                    span,
                );
            }
        }
        if let Some(implementation) =
            self.find_impl_by_full_args(interface_def, interface_name, interface_args, receiver_ty)
        {
            return Some(implementation);
        }
        if let Some(implementation) = self.instantiate_generic_impl(
            interface_def,
            interface_name,
            interface_args,
            receiver_ty,
            span,
        ) {
            return Some(implementation);
        }
        if let Some(implementation) = self.find_symbolic_impl_by_full_args(
            interface_def,
            interface_name,
            interface_args,
            receiver_ty,
        ) {
            return Some(implementation);
        }
        let storage_interface_args = interface_args
            .iter()
            .map(|ty| self.meta_repr_constraint_receiver_ty(ty, span))
            .collect::<Vec<_>>();
        let storage_receiver_ty =
            receiver_ty.map(|ty| self.meta_repr_constraint_receiver_ty(ty, span));
        if (storage_interface_args.as_slice() != interface_args
            || storage_receiver_ty.as_ref() != receiver_ty)
            && let Some(implementation) = self
                .find_impl_by_full_args(
                    interface_def,
                    interface_name,
                    &storage_interface_args,
                    storage_receiver_ty.as_ref(),
                )
                .or_else(|| {
                    self.instantiate_generic_impl(
                        interface_def,
                        interface_name,
                        &storage_interface_args,
                        storage_receiver_ty.as_ref(),
                        span,
                    )
                })
        {
            return Some(implementation);
        }
        if self.symbolic_impl_resolution_depth > 0 {
            let storage_interface_args = interface_args
                .iter()
                .map(|ty| self.meta_repr_symbolic_constraint_receiver_ty(ty, span))
                .collect::<Vec<_>>();
            let storage_receiver_ty =
                receiver_ty.map(|ty| self.meta_repr_symbolic_constraint_receiver_ty(ty, span));
            if (storage_interface_args.as_slice() != interface_args
                || storage_receiver_ty.as_ref() != receiver_ty)
                && let Some(implementation) = self.find_symbolic_impl_by_full_args(
                    interface_def,
                    interface_name,
                    &storage_interface_args,
                    storage_receiver_ty.as_ref(),
                )
            {
                return Some(implementation);
            }
        }
        None
    }

    pub(in crate::typeck) fn with_symbolic_impl_resolution<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.symbolic_impl_resolution_depth += 1;
        let result = f(self);
        self.symbolic_impl_resolution_depth -= 1;
        result
    }

    pub(super) fn find_symbolic_impl_by_full_args(
        &mut self,
        interface_def: DefId,
        interface_name: &str,
        interface_args: &[Ty],
        receiver_ty: Option<&Ty>,
    ) -> Option<ImplSig> {
        if self.symbolic_impl_resolution_depth == 0 {
            return None;
        }
        let receiver_ty = receiver_ty?;
        let non_receiver_args = interface_non_receiver_args(interface_args);
        if !self.symbolically_proves_capability_by_def(
            interface_def,
            interface_name,
            non_receiver_args,
            receiver_ty,
        ) {
            return None;
        }
        let interface = self.ctx.interfaces.get(&interface_def).cloned()?;
        if interface.generics.len() != interface_args.len() {
            return None;
        }
        let subst = interface
            .generics
            .iter()
            .cloned()
            .zip(interface_args.iter().cloned())
            .collect::<HashMap<_, _>>();
        let ret = self.lower_type_with_subst(&interface.ret, &subst);
        let params = interface
            .params
            .iter()
            .map(|param| self.lower_type_with_subst(&param.ty, &subst))
            .collect::<Vec<_>>();
        Some(ImplSig {
            interface_def,
            interface_name: interface_name.to_string(),
            interface_args: interface_args.to_vec(),
            receiver_ty: Some(receiver_ty.clone()),
            function_def: self.alloc_synthetic_def(),
            ret,
            params,
        })
    }

    pub(super) fn symbolically_proves_capability_by_def(
        &mut self,
        interface_def: DefId,
        interface_name: &str,
        args: &[Ty],
        receiver_ty: &Ty,
    ) -> bool {
        let key = CapabilityResolutionKey {
            interface_def,
            interface_name: interface_name.to_string(),
            args: args.to_vec(),
            receiver_ty: receiver_ty.clone(),
        };
        if !self
            .symbolic_capability_resolution_stack
            .insert(key.clone())
        {
            return false;
        }
        let proves = self.symbolically_proves_capability_inner(
            interface_def,
            interface_name,
            args,
            receiver_ty,
        );
        self.symbolic_capability_resolution_stack.remove(&key);
        proves
    }

    pub(super) fn symbolically_proves_capability_inner(
        &mut self,
        interface_def: DefId,
        interface_name: &str,
        args: &[Ty],
        receiver_ty: &Ty,
    ) -> bool {
        if (self.is_std_message_clone_interface_def(interface_def)
            || self.is_std_message_share_handle_marker_def(interface_def))
            && args.is_empty()
            && self.type_is_affine(receiver_ty)
        {
            return false;
        }
        if self.is_std_message_capability_interface_def(interface_def)
            && args.is_empty()
            && self.type_is_affine(receiver_ty)
        {
            return false;
        }
        if self.symbolic_generic_env_proves_capability(interface_def, args, receiver_ty) {
            return true;
        }
        if self.is_std_async_spawnable_future_marker_def(interface_def) && args.is_empty() {
            return self.type_implements_spawnable_future_marker(
                interface_def,
                interface_name,
                receiver_ty,
            );
        }
        if self.type_implements_compiler_provided_interface(interface_def, args, receiver_ty) {
            return true;
        }
        if let Ty::GeneratedFuture {
            output,
            cancel_safe,
            abortable,
            ..
        } = receiver_ty
        {
            return (std_id::is_std_async_interface(
                &self.ctx.resolved,
                interface_def,
                STD_ASYNC_AWAITABLE_FUTURE_INTERFACE,
            ) && args.len() == 1
                && args.first() == Some(output))
                || (std_id::is_std_async_interface(
                    &self.ctx.resolved,
                    interface_def,
                    STD_ASYNC_CANCEL_SAFE_INTERFACE,
                ) && args.is_empty()
                    && *cancel_safe)
                || (std_id::is_std_async_interface(
                    &self.ctx.resolved,
                    interface_def,
                    STD_ASYNC_ABORT_FUTURE_INTERFACE,
                ) && args.is_empty()
                    && *abortable);
        }
        if retained_closure_proves_capability(receiver_ty, interface_def, args) {
            return true;
        }
        if CapabilityTable::new(&self.ctx)
            .find_impl(interface_def, args, receiver_ty)
            .is_some()
        {
            return true;
        }
        if self.symbolic_generic_impl_proves_capability(
            interface_def,
            interface_name,
            args,
            receiver_ty,
        ) {
            return true;
        }
        let storage_receiver_ty = self.meta_repr_symbolic_constraint_receiver_ty(receiver_ty, None);
        let storage_args = args
            .iter()
            .map(|arg| self.meta_repr_symbolic_constraint_receiver_ty(arg, None))
            .collect::<Vec<_>>();
        if &storage_receiver_ty != receiver_ty || storage_args.as_slice() != args {
            return self.symbolically_proves_capability_by_def(
                interface_def,
                interface_name,
                &storage_args,
                &storage_receiver_ty,
            );
        }
        false
    }

    pub(super) fn symbolic_generic_env_proves_capability(
        &mut self,
        interface_def: DefId,
        args: &[Ty],
        receiver_ty: &Ty,
    ) -> bool {
        let Ty::Generic(receiver_name) = receiver_ty else {
            return false;
        };
        for env in self.symbolic_constraint_env_stack.iter().rev() {
            let Some(generic_constraint) = env
                .iter()
                .find(|constraint| constraint.name == *receiver_name)
            else {
                continue;
            };
            if generic_constraint
                .bounds
                .positive
                .iter()
                .any(|entry| entry.def_id == interface_def && entry.args == args)
            {
                return true;
            }
            if generic_constraint
                .bounds
                .negative
                .iter()
                .any(|entry| entry.def_id == interface_def && entry.args == args)
            {
                return false;
            }
        }
        let envs = self.generic_env_stack.clone();
        for env in envs {
            let Some(generic) = env.iter().find(|generic| generic.name == *receiver_name) else {
                continue;
            };
            let Some(constraint) = &generic.constraint else {
                continue;
            };
            let subst = Self::initial_generic_subst(&env);
            let bounds = self.constraint_bounds(constraint, &subst);
            if bounds
                .positive
                .iter()
                .any(|entry| entry.def_id == interface_def && entry.args == args)
            {
                return true;
            }
            if bounds
                .negative
                .iter()
                .any(|entry| entry.def_id == interface_def && entry.args == args)
            {
                return false;
            }
        }
        false
    }

    pub(super) fn symbolic_generic_impl_proves_capability(
        &mut self,
        interface_def: DefId,
        interface_name: &str,
        args: &[Ty],
        receiver_ty: &Ty,
    ) -> bool {
        let full_args = std::iter::once(receiver_ty.clone())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>();
        let templates = self.visible_generic_impl_templates();
        templates.iter().any(|template| {
            if template.interface_def != interface_def
                || template.interface_args.len() != full_args.len()
            {
                return false;
            }
            let scoped_names = template
                .generics
                .iter()
                .map(|generic| {
                    (
                        generic.name.clone(),
                        format!("$symbolic_impl${interface_name}${}", generic.name),
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
            for (pattern, actual) in template.interface_args.iter().zip(full_args.iter()) {
                let pattern = substitute_ty(pattern, &scoped_subst);
                if !unify_ty(&pattern, actual, &mut subst) {
                    return false;
                }
            }
            if let Some(pattern) = template.receiver_ty.as_ref() {
                let pattern = substitute_ty(pattern, &scoped_subst);
                if !unify_ty(&pattern, receiver_ty, &mut subst) {
                    return false;
                }
            }
            let scoped_name_set = scoped_names.values().cloned().collect::<HashSet<_>>();
            if scoped_names.values().any(|name| {
                subst
                    .get(name)
                    .is_none_or(|ty| contains_any_generic_name(ty, &scoped_name_set))
            }) {
                return false;
            }
            template.generic_constraints.iter().all(|constraint| {
                let Some(scoped_name) = scoped_names.get(&constraint.name) else {
                    return false;
                };
                let Some(concrete) = subst.get(scoped_name).cloned() else {
                    return false;
                };
                if constraint.is_resource && !self.type_is_affine(&concrete) {
                    return false;
                }
                let scoped_bounds = substitute_constraint_bounds(&constraint.bounds, &scoped_subst);
                let bounds = substitute_constraint_bounds(&scoped_bounds, &subst);
                self.symbolic_constraint_bounds_satisfied(&concrete, &bounds)
            })
        })
    }

    pub(super) fn symbolic_constraint_bounds_satisfied(
        &mut self,
        receiver_ty: &Ty,
        bounds: &ConstraintBounds,
    ) -> bool {
        bounds.positive.iter().all(|capability| {
            self.symbolically_proves_capability_by_def(
                capability.def_id,
                &capability.name,
                &capability.args,
                receiver_ty,
            )
        }) && bounds.negative.iter().all(|capability| {
            !self.symbolically_proves_capability_by_def(
                capability.def_id,
                &capability.name,
                &capability.args,
                receiver_ty,
            )
        })
    }

    pub(super) fn instantiate_generic_impl_for_receiver(
        &mut self,
        interface_def: DefId,
        interface_name: &str,
        non_receiver_args: &[Ty],
        receiver_ty: &Ty,
        span: Option<crate::span::Span>,
    ) -> Option<ImplSig> {
        let interface_args = std::iter::once(receiver_ty.clone())
            .chain(non_receiver_args.iter().cloned())
            .collect::<Vec<_>>();
        self.instantiate_generic_impl(
            interface_def,
            interface_name,
            &interface_args,
            Some(receiver_ty),
            span,
        )
    }

    pub(super) fn instantiate_generic_impl(
        &mut self,
        interface_def: DefId,
        interface_name: &str,
        interface_args: &[Ty],
        receiver_ty: Option<&Ty>,
        span: Option<crate::span::Span>,
    ) -> Option<ImplSig> {
        if let Some(existing) =
            self.find_impl_by_full_args(interface_def, interface_name, interface_args, receiver_ty)
        {
            return Some(existing);
        }
        let templates = self.ctx.generic_impls.clone();
        let mut matches = Vec::new();
        for template in templates {
            if template.interface_def != interface_def {
                continue;
            }
            let mut subst = template
                .generics
                .iter()
                .map(|generic| (generic.name.clone(), Ty::Generic(generic.name.clone())))
                .collect::<HashMap<_, _>>();
            if template.interface_args.len() != interface_args.len() {
                continue;
            }
            let mut args_match = true;
            for (pattern, actual) in template.interface_args.iter().zip(interface_args.iter()) {
                if !self.unify_ty_for_inference(pattern, actual, &mut subst) {
                    args_match = false;
                    break;
                }
            }
            if !args_match {
                continue;
            }
            if let (Some(pattern), Some(actual)) = (template.receiver_ty.as_ref(), receiver_ty)
                && !self.unify_ty_for_inference(pattern, actual, &mut subst)
            {
                continue;
            }
            if template
                .generics
                .iter()
                .any(|generic| subst.get(&generic.name).is_none_or(contains_generic))
            {
                continue;
            }
            let diagnostic_count = self.diagnostics.len();
            self.check_generic_constraints(
                &template.generics,
                &subst,
                span.unwrap_or(template.item_span),
            );
            if self.diagnostics.len() != diagnostic_count {
                self.diagnostics.truncate(diagnostic_count);
                continue;
            }
            let instance_span = span.unwrap_or(template.item_span);
            let params = template
                .params
                .iter()
                .map(|ty| self.substitute_ty_normalized(ty, &subst, instance_span))
                .collect::<Vec<_>>();
            let ret = self.substitute_ty_normalized(&template.ret, &subst, instance_span);
            let concrete_interface_args = template
                .interface_args
                .iter()
                .map(|ty| self.substitute_ty_normalized(ty, &subst, instance_span))
                .collect::<Vec<_>>();
            let concrete_receiver = template
                .receiver_ty
                .as_ref()
                .map(|ty| self.substitute_ty_normalized(ty, &subst, instance_span));
            // Inference permits view weakening, but impl selection is over the
            // exact fully applied capability term.
            if !concrete_interface_args
                .iter()
                .zip(interface_args.iter())
                .all(|(candidate, required)| self.impl_term_ty_equivalent(candidate, required))
            {
                continue;
            }
            if let (Some(candidate), Some(required)) = (concrete_receiver.as_ref(), receiver_ty)
                && !self.impl_term_ty_equivalent(candidate, required)
            {
                continue;
            }
            matches.push((
                template,
                concrete_interface_args,
                concrete_receiver,
                ret,
                params,
                subst,
            ));
        }
        if matches.len() > 1 {
            let mut diagnostic = Diagnostic::new(
                span.unwrap_or(matches[0].0.item_span),
                format!("ambiguous generic impls for interface `{interface_name}`"),
            );
            for (idx, (template, _, _, _, _, _)) in matches.iter().take(3).enumerate() {
                diagnostic = diagnostic.note(format!(
                    "candidate {}: generic impl declared in module `{}`",
                    idx + 1,
                    self.ctx.resolved.modules[template.module.0].path.display()
                ));
            }
            self.diagnostics.push(diagnostic);
            return None;
        }
        if let Some((template, concrete_interface_args, concrete_receiver, ret, params, subst)) =
            matches.into_iter().next()
        {
            let mut body_subst = subst.clone();
            for (name, ty) in &template.body_subst {
                body_subst.insert(
                    name.clone(),
                    self.substitute_ty_normalized(ty, &subst, span.unwrap_or(template.item_span)),
                );
            }
            return self.instantiate_impl_body(
                template.module,
                template.body_reflection_module,
                &template.decl,
                template.interface_def,
                &template.interface_name,
                concrete_interface_args,
                concrete_receiver,
                ret,
                params,
                &body_subst,
            );
        }
        None
    }

    fn impl_term_ty_equivalent(&mut self, candidate: &Ty, required: &Ty) -> bool {
        self.meta_repr_storage_equivalent(candidate, required)
            || std_id::std_async_future_accepts_generated(&self.ctx.resolved, candidate, required)
            || std_id::std_async_future_accepts_generated(&self.ctx.resolved, required, candidate)
    }

    pub(super) fn dynamic_view_interface(
        &mut self,
        dyn_def_id: DefId,
        dyn_args: &[Ty],
        interface_def: DefId,
    ) -> Option<InterfaceRefTy> {
        self.interface_view_for_def(dyn_def_id, dyn_args)
            .positive
            .into_iter()
            .find(|entry| entry.def_id == interface_def)
    }

    pub(super) fn type_satisfies_dynamic_view(
        &mut self,
        def_id: DefId,
        _name: &str,
        args: &[Ty],
        actual: &Ty,
    ) -> bool {
        let view = self.interface_view_for_def(def_id, args);
        if let Ty::DynamicInterface {
            def_id: actual_def_id,
            args: actual_args,
            ..
        } = actual
        {
            let actual_view = self.interface_view_for_def(*actual_def_id, actual_args);
            return view
                .positive
                .iter()
                .all(|expected| actual_view.positive.contains(expected))
                && view
                    .negative
                    .iter()
                    .all(|expected| actual_view.negative.contains(expected));
        }
        let receiver_ty = receiver_ty_from_value_ty(actual);
        view.positive
            .iter()
            .all(|entry| self.type_implements_capability_ref(entry, &receiver_ty))
            && view
                .negative
                .iter()
                .all(|entry| !self.type_implements_capability_ref(entry, &receiver_ty))
    }

    pub(in crate::typeck) fn interface_view_for_def(
        &mut self,
        def_id: DefId,
        args: &[Ty],
    ) -> InterfaceView {
        self.interface_view_inner(def_id, args, &mut HashSet::new())
    }

    pub(in crate::typeck) fn constraint_bounds(
        &mut self,
        expr: &ConstraintExpr,
        subst: &HashMap<String, Ty>,
    ) -> ConstraintBounds {
        let mut bounds = ConstraintBounds::default();
        for term in &expr.terms {
            let args = term
                .args
                .iter()
                .map(|arg| self.lower_constraint_arg_with_subst(arg, subst))
                .collect::<Vec<_>>();
            let Some(def_id) = self.lookup_interface_name(&term.name) else {
                continue;
            };
            let view = self.interface_view_for_def(def_id, &args);
            if term.removed {
                bounds
                    .positive
                    .retain(|entry| !view.positive.contains(entry));
                bounds
                    .negative
                    .retain(|entry| !view.negative.contains(entry));
            } else if term.negated {
                for entry in view.positive {
                    if !bounds.negative.contains(&entry) {
                        bounds.negative.push(entry);
                    }
                }
            } else {
                for entry in view.positive {
                    if !bounds.positive.contains(&entry) {
                        bounds.positive.push(entry);
                    }
                }
                for entry in view.negative {
                    if !bounds.negative.contains(&entry) {
                        bounds.negative.push(entry);
                    }
                }
            }
        }
        bounds
    }

    pub(super) fn lower_constraint_arg_with_subst(
        &mut self,
        arg: &ConstraintArg,
        subst: &HashMap<String, Ty>,
    ) -> Ty {
        match arg {
            ConstraintArg::Type(ty) => self.lower_type_with_subst(ty, subst),
            ConstraintArg::Binding { name, .. } => subst
                .get(&name.name)
                .cloned()
                .unwrap_or_else(|| Ty::Generic(name.name.clone())),
        }
    }

    pub(in crate::typeck) fn resolve_constraint_bounds_type_holes(
        &self,
        bounds: &ConstraintBounds,
    ) -> ConstraintBounds {
        ConstraintBounds {
            positive: bounds
                .positive
                .iter()
                .map(|entry| ConstraintRef {
                    def_id: entry.def_id,
                    name: entry.name.clone(),
                    args: entry
                        .args
                        .iter()
                        .map(|arg| self.resolve_type_holes(arg))
                        .collect(),
                })
                .collect(),
            negative: bounds
                .negative
                .iter()
                .map(|entry| ConstraintRef {
                    def_id: entry.def_id,
                    name: entry.name.clone(),
                    args: entry
                        .args
                        .iter()
                        .map(|arg| self.resolve_type_holes(arg))
                        .collect(),
                })
                .collect(),
        }
    }

    pub(super) fn interface_view_inner(
        &mut self,
        def_id: DefId,
        args: &[Ty],
        expanding: &mut HashSet<DefId>,
    ) -> InterfaceView {
        if let Some(alias) = self.ctx.interface_aliases.get(&def_id).cloned() {
            if alias.generics.len() != args.len() {
                let name = self.ctx.resolved.def(def_id).name.clone();
                self.diagnostics.push(Diagnostic::new(
                    None,
                    format!(
                        "interface alias `{name}` expects {} type arguments, got {}",
                        alias.generics.len(),
                        args.len()
                    ),
                ));
                return InterfaceView::default();
            }
            if !expanding.insert(def_id) {
                return InterfaceView::default();
            }
            let subst = alias
                .generics
                .iter()
                .map(|generic| generic.name.clone())
                .zip(args.iter().cloned())
                .collect::<HashMap<_, _>>();
            let view = self.interface_view_from_expr(&alias.expr, &subst, expanding);
            expanding.remove(&def_id);
            return view;
        }
        let name = self.ctx.resolved.def(def_id).name.clone();
        InterfaceView {
            positive: vec![InterfaceRefTy {
                def_id,
                name: name.to_string(),
                args: args.to_vec(),
            }],
            negative: Vec::new(),
        }
    }

    pub(super) fn interface_view_from_expr(
        &mut self,
        expr: &InterfaceExpr,
        subst: &HashMap<String, Ty>,
        expanding: &mut HashSet<DefId>,
    ) -> InterfaceView {
        let mut view = InterfaceView::default();
        self.view_add_term(&mut view, &expr.first, subst, expanding);
        for (op, term) in &expr.rest {
            match op {
                InterfaceOp::Add => self.view_add_term(&mut view, term, subst, expanding),
                InterfaceOp::Sub => {
                    let removed = self.interface_view_for_term(term, subst, expanding);
                    view.positive
                        .retain(|entry| !removed.positive.contains(entry));
                    view.negative
                        .retain(|entry| !removed.negative.contains(entry));
                }
            }
        }
        view
    }

    pub(super) fn view_add_term(
        &mut self,
        view: &mut InterfaceView,
        term: &InterfaceTerm,
        subst: &HashMap<String, Ty>,
        expanding: &mut HashSet<DefId>,
    ) {
        let term_view = self.interface_view_for_term(term, subst, expanding);
        if term.negated {
            for entry in term_view.positive {
                if !view.negative.contains(&entry) {
                    view.negative.push(entry);
                }
            }
        } else {
            for entry in term_view.positive {
                if !view.positive.contains(&entry) {
                    view.positive.push(entry);
                }
            }
            for entry in term_view.negative {
                if !view.negative.contains(&entry) {
                    view.negative.push(entry);
                }
            }
        }
    }

    pub(super) fn interface_view_for_term(
        &mut self,
        term: &InterfaceTerm,
        subst: &HashMap<String, Ty>,
        expanding: &mut HashSet<DefId>,
    ) -> InterfaceView {
        let Some(def_id) = self.lookup_interface_name(&term.name) else {
            return InterfaceView::default();
        };
        let args = term
            .args
            .iter()
            .map(|ty| self.lower_type_with_subst(ty, subst))
            .collect::<Vec<_>>();
        self.interface_view_inner(def_id, &args, expanding)
    }

    pub(in crate::typeck) fn result_ok_err_tys(&self, ty: &Ty) -> Option<(Ty, Ty)> {
        if let Ty::OpaqueState { base, state } = ty {
            let (ok_ty, err_ty) = self.result_ok_err_tys(base)?;
            return Some((
                self.project_opaque_variant_payload_ty(ok_ty, state, "Ok", 0),
                self.project_opaque_variant_payload_ty(err_ty, state, "Err", 0),
            ));
        }
        let Ty::Named { name, args, .. } = ty else {
            return None;
        };
        if args.len() != 2 {
            return None;
        }
        let def_id = self.ctx.nominal_type_defs.get(name).copied()?;
        if !std_id::is_std_result_enum(&self.ctx.resolved, def_id) {
            return None;
        }
        let template = self.ctx.enum_templates.get(name)?;
        if !template.variants.iter().any(|variant| variant.name == "Ok")
            || !template
                .variants
                .iter()
                .any(|variant| variant.name == "Err")
        {
            return None;
        }
        Some((args[0].clone(), args[1].clone()))
    }

    pub(in crate::typeck) fn async_result_error_can_represent_runtime_failure(
        &mut self,
        err_ty: &Ty,
    ) -> bool {
        if err_ty == &std_error_ty(&self.ctx.resolved)
            || err_ty == &std_report_ty(&self.ctx.resolved)
            || err_ty == &std_async_error_ty(&self.ctx.resolved)
        {
            return true;
        }
        let Ty::Named { name, args, .. } = err_ty else {
            return false;
        };
        let Some(template) = self.ctx.enum_templates.get(name).cloned() else {
            return false;
        };
        if args.len() != template.generics.len() {
            return true;
        }
        let subst = template
            .generics
            .iter()
            .map(|generic| generic.name.clone())
            .zip(args.iter().cloned())
            .collect::<HashMap<_, _>>();
        template.variants.iter().any(|variant| {
            let payload = variant
                .payload
                .iter()
                .map(|ty| self.lower_type_with_subst(ty, &subst))
                .collect::<Vec<_>>();
            match variant.name.as_str() {
                "Runtime" => payload == [Ty::I64],
                "Async" | "TaskGroupAsync" => payload == [std_async_error_ty(&self.ctx.resolved)],
                "Resource" => payload == [std_resource_error_ty(&self.ctx.resolved)],
                _ => false,
            }
        })
    }

    pub(in crate::typeck) fn async_result_error_can_represent_message_clone_failure(
        &mut self,
        err_ty: &Ty,
    ) -> bool {
        if err_ty == &std_error_ty(&self.ctx.resolved)
            || err_ty == &std_report_ty(&self.ctx.resolved)
            || err_ty == &std_async_error_ty(&self.ctx.resolved)
        {
            return true;
        }
        let Ty::Named { name, args, .. } = err_ty else {
            return false;
        };
        let Some(template) = self.ctx.enum_templates.get(name).cloned() else {
            return false;
        };
        if args.len() != template.generics.len() {
            return true;
        }
        let subst = template
            .generics
            .iter()
            .map(|generic| generic.name.clone())
            .zip(args.iter().cloned())
            .collect::<HashMap<_, _>>();
        template.variants.iter().any(|variant| {
            let payload = variant
                .payload
                .iter()
                .map(|ty| self.lower_type_with_subst(ty, &subst))
                .collect::<Vec<_>>();
            match variant.name.as_str() {
                "MessageClone" => payload == [std_report_ty(&self.ctx.resolved)],
                "Async" | "TaskGroupAsync" => payload == [std_async_error_ty(&self.ctx.resolved)],
                _ => false,
            }
        })
    }

    pub(super) fn awaitable_ty(
        &mut self,
        ty: &Ty,
        span: crate::span::Span,
    ) -> Option<AwaitableInfo> {
        self.awaitable_output_ty(ty, span)
            .map(|output_ty| AwaitableInfo { output_ty })
    }

    pub(super) fn awaitable_output_ty(&mut self, ty: &Ty, span: crate::span::Span) -> Option<Ty> {
        if let Some(output_ty) = generated_future_output_ty(ty) {
            return Some(output_ty);
        }
        let Some(interface_def) =
            self.std_async_interface_def(STD_ASYNC_AWAITABLE_FUTURE_INTERFACE)
        else {
            return None;
        };
        let output_ty = self.capability_determined_arg(
            interface_def,
            STD_ASYNC_AWAITABLE_FUTURE_INTERFACE,
            "Awaitable::Out",
            ty,
            span,
        )?;
        self.type_implements_capability_by_def(
            interface_def,
            STD_ASYNC_AWAITABLE_FUTURE_INTERFACE,
            std::slice::from_ref(&output_ty),
            ty,
        );
        Some(output_ty)
    }

    pub(in crate::typeck) fn is_abortable_ty(&mut self, ty: &Ty) -> bool {
        let Some(interface_def) = self.std_async_interface_def(STD_ASYNC_ABORT_FUTURE_INTERFACE)
        else {
            return false;
        };
        self.type_implements_capability_by_def(
            interface_def,
            STD_ASYNC_ABORT_FUTURE_INTERFACE,
            &[],
            ty,
        )
    }

    pub(in crate::typeck) fn is_cancel_safe_ty(&mut self, ty: &Ty) -> bool {
        let Some(interface_def) = self.std_async_interface_def(STD_ASYNC_CANCEL_SAFE_INTERFACE)
        else {
            return false;
        };
        self.type_implements_capability_by_def(
            interface_def,
            STD_ASYNC_CANCEL_SAFE_INTERFACE,
            &[],
            ty,
        )
    }

    pub(super) fn selectable_future_output_ty(
        &mut self,
        ty: &Ty,
        span: crate::span::Span,
    ) -> Option<Ty> {
        let output_ty = self.awaitable_output_ty(ty, span)?;
        (self.is_cancel_safe_ty(ty) && self.is_abortable_ty(ty)).then_some(output_ty)
    }

    pub(super) fn async_function_future_ty(
        &mut self,
        def_id: DefId,
        output_ty: Ty,
        params: &[Ty],
        param_names: &[String],
    ) -> Ty {
        let affine_state = params.iter().any(|param| self.type_is_affine(param));
        let state = param_names
            .iter()
            .cloned()
            .zip(params.iter().cloned())
            .collect();
        generated_future_ty_with_state(
            format!("fn_{}", def_id.0),
            output_ty,
            self.ctx
                .async_function_cancel_safety
                .get(&def_id)
                .copied()
                .unwrap_or(false),
            self.ctx
                .async_function_abortability
                .get(&def_id)
                .copied()
                .unwrap_or(true),
            affine_state,
            state,
        )
    }

    pub(super) fn async_closure_future_ty(
        &self,
        id: ClosureInstanceId,
        output_ty: Ty,
        cancel_safe: bool,
        abortable: bool,
        affine_state: bool,
        state: Vec<(String, Ty)>,
    ) -> Ty {
        generated_future_ty_with_state(
            format!("closure_{id}"),
            output_ty,
            cancel_safe,
            abortable,
            affine_state,
            state,
        )
    }

    pub(super) fn async_sleep_future_ty(&self, output_ty: Ty) -> Ty {
        generated_future_ty(
            format!("sleep_ms_{}", mangle_ty_fragment(&output_ty)),
            output_ty,
            true,
            true,
        )
    }

    pub(super) fn capability_determined_arg(
        &mut self,
        interface_def: DefId,
        interface_name: &str,
        binding_label: &str,
        receiver_ty: &Ty,
        span: crate::span::Span,
    ) -> Option<Ty> {
        let Some(interface) = self.ctx.interfaces.get(&interface_def) else {
            return None;
        };
        if interface.determined_start.is_none() {
            return None;
        }
        let hidden_name = format!("__ciel_{interface_name}_determined");
        let hidden_names = HashSet::from([hidden_name.clone()]);
        let capability = ConstraintRef {
            def_id: interface_def,
            name: interface_name.to_string(),
            args: vec![Ty::Generic(hidden_name.clone())],
        };
        let mut receiver_candidates = vec![receiver_ty.clone()];
        let stripped_receiver_ty = strip_opaque_state_for_lookup(receiver_ty);
        if !receiver_candidates.contains(&stripped_receiver_ty) {
            receiver_candidates.push(stripped_receiver_ty);
        }

        let mut solved_receiver_ty = None;
        let mut solved_bindings = None;
        for candidate_receiver_ty in &receiver_candidates {
            let assumptions = self.hidden_solver_assumptions(candidate_receiver_ty);
            match capability_solve::solve_hidden_from_capability(
                &self.ctx,
                candidate_receiver_ty,
                &capability,
                &hidden_names,
                &assumptions,
            ) {
                capability_solve::HiddenSolveResult::Unique(bindings) => {
                    solved_receiver_ty = Some(candidate_receiver_ty.clone());
                    solved_bindings = Some(bindings);
                    break;
                }
                capability_solve::HiddenSolveResult::Ambiguous => {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!("ambiguous impls for {binding_label}"),
                    ));
                    return None;
                }
                capability_solve::HiddenSolveResult::NoSolution => {}
            }
        }
        let solved_receiver_ty = solved_receiver_ty?;
        let bindings = solved_bindings?;
        let output_ty = bindings
            .into_iter()
            .find_map(|(name, ty)| (name == hidden_name).then_some(ty))?;

        if !contains_generic(&solved_receiver_ty) && !contains_generic(&output_ty) {
            let interface_args = vec![solved_receiver_ty.clone(), output_ty.clone()];
            let _ = self.find_or_instantiate_impl_by_full_args(
                interface_def,
                interface_name,
                &interface_args,
                Some(&solved_receiver_ty),
                span,
            );
        }
        Some(output_ty)
    }

    pub(super) fn future_output_ty(&self, ty: &Ty) -> Option<Ty> {
        if let Some(output_ty) = generated_future_output_ty(ty) {
            return Some(output_ty);
        }
        std_id::std_async_future_output_arg(&self.ctx.resolved, ty).cloned()
    }

    pub(super) fn task_tys(&self, ty: &Ty) -> Option<(Ty, Ty)> {
        std_id::std_async_task_args(&self.ctx.resolved, ty)
            .map(|(output_ty, error_ty)| (output_ty.clone(), error_ty.clone()))
    }

    pub(super) fn task_tys_from_pointer_ty(&self, ty: &Ty) -> Option<(Ty, Ty)> {
        let Ty::Pointer {
            nullable: false,
            inner,
            ..
        } = ty
        else {
            return None;
        };
        self.task_tys(inner)
    }

    pub(super) fn check_task_await_boundary(&mut self, task_ty: &Ty, span: crate::span::Span) {
        let Some((task_output_ty, task_error_ty)) = self.task_tys(task_ty) else {
            return;
        };
        self.check_task_result_payload_boundary(&task_output_ty, "result", span);
        self.check_task_result_payload_boundary(&task_error_ty, "error", span);
        self.check_task_error_carriers(&task_error_ty, span);
    }

    pub(super) fn check_task_result_payload_boundary(
        &mut self,
        payload_ty: &Ty,
        label: &str,
        span: crate::span::Span,
    ) {
        if let Some(reason) =
            self.task_boundary_message_violation(payload_ty, label, &mut HashSet::new())
        {
            self.diagnostics.push(self.diagnostic_with_reason_note(
                span,
                format!("`async` task {label} type `{payload_ty}` does not implement `Message`"),
                reason,
            ));
        }
    }

    pub(super) fn check_task_error_carriers(&mut self, error_ty: &Ty, span: crate::span::Span) {
        if contains_generic(error_ty) || contains_type_hole(error_ty) {
            return;
        }
        if !self.async_result_error_can_represent_runtime_failure(error_ty) {
            self.diagnostics.push(
                Diagnostic::new(
                    span,
                    format!(
                        "`async` task error type `{error_ty}` cannot represent async runtime failures"
                    ),
                )
                .note(
                    "custom task error types need a carrier such as `Runtime(i64)`, `Async(async::AsyncError)`, or `TaskGroupAsync(async::AsyncError)`",
                ),
            );
        }
        if !self.async_result_error_can_represent_message_clone_failure(error_ty) {
            self.diagnostics.push(
                Diagnostic::new(
                    span,
                    format!(
                        "`async` task error type `{error_ty}` cannot represent task-boundary message-clone failures"
                    ),
                )
                .note(
                    "custom task error types need a carrier such as `MessageClone(Report)`, `Async(async::AsyncError)`, or `TaskGroupAsync(async::AsyncError)`",
                ),
            );
        }
    }

    pub(in crate::typeck) fn is_std_message_clone_interface_def(&self, def_id: DefId) -> bool {
        std_id::is_std_message_clone_interface(&self.ctx.resolved, def_id)
    }

    pub(in crate::typeck) fn std_async_interface_def(&self, name: &str) -> Option<DefId> {
        self.ctx
            .interfaces
            .keys()
            .copied()
            .find(|def_id| std_id::is_std_async_interface(&self.ctx.resolved, *def_id, name))
    }

    pub(in crate::typeck) fn is_std_async_spawnable_future_marker_def(
        &self,
        def_id: DefId,
    ) -> bool {
        std_id::is_std_async_interface(
            &self.ctx.resolved,
            def_id,
            STD_ASYNC_SPAWNABLE_FUTURE_INTERFACE,
        )
    }

    pub(in crate::typeck) fn is_std_message_share_handle_marker_def(&self, def_id: DefId) -> bool {
        std_id::is_std_message_interface(
            &self.ctx.resolved,
            def_id,
            STD_MESSAGE_SHARE_HANDLE_INTERFACE,
        )
    }

    pub(in crate::typeck) fn is_std_meta_ciel_fn_value_marker_def(&self, def_id: DefId) -> bool {
        std_id::is_std_meta_interface(&self.ctx.resolved, def_id, "ciel_fn_value_marker")
    }

    pub(in crate::typeck) fn is_std_meta_closure_value_marker_def(&self, def_id: DefId) -> bool {
        std_id::is_std_meta_interface(&self.ctx.resolved, def_id, "closure_value_marker")
    }

    pub(in crate::typeck) fn is_std_error_ty(&self, ty: &Ty) -> bool {
        let Ty::Named { name, args, .. } = ty else {
            return false;
        };
        if !args.is_empty() {
            return false;
        }
        self.ctx
            .nominal_type_defs
            .get(name)
            .is_some_and(|def_id| std_id::is_std_error_struct(&self.ctx.resolved, *def_id))
    }

    pub(in crate::typeck) fn is_std_report_ty(&self, ty: &Ty) -> bool {
        let Ty::Named { name, args, .. } = ty else {
            return false;
        };
        if !args.is_empty() {
            return false;
        }
        self.ctx
            .nominal_type_defs
            .get(name)
            .is_some_and(|def_id| std_id::is_std_report_struct(&self.ctx.resolved, *def_id))
    }

    pub(super) fn type_implements_std_error_trait(&mut self, ty: &Ty) -> bool {
        if self.dynamic_type_implements_std_error_trait(ty) {
            return true;
        }
        let Some(interface_def) = self.std_error_interface_def(STD_ERROR_FORMAT_INTERFACE) else {
            return false;
        };
        let receiver_ty = receiver_ty_from_value_ty(ty);
        self.type_implements_capability_by_def(
            interface_def,
            STD_ERROR_FORMAT_INTERFACE,
            &[],
            &receiver_ty,
        )
    }

    pub(super) fn dynamic_type_implements_std_error_trait(&mut self, ty: &Ty) -> bool {
        let Ty::DynamicInterface { .. } = ty else {
            return false;
        };
        let Some(alias_def) = self.std_error_interface_alias_def(STD_ERROR_TRAIT_ALIAS) else {
            return false;
        };
        self.type_satisfies_dynamic_view(alias_def, STD_ERROR_TRAIT_ALIAS, &[], ty)
    }

    pub(super) fn reject_error_erasure(
        &mut self,
        span: crate::span::Span,
        concrete_ty: &Ty,
        target_ty: &Ty,
    ) -> bool {
        if matches!(concrete_ty, Ty::Pointer { nullable: true, .. }) {
            self.diagnostics.push(
                Diagnostic::new(
                    span,
                    format!(
                        "cannot erase nullable error receiver `{concrete_ty}` into `{target_ty}`"
                    ),
                )
                .note("unwrap the nullable pointer before erasing the error"),
            );
            return true;
        }
        let receiver_ty = receiver_ty_from_value_ty(concrete_ty);
        let affine_ty = if self.type_is_affine(concrete_ty) {
            concrete_ty
        } else if self.type_is_affine(&receiver_ty) {
            &receiver_ty
        } else {
            return false;
        };
        self.diagnostics.push(
            Diagnostic::new(
                span,
                format!("cannot erase resource-affine type `{affine_ty}` into `{target_ty}`"),
            )
            .note("erasing this value would hide ownership and cleanup requirements"),
        );
        true
    }

    pub(in crate::typeck) fn is_compiler_provided_meta_marker_def(&self, def_id: DefId) -> bool {
        std_id::is_std_meta_interface(&self.ctx.resolved, def_id, "ciel_fn_value_marker")
            || std_id::is_std_meta_interface(&self.ctx.resolved, def_id, "closure_value_marker")
    }

    pub(in crate::typeck) fn is_compiler_provided_error_witness_def(&self, def_id: DefId) -> bool {
        std_id::is_std_error_interface(&self.ctx.resolved, def_id, STD_ERROR_ERASED_REF_INTERFACE)
    }

    pub(in crate::typeck) fn is_compiler_provided_interface_def(&self, def_id: DefId) -> bool {
        self.is_compiler_provided_meta_marker_def(def_id)
            || self.is_compiler_provided_error_witness_def(def_id)
    }

    pub(in crate::typeck) fn is_std_message_capability_interface_def(&self, def_id: DefId) -> bool {
        self.is_std_message_clone_interface_def(def_id)
            || self.is_std_message_share_handle_marker_def(def_id)
            || std_id::is_std_message_interface(
                &self.ctx.resolved,
                def_id,
                STD_MESSAGE_THREAD_LOCAL_INTERFACE,
            )
    }

    pub(in crate::typeck) fn is_std_message_async_frame_opt_in_marker_def(
        &self,
        def_id: DefId,
    ) -> bool {
        std_id::is_std_message_interface(
            &self.ctx.resolved,
            def_id,
            STD_MESSAGE_ASYNC_FRAME_OPT_IN_INTERFACE,
        )
    }

    pub(in crate::typeck) fn is_marker_interface_def(&self, def_id: DefId) -> bool {
        self.is_std_message_capability_interface_def(def_id)
            || self.is_std_message_async_frame_opt_in_marker_def(def_id)
            || self.is_std_async_spawnable_future_marker_def(def_id)
            || self.is_compiler_provided_meta_marker_def(def_id)
    }
}

fn strip_opaque_state_for_lookup(ty: &Ty) -> Ty {
    match ty {
        Ty::OpaqueState { base, .. } => strip_opaque_state_for_lookup(base),
        _ => ty.clone(),
    }
}
