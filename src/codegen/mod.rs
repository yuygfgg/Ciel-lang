use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{
    ast::{BinaryOp, Literal, UnaryOp, ViewMutability},
    common::nominal_type_name,
    diagnostic::{DiagResult, Diagnostic},
    escape::EscapeProgram,
    hir::LocalId,
    interfaces::{
        checked_interface_view, dynamic_interface_signature, impl_matches_dynamic_interface,
        impl_matches_interface_receiver, retained_closure_interface_signature,
    },
    mono::MonoProgram,
    resolve::DefId,
    retained::{
        retained_closure_can_forward_source_witness,
        retained_closure_can_reuse_source_witness_field, retained_closure_needs_wrapper,
        retained_closure_required_witnesses,
    },
    source::SourceMap,
    std_id,
    thir::{
        ActorSpawnMode, AsyncFacts, AsyncFrameLocal, CheckedFunction, CheckedImpl,
        CheckedInterfaceRef, CheckedVariant, TBlock, TClosureBody, TClosureCapture, TExpr,
        TExprKind, TForInit, TPattern, TSelectArm, TStmt, TStmtKind, ThirVisitor, TryPropagation,
        walk_block, walk_expr, walk_for_init, walk_pattern, walk_stmt,
    },
    type_display::result_args,
    types::{
        ClosureInstanceId, ConstraintBounds, ConstraintRef, STD_ASYNC_AWAITABLE_FUTURE_INTERFACE,
        STD_ERROR_ERASED_REF_INTERFACE, STD_ERROR_TRAIT_ALIAS, STD_MESSAGE_CLONE_INTERFACE,
        STD_MESSAGE_SHARE_HANDLE_INTERFACE, STD_MESSAGE_THREAD_LOCAL_INTERFACE, Ty,
        aggregate_instance_name, callable_ret_params_ty, canonical_type_identity_ty,
        clone_message_capability, generated_future_output_ty, generated_future_ty_with_state,
        mangle_constraint_ref, mangle_ty_fragment, map_ty_children, meta_array_split_len,
        meta_named, meta_product_ty, meta_ref_array_repr_ty, meta_repr_borrowed_array_leaf_ty,
        meta_repr_marker_name, meta_schema_marker_name, meta_schema_product_ty, meta_schema_sum_ty,
        meta_sum_ty, named_ty, named_ty_identity_eq, receiver_ty_from_value_ty,
        retained_closure_capabilities, std_actor_ty, std_async_error_ty, std_error_code_ty,
        std_error_trait_ty, std_error_ty, std_future_ty, std_meta_repr_marker_ty_with_def_id,
        std_meta_schema_marker_ty_with_def_id, std_meta_type_id_ty, std_report_ty,
        std_resource_error_ty, std_result_ty, std_task_ty, unify_ty,
    },
};

mod async_gen;
mod c_ty;
mod closure;
mod emit;
mod expr;
mod plan;
mod state;
mod stmt;
mod support;

use c_ty::*;
use state::*;

pub fn generate_c(
    program: &MonoProgram,
    escapes: &EscapeProgram,
    source_map: &SourceMap,
) -> DiagResult<String> {
    let mut generator = CGenerator::new(program, escapes, source_map);
    generator.prepare_plan_data();
    generator.emit()
}
