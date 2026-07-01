use std::collections::HashMap;

use crate::{
    hir::{Local, Module},
    resolve::ResolvedProgram,
    thir::{
        CheckedEnum, CheckedFunction, CheckedGenericFunction, CheckedImpl, CheckedInterface,
        CheckedInterfaceAlias, CheckedOpaqueStruct, CheckedStruct,
    },
    typeck::env::TyCtx,
    types::{OpaqueReturnKey, Ty},
};

#[derive(Clone, Debug)]
pub struct CheckedProgram {
    pub ty_ctx: TyCtx,
    pub resolved: ResolvedProgram,
    pub hir_modules: Vec<Module>,
    pub hir_locals: Vec<Local>,
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
