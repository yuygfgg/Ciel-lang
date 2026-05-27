use crate::types::{
    ConstraintRef, Ty, is_clone_message_capability, retained_closure_capabilities,
    retained_closure_has_capability,
};

pub fn retained_closure_missing_capabilities(target_ty: &Ty, source_ty: &Ty) -> Vec<ConstraintRef> {
    retained_closure_capabilities(target_ty)
        .into_iter()
        .filter(|capability| !retained_closure_has_capability(source_ty, capability))
        .collect()
}

pub fn retained_closure_has_clone_message_capability(target_ty: &Ty) -> bool {
    retained_closure_capabilities(target_ty)
        .iter()
        .any(is_clone_message_capability)
}

pub fn retained_closure_can_forward_source_witness(
    source_ty: &Ty,
    capability: &ConstraintRef,
) -> bool {
    matches!(source_ty.unqualified(), Ty::Closure { .. })
        && retained_closure_has_capability(source_ty, capability)
}

pub fn retained_closure_can_reuse_source_witness_field(
    target_ty: &Ty,
    source_ty: &Ty,
    capability: &ConstraintRef,
) -> bool {
    retained_closure_can_forward_source_witness(source_ty, capability)
        && source_ty.unqualified() == target_ty.unqualified()
}

pub fn retained_closure_required_witnesses(target_ty: &Ty, source_ty: &Ty) -> Vec<ConstraintRef> {
    retained_closure_capabilities(target_ty)
        .into_iter()
        .filter(|capability| {
            !retained_closure_can_reuse_source_witness_field(target_ty, source_ty, capability)
        })
        .collect()
}

pub fn retained_closure_needs_wrapper(target_ty: &Ty, source_ty: &Ty) -> bool {
    matches!(source_ty.unqualified(), Ty::Closure { .. })
        && source_ty.unqualified() != target_ty.unqualified()
        && !retained_closure_capabilities(target_ty).is_empty()
}
