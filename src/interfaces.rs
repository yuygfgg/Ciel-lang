use std::collections::HashMap;

use crate::{
    resolve::DefId,
    thir::{CheckedImpl, CheckedInterface, CheckedInterfaceAlias, CheckedInterfaceRef},
    types::{ConstraintBounds, ConstraintRef, Ty, receiver_ty_from_value_ty, substitute_ty},
};

#[derive(Clone, Debug)]
pub struct InterfaceSignature {
    pub ret: Ty,
    pub params: Vec<Ty>,
}

pub fn interface_by_def(
    interfaces: &[CheckedInterface],
    def_id: DefId,
) -> Option<&CheckedInterface> {
    interfaces
        .iter()
        .find(|interface| interface.def_id == def_id)
}

pub fn checked_interface_view(
    interfaces: &[CheckedInterface],
    aliases: &[CheckedInterfaceAlias],
    def_id: DefId,
    args: &[Ty],
) -> Vec<CheckedInterfaceRef> {
    if let Some(interface) = interface_by_def(interfaces, def_id) {
        return vec![CheckedInterfaceRef {
            def_id: interface.def_id,
            name: interface.name.clone(),
            args: args.to_vec(),
        }];
    }
    aliases
        .iter()
        .find(|alias| alias.def_id == def_id)
        .map(|alias| substitute_checked_alias_refs(&alias.generics, args, &alias.positive))
        .unwrap_or_default()
}

pub fn constraint_interface_view(
    aliases: &[CheckedInterfaceAlias],
    def_id: DefId,
    name: &str,
    args: &[Ty],
) -> ConstraintBounds {
    aliases
        .iter()
        .find(|alias| alias.def_id == def_id)
        .map(|alias| {
            let subst = alias_subst(&alias.generics, args);
            let positive = alias
                .positive
                .iter()
                .map(|entry| ConstraintRef {
                    def_id: entry.def_id,
                    name: entry.name.clone(),
                    args: entry
                        .args
                        .iter()
                        .map(|arg| substitute_ty(arg, &subst))
                        .collect(),
                })
                .collect();
            let negative = alias
                .negative
                .iter()
                .map(|entry| ConstraintRef {
                    def_id: entry.def_id,
                    name: entry.name.clone(),
                    args: entry
                        .args
                        .iter()
                        .map(|arg| substitute_ty(arg, &subst))
                        .collect(),
                })
                .collect();
            ConstraintBounds { positive, negative }
        })
        .unwrap_or_else(|| ConstraintBounds {
            positive: vec![ConstraintRef {
                def_id,
                name: name.to_string(),
                args: args.to_vec(),
            }],
            negative: Vec::new(),
        })
}

fn alias_subst(generics: &[String], args: &[Ty]) -> HashMap<String, Ty> {
    if generics.len() != args.len() {
        return HashMap::new();
    }
    generics.iter().cloned().zip(args.iter().cloned()).collect()
}

fn substitute_checked_alias_refs(
    generics: &[String],
    args: &[Ty],
    refs: &[CheckedInterfaceRef],
) -> Vec<CheckedInterfaceRef> {
    let subst = alias_subst(generics, args);
    refs.iter()
        .map(|entry| CheckedInterfaceRef {
            def_id: entry.def_id,
            name: entry.name.clone(),
            args: entry
                .args
                .iter()
                .map(|arg| substitute_ty(arg, &subst))
                .collect(),
        })
        .collect()
}

pub fn interface_subst(
    interface: &CheckedInterface,
    receiver_ty: Ty,
    args: &[Ty],
) -> HashMap<String, Ty> {
    let mut subst = HashMap::new();
    if let Some(receiver) = interface.generics.first() {
        subst.insert(receiver.clone(), receiver_ty);
    }
    for (generic, arg) in interface.generics.iter().skip(1).zip(args.iter()) {
        subst.insert(generic.clone(), arg.clone());
    }
    subst
}

pub fn interface_signature(
    interface: &CheckedInterface,
    receiver_subst_ty: Ty,
    receiver_param_ty: Ty,
    args: &[Ty],
) -> InterfaceSignature {
    let subst = interface_subst(interface, receiver_subst_ty, args);
    let mut params = vec![receiver_param_ty];
    params.extend(
        interface
            .params
            .iter()
            .skip(1)
            .map(|param| substitute_ty(param, &subst)),
    );
    InterfaceSignature {
        ret: substitute_ty(&interface.ret, &subst),
        params,
    }
}

pub fn retained_closure_interface_signature(
    interfaces: &[CheckedInterface],
    receiver_ty: &Ty,
    capability: &ConstraintRef,
) -> Option<InterfaceSignature> {
    let interface = interface_by_def(interfaces, capability.def_id)?;
    Some(interface_signature(
        interface,
        receiver_ty.clone(),
        Ty::pointer_to(Ty::Void),
        &capability.args,
    ))
}

pub fn dynamic_interface_signature(
    interfaces: &[CheckedInterface],
    interface_ref: &CheckedInterfaceRef,
) -> Option<InterfaceSignature> {
    let interface = interface_by_def(interfaces, interface_ref.def_id)?;
    Some(interface_signature(
        interface,
        Ty::pointer_to(Ty::Void),
        Ty::pointer_to(Ty::Void),
        &interface_ref.args,
    ))
}

pub fn impl_matches_interface_receiver(
    implementation: &CheckedImpl,
    interface_def: DefId,
    non_receiver_args: &[Ty],
    receiver_ty: &Ty,
) -> bool {
    implementation.interface_def == interface_def
        && implementation
            .receiver_ty
            .as_ref()
            .is_some_and(|receiver| ty_matches_ignoring_opaque_state(receiver, receiver_ty))
        && implementation.interface_args.get(1..).is_some_and(|args| {
            args.len() == non_receiver_args.len()
                && args
                    .iter()
                    .zip(non_receiver_args.iter())
                    .all(|(left, right)| ty_matches_ignoring_opaque_state(left, right))
        })
}

fn ty_matches_ignoring_opaque_state(left: &Ty, right: &Ty) -> bool {
    let left = match left {
        Ty::OpaqueState { base, .. } => base.as_ref(),
        _ => left,
    };
    let right = match right {
        Ty::OpaqueState { base, .. } => base.as_ref(),
        _ => right,
    };
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
                && ty_matches_ignoring_opaque_state(left_inner, right_inner)
        }
        (
            Ty::Named {
                name: left_name,
                args: left_args,
            },
            Ty::Named {
                name: right_name,
                args: right_args,
            },
        )
        | (
            Ty::DynamicInterface {
                name: left_name,
                args: left_args,
                ..
            },
            Ty::DynamicInterface {
                name: right_name,
                args: right_args,
                ..
            },
        ) => {
            left_name == right_name
                && left_args.len() == right_args.len()
                && left_args
                    .iter()
                    .zip(right_args.iter())
                    .all(|(left, right)| ty_matches_ignoring_opaque_state(left, right))
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
        ) => left_len == right_len && ty_matches_ignoring_opaque_state(left_elem, right_elem),
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
                && ty_matches_ignoring_opaque_state(left_elem, right_elem)
        }
        _ => left == right,
    }
}

pub fn impl_matches_dynamic_interface(
    implementation: &CheckedImpl,
    interface_ref: &CheckedInterfaceRef,
    concrete_ty: &Ty,
) -> bool {
    let receiver_ty = receiver_ty_from_value_ty(concrete_ty);
    impl_matches_interface_receiver(
        implementation,
        interface_ref.def_id,
        &interface_ref.args,
        &receiver_ty,
    )
}
