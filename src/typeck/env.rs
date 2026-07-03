use std::collections::{HashMap, HashSet};

use crate::{
    hir::{HirProgram, Local, Module},
    resolve::{DefId, ResolvedProgram},
    thir::{CheckedEnum, CheckedImpl},
    types::{ConstraintRef, Ty},
};

use super::{
    DerivableImplTemplate, EnumTemplate, FunctionSig, GenericFunctionTemplate, GenericImplTemplate,
    ImplSig, InterfaceAliasTemplate, InterfaceSig, ReceiverSelectorSig, StructTemplate,
    TypeAliasTemplate, VariantSig,
};

#[derive(Clone, Debug)]
pub struct TyCtx {
    pub(super) resolved: ResolvedProgram,
    pub(super) hir_modules: Vec<Module>,
    pub(super) hir_locals: Vec<Local>,
    pub(super) functions_by_def: HashMap<DefId, FunctionSig>,
    pub(super) functions_by_name: HashMap<String, Vec<DefId>>,
    pub(super) receiver_selectors: Vec<ReceiverSelectorSig>,
    pub(super) type_aliases: HashMap<DefId, TypeAliasTemplate>,
    pub(super) opaque_structs: HashSet<String>,
    pub(super) unsafe_structs: HashSet<String>,
    pub(super) resource_structs: HashSet<String>,
    pub(super) structs: HashMap<String, Vec<(String, Ty)>>,
    pub(super) struct_templates: HashMap<String, StructTemplate>,
    pub(super) enum_templates: HashMap<String, EnumTemplate>,
    pub(super) nominal_type_defs: HashMap<String, DefId>,
    pub(super) variants: HashMap<DefId, VariantSig>,
    pub(super) interfaces: HashMap<DefId, InterfaceSig>,
    pub(super) interface_aliases: HashMap<DefId, InterfaceAliasTemplate>,
    pub(super) impls: Vec<ImplSig>,
    pub(super) generic_impls: Vec<GenericImplTemplate>,
    pub(super) derivable_impls: Vec<DerivableImplTemplate>,
    pub(super) generic_functions: HashMap<DefId, GenericFunctionTemplate>,
    pub(super) async_function_cancel_safety: HashMap<DefId, bool>,
    pub(super) async_function_abortability: HashMap<DefId, bool>,
    pub(super) checked_enums: HashMap<String, CheckedEnum>,
    pub(super) next_synthetic_def: usize,
    // ICT: reserved for inferred capability type dependency tracking.
    #[allow(dead_code)]
    pub(super) interface_dependencies: HashMap<DefId, Vec<ConstraintRef>>,
    // ICT: reserved for hidden capability parameter templates.
    #[allow(dead_code)]
    pub(super) hidden_param_templates: HashMap<DefId, Vec<Ty>>,
}

impl TyCtx {
    fn from_hir(hir: HirProgram) -> Self {
        let next_synthetic_def = hir.resolved.defs.len();
        Self {
            resolved: hir.resolved,
            hir_modules: hir.modules,
            hir_locals: hir.locals,
            functions_by_def: HashMap::new(),
            functions_by_name: HashMap::new(),
            receiver_selectors: Vec::new(),
            type_aliases: HashMap::new(),
            opaque_structs: HashSet::new(),
            unsafe_structs: HashSet::new(),
            resource_structs: HashSet::new(),
            structs: HashMap::new(),
            struct_templates: HashMap::new(),
            enum_templates: HashMap::new(),
            nominal_type_defs: HashMap::new(),
            variants: HashMap::new(),
            interfaces: HashMap::new(),
            interface_aliases: HashMap::new(),
            impls: Vec::new(),
            generic_impls: Vec::new(),
            derivable_impls: Vec::new(),
            generic_functions: HashMap::new(),
            async_function_cancel_safety: HashMap::new(),
            async_function_abortability: HashMap::new(),
            checked_enums: HashMap::new(),
            next_synthetic_def,
            interface_dependencies: HashMap::new(),
            hidden_param_templates: HashMap::new(),
        }
    }

    pub(super) fn ensure_next_synthetic_def_at_least(&mut self, next_synthetic_def: usize) {
        self.next_synthetic_def = self.next_synthetic_def.max(next_synthetic_def);
    }

    pub(super) fn alloc_synthetic_def(&mut self) -> DefId {
        let id = DefId(self.next_synthetic_def);
        self.next_synthetic_def += 1;
        id
    }

    pub(super) fn clone_for_generic_instance(
        &self,
        existing_impls: &[CheckedImpl],
        next_synthetic_def: usize,
    ) -> Self {
        let mut ctx = self.clone();
        ctx.retain_declared_aggregate_instances();
        ctx.retain_declared_function_sigs();
        ctx.async_function_cancel_safety.clear();
        ctx.async_function_abortability.clear();
        ctx.impls.clear();
        ctx.merge_checked_impls(existing_impls);
        ctx.ensure_next_synthetic_def_at_least(next_synthetic_def);
        ctx
    }

    fn retain_declared_aggregate_instances(&mut self) {
        let nominal_names = self
            .nominal_type_defs
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        self.structs.retain(|name, _| nominal_names.contains(name));
        self.checked_enums
            .retain(|name, _| nominal_names.contains(name));
        self.resource_structs
            .retain(|name| nominal_names.contains(name));
        self.unsafe_structs
            .retain(|name| nominal_names.contains(name));
    }

    fn retain_declared_function_sigs(&mut self) {
        let declared_def_count = self.resolved.defs.len();
        self.functions_by_def
            .retain(|def_id, _| def_id.0 < declared_def_count);
    }

    fn merge_checked_impls(&mut self, impls: &[CheckedImpl]) {
        for implementation in impls {
            if self.impls.iter().any(|existing| {
                existing.interface_def == implementation.interface_def
                    && existing.interface_args == implementation.interface_args
                    && existing.receiver_ty == implementation.receiver_ty
            }) {
                continue;
            }
            self.impls.push(ImplSig {
                interface_def: implementation.interface_def,
                interface_name: implementation.interface_name.clone(),
                interface_args: implementation.interface_args.clone(),
                receiver_ty: implementation.receiver_ty.clone(),
                function_def: implementation.function_def,
                ret: implementation.ret.clone(),
                params: implementation.params.clone(),
            });
        }
    }
}

#[derive(Debug)]
pub(super) struct TyCtxBuilder {
    ctx: TyCtx,
}

impl TyCtxBuilder {
    pub(super) fn from_hir(hir: HirProgram) -> Self {
        Self {
            ctx: TyCtx::from_hir(hir),
        }
    }

    pub(super) fn finish(self) -> TyCtx {
        self.ctx
    }
}
