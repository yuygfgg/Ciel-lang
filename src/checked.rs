use std::collections::HashMap;

use crate::{
    hir::{Local, Module},
    resolve::ResolvedProgram,
    span::Span,
    thir::{
        CheckedEnum, CheckedFunction, CheckedGenericFunction, CheckedImpl, CheckedInterface,
        CheckedInterfaceAlias, CheckedOpaqueStruct, CheckedStruct,
    },
    typeck::env::TyCtx,
    types::{ClosureOwnerTable, OpaqueReturnKey, Ty},
};

#[derive(Clone, Debug)]
pub struct CheckedProgram {
    pub ty_ctx: TyCtx,
    pub(crate) closure_owners: ClosureOwnerTable,
    pub resolved: ResolvedProgram,
    pub hir_modules: Vec<Module>,
    pub hir_locals: Vec<Local>,
    pub inferred_type_holes: Vec<InferredTypeHole>,
    pub share_handle_templates: Vec<Ty>,
    pub thread_local_templates: Vec<Ty>,
    pub opaque_structs: Vec<CheckedOpaqueStruct>,
    pub structs: Vec<CheckedStruct>,
    pub enums: Vec<CheckedEnum>,
    pub interfaces: Vec<CheckedInterface>,
    pub interface_aliases: Vec<CheckedInterfaceAlias>,
    pub impls: Vec<CheckedImpl>,
    pub functions: Vec<CheckedFunction>,
    pub generic_functions: Vec<CheckedGenericFunction>,
    pub opaque_returns: HashMap<OpaqueReturnKey, Ty>,
}

#[derive(Clone, Debug)]
pub struct InferredTypeHole {
    pub span: Span,
    pub local_name: String,
    pub ty: Ty,
}
