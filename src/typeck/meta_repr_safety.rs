use std::collections::HashSet;

use crate::types::{Ty, contains_generic, contains_type_hole, meta_repr_marker_name};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaReprSafetyOperation {
    Reflection,
    Reconstruction,
}

pub(crate) trait MetaReprSafetyEnv {
    fn meta_safety_type_is_affine(&mut self, ty: &Ty) -> bool;

    fn meta_safety_is_owned_policy_leaf(&mut self, ty: &Ty, root: Option<&Ty>) -> bool;

    fn meta_safety_is_unsafe_struct_instance(&mut self, name: &str, args: &[Ty]) -> bool;

    fn meta_safety_struct_fields(
        &mut self,
        instance_ty: &Ty,
        name: &str,
        args: &[Ty],
    ) -> Option<Vec<(String, Ty)>>;

    fn meta_safety_enum_payloads(
        &mut self,
        instance_ty: &Ty,
        name: &str,
        args: &[Ty],
    ) -> Option<Vec<Vec<Ty>>>;
}

pub(crate) fn owned_meta_repr_affine_message(source_ty: &Ty) -> String {
    format!("owned meta representation cannot copy resource-affine type `{source_ty}`")
}

pub(crate) fn meta_repr_unsafe_struct_message(
    operation: MetaReprSafetyOperation,
    name: &str,
) -> String {
    let operation = match operation {
        MetaReprSafetyOperation::Reflection => "meta reflection on",
        MetaReprSafetyOperation::Reconstruction => "meta reconstruction of",
    };
    format!("{operation} unsafe struct `{name}` requires unsafe block")
}

pub(crate) fn owned_meta_repr_contains_affine<E: MetaReprSafetyEnv>(
    env: &mut E,
    source_ty: &Ty,
) -> bool {
    env.meta_safety_type_is_affine(source_ty)
}

pub(crate) fn meta_structural_repr_unsafe_struct_name<E: MetaReprSafetyEnv>(
    env: &mut E,
    source_ty: &Ty,
    borrowed: bool,
) -> Option<String> {
    let root = (!borrowed).then(|| source_ty.clone());
    let mut expanding = HashSet::new();
    meta_structural_repr_unsafe_struct_name_rec(
        env,
        source_ty,
        borrowed,
        root.as_ref(),
        &mut expanding,
    )
}

fn meta_structural_repr_unsafe_struct_name_rec<E: MetaReprSafetyEnv>(
    env: &mut E,
    source_ty: &Ty,
    borrowed: bool,
    root: Option<&Ty>,
    expanding: &mut HashSet<Ty>,
) -> Option<String> {
    if contains_generic(source_ty) || contains_type_hole(source_ty) {
        return None;
    }
    match source_ty {
        Ty::Array { elem, .. } => {
            if borrowed {
                None
            } else {
                meta_owned_leaf_unsafe_struct_name(env, elem, root, expanding)
            }
        }
        Ty::Named { name, args } => {
            if let Some(marker_borrowed) = meta_repr_marker_name(name) {
                if args.len() != 1 {
                    return None;
                }
                return meta_structural_repr_unsafe_struct_name_rec(
                    env,
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
            if !borrowed && env.meta_safety_is_owned_policy_leaf(&instance_ty, root) {
                return None;
            }
            if env.meta_safety_is_unsafe_struct_instance(name, args) {
                return Some(name.clone());
            }
            if borrowed {
                return None;
            }
            if !expanding.insert(instance_ty.clone()) {
                return None;
            }
            if let Some(fields) = env.meta_safety_struct_fields(&instance_ty, name, args) {
                for (_, field_ty) in fields {
                    if let Some(name) =
                        meta_owned_leaf_unsafe_struct_name(env, &field_ty, root, expanding)
                    {
                        expanding.remove(&instance_ty);
                        return Some(name);
                    }
                }
                expanding.remove(&instance_ty);
                return None;
            }
            if let Some(variants) = env.meta_safety_enum_payloads(&instance_ty, name, args) {
                for payload in variants {
                    for payload_ty in payload {
                        if let Some(name) =
                            meta_owned_leaf_unsafe_struct_name(env, &payload_ty, root, expanding)
                        {
                            expanding.remove(&instance_ty);
                            return Some(name);
                        }
                    }
                }
            }
            expanding.remove(&instance_ty);
            None
        }
        Ty::ClosureInstance { captures, .. } => {
            if borrowed {
                return None;
            }
            for capture_ty in captures.iter().filter(|ty| !ty.is_erased_value()) {
                if let Some(name) =
                    meta_owned_leaf_unsafe_struct_name(env, capture_ty, root, expanding)
                {
                    return Some(name);
                }
            }
            None
        }
        _ => None,
    }
}

fn meta_owned_leaf_unsafe_struct_name<E: MetaReprSafetyEnv>(
    env: &mut E,
    ty: &Ty,
    root: Option<&Ty>,
    expanding: &mut HashSet<Ty>,
) -> Option<String> {
    if env.meta_safety_is_owned_policy_leaf(ty, root) {
        return None;
    }
    match ty {
        Ty::Array { elem, .. } => meta_owned_leaf_unsafe_struct_name(env, elem, root, expanding),
        Ty::Named { .. } | Ty::ClosureInstance { .. } => {
            meta_structural_repr_unsafe_struct_name_rec(env, ty, false, root, expanding)
        }
        _ => None,
    }
}
