use crate::types::Ty;
use std::collections::HashSet;

pub struct AffineStructInfo {
    pub is_resource: bool,
    pub fields: Vec<Ty>,
}

pub trait AffineTypeEnv {
    fn is_resource_handle_leaf(&self, ty: &Ty) -> bool;

    fn named_type_is_async_future(&self, ty: &Ty) -> bool;

    fn opaque_return_concrete(&self, _ty: &Ty) -> Option<Ty> {
        None
    }

    fn generic_is_resource_only(&self, _name: &str) -> bool {
        false
    }

    fn named_struct_info(&mut self, _ty: &Ty) -> Option<AffineStructInfo> {
        None
    }

    fn named_enum_payloads(&mut self, _ty: &Ty) -> Option<Vec<Ty>> {
        None
    }
}

pub fn type_is_affine<E: AffineTypeEnv + ?Sized>(env: &mut E, ty: &Ty) -> bool {
    let mut visiting = HashSet::new();
    type_is_affine_inner(env, ty, &mut visiting)
}

pub fn type_is_affine_inner<E: AffineTypeEnv + ?Sized>(
    env: &mut E,
    ty: &Ty,
    visiting: &mut HashSet<Ty>,
) -> bool {
    if env.is_resource_handle_leaf(ty) {
        return true;
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
        | Ty::F64
        | Ty::CSpelling { .. }
        | Ty::Function { .. }
        | Ty::Closure { .. }
        | Ty::Pointer { .. }
        | Ty::Slice { .. }
        | Ty::DynamicInterface { .. } => false,
        Ty::OpaqueReturn { .. } => env.opaque_return_concrete(ty).is_some_and(|concrete| {
            &concrete != ty && type_is_affine_inner(env, &concrete, visiting)
        }),
        Ty::Generic(name) => env.generic_is_resource_only(name),
        Ty::Array { elem, .. } => type_is_affine_inner(env, elem, visiting),
        Ty::GeneratedFuture { .. } => true,
        Ty::OpaqueState { base, state } => {
            type_is_affine_inner(env, base, visiting)
                || state
                    .iter()
                    .any(|(_, ty)| type_is_affine_inner(env, ty, visiting))
        }
        Ty::ClosureInstance { captures, .. } => captures
            .iter()
            .any(|capture| type_is_affine_inner(env, capture, visiting)),
        Ty::Named { .. } => {
            if env.named_type_is_async_future(ty) {
                return true;
            }
            if !visiting.insert(ty.clone()) {
                return false;
            }
            if let Some(info) = env.named_struct_info(ty) {
                let affine = info.is_resource
                    || info
                        .fields
                        .iter()
                        .any(|field_ty| type_is_affine_inner(env, field_ty, visiting));
                visiting.remove(ty);
                return affine;
            }
            if let Some(payloads) = env.named_enum_payloads(ty) {
                let affine = payloads
                    .iter()
                    .any(|payload_ty| type_is_affine_inner(env, payload_ty, visiting));
                visiting.remove(ty);
                return affine;
            }
            visiting.remove(ty);
            false
        }
    }
}
