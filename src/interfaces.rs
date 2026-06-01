use std::collections::HashMap;

use crate::{
    thir::{CheckedImpl, CheckedInterface, CheckedInterfaceAlias, CheckedInterfaceRef},
    types::{ConstraintBounds, ConstraintRef, Ty, receiver_ty_from_value_ty, substitute_ty},
};

#[derive(Clone, Debug)]
pub struct InterfaceSignature {
    pub ret: Ty,
    pub params: Vec<Ty>,
}

pub fn interface_by_name<'a>(
    interfaces: &'a [CheckedInterface],
    name: &str,
) -> Option<&'a CheckedInterface> {
    interfaces.iter().find(|interface| interface.name == name)
}

pub fn checked_interface_view(
    interfaces: &[CheckedInterface],
    aliases: &[CheckedInterfaceAlias],
    name: &str,
    args: &[Ty],
) -> Vec<CheckedInterfaceRef> {
    if let Some(interface) = interface_by_name(interfaces, name) {
        return vec![CheckedInterfaceRef {
            name: interface.name.clone(),
            args: args.to_vec(),
        }];
    }
    aliases
        .iter()
        .find(|alias| alias.name == name)
        .map(|alias| substitute_checked_alias_refs(&alias.generics, args, &alias.positive))
        .unwrap_or_default()
}

pub fn constraint_interface_view(
    aliases: &[CheckedInterfaceAlias],
    name: &str,
    args: &[Ty],
) -> ConstraintBounds {
    aliases
        .iter()
        .find(|alias| alias.name == name)
        .map(|alias| {
            let subst = alias_subst(&alias.generics, args);
            let positive = alias
                .positive
                .iter()
                .map(|entry| ConstraintRef {
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
    let interface = interface_by_name(interfaces, &capability.name)?;
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
    let interface = interface_by_name(interfaces, &interface_ref.name)?;
    Some(interface_signature(
        interface,
        Ty::pointer_to(Ty::Void),
        Ty::pointer_to(Ty::Void),
        &interface_ref.args,
    ))
}

pub fn impl_matches_interface_receiver(
    implementation: &CheckedImpl,
    interface_name: &str,
    non_receiver_args: &[Ty],
    receiver_ty: &Ty,
) -> bool {
    implementation.interface_name == interface_name
        && implementation
            .receiver_ty
            .as_ref()
            .is_some_and(|receiver| receiver == receiver_ty)
        && implementation.interface_args.get(1..) == Some(non_receiver_args)
}

pub fn impl_matches_dynamic_interface(
    implementation: &CheckedImpl,
    interface_ref: &CheckedInterfaceRef,
    concrete_ty: &Ty,
) -> bool {
    let receiver_ty = receiver_ty_from_value_ty(concrete_ty);
    impl_matches_interface_receiver(
        implementation,
        &interface_ref.name,
        &interface_ref.args,
        &receiver_ty,
    )
}
