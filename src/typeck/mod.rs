use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{
    capture::collect_closure_capture_ids,
    checked::CheckedProgram,
    diagnostic::{DiagResult, Diagnostic},
    hir::*,
    layout::check_checked_aggregate_layouts,
    resolve::{DefId, DefKind, ModuleId, ResolvedProgram},
    std_id,
    thir::*,
    types::{
        ConstraintBounds, ConstraintRef, META_ARRAY_EXPANSION_BUDGET, OpaqueReturnKey,
        STD_ASYNC_ABORT_FUTURE_INTERFACE, STD_ASYNC_AWAITABLE_FUTURE_INTERFACE,
        STD_ASYNC_CANCEL_SAFE_INTERFACE, STD_ERROR_FORMAT_INTERFACE,
        STD_MESSAGE_ASYNC_FRAME_OPT_IN_INTERFACE, STD_MESSAGE_CLONE_INTERFACE,
        STD_MESSAGE_SHARE_HANDLE_INTERFACE, STD_MESSAGE_THREAD_LOCAL_INTERFACE, Ty,
        aggregate_instance_name, callable_ret_params_ty, closure_instance_satisfies_signature,
        contains_any_generic_name, contains_generic, contains_type_hole,
        generated_future_output_ty, generated_future_ty, generated_future_ty_with_affine_state,
        mangle_ty_fragment, map_ty_children, meta_named, meta_product_ty, meta_ref_array_repr_ty,
        meta_repr_borrowed_array_leaf_ty, meta_repr_marker_name, meta_repr_marker_source,
        meta_schema_marker_name, meta_schema_marker_source, meta_schema_product_ty,
        meta_schema_sum_ty, meta_sum_ty, pointer_view_can_weaken, receiver_ty_from_value_ty,
        retained_closure_proves_capability, std_actor_ty, std_async_error_ty, std_error_ty,
        std_future_ty, std_meta_repr_marker_ty, std_meta_schema_marker_ty, std_receiver_ty,
        std_result_ty, std_send_permit_ty, std_sender_ty, std_task_ty,
        substitute_constraint_bounds, substitute_ty, ty_from_primitive, unify_ty,
    },
};

mod aggregate;
mod async_check;
mod capability;
mod capability_solve;
mod collect;
mod control_flow;
pub(crate) mod env;
mod expr;
mod functions;
mod helpers;
mod infer;
mod meta_repr;

use crate::common::nominal_type_name;
use capability::CapabilityTable;
use env::{TyCtx, TyCtxBuilder};
use helpers::*;

#[derive(Clone, Debug)]
struct FunctionSig {
    def_id: DefId,
    module: ModuleId,
    name: String,
    is_unsafe: bool,
    is_async: bool,
    abi: Option<String>,
    noescape: bool,
    has_body: bool,
    ret: Ty,
    params: Vec<Ty>,
    generics: Vec<GenericInfo>,
    exported: bool,
}

#[derive(Clone, Debug)]
struct ReceiverSelectorSig {
    selector: String,
    module: ModuleId,
    exported: bool,
    receiver_index: usize,
    span: crate::span::Span,
    callable: ReceiverSelectorCallable,
}

#[derive(Clone, Debug)]
enum ReceiverSelectorCallable {
    Function(DefId),
    Interface(DefId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiverAdaptation {
    Direct,
    Address,
}

struct SubstitutedTy {
    ty: Ty,
    from_replacement: bool,
}

#[derive(Clone, Debug)]
struct GenericInfo {
    name: String,
    is_resource: bool,
    is_hidden: bool,
    constraint: Option<ConstraintExpr>,
}

#[derive(Clone, Debug)]
struct StructTemplate {
    is_resource: bool,
    is_unsafe: bool,
    generics: Vec<GenericInfo>,
    fields: Vec<FieldDecl>,
}

#[derive(Clone, Debug)]
struct EnumTemplate {
    generics: Vec<GenericInfo>,
    variants: Vec<EnumVariantTemplate>,
}

#[derive(Clone, Debug)]
struct EnumVariantTemplate {
    name: String,
    payload: Vec<Type>,
}

#[derive(Clone, Debug)]
struct TypeAliasTemplate {
    generics: Vec<GenericInfo>,
    target: TypeAliasTarget,
}

#[derive(Clone, Debug)]
struct VariantSig {
    enum_name: String,
    enum_generics: Vec<String>,
    variant_index: usize,
    payload: Vec<Type>,
}

#[derive(Clone, Debug)]
struct InterfaceSig {
    def_id: DefId,
    name: String,
    is_unsafe: bool,
    generics: Vec<String>,
    determined_start: Option<usize>,
    ret: Type,
    params: Vec<Param>,
}

#[derive(Clone, Debug)]
struct InterfaceAliasTemplate {
    generics: Vec<GenericInfo>,
    expr: InterfaceExpr,
}

type InterfaceRefTy = ConstraintRef;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct InterfaceView {
    positive: Vec<InterfaceRefTy>,
    negative: Vec<InterfaceRefTy>,
}

#[derive(Clone, Debug)]
struct ImplSig {
    interface_def: DefId,
    interface_name: String,
    interface_args: Vec<Ty>,
    receiver_ty: Option<Ty>,
    function_def: DefId,
    ret: Ty,
    params: Vec<Ty>,
}

#[derive(Clone, Debug)]
struct GenericImplTemplate {
    module: ModuleId,
    item_span: crate::span::Span,
    interface_def: DefId,
    interface_name: String,
    generics: Vec<GenericInfo>,
    generic_constraints: Vec<GenericConstraintBounds>,
    interface_args: Vec<Ty>,
    receiver_ty: Option<Ty>,
    ret: Ty,
    params: Vec<Ty>,
    decl: ImplDecl,
}

#[derive(Clone, Debug)]
struct GenericConstraintBounds {
    name: String,
    is_resource: bool,
    bounds: ConstraintBounds,
}

#[derive(Clone, Debug)]
struct ImplAnalysis {
    interface_def: DefId,
    interface_name: String,
    generics: Vec<GenericInfo>,
    generic_constraints: Vec<GenericConstraintBounds>,
    interface_args: Vec<Ty>,
    receiver_ty: Option<Ty>,
    ret: Ty,
    params: Vec<Ty>,
}

#[derive(Clone, Debug)]
struct PendingImplBody {
    decl: ImplDecl,
    module: ModuleId,
    function_name: String,
    function_sig: FunctionSig,
    implementation: ImplSig,
}

#[derive(Clone, Debug)]
struct QueuedImplBody {
    pending: PendingImplBody,
    subst: HashMap<String, Ty>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CapabilityResolutionKey {
    interface_def: DefId,
    interface_name: String,
    args: Vec<Ty>,
    receiver_ty: Ty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompilerMarkerDomain {
    CielFnValue,
    ClosureValue,
}

#[derive(Clone, Debug)]
struct GenericFunctionTemplate {
    function: FunctionDecl,
    exported: bool,
}

#[derive(Clone, Debug)]
struct Binding {
    name: String,
    ty: Ty,
    narrowed_ty: Option<Ty>,
    init_state: InitState,
    mutability: BindingMutability,
    captured: bool,
    declared_loop_depth: usize,
}

struct CheckedLocalInit {
    ty: Ty,
    init: Option<TExpr>,
    assigned: bool,
}

struct CheckedFunctionBody {
    block: TBlock,
    async_facts: Option<AsyncFacts>,
}

#[derive(Clone, Debug)]
struct OpaqueReturnState {
    opaque_ty: Ty,
    concrete_ty: Option<Ty>,
    saw_recursive_concrete_ty: bool,
}

#[derive(Clone, Debug)]
struct AsyncLocalInfo {
    name: String,
    ty: Ty,
    static_const_slice: bool,
}

struct AsyncLocalUseCollector {
    locals: HashSet<LocalId>,
}

impl ThirVisitor for AsyncLocalUseCollector {
    fn visit_expr(&mut self, expr: &TExpr) {
        match &expr.kind {
            TExprKind::Local(local_id, _) => {
                self.locals.insert(*local_id);
            }
            TExprKind::Closure { captures, .. } => {
                for capture in captures {
                    self.locals.insert(capture.local_id);
                }
            }
            _ => walk_expr(self, expr),
        }
    }
}

struct AsyncAwaitValidator<'a, 'b> {
    checker: &'a mut TypeChecker,
    infos: &'b HashMap<LocalId, AsyncLocalInfo>,
    live_after: &'b HashSet<LocalId>,
}

impl ThirVisitor for AsyncAwaitValidator<'_, '_> {
    fn visit_expr(&mut self, expr: &TExpr) {
        match &expr.kind {
            TExprKind::Await { future } => {
                let mut live = self.live_after.clone();
                live.extend(TypeChecker::async_expr_used_locals(future));
                self.checker
                    .async_check_live_locals_at_await(expr.span, &live, self.infos);
                self.visit_expr(future);
            }
            TExprKind::AsyncSelect { arms, .. } => {
                let mut live = self.live_after.clone();
                for arm in arms {
                    live.extend(TypeChecker::async_expr_used_locals(&arm.future));
                    let mut body_live = TypeChecker::async_expr_used_locals(&arm.body);
                    body_live.remove(&arm.binding_local);
                    live.extend(body_live);
                }
                self.checker
                    .async_check_live_locals_at_await(expr.span, &live, self.infos);
                for arm in arms {
                    self.visit_expr(&arm.future);
                    self.visit_expr(&arm.body);
                }
            }
            TExprKind::Closure { .. } => {}
            _ => walk_expr(self, expr),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InitState {
    Unassigned,
    Assigned,
    Moved,
    MaybeAssigned,
    MaybeMoved,
}

impl InitState {
    fn from_assigned(assigned: bool) -> Self {
        if assigned {
            InitState::Assigned
        } else {
            InitState::Unassigned
        }
    }

    fn is_assigned(self) -> bool {
        matches!(self, InitState::Assigned)
    }

    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (InitState::Assigned, InitState::Assigned) => InitState::Assigned,
            (InitState::Unassigned, InitState::Unassigned) => InitState::Unassigned,
            (InitState::Moved, InitState::Moved) => InitState::Moved,
            (InitState::Assigned, InitState::Moved)
            | (InitState::Moved, InitState::Assigned)
            | (InitState::MaybeMoved, _)
            | (_, InitState::MaybeMoved) => InitState::MaybeMoved,
            _ => InitState::MaybeAssigned,
        }
    }

    fn is_moved(self) -> bool {
        matches!(self, InitState::Moved | InitState::MaybeMoved)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LvalueAccess {
    ReadOnly,
    Writable,
}

impl LvalueAccess {
    fn from_view(mutability: ViewMutability) -> Self {
        match mutability {
            ViewMutability::ReadOnly => LvalueAccess::ReadOnly,
            ViewMutability::Writable => LvalueAccess::Writable,
        }
    }

    fn pointer_ty(self, inner: Ty) -> Ty {
        match self {
            LvalueAccess::ReadOnly => Ty::const_pointer_to(inner),
            LvalueAccess::Writable => Ty::pointer_to(inner),
        }
    }

    fn is_writable(self) -> bool {
        matches!(self, LvalueAccess::Writable)
    }
}

#[derive(Clone, Debug)]
enum ReadOnlyReason {
    ImmutableBinding(String),
    CapturedBinding(String),
    ReadOnlyPointer,
    ReadOnlySlice,
}

#[derive(Clone, Debug)]
struct CheckedLvalue {
    expr: TExpr,
    access: LvalueAccess,
    read_only_reason: Option<ReadOnlyReason>,
}

impl CheckedLvalue {
    fn writable(expr: TExpr) -> Self {
        Self {
            expr,
            access: LvalueAccess::Writable,
            read_only_reason: None,
        }
    }

    fn read_only(expr: TExpr, reason: ReadOnlyReason) -> Self {
        Self {
            expr,
            access: LvalueAccess::ReadOnly,
            read_only_reason: Some(reason),
        }
    }

    fn from_view(expr: TExpr, mutability: ViewMutability, reason: ReadOnlyReason) -> Self {
        match LvalueAccess::from_view(mutability) {
            LvalueAccess::Writable => Self::writable(expr),
            LvalueAccess::ReadOnly => Self::read_only(expr, reason),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct LocalScopes {
    scopes: Vec<HashMap<LocalId, Binding>>,
}

impl LocalScopes {
    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    fn insert(&mut self, id: LocalId, mut binding: Binding) -> Result<(), String> {
        let scope = self.scopes.last_mut().expect("scope stack is not empty");
        if scope.contains_key(&id) {
            return Err(binding.name);
        }
        binding.declared_loop_depth = binding.declared_loop_depth.max(0);
        scope.insert(id, binding);
        Ok(())
    }

    fn get(&self, id: LocalId) -> Option<&Binding> {
        self.scopes.iter().rev().find_map(|scope| scope.get(&id))
    }

    fn get_mut(&mut self, id: LocalId) -> Option<&mut Binding> {
        self.scopes
            .iter_mut()
            .rev()
            .find_map(|scope| scope.get_mut(&id))
    }

    fn effective_ty(&self, id: LocalId) -> Option<Ty> {
        self.get(id).map(|binding| {
            binding
                .narrowed_ty
                .clone()
                .unwrap_or_else(|| binding.ty.clone())
        })
    }

    fn narrow_to(&mut self, id: LocalId, ty: Ty) {
        if let Some(binding) = self.get_mut(id) {
            binding.narrowed_ty = Some(ty);
        }
    }

    fn clear_narrowing(&mut self, id: LocalId) {
        if let Some(binding) = self.get_mut(id) {
            binding.narrowed_ty = None;
        }
    }

    fn mark_all_captured(&mut self) {
        for scope in &mut self.scopes {
            for binding in scope.values_mut() {
                binding.captured = true;
            }
        }
    }

    fn merge_assigned_intersection(&mut self, left: &LocalScopes, right: &LocalScopes) {
        for (scope_index, scope) in self.scopes.iter_mut().enumerate() {
            let Some(left_scope) = left.scopes.get(scope_index) else {
                continue;
            };
            let Some(right_scope) = right.scopes.get(scope_index) else {
                continue;
            };
            for (id, binding) in scope {
                let left_state = left_scope
                    .get(id)
                    .map(|binding| binding.init_state)
                    .unwrap_or(binding.init_state);
                let right_state = right_scope
                    .get(id)
                    .map(|binding| binding.init_state)
                    .unwrap_or(binding.init_state);
                binding.init_state = left_state.merge(right_state);
                let left_narrowed = left_scope
                    .get(id)
                    .and_then(|binding| binding.narrowed_ty.clone());
                let right_narrowed = right_scope
                    .get(id)
                    .and_then(|binding| binding.narrowed_ty.clone());
                binding.narrowed_ty = (left_narrowed == right_narrowed)
                    .then_some(left_narrowed)
                    .flatten();
            }
        }
    }

    fn replace_flow_from(&mut self, source: &LocalScopes) {
        for (scope_index, scope) in self.scopes.iter_mut().enumerate() {
            let Some(source_scope) = source.scopes.get(scope_index) else {
                continue;
            };
            for (id, binding) in scope {
                if let Some(source_binding) = source_scope.get(id) {
                    binding.init_state = source_binding.init_state;
                    binding.narrowed_ty = source_binding.narrowed_ty.clone();
                }
            }
        }
    }

    fn merge_reachable_flows(&mut self, flows: &[LocalScopes]) {
        let Some((first, rest)) = flows.split_first() else {
            return;
        };
        self.replace_flow_from(first);
        for flow in rest {
            let current = self.clone();
            self.merge_assigned_intersection(&current, flow);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlContextKind {
    Loop,
    Switch,
}

#[derive(Clone, Debug)]
struct ControlContext {
    kind: ControlContextKind,
    break_scopes: Vec<LocalScopes>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Flow {
    can_fallthrough: bool,
}

impl Flow {
    fn fallthrough() -> Self {
        Self {
            can_fallthrough: true,
        }
    }

    fn no_fallthrough() -> Self {
        Self {
            can_fallthrough: false,
        }
    }
}

struct CheckedBlockFlow {
    block: TBlock,
    flow: Flow,
}

struct CheckedStmtFlow {
    stmt: TStmt,
    flow: Flow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActorLifecycleOp {
    Stop,
    Join,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AsyncTaskControl {
    Cancel,
    IsFinished,
}

#[derive(Clone, Debug)]
struct AwaitableInfo {
    output_ty: Ty,
}

struct AsyncSuspensionCapabilityVisitor<'a> {
    checker: &'a mut TypeChecker,
    await_count: usize,
    abortable: bool,
}

impl ThirVisitor for AsyncSuspensionCapabilityVisitor<'_> {
    fn visit_expr(&mut self, expr: &TExpr) {
        match &expr.kind {
            TExprKind::Await { future } => {
                self.await_count += 1;
                if !self.checker.is_abortable_ty(&future.ty) {
                    self.abortable = false;
                }
                self.visit_expr(future);
            }
            TExprKind::AsyncSelect { arms, .. } => {
                self.await_count += 1;
                for arm in arms {
                    self.visit_expr(&arm.future);
                    self.visit_expr(&arm.body);
                }
            }
            TExprKind::Closure { .. } => {}
            _ => walk_expr(self, expr),
        }
    }
}

pub fn type_check(hir: HirProgram) -> DiagResult<CheckedProgram> {
    TypeChecker::for_program(hir).check()
}

pub struct CheckedGenericInstance {
    pub function: CheckedFunction,
    pub generated_functions: Vec<CheckedFunction>,
    pub impls: Vec<CheckedImpl>,
    pub opaque_returns: HashMap<OpaqueReturnKey, Ty>,
}

fn lower_ast_type_static(ty: &Type, subst: &HashMap<String, Ty>, resolved: &ResolvedProgram) -> Ty {
    match &ty.kind {
        TypeKind::Hole => Ty::Unknown,
        TypeKind::Never => Ty::Never,
        TypeKind::Void => Ty::Void,
        TypeKind::Primitive(primitive) => ty_from_primitive(primitive),
        TypeKind::Named(type_name, args) => {
            let name = match &type_name.kind {
                TypeNameKind::Def(def_id) => nominal_type_name(resolved, *def_id),
                TypeNameKind::Generic(generic) => generic.clone(),
                TypeNameKind::Error => return Ty::Unknown,
            };
            if args.is_empty()
                && let Some(replacement) = subst.get(&name)
            {
                return replacement.clone();
            }
            Ty::Named {
                name,
                args: args
                    .iter()
                    .map(|arg| lower_ast_type_static(arg, subst, resolved))
                    .collect(),
            }
        }
        TypeKind::Pointer {
            nullable,
            mutability,
            inner,
        } => Ty::Pointer {
            nullable: *nullable,
            mutability: *mutability,
            inner: Box::new(lower_ast_type_static(inner, subst, resolved)),
        },
        TypeKind::Array { len, elem } => Ty::Array {
            len: *len,
            elem: Box::new(lower_ast_type_static(elem, subst, resolved)),
        },
        TypeKind::Slice { mutability, elem } => Ty::Slice {
            mutability: *mutability,
            elem: Box::new(lower_ast_type_static(elem, subst, resolved)),
        },
        TypeKind::Function {
            is_unsafe,
            abi,
            ret,
            params,
        } => Ty::Function {
            is_unsafe: *is_unsafe,
            abi: abi.clone(),
            ret: Box::new(lower_ast_type_static(ret, subst, resolved)),
            params: params
                .iter()
                .map(|param| lower_ast_type_static(param, subst, resolved))
                .collect(),
        },
        TypeKind::Closure { ret, params, .. } => Ty::Closure {
            ret: Box::new(lower_ast_type_static(ret, subst, resolved)),
            params: params
                .iter()
                .map(|param| lower_ast_type_static(param, subst, resolved))
                .collect(),
            constraints: ConstraintBounds::default(),
        },
    }
}

fn collect_policy_marker_templates(
    modules: &[Module],
    resolved: &ResolvedProgram,
    interface_name: &str,
) -> Vec<Ty> {
    let mut templates = Vec::new();
    for module in modules {
        for item in &module.items {
            let ItemKind::Impl(decl) = &item.kind else {
                continue;
            };
            if !impl_targets_std_policy_marker(resolved, decl, interface_name)
                || !decl.args.is_empty()
                || decl
                    .generics
                    .iter()
                    .any(|generic| generic.constraint.is_some())
            {
                continue;
            }
            let Some(param) = decl.params.first() else {
                continue;
            };
            let subst = decl
                .generics
                .iter()
                .map(|generic| {
                    (
                        generic.name.name.clone(),
                        Ty::Generic(generic.name.name.clone()),
                    )
                })
                .collect::<HashMap<_, _>>();
            let receiver =
                receiver_ty_from_value_ty(&lower_ast_type_static(&param.ty, &subst, resolved));
            if contains_generic(&receiver) {
                templates.push(receiver);
            }
        }
    }
    templates
}

fn impl_targets_std_policy_marker(
    resolved: &ResolvedProgram,
    decl: &ImplDecl,
    interface_name: &str,
) -> bool {
    let NameRefKind::Def(def_id) = decl.name.kind else {
        return false;
    };
    match interface_name {
        STD_MESSAGE_SHARE_HANDLE_INTERFACE | STD_MESSAGE_THREAD_LOCAL_INTERFACE => {
            std_id::is_std_message_interface(resolved, def_id, interface_name)
        }
        _ => false,
    }
}

pub fn type_check_generic_instance(
    checked: &CheckedProgram,
    template: &CheckedGenericFunction,
    instance_args: &[Ty],
    def_id: DefId,
    instance_name: String,
    next_synthetic_def: usize,
) -> DiagResult<CheckedGenericInstance> {
    let mut checker =
        TypeChecker::for_generic_instance(&checked.ty_ctx, &checked.impls, next_synthetic_def);
    checker.opaque_returns = checked.opaque_returns.clone();
    let base_generated = checker.generated_functions.len();
    let base_impls = checker.ctx.impls.len();
    let function = checker.instantiate_generic_template_for_mono(
        template,
        instance_args,
        def_id,
        instance_name,
    );
    checker.drain_pending_impl_bodies();
    if checker.diagnostics.is_empty() {
        let function = function.ok_or_else(|| {
            vec![Diagnostic::new(
                template.function.signature.name.span,
                format!("failed to instantiate generic function `{}`", template.name),
            )]
        })?;
        let generated_functions = checker
            .generated_functions
            .drain(base_generated..)
            .collect();
        let impls = checker
            .ctx
            .impls
            .iter()
            .skip(base_impls)
            .map(|implementation| CheckedImpl {
                interface_def: implementation.interface_def,
                interface_name: implementation.interface_name.clone(),
                interface_args: implementation.interface_args.clone(),
                receiver_ty: implementation.receiver_ty.clone(),
                function_def: implementation.function_def,
                ret: implementation.ret.clone(),
                params: implementation.params.clone(),
            })
            .collect::<Vec<_>>();
        let opaque_returns = checker.opaque_returns;
        Ok(CheckedGenericInstance {
            function,
            generated_functions,
            impls,
            opaque_returns,
        })
    } else {
        Err(checker.diagnostics)
    }
}

struct TypeChecker {
    ctx: TyCtx,
    diagnostics: Vec<Diagnostic>,
    visiting_structs: HashSet<String>,
    visiting_enums: HashSet<String>,
    generated_functions: Vec<CheckedFunction>,
    pending_impl_bodies: Vec<QueuedImplBody>,
    capability_resolution_stack: HashSet<CapabilityResolutionKey>,
    fatal_impl_coherence_error: bool,
    type_subst_stack: Vec<HashMap<String, Ty>>,
    generic_env_stack: Vec<Vec<GenericInfo>>,
    opaque_returns: HashMap<OpaqueReturnKey, Ty>,
    current_opaque_return: Option<OpaqueReturnState>,
    opaque_return_probe_stack: HashSet<OpaqueReturnKey>,
    resource_generic_stack: Vec<HashSet<String>>,
    alias_expansion_stack: Vec<DefId>,
    current_module: ModuleId,
    current_return_ty: Ty,
    current_async_depth: usize,
    control_contexts: Vec<ControlContext>,
    next_closure_id: usize,
    next_type_hole_id: usize,
    type_hole_solutions: HashMap<usize, Ty>,
    current_loop_depth: usize,
    return_loop_move_depth: usize,
    unsafe_depth: usize,
    defer_meta_repr_expansion: bool,
    deferred_meta_repr_roots: Vec<Ty>,
}

impl TypeChecker {
    fn for_program(hir: HirProgram) -> Self {
        Self::with_ctx(TyCtxBuilder::from_hir(hir).finish())
    }

    fn for_generic_instance(
        ctx: &TyCtx,
        existing_impls: &[CheckedImpl],
        next_synthetic_def: usize,
    ) -> Self {
        Self::with_ctx(ctx.clone_for_generic_instance(existing_impls, next_synthetic_def))
    }

    fn with_ctx(ctx: TyCtx) -> Self {
        Self {
            ctx,
            diagnostics: Vec::new(),
            visiting_structs: HashSet::new(),
            visiting_enums: HashSet::new(),
            generated_functions: Vec::new(),
            pending_impl_bodies: Vec::new(),
            capability_resolution_stack: HashSet::new(),
            fatal_impl_coherence_error: false,
            type_subst_stack: Vec::new(),
            generic_env_stack: Vec::new(),
            opaque_returns: HashMap::new(),
            current_opaque_return: None,
            opaque_return_probe_stack: HashSet::new(),
            resource_generic_stack: Vec::new(),
            alias_expansion_stack: Vec::new(),
            current_module: ModuleId(0),
            current_return_ty: Ty::Void,
            current_async_depth: 0,
            control_contexts: Vec::new(),
            next_closure_id: 0,
            next_type_hole_id: 0,
            type_hole_solutions: HashMap::new(),
            current_loop_depth: 0,
            return_loop_move_depth: 0,
            unsafe_depth: 0,
            defer_meta_repr_expansion: false,
            deferred_meta_repr_roots: Vec::new(),
        }
    }

    fn check(mut self) -> DiagResult<CheckedProgram> {
        self.collect_interfaces();
        self.collect_type_aliases_and_opaque_structs();
        self.collect_structs();
        self.collect_enums();
        self.collect_impl_signatures();
        self.diagnostics
            .extend(capability_solve::check_determined_coherence(
                &CapabilityTable::new(&self.ctx),
            ));
        self.instantiate_declared_aggregate_instances();
        self.collect_functions();
        self.normalize_function_sigs();
        self.collect_receiver_selectors();
        self.validate_receiver_selector_conflicts();
        self.validate_c_abi_functions();
        self.check_by_value_layout_cycles();
        if self.fatal_impl_coherence_error {
            return Err(self.diagnostics);
        }

        let mut checked_functions = Vec::new();

        let mut modules = self.ctx.hir_modules.clone();
        modules.sort_by_key(|module| {
            (
                !std_id::is_std_module(&self.ctx.resolved, module.id),
                module.id.0,
            )
        });
        let mut checked_function_defs = HashSet::new();
        for opaque_pass in [true, false] {
            for module in &modules {
                for item in &module.items {
                    let ItemKind::Function(function) = &item.kind else {
                        continue;
                    };
                    let is_opaque = matches!(
                        function.signature.ret,
                        FunctionReturnType::OpaqueConstraint { .. }
                    );
                    if opaque_pass != (is_opaque && function.signature.generics.is_empty()) {
                        continue;
                    }
                    self.current_module = module.id;
                    if let Some(checked) = self.check_function_item(function, item.export)
                        && checked_function_defs.insert(checked.def_id)
                    {
                        checked_functions.push(checked);
                    }
                }
            }
        }
        for module in &modules {
            for item in &module.items {
                match &item.kind {
                    ItemKind::Function(_) => {}
                    ItemKind::ExternBlock(block) => {
                        for extern_item in &block.items {
                            if let ExternItem::Function {
                                noescape,
                                signature,
                            } = extern_item
                            {
                                let FunctionReturnType::Type(ret_ty) = &signature.ret else {
                                    continue;
                                };
                                let ret = self.lower_type(ret_ty);
                                let params = signature
                                    .params
                                    .iter()
                                    .map(|param| {
                                        (None, param.name.name.clone(), self.lower_type(&param.ty))
                                    })
                                    .collect::<Vec<_>>();
                                if let Some(sig) =
                                    self.function_sig_for(module.id, &signature.name.name)
                                {
                                    checked_functions.push(CheckedFunction {
                                        def_id: sig.def_id,
                                        name: signature.name.name.clone(),
                                        is_unsafe: sig.is_unsafe,
                                        is_async: sig.is_async,
                                        async_facts: None,
                                        abi: Some(block.abi.clone()),
                                        noescape: *noescape,
                                        exported: item.export,
                                        ret,
                                        params,
                                        body: None,
                                    });
                                }
                            }
                        }
                    }
                    ItemKind::Impl(_) | ItemKind::Interface(_) | ItemKind::InterfaceAlias(_) => {
                        // Collected and checked through the global interface/impl tables.
                    }
                    _ => {}
                }
            }
        }
        checked_functions.append(&mut self.generated_functions);
        self.drain_pending_impl_bodies();
        checked_functions.append(&mut self.generated_functions);
        self.check_by_value_layout_cycles();

        if self.diagnostics.is_empty() {
            let mut checked_structs = self
                .ctx
                .structs
                .iter()
                .map(|(name, fields)| CheckedStruct {
                    name: name.clone(),
                    is_resource: self.ctx.resource_structs.contains(name),
                    fields: fields.clone(),
                })
                .collect::<Vec<_>>();
            checked_structs.sort_by(|a, b| a.name.cmp(&b.name));
            let mut checked_opaque_structs = self
                .ctx
                .opaque_structs
                .iter()
                .cloned()
                .map(|name| CheckedOpaqueStruct { name })
                .collect::<Vec<_>>();
            checked_opaque_structs.sort_by(|a, b| a.name.cmp(&b.name));
            let mut checked_enums = self.ctx.checked_enums.values().cloned().collect::<Vec<_>>();
            checked_enums.sort_by(|a, b| a.name.cmp(&b.name));
            let mut checked_impls = self
                .ctx
                .impls
                .iter()
                .map(|implementation| CheckedImpl {
                    interface_def: implementation.interface_def,
                    interface_name: implementation.interface_name.clone(),
                    interface_args: implementation.interface_args.clone(),
                    receiver_ty: implementation.receiver_ty.clone(),
                    function_def: implementation.function_def,
                    ret: implementation.ret.clone(),
                    params: implementation.params.clone(),
                })
                .collect::<Vec<_>>();
            checked_impls.sort_by(|a, b| {
                a.interface_name
                    .cmp(&b.interface_name)
                    .then_with(|| a.function_def.0.cmp(&b.function_def.0))
            });
            let interface_values = self
                .ctx
                .interfaces
                .iter()
                .map(|(def_id, interface)| (*def_id, interface.clone()))
                .collect::<Vec<_>>();
            let mut checked_interfaces = interface_values
                .iter()
                .map(|interface| {
                    let (def_id, interface) = interface;
                    self.current_module = self.ctx.resolved.def(*def_id).module;
                    let generics = interface.generics.clone();
                    let subst = generics
                        .iter()
                        .cloned()
                        .map(|name| {
                            let generic = Ty::Generic(name.clone());
                            (name, generic)
                        })
                        .collect::<HashMap<_, _>>();
                    CheckedInterface {
                        def_id: *def_id,
                        name: interface.name.clone(),
                        is_unsafe: interface.is_unsafe,
                        generics,
                        ret: self.lower_type_with_subst(&interface.ret, &subst),
                        params: interface
                            .params
                            .iter()
                            .map(|param| self.lower_type_with_subst(&param.ty, &subst))
                            .collect(),
                    }
                })
                .collect::<Vec<_>>();
            checked_interfaces.sort_by(|a, b| a.name.cmp(&b.name));
            let alias_def_ids = self
                .ctx
                .interface_aliases
                .keys()
                .copied()
                .collect::<Vec<_>>();
            let mut checked_aliases = alias_def_ids
                .iter()
                .filter_map(|def_id| {
                    let name = self.ctx.resolved.def(*def_id).name.clone();
                    let alias = self.ctx.interface_aliases.get(def_id).cloned()?;
                    self.current_module = self.ctx.resolved.def(*def_id).module;
                    let generics = alias
                        .generics
                        .iter()
                        .map(|generic| generic.name.clone())
                        .collect::<Vec<_>>();
                    let alias_args = generics
                        .iter()
                        .cloned()
                        .map(Ty::Generic)
                        .collect::<Vec<_>>();
                    let view = self.interface_view_for_def(*def_id, &alias_args);
                    Some(CheckedInterfaceAlias {
                        def_id: *def_id,
                        name,
                        generics,
                        positive: view
                            .positive
                            .into_iter()
                            .map(|entry| CheckedInterfaceRef {
                                def_id: entry.def_id,
                                name: entry.name,
                                args: entry.args,
                            })
                            .collect(),
                        negative: view
                            .negative
                            .into_iter()
                            .map(|entry| CheckedInterfaceRef {
                                def_id: entry.def_id,
                                name: entry.name,
                                args: entry.args,
                            })
                            .collect(),
                    })
                })
                .collect::<Vec<_>>();
            checked_aliases.sort_by(|a, b| a.name.cmp(&b.name));
            let mut checked_generic_functions = self
                .ctx
                .generic_functions
                .iter()
                .filter_map(|(def_id, template)| {
                    let sig = self.ctx.functions_by_def.get(def_id)?;
                    Some(CheckedGenericFunction {
                        def_id: *def_id,
                        module: sig.module,
                        name: sig.name.clone(),
                        is_unsafe: sig.is_unsafe,
                        is_async: sig.is_async,
                        abi: sig.abi.clone(),
                        noescape: sig.noescape,
                        exported: template.exported,
                        generics: sig
                            .generics
                            .iter()
                            .map(|param| CheckedGenericParam {
                                name: param.name.clone(),
                                is_resource: param.is_resource,
                                is_hidden: param.is_hidden,
                                constraint: param.constraint.clone(),
                            })
                            .collect(),
                        ret: sig.ret.clone(),
                        params: sig.params.clone(),
                        function: template.function.clone(),
                    })
                })
                .collect::<Vec<_>>();
            checked_generic_functions.sort_by(|a, b| a.name.cmp(&b.name));
            let share_handle_templates = collect_policy_marker_templates(
                &self.ctx.hir_modules,
                &self.ctx.resolved,
                STD_MESSAGE_SHARE_HANDLE_INTERFACE,
            );
            let thread_local_templates = collect_policy_marker_templates(
                &self.ctx.hir_modules,
                &self.ctx.resolved,
                STD_MESSAGE_THREAD_LOCAL_INTERFACE,
            );
            let ty_ctx = self.ctx.clone();
            Ok(CheckedProgram {
                ty_ctx,
                resolved: self.ctx.resolved.clone(),
                hir_modules: self.ctx.hir_modules.clone(),
                hir_locals: self.ctx.hir_locals.clone(),
                share_handle_templates,
                thread_local_templates,
                opaque_structs: checked_opaque_structs,
                structs: checked_structs,
                enums: checked_enums,
                interfaces: checked_interfaces,
                interface_aliases: checked_aliases,
                impls: checked_impls,
                functions: checked_functions,
                generic_functions: checked_generic_functions,
                opaque_returns: self.opaque_returns.clone(),
            })
        } else {
            Err(self.diagnostics)
        }
    }
}
