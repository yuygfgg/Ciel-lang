use std::collections::{HashMap, HashSet};

use crate::{
    capture::collect_closure_capture_ids,
    diagnostic::{DiagResult, Diagnostic},
    hir::*,
    layout::check_checked_aggregate_layouts,
    resolve::{DefId, DefKind, ModuleId, ResolvedProgram},
    std_id,
    thir::*,
    types::{
        ConstraintBounds, ConstraintRef, META_ARRAY_EXPANSION_BUDGET, STD_ERROR_FORMAT_INTERFACE,
        STD_MESSAGE_CLONE_INTERFACE, STD_MESSAGE_SHARE_HANDLE_INTERFACE,
        STD_MESSAGE_THREAD_LOCAL_INTERFACE, Ty, aggregate_instance_name, callable_ret_params_ty,
        closure_instance_satisfies_signature, closure_shape_satisfies, contains_any_generic_name,
        contains_generic, contains_type_hole, mangle_ty_fragment, meta_named, meta_product_ty,
        meta_ref_array_repr_ty, meta_repr_borrowed_array_leaf_ty, meta_repr_marker_name,
        meta_sum_ty, pointer_view_can_weaken, receiver_ty_from_value_ty,
        retained_closure_proves_capability, std_actor_ty, std_error_ty, std_future_ty,
        std_meta_repr_marker_ty, std_result_ty, substitute_ty, ty_from_primitive, unify_ty,
    },
};

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
struct GenericInfo {
    name: String,
    constraint: Option<ConstraintExpr>,
}

#[derive(Clone, Debug)]
struct StructTemplate {
    is_unsafe: bool,
    generics: Vec<String>,
    fields: Vec<FieldDecl>,
}

#[derive(Clone, Debug)]
struct EnumTemplate {
    generics: Vec<String>,
    variants: Vec<EnumVariantTemplate>,
}

#[derive(Clone, Debug)]
struct EnumVariantTemplate {
    name: String,
    payload: Vec<Type>,
}

#[derive(Clone, Debug)]
struct TypeAliasTemplate {
    generics: Vec<String>,
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
    name: String,
    is_unsafe: bool,
    generics: Vec<String>,
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
    interface_name: String,
    generics: Vec<GenericInfo>,
    interface_args: Vec<Ty>,
    receiver_ty: Option<Ty>,
    ret: Ty,
    params: Vec<Ty>,
    decl: ImplDecl,
}

#[derive(Clone, Debug)]
struct ImplAnalysis {
    interface_name: String,
    generics: Vec<GenericInfo>,
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

#[derive(Clone, Debug)]
struct AsyncLocalInfo {
    name: String,
    ty: Ty,
    static_const_slice: bool,
}

struct AsyncLocalInfoCollector<'a, 'b> {
    checker: &'a TypeChecker,
    infos: &'b mut HashMap<LocalId, AsyncLocalInfo>,
}

impl ThirVisitor for AsyncLocalInfoCollector<'_, '_> {
    fn visit_expr(&mut self, expr: &TExpr) {
        match &expr.kind {
            TExprKind::UnsafeBlock { statements, value } => {
                for stmt in statements {
                    self.checker.async_collect_local_infos_stmt(stmt, self.infos);
                }
                if let Some(value) = value {
                    self.visit_expr(value);
                }
            }
            TExprKind::Closure { .. } => {}
            _ => walk_expr(self, expr),
        }
    }
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
            TExprKind::Closure { .. } => {}
            _ => walk_expr(self, expr),
        }
    }
}

struct AsyncDeferArgFrameSafetyVisitor<'a> {
    checker: &'a mut TypeChecker,
}

impl ThirVisitor for AsyncDeferArgFrameSafetyVisitor<'_> {
    fn visit_expr(&mut self, expr: &TExpr) {
        match &expr.kind {
            TExprKind::UnsafeBlock { statements, value } => {
                for stmt in statements {
                    self.checker.async_check_defer_arg_frame_safety_stmt(stmt);
                }
                if let Some(value) = value {
                    self.visit_expr(value);
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
    MaybeAssigned,
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
            _ => InitState::MaybeAssigned,
        }
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

pub fn type_check(hir: HirProgram) -> DiagResult<CheckedProgram> {
    TypeChecker::new(hir).check()
}

pub struct CheckedGenericInstance {
    pub function: CheckedFunction,
    pub generated_functions: Vec<CheckedFunction>,
    pub impls: Vec<CheckedImpl>,
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

fn is_nominal_type_def_kind(kind: &DefKind) -> bool {
    matches!(kind, DefKind::Struct | DefKind::Enum)
}

fn nominal_type_name(resolved: &ResolvedProgram, def_id: DefId) -> String {
    let def = resolved.def(def_id);
    if !is_nominal_type_def_kind(&def.kind) {
        return def.name.clone();
    }
    let has_same_named_nominal = resolved.defs.iter().any(|other| {
        other.id != def.id && other.name == def.name && is_nominal_type_def_kind(&other.kind)
    });
    if has_same_named_nominal {
        format!("{}__def{}", def.name, def.id.0)
    } else {
        def.name.clone()
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
            if decl.name.display != interface_name
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

pub fn type_check_generic_instance(
    checked: &CheckedProgram,
    template: &CheckedGenericFunction,
    instance_args: &[Ty],
    def_id: DefId,
    instance_name: String,
    next_synthetic_def: usize,
) -> DiagResult<CheckedGenericInstance> {
    let mut checker = TypeChecker::new(HirProgram {
        resolved: checked.resolved.clone(),
        modules: checked.hir_modules.clone(),
        locals: checked.hir_locals.clone(),
    });
    checker.collect_interfaces();
    checker.collect_type_aliases_and_opaque_structs();
    checker.collect_structs();
    checker.collect_enums();
    checker.collect_functions();
    checker.merge_existing_impls(&checked.impls);
    checker.collect_impls(false);
    checker.next_synthetic_def = checker.next_synthetic_def.max(next_synthetic_def);
    let base_generated = checker.generated_functions.len();
    let base_impls = checker.impls.len();
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
            .into_iter()
            .skip(base_generated)
            .collect::<Vec<_>>();
        let impls = checker
            .impls
            .into_iter()
            .skip(base_impls)
            .map(|implementation| CheckedImpl {
                interface_name: implementation.interface_name,
                interface_args: implementation.interface_args,
                receiver_ty: implementation.receiver_ty,
                function_def: implementation.function_def,
                ret: implementation.ret,
                params: implementation.params,
            })
            .collect::<Vec<_>>();
        Ok(CheckedGenericInstance {
            function,
            generated_functions,
            impls,
        })
    } else {
        Err(checker.diagnostics)
    }
}

struct TypeChecker {
    resolved: ResolvedProgram,
    hir_modules: Vec<Module>,
    hir_locals: Vec<Local>,
    diagnostics: Vec<Diagnostic>,
    functions_by_def: HashMap<DefId, FunctionSig>,
    functions_by_name: HashMap<String, Vec<DefId>>,
    type_aliases: HashMap<DefId, TypeAliasTemplate>,
    opaque_structs: HashSet<String>,
    unsafe_structs: HashSet<String>,
    structs: HashMap<String, Vec<(String, Ty)>>,
    visiting_structs: HashSet<String>,
    struct_templates: HashMap<String, StructTemplate>,
    enum_templates: HashMap<String, EnumTemplate>,
    visiting_enums: HashSet<String>,
    variants: HashMap<DefId, VariantSig>,
    interfaces: HashMap<DefId, InterfaceSig>,
    interface_names: HashMap<String, DefId>,
    interface_aliases: HashMap<DefId, InterfaceAliasTemplate>,
    interface_alias_names: HashMap<String, DefId>,
    impls: Vec<ImplSig>,
    generic_impls: Vec<GenericImplTemplate>,
    generic_functions: HashMap<DefId, GenericFunctionTemplate>,
    generated_functions: Vec<CheckedFunction>,
    pending_impl_bodies: Vec<QueuedImplBody>,
    type_subst_stack: Vec<HashMap<String, Ty>>,
    alias_expansion_stack: Vec<DefId>,
    checked_enums: HashMap<String, CheckedEnum>,
    current_module: ModuleId,
    current_return_ty: Ty,
    current_async_depth: usize,
    control_contexts: Vec<ControlContext>,
    next_synthetic_def: usize,
    next_closure_id: usize,
    next_type_hole_id: usize,
    type_hole_solutions: HashMap<usize, Ty>,
    current_loop_depth: usize,
    unsafe_depth: usize,
    defer_meta_repr_expansion: bool,
}

impl TypeChecker {
    fn new(hir: HirProgram) -> Self {
        let next_synthetic_def = hir.resolved.defs.len();
        Self {
            resolved: hir.resolved,
            hir_modules: hir.modules,
            hir_locals: hir.locals,
            diagnostics: Vec::new(),
            functions_by_def: HashMap::new(),
            functions_by_name: HashMap::new(),
            type_aliases: HashMap::new(),
            opaque_structs: HashSet::new(),
            unsafe_structs: HashSet::new(),
            structs: HashMap::new(),
            visiting_structs: HashSet::new(),
            struct_templates: HashMap::new(),
            enum_templates: HashMap::new(),
            visiting_enums: HashSet::new(),
            variants: HashMap::new(),
            interfaces: HashMap::new(),
            interface_names: HashMap::new(),
            interface_aliases: HashMap::new(),
            interface_alias_names: HashMap::new(),
            impls: Vec::new(),
            generic_impls: Vec::new(),
            generic_functions: HashMap::new(),
            generated_functions: Vec::new(),
            pending_impl_bodies: Vec::new(),
            type_subst_stack: Vec::new(),
            alias_expansion_stack: Vec::new(),
            checked_enums: HashMap::new(),
            current_module: ModuleId(0),
            current_return_ty: Ty::Void,
            current_async_depth: 0,
            control_contexts: Vec::new(),
            next_synthetic_def,
            next_closure_id: 0,
            next_type_hole_id: 0,
            type_hole_solutions: HashMap::new(),
            current_loop_depth: 0,
            unsafe_depth: 0,
            defer_meta_repr_expansion: false,
        }
    }

    fn check(mut self) -> DiagResult<CheckedProgram> {
        self.collect_interfaces();
        self.collect_type_aliases_and_opaque_structs();
        self.collect_structs();
        self.collect_enums();
        self.collect_functions();
        self.collect_impls(true);
        self.normalize_function_sigs();
        self.validate_c_abi_functions();
        self.check_by_value_layout_cycles();

        let mut checked_functions = Vec::new();

        let modules = self.hir_modules.clone();
        for module in &modules {
            for item in &module.items {
                match &item.kind {
                    ItemKind::Function(function) => {
                        self.current_module = module.id;
                        if let Some(checked) = self.check_function_item(function, item.export) {
                            checked_functions.push(checked);
                        }
                    }
                    ItemKind::ExternBlock(block) => {
                        for extern_item in &block.items {
                            if let ExternItem::Function {
                                noescape,
                                signature,
                            } = extern_item
                            {
                                let ret = self.lower_type(&signature.ret);
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
                .structs
                .iter()
                .map(|(name, fields)| CheckedStruct {
                    name: name.clone(),
                    fields: fields.clone(),
                })
                .collect::<Vec<_>>();
            checked_structs.sort_by(|a, b| a.name.cmp(&b.name));
            let mut checked_opaque_structs = self
                .opaque_structs
                .iter()
                .cloned()
                .map(|name| CheckedOpaqueStruct { name })
                .collect::<Vec<_>>();
            checked_opaque_structs.sort_by(|a, b| a.name.cmp(&b.name));
            let mut checked_enums = self.checked_enums.values().cloned().collect::<Vec<_>>();
            checked_enums.sort_by(|a, b| a.name.cmp(&b.name));
            let mut checked_impls = self
                .impls
                .iter()
                .map(|implementation| CheckedImpl {
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
                .interfaces
                .iter()
                .map(|(def_id, interface)| (*def_id, interface.clone()))
                .collect::<Vec<_>>();
            let mut checked_interfaces = interface_values
                .iter()
                .map(|interface| {
                    let (def_id, interface) = interface;
                    self.current_module = self.resolved.def(*def_id).module;
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
            let alias_def_ids = self.interface_aliases.keys().copied().collect::<Vec<_>>();
            let mut checked_aliases = alias_def_ids
                .iter()
                .filter_map(|def_id| {
                    let name = self.resolved.def(*def_id).name.clone();
                    let alias = self.interface_aliases.get(def_id).cloned()?;
                    self.current_module = self.resolved.def(*def_id).module;
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
                    let view = self.interface_view(&name, &alias_args);
                    Some(CheckedInterfaceAlias {
                        name,
                        generics,
                        positive: view
                            .positive
                            .into_iter()
                            .map(|entry| CheckedInterfaceRef {
                                name: entry.name,
                                args: entry.args,
                            })
                            .collect(),
                        negative: view
                            .negative
                            .into_iter()
                            .map(|entry| CheckedInterfaceRef {
                                name: entry.name,
                                args: entry.args,
                            })
                            .collect(),
                    })
                })
                .collect::<Vec<_>>();
            checked_aliases.sort_by(|a, b| a.name.cmp(&b.name));
            let mut checked_generic_functions = self
                .generic_functions
                .iter()
                .filter_map(|(def_id, template)| {
                    let sig = self.functions_by_def.get(def_id)?;
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
                &self.hir_modules,
                &self.resolved,
                STD_MESSAGE_SHARE_HANDLE_INTERFACE,
            );
            let thread_local_templates = collect_policy_marker_templates(
                &self.hir_modules,
                &self.resolved,
                STD_MESSAGE_THREAD_LOCAL_INTERFACE,
            );
            Ok(CheckedProgram {
                resolved: self.resolved,
                hir_modules: self.hir_modules,
                hir_locals: self.hir_locals,
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
            })
        } else {
            Err(self.diagnostics)
        }
    }

    fn collect_interfaces(&mut self) {
        let modules = self.hir_modules.clone();
        for module in &modules {
            self.current_module = module.id;
            for item in &module.items {
                match &item.kind {
                    ItemKind::Interface(decl) => {
                        let Some(def_id) = self.resolved.local_def(
                            module.id,
                            &decl.signature.name.name,
                            &[DefKind::Interface],
                        ) else {
                            continue;
                        };
                        let generics = decl
                            .generics
                            .iter()
                            .map(|param| param.name.name.clone())
                            .collect::<Vec<_>>();
                        self.interfaces.insert(
                            def_id,
                            InterfaceSig {
                                name: decl.signature.name.name.clone(),
                                is_unsafe: decl.is_unsafe,
                                generics,
                                ret: decl.signature.ret.clone(),
                                params: decl.signature.params.clone(),
                            },
                        );
                        self.interface_names
                            .insert(decl.signature.name.name.clone(), def_id);
                    }
                    ItemKind::InterfaceAlias(decl) => {
                        let Some(def_id) = self.resolved.local_def(
                            module.id,
                            &decl.name.name,
                            &[DefKind::InterfaceAlias],
                        ) else {
                            continue;
                        };
                        for generic in &decl.generics {
                            if generic.constraint.is_some() {
                                self.diagnostics.push(Diagnostic::new(
                                    generic.name.span,
                                    "interface alias generic parameters cannot have constraints",
                                ));
                            }
                        }
                        let generics = decl
                            .generics
                            .iter()
                            .map(|param| GenericInfo {
                                name: param.name.name.clone(),
                                constraint: param.constraint.clone(),
                            })
                            .collect::<Vec<_>>();
                        self.interface_aliases.insert(
                            def_id,
                            InterfaceAliasTemplate {
                                generics,
                                expr: decl.expr.clone(),
                            },
                        );
                        self.interface_alias_names
                            .insert(decl.name.name.clone(), def_id);
                    }
                    _ => {}
                }
            }
        }
    }

    fn collect_type_aliases_and_opaque_structs(&mut self) {
        let modules = self.hir_modules.clone();
        for module in &modules {
            self.current_module = module.id;
            for item in &module.items {
                match &item.kind {
                    ItemKind::TypeAlias(decl) => {
                        let Some(def_id) = self.resolved.local_def(
                            module.id,
                            &decl.name.name,
                            &[DefKind::TypeAlias],
                        ) else {
                            continue;
                        };
                        let generics = decl
                            .generics
                            .iter()
                            .map(|param| param.name.name.clone())
                            .collect::<Vec<_>>();
                        self.type_aliases.insert(
                            def_id,
                            TypeAliasTemplate {
                                generics,
                                target: decl.target.clone(),
                            },
                        );
                    }
                    ItemKind::ExternBlock(block) => {
                        for extern_item in &block.items {
                            match extern_item {
                                ExternItem::OpaqueStruct(name) => {
                                    let Some(def_id) = self.resolved.local_def(
                                        module.id,
                                        &name.name,
                                        &[DefKind::OpaqueStruct],
                                    ) else {
                                        continue;
                                    };
                                    self.opaque_structs
                                        .insert(nominal_type_name(&self.resolved, def_id));
                                }
                                ExternItem::TypeAlias(decl) => {
                                    let Some(def_id) = self.resolved.local_def(
                                        module.id,
                                        &decl.name.name,
                                        &[DefKind::TypeAlias],
                                    ) else {
                                        continue;
                                    };
                                    let generics = decl
                                        .generics
                                        .iter()
                                        .map(|param| param.name.name.clone())
                                        .collect::<Vec<_>>();
                                    self.type_aliases.insert(
                                        def_id,
                                        TypeAliasTemplate {
                                            generics,
                                            target: decl.target.clone(),
                                        },
                                    );
                                }
                                ExternItem::Function { .. } => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn check_by_value_layout_cycles(&mut self) {
        let structs = self
            .structs
            .iter()
            .map(|(name, fields)| CheckedStruct {
                name: name.clone(),
                fields: fields.clone(),
            })
            .collect::<Vec<_>>();
        let enums = self.checked_enums.values().cloned().collect::<Vec<_>>();
        self.diagnostics
            .extend(check_checked_aggregate_layouts(&structs, &enums));
    }

    fn collect_structs(&mut self) {
        let modules = self.hir_modules.clone();
        for module in &modules {
            self.current_module = module.id;
            for item in &module.items {
                if let ItemKind::Struct(decl) = &item.kind {
                    let Some(def_id) =
                        self.resolved
                            .local_def(module.id, &decl.name.name, &[DefKind::Struct])
                    else {
                        continue;
                    };
                    let nominal_name = nominal_type_name(&self.resolved, def_id);
                    if decl.generics.is_empty() {
                        let previous_defer_meta_repr_expansion =
                            std::mem::replace(&mut self.defer_meta_repr_expansion, true);
                        let fields = decl
                            .fields
                            .iter()
                            .map(|field| {
                                let ty = self.lower_type(&field.ty);
                                self.reject_invalid_plain_value_type(
                                    &ty,
                                    field.ty.span,
                                    "struct field",
                                );
                                (field.name.name.clone(), ty)
                            })
                            .collect::<Vec<_>>();
                        self.defer_meta_repr_expansion = previous_defer_meta_repr_expansion;
                        self.structs.insert(nominal_name.clone(), fields);
                        if decl.is_unsafe {
                            self.unsafe_structs.insert(nominal_name);
                        }
                        continue;
                    }
                    let generics = decl
                        .generics
                        .iter()
                        .map(|param| param.name.name.clone())
                        .collect::<Vec<_>>();
                    self.struct_templates.insert(
                        nominal_name,
                        StructTemplate {
                            is_unsafe: decl.is_unsafe,
                            generics,
                            fields: decl.fields.clone(),
                        },
                    );
                }
            }
        }
    }

    fn lower_type_preserving_meta_repr_markers(
        &mut self,
        ty: &Type,
        subst: &HashMap<String, Ty>,
    ) -> Ty {
        self.lower_type_with_subst_preserving_meta_repr_markers(ty, subst, false)
    }

    fn lower_type_with_subst_preserving_meta_repr_markers(
        &mut self,
        ty: &Type,
        subst: &HashMap<String, Ty>,
        allow_holes: bool,
    ) -> Ty {
        let previous_defer_meta_repr_expansion =
            std::mem::replace(&mut self.defer_meta_repr_expansion, true);
        let lowered = self.lower_type_with_subst_inner(ty, subst, allow_holes);
        self.defer_meta_repr_expansion = previous_defer_meta_repr_expansion;
        lowered
    }

    fn collect_enums(&mut self) {
        let modules = self.hir_modules.clone();
        for module in &modules {
            self.current_module = module.id;
            for item in &module.items {
                if let ItemKind::Enum(decl) = &item.kind {
                    let Some(enum_def_id) =
                        self.resolved
                            .local_def(module.id, &decl.name.name, &[DefKind::Enum])
                    else {
                        continue;
                    };
                    let enum_name = nominal_type_name(&self.resolved, enum_def_id);
                    let generics = decl
                        .generics
                        .iter()
                        .map(|param| param.name.name.clone())
                        .collect::<Vec<_>>();
                    let variants = decl
                        .variants
                        .iter()
                        .map(|variant| EnumVariantTemplate {
                            name: variant.name.name.clone(),
                            payload: variant.payload.clone(),
                        })
                        .collect::<Vec<_>>();
                    for (variant_index, variant) in decl.variants.iter().enumerate() {
                        let Some(def_id) = self.resolved.local_def(
                            module.id,
                            &variant.name.name,
                            &[DefKind::EnumVariant],
                        ) else {
                            continue;
                        };
                        self.variants.insert(
                            def_id,
                            VariantSig {
                                enum_name: enum_name.clone(),
                                enum_generics: generics.clone(),
                                variant_index,
                                payload: variant.payload.clone(),
                            },
                        );
                    }
                    self.enum_templates
                        .insert(enum_name.clone(), EnumTemplate { generics, variants });
                    if decl.generics.is_empty() {
                        self.ensure_enum_instance(&Ty::Named {
                            name: enum_name,
                            args: Vec::new(),
                        });
                    }
                }
            }
        }
    }

    fn lower_type(&mut self, ty: &Type) -> Ty {
        let subst = self.current_type_subst();
        let lowered = self.lower_type_with_subst_inner(ty, &subst, false);
        self.ensure_enum_instance(&lowered);
        self.ensure_struct_instance(&lowered);
        lowered
    }

    fn lower_type_allowing_holes(&mut self, ty: &Type) -> Ty {
        let subst = self.current_type_subst();
        let lowered = self.lower_type_with_subst_inner(ty, &subst, true);
        if !contains_type_hole(&lowered) {
            self.ensure_enum_instance(&lowered);
            self.ensure_struct_instance(&lowered);
        }
        lowered
    }

    fn lower_type_with_subst(&mut self, ty: &Type, subst: &HashMap<String, Ty>) -> Ty {
        self.lower_type_with_subst_inner(ty, subst, false)
    }

    fn lower_type_with_subst_allowing_holes(
        &mut self,
        ty: &Type,
        subst: &HashMap<String, Ty>,
    ) -> Ty {
        self.lower_type_with_subst_inner(ty, subst, true)
    }

    fn lower_type_with_subst_inner(
        &mut self,
        ty: &Type,
        subst: &HashMap<String, Ty>,
        allow_holes: bool,
    ) -> Ty {
        let lowered = match &ty.kind {
            TypeKind::Hole => {
                if allow_holes {
                    let id = self.next_type_hole_id;
                    self.next_type_hole_id += 1;
                    Ty::Hole(id)
                } else {
                    self.diagnostics.push(Diagnostic::new(
                        ty.span,
                        "type hole is only allowed in initialized local declarations",
                    ));
                    Ty::Unknown
                }
            }
            TypeKind::Never => Ty::Never,
            TypeKind::Void => Ty::Void,
            TypeKind::Primitive(primitive) => ty_from_primitive(primitive),
            TypeKind::Named(name, args) => {
                let display_name = &name.display;
                if args.is_empty()
                    && let TypeNameKind::Generic(generic_name) = &name.kind
                    && let Some(replacement) = subst.get(generic_name)
                {
                    replacement.clone()
                } else if let TypeNameKind::Def(def_id) = &name.kind {
                    let def_id = *def_id;
                    let def = self.resolved.def(def_id).clone();
                    if let Some(normalized) =
                        self.lower_std_meta_repr_type(ty.span, def_id, args, subst, allow_holes)
                    {
                        normalized
                    } else if def.kind == DefKind::TypeAlias {
                        self.expand_type_alias(ty.span, def_id, args, subst, allow_holes)
                    } else if let Some(interface) = self.interfaces.get(&def_id).cloned() {
                        let required_args = interface.generics.len().saturating_sub(1);
                        if args.len() != required_args {
                            self.diagnostics.push(Diagnostic::new(
                                ty.span,
                                format!(
                                    "dynamic interface `{}` requires {required_args} non-receiver type arguments",
                                    display_name
                                ),
                            ));
                        }
                        if !interface_receiver_is_input(&interface) {
                            self.diagnostics.push(Diagnostic::new(
                                ty.span,
                                format!(
                                    "interface `{}` cannot be used dynamically because its receiver is not an input parameter",
                                    display_name
                                ),
                            ));
                        }
                        Ty::DynamicInterface {
                            name: def.name,
                            args: args
                                .iter()
                                .map(|arg| {
                                    self.lower_type_with_subst_inner(arg, subst, allow_holes)
                                })
                                .collect(),
                        }
                    } else if self.interface_aliases.contains_key(&def_id) {
                        let alias_args = args
                            .iter()
                            .map(|arg| self.lower_type_with_subst_inner(arg, subst, allow_holes))
                            .collect::<Vec<_>>();
                        let view = self.interface_view(&def.name, &alias_args);
                        for entry in view.positive.iter().chain(view.negative.iter()) {
                            if let Some(interface) =
                                self.interface_sig_by_name(&entry.name).cloned()
                            {
                                let required_args = interface.generics.len().saturating_sub(1);
                                if entry.args.len() != required_args {
                                    self.diagnostics.push(Diagnostic::new(
                                        ty.span,
                                        format!(
                                            "dynamic interface alias `{}` leaves `{}` without {required_args} non-receiver type arguments",
                                            display_name, entry.name
                                        ),
                                    ));
                                }
                                if !interface_receiver_is_input(&interface) {
                                    self.diagnostics.push(Diagnostic::new(
                                        ty.span,
                                        format!(
                                            "interface alias `{}` cannot be used dynamically because `{}` has no input receiver",
                                            display_name, entry.name
                                        ),
                                    ));
                                }
                            }
                        }
                        Ty::DynamicInterface {
                            name: def.name,
                            args: alias_args,
                        }
                    } else {
                        let nominal_name = nominal_type_name(&self.resolved, def_id);
                        let preserved_args = args
                            .iter()
                            .map(|arg| {
                                self.lower_type_with_subst_preserving_meta_repr_markers(
                                    arg,
                                    subst,
                                    allow_holes,
                                )
                            })
                            .collect::<Vec<_>>();
                        let preserved_candidate = Ty::Named {
                            name: nominal_name.clone(),
                            args: preserved_args,
                        };
                        if self.type_implements_share_handle(&preserved_candidate) {
                            return preserved_candidate;
                        }
                        Ty::Named {
                            name: nominal_name,
                            args: args
                                .iter()
                                .map(|arg| {
                                    self.lower_type_with_subst_inner(arg, subst, allow_holes)
                                })
                                .collect(),
                        }
                    }
                } else if args.is_empty()
                    && let TypeNameKind::Generic(generic_name) = &name.kind
                {
                    Ty::Generic(generic_name.clone())
                } else {
                    self.diagnostics.push(Diagnostic::new(
                        ty.span,
                        format!("unknown type `{display_name}`"),
                    ));
                    Ty::Unknown
                }
            }
            TypeKind::Pointer {
                nullable,
                mutability,
                inner,
            } => Ty::Pointer {
                nullable: *nullable,
                mutability: *mutability,
                inner: Box::new(self.lower_type_with_subst_inner(inner, subst, allow_holes)),
            },
            TypeKind::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(self.lower_type_with_subst_inner(elem, subst, allow_holes)),
            },
            TypeKind::Slice { mutability, elem } => Ty::Slice {
                mutability: *mutability,
                elem: Box::new(self.lower_type_with_subst_inner(elem, subst, allow_holes)),
            },
            TypeKind::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => Ty::Function {
                is_unsafe: *is_unsafe,
                abi: abi.clone(),
                ret: Box::new(self.lower_type_with_subst_inner(ret, subst, allow_holes)),
                params: params
                    .iter()
                    .map(|param| self.lower_type_with_subst_inner(param, subst, allow_holes))
                    .collect(),
            },
            TypeKind::Closure {
                ret,
                params,
                constraint,
            } => Ty::Closure {
                ret: Box::new(self.lower_type_with_subst_inner(ret, subst, allow_holes)),
                params: params
                    .iter()
                    .map(|param| self.lower_type_with_subst_inner(param, subst, allow_holes))
                    .collect(),
                constraints: constraint
                    .as_ref()
                    .map(|constraint| self.constraint_bounds(constraint, subst))
                    .unwrap_or_default(),
            },
        };
        let lowered = self.normalize_meta_repr_markers(&lowered, ty.span);
        if !contains_type_hole(&lowered) {
            self.ensure_enum_instance(&lowered);
            self.ensure_struct_instance(&lowered);
        }
        lowered
    }

    fn current_type_subst(&self) -> HashMap<String, Ty> {
        self.type_subst_stack.last().cloned().unwrap_or_default()
    }

    fn resolve_type_holes(&self, ty: &Ty) -> Ty {
        match ty {
            Ty::Hole(id) => self
                .type_hole_solutions
                .get(id)
                .map(|solution| self.resolve_type_holes(solution))
                .unwrap_or(Ty::Hole(*id)),
            Ty::Pointer {
                nullable,
                mutability,
                inner,
            } => Ty::Pointer {
                nullable: *nullable,
                mutability: *mutability,
                inner: Box::new(self.resolve_type_holes(inner)),
            },
            Ty::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(self.resolve_type_holes(elem)),
            },
            Ty::Slice { mutability, elem } => Ty::Slice {
                mutability: *mutability,
                elem: Box::new(self.resolve_type_holes(elem)),
            },
            Ty::Named { name, args } => Ty::Named {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.resolve_type_holes(arg))
                    .collect(),
            },
            Ty::DynamicInterface { name, args } => Ty::DynamicInterface {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.resolve_type_holes(arg))
                    .collect(),
            },
            Ty::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => Ty::Function {
                is_unsafe: *is_unsafe,
                abi: abi.clone(),
                ret: Box::new(self.resolve_type_holes(ret)),
                params: params
                    .iter()
                    .map(|param| self.resolve_type_holes(param))
                    .collect(),
            },
            Ty::Closure {
                ret,
                params,
                constraints,
            } => Ty::Closure {
                ret: Box::new(self.resolve_type_holes(ret)),
                params: params
                    .iter()
                    .map(|param| self.resolve_type_holes(param))
                    .collect(),
                constraints: self.resolve_constraint_bounds_type_holes(constraints),
            },
            Ty::ClosureInstance {
                id,
                ret,
                params,
                captures,
            } => Ty::ClosureInstance {
                id: *id,
                ret: Box::new(self.resolve_type_holes(ret)),
                params: params
                    .iter()
                    .map(|param| self.resolve_type_holes(param))
                    .collect(),
                captures: captures
                    .iter()
                    .map(|capture| self.resolve_type_holes(capture))
                    .collect(),
            },
            other => other.clone(),
        }
    }

    fn bind_type_hole(&mut self, id: usize, ty: &Ty) -> bool {
        let ty = self.resolve_type_holes(ty);
        if matches!(ty, Ty::Hole(other) if other == id) || matches!(ty, Ty::Unknown) {
            return true;
        }
        if let Some(existing) = self.type_hole_solutions.get(&id).cloned() {
            return self.unify_type_holes(&existing, &ty);
        }
        self.type_hole_solutions.insert(id, ty);
        true
    }

    fn unify_type_holes(&mut self, expected: &Ty, actual: &Ty) -> bool {
        let expected = self.resolve_type_holes(expected);
        let actual = self.resolve_type_holes(actual);
        if self.meta_repr_marker_matches_concrete(&expected, &actual)
            || self.meta_repr_marker_matches_concrete(&actual, &expected)
        {
            return true;
        }
        match (&expected, &actual) {
            (Ty::Hole(id), _) => self.bind_type_hole(*id, &actual),
            (_, Ty::Hole(id)) => self.bind_type_hole(*id, &expected),
            (
                Ty::Pointer {
                    nullable,
                    mutability,
                    inner: expected_inner,
                },
                Ty::Pointer {
                    nullable: actual_nullable,
                    mutability: actual_mutability,
                    inner: actual_inner,
                },
            ) if nullable == actual_nullable && mutability == actual_mutability => {
                self.unify_type_holes(expected_inner, actual_inner)
            }
            (
                Ty::Array {
                    len,
                    elem: expected_elem,
                },
                Ty::Array {
                    len: actual_len,
                    elem: actual_elem,
                },
            ) if len == actual_len => self.unify_type_holes(expected_elem, actual_elem),
            (
                Ty::Slice {
                    mutability,
                    elem: expected_elem,
                },
                Ty::Slice {
                    mutability: actual_mutability,
                    elem: actual_elem,
                },
            ) if mutability == actual_mutability => {
                self.unify_type_holes(expected_elem, actual_elem)
            }
            (
                Ty::Slice {
                    elem: expected_elem,
                    ..
                },
                Ty::Array {
                    elem: actual_elem, ..
                },
            ) => self.unify_type_holes(expected_elem, actual_elem),
            (
                Ty::Named { name, args },
                Ty::Named {
                    name: actual_name,
                    args: actual_args,
                },
            ) if name == actual_name && args.len() == actual_args.len() => args
                .iter()
                .zip(actual_args.iter())
                .all(|(expected, actual)| self.unify_type_holes(expected, actual)),
            (
                Ty::DynamicInterface { name, args },
                Ty::DynamicInterface {
                    name: actual_name,
                    args: actual_args,
                },
            ) if name == actual_name && args.len() == actual_args.len() => args
                .iter()
                .zip(actual_args.iter())
                .all(|(expected, actual)| self.unify_type_holes(expected, actual)),
            (
                Ty::Function {
                    is_unsafe,
                    abi,
                    ret,
                    params,
                },
                Ty::Function {
                    is_unsafe: actual_is_unsafe,
                    abi: actual_abi,
                    ret: actual_ret,
                    params: actual_params,
                },
            ) if is_unsafe == actual_is_unsafe
                && abi == actual_abi
                && params.len() == actual_params.len() =>
            {
                self.unify_type_holes(ret, actual_ret)
                    && params
                        .iter()
                        .zip(actual_params.iter())
                        .all(|(expected, actual)| self.unify_type_holes(expected, actual))
            }
            (
                Ty::Closure { ret, params, .. },
                Ty::Closure {
                    ret: actual_ret,
                    params: actual_params,
                    ..
                },
            )
            | (
                Ty::Closure { ret, params, .. },
                Ty::ClosureInstance {
                    ret: actual_ret,
                    params: actual_params,
                    ..
                },
            ) if params.len() == actual_params.len() => {
                self.unify_type_holes(ret, actual_ret)
                    && params
                        .iter()
                        .zip(actual_params.iter())
                        .all(|(expected, actual)| self.unify_type_holes(expected, actual))
            }
            (
                Ty::ClosureInstance {
                    id,
                    ret,
                    params,
                    captures,
                },
                Ty::ClosureInstance {
                    id: actual_id,
                    ret: actual_ret,
                    params: actual_params,
                    captures: actual_captures,
                },
            ) if id == actual_id
                && params.len() == actual_params.len()
                && captures.len() == actual_captures.len() =>
            {
                self.unify_type_holes(ret, actual_ret)
                    && params
                        .iter()
                        .zip(actual_params.iter())
                        .all(|(expected, actual)| self.unify_type_holes(expected, actual))
                    && captures
                        .iter()
                        .zip(actual_captures.iter())
                        .all(|(expected, actual)| self.unify_type_holes(expected, actual))
            }
            _ => expected == actual,
        }
    }

    fn unify_ty_for_inference(
        &mut self,
        pattern: &Ty,
        actual: &Ty,
        subst: &mut HashMap<String, Ty>,
    ) -> bool {
        let pattern = self.substitute_ty_normalized_silent(pattern, subst);
        let pattern = self.resolve_type_holes(&pattern);
        let actual = self.resolve_type_holes(actual);
        if self.meta_repr_marker_matches_concrete(&pattern, &actual) {
            return true;
        }
        if let Ty::Generic(name) = &pattern
            && let Some(existing) = subst.get(name).cloned()
            && self.meta_repr_marker_matches_concrete(&existing, &actual)
        {
            return true;
        }
        match &pattern {
            Ty::Hole(id) => self.bind_type_hole(*id, &actual),
            Ty::Generic(name) => match subst.get(name).cloned() {
                Some(Ty::Generic(existing)) if existing == *name => {
                    subst.insert(name.clone(), actual);
                    true
                }
                Some(existing) => {
                    let ok = self.unify_ty_for_inference(&existing, &actual, subst);
                    if ok {
                        subst.insert(name.clone(), self.resolve_type_holes(&existing));
                    }
                    ok
                }
                None => {
                    subst.insert(name.clone(), actual);
                    true
                }
            },
            Ty::Pointer {
                nullable,
                mutability,
                inner: pattern_inner,
            } => match &actual {
                Ty::Pointer {
                    nullable: actual_nullable,
                    mutability: actual_mutability,
                    inner: actual_inner,
                } if (*nullable || !*actual_nullable)
                    && pointer_view_can_weaken(*mutability, *actual_mutability) =>
                {
                    self.unify_ty_for_inference(pattern_inner, actual_inner, subst)
                }
                _ => false,
            },
            Ty::Array {
                len,
                elem: pattern_elem,
            } => match &actual {
                Ty::Array {
                    len: actual_len,
                    elem: actual_elem,
                } if len == actual_len => {
                    self.unify_ty_for_inference(pattern_elem, actual_elem, subst)
                }
                _ => false,
            },
            Ty::Slice {
                mutability,
                elem: pattern_elem,
            } => match &actual {
                Ty::Slice {
                    mutability: actual_mutability,
                    elem: actual_elem,
                } if pointer_view_can_weaken(*mutability, *actual_mutability) => {
                    self.unify_ty_for_inference(pattern_elem, actual_elem, subst)
                }
                Ty::Array {
                    elem: actual_elem, ..
                } => self.unify_ty_for_inference(pattern_elem, actual_elem, subst),
                _ => false,
            },
            Ty::Named { name, args } => match &actual {
                Ty::Named {
                    name: actual_name,
                    args: actual_args,
                } if name == actual_name && args.len() == actual_args.len() => args
                    .iter()
                    .zip(actual_args.iter())
                    .all(|(pattern, actual)| self.unify_ty_for_inference(pattern, actual, subst)),
                _ => false,
            },
            Ty::DynamicInterface { name, args } => match &actual {
                Ty::DynamicInterface {
                    name: actual_name,
                    args: actual_args,
                } if name == actual_name && args.len() == actual_args.len() => args
                    .iter()
                    .zip(actual_args.iter())
                    .all(|(pattern, actual)| self.unify_ty_for_inference(pattern, actual, subst)),
                _ => false,
            },
            Ty::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => match &actual {
                Ty::Function {
                    is_unsafe: actual_is_unsafe,
                    abi: actual_abi,
                    ret: actual_ret,
                    params: actual_params,
                } if is_unsafe == actual_is_unsafe
                    && abi == actual_abi
                    && params.len() == actual_params.len() =>
                {
                    self.unify_ty_for_inference(ret, actual_ret, subst)
                        && params
                            .iter()
                            .zip(actual_params.iter())
                            .all(|(pattern, actual)| {
                                self.unify_ty_for_inference(pattern, actual, subst)
                            })
                }
                _ => false,
            },
            Ty::Closure {
                ret,
                params,
                constraints,
            } => match &actual {
                Ty::Closure {
                    ret: actual_ret,
                    params: actual_params,
                    constraints: actual_constraints,
                } if params.len() == actual_params.len() => {
                    self.unify_ty_for_inference(ret, actual_ret, subst)
                        && params
                            .iter()
                            .zip(actual_params.iter())
                            .all(|(pattern, actual)| {
                                self.unify_ty_for_inference(pattern, actual, subst)
                            })
                        && self.unify_constraint_bounds_for_inference(
                            constraints,
                            actual_constraints,
                            subst,
                        )
                }
                Ty::ClosureInstance {
                    ret: actual_ret,
                    params: actual_params,
                    ..
                } if params.len() == actual_params.len() => {
                    self.unify_ty_for_inference(ret, actual_ret, subst)
                        && params
                            .iter()
                            .zip(actual_params.iter())
                            .all(|(pattern, actual)| {
                                self.unify_ty_for_inference(pattern, actual, subst)
                            })
                }
                _ => false,
            },
            Ty::ClosureInstance {
                id,
                ret,
                params,
                captures,
            } => match &actual {
                Ty::ClosureInstance {
                    id: actual_id,
                    ret: actual_ret,
                    params: actual_params,
                    captures: actual_captures,
                } if id == actual_id
                    && params.len() == actual_params.len()
                    && captures.len() == actual_captures.len() =>
                {
                    self.unify_ty_for_inference(ret, actual_ret, subst)
                        && params
                            .iter()
                            .zip(actual_params.iter())
                            .all(|(pattern, actual)| {
                                self.unify_ty_for_inference(pattern, actual, subst)
                            })
                        && captures
                            .iter()
                            .zip(actual_captures.iter())
                            .all(|(pattern, actual)| {
                                self.unify_ty_for_inference(pattern, actual, subst)
                            })
                }
                _ => false,
            },
            other => other == &actual,
        }
    }

    fn unify_constraint_bounds_for_inference(
        &mut self,
        pattern: &ConstraintBounds,
        actual: &ConstraintBounds,
        subst: &mut HashMap<String, Ty>,
    ) -> bool {
        let mut trial = subst.clone();
        if !self.unify_constraint_refs_for_inference(
            &pattern.positive,
            &actual.positive,
            &mut trial,
        ) {
            return false;
        }
        if !self.unify_constraint_refs_for_inference(
            &pattern.negative,
            &actual.negative,
            &mut trial,
        ) {
            return false;
        }
        *subst = trial;
        true
    }

    fn unify_constraint_refs_for_inference(
        &mut self,
        pattern: &[ConstraintRef],
        actual: &[ConstraintRef],
        subst: &mut HashMap<String, Ty>,
    ) -> bool {
        let Some((first, rest)) = pattern.split_first() else {
            return true;
        };
        for candidate in actual {
            if first.name != candidate.name || first.args.len() != candidate.args.len() {
                continue;
            }
            let mut trial = subst.clone();
            if !first
                .args
                .iter()
                .zip(candidate.args.iter())
                .all(|(pattern_arg, actual_arg)| {
                    self.unify_ty_for_inference(pattern_arg, actual_arg, &mut trial)
                })
            {
                continue;
            }
            if self.unify_constraint_refs_for_inference(rest, actual, &mut trial) {
                *subst = trial;
                return true;
            }
        }
        false
    }

    fn check_local_decl_init(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        ty: &Type,
        local_name: &str,
        init: Option<&Expr>,
    ) -> CheckedLocalInit {
        if !hir_type_contains_hole(ty) {
            let erased_after_generic_subst = hir_type_contains_generic(ty);
            let ty = self.lower_type(ty);
            self.reject_invalid_plain_value_type(&ty, span, "local variable");
            let init = if ty.is_erased_value() {
                if let Some(expr) = init {
                    if erased_after_generic_subst {
                        self.check_expr(scopes, expr, Some(&ty))
                    } else {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            "void values are implicit and cannot be explicitly initialized",
                        ));
                        None
                    }
                } else {
                    None
                }
            } else {
                init.and_then(|expr| self.check_expr(scopes, expr, Some(&ty)))
            };
            if let Some(init) = &init {
                self.require_assignable(&ty, &init.ty, span);
            }
            return CheckedLocalInit {
                assigned: init.as_ref().is_some_and(|init| !init.is_never())
                    || ty.is_erased_value(),
                ty,
                init,
            };
        }

        let Some(init_expr) = init else {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("type hole in local `{local_name}` requires an initializer"),
            ));
            return CheckedLocalInit {
                ty: Ty::Unknown,
                init: None,
                assigned: false,
            };
        };

        let declared_ty = self.lower_type_allowing_holes(ty);
        let expected = (!matches!(declared_ty, Ty::Hole(_))).then_some(&declared_ty);
        let checked_init = self.check_expr(scopes, init_expr, expected);
        if let Some(init) = &checked_init {
            self.unify_type_holes(&declared_ty, &init.ty);
        }

        let mut solved_ty = self.resolve_type_holes(&declared_ty);
        if contains_type_hole(&solved_ty) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("cannot infer type hole in local `{local_name}` from initializer"),
            ));
            solved_ty = Ty::Unknown;
        } else {
            self.ensure_enum_instance(&solved_ty);
            self.ensure_struct_instance(&solved_ty);
            self.reject_invalid_plain_value_type(&solved_ty, span, "local variable");
        }

        let init = checked_init.map(|init| {
            if matches!(solved_ty, Ty::Unknown) {
                init
            } else {
                self.coerce_expr_to_expected(scopes, init, Some(&solved_ty))
            }
        });
        if let Some(init) = &init {
            self.require_assignable(&solved_ty, &init.ty, span);
        }
        if solved_ty.is_erased_value() {
            self.diagnostics.push(Diagnostic::new(
                span,
                "void values are implicit and cannot be explicitly initialized",
            ));
            return CheckedLocalInit {
                ty: solved_ty,
                init: None,
                assigned: true,
            };
        }

        CheckedLocalInit {
            assigned: init.as_ref().is_some_and(|init| !init.is_never()),
            ty: solved_ty,
            init,
        }
    }

    fn expand_type_alias(
        &mut self,
        span: crate::span::Span,
        def_id: DefId,
        args: &[Type],
        outer_subst: &HashMap<String, Ty>,
        allow_holes: bool,
    ) -> Ty {
        if self.alias_expansion_stack.contains(&def_id) {
            let name = self.resolved.def(def_id).name.clone();
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("recursive type alias `{name}`"),
            ));
            return Ty::Unknown;
        }
        let Some(template) = self.type_aliases.get(&def_id).cloned() else {
            let name = self.resolved.def(def_id).name.clone();
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("unknown type alias `{name}`"),
            ));
            return Ty::Unknown;
        };
        if template.generics.len() != args.len() {
            let name = self.resolved.def(def_id).name.clone();
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "type alias `{name}` expects {} type arguments, got {}",
                    template.generics.len(),
                    args.len()
                ),
            ));
            return Ty::Unknown;
        }
        let mut subst = outer_subst.clone();
        for (generic, arg) in template.generics.iter().zip(args.iter()) {
            let concrete = self.lower_type_with_subst_inner(arg, outer_subst, allow_holes);
            subst.insert(generic.clone(), concrete);
        }
        self.alias_expansion_stack.push(def_id);
        let ty = match &template.target {
            TypeAliasTarget::Type(ty) => self.lower_type_with_subst_inner(ty, &subst, allow_holes),
            TypeAliasTarget::CSpelling { abi, spelling } => Ty::CSpelling {
                abi: abi.clone(),
                spelling: spelling.clone(),
            },
        };
        self.alias_expansion_stack.pop();
        ty
    }

    fn lower_std_meta_repr_type(
        &mut self,
        span: crate::span::Span,
        def_id: DefId,
        args: &[Type],
        subst: &HashMap<String, Ty>,
        allow_holes: bool,
    ) -> Option<Ty> {
        let borrowed = if std_id::is_std_meta_type(&self.resolved, def_id, "RefRepr") {
            true
        } else if std_id::is_std_meta_type(&self.resolved, def_id, "Repr") {
            false
        } else {
            return None;
        };
        if args.len() != 1 {
            let name = if borrowed { "RefRepr" } else { "Repr" };
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("meta::{name} requires exactly one type argument"),
            ));
            return Some(Ty::Unknown);
        }
        let source_ty = self.lower_type_with_subst_inner(&args[0], subst, allow_holes);
        if self.defer_meta_repr_expansion
            || contains_generic(&source_ty)
            || contains_type_hole(&source_ty)
        {
            return Some(std_meta_repr_marker_ty(borrowed, source_ty));
        }
        Some(self.meta_repr_ty(span, &source_ty, borrowed))
    }

    fn normalize_meta_repr_markers(
        &mut self,
        ty: &Ty,
        span: impl Into<Option<crate::span::Span>>,
    ) -> Ty {
        self.normalize_meta_repr_markers_inner(ty, span.into(), true)
    }

    fn normalize_meta_repr_markers_silent(&mut self, ty: &Ty) -> Ty {
        self.normalize_meta_repr_markers_inner(ty, None, false)
    }

    fn preserve_meta_repr_markers(&mut self, ty: &Ty) -> Ty {
        let previous_defer_meta_repr_expansion =
            std::mem::replace(&mut self.defer_meta_repr_expansion, true);
        let normalized = self.normalize_meta_repr_markers_inner(ty, None, false);
        self.defer_meta_repr_expansion = previous_defer_meta_repr_expansion;
        normalized
    }

    fn meta_repr_storage_ty(&mut self, ty: &Ty, span: impl Into<Option<crate::span::Span>>) -> Ty {
        let span = span.into();
        match ty {
            Ty::Pointer {
                nullable,
                mutability,
                inner,
            } => Ty::Pointer {
                nullable: *nullable,
                mutability: *mutability,
                inner: Box::new(self.meta_repr_storage_ty(inner, span)),
            },
            Ty::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(self.meta_repr_storage_ty(elem, span)),
            },
            Ty::Slice { mutability, elem } => Ty::Slice {
                mutability: *mutability,
                elem: Box::new(self.meta_repr_storage_ty(elem, span)),
            },
            Ty::Named { name, args } => {
                if let Some(borrowed) = meta_repr_marker_name(name) {
                    if args.len() != 1 {
                        return Ty::Unknown;
                    }
                    return self.meta_repr_ty(span, &args[0], borrowed);
                }
                let original = Ty::Named {
                    name: name.clone(),
                    args: args.clone(),
                };
                if self.type_implements_share_handle(&original)
                    || self.type_implements_thread_local(&original)
                {
                    return self.meta_repr_policy_leaf_ty(&original);
                }
                Ty::Named {
                    name: name.clone(),
                    args: args
                        .iter()
                        .map(|arg| self.meta_repr_storage_ty(arg, span))
                        .collect(),
                }
            }
            Ty::DynamicInterface { name, args } => Ty::DynamicInterface {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.meta_repr_storage_ty(arg, span))
                    .collect(),
            },
            Ty::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => Ty::Function {
                is_unsafe: *is_unsafe,
                abi: abi.clone(),
                ret: Box::new(self.meta_repr_storage_ty(ret, span)),
                params: params
                    .iter()
                    .map(|param| self.meta_repr_storage_ty(param, span))
                    .collect(),
            },
            Ty::Closure {
                ret,
                params,
                constraints,
            } => Ty::Closure {
                ret: Box::new(self.meta_repr_storage_ty(ret, span)),
                params: params
                    .iter()
                    .map(|param| self.meta_repr_storage_ty(param, span))
                    .collect(),
                constraints: constraints.clone(),
            },
            Ty::ClosureInstance {
                id,
                ret,
                params,
                captures,
            } => Ty::ClosureInstance {
                id: *id,
                ret: Box::new(self.meta_repr_storage_ty(ret, span)),
                params: params
                    .iter()
                    .map(|param| self.meta_repr_storage_ty(param, span))
                    .collect(),
                captures: captures
                    .iter()
                    .map(|capture| self.meta_repr_storage_ty(capture, span))
                    .collect(),
            },
            Ty::Hole(_)
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
            | Ty::Generic(_)
            | Ty::Unknown => ty.clone(),
        }
    }

    fn normalize_meta_repr_markers_inner(
        &mut self,
        ty: &Ty,
        span: Option<crate::span::Span>,
        emit_diagnostics: bool,
    ) -> Ty {
        match ty {
            Ty::Pointer {
                nullable,
                mutability,
                inner,
            } => Ty::Pointer {
                nullable: *nullable,
                mutability: *mutability,
                inner: Box::new(self.normalize_meta_repr_markers_inner(
                    inner,
                    span,
                    emit_diagnostics,
                )),
            },
            Ty::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(self.normalize_meta_repr_markers_inner(
                    elem,
                    span,
                    emit_diagnostics,
                )),
            },
            Ty::Slice { mutability, elem } => Ty::Slice {
                mutability: *mutability,
                elem: Box::new(self.normalize_meta_repr_markers_inner(
                    elem,
                    span,
                    emit_diagnostics,
                )),
            },
            Ty::Named { name, args } => {
                if let Some(borrowed) = meta_repr_marker_name(name) {
                    if args.len() == 1
                        && !self.defer_meta_repr_expansion
                        && !contains_generic(&args[0])
                        && !contains_type_hole(&args[0])
                    {
                        if emit_diagnostics {
                            return self.meta_repr_ty(span, &args[0], borrowed);
                        }
                        if let Some(normalized) = self.try_meta_repr_ty(&args[0], borrowed) {
                            return normalized;
                        }
                    }
                    return Ty::Named {
                        name: name.clone(),
                        args: args.clone(),
                    };
                }
                let original = Ty::Named {
                    name: name.clone(),
                    args: args.clone(),
                };
                if self.type_implements_share_handle(&original)
                    || self.type_implements_thread_local(&original)
                {
                    return original;
                }
                let args = args
                    .iter()
                    .map(|arg| self.normalize_meta_repr_markers_inner(arg, span, emit_diagnostics))
                    .collect::<Vec<_>>();
                Ty::Named {
                    name: name.clone(),
                    args,
                }
            }
            Ty::DynamicInterface { name, args } => Ty::DynamicInterface {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.normalize_meta_repr_markers_inner(arg, span, emit_diagnostics))
                    .collect(),
            },
            Ty::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => Ty::Function {
                is_unsafe: *is_unsafe,
                abi: abi.clone(),
                ret: Box::new(self.normalize_meta_repr_markers_inner(ret, span, emit_diagnostics)),
                params: params
                    .iter()
                    .map(|param| {
                        self.normalize_meta_repr_markers_inner(param, span, emit_diagnostics)
                    })
                    .collect(),
            },
            Ty::Closure {
                ret,
                params,
                constraints,
            } => Ty::Closure {
                ret: Box::new(self.normalize_meta_repr_markers_inner(ret, span, emit_diagnostics)),
                params: params
                    .iter()
                    .map(|param| {
                        self.normalize_meta_repr_markers_inner(param, span, emit_diagnostics)
                    })
                    .collect(),
                constraints: constraints.clone(),
            },
            Ty::ClosureInstance {
                id,
                ret,
                params,
                captures,
            } => Ty::ClosureInstance {
                id: *id,
                ret: Box::new(self.normalize_meta_repr_markers_inner(ret, span, emit_diagnostics)),
                params: params
                    .iter()
                    .map(|param| {
                        self.normalize_meta_repr_markers_inner(param, span, emit_diagnostics)
                    })
                    .collect(),
                captures: captures
                    .iter()
                    .map(|capture| {
                        self.normalize_meta_repr_markers_inner(capture, span, emit_diagnostics)
                    })
                    .collect(),
            },
            other => other.clone(),
        }
    }

    fn substitute_ty_normalized(
        &mut self,
        ty: &Ty,
        subst: &HashMap<String, Ty>,
        span: impl Into<Option<crate::span::Span>>,
    ) -> Ty {
        let substituted = substitute_ty(ty, subst);
        self.normalize_meta_repr_markers(&substituted, span)
    }

    fn substitute_ty_normalized_silent(&mut self, ty: &Ty, subst: &HashMap<String, Ty>) -> Ty {
        let substituted = substitute_ty(ty, subst);
        self.normalize_meta_repr_markers_silent(&substituted)
    }

    fn inference_arg_expected(
        &mut self,
        param_ty: &Ty,
        subst: &HashMap<String, Ty>,
        expected_hints: &HashMap<String, Ty>,
    ) -> (Ty, Option<Ty>) {
        let expected_arg = self.substitute_ty_normalized_silent(param_ty, subst);
        if !contains_generic(&expected_arg) {
            return (expected_arg.clone(), Some(expected_arg));
        }

        let mut hinted_subst = expected_hints.clone();
        for (name, ty) in subst {
            hinted_subst.insert(name.clone(), ty.clone());
        }
        let hinted_arg = self.substitute_ty_normalized_silent(param_ty, &hinted_subst);
        let expected_for_arg = if contains_generic(&hinted_arg) {
            None
        } else {
            Some(hinted_arg)
        };
        (expected_arg, expected_for_arg)
    }

    fn closure_inference_expected(
        &mut self,
        param_ty: &Ty,
        subst: &HashMap<String, Ty>,
        expected_hints: &HashMap<String, Ty>,
    ) -> Option<Ty> {
        let hinted = expected_hints
            .iter()
            .chain(subst.iter())
            .map(|(name, ty)| (name.clone(), ty.clone()))
            .collect::<HashMap<_, _>>();
        let candidate = self.substitute_ty_normalized_silent(param_ty, &hinted);
        let mut inference_holes = HashMap::new();
        match candidate {
            Ty::Closure {
                ret,
                params,
                constraints,
            } => Some(Ty::Closure {
                ret: Box::new(self.partial_inference_ty(&ret, &mut inference_holes)),
                params: params
                    .iter()
                    .map(|param| self.partial_inference_ty(param, &mut inference_holes))
                    .collect(),
                constraints: self
                    .partial_inference_constraint_bounds(&constraints, &mut inference_holes),
            }),
            Ty::ClosureInstance {
                id,
                ret,
                params,
                captures,
            } => Some(Ty::ClosureInstance {
                id,
                ret: Box::new(self.partial_inference_ty(&ret, &mut inference_holes)),
                params: params
                    .iter()
                    .map(|param| self.partial_inference_ty(param, &mut inference_holes))
                    .collect(),
                captures: captures
                    .iter()
                    .map(|capture| self.partial_inference_ty(capture, &mut inference_holes))
                    .collect(),
            }),
            Ty::Function {
                is_unsafe: false,
                abi: None,
                ret,
                params,
            } => Some(Ty::Function {
                is_unsafe: false,
                abi: None,
                ret: Box::new(self.partial_inference_ty(&ret, &mut inference_holes)),
                params: params
                    .iter()
                    .map(|param| self.partial_inference_ty(param, &mut inference_holes))
                    .collect(),
            }),
            _ => None,
        }
    }

    fn partial_inference_ty(&mut self, ty: &Ty, holes: &mut HashMap<String, Ty>) -> Ty {
        match ty {
            Ty::Generic(name) => {
                if let Some(hole) = holes.get(name) {
                    return hole.clone();
                }
                let hole = Ty::Hole(self.next_type_hole_id);
                self.next_type_hole_id += 1;
                holes.insert(name.clone(), hole.clone());
                hole
            }
            Ty::Pointer {
                nullable,
                mutability,
                inner,
            } => Ty::Pointer {
                nullable: *nullable,
                mutability: *mutability,
                inner: Box::new(self.partial_inference_ty(inner, holes)),
            },
            Ty::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(self.partial_inference_ty(elem, holes)),
            },
            Ty::Slice { mutability, elem } => Ty::Slice {
                mutability: *mutability,
                elem: Box::new(self.partial_inference_ty(elem, holes)),
            },
            Ty::Named { name, args } => Ty::Named {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.partial_inference_ty(arg, holes))
                    .collect(),
            },
            Ty::DynamicInterface { name, args } => Ty::DynamicInterface {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.partial_inference_ty(arg, holes))
                    .collect(),
            },
            Ty::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => Ty::Function {
                is_unsafe: *is_unsafe,
                abi: abi.clone(),
                ret: Box::new(self.partial_inference_ty(ret, holes)),
                params: params
                    .iter()
                    .map(|param| self.partial_inference_ty(param, holes))
                    .collect(),
            },
            Ty::Closure {
                ret,
                params,
                constraints,
            } => Ty::Closure {
                ret: Box::new(self.partial_inference_ty(ret, holes)),
                params: params
                    .iter()
                    .map(|param| self.partial_inference_ty(param, holes))
                    .collect(),
                constraints: self.partial_inference_constraint_bounds(constraints, holes),
            },
            Ty::ClosureInstance {
                id,
                ret,
                params,
                captures,
            } => Ty::ClosureInstance {
                id: *id,
                ret: Box::new(self.partial_inference_ty(ret, holes)),
                params: params
                    .iter()
                    .map(|param| self.partial_inference_ty(param, holes))
                    .collect(),
                captures: captures
                    .iter()
                    .map(|capture| self.partial_inference_ty(capture, holes))
                    .collect(),
            },
            other => other.clone(),
        }
    }

    fn partial_inference_constraint_bounds(
        &mut self,
        bounds: &ConstraintBounds,
        holes: &mut HashMap<String, Ty>,
    ) -> ConstraintBounds {
        ConstraintBounds {
            positive: bounds
                .positive
                .iter()
                .map(|entry| self.partial_inference_constraint_ref(entry, holes))
                .collect(),
            negative: bounds
                .negative
                .iter()
                .map(|entry| self.partial_inference_constraint_ref(entry, holes))
                .collect(),
        }
    }

    fn partial_inference_constraint_ref(
        &mut self,
        entry: &ConstraintRef,
        holes: &mut HashMap<String, Ty>,
    ) -> ConstraintRef {
        ConstraintRef {
            name: entry.name.clone(),
            args: entry
                .args
                .iter()
                .map(|arg| self.partial_inference_ty(arg, holes))
                .collect(),
        }
    }

    fn meta_repr_ty(
        &mut self,
        span: impl Into<Option<crate::span::Span>>,
        source_ty: &Ty,
        borrowed: bool,
    ) -> Ty {
        self.meta_repr_ty_inner(span.into(), source_ty, borrowed, true)
            .unwrap_or(Ty::Unknown)
    }

    fn try_meta_repr_ty(&mut self, source_ty: &Ty, borrowed: bool) -> Option<Ty> {
        self.meta_repr_ty_inner(None, source_ty, borrowed, false)
    }

    fn meta_repr_ty_inner(
        &mut self,
        span: Option<crate::span::Span>,
        source_ty: &Ty,
        borrowed: bool,
        emit_diagnostics: bool,
    ) -> Option<Ty> {
        let root = (!borrowed).then(|| source_ty.clone());
        let mut expanding = HashSet::new();
        self.meta_repr_ty_inner_rec(
            span,
            source_ty,
            borrowed,
            emit_diagnostics,
            root.as_ref(),
            &mut expanding,
        )
    }

    fn meta_repr_ty_inner_rec(
        &mut self,
        span: Option<crate::span::Span>,
        source_ty: &Ty,
        borrowed: bool,
        emit_diagnostics: bool,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Option<Ty> {
        if contains_generic(source_ty) || contains_type_hole(source_ty) {
            return Some(std_meta_repr_marker_ty(borrowed, source_ty.clone()));
        }
        match source_ty {
            Ty::Array { len, elem } => {
                self.check_meta_array_budget(span, source_ty, *len, elem, emit_diagnostics)?;
                Some(if borrowed {
                    meta_ref_array_repr_ty(*len, elem)
                } else {
                    self.meta_array_repr_ty_inner(
                        span,
                        *len,
                        elem,
                        false,
                        emit_diagnostics,
                        root,
                        expanding,
                    )?
                })
            }
            Ty::Named { name, args } => {
                let instance_ty = Ty::Named {
                    name: name.clone(),
                    args: args.clone(),
                };
                if !borrowed && self.is_owned_meta_policy_leaf(&instance_ty, root) {
                    return Some(self.meta_repr_policy_leaf_ty(&instance_ty));
                }
                if !expanding.insert(instance_ty.clone()) {
                    if emit_diagnostics {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            format!(
                                "meta structural representation is recursive through `{source_ty}`"
                            ),
                        ));
                    }
                    return None;
                }
                self.ensure_struct_instance(&instance_ty);
                let instance_name = enum_instance_name(name, args);
                if let Some(fields) = self.structs.get(&instance_name).cloned() {
                    let mut field_tys = Vec::new();
                    for (_, ty) in fields {
                        field_tys.push(self.meta_repr_field_ty(
                            span,
                            &ty,
                            borrowed,
                            emit_diagnostics,
                            root,
                            expanding,
                        )?);
                    }
                    expanding.remove(&instance_ty);
                    return Some(meta_product_ty(
                        field_tys,
                        if borrowed { "FieldRef" } else { "Field" },
                    ));
                }
                self.ensure_enum_instance(&instance_ty);
                if let Some(enm) = self.checked_enums.get(&instance_name).cloned() {
                    let mut variants = Vec::new();
                    for variant in enm.variants {
                        let mut payloads = Vec::new();
                        for payload in variant.payload {
                            payloads.push(self.meta_repr_field_ty(
                                span,
                                &payload,
                                borrowed,
                                emit_diagnostics,
                                root,
                                expanding,
                            )?);
                        }
                        variants.push(payloads);
                    }
                    expanding.remove(&instance_ty);
                    return Some(meta_sum_ty(variants, borrowed));
                }
                expanding.remove(&instance_ty);
                if emit_diagnostics {
                    self.push_meta_unsupported_repr(span, source_ty);
                }
                None
            }
            Ty::ClosureInstance { captures, .. } => {
                let mut capture_tys = Vec::new();
                for ty in captures.iter().filter(|ty| !ty.is_erased_value()) {
                    capture_tys.push(self.meta_repr_field_ty(
                        span,
                        ty,
                        borrowed,
                        emit_diagnostics,
                        root,
                        expanding,
                    )?);
                }
                Some(meta_product_ty(
                    capture_tys,
                    if borrowed { "FieldRef" } else { "Field" },
                ))
            }
            Ty::Closure { .. } => {
                if emit_diagnostics {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!(
                            "meta structural representation requires a concrete closure value, got erased closure `{source_ty}`"
                        ),
                    ));
                }
                None
            }
            _ => {
                if emit_diagnostics {
                    self.push_meta_unsupported_repr(span, source_ty);
                }
                None
            }
        }
    }

    fn meta_repr_field_ty(
        &mut self,
        span: Option<crate::span::Span>,
        ty: &Ty,
        borrowed: bool,
        emit_diagnostics: bool,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Option<Ty> {
        if borrowed {
            return Some(meta_repr_borrowed_array_leaf_ty(ty));
        }
        self.meta_repr_owned_leaf_ty_inner(span, ty, emit_diagnostics, root, expanding)
    }

    fn meta_repr_policy_leaf_ty(&mut self, ty: &Ty) -> Ty {
        match ty {
            Ty::Named { name, args } => Ty::Named {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.preserve_meta_repr_markers(arg))
                    .collect(),
            },
            _ => ty.clone(),
        }
    }

    fn meta_repr_owned_leaf_ty_inner(
        &mut self,
        span: Option<crate::span::Span>,
        ty: &Ty,
        emit_diagnostics: bool,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Option<Ty> {
        if self.is_owned_meta_policy_leaf(ty, root) {
            return Some(self.meta_repr_policy_leaf_ty(ty));
        }
        match ty {
            Ty::Array { len, elem } => {
                self.check_meta_array_budget(span, ty, *len, elem, emit_diagnostics)?;
                self.meta_array_repr_ty_inner(
                    span,
                    *len,
                    elem,
                    false,
                    emit_diagnostics,
                    root,
                    expanding,
                )
            }
            Ty::Named { .. } | Ty::ClosureInstance { .. } => {
                self.meta_repr_ty_inner_rec(span, ty, false, emit_diagnostics, root, expanding)
            }
            other => Some(other.clone()),
        }
    }

    fn is_owned_meta_policy_leaf(&mut self, ty: &Ty, root: Option<&Ty>) -> bool {
        if contains_generic(ty) || contains_type_hole(ty) {
            return false;
        }
        let leaf_ty = self.meta_repr_policy_leaf_ty(ty);
        let is_thread_local = self.type_implements_thread_local(&leaf_ty);
        if root.is_some_and(|root| ty == root) && !is_thread_local {
            return false;
        }
        matches!(ty, Ty::Named { .. })
            && (is_thread_local
                || self.type_implements_share_handle(&leaf_ty)
                || self.type_implements_message(&leaf_ty))
    }

    fn meta_array_repr_ty_inner(
        &mut self,
        span: Option<crate::span::Span>,
        len: usize,
        elem: &Ty,
        borrowed: bool,
        emit_diagnostics: bool,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Option<Ty> {
        if len == 0 {
            return Some(meta_named("ArrayNil", Vec::new()));
        }
        if len <= crate::types::META_ARRAY_CHUNK_SIZE {
            let elem_ty = if borrowed {
                meta_repr_borrowed_array_leaf_ty(elem)
            } else {
                self.meta_repr_owned_leaf_ty_inner(span, elem, emit_diagnostics, root, expanding)?
            };
            return Some(meta_named(&format!("ArrayChunk{len}"), vec![elem_ty]));
        }
        let split = crate::types::meta_array_split_len(len);
        Some(meta_named(
            "ArrayCat",
            vec![
                self.meta_array_repr_ty_inner(
                    span,
                    split,
                    elem,
                    borrowed,
                    emit_diagnostics,
                    root,
                    expanding,
                )?,
                self.meta_array_repr_ty_inner(
                    span,
                    len - split,
                    elem,
                    borrowed,
                    emit_diagnostics,
                    root,
                    expanding,
                )?,
            ],
        ))
    }

    fn check_meta_array_budget(
        &mut self,
        span: Option<crate::span::Span>,
        source_ty: &Ty,
        len: usize,
        elem: &Ty,
        emit_diagnostics: bool,
    ) -> Option<()> {
        let cost = crate::types::meta_array_expansion_cost(len, elem)?;
        if cost <= META_ARRAY_EXPANSION_BUDGET {
            return Some(());
        }
        if emit_diagnostics {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "meta::Repr<{source_ty}> expands too many structural array nodes; use an explicit Message wrapper or an owned buffer type"
                ),
            ));
        }
        None
    }

    fn push_meta_unsupported_repr(
        &mut self,
        span: impl Into<Option<crate::span::Span>>,
        source_ty: &Ty,
    ) {
        self.diagnostics.push(Diagnostic::new(
            span,
            format!(
                "meta structural representation supports visible structs, enums, and concrete closure values, got `{source_ty}`"
            ),
        ));
    }

    fn alloc_synthetic_def(&mut self) -> DefId {
        let id = DefId(self.next_synthetic_def);
        self.next_synthetic_def += 1;
        id
    }

    fn ensure_enum_instance(&mut self, ty: &Ty) {
        match ty {
            Ty::Named { name, args } => {
                let Some(template) = self.enum_templates.get(name).cloned() else {
                    return;
                };
                if args.iter().any(contains_generic) {
                    return;
                }
                if args.len() != template.generics.len() {
                    self.diagnostics.push(Diagnostic::new(
                        None,
                        format!(
                            "enum `{name}` expects {} type arguments, got {}",
                            template.generics.len(),
                            args.len()
                        ),
                    ));
                    return;
                }
                let instance_name = enum_instance_name(name, args);
                if self.checked_enums.contains_key(&instance_name)
                    || self.visiting_enums.contains(&instance_name)
                {
                    return;
                }
                let subst = template
                    .generics
                    .iter()
                    .cloned()
                    .zip(args.iter().cloned())
                    .collect::<HashMap<_, _>>();
                self.visiting_enums.insert(instance_name.clone());
                let variants = template
                    .variants
                    .iter()
                    .map(|variant| CheckedVariant {
                        name: variant.name.clone(),
                        payload: variant
                            .payload
                            .iter()
                            .filter_map(|payload| {
                                let ty =
                                    self.lower_type_preserving_meta_repr_markers(payload, &subst);
                                (!ty.is_erased_value()).then_some(ty)
                            })
                            .collect(),
                    })
                    .collect::<Vec<_>>();
                self.visiting_enums.remove(&instance_name);
                self.checked_enums.insert(
                    instance_name.clone(),
                    CheckedEnum {
                        name: instance_name,
                        variants,
                    },
                );
            }
            Ty::Pointer { inner, .. } => self.ensure_enum_instance(inner),
            Ty::Array { elem, .. } | Ty::Slice { elem, .. } => self.ensure_enum_instance(elem),
            Ty::DynamicInterface { args, .. } => {
                for arg in args {
                    self.ensure_enum_instance(arg);
                }
            }
            Ty::Function { ret, params, .. } => {
                self.ensure_enum_instance(ret);
                for param in params {
                    self.ensure_enum_instance(param);
                }
            }
            Ty::Closure { ret, params, .. } | Ty::ClosureInstance { ret, params, .. } => {
                self.ensure_enum_instance(ret);
                for param in params {
                    self.ensure_enum_instance(param);
                }
            }
            _ => {}
        }
    }

    fn ensure_struct_instance(&mut self, ty: &Ty) {
        match ty {
            Ty::Named { name, args } => {
                let Some(template) = self.struct_templates.get(name).cloned() else {
                    return;
                };
                if args.iter().any(contains_generic) {
                    return;
                }
                if args.len() != template.generics.len() {
                    self.diagnostics.push(Diagnostic::new(
                        None,
                        format!(
                            "struct `{name}` expects {} type arguments, got {}",
                            template.generics.len(),
                            args.len()
                        ),
                    ));
                    return;
                }
                let instance_name = enum_instance_name(name, args);
                if self.structs.contains_key(&instance_name)
                    || self.visiting_structs.contains(&instance_name)
                {
                    return;
                }
                let subst = template
                    .generics
                    .iter()
                    .cloned()
                    .zip(args.iter().cloned())
                    .collect::<HashMap<_, _>>();
                self.visiting_structs.insert(instance_name.clone());
                let fields = template
                    .fields
                    .iter()
                    .map(|field| {
                        let ty = self.lower_type_preserving_meta_repr_markers(&field.ty, &subst);
                        self.reject_invalid_plain_value_type(&ty, field.ty.span, "struct field");
                        (field.name.name.clone(), ty)
                    })
                    .collect::<Vec<_>>();
                self.visiting_structs.remove(&instance_name);
                self.structs.insert(instance_name, fields);
                if template.is_unsafe {
                    self.unsafe_structs.insert(enum_instance_name(name, args));
                }
            }
            Ty::Pointer { inner, .. } => self.ensure_struct_instance(inner),
            Ty::Array { elem, .. } | Ty::Slice { elem, .. } => self.ensure_struct_instance(elem),
            Ty::DynamicInterface { args, .. } => {
                for arg in args {
                    self.ensure_struct_instance(arg);
                }
            }
            Ty::Function { ret, params, .. } => {
                self.ensure_struct_instance(ret);
                for param in params {
                    self.ensure_struct_instance(param);
                }
            }
            Ty::Closure { ret, params, .. } | Ty::ClosureInstance { ret, params, .. } => {
                self.ensure_struct_instance(ret);
                for param in params {
                    self.ensure_struct_instance(param);
                }
            }
            _ => {}
        }
    }

    fn function_sig_for(&self, module: ModuleId, name: &str) -> Option<&FunctionSig> {
        self.functions_by_name.get(name).and_then(|defs| {
            defs.iter().find_map(|def_id| {
                let sig = self.functions_by_def.get(def_id)?;
                (sig.module == module).then_some(sig)
            })
        })
    }

    fn resolve_function_name(&mut self, name: &NameRef) -> Option<FunctionSig> {
        let def_id = self.name_def_of_kind(
            name,
            &[DefKind::Function, DefKind::ExternFunction],
            "function",
        )?;
        self.functions_by_def.get(&def_id).cloned()
    }

    fn lookup_variant_name(&self, name: &NameRef) -> Option<(DefId, VariantSig)> {
        let def_id = self.name_def_of_kind_ref(name, &[DefKind::EnumVariant])?;
        let sig = self.variants.get(&def_id)?.clone();
        Some((def_id, sig))
    }

    fn lookup_interface_name(&self, name: &NameRef) -> Option<DefId> {
        self.name_def_of_kind_ref(name, &[DefKind::Interface, DefKind::InterfaceAlias])
    }

    fn name_def_of_kind(
        &mut self,
        name: &NameRef,
        kinds: &[DefKind],
        kind_name: &str,
    ) -> Option<DefId> {
        let def_id = self.name_def_of_kind_ref(name, kinds)?;
        Some(def_id).or_else(|| {
            self.diagnostics.push(Diagnostic::new(
                name.span,
                format!("`{}` is not a {kind_name}", name.display),
            ));
            None
        })
    }

    fn name_def_of_kind_ref(&self, name: &NameRef, kinds: &[DefKind]) -> Option<DefId> {
        let NameRefKind::Def(def_id) = name.kind else {
            return None;
        };
        let def = self.resolved.def(def_id);
        if kinds.iter().any(|kind| *kind == def.kind) {
            Some(def_id)
        } else {
            None
        }
    }

    fn resolved_local_id(&self, name: &NameRef) -> Option<LocalId> {
        match name.kind {
            NameRefKind::Local(local_id) => Some(local_id),
            _ => None,
        }
    }

    fn interface_sig_by_name(&self, name: &str) -> Option<&InterfaceSig> {
        self.interface_names
            .get(name)
            .and_then(|def_id| self.interfaces.get(def_id))
    }

    fn collect_functions(&mut self) {
        let modules = self.hir_modules.clone();
        for module in &modules {
            self.current_module = module.id;
            for item in &module.items {
                match &item.kind {
                    ItemKind::Function(function) => {
                        let Some(def_id) = item.def_ids.iter().copied().find(|def_id| {
                            let def = self.resolved.def(*def_id);
                            def.name == function.signature.name.name
                                && matches!(def.kind, DefKind::Function | DefKind::ExternFunction)
                        }) else {
                            continue;
                        };
                        let exported = self.resolved.def(def_id).exported;
                        let is_generic = !function.signature.generics.is_empty();
                        self.insert_function_sig(
                            def_id,
                            module.id,
                            &function.signature,
                            function.is_unsafe,
                            function.is_async,
                            function.abi.clone(),
                            false,
                            function.body.is_some(),
                            exported,
                        );
                        if is_generic {
                            self.generic_functions.insert(
                                def_id,
                                GenericFunctionTemplate {
                                    function: function.clone(),
                                    exported,
                                },
                            );
                        }
                    }
                    ItemKind::ExternBlock(block) => {
                        for extern_item in &block.items {
                            if let ExternItem::Function {
                                noescape,
                                signature,
                            } = extern_item
                            {
                                let Some(def_id) = item.def_ids.iter().copied().find(|def_id| {
                                    let def = self.resolved.def(*def_id);
                                    def.name == signature.name.name
                                        && def.kind == DefKind::ExternFunction
                                }) else {
                                    continue;
                                };
                                let exported = self.resolved.def(def_id).exported;
                                self.insert_function_sig(
                                    def_id,
                                    module.id,
                                    signature,
                                    block.is_unsafe,
                                    false,
                                    Some(block.abi.clone()),
                                    *noescape,
                                    false,
                                    exported,
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn collect_impls(&mut self, check_concrete_bodies: bool) {
        let modules = self.hir_modules.clone();
        let mut pending_bodies = Vec::new();
        for module in &modules {
            for item in &module.items {
                let ItemKind::Impl(decl) = &item.kind else {
                    continue;
                };
                self.current_module = module.id;
                let Some(interface_def) =
                    self.name_def_of_kind(&decl.name, &[DefKind::Interface], "interface")
                else {
                    self.diagnostics.push(Diagnostic::new(
                        decl.name.span,
                        format!("unknown interface `{}` in impl", decl.name.display),
                    ));
                    continue;
                };
                let Some(interface) = self.interfaces.get(&interface_def).cloned() else {
                    continue;
                };
                if interface.is_unsafe && !decl.is_unsafe {
                    self.diagnostics.push(Diagnostic::new(
                        decl.name.span,
                        format!(
                            "impl `{}` requires `unsafe impl` because the interface is unsafe",
                            interface.name
                        ),
                    ));
                    continue;
                }
                if !interface.is_unsafe && decl.is_unsafe {
                    self.diagnostics.push(Diagnostic::new(
                        decl.name.span,
                        format!(
                            "`unsafe impl` cannot implement safe interface `{}`",
                            interface.name
                        ),
                    ));
                    continue;
                }
                if self.is_compiler_provided_meta_marker_def(interface_def) {
                    self.diagnostics.push(Diagnostic::new(
                        decl.name.span,
                        format!(
                            "`{}` is a compiler-provided marker and cannot be implemented in source",
                            interface.name
                        ),
                    ));
                    continue;
                }

                let Some(analysis) = self.analyze_impl_signature(item.span, decl, &interface)
                else {
                    continue;
                };
                if check_concrete_bodies {
                    self.check_generic_marker_impl_overlap(item.span, &analysis);
                }
                if analysis.generics.is_empty() {
                    if !check_concrete_bodies {
                        continue;
                    }
                    if self
                        .find_impl_by_full_args(
                            &analysis.interface_name,
                            &analysis.interface_args,
                            analysis.receiver_ty.as_ref(),
                        )
                        .is_some()
                    {
                        self.diagnostics.push(Diagnostic::new(
                            decl.name.span,
                            format!(
                                "conflicting impl of `{}` for this receiver",
                                analysis.interface_name
                            ),
                        ));
                        continue;
                    }
                    if let Some(pending) = self.register_impl_signature(
                        module.id,
                        decl,
                        &analysis.interface_name,
                        analysis.interface_args,
                        analysis.receiver_ty,
                        analysis.ret,
                        analysis.params,
                    ) {
                        pending_bodies.push(pending);
                    }
                } else {
                    self.generic_impls.push(GenericImplTemplate {
                        module: module.id,
                        item_span: item.span,
                        interface_name: analysis.interface_name,
                        generics: analysis.generics,
                        interface_args: analysis.interface_args,
                        receiver_ty: analysis.receiver_ty,
                        ret: analysis.ret,
                        params: analysis.params,
                        decl: decl.clone(),
                    });
                }
            }
        }
        for pending in &pending_bodies {
            self.check_registered_impl_body(pending, &HashMap::new());
        }
    }

    fn check_generic_marker_impl_overlap(
        &mut self,
        span: crate::span::Span,
        analysis: &ImplAnalysis,
    ) {
        if analysis.generics.is_empty()
            || !self.is_std_message_capability_interface_name(&analysis.interface_name)
        {
            return;
        }
        let current_domain =
            self.compiler_marker_domain_for_impl(&analysis.generics, analysis.receiver_ty.as_ref());
        let templates = self.generic_impls.clone();
        for template in &templates {
            if template.interface_name != analysis.interface_name {
                continue;
            }
            let template_domain = self
                .compiler_marker_domain_for_impl(&template.generics, template.receiver_ty.as_ref());
            if marker_impl_domains_disjoint(
                current_domain,
                analysis.receiver_ty.as_ref(),
                template_domain,
                template.receiver_ty.as_ref(),
            ) {
                continue;
            }
            if marker_impl_patterns_overlap(
                &template.interface_args,
                template.receiver_ty.as_ref(),
                &analysis.interface_args,
                analysis.receiver_ty.as_ref(),
            ) {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "ambiguous generic impls for marker interface `{}`",
                        analysis.interface_name
                    ),
                ));
                return;
            }
        }
        for existing in &self.impls {
            if existing.interface_name != analysis.interface_name {
                continue;
            }
            if marker_impl_domains_disjoint(
                current_domain,
                analysis.receiver_ty.as_ref(),
                None,
                existing.receiver_ty.as_ref(),
            ) {
                continue;
            }
            if marker_impl_patterns_overlap(
                &existing.interface_args,
                existing.receiver_ty.as_ref(),
                &analysis.interface_args,
                analysis.receiver_ty.as_ref(),
            ) {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "generic marker impl for `{}` conflicts with an existing concrete impl",
                        analysis.interface_name
                    ),
                ));
                return;
            }
        }
    }

    fn compiler_marker_domain_for_impl(
        &mut self,
        generics: &[GenericInfo],
        receiver_ty: Option<&Ty>,
    ) -> Option<CompilerMarkerDomain> {
        let Ty::Generic(receiver_name) = receiver_ty? else {
            return None;
        };
        let generic = generics
            .iter()
            .find(|generic| generic.name == *receiver_name)?;
        let constraint = generic.constraint.as_ref()?;
        let subst = generics
            .iter()
            .map(|generic| (generic.name.clone(), Ty::Generic(generic.name.clone())))
            .collect::<HashMap<_, _>>();
        let bounds = self.constraint_bounds(constraint, &subst);
        let has_ciel_fn = bounds.positive.iter().any(|entry| {
            self.is_std_meta_ciel_fn_value_marker_name(&entry.name) && entry.args.is_empty()
        });
        let has_closure = bounds.positive.iter().any(|entry| {
            self.is_std_meta_closure_value_marker_name(&entry.name) && entry.args.is_empty()
        });
        match (has_ciel_fn, has_closure) {
            (true, false) => Some(CompilerMarkerDomain::CielFnValue),
            (false, true) => Some(CompilerMarkerDomain::ClosureValue),
            _ => None,
        }
    }

    fn analyze_impl_signature(
        &mut self,
        span: crate::span::Span,
        decl: &ImplDecl,
        interface: &InterfaceSig,
    ) -> Option<ImplAnalysis> {
        if decl.params.len() != interface.params.len() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "impl `{}` expects {} parameters, got {}",
                    interface.name,
                    interface.params.len(),
                    decl.params.len()
                ),
            ));
            return None;
        }

        let generics = decl
            .generics
            .iter()
            .map(|param| GenericInfo {
                name: param.name.name.clone(),
                constraint: param.constraint.clone(),
            })
            .collect::<Vec<_>>();
        let impl_subst = generics
            .iter()
            .map(|param| (param.name.clone(), Ty::Generic(param.name.clone())))
            .collect::<HashMap<_, _>>();
        let interface_placeholders = interface
            .generics
            .iter()
            .map(|name| {
                (
                    name.clone(),
                    interface_generic_placeholder(&interface.name, name),
                )
            })
            .collect::<HashMap<_, _>>();
        let interface_lower_subst = interface_placeholders
            .iter()
            .map(|(name, placeholder)| (name.clone(), Ty::Generic(placeholder.clone())))
            .collect::<HashMap<_, _>>();
        let mut inferred = interface_placeholders
            .values()
            .cloned()
            .map(|placeholder| (placeholder.clone(), Ty::Generic(placeholder)))
            .collect::<HashMap<_, _>>();

        for (idx, arg) in decl.args.iter().enumerate() {
            let Some(generic_name) = interface.generics.iter().skip(1).nth(idx) else {
                self.diagnostics.push(Diagnostic::new(
                    arg.span,
                    format!("too many type arguments for impl `{}`", interface.name),
                ));
                return None;
            };
            let placeholder = interface_placeholders
                .get(generic_name)
                .expect("interface generic has placeholder");
            let concrete = self.lower_type_with_subst(arg, &impl_subst);
            inferred.insert(placeholder.clone(), concrete);
        }

        let impl_params = decl
            .params
            .iter()
            .map(|param| {
                let ty = self.lower_type_with_subst(&param.ty, &impl_subst);
                self.reject_invalid_plain_value_type(&ty, param.ty.span, "impl parameter");
                ty
            })
            .collect::<Vec<_>>();
        for (interface_param, impl_param) in interface.params.iter().zip(impl_params.iter()) {
            let expected = self.lower_type_with_subst(&interface_param.ty, &interface_lower_subst);
            unify_ty(&expected, impl_param, &mut inferred);
        }
        let lowered_ret = self.lower_type_with_subst(&interface.ret, &interface_lower_subst);
        let ret = self.substitute_ty_normalized(&lowered_ret, &inferred, span);
        let expected_params = interface
            .params
            .iter()
            .map(|param| {
                let ty = self.lower_type_with_subst(&param.ty, &interface_lower_subst);
                self.substitute_ty_normalized(&ty, &inferred, param.ty.span)
            })
            .collect::<Vec<_>>();
        let placeholder_names = interface_placeholders
            .values()
            .cloned()
            .collect::<HashSet<_>>();
        if contains_any_generic_name(&ret, &placeholder_names)
            || expected_params
                .iter()
                .any(|ty| contains_any_generic_name(ty, &placeholder_names))
        {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "impl `{}` leaves interface generic parameters unresolved",
                    interface.name
                ),
            ));
            return None;
        }
        for (expected, actual) in expected_params.iter().zip(impl_params.iter()) {
            if expected != actual {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "impl `{}` parameter mismatch: expected `{expected}`, got `{actual}`",
                        interface.name
                    ),
                ));
            }
        }
        let interface_args = interface
            .generics
            .iter()
            .map(|name| {
                let placeholder = interface_placeholders
                    .get(name)
                    .expect("interface generic has placeholder");
                inferred.get(placeholder).cloned().unwrap_or(Ty::Unknown)
            })
            .collect::<Vec<_>>();
        let receiver_ty = interface.generics.first().and_then(|name| {
            let placeholder = interface_placeholders.get(name)?;
            inferred.get(placeholder).cloned()
        });
        Some(ImplAnalysis {
            interface_name: interface.name.clone(),
            generics,
            interface_args,
            receiver_ty,
            ret,
            params: impl_params,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn instantiate_impl_body(
        &mut self,
        module: ModuleId,
        decl: &ImplDecl,
        interface_name: &str,
        interface_args: Vec<Ty>,
        receiver_ty: Option<Ty>,
        ret: Ty,
        params_ty: Vec<Ty>,
        subst: &HashMap<String, Ty>,
    ) -> Option<ImplSig> {
        if let Some(existing) =
            self.find_impl_by_full_args(interface_name, &interface_args, receiver_ty.as_ref())
        {
            if subst.is_empty() {
                self.diagnostics.push(Diagnostic::new(
                    decl.name.span,
                    format!("conflicting impl of `{interface_name}` for this receiver"),
                ));
            }
            return Some(existing);
        }
        let pending = self.register_impl_signature(
            module,
            decl,
            interface_name,
            interface_args,
            receiver_ty,
            ret,
            params_ty,
        )?;
        let implementation = pending.implementation.clone();
        self.queue_impl_body(pending, subst.clone());
        Some(implementation)
    }

    #[allow(clippy::too_many_arguments)]
    fn register_impl_signature(
        &mut self,
        module: ModuleId,
        decl: &ImplDecl,
        interface_name: &str,
        interface_args: Vec<Ty>,
        receiver_ty: Option<Ty>,
        ret: Ty,
        params_ty: Vec<Ty>,
    ) -> Option<PendingImplBody> {
        if self
            .find_impl_by_full_args(interface_name, &interface_args, receiver_ty.as_ref())
            .is_some()
        {
            return None;
        }

        let function_def = self.alloc_synthetic_def();
        let function_name = impl_function_name(interface_name, &params_ty);
        let sig = FunctionSig {
            def_id: function_def,
            module,
            name: function_name.clone(),
            is_unsafe: false,
            is_async: false,
            abi: None,
            noescape: false,
            has_body: true,
            ret: ret.clone(),
            params: params_ty.clone(),
            generics: Vec::new(),
            exported: false,
        };
        self.functions_by_def.insert(function_def, sig.clone());
        self.ensure_struct_instance(&ret);
        self.ensure_enum_instance(&ret);
        for param in &params_ty {
            self.ensure_struct_instance(param);
            self.ensure_enum_instance(param);
        }
        let implementation = ImplSig {
            interface_name: interface_name.to_string(),
            interface_args,
            receiver_ty,
            function_def,
            ret,
            params: params_ty,
        };
        self.impls.push(implementation.clone());
        Some(PendingImplBody {
            decl: decl.clone(),
            module,
            function_name,
            function_sig: sig,
            implementation,
        })
    }

    fn check_registered_impl_body(
        &mut self,
        pending: &PendingImplBody,
        subst: &HashMap<String, Ty>,
    ) {
        let params = pending
            .decl
            .params
            .iter()
            .zip(pending.function_sig.params.iter())
            .map(|(param, ty)| (param.local_id, param.name.name.clone(), ty.clone()))
            .collect::<Vec<_>>();
        let body_params = pending
            .decl
            .params
            .iter()
            .zip(pending.function_sig.params.iter())
            .filter_map(|(param, ty)| {
                param.local_id.map(|local_id| {
                    (
                        local_id,
                        param.name.name.clone(),
                        ty.clone(),
                        param.mutability,
                    )
                })
            })
            .collect::<Vec<_>>();
        let previous_module = self.current_module;
        self.current_module = pending.module;
        self.type_subst_stack.push(subst.clone());
        let body =
            self.check_function_body(&pending.function_sig, &body_params, &pending.decl.body);
        self.type_subst_stack.pop();
        self.current_module = previous_module;
        if let Some(body) = body {
            self.generated_functions.push(CheckedFunction {
                def_id: pending.function_sig.def_id,
                name: pending.function_name.clone(),
                is_unsafe: false,
                is_async: false,
                abi: None,
                noescape: false,
                exported: false,
                ret: pending.function_sig.ret.clone(),
                params,
                body: Some(body),
            });
        }
    }

    fn queue_impl_body(&mut self, pending: PendingImplBody, subst: HashMap<String, Ty>) {
        if self
            .generated_functions
            .iter()
            .any(|function| function.def_id == pending.function_sig.def_id)
            || self
                .pending_impl_bodies
                .iter()
                .any(|queued| queued.pending.function_sig.def_id == pending.function_sig.def_id)
        {
            return;
        }
        self.pending_impl_bodies
            .push(QueuedImplBody { pending, subst });
    }

    fn drain_pending_impl_bodies(&mut self) {
        while let Some(queued) = self.pending_impl_bodies.pop() {
            if self
                .generated_functions
                .iter()
                .any(|function| function.def_id == queued.pending.function_sig.def_id)
            {
                continue;
            }
            self.check_registered_impl_body(&queued.pending, &queued.subst);
        }
    }

    fn insert_function_sig(
        &mut self,
        def_id: DefId,
        module: ModuleId,
        signature: &FunctionSignature,
        is_unsafe: bool,
        is_async: bool,
        abi: Option<String>,
        noescape: bool,
        has_body: bool,
        exported: bool,
    ) {
        let generics = signature
            .generics
            .iter()
            .map(|param| GenericInfo {
                name: param.name.name.clone(),
                constraint: param.constraint.clone(),
            })
            .collect::<Vec<_>>();
        let subst = generics
            .iter()
            .map(|param| (param.name.clone(), Ty::Generic(param.name.clone())))
            .collect::<HashMap<_, _>>();
        let previous_defer_meta_repr_expansion =
            std::mem::replace(&mut self.defer_meta_repr_expansion, true);
        let sig = FunctionSig {
            def_id,
            module,
            name: signature.name.name.clone(),
            is_unsafe,
            is_async,
            abi,
            noescape,
            has_body,
            ret: self.lower_type_with_subst(&signature.ret, &subst),
            params: signature
                .params
                .iter()
                .map(|param| {
                    let ty = self.lower_type_with_subst(&param.ty, &subst);
                    self.reject_invalid_plain_value_type(&ty, param.ty.span, "function parameter");
                    ty
                })
                .collect(),
            generics: generics.clone(),
            exported,
        };
        self.defer_meta_repr_expansion = previous_defer_meta_repr_expansion;
        self.reject_invalid_return_type(&sig.ret, signature.ret.span);
        self.functions_by_name
            .entry(signature.name.name.clone())
            .or_default()
            .push(def_id);
        self.functions_by_def.insert(def_id, sig);
    }

    fn normalize_function_sigs(&mut self) {
        let mut normalized = HashMap::new();
        let sigs = self
            .functions_by_def
            .iter()
            .map(|(def_id, sig)| (*def_id, sig.clone()))
            .collect::<Vec<_>>();
        for (def_id, mut sig) in sigs {
            let span = self.resolved.defs.get(def_id.0).map(|def| def.span);
            sig.ret = self.normalize_meta_repr_markers(&sig.ret, span);
            sig.params = sig
                .params
                .iter()
                .map(|param| self.normalize_meta_repr_markers(param, span))
                .collect();
            normalized.insert(def_id, sig);
        }
        self.functions_by_def = normalized;
    }

    fn validate_c_abi_functions(&mut self) {
        let mut by_symbol: HashMap<String, Vec<FunctionSig>> = HashMap::new();
        for sig in self.functions_by_def.values() {
            if sig.abi.as_deref() != Some("C") {
                continue;
            }
            if !sig.has_body && !sig.is_unsafe {
                self.diagnostics.push(Diagnostic::new(
                    self.resolved.def(sig.def_id).span,
                    "imported C function declarations must be in `unsafe extern \"C\"` blocks",
                ));
            }
            if sig.is_async {
                self.diagnostics.push(Diagnostic::new(
                    self.resolved.def(sig.def_id).span,
                    "`extern \"C\"` functions cannot be async",
                ));
            }
            if sig.has_body && !sig.exported {
                self.diagnostics.push(Diagnostic::new(
                    self.resolved.def(sig.def_id).span,
                    "`extern \"C\"` function bodies must be declared with `export`",
                ));
            }
            if !sig.generics.is_empty() {
                self.diagnostics.push(Diagnostic::new(
                    self.resolved.def(sig.def_id).span,
                    "`extern \"C\"` functions cannot be generic",
                ));
            }
            if type_contains_closure(&sig.ret) || sig.params.iter().any(type_contains_closure) {
                self.diagnostics.push(Diagnostic::new(
                    self.resolved.def(sig.def_id).span,
                    "closure types are not allowed in extern C declarations",
                ));
            }
            if sig.params.iter().any(Ty::is_erased_value) {
                self.diagnostics.push(Diagnostic::new(
                    self.resolved.def(sig.def_id).span,
                    "`extern \"C\"` parameters cannot have type `void` by value",
                ));
            }
            by_symbol
                .entry(sig.name.clone())
                .or_default()
                .push(sig.clone());
        }

        for (symbol, mut sigs) in by_symbol {
            sigs.sort_by_key(|sig| sig.def_id.0);
            let Some(first) = sigs.first() else {
                continue;
            };
            for sig in sigs.iter().skip(1) {
                if sig.ret != first.ret || sig.params != first.params {
                    self.diagnostics.push(Diagnostic::new(
                        self.resolved.def(sig.def_id).span,
                        format!("conflicting `extern \"C\"` declarations for symbol `{symbol}`"),
                    ));
                }
            }
            let definitions = sigs.iter().filter(|sig| sig.has_body).collect::<Vec<_>>();
            if definitions.len() > 1 {
                for sig in definitions.iter().skip(1) {
                    self.diagnostics.push(Diagnostic::new(
                        self.resolved.def(sig.def_id).span,
                        format!("multiple definitions of C ABI symbol `{symbol}`"),
                    ));
                }
            }
        }
    }

    fn check_function_item(
        &mut self,
        function: &FunctionDecl,
        exported: bool,
    ) -> Option<CheckedFunction> {
        let signature = &function.signature;
        if !signature.generics.is_empty() {
            return None;
        }
        let sig = self
            .function_sig_for(self.current_module, &signature.name.name)?
            .clone();
        let params = signature
            .params
            .iter()
            .map(|param| {
                (
                    param.local_id,
                    param.name.name.clone(),
                    self.lower_type(&param.ty),
                )
            })
            .collect::<Vec<_>>();
        let body_params = signature
            .params
            .iter()
            .zip(params.iter())
            .filter_map(|(param, (_, _, ty))| {
                param.local_id.map(|local_id| {
                    (
                        local_id,
                        param.name.name.clone(),
                        ty.clone(),
                        param.mutability,
                    )
                })
            })
            .collect::<Vec<_>>();
        let body = function
            .body
            .as_ref()
            .and_then(|body| self.check_function_body(&sig, &body_params, body));

        Some(CheckedFunction {
            def_id: sig.def_id,
            name: sig.name,
            is_unsafe: sig.is_unsafe,
            is_async: sig.is_async,
            abi: sig.abi,
            noescape: sig.noescape,
            exported,
            ret: sig.ret,
            params,
            body,
        })
    }

    fn check_function_body(
        &mut self,
        sig: &FunctionSig,
        params: &[(LocalId, String, Ty, BindingMutability)],
        body: &Block,
    ) -> Option<TBlock> {
        let previous_return_ty = std::mem::replace(&mut self.current_return_ty, sig.ret.clone());
        let previous_control_contexts = std::mem::take(&mut self.control_contexts);
        let previous_unsafe_depth = std::mem::replace(&mut self.unsafe_depth, 0);
        let previous_async_depth = std::mem::replace(
            &mut self.current_async_depth,
            if sig.is_async { 1 } else { 0 },
        );
        let mut scopes = LocalScopes::default();
        scopes.push();
        for (local_id, name, ty, mutability) in params {
            if let Err(name) = scopes.insert(
                *local_id,
                Binding {
                    name: name.clone(),
                    ty: ty.clone(),
                    narrowed_ty: None,
                    init_state: InitState::Assigned,
                    mutability: *mutability,
                    captured: false,
                    declared_loop_depth: self.current_loop_depth,
                },
            ) {
                self.diagnostics.push(Diagnostic::new(
                    body.span,
                    format!("duplicate parameter `{name}`"),
                ));
            }
        }
        let checked = self.check_block_with_existing_scope(&mut scopes, body, &sig.ret);
        if sig.ret.is_never()
            && checked
                .as_ref()
                .is_some_and(|checked| checked.flow.can_fallthrough)
        {
            self.diagnostics.push(Diagnostic::new(
                body.span,
                format!(
                    "function `{}` with return type `never` can fall through",
                    sig.name
                ),
            ));
        } else if !sig.ret.is_erased_value()
            && !checked
                .as_ref()
                .is_some_and(|checked| !checked.flow.can_fallthrough)
        {
            self.diagnostics.push(Diagnostic::new(
                body.span,
                format!(
                    "function `{}` must return `{}` on every path",
                    sig.name, sig.ret
                ),
            ));
        }
        if sig.is_async && let Some(checked) = checked.as_ref() {
            self.check_async_frame_safety(&checked.block, params);
        }
        self.current_return_ty = previous_return_ty;
        self.control_contexts = previous_control_contexts;
        self.unsafe_depth = previous_unsafe_depth;
        self.current_async_depth = previous_async_depth;
        checked.map(|checked| checked.block)
    }

    fn check_async_frame_safety(
        &mut self,
        block: &TBlock,
        params: &[(LocalId, String, Ty, BindingMutability)],
    ) {
        let mut infos = HashMap::<LocalId, AsyncLocalInfo>::new();
        for (local_id, name, ty, _) in params {
            infos.insert(
                *local_id,
                AsyncLocalInfo {
                    name: name.clone(),
                    ty: ty.clone(),
                    static_const_slice: false,
                },
            );
        }
        self.async_collect_local_infos_block(block, &mut infos);
        let live_after = HashSet::new();
        self.async_live_before_block(block, live_after, &infos);
        self.async_check_defer_arg_frame_safety_block(block);
    }

    fn async_check_defer_arg_frame_safety_block(&mut self, block: &TBlock) {
        for stmt in &block.statements {
            self.async_check_defer_arg_frame_safety_stmt(stmt);
        }
    }

    fn async_check_defer_arg_frame_safety_stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::Block(block) | TStmtKind::While { body: block, .. } => {
                self.async_check_defer_arg_frame_safety_block(block);
            }
            TStmtKind::VarDecl { init, .. } => {
                if let Some(init) = init {
                    self.async_check_defer_arg_frame_safety_expr(init);
                }
            }
            TStmtKind::Assign { target, value } => {
                self.async_check_defer_arg_frame_safety_expr(target);
                self.async_check_defer_arg_frame_safety_expr(value);
            }
            TStmtKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                self.async_check_defer_arg_frame_safety_expr(cond);
                self.async_check_defer_arg_frame_safety_block(then_block);
                if let Some(else_branch) = else_branch {
                    self.async_check_defer_arg_frame_safety_stmt(else_branch);
                }
            }
            TStmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(init) = init {
                    self.async_check_defer_arg_frame_safety_for_init(init);
                }
                if let Some(cond) = cond {
                    self.async_check_defer_arg_frame_safety_expr(cond);
                }
                if let Some(step) = step {
                    self.async_check_defer_arg_frame_safety_for_init(step);
                }
                self.async_check_defer_arg_frame_safety_block(body);
            }
            TStmtKind::Switch {
                expr,
                cases,
                default,
                ..
            } => {
                self.async_check_defer_arg_frame_safety_expr(expr);
                for case in cases {
                    for stmt in &case.statements {
                        self.async_check_defer_arg_frame_safety_stmt(stmt);
                    }
                }
                for stmt in default {
                    self.async_check_defer_arg_frame_safety_stmt(stmt);
                }
            }
            TStmtKind::Defer(expr) => {
                if let TExprKind::Call { args, .. } = &expr.kind {
                    for arg in args {
                        if arg.ty.is_erased_value() {
                            continue;
                        }
                        let static_const_slice =
                            self.async_is_static_const_slice_init(&arg.ty, Some(arg));
                        let mut visiting = HashSet::new();
                        if let Some(reason) = self.async_frame_safety_violation(
                            &arg.ty,
                            static_const_slice,
                            "`defer` argument",
                            &mut visiting,
                        ) {
                            self.diagnostics.push(Diagnostic::new(
                                arg.span,
                                format!(
                                    "`defer` argument is not async-frame-safe: {reason}"
                                ),
                            ));
                        }
                    }
                }
                self.async_check_defer_arg_frame_safety_expr(expr);
            }
            TStmtKind::Return(Some(expr)) | TStmtKind::Expr(expr) => {
                self.async_check_defer_arg_frame_safety_expr(expr);
            }
            TStmtKind::Return(None)
            | TStmtKind::Break
            | TStmtKind::Continue
            | TStmtKind::Unsupported => {}
        }
    }

    fn async_check_defer_arg_frame_safety_for_init(&mut self, init: &TForInit) {
        match init {
            TForInit::VarDecl { init, .. } => {
                if let Some(init) = init {
                    self.async_check_defer_arg_frame_safety_expr(init);
                }
            }
            TForInit::Assign { target, value } => {
                self.async_check_defer_arg_frame_safety_expr(target);
                self.async_check_defer_arg_frame_safety_expr(value);
            }
            TForInit::Expr(expr) => self.async_check_defer_arg_frame_safety_expr(expr),
        }
    }

    fn async_check_defer_arg_frame_safety_expr(&mut self, expr: &TExpr) {
        match &expr.kind {
            TExprKind::UnsafeBlock { statements, value } => {
                for stmt in statements {
                    self.async_check_defer_arg_frame_safety_stmt(stmt);
                }
                if let Some(value) = value {
                    self.async_check_defer_arg_frame_safety_expr(value);
                }
            }
            TExprKind::Closure { .. } => {}
            _ => walk_expr(
                &mut AsyncDeferArgFrameSafetyVisitor { checker: self },
                expr,
            ),
        }
    }

    fn async_collect_local_infos_block(
        &self,
        block: &TBlock,
        infos: &mut HashMap<LocalId, AsyncLocalInfo>,
    ) {
        for stmt in &block.statements {
            self.async_collect_local_infos_stmt(stmt, infos);
        }
    }

    fn async_collect_local_infos_stmt(
        &self,
        stmt: &TStmt,
        infos: &mut HashMap<LocalId, AsyncLocalInfo>,
    ) {
        match &stmt.kind {
            TStmtKind::Block(block) | TStmtKind::While { body: block, .. } => {
                self.async_collect_local_infos_block(block, infos);
            }
            TStmtKind::VarDecl {
                ty,
                name,
                local_id,
                init,
            } => {
                infos.insert(
                    *local_id,
                    AsyncLocalInfo {
                        name: name.clone(),
                        ty: ty.clone(),
                        static_const_slice: self.async_is_static_const_slice_init(ty, init.as_ref()),
                    },
                );
                if let Some(init) = init {
                    self.async_collect_local_infos_expr(init, infos);
                }
            }
            TStmtKind::Assign { target, value } => {
                self.async_collect_local_infos_expr(target, infos);
                self.async_collect_local_infos_expr(value, infos);
            }
            TStmtKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                self.async_collect_local_infos_expr(cond, infos);
                self.async_collect_local_infos_block(then_block, infos);
                if let Some(else_branch) = else_branch {
                    self.async_collect_local_infos_stmt(else_branch, infos);
                }
            }
            TStmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(init) = init {
                    self.async_collect_local_infos_for_init(init, infos);
                }
                if let Some(cond) = cond {
                    self.async_collect_local_infos_expr(cond, infos);
                }
                if let Some(step) = step {
                    self.async_collect_local_infos_for_init(step, infos);
                }
                self.async_collect_local_infos_block(body, infos);
            }
            TStmtKind::Switch {
                expr,
                cases,
                default,
                ..
            } => {
                self.async_collect_local_infos_expr(expr, infos);
                for case in cases {
                    self.async_collect_pattern_infos(&case.pattern, infos);
                    for stmt in &case.statements {
                        self.async_collect_local_infos_stmt(stmt, infos);
                    }
                }
                for stmt in default {
                    self.async_collect_local_infos_stmt(stmt, infos);
                }
            }
            TStmtKind::Defer(expr) | TStmtKind::Return(Some(expr)) | TStmtKind::Expr(expr) => {
                self.async_collect_local_infos_expr(expr, infos);
            }
            TStmtKind::Return(None)
            | TStmtKind::Break
            | TStmtKind::Continue
            | TStmtKind::Unsupported => {}
        }
    }

    fn async_collect_local_infos_for_init(
        &self,
        init: &TForInit,
        infos: &mut HashMap<LocalId, AsyncLocalInfo>,
    ) {
        match init {
            TForInit::VarDecl {
                ty,
                name,
                local_id,
                init,
            } => {
                infos.insert(
                    *local_id,
                    AsyncLocalInfo {
                        name: name.clone(),
                        ty: ty.clone(),
                        static_const_slice: self.async_is_static_const_slice_init(ty, init.as_ref()),
                    },
                );
                if let Some(init) = init {
                    self.async_collect_local_infos_expr(init, infos);
                }
            }
            TForInit::Assign { target, value } => {
                self.async_collect_local_infos_expr(target, infos);
                self.async_collect_local_infos_expr(value, infos);
            }
            TForInit::Expr(expr) => self.async_collect_local_infos_expr(expr, infos),
        }
    }

    fn async_collect_local_infos_expr(
        &self,
        expr: &TExpr,
        infos: &mut HashMap<LocalId, AsyncLocalInfo>,
    ) {
        match &expr.kind {
            TExprKind::UnsafeBlock { statements, value } => {
                for stmt in statements {
                    self.async_collect_local_infos_stmt(stmt, infos);
                }
                if let Some(value) = value {
                    self.async_collect_local_infos_expr(value, infos);
                }
            }
            TExprKind::Closure { .. } => {}
            _ => walk_expr(&mut AsyncLocalInfoCollector { checker: self, infos }, expr),
        }
    }

    fn async_collect_pattern_infos(
        &self,
        pattern: &TPattern,
        infos: &mut HashMap<LocalId, AsyncLocalInfo>,
    ) {
        let mut bindings = Vec::new();
        pattern.collect_bindings(&mut bindings);
        for (local_id, name, _, ty) in bindings {
            infos.insert(
                *local_id,
                AsyncLocalInfo {
                    name: name.clone(),
                    ty: ty.clone(),
                    static_const_slice: false,
                },
            );
        }
    }

    fn async_live_before_block(
        &mut self,
        block: &TBlock,
        mut live: HashSet<LocalId>,
        infos: &HashMap<LocalId, AsyncLocalInfo>,
    ) -> HashSet<LocalId> {
        for stmt in block.statements.iter().rev() {
            live = self.async_live_before_stmt(stmt, live, infos);
        }
        live
    }

    fn async_live_before_stmt(
        &mut self,
        stmt: &TStmt,
        live_after: HashSet<LocalId>,
        infos: &HashMap<LocalId, AsyncLocalInfo>,
    ) -> HashSet<LocalId> {
        match &stmt.kind {
            TStmtKind::Block(block) => self.async_live_before_block(block, live_after, infos),
            TStmtKind::VarDecl { local_id, init, .. } => {
                let mut live = live_after;
                live.remove(local_id);
                if let Some(init) = init {
                    self.async_validate_awaits_in_expr(init, &live, infos);
                    live.extend(Self::async_expr_used_locals(init));
                }
                live
            }
            TStmtKind::Assign { target, value } => {
                let mut live = live_after;
                self.async_validate_awaits_in_expr(value, &live, infos);
                self.async_validate_awaits_in_expr(target, &live, infos);
                live.extend(Self::async_expr_used_locals(value));
                live.extend(Self::async_expr_used_locals(target));
                live
            }
            TStmtKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                let then_live = self.async_live_before_block(then_block, live_after.clone(), infos);
                let else_live = else_branch
                    .as_ref()
                    .map(|stmt| self.async_live_before_stmt(stmt, live_after.clone(), infos))
                    .unwrap_or_else(|| live_after.clone());
                let mut live = then_live;
                live.extend(else_live);
                self.async_validate_awaits_in_expr(cond, &live, infos);
                live.extend(Self::async_expr_used_locals(cond));
                live
            }
            TStmtKind::While { cond, body } => {
                let mut loop_live = live_after.clone();
                loop_live.extend(Self::async_expr_used_locals(cond));
                for _ in 0..2 {
                    let body_live = self.async_live_before_block(body, loop_live.clone(), infos);
                    let old_len = loop_live.len();
                    loop_live.extend(body_live);
                    loop_live.extend(Self::async_expr_used_locals(cond));
                    if loop_live.len() == old_len {
                        break;
                    }
                }
                self.async_validate_awaits_in_expr(cond, &loop_live, infos);
                loop_live
            }
            TStmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                let mut live = live_after;
                if let Some(step) = step {
                    live = self.async_live_before_for_init(step, live, infos);
                }
                if let Some(cond) = cond {
                    self.async_validate_awaits_in_expr(cond, &live, infos);
                    live.extend(Self::async_expr_used_locals(cond));
                }
                live = self.async_live_before_block(body, live, infos);
                if let Some(init) = init {
                    live = self.async_live_before_for_init(init, live, infos);
                }
                live
            }
            TStmtKind::Switch {
                expr,
                cases,
                default,
                ..
            } => {
                let mut live = HashSet::new();
                for case in cases {
                    let mut case_live = live_after.clone();
                    for stmt in case.statements.iter().rev() {
                        case_live = self.async_live_before_stmt(stmt, case_live, infos);
                    }
                    let mut bindings = Vec::new();
                    case.pattern.collect_bindings(&mut bindings);
                    for (local_id, _, _, _) in bindings {
                        case_live.remove(local_id);
                    }
                    live.extend(case_live);
                }
                let mut default_live = live_after;
                for stmt in default.iter().rev() {
                    default_live = self.async_live_before_stmt(stmt, default_live, infos);
                }
                live.extend(default_live);
                self.async_validate_awaits_in_expr(expr, &live, infos);
                live.extend(Self::async_expr_used_locals(expr));
                live
            }
            TStmtKind::Defer(expr) | TStmtKind::Expr(expr) => {
                let mut live = live_after;
                self.async_validate_awaits_in_expr(expr, &live, infos);
                live.extend(Self::async_expr_used_locals(expr));
                live
            }
            TStmtKind::Return(Some(expr)) => {
                let live = HashSet::new();
                self.async_validate_awaits_in_expr(expr, &live, infos);
                Self::async_expr_used_locals(expr)
            }
            TStmtKind::Return(None)
            | TStmtKind::Break
            | TStmtKind::Continue
            | TStmtKind::Unsupported => HashSet::new(),
        }
    }

    fn async_live_before_for_init(
        &mut self,
        init: &TForInit,
        live_after: HashSet<LocalId>,
        infos: &HashMap<LocalId, AsyncLocalInfo>,
    ) -> HashSet<LocalId> {
        match init {
            TForInit::VarDecl { local_id, init, .. } => {
                let mut live = live_after;
                live.remove(local_id);
                if let Some(init) = init {
                    self.async_validate_awaits_in_expr(init, &live, infos);
                    live.extend(Self::async_expr_used_locals(init));
                }
                live
            }
            TForInit::Assign { target, value } => {
                let mut live = live_after;
                self.async_validate_awaits_in_expr(value, &live, infos);
                self.async_validate_awaits_in_expr(target, &live, infos);
                live.extend(Self::async_expr_used_locals(value));
                live.extend(Self::async_expr_used_locals(target));
                live
            }
            TForInit::Expr(expr) => {
                let mut live = live_after;
                self.async_validate_awaits_in_expr(expr, &live, infos);
                live.extend(Self::async_expr_used_locals(expr));
                live
            }
        }
    }

    fn async_validate_awaits_in_expr(
        &mut self,
        expr: &TExpr,
        live_after: &HashSet<LocalId>,
        infos: &HashMap<LocalId, AsyncLocalInfo>,
    ) {
        let mut live_after = live_after.clone();
        live_after.extend(Self::async_expr_used_locals(expr));
        let mut validator = AsyncAwaitValidator {
            checker: self,
            infos,
            live_after: &live_after,
        };
        validator.visit_expr(expr);
    }

    fn async_check_live_locals_at_await(
        &mut self,
        span: crate::span::Span,
        live: &HashSet<LocalId>,
        infos: &HashMap<LocalId, AsyncLocalInfo>,
    ) {
        let mut checked = HashSet::<LocalId>::new();
        for local_id in live {
            if !checked.insert(*local_id) {
                continue;
            }
            let Some(info) = infos.get(local_id) else {
                continue;
            };
            let mut visiting = HashSet::new();
            if let Some(reason) = self.async_frame_safety_violation(
                &info.ty,
                info.static_const_slice,
                &format!("local `{}`", info.name),
                &mut visiting,
            ) {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "`{}` is not async-frame-safe across `await`: {reason}",
                        info.name
                    ),
                ));
            }
        }
    }

    fn async_frame_safety_violation(
        &mut self,
        ty: &Ty,
        static_const_slice: bool,
        path: &str,
        visiting: &mut HashSet<Ty>,
    ) -> Option<String> {
        if matches!(
            ty,
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
        ) {
            return None;
        }
        if contains_generic(ty) || contains_type_hole(ty) {
            return Some(format!(
                "{path} has generic type `{ty}` without a proven async-frame-safety policy"
            ));
        }
        if self.type_implements_thread_local(ty) {
            return Some(format!("{path} has ThreadLocal type `{ty}`"));
        }
        if self.type_implements_share_handle(ty) {
            return None;
        }
        match ty {
            Ty::Pointer { nullable, .. } => {
                if *nullable {
                    Some(format!("{path} has nullable raw pointer type `{ty}`"))
                } else {
                    Some(format!("{path} has raw pointer type `{ty}`"))
                }
            }
            Ty::Slice { mutability, elem } => {
                if *mutability == ViewMutability::Writable {
                    return Some(format!("{path} has mutable slice type `{ty}`"));
                }
                if static_const_slice && matches!(&**elem, Ty::Char) {
                    None
                } else {
                    Some(format!(
                        "{path} has non-static borrowed read-only slice type `{ty}`"
                    ))
                }
            }
            Ty::Array { elem, .. } => self.async_frame_safety_violation(
                elem,
                false,
                &format!("{path} element"),
                visiting,
            ),
            Ty::Named { name, args } => {
                if name == "Future" && args.len() == 1 {
                    return None;
                }
                if !visiting.insert(ty.clone()) {
                    return None;
                }
                let instance_name = aggregate_instance_name(name, args);
                if let Some(fields) = self.structs.get(&instance_name).cloned() {
                    for (field, field_ty) in fields {
                        if let Some(reason) = self.async_frame_safety_violation(
                            &field_ty,
                            false,
                            &format!("{path}.{field}"),
                            visiting,
                        ) {
                            visiting.remove(ty);
                            return Some(reason);
                        }
                    }
                    visiting.remove(ty);
                    return None;
                }
                self.ensure_enum_instance(ty);
                if let Some(enm) = self.checked_enums.get(&instance_name).cloned() {
                    for variant in enm.variants {
                        for (idx, payload_ty) in variant.payload.iter().enumerate() {
                            if let Some(reason) = self.async_frame_safety_violation(
                                payload_ty,
                                false,
                                &format!("{path}.{}[{idx}]", variant.name),
                                visiting,
                            ) {
                                visiting.remove(ty);
                                return Some(reason);
                            }
                        }
                    }
                }
                visiting.remove(ty);
                None
            }
            Ty::ClosureInstance { captures, .. } => {
                for (idx, capture_ty) in captures.iter().enumerate() {
                    if let Some(reason) = self.async_frame_safety_violation(
                        capture_ty,
                        false,
                        &format!("{path} closure capture {idx}"),
                        visiting,
                    ) {
                        return Some(reason);
                    }
                }
                None
            }
            Ty::Closure { .. } => Some(format!("{path} has erased closure type `{ty}`")),
            Ty::DynamicInterface { .. } => {
                Some(format!("{path} has dynamic interface type `{ty}`"))
            }
            Ty::Function { .. } => Some(format!("{path} has function pointer type `{ty}`")),
            Ty::Generic(_) => Some(format!(
                "{path} has generic type `{ty}` without a proven async-frame-safety policy"
            )),
            Ty::Hole(_)
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
            | Ty::Unknown => None,
        }
    }

    fn async_is_static_const_slice_init(&self, ty: &Ty, init: Option<&TExpr>) -> bool {
        matches!(
            ty,
            Ty::Slice {
                mutability: ViewMutability::ReadOnly,
                elem
            } if matches!(&**elem, Ty::Char)
        ) && init.is_some_and(|expr| matches!(expr.kind, TExprKind::Literal(Literal::String(_))))
    }

    fn async_expr_used_locals(expr: &TExpr) -> HashSet<LocalId> {
        let mut collector = AsyncLocalUseCollector {
            locals: HashSet::new(),
        };
        collector.visit_expr(expr);
        collector.locals
    }

    fn push_control_context(&mut self, kind: ControlContextKind) {
        self.control_contexts.push(ControlContext {
            kind,
            break_scopes: Vec::new(),
        });
    }

    fn pop_control_context(&mut self) -> ControlContext {
        self.control_contexts
            .pop()
            .expect("control context stack is not empty")
    }

    fn record_break_scope(&mut self, scopes: &LocalScopes) -> bool {
        if let Some(context) = self.control_contexts.iter_mut().rev().find(|context| {
            matches!(
                context.kind,
                ControlContextKind::Loop | ControlContextKind::Switch
            )
        }) {
            context.break_scopes.push(scopes.clone());
            true
        } else {
            false
        }
    }

    fn has_continue_target(&self) -> bool {
        self.control_contexts
            .iter()
            .rev()
            .any(|context| matches!(context.kind, ControlContextKind::Loop))
    }

    fn check_block(
        &mut self,
        scopes: &mut LocalScopes,
        block: &Block,
        ret_ty: &Ty,
    ) -> Option<CheckedBlockFlow> {
        scopes.push();
        let result = self.check_block_with_existing_scope(scopes, block, ret_ty);
        scopes.pop();
        result
    }

    fn check_block_with_existing_scope(
        &mut self,
        scopes: &mut LocalScopes,
        block: &Block,
        ret_ty: &Ty,
    ) -> Option<CheckedBlockFlow> {
        let (statements, flow) = self.check_statement_sequence(scopes, &block.statements, ret_ty);
        Some(CheckedBlockFlow {
            block: TBlock {
                span: block.span,
                statements,
            },
            flow,
        })
    }

    fn check_statement_sequence(
        &mut self,
        scopes: &mut LocalScopes,
        source_statements: &[Stmt],
        ret_ty: &Ty,
    ) -> (Vec<TStmt>, Flow) {
        let mut statements = Vec::new();
        let mut flow = Flow::fallthrough();
        for stmt in source_statements {
            if let Some(checked) = self.check_stmt(scopes, stmt, ret_ty) {
                if flow.can_fallthrough {
                    flow = checked.flow;
                }
                statements.push(checked.stmt);
            }
        }
        (statements, flow)
    }

    fn check_unsafe_block_expr(
        &mut self,
        scopes: &mut LocalScopes,
        block: &ExprBlock,
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        scopes.push();
        let previous_unsafe_depth = self.unsafe_depth;
        self.unsafe_depth += 1;
        let ret_ty = self.current_return_ty.clone();
        let (statements, flow) = self.check_statement_sequence(scopes, &block.statements, &ret_ty);
        let value = if flow.can_fallthrough {
            block
                .value
                .as_ref()
                .and_then(|expr| self.check_expr(scopes, expr, expected))
                .map(Box::new)
        } else {
            None
        };
        let ty = if !flow.can_fallthrough {
            Ty::Never
        } else {
            value
                .as_ref()
                .map(|expr| expr.ty.clone())
                .unwrap_or(Ty::Void)
        };
        self.unsafe_depth = previous_unsafe_depth;
        scopes.pop();
        Some(TExpr {
            span: block.span,
            ty,
            kind: TExprKind::UnsafeBlock { statements, value },
        })
    }

    fn check_stmt(
        &mut self,
        scopes: &mut LocalScopes,
        stmt: &Stmt,
        ret_ty: &Ty,
    ) -> Option<CheckedStmtFlow> {
        let (kind, flow) = match &stmt.kind {
            StmtKind::Block(block) => {
                let checked = self.check_block(scopes, block, ret_ty)?;
                (TStmtKind::Block(checked.block), checked.flow)
            }
            StmtKind::VarDecl {
                ty,
                name,
                mutability,
                local_id,
                init,
            } => {
                let checked =
                    self.check_local_decl_init(scopes, stmt.span, ty, &name.name, init.as_ref());
                if let Err(name) = scopes.insert(
                    *local_id,
                    Binding {
                        name: name.name.clone(),
                        ty: checked.ty.clone(),
                        narrowed_ty: None,
                        init_state: InitState::from_assigned(checked.assigned),
                        mutability: *mutability,
                        captured: false,
                        declared_loop_depth: self.current_loop_depth,
                    },
                ) {
                    self.diagnostics.push(Diagnostic::new(
                        stmt.span,
                        format!("duplicate local `{name}`"),
                    ));
                }
                (
                    TStmtKind::VarDecl {
                        ty: checked.ty,
                        name: name.name.clone(),
                        local_id: *local_id,
                        init: checked.init,
                    },
                    Flow::fallthrough(),
                )
            }
            StmtKind::Assign { target, value } => {
                let checked_target = self.check_lvalue(scopes, target, false)?;
                let assignment_allowed = self.validate_assignment_target(
                    scopes,
                    &checked_target,
                    checked_target.expr.span,
                );
                let target = checked_target.expr;
                let value = self.check_expr(scopes, value, Some(&target.ty))?;
                if target.ty.is_erased_value() {
                    self.diagnostics.push(Diagnostic::new(
                        stmt.span,
                        "void values are implicit and cannot be explicitly assigned",
                    ));
                }
                self.require_assignable(&target.ty, &value.ty, stmt.span);
                if assignment_allowed {
                    self.mark_assignment_complete(scopes, &target);
                }
                (TStmtKind::Assign { target, value }, Flow::fallthrough())
            }
            StmtKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                let cond = self.check_expr(scopes, cond, Some(&Ty::Bool))?;
                self.require_assignable(&Ty::Bool, &cond.ty, cond.span);
                let before = scopes.clone();
                let mut then_scopes = before.clone();
                self.apply_condition_narrowing(&mut then_scopes, &cond, true);
                let checked_then = self.check_block(&mut then_scopes, then_block, ret_ty)?;
                let mut else_scopes = before.clone();
                self.apply_condition_narrowing(&mut else_scopes, &cond, false);
                let checked_else = else_branch
                    .as_ref()
                    .and_then(|stmt| self.check_stmt(&mut else_scopes, stmt, ret_ty));
                let else_flow = checked_else
                    .as_ref()
                    .map(|checked| checked.flow)
                    .unwrap_or_else(Flow::fallthrough);

                let mut reachable = Vec::new();
                if checked_then.flow.can_fallthrough {
                    reachable.push(then_scopes);
                }
                if else_flow.can_fallthrough {
                    reachable.push(else_scopes);
                }
                scopes.merge_reachable_flows(&reachable);
                let flow = Flow {
                    can_fallthrough: !reachable.is_empty(),
                };
                let then_block = checked_then.block;
                let else_branch = checked_else.map(|checked| Box::new(checked.stmt));
                (
                    TStmtKind::If {
                        cond,
                        then_block,
                        else_branch,
                    },
                    flow,
                )
            }
            StmtKind::While { cond, body } => {
                let cond = self.check_expr(scopes, cond, Some(&Ty::Bool))?;
                self.require_assignable(&Ty::Bool, &cond.ty, cond.span);
                let mut body_scopes = scopes.clone();
                self.push_control_context(ControlContextKind::Loop);
                self.current_loop_depth += 1;
                let checked_body = self.check_block(&mut body_scopes, body, ret_ty);
                self.current_loop_depth -= 1;
                let loop_context = self.pop_control_context();
                let checked_body = checked_body?;
                let flow = if bool_literal_is(&cond, true) && loop_context.break_scopes.is_empty() {
                    Flow::no_fallthrough()
                } else {
                    Flow::fallthrough()
                };
                (
                    TStmtKind::While {
                        cond,
                        body: checked_body.block,
                    },
                    flow,
                )
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                scopes.push();
                let init = init
                    .as_ref()
                    .and_then(|init| self.check_for_init(scopes, init));
                let cond = cond
                    .as_ref()
                    .and_then(|expr| self.check_expr(scopes, expr, Some(&Ty::Bool)));
                if let Some(cond) = &cond {
                    self.require_assignable(&Ty::Bool, &cond.ty, cond.span);
                }
                let mut loop_scopes = scopes.clone();
                self.current_loop_depth += 1;
                let step = step
                    .as_ref()
                    .and_then(|step| self.check_for_step(&mut loop_scopes, step));
                self.push_control_context(ControlContextKind::Loop);
                let checked_body = self.check_block(&mut loop_scopes, body, ret_ty);
                self.current_loop_depth -= 1;
                let loop_context = self.pop_control_context();
                let checked_body = checked_body?;
                let condition_always_true = cond
                    .as_ref()
                    .map(|cond| bool_literal_is(cond, true))
                    .unwrap_or(true);
                let flow = if condition_always_true && loop_context.break_scopes.is_empty() {
                    Flow::no_fallthrough()
                } else {
                    Flow::fallthrough()
                };
                scopes.pop();
                (
                    TStmtKind::For {
                        init,
                        cond,
                        step,
                        body: checked_body.block,
                    },
                    flow,
                )
            }
            StmtKind::Switch {
                expr,
                cases,
                has_default,
                default,
            } => self.check_switch_stmt(
                scopes,
                stmt.span,
                expr,
                cases,
                *has_default,
                default,
                ret_ty,
            )?,
            StmtKind::Defer(expr) => {
                let expr = self.check_expr(scopes, expr, None)?;
                if !matches!(expr.kind, TExprKind::Call { .. }) {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        "defer requires a direct function call",
                    ));
                }
                (TStmtKind::Defer(expr), Flow::fallthrough())
            }
            StmtKind::Return(expr) => {
                if ret_ty.is_never() {
                    self.diagnostics.push(Diagnostic::new(
                        stmt.span,
                        "`never` function cannot return normally",
                    ));
                    return Some(CheckedStmtFlow {
                        stmt: TStmt {
                            span: stmt.span,
                            kind: TStmtKind::Return(None),
                        },
                        flow: Flow::no_fallthrough(),
                    });
                }
                let expr = match expr {
                    Some(expr) => {
                        let expr = self.check_expr(scopes, expr, Some(ret_ty))?;
                        if ret_ty.is_void() && !expr.ty.is_void() {
                            self.diagnostics.push(Diagnostic::new(
                                expr.span,
                                "void function cannot return a non-void value",
                            ));
                        }
                        self.require_assignable(ret_ty, &expr.ty, expr.span);
                        Some(expr)
                    }
                    None => {
                        if !ret_ty.is_erased_value() {
                            self.diagnostics.push(Diagnostic::new(
                                stmt.span,
                                format!("function must return `{ret_ty}`"),
                            ));
                        }
                        None
                    }
                };
                (TStmtKind::Return(expr), Flow::no_fallthrough())
            }
            StmtKind::Break => {
                if !self.record_break_scope(scopes) {
                    self.diagnostics
                        .push(Diagnostic::new(stmt.span, "break outside loop or switch"));
                }
                (TStmtKind::Break, Flow::no_fallthrough())
            }
            StmtKind::Continue => {
                if !self.has_continue_target() {
                    self.diagnostics
                        .push(Diagnostic::new(stmt.span, "continue outside loop"));
                }
                (TStmtKind::Continue, Flow::no_fallthrough())
            }
            StmtKind::Expr(expr) => {
                let expr = self.check_expr(scopes, expr, None)?;
                let flow = if expr.is_never() {
                    Flow::no_fallthrough()
                } else {
                    Flow::fallthrough()
                };
                (TStmtKind::Expr(expr), flow)
            }
        };

        Some(CheckedStmtFlow {
            stmt: TStmt {
                span: stmt.span,
                kind,
            },
            flow,
        })
    }

    fn check_for_init(&mut self, scopes: &mut LocalScopes, init: &ForInit) -> Option<TForInit> {
        match init {
            ForInit::VarDecl {
                ty,
                name,
                mutability,
                local_id,
                init: initializer,
            } => {
                let checked = self.check_local_decl_init(
                    scopes,
                    name.span,
                    ty,
                    &name.name,
                    initializer.as_ref(),
                );
                let local_name = name.name.clone();
                let local_span = name.span;
                if let Err(duplicate) = scopes.insert(
                    *local_id,
                    Binding {
                        name: local_name.clone(),
                        ty: checked.ty.clone(),
                        narrowed_ty: None,
                        init_state: InitState::from_assigned(checked.assigned),
                        mutability: *mutability,
                        captured: false,
                        declared_loop_depth: self.current_loop_depth,
                    },
                ) {
                    self.diagnostics.push(Diagnostic::new(
                        local_span,
                        format!("duplicate local `{duplicate}`"),
                    ));
                }
                Some(TForInit::VarDecl {
                    ty: checked.ty,
                    name: local_name,
                    local_id: *local_id,
                    init: checked.init,
                })
            }
            ForInit::Assign { target, value } => {
                let checked_target = self.check_lvalue(scopes, target, false)?;
                let assignment_allowed = self.validate_assignment_target(
                    scopes,
                    &checked_target,
                    checked_target.expr.span,
                );
                let target = checked_target.expr;
                let value = self.check_expr(scopes, value, Some(&target.ty))?;
                if target.ty.is_erased_value() {
                    self.diagnostics.push(Diagnostic::new(
                        target.span,
                        "void values are implicit and cannot be explicitly assigned",
                    ));
                }
                self.require_assignable(&target.ty, &value.ty, value.span);
                if assignment_allowed {
                    self.mark_assignment_complete(scopes, &target);
                }
                Some(TForInit::Assign { target, value })
            }
            ForInit::Expr(expr) => self.check_expr(scopes, expr, None).map(TForInit::Expr),
        }
    }

    fn check_for_step(&mut self, scopes: &mut LocalScopes, step: &ForInit) -> Option<TForInit> {
        match step {
            ForInit::Assign { target, value } => {
                let checked_target = self.check_lvalue(scopes, target, false)?;
                let assignment_allowed = self.validate_assignment_target(
                    scopes,
                    &checked_target,
                    checked_target.expr.span,
                );
                let target = checked_target.expr;
                let value = self.check_expr(scopes, value, Some(&target.ty))?;
                if target.ty.is_erased_value() {
                    self.diagnostics.push(Diagnostic::new(
                        target.span,
                        "void values are implicit and cannot be explicitly assigned",
                    ));
                }
                self.require_assignable(&target.ty, &value.ty, value.span);
                if assignment_allowed {
                    self.mark_assignment_complete(scopes, &target);
                }
                Some(TForInit::Assign { target, value })
            }
            ForInit::Expr(expr) => self.check_expr(scopes, expr, None).map(TForInit::Expr),
            ForInit::VarDecl { ty, name, .. } => {
                self.diagnostics.push(Diagnostic::new(
                    ty.span.merge(name.span),
                    "for step cannot declare a variable",
                ));
                None
            }
        }
    }

    fn check_switch_stmt(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        expr: &Expr,
        cases: &[CaseClause],
        has_default: bool,
        default: &[Stmt],
        ret_ty: &Ty,
    ) -> Option<(TStmtKind, Flow)> {
        let expr = self.check_expr(scopes, expr, None)?;
        let Ty::Named { name, args } = &expr.ty else {
            self.diagnostics.push(Diagnostic::new(
                expr.span,
                format!("switch requires enum value, got `{}`", expr.ty),
            ));
            return Some((TStmtKind::Unsupported, Flow::fallthrough()));
        };
        let enum_type_name = enum_instance_name(name, args);
        self.ensure_enum_instance(&expr.ty);
        let Some(checked_enum) = self.checked_enums.get(&enum_type_name).cloned() else {
            self.diagnostics.push(Diagnostic::new(
                expr.span,
                format!("`{}` is not an enum type", expr.ty),
            ));
            return Some((TStmtKind::Unsupported, Flow::fallthrough()));
        };

        let before = scopes.clone();
        let mut top_patterns = Vec::new();
        let mut checked_cases = Vec::new();
        let mut reachable_after_switch = Vec::new();
        self.push_control_context(ControlContextKind::Switch);
        for case in cases {
            let Some((variant_index, pattern)) =
                self.check_case_pattern(&case.pattern, &expr.ty, &checked_enum, true)
            else {
                continue;
            };
            top_patterns.push(pattern.clone());

            let mut case_scopes = before.clone();
            case_scopes.push();
            let mut bindings = Vec::new();
            pattern.collect_bindings(&mut bindings);
            for (local_id, binding_name, mutability, binding_ty) in bindings {
                if let Err(duplicate) = case_scopes.insert(
                    *local_id,
                    Binding {
                        name: binding_name.clone(),
                        ty: binding_ty.clone(),
                        narrowed_ty: None,
                        init_state: InitState::Assigned,
                        mutability,
                        captured: false,
                        declared_loop_depth: self.current_loop_depth,
                    },
                ) {
                    self.diagnostics.push(Diagnostic::new(
                        pattern_span(&case.pattern),
                        format!("duplicate pattern binding `{duplicate}`"),
                    ));
                }
            }
            let (statements, case_flow) =
                self.check_statement_sequence(&mut case_scopes, &case.statements, ret_ty);
            case_scopes.pop();
            if case_flow.can_fallthrough {
                reachable_after_switch.push(case_scopes);
            }
            checked_cases.push(TCase {
                variant_name: checked_enum.variants[variant_index].name.clone(),
                variant_index,
                pattern,
                statements,
            });
        }

        let exhaustive = self.patterns_exhaustive_for_type(&expr.ty, &top_patterns);
        if !has_default && !exhaustive {
            self.diagnostics
                .push(Diagnostic::new(span, "switch is not exhaustive"));
        }

        let mut default_scopes = before.clone();
        default_scopes.push();
        let default_break_start = self
            .control_contexts
            .last()
            .map(|context| context.break_scopes.len())
            .unwrap_or(0);
        let (default, default_flow) =
            self.check_statement_sequence(&mut default_scopes, default, ret_ty);
        default_scopes.pop();
        if has_default && !exhaustive {
            if default_flow.can_fallthrough {
                reachable_after_switch.push(default_scopes);
            }
        } else if has_default && let Some(context) = self.control_contexts.last_mut() {
            context.break_scopes.truncate(default_break_start);
        } else if !has_default && !exhaustive {
            reachable_after_switch.push(before.clone());
        }

        let switch_context = self.pop_control_context();
        reachable_after_switch.extend(switch_context.break_scopes);
        scopes.merge_reachable_flows(&reachable_after_switch);
        let flow = Flow {
            can_fallthrough: !reachable_after_switch.is_empty(),
        };

        Some((
            TStmtKind::Switch {
                expr,
                enum_type_name,
                cases: checked_cases,
                has_default,
                default,
                can_fallthrough: flow.can_fallthrough,
            },
            flow,
        ))
    }

    fn check_case_pattern(
        &mut self,
        pattern: &Pattern,
        expected_ty: &Ty,
        checked_enum: &CheckedEnum,
        is_case_head: bool,
    ) -> Option<(usize, TPattern)> {
        let Pattern::Variant(name, _subpatterns) = pattern else {
            self.diagnostics.push(Diagnostic::new(
                pattern_span(pattern),
                "top-level wildcard pattern is not supported; use default",
            ));
            return None;
        };
        let Some(checked_pattern) = self.check_pattern(pattern, expected_ty, is_case_head) else {
            return None;
        };
        let TPattern::Variant {
            variant_index,
            variant_name,
            ..
        } = &checked_pattern
        else {
            self.diagnostics.push(Diagnostic::new(
                pattern_span(pattern),
                "switch case must name an enum variant",
            ));
            return None;
        };
        if !checked_enum
            .variants
            .iter()
            .any(|variant| variant.name == *variant_name)
        {
            self.diagnostics.push(Diagnostic::new(
                name.span,
                format!(
                    "`{}` is not a variant of `{}`",
                    name.display, checked_enum.name
                ),
            ));
            return None;
        }
        Some((*variant_index, checked_pattern))
    }

    fn check_pattern(
        &mut self,
        pattern: &Pattern,
        expected_ty: &Ty,
        is_case_head: bool,
    ) -> Option<TPattern> {
        match pattern {
            Pattern::Wildcard(span) => {
                if is_case_head {
                    self.diagnostics.push(Diagnostic::new(
                        *span,
                        "top-level wildcard pattern is not supported; use default",
                    ));
                    None
                } else {
                    Some(TPattern::Wildcard {
                        ty: expected_ty.clone(),
                    })
                }
            }
            Pattern::Variant(name, subpatterns) => match name.kind {
                PatternNameKind::Variant(_) => {
                    self.check_variant_pattern(name, subpatterns, expected_ty)
                }
                PatternNameKind::Binding {
                    local_id,
                    mutability,
                } if !is_case_head && subpatterns.is_empty() => Some(TPattern::Binding {
                    local_id,
                    name: name.display.clone(),
                    mutability,
                    ty: expected_ty.clone(),
                }),
                PatternNameKind::Binding { .. } => {
                    self.diagnostics.push(Diagnostic::new(
                        name.span,
                        "pattern binding cannot have payload patterns",
                    ));
                    None
                }
                PatternNameKind::Error => None,
            },
        }
    }

    fn check_variant_pattern(
        &mut self,
        name: &PatternName,
        subpatterns: &[Pattern],
        expected_ty: &Ty,
    ) -> Option<TPattern> {
        let PatternNameKind::Variant(def_id) = name.kind else {
            return None;
        };
        let Some(sig) = self.variants.get(&def_id).cloned() else {
            self.diagnostics.push(Diagnostic::new(
                name.span,
                format!("unknown enum variant `{}`", name.display),
            ));
            return None;
        };
        let Ty::Named {
            name: enum_name,
            args: enum_args,
        } = expected_ty
        else {
            self.diagnostics.push(Diagnostic::new(
                name.span,
                format!(
                    "variant `{}` pattern requires enum value, got `{expected_ty}`",
                    name.display
                ),
            ));
            return None;
        };
        if enum_name != &sig.enum_name {
            self.diagnostics.push(Diagnostic::new(
                name.span,
                format!(
                    "variant `{}` belongs to `{}`, not `{expected_ty}`",
                    name.display, sig.enum_name
                ),
            ));
            return None;
        }
        if enum_args.len() != sig.enum_generics.len() {
            self.diagnostics.push(Diagnostic::new(
                name.span,
                format!(
                    "enum `{enum_name}` expects {} type arguments, got {}",
                    sig.enum_generics.len(),
                    enum_args.len()
                ),
            ));
            return None;
        }
        let subst = sig
            .enum_generics
            .iter()
            .cloned()
            .zip(enum_args.iter().cloned())
            .collect::<HashMap<_, _>>();
        let logical_payload_tys = sig
            .payload
            .iter()
            .map(|ty| self.lower_type_with_subst(ty, &subst))
            .collect::<Vec<_>>();
        let physical_payload_tys = logical_payload_tys
            .iter()
            .filter(|ty| !ty.is_erased_value())
            .cloned()
            .collect::<Vec<_>>();
        let use_logical_payload = subpatterns.len() == logical_payload_tys.len();
        let use_physical_payload =
            subpatterns.len() == physical_payload_tys.len() && !use_logical_payload;
        if !use_logical_payload && !use_physical_payload {
            self.diagnostics.push(Diagnostic::new(
                name.span,
                format!(
                    "variant `{}` expects {} pattern fields, got {}",
                    name.display,
                    physical_payload_tys.len(),
                    subpatterns.len()
                ),
            ));
            return None;
        }

        let mut payload = Vec::new();
        if use_logical_payload {
            for (subpattern, payload_ty) in subpatterns.iter().zip(logical_payload_tys.iter()) {
                payload.push(self.check_pattern(subpattern, payload_ty, false)?);
            }
        } else {
            for (subpattern, payload_ty) in subpatterns.iter().zip(physical_payload_tys.iter()) {
                payload.push(self.check_pattern(subpattern, payload_ty, false)?);
            }
        }
        self.ensure_enum_instance(expected_ty);
        Some(TPattern::Variant {
            ty: expected_ty.clone(),
            enum_type_name: enum_instance_name(enum_name, enum_args),
            variant_name: name
                .path
                .last()
                .map(|ident| ident.name.clone())
                .unwrap_or_else(|| name.display.clone()),
            variant_index: sig.variant_index,
            payload,
        })
    }

    fn patterns_exhaustive_for_type(&mut self, ty: &Ty, patterns: &[TPattern]) -> bool {
        let rows = patterns
            .iter()
            .cloned()
            .map(|pattern| vec![pattern])
            .collect::<Vec<_>>();
        self.tuple_patterns_exhaustive(&[ty.clone()], &rows)
    }

    fn tuple_patterns_exhaustive(&mut self, tys: &[Ty], rows: &[Vec<TPattern>]) -> bool {
        let Some((first_ty, rest_tys)) = tys.split_first() else {
            return !rows.is_empty();
        };
        if rows.iter().any(|row| row.len() != tys.len()) {
            return false;
        }

        if let Some(checked_enum) = self.checked_enum_for_type(first_ty) {
            for variant in &checked_enum.variants {
                let mut specialized_rows = Vec::new();
                for row in rows {
                    match &row[0] {
                        TPattern::Wildcard { .. } | TPattern::Binding { .. } => {
                            let mut specialized = variant
                                .payload
                                .iter()
                                .cloned()
                                .map(|ty| TPattern::Wildcard { ty })
                                .collect::<Vec<_>>();
                            specialized.extend(row[1..].iter().cloned());
                            specialized_rows.push(specialized);
                        }
                        TPattern::Variant {
                            variant_name,
                            payload,
                            ..
                        } if variant_name == &variant.name => {
                            let mut specialized = payload
                                .iter()
                                .filter(|pattern| !pattern.ty().is_erased_value())
                                .cloned()
                                .collect::<Vec<_>>();
                            specialized.extend(row[1..].iter().cloned());
                            specialized_rows.push(specialized);
                        }
                        TPattern::Variant { .. } => {}
                    }
                }
                let mut specialized_tys = variant.payload.clone();
                specialized_tys.extend_from_slice(rest_tys);
                if !self.tuple_patterns_exhaustive(&specialized_tys, &specialized_rows) {
                    return false;
                }
            }
            true
        } else {
            let rest_rows = rows
                .iter()
                .filter_map(|row| match row[0] {
                    TPattern::Wildcard { .. } | TPattern::Binding { .. } => Some(row[1..].to_vec()),
                    TPattern::Variant { .. } => None,
                })
                .collect::<Vec<_>>();
            self.tuple_patterns_exhaustive(rest_tys, &rest_rows)
        }
    }

    fn checked_enum_for_type(&mut self, ty: &Ty) -> Option<CheckedEnum> {
        let Ty::Named { name, args } = ty else {
            return None;
        };
        self.ensure_enum_instance(ty);
        let instance_name = enum_instance_name(name, args);
        self.checked_enums.get(&instance_name).cloned()
    }

    fn check_expr(
        &mut self,
        scopes: &mut LocalScopes,
        expr: &Expr,
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        let result = self.check_expr_uncoerced(scopes, expr, expected)?;
        Some(self.coerce_expr_to_expected(scopes, result, expected))
    }

    fn check_expr_uncoerced(
        &mut self,
        scopes: &mut LocalScopes,
        expr: &Expr,
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        let result = match &expr.kind {
            ExprKind::Name(name_ref) => {
                if let Some(local_id) = self.resolved_local_id(name_ref)
                    && let Some(binding) = scopes.get(local_id)
                {
                    let name = binding.name.clone();
                    if !binding.init_state.is_assigned() {
                        self.diagnostics.push(Diagnostic::new(
                            expr.span,
                            format!("local `{name}` is not definitely assigned"),
                        ));
                    }
                    TExpr {
                        span: expr.span,
                        ty: scopes
                            .effective_ty(local_id)
                            .unwrap_or_else(|| binding.ty.clone()),
                        kind: TExprKind::Local(local_id, name),
                    }
                } else if let Some(sig) = self.resolve_function_name(name_ref) {
                    if sig.is_unsafe {
                        self.require_unsafe(
                            expr.span,
                            format!(
                                "use of unsafe function `{}` as a value requires unsafe block",
                                sig.name
                            ),
                        );
                    }
                    TExpr {
                        span: expr.span,
                        ty: Ty::Function {
                            is_unsafe: sig.is_unsafe,
                            abi: sig.abi.clone(),
                            ret: Box::new(sig.ret.clone()),
                            params: sig.params.clone(),
                        },
                        kind: TExprKind::Function(sig.def_id, sig.name.clone()),
                    }
                } else if let Some((def_id, sig)) = self.lookup_variant_name(name_ref) {
                    let variant_name = self.resolved.def(def_id).name.clone();
                    self.check_variant_literal(
                        scopes,
                        expr.span,
                        &variant_name,
                        sig,
                        Vec::new(),
                        expected,
                    )?
                } else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!("unresolved name `{}`", name_ref.display),
                    ));
                    return None;
                }
            }
            ExprKind::Literal(literal) => self.check_literal(expr.span, literal, expected)?,
            ExprKind::StructLiteral(fields) => {
                let Some(Ty::Named {
                    name: type_name,
                    args,
                }) = expected
                else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        "struct literal requires an expected struct type",
                    ));
                    return None;
                };
                let instance_name = enum_instance_name(type_name, args);
                let struct_fields = if let Some(fields) = self.structs.get(&instance_name).cloned()
                {
                    fields
                } else if let Some(template) = self.struct_templates.get(type_name).cloned() {
                    if template.generics.len() != args.len() {
                        self.diagnostics.push(Diagnostic::new(
                            expr.span,
                            format!(
                                "struct `{type_name}` expects {} type arguments, got {}",
                                template.generics.len(),
                                args.len()
                            ),
                        ));
                        return None;
                    }
                    let subst = template
                        .generics
                        .iter()
                        .cloned()
                        .zip(args.iter().cloned())
                        .collect::<HashMap<_, _>>();
                    template
                        .fields
                        .iter()
                        .map(|field| {
                            (
                                field.name.name.clone(),
                                self.lower_type_with_subst_allowing_holes(&field.ty, &subst),
                            )
                        })
                        .collect()
                } else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!("`{}` is not a known struct", expected.unwrap()),
                    ));
                    return None;
                };
                if self.is_unsafe_struct_instance(type_name, args) {
                    self.require_unsafe(
                        expr.span,
                        format!("constructing unsafe struct `{type_name}` requires unsafe block"),
                    );
                }
                let mut seen = HashMap::<String, ()>::new();
                let mut checked_fields = Vec::new();
                for init in fields {
                    if seen.insert(init.name.name.clone(), ()).is_some() {
                        self.diagnostics.push(Diagnostic::new(
                            init.name.span,
                            format!("duplicate field `{}`", init.name.name),
                        ));
                    }
                    let Some((_, field_ty)) = struct_fields
                        .iter()
                        .find(|(field_name, _)| field_name == &init.name.name)
                    else {
                        self.diagnostics.push(Diagnostic::new(
                            init.name.span,
                            format!("unknown field `{}` on `{type_name}`", init.name.name),
                        ));
                        continue;
                    };
                    let field_ty = self.resolve_type_holes(field_ty);
                    if field_ty.is_erased_value() {
                        self.diagnostics.push(Diagnostic::new(
                            init.name.span,
                            "void fields are implicit and cannot be explicitly initialized",
                        ));
                        continue;
                    }
                    let value = self.check_expr(scopes, &init.expr, Some(&field_ty))?;
                    self.require_assignable(&field_ty, &value.ty, init.expr.span);
                    checked_fields.push((init.name.name.clone(), value));
                }
                for (field_name, field_ty) in &struct_fields {
                    if !field_ty.is_erased_value() && !seen.contains_key(field_name) {
                        self.diagnostics.push(Diagnostic::new(
                            expr.span,
                            format!("missing field `{field_name}` in `{type_name}` literal"),
                        ));
                    }
                }
                let ty = self.resolve_type_holes(&Ty::Named {
                    name: type_name.clone(),
                    args: args.clone(),
                });
                self.ensure_struct_instance(&ty);
                let Ty::Named {
                    name: concrete_name,
                    args: concrete_args,
                } = &ty
                else {
                    unreachable!("struct literal expected type is named");
                };
                let instance_name = enum_instance_name(concrete_name, concrete_args);
                TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::StructLiteral {
                        type_name: instance_name,
                        fields: checked_fields,
                    },
                }
            }
            ExprKind::ArrayLiteral(elements) => {
                let (elem_ty, result_ty) = match expected {
                    Some(Ty::Array { len, elem }) => {
                        if *len != elements.len() {
                            self.diagnostics.push(Diagnostic::new(
                                expr.span,
                                format!(
                                    "array literal has {} elements, expected {len}",
                                    elements.len()
                                ),
                            ));
                        }
                        ((**elem).clone(), expected.cloned().unwrap())
                    }
                    Some(Ty::Slice { mutability, elem }) => (
                        (**elem).clone(),
                        Ty::Slice {
                            mutability: *mutability,
                            elem: elem.clone(),
                        },
                    ),
                    _ => {
                        self.diagnostics.push(Diagnostic::new(
                            expr.span,
                            "array literal requires an expected array or slice type",
                        ));
                        return None;
                    }
                };
                let checked_elements = elements
                    .iter()
                    .filter_map(|element| self.check_expr(scopes, element, Some(&elem_ty)))
                    .collect::<Vec<_>>();
                for element in &checked_elements {
                    self.require_assignable(&elem_ty, &element.ty, element.span);
                }
                TExpr {
                    span: expr.span,
                    ty: result_ty,
                    kind: TExprKind::ArrayLiteral(checked_elements),
                }
            }
            ExprKind::ArrayRepeat { element, len } => {
                let (elem_ty, result_ty, resolved_len) = match expected {
                    Some(Ty::Array {
                        len: expected_len,
                        elem,
                    }) => {
                        if len.is_some() {
                            self.diagnostics.push(Diagnostic::new(
                                expr.span,
                                "array repeat literal in fixed array context must omit the length",
                            ));
                        }
                        ((**elem).clone(), expected.cloned().unwrap(), *expected_len)
                    }
                    Some(Ty::Slice { mutability, elem }) => {
                        let Some(len) = len else {
                            self.diagnostics.push(Diagnostic::new(
                                expr.span,
                                "array repeat literal with omitted length requires an expected array type",
                            ));
                            return None;
                        };
                        (
                            (**elem).clone(),
                            Ty::Slice {
                                mutability: *mutability,
                                elem: elem.clone(),
                            },
                            *len,
                        )
                    }
                    _ => {
                        self.diagnostics.push(Diagnostic::new(
                            expr.span,
                            "array repeat literal requires an expected array or slice type",
                        ));
                        return None;
                    }
                };
                let checked_element = self.check_expr(scopes, element, Some(&elem_ty))?;
                self.require_assignable(&elem_ty, &checked_element.ty, checked_element.span);
                TExpr {
                    span: expr.span,
                    ty: result_ty,
                    kind: TExprKind::ArrayRepeat {
                        element: Box::new(checked_element),
                        len: resolved_len,
                    },
                }
            }
            ExprKind::Closure {
                is_async,
                params,
                body,
            } => self.check_closure_expr(scopes, expr.span, *is_async, params, body, expected)?,
            ExprKind::Unary { op, expr: inner } => {
                if matches!(op, UnaryOp::Neg)
                    && let ExprKind::Literal(Literal::Integer(raw)) = &inner.kind
                    && let Some(expected_ty) = expected
                    && expected_ty.is_signed_integer()
                {
                    self.check_integer_literal_range(inner.span, raw, expected_ty, true);
                    let inner = TExpr {
                        span: inner.span,
                        ty: expected_ty.clone(),
                        kind: TExprKind::Literal(Literal::Integer(raw.clone())),
                    };
                    return Some(TExpr {
                        span: expr.span,
                        ty: expected_ty.clone(),
                        kind: TExprKind::Unary {
                            op: *op,
                            expr: Box::new(inner),
                        },
                    });
                }
                let inner = match op {
                    UnaryOp::Addr => {
                        let inner = self.check_lvalue(scopes, inner, true)?;
                        if let Some(ReadOnlyReason::CapturedBinding(name)) =
                            inner.read_only_reason.as_ref()
                        {
                            self.diagnostics.push(Diagnostic::new(
                                inner.expr.span,
                                format!("cannot take address of captured binding `{name}`"),
                            ));
                        }
                        if inner.expr.ty.is_erased_value() {
                            self.diagnostics.push(Diagnostic::new(
                                inner.expr.span,
                                "cannot take the address of a void value",
                            ));
                        }
                        if let TExprKind::Local(local_id, _) = &inner.expr.kind {
                            scopes.clear_narrowing(*local_id);
                        }
                        inner
                    }
                    UnaryOp::Neg => CheckedLvalue::writable(self.check_expr(
                        scopes,
                        inner,
                        expected.filter(|ty| ty.is_numeric()),
                    )?),
                    UnaryOp::BitNot => CheckedLvalue::writable(self.check_expr(
                        scopes,
                        inner,
                        expected.filter(|ty| ty.is_integer()),
                    )?),
                    _ => CheckedLvalue::writable(self.check_expr(scopes, inner, None)?),
                };
                let ty = match op {
                    UnaryOp::Not => {
                        self.require_assignable(&Ty::Bool, &inner.expr.ty, inner.expr.span);
                        Ty::Bool
                    }
                    UnaryOp::Neg => {
                        if !(inner.expr.ty.is_signed_integer()
                            || matches!(inner.expr.ty, Ty::F32 | Ty::F64))
                        {
                            self.diagnostics.push(Diagnostic::new(
                                inner.expr.span,
                                format!("cannot negate `{}`", inner.expr.ty),
                            ));
                        }
                        inner.expr.ty.clone()
                    }
                    UnaryOp::BitNot => {
                        if !inner.expr.ty.is_integer() {
                            self.diagnostics.push(Diagnostic::new(
                                inner.expr.span,
                                format!("bitwise not does not accept `{}`", inner.expr.ty),
                            ));
                        }
                        inner.expr.ty.clone()
                    }
                    UnaryOp::Addr => inner.access.pointer_ty(inner.expr.ty.clone()),
                    UnaryOp::Deref => match &inner.expr.ty {
                        Ty::Pointer {
                            nullable: false,
                            inner,
                            ..
                        } => (**inner).clone(),
                        Ty::Pointer { nullable: true, .. } => {
                            self.diagnostics.push(Diagnostic::new(
                                inner.expr.span,
                                "cannot dereference nullable pointer without narrowing",
                            ));
                            Ty::Unknown
                        }
                        _ => {
                            self.diagnostics.push(Diagnostic::new(
                                inner.expr.span,
                                format!("cannot dereference `{}`", inner.expr.ty),
                            ));
                            Ty::Unknown
                        }
                    },
                };
                TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Unary {
                        op: *op,
                        expr: Box::new(inner.expr),
                    },
                }
            }
            ExprKind::Binary { op, left, right } => {
                let (left, right) = if matches!(op, BinaryOp::And) {
                    let left = self.check_expr(scopes, left, Some(&Ty::Bool))?;
                    self.require_assignable(&Ty::Bool, &left.ty, left.span);
                    let mut right_scopes = scopes.clone();
                    self.apply_condition_narrowing(&mut right_scopes, &left, true);
                    let right = self.check_expr(&mut right_scopes, right, Some(&Ty::Bool))?;
                    (left, right)
                } else if op.is_equality() && matches!(left.kind, ExprKind::Literal(Literal::Null))
                {
                    let right = self.check_expr(scopes, right, None)?;
                    let left = self.check_expr(scopes, left, Some(&right.ty))?;
                    (left, right)
                } else {
                    let left = self.check_expr(scopes, left, None)?;
                    let right_expected = if op.is_shift() { None } else { Some(&left.ty) };
                    let right = self.check_expr(scopes, right, right_expected)?;
                    (left, right)
                };
                let ty = self.check_binary(*op, &left, &right, expr.span);
                TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Binary {
                        op: *op,
                        left: Box::new(left),
                        right: Box::new(right),
                    },
                }
            }
            ExprKind::Cast { expr: inner, ty } => {
                let target = self.lower_type(ty);
                if let ExprKind::Closure {
                    is_async,
                    params,
                    body,
                } = &inner.kind
                {
                    self.check_closure_cast_allowed(&target, expr.span);
                    let checked = self.check_closure_expr(
                        scopes,
                        inner.span,
                        *is_async,
                        params,
                        body,
                        Some(&target),
                    )?;
                    let checked = TExpr {
                        span: expr.span,
                        ..checked
                    };
                    return Some(self.coerce_expr_to_expected(scopes, checked, Some(&target)));
                }
                let literal_expected = match (&inner.kind, &target) {
                    (ExprKind::Literal(Literal::Integer(_)), ty)
                        if ty.is_integer() || matches!(ty, Ty::Char | Ty::CSpelling { .. }) =>
                    {
                        true
                    }
                    (
                        ExprKind::StructLiteral(_)
                        | ExprKind::ArrayLiteral(_)
                        | ExprKind::ArrayRepeat { .. }
                        | ExprKind::Literal(Literal::Null),
                        _,
                    ) => true,
                    _ => false,
                };
                let inner = self.check_expr(scopes, inner, literal_expected.then_some(&target))?;
                self.check_cast_allowed(&inner.ty, &target, expr.span);
                self.require_unsafe_pointer_cast_through_void(&inner.ty, &target, expr.span);
                TExpr {
                    span: expr.span,
                    ty: target.clone(),
                    kind: TExprKind::Cast {
                        expr: Box::new(inner),
                        ty: target,
                    },
                }
            }
            ExprKind::UnsafeBlock(block) => {
                self.check_unsafe_block_expr(scopes, block, expected)?
            }
            ExprKind::Call {
                callee,
                type_args,
                args,
            } => {
                if let ExprKind::Name(name_ref) = &callee.kind
                    && !matches!(name_ref.kind, NameRefKind::Local(_))
                    && let Some((def_id, sig)) = self.lookup_variant_name(name_ref)
                {
                    let variant_name = self.resolved.def(def_id).name.clone();
                    return self.check_variant_literal(
                        scopes,
                        expr.span,
                        &variant_name,
                        sig,
                        args.clone(),
                        expected,
                    );
                }
                if let ExprKind::Name(name_ref) = &callee.kind
                    && !matches!(name_ref.kind, NameRefKind::Local(_))
                    && let Some(def_id) = self.lookup_interface_name(name_ref)
                {
                    return self.check_interface_call(
                        scopes, expr.span, def_id, type_args, args, expected,
                    );
                }
                if let ExprKind::Name(name_ref) = &callee.kind
                    && !matches!(name_ref.kind, NameRefKind::Local(_))
                    && let Some(sig) = self.resolve_function_name(name_ref)
                {
                    return self.check_direct_function_call(
                        scopes, expr.span, sig, type_args, args, expected,
                    );
                }
                if !type_args.is_empty() {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        "type arguments can only be used on generic function or interface calls",
                    ));
                    return None;
                }
                let callee = self.check_expr(scopes, callee, None)?;
                if matches!(
                    &callee.ty,
                    Ty::Function {
                        is_unsafe: true,
                        ..
                    }
                ) {
                    self.require_unsafe(
                        callee.span,
                        "call to unsafe function value requires unsafe block",
                    );
                }
                let (ret, params) = match &callee.ty {
                    Ty::Function { ret, params, .. }
                    | Ty::Closure { ret, params, .. }
                    | Ty::ClosureInstance { ret, params, .. } => ((**ret).clone(), params.clone()),
                    _ => {
                        self.diagnostics.push(Diagnostic::new(
                            callee.span,
                            format!("`{}` is not callable", callee.ty),
                        ));
                        return None;
                    }
                };
                if params.len() != args.len() {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!(
                            "call expects {} arguments, got {}",
                            params.len(),
                            args.len()
                        ),
                    ));
                }
                let mut checked_args = Vec::new();
                for (idx, arg) in args.iter().enumerate() {
                    let expected = params.get(idx);
                    let checked = self.check_expr(scopes, arg, expected)?;
                    if let Some(expected) = expected {
                        self.require_assignable(expected, &checked.ty, arg.span);
                    }
                    checked_args.push(checked);
                }
                TExpr {
                    span: expr.span,
                    ty: ret,
                    kind: TExprKind::Call {
                        callee: Box::new(callee),
                        args: checked_args,
                    },
                }
            }
            ExprKind::Field { base, field } => {
                let base = self.check_expr(scopes, base, None)?;
                let ty = self.field_ty(&base.ty, &field.name, field.span)?;
                TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Field {
                        base: Box::new(base),
                        field: field.name.clone(),
                    },
                }
            }
            ExprKind::Arrow { base, field } => {
                let base = self.check_expr(scopes, base, None)?;
                let Ty::Pointer {
                    nullable: false,
                    inner,
                    ..
                } = &base.ty
                else {
                    self.diagnostics.push(Diagnostic::new(
                        base.span,
                        format!("`->` requires non-null pointer, got `{}`", base.ty),
                    ));
                    return None;
                };
                let ty = self.field_ty(inner, &field.name, field.span)?;
                TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Arrow {
                        base: Box::new(base),
                        field: field.name.clone(),
                    },
                }
            }
            ExprKind::Index { base, index } => {
                let base = self.check_expr(scopes, base, None)?;
                let index = self.check_expr(scopes, index, Some(&Ty::Usize))?;
                if !index.ty.is_integer() {
                    self.diagnostics
                        .push(Diagnostic::new(index.span, "index must be integer"));
                }
                let ty = match &base.ty {
                    Ty::Array { elem, .. } | Ty::Slice { elem, .. } => (**elem).clone(),
                    _ => {
                        self.diagnostics.push(Diagnostic::new(
                            base.span,
                            format!("cannot index `{}`", base.ty),
                        ));
                        Ty::Unknown
                    }
                };
                TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Index {
                        base: Box::new(base),
                        index: Box::new(index),
                    },
                }
            }
            ExprKind::Slice { base, start, end } => {
                let base = self.check_expr(scopes, base, None)?;
                let start = match start {
                    Some(start) => {
                        let start = self.check_expr(scopes, start, Some(&Ty::Usize))?;
                        if !start.ty.is_integer() {
                            self.diagnostics
                                .push(Diagnostic::new(start.span, "slice start must be integer"));
                        }
                        Some(Box::new(start))
                    }
                    None => None,
                };
                let end = match end {
                    Some(end) => {
                        let end = self.check_expr(scopes, end, Some(&Ty::Usize))?;
                        if !end.ty.is_integer() {
                            self.diagnostics
                                .push(Diagnostic::new(end.span, "slice end must be integer"));
                        }
                        Some(Box::new(end))
                    }
                    None => None,
                };
                let ty = match &base.ty {
                    Ty::Array { elem, .. } => Ty::Slice {
                        mutability: match self.texpr_lvalue_access(scopes, &base) {
                            Some(LvalueAccess::Writable) => ViewMutability::Writable,
                            _ => ViewMutability::ReadOnly,
                        },
                        elem: elem.clone(),
                    },
                    Ty::Slice { mutability, elem } => Ty::Slice {
                        mutability: *mutability,
                        elem: elem.clone(),
                    },
                    _ => {
                        self.diagnostics.push(Diagnostic::new(
                            base.span,
                            format!("cannot slice `{}`", base.ty),
                        ));
                        Ty::Unknown
                    }
                };
                TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Slice {
                        base: Box::new(base),
                        start,
                        end,
                    },
                }
            }
            ExprKind::Try(inner) => {
                let inner = self.check_expr(scopes, inner, None)?;
                let Some((ok_ty, err_ty)) = self.result_ok_err_tys(&inner.ty) else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!(
                            "`?` requires `/std/result` Result<T, E>, got `{}`",
                            inner.ty
                        ),
                    ));
                    return Some(TExpr {
                        span: expr.span,
                        ty: Ty::Unknown,
                        kind: TExprKind::Try {
                            expr: Box::new(inner),
                            propagation: TryPropagation::Exact,
                        },
                    });
                };
                let Some((_, return_err_ty)) = self.result_ok_err_tys(&self.current_return_ty)
                else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!(
                            "`?` requires enclosing function to return `/std/result` Result<_, {}>",
                            err_ty
                        ),
                    ));
                    return Some(TExpr {
                        span: expr.span,
                        ty: Ty::Unknown,
                        kind: TExprKind::Try {
                            expr: Box::new(inner),
                            propagation: TryPropagation::Exact,
                        },
                    });
                };
                let propagation = if err_ty == return_err_ty {
                    TryPropagation::Exact
                } else if self.is_std_error_ty(&return_err_ty)
                    && self.type_implements_std_error_trait(&err_ty)
                {
                    TryPropagation::ErrorBox
                } else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        if self.is_std_error_ty(&return_err_ty) {
                            format!(
                                "`?` cannot convert error type `{err_ty}` to `{return_err_ty}` because `{err_ty}` does not implement `{STD_ERROR_FORMAT_INTERFACE}`"
                            )
                        } else {
                            format!(
                                "`?` error type mismatch: expected `{return_err_ty}`, got `{err_ty}`"
                            )
                        },
                    ));
                    TryPropagation::Exact
                };
                TExpr {
                    span: expr.span,
                    ty: ok_ty,
                    kind: TExprKind::Try {
                        expr: Box::new(inner),
                        propagation,
                    },
                }
            }
            ExprKind::Await(inner) => {
                if self.current_async_depth == 0 {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        "`await` is allowed only inside async functions or async closures",
                    ));
                }
                let future = self.check_expr(scopes, inner, None)?;
                let Some(output_ty) = self.future_output_ty(&future.ty) else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!("`await` requires `Future<T>`, got `{}`", future.ty),
                    ));
                    return Some(TExpr {
                        span: expr.span,
                        ty: Ty::Unknown,
                        kind: TExprKind::Await {
                            future: Box::new(future),
                        },
                    });
                };
                TExpr {
                    span: expr.span,
                    ty: output_ty,
                    kind: TExprKind::Await {
                        future: Box::new(future),
                    },
                }
            }
        };

        Some(result)
    }

    fn check_actor_spawn_cloned_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        if args.len() != 2 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("spawn_actor_cloned expects 2 arguments, got {}", args.len()),
            ));
            return None;
        }
        let current_subst = self.current_type_subst();
        let explicit_args = type_args
            .iter()
            .map(|arg| {
                self.lower_type_with_subst_preserving_meta_repr_markers(arg, &current_subst, false)
            })
            .collect::<Vec<_>>();
        if explicit_args.len() > 2 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "spawn_actor_cloned accepts at most S and M type arguments",
            ));
            return None;
        }

        let explicit_state_ty = explicit_args
            .first()
            .map(|ty| self.normalize_meta_repr_markers(ty, span));
        let explicit_handle_message_ty = explicit_args.get(1).cloned();
        let explicit_message_ty = explicit_handle_message_ty
            .as_ref()
            .map(|ty| self.normalize_meta_repr_markers(ty, span));

        let initial_state = self.check_expr(scopes, &args[0], explicit_state_ty.as_ref())?;
        let state_ty = explicit_state_ty.unwrap_or_else(|| initial_state.ty.clone());
        self.require_assignable(&state_ty, &initial_state.ty, initial_state.span);

        let mut prechecked_handler = None;
        let mut handle_message_ty = explicit_handle_message_ty
            .clone()
            .or_else(|| self.actor_message_ty_from_spawn_expected(expected))
            .or_else(|| self.actor_message_ty_from_closure_literal(&args[1]));
        if handle_message_ty.is_none() && !expr_is_closure_literal(&args[1]) {
            let handler = self.check_expr(scopes, &args[1], None)?;
            handle_message_ty =
                callable_ret_params_ty(&handler.ty).and_then(|(_, params)| params.get(1).cloned());
            prechecked_handler = Some(handler);
        }
        let Some(handle_message_ty) = handle_message_ty else {
            self.diagnostics.push(Diagnostic::new(
                span,
                "could not infer actor message type; add spawn_actor_cloned<S, M> type arguments, an expected Actor<M>, or handler parameter types",
            ));
            return None;
        };
        let message_ty = explicit_message_ty
            .unwrap_or_else(|| self.normalize_meta_repr_markers(&handle_message_ty, span));

        let handler_state_ty = self.normalize_meta_repr_markers(&state_ty, span);
        let handler_ret = std_result_ty(handler_state_ty.clone(), std_error_ty());
        let message_view = self.interface_view("Message", &[]);
        let expected_handler_ty = Ty::Closure {
            ret: Box::new(handler_ret.clone()),
            params: vec![handler_state_ty.clone(), message_ty.clone()],
            constraints: ConstraintBounds {
                positive: message_view.positive,
                negative: message_view.negative,
            },
        };
        let handler = if let Some(handler) = prechecked_handler {
            self.coerce_expr_to_expected(scopes, handler, Some(&expected_handler_ty))
        } else if let ExprKind::Closure {
            is_async: false,
            params,
            body,
        } = &args[1].kind
        {
            let handler = self.check_closure_expr(
                scopes,
                args[1].span,
                false,
                params,
                body,
                Some(&expected_handler_ty),
            )?;
            self.coerce_expr_to_expected(scopes, handler, Some(&expected_handler_ty))
        } else {
            self.check_expr(scopes, &args[1], Some(&expected_handler_ty))?
        };
        self.require_actor_handler_callable(
            &handler.ty,
            &handler_state_ty,
            &message_ty,
            &handler_ret,
            handler.span,
        );

        if !self.type_implements_message(&handler_state_ty) {
            self.diagnostics.push(Diagnostic::new(
                initial_state.span,
                format!("actor state type `{state_ty}` does not implement `Message`"),
            ));
        }
        if !self.type_implements_message(&message_ty) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("actor message type `{message_ty}` does not implement `Message`"),
            ));
        }
        let ret = std_result_ty(std_actor_ty(handle_message_ty.clone()), std_error_ty());
        self.ensure_struct_instance(&ret);
        self.ensure_enum_instance(&ret);
        Some(TExpr {
            span,
            ty: ret,
            kind: TExprKind::ActorSpawn {
                mode: ActorSpawnMode::Cloned,
                state_arg: Box::new(initial_state),
                handler_ty: handler.ty.clone(),
                handler: Box::new(handler),
                state_ty: handler_state_ty,
                handle_message_ty,
                message_ty,
            },
        })
    }

    fn check_actor_spawn_state_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        if args.len() != 2 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("spawn_actor_state expects 2 arguments, got {}", args.len()),
            ));
            return None;
        }
        let current_subst = self.current_type_subst();
        let explicit_args = type_args
            .iter()
            .map(|arg| {
                self.lower_type_with_subst_preserving_meta_repr_markers(arg, &current_subst, false)
            })
            .collect::<Vec<_>>();
        if explicit_args.len() > 2 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "spawn_actor_state accepts at most S and M type arguments",
            ));
            return None;
        }

        let explicit_state_ty = explicit_args
            .first()
            .map(|ty| self.normalize_meta_repr_markers(ty, span));
        let explicit_handle_message_ty = explicit_args.get(1).cloned();
        let explicit_message_ty = explicit_handle_message_ty
            .as_ref()
            .map(|ty| self.normalize_meta_repr_markers(ty, span));

        let mut prechecked_init = None;
        let state_ty = if let Some(state_ty) = explicit_state_ty {
            state_ty
        } else {
            let init = self.check_expr(scopes, &args[0], None)?;
            let Some((ret, params)) = callable_ret_params_ty(&init.ty) else {
                self.diagnostics.push(Diagnostic::new(
                    init.span,
                    format!("actor state initializer `{}` is not callable", init.ty),
                ));
                return None;
            };
            if !params.is_empty() {
                self.diagnostics.push(Diagnostic::new(
                    init.span,
                    format!(
                        "actor state initializer expects 0 parameters, got {}",
                        params.len()
                    ),
                ));
                return None;
            }
            let Some((ok_ty, err_ty)) = self.result_ok_err_tys(&ret) else {
                self.diagnostics.push(Diagnostic::new(
                    init.span,
                    format!("actor state initializer must return `Result<S, Error>`, got `{ret}`"),
                ));
                return None;
            };
            if err_ty != std_error_ty() {
                self.diagnostics.push(Diagnostic::new(
                    init.span,
                    format!("actor state initializer error type must be `Error`, got `{err_ty}`"),
                ));
                return None;
            }
            prechecked_init = Some(init);
            self.normalize_meta_repr_markers(&ok_ty, span)
        };

        let mut prechecked_handler = None;
        let mut handle_message_ty = explicit_handle_message_ty
            .clone()
            .or_else(|| self.actor_message_ty_from_spawn_expected(expected))
            .or_else(|| self.actor_message_ty_from_closure_literal_at(&args[1], 2));
        if handle_message_ty.is_none() && !expr_is_closure_literal(&args[1]) {
            let handler = self.check_expr(scopes, &args[1], None)?;
            handle_message_ty =
                callable_ret_params_ty(&handler.ty).and_then(|(_, params)| params.get(2).cloned());
            prechecked_handler = Some(handler);
        }
        let Some(handle_message_ty) = handle_message_ty else {
            self.diagnostics.push(Diagnostic::new(
                span,
                "could not infer actor message type; add spawn_actor_state<S, M> type arguments, an expected Actor<M>, or handler parameter types",
            ));
            return None;
        };
        let message_ty = explicit_message_ty
            .unwrap_or_else(|| self.normalize_meta_repr_markers(&handle_message_ty, span));
        let handler_state_ty = self.normalize_meta_repr_markers(&state_ty, span);
        let state_ptr_ty = Ty::Pointer {
            nullable: false,
            mutability: ViewMutability::Writable,
            inner: Box::new(handler_state_ty.clone()),
        };
        let actor_self_ty = std_actor_ty(handle_message_ty.clone());
        let init_ret = std_result_ty(handler_state_ty.clone(), std_error_ty());
        let handler_ret = std_result_ty(Ty::Void, std_error_ty());
        let message_view = self.interface_view("Message", &[]);
        let expected_init_ty = Ty::Closure {
            ret: Box::new(init_ret.clone()),
            params: vec![],
            constraints: ConstraintBounds {
                positive: message_view.positive.clone(),
                negative: message_view.negative.clone(),
            },
        };
        let expected_handler_ty = Ty::Closure {
            ret: Box::new(handler_ret.clone()),
            params: vec![
                state_ptr_ty.clone(),
                actor_self_ty.clone(),
                message_ty.clone(),
            ],
            constraints: ConstraintBounds {
                positive: message_view.positive,
                negative: message_view.negative,
            },
        };
        let init = if let Some(init) = prechecked_init {
            self.coerce_expr_to_expected(scopes, init, Some(&expected_init_ty))
        } else {
            let init = self.check_expr(scopes, &args[0], Some(&expected_init_ty))?;
            self.coerce_expr_to_expected(scopes, init, Some(&expected_init_ty))
        };
        self.require_actor_callable(
            &init.ty,
            &[],
            &init_ret,
            "actor state initializer",
            init.span,
        );

        let handler = if let Some(handler) = prechecked_handler {
            self.coerce_expr_to_expected(scopes, handler, Some(&expected_handler_ty))
        } else if let ExprKind::Closure {
            is_async: false,
            params,
            body,
        } = &args[1].kind
        {
            let handler = self.check_closure_expr(
                scopes,
                args[1].span,
                false,
                params,
                body,
                Some(&expected_handler_ty),
            )?;
            self.coerce_expr_to_expected(scopes, handler, Some(&expected_handler_ty))
        } else {
            let handler = self.check_expr(scopes, &args[1], Some(&expected_handler_ty))?;
            self.coerce_expr_to_expected(scopes, handler, Some(&expected_handler_ty))
        };
        self.require_actor_callable(
            &handler.ty,
            &[state_ptr_ty, actor_self_ty, message_ty.clone()],
            &handler_ret,
            "actor state handler",
            handler.span,
        );

        if !self.type_implements_message(&message_ty) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("actor message type `{message_ty}` does not implement `Message`"),
            ));
        }
        let ret = std_result_ty(std_actor_ty(handle_message_ty.clone()), std_error_ty());
        self.ensure_struct_instance(&ret);
        self.ensure_enum_instance(&ret);
        Some(TExpr {
            span,
            ty: ret,
            kind: TExprKind::ActorSpawn {
                mode: ActorSpawnMode::State,
                state_arg: Box::new(init),
                handler_ty: handler.ty.clone(),
                handler: Box::new(handler),
                state_ty: handler_state_ty,
                handle_message_ty,
                message_ty,
            },
        })
    }

    fn check_actor_send_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        if args.len() != 2 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("send expects 2 arguments, got {}", args.len()),
            ));
            return None;
        }
        let current_subst = self.current_type_subst();
        let explicit_args = type_args
            .iter()
            .map(|arg| {
                self.lower_type_with_subst_preserving_meta_repr_markers(arg, &current_subst, false)
            })
            .collect::<Vec<_>>();
        if explicit_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "send accepts at most one message type argument",
            ));
            return None;
        }
        let actor = self.check_expr(scopes, &args[0], None)?;
        let inferred_message_ty = self.actor_message_ty_from_pointer(&actor.ty, actor.span);
        let handle_message_ty = explicit_args
            .first()
            .cloned()
            .or(inferred_message_ty)
            .unwrap_or(Ty::Unknown);
        let value = self.check_expr(scopes, &args[1], Some(&handle_message_ty))?;
        self.require_assignable(&handle_message_ty, &value.ty, value.span);
        let message_ty = if self.meta_repr_marker_matches_concrete(&handle_message_ty, &value.ty) {
            value.ty.clone()
        } else {
            self.normalize_meta_repr_markers(&handle_message_ty, span)
        };
        if !self.type_implements_message(&message_ty) {
            self.diagnostics.push(Diagnostic::new(
                value.span,
                format!("actor message type `{message_ty}` does not implement `Message`"),
            ));
        }
        let ret = std_result_ty(Ty::Void, std_error_ty());
        self.ensure_enum_instance(&ret);
        Some(TExpr {
            span,
            ty: ret,
            kind: TExprKind::ActorSend {
                actor: Box::new(actor),
                value: Box::new(value),
                message_ty,
            },
        })
    }

    fn check_actor_lifecycle_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
        op: ActorLifecycleOp,
    ) -> Option<TExpr> {
        if args.len() != 1 {
            let name = match op {
                ActorLifecycleOp::Stop => "stop",
                ActorLifecycleOp::Join => "join",
            };
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{name} expects 1 argument, got {}", args.len()),
            ));
            return None;
        }
        if type_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "actor lifecycle calls accept at most one message type argument",
            ));
            return None;
        }
        let current_subst = self.current_type_subst();
        let explicit_message_ty = type_args.first().map(|arg| {
            self.lower_type_with_subst_preserving_meta_repr_markers(arg, &current_subst, false)
        });
        let actor = self.check_expr(scopes, &args[0], None)?;
        let message_ty = explicit_message_ty
            .or_else(|| self.actor_message_ty_from_pointer(&actor.ty, actor.span))
            .unwrap_or(Ty::Unknown);
        let ret = std_result_ty(Ty::Void, std_error_ty());
        self.ensure_enum_instance(&ret);
        let kind = match op {
            ActorLifecycleOp::Stop => TExprKind::ActorStop {
                actor: Box::new(actor),
                message_ty,
            },
            ActorLifecycleOp::Join => TExprKind::ActorJoin {
                actor: Box::new(actor),
                message_ty,
            },
        };
        Some(TExpr {
            span,
            ty: ret,
            kind,
        })
    }

    fn check_meta_repr_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        name: &str,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        match name {
            "as_ref_repr" => self.check_meta_as_ref_repr_call(scopes, span, type_args, args),
            "into_repr" => self.check_meta_into_repr_call(scopes, span, type_args, args),
            "from_repr" => self.check_meta_from_repr_call(scopes, span, type_args, args, expected),
            _ => None,
        }
    }

    fn check_meta_as_ref_repr_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("as_ref_repr expects 1 argument, got {}", args.len()),
            ));
            return None;
        }
        if type_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "as_ref_repr accepts at most one type argument",
            ));
            return None;
        }
        let explicit = type_args.first().map(|ty| self.lower_type(ty));
        let expected_arg = explicit.clone().map(Ty::const_pointer_to);
        let value = self.check_expr(scopes, &args[0], expected_arg.as_ref())?;
        let source_ty = if let Some(source_ty) = explicit {
            source_ty
        } else {
            match &value.ty {
                Ty::Pointer {
                    nullable: false,
                    inner,
                    ..
                } => (**inner).clone(),
                Ty::Pointer { nullable: true, .. } => {
                    self.diagnostics.push(Diagnostic::new(
                        value.span,
                        "as_ref_repr requires a non-null pointer",
                    ));
                    Ty::Unknown
                }
                other => {
                    self.diagnostics.push(Diagnostic::new(
                        value.span,
                        format!("as_ref_repr requires `*const T`, got `{other}`"),
                    ));
                    Ty::Unknown
                }
            }
        };
        if let Some(expected_arg) = expected_arg.as_ref() {
            self.require_assignable(expected_arg, &value.ty, value.span);
        }
        self.reject_meta_ref_repr_erased_fields(span, &source_ty);
        let ret = self.meta_repr_ty(span, &source_ty, true);
        Some(TExpr {
            span,
            ty: ret,
            kind: TExprKind::MetaAsRefRepr {
                value: Box::new(value),
                source_ty,
            },
        })
    }

    fn check_meta_into_repr_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("into_repr expects 1 argument, got {}", args.len()),
            ));
            return None;
        }
        if type_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "into_repr accepts at most one type argument",
            ));
            return None;
        }
        let explicit = type_args.first().map(|ty| self.lower_type(ty));
        let expected_arg = explicit.clone().map(Ty::const_pointer_to);
        let value = self.check_expr(scopes, &args[0], expected_arg.as_ref())?;
        let source_ty = if let Some(source_ty) = explicit {
            source_ty
        } else {
            match &value.ty {
                Ty::Pointer {
                    nullable: false,
                    inner,
                    ..
                } => (**inner).clone(),
                Ty::Pointer { nullable: true, .. } => {
                    self.diagnostics.push(Diagnostic::new(
                        value.span,
                        "into_repr requires a non-null pointer",
                    ));
                    Ty::Unknown
                }
                other => {
                    self.diagnostics.push(Diagnostic::new(
                        value.span,
                        format!("into_repr requires `*const T`, got `{other}`"),
                    ));
                    Ty::Unknown
                }
            }
        };
        if let Some(expected_arg) = expected_arg.as_ref() {
            self.require_assignable(expected_arg, &value.ty, value.span);
        }
        let ret = self.meta_repr_ty(span, &source_ty, false);
        Some(TExpr {
            span,
            ty: ret,
            kind: TExprKind::MetaIntoRepr {
                value: Box::new(value),
                source_ty,
            },
        })
    }

    fn check_meta_from_repr_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("from_repr expects 1 argument, got {}", args.len()),
            ));
            return None;
        }
        if type_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                "from_repr accepts at most one type argument",
            ));
            return None;
        }
        let target_ty = if let Some(ty) = type_args.first() {
            self.lower_type(ty)
        } else if let Some(expected) = expected {
            expected.clone()
        } else {
            self.diagnostics.push(Diagnostic::new(
                span,
                "from_repr requires an explicit type argument or expected result type",
            ));
            Ty::Unknown
        };
        let repr_ty = self.meta_repr_ty(span, &target_ty, false);
        let storage_repr_ty = std_meta_repr_marker_ty(false, target_ty.clone());
        let value = self.check_expr(scopes, &args[0], Some(&storage_repr_ty))?;
        if value.ty == storage_repr_ty {
            // Source-level storage keeps `meta::Repr<T>` as the safe-envelope type,
            // while representation operations lower through the concrete SOP layout.
        } else {
            self.require_assignable(&repr_ty, &value.ty, value.span);
        }
        Some(TExpr {
            span,
            ty: target_ty.clone(),
            kind: TExprKind::MetaFromRepr {
                value: Box::new(value),
                target_ty,
            },
        })
    }

    fn reject_meta_ref_repr_erased_fields(&mut self, span: crate::span::Span, source_ty: &Ty) {
        let Ty::Named { name, args } = source_ty else {
            return;
        };
        let instance_name = enum_instance_name(name, args);
        let Some(fields) = self.structs.get(&instance_name) else {
            return;
        };
        for (field, ty) in fields {
            if ty.is_erased_value() {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("as_ref_repr cannot borrow erased field `{field}` of `{source_ty}`"),
                ));
            }
        }
    }

    fn check_type_metadata_call(
        &mut self,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
        name: &str,
    ) -> Option<TExpr> {
        if !args.is_empty() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{name} expects 0 arguments, got {}", args.len()),
            ));
            return None;
        }
        if type_args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{name} requires exactly one type argument"),
            ));
            return None;
        }
        let subst = self.current_type_subst();
        let lowered =
            self.lower_type_with_subst_preserving_meta_repr_markers(&type_args[0], &subst, false);
        let ty = self.meta_repr_storage_ty(&lowered, type_args[0].span);
        self.ensure_struct_instance(&ty);
        self.ensure_enum_instance(&ty);
        let kind = match name {
            "type_size" => TExprKind::TypeSize { ty },
            "type_align" => TExprKind::TypeAlign { ty },
            _ => return None,
        };
        Some(TExpr {
            span,
            ty: Ty::Usize,
            kind,
        })
    }

    fn actor_message_ty_from_spawn_expected(&self, expected: Option<&Ty>) -> Option<Ty> {
        let Ty::Named { name, args } = expected? else {
            return None;
        };
        if name != "Result" || args.len() != 2 {
            return None;
        }
        let Ty::Named {
            name: actor_name,
            args: actor_args,
        } = &args[0]
        else {
            return None;
        };
        if actor_name == "Actor" && actor_args.len() == 1 {
            Some(actor_args[0].clone())
        } else {
            None
        }
    }

    fn actor_message_ty_from_closure_literal(&mut self, expr: &Expr) -> Option<Ty> {
        self.actor_message_ty_from_closure_literal_at(expr, 1)
    }

    fn actor_message_ty_from_closure_literal_at(
        &mut self,
        expr: &Expr,
        param_index: usize,
    ) -> Option<Ty> {
        match &expr.kind {
            ExprKind::Closure { params, .. } => params
                .get(param_index)
                .and_then(|param| param.ty.as_ref())
                .map(|ty| self.lower_type(ty)),
            ExprKind::Cast { expr, .. } => {
                self.actor_message_ty_from_closure_literal_at(expr, param_index)
            }
            _ => None,
        }
    }

    fn actor_message_ty_from_pointer(
        &mut self,
        actor_ty: &Ty,
        span: crate::span::Span,
    ) -> Option<Ty> {
        let Ty::Pointer {
            nullable: false,
            inner,
            ..
        } = actor_ty
        else {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("actor handle argument must be `*Actor<M>`, got `{actor_ty}`"),
            ));
            return None;
        };
        let Ty::Named { name, args } = &**inner else {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("actor handle argument must be `*Actor<M>`, got `{actor_ty}`"),
            ));
            return None;
        };
        if name != "Actor" || args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("actor handle argument must be `*Actor<M>`, got `{actor_ty}`"),
            ));
            return None;
        }
        Some(args[0].clone())
    }

    fn require_actor_handler_callable(
        &mut self,
        handler_ty: &Ty,
        state_ty: &Ty,
        message_ty: &Ty,
        expected_ret: &Ty,
        span: crate::span::Span,
    ) {
        self.require_actor_callable(
            handler_ty,
            &[state_ty.clone(), message_ty.clone()],
            expected_ret,
            "actor handler",
            span,
        );
    }

    fn require_actor_callable(
        &mut self,
        callable_ty: &Ty,
        expected_params: &[Ty],
        expected_ret: &Ty,
        label: &str,
        span: crate::span::Span,
    ) {
        let Some((ret, params)) = callable_ret_params_ty(callable_ty) else {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{label} `{callable_ty}` is not callable"),
            ));
            return;
        };
        if params.len() != expected_params.len() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "{label} expects {} parameters, got {}",
                    expected_params.len(),
                    params.len()
                ),
            ));
            return;
        }
        for (index, (actual, expected)) in params.iter().zip(expected_params.iter()).enumerate() {
            if actual != expected {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "{label} parameter {index} mismatch: expected `{expected}`, got `{actual}`",
                    ),
                ));
            }
        }
        if !expected_ret.can_assign_from(&ret) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{label} must return `{expected_ret}`, got `{ret}`"),
            ));
        }
    }

    fn check_direct_function_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        sig: FunctionSig,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        if std_id::is_std_actor_function(
            &self.resolved,
            sig.module,
            &sig.name,
            "spawn_actor_cloned",
        ) {
            return self.check_actor_spawn_cloned_call(scopes, span, type_args, args, expected);
        }
        if std_id::is_std_actor_function(&self.resolved, sig.module, &sig.name, "spawn_actor_state")
        {
            return self.check_actor_spawn_state_call(scopes, span, type_args, args, expected);
        }
        if std_id::is_std_actor_function(&self.resolved, sig.module, &sig.name, "send") {
            return self.check_actor_send_call(scopes, span, type_args, args);
        }
        if std_id::is_std_actor_function(&self.resolved, sig.module, &sig.name, "stop") {
            return self.check_actor_lifecycle_call(
                scopes,
                span,
                type_args,
                args,
                ActorLifecycleOp::Stop,
            );
        }
        if std_id::is_std_actor_function(&self.resolved, sig.module, &sig.name, "join") {
            return self.check_actor_lifecycle_call(
                scopes,
                span,
                type_args,
                args,
                ActorLifecycleOp::Join,
            );
        }
        if std_id::is_std_meta_function(&self.resolved, sig.module, &sig.name, "as_ref_repr")
            || std_id::is_std_meta_function(&self.resolved, sig.module, &sig.name, "into_repr")
            || std_id::is_std_meta_function(&self.resolved, sig.module, &sig.name, "from_repr")
        {
            return self.check_meta_repr_call(scopes, span, &sig.name, type_args, args, expected);
        }
        if std_id::is_std_meta_function(&self.resolved, sig.module, &sig.name, "type_size")
            || std_id::is_std_meta_function(&self.resolved, sig.module, &sig.name, "type_align")
        {
            return self.check_type_metadata_call(span, type_args, args, &sig.name);
        }
        if std_id::is_std_async_function(&self.resolved, sig.module, &sig.name, "block_on") {
            return self.check_async_block_on_call(scopes, span, type_args, args);
        }
        if std_id::is_std_async_time_function(&self.resolved, sig.module, &sig.name, "sleep_ms") {
            return self.check_async_sleep_ms_call(scopes, span, type_args, args);
        }

        let (call_sig, generic_args) = if sig.generics.is_empty() {
            if !type_args.is_empty() {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("function `{}` is not generic", sig.name),
                ));
                return None;
            }
            (sig, None)
        } else {
            let (call_sig, instance_args) =
                self.infer_generic_function_call(scopes, span, &sig, type_args, args, expected)?;
            (call_sig, Some(instance_args))
        };
        if call_sig.is_unsafe {
            self.require_unsafe(
                span,
                format!(
                    "call to unsafe function `{}` requires unsafe block",
                    call_sig.name
                ),
            );
        }
        let call_ret = if call_sig.is_async {
            std_future_ty(call_sig.ret.clone())
        } else {
            call_sig.ret.clone()
        };
        let callee = TExpr {
            span,
            ty: Ty::Function {
                is_unsafe: call_sig.is_unsafe,
                abi: call_sig.abi.clone(),
                ret: Box::new(call_ret.clone()),
                params: call_sig.params.clone(),
            },
            kind: if let Some(type_args) = generic_args {
                TExprKind::GenericFunction {
                    def_id: call_sig.def_id,
                    name: call_sig.name.clone(),
                    type_args,
                }
            } else {
                TExprKind::Function(call_sig.def_id, call_sig.name.clone())
            },
        };
        self.check_call_with_sig(scopes, span, callee, &call_ret, &call_sig.params, args)
    }

    fn check_closure_cast_allowed(&mut self, target: &Ty, span: crate::span::Span) {
        match target {
            Ty::Closure { .. } => {}
            Ty::Function {
                is_unsafe: false,
                abi: None,
                ..
            } => {}
            Ty::Function { abi: Some(_), .. } => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    "closure expressions cannot produce extern C function pointers",
                ));
            }
            Ty::Function {
                is_unsafe: true, ..
            } => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    "closure expressions cannot produce unsafe function pointers",
                ));
            }
            _ => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "closure annotation must be a closure or Ciel ABI function type, got `{target}`"
                    ),
                ));
            }
        }
    }

    fn check_closure_expr(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        is_async: bool,
        params: &[ClosureParam],
        body: &ClosureBody,
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        let expected_closure_instance_id = match expected {
            Some(Ty::ClosureInstance { id, .. }) => Some(*id),
            _ => None,
        };
        let expected_sig = match expected {
            Some(Ty::Closure { ret, params, .. })
            | Some(Ty::ClosureInstance { ret, params, .. }) => {
                Some(((**ret).clone(), params.clone(), false))
            }
            Some(Ty::Function {
                is_unsafe: false,
                abi: None,
                ret,
                params,
            }) => Some(((**ret).clone(), params.clone(), true)),
            Some(Ty::Function { abi: Some(_), .. }) => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    "closure expressions cannot produce extern C function pointers",
                ));
                None
            }
            Some(other) => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("closure requires expected callable type, got `{other}`"),
                ));
                None
            }
            None => None,
        };

        if let Some((_, expected_params, _)) = &expected_sig
            && expected_params.len() != params.len()
        {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "closure expects {} parameters, got {}",
                    expected_params.len(),
                    params.len()
                ),
            ));
        }

        let mut checked_params = Vec::new();
        let mut closure_scopes = scopes.clone();
        closure_scopes.mark_all_captured();
        closure_scopes.push();
        for (idx, param) in params.iter().enumerate() {
            let param_ty = if let Some(ty) = &param.ty {
                let ty = self.lower_type(ty);
                if let Some((_, expected_params, _)) = &expected_sig
                    && let Some(expected_ty) = expected_params.get(idx)
                {
                    if contains_type_hole(expected_ty) {
                        self.unify_type_holes(expected_ty, &ty);
                    } else {
                        let expected_ty = self.resolve_type_holes(expected_ty);
                        if !matches!(expected_ty, Ty::Unknown)
                            && !contains_generic(&expected_ty)
                            && ty != expected_ty
                        {
                            self.diagnostics.push(Diagnostic::new(
                                param.name.span,
                                format!(
                                    "closure parameter `{}` expected `{expected_ty}`, got `{ty}`",
                                    param.name.name
                                ),
                            ));
                        }
                    }
                }
                ty
            } else if let Some((_, expected_params, _)) = &expected_sig {
                expected_params
                    .get(idx)
                    .map(|ty| self.resolve_type_holes(ty))
                    .unwrap_or(Ty::Unknown)
            } else {
                self.diagnostics.push(Diagnostic::new(
                    param.name.span,
                    format!(
                        "closure parameter `{}` requires an explicit type or expected callable type",
                        param.name.name
                    ),
                ));
                Ty::Unknown
            };
            self.reject_invalid_plain_value_type(&param_ty, param.name.span, "closure parameter");
            if let Err(name) = closure_scopes.insert(
                param.local_id,
                Binding {
                    name: param.name.name.clone(),
                    ty: param_ty.clone(),
                    narrowed_ty: None,
                    init_state: InitState::Assigned,
                    mutability: param.mutability,
                    captured: false,
                    declared_loop_depth: self.current_loop_depth,
                },
            ) {
                self.diagnostics.push(Diagnostic::new(
                    param.name.span,
                    format!("duplicate closure parameter `{name}`"),
                ));
            }
            checked_params.push((param.local_id, param.name.name.clone(), param_ty));
        }

        let previous_return_ty = self.current_return_ty.clone();
        let previous_control_contexts = std::mem::take(&mut self.control_contexts);
        let previous_unsafe_depth = std::mem::replace(&mut self.unsafe_depth, 0);
        let previous_async_depth =
            std::mem::replace(&mut self.current_async_depth, if is_async { 1 } else { 0 });
        let (ret_ty, checked_body) = match body {
            ClosureBody::Expr(body_expr) => {
                if let Some((expected_ret, _, _)) = &expected_sig {
                    let expected_ret = self.resolve_type_holes(expected_ret);
                    self.current_return_ty = expected_ret.clone();
                    let checked =
                        self.check_expr(&mut closure_scopes, body_expr, Some(&expected_ret))?;
                    self.require_assignable(&expected_ret, &checked.ty, checked.span);
                    (expected_ret, TClosureBody::Expr(Box::new(checked)))
                } else {
                    self.current_return_ty = Ty::Unknown;
                    let checked = self.check_expr(&mut closure_scopes, body_expr, None)?;
                    let ret_ty = checked.ty.clone();
                    (ret_ty, TClosureBody::Expr(Box::new(checked)))
                }
            }
            ClosureBody::Block(block) => {
                let Some((expected_ret, _, _)) = &expected_sig else {
                    self.diagnostics.push(Diagnostic::new(
                        block.span,
                        "block-bodied closure requires an expected callable return type",
                    ));
                    self.current_return_ty = previous_return_ty;
                    self.control_contexts = previous_control_contexts;
                    self.unsafe_depth = previous_unsafe_depth;
                    self.current_async_depth = previous_async_depth;
                    return None;
                };
                let expected_ret = self.resolve_type_holes(expected_ret);
                self.current_return_ty = expected_ret.clone();
                let checked = self.check_block_with_existing_scope(
                    &mut closure_scopes,
                    block,
                    &expected_ret,
                )?;
                if expected_ret.is_never() && checked.flow.can_fallthrough {
                    self.diagnostics.push(Diagnostic::new(
                        block.span,
                        "closure with return type `never` can fall through",
                    ));
                } else if !expected_ret.is_erased_value() && checked.flow.can_fallthrough {
                    self.diagnostics.push(Diagnostic::new(
                        block.span,
                        format!("closure must return `{expected_ret}` on every path"),
                    ));
                }
                (expected_ret, TClosureBody::Block(checked.block))
            }
        };
        self.current_return_ty = previous_return_ty;
        self.control_contexts = previous_control_contexts;
        self.unsafe_depth = previous_unsafe_depth;
        self.current_async_depth = previous_async_depth;

        let capture_ids = collect_closure_capture_ids(&checked_params, &checked_body);
        let mut captures = Vec::new();
        for local_id in capture_ids {
            let Some(binding) = scopes.get(local_id) else {
                continue;
            };
            if !binding.init_state.is_assigned() {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "captured local `{}` is not definitely assigned at closure creation",
                        binding.name
                    ),
                ));
            }
            captures.push(TClosureCapture {
                local_id,
                name: binding.name.clone(),
                ty: scopes
                    .effective_ty(local_id)
                    .unwrap_or_else(|| binding.ty.clone()),
            });
        }

        let id = if let Some(id) = expected_closure_instance_id {
            id
        } else {
            let id = self.next_closure_id;
            self.next_closure_id += 1;
            id
        };
        let capture_tys = captures
            .iter()
            .map(|capture| capture.ty.clone())
            .collect::<Vec<_>>();

        let result_ty = if let Some((expected_ret, expected_params, target_fn)) = expected_sig {
            let expected_ret = self.resolve_type_holes(&expected_ret);
            let expected_params = expected_params
                .iter()
                .map(|param| self.resolve_type_holes(param))
                .collect::<Vec<_>>();
            if target_fn {
                if !captures.is_empty() {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        "capturing closure cannot convert to `fn`",
                    ));
                }
                Ty::Function {
                    is_unsafe: false,
                    abi: None,
                    ret: Box::new(expected_ret),
                    params: expected_params,
                }
            } else {
                Ty::ClosureInstance {
                    id,
                    ret: Box::new(expected_ret),
                    params: expected_params,
                    captures: capture_tys,
                }
            }
        } else {
            Ty::ClosureInstance {
                id,
                ret: Box::new(ret_ty.clone()),
                params: checked_params.iter().map(|(_, _, ty)| ty.clone()).collect(),
                captures: capture_tys,
            }
        };
        Some(TExpr {
            span,
            ty: result_ty,
            kind: TExprKind::Closure {
                is_async,
                id,
                params: checked_params,
                captures,
                body: checked_body,
            },
        })
    }

    fn check_call_with_sig(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        callee: TExpr,
        ret: &Ty,
        params: &[Ty],
        args: &[Expr],
    ) -> Option<TExpr> {
        if params.len() != args.len() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "call expects {} arguments, got {}",
                    params.len(),
                    args.len()
                ),
            ));
        }
        let mut checked_args = Vec::new();
        for (idx, arg) in args.iter().enumerate() {
            let expected = params.get(idx);
            let checked = self.check_expr(scopes, arg, expected)?;
            if let Some(expected) = expected {
                self.require_assignable(expected, &checked.ty, arg.span);
            }
            checked_args.push(checked);
        }
        Some(TExpr {
            span,
            ty: ret.clone(),
            kind: TExprKind::Call {
                callee: Box::new(callee),
                args: checked_args,
            },
        })
    }

    fn check_async_block_on_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        if type_args.len() > 1 {
            self.diagnostics.push(Diagnostic::new(
                type_args[1].span,
                "too many type arguments for `block_on`",
            ));
            return None;
        }
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("call expects 1 arguments, got {}", args.len()),
            ));
        }
        let explicit_output = type_args.first().map(|ty| self.lower_type(ty));
        let expected_future = explicit_output.as_ref().map(|ty| std_future_ty(ty.clone()));
        let Some(arg) = args.first() else {
            return None;
        };
        let future = self.check_expr(scopes, arg, expected_future.as_ref())?;
        if let Some(expected) = expected_future.as_ref() {
            self.require_assignable(expected, &future.ty, future.span);
        }
        let output_ty = explicit_output.or_else(|| self.future_output_ty(&future.ty));
        let Some(output_ty) = output_ty else {
            self.diagnostics.push(Diagnostic::new(
                future.span,
                format!("`block_on` requires `Future<T>`, got `{}`", future.ty),
            ));
            return Some(TExpr {
                span,
                ty: Ty::Unknown,
                kind: TExprKind::AsyncBlockOn {
                    future: Box::new(future),
                },
            });
        };
        Some(TExpr {
            span,
            ty: output_ty,
            kind: TExprKind::AsyncBlockOn {
                future: Box::new(future),
            },
        })
    }

    fn check_async_sleep_ms_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        type_args: &[Type],
        args: &[Expr],
    ) -> Option<TExpr> {
        if !type_args.is_empty() {
            self.diagnostics
                .push(Diagnostic::new(span, "function `sleep_ms` is not generic"));
            return None;
        }
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("call expects 1 arguments, got {}", args.len()),
            ));
        }
        let Some(ms_arg) = args.first() else {
            return None;
        };
        let ms = self.check_expr(scopes, ms_arg, Some(&Ty::U64))?;
        self.require_assignable(&Ty::U64, &ms.ty, ms.span);
        let output_ty = std_result_ty(Ty::Void, std_error_ty());
        Some(TExpr {
            span,
            ty: std_future_ty(output_ty.clone()),
            kind: TExprKind::AsyncSleep {
                ms: Box::new(ms),
                output_ty,
            },
        })
    }

    fn check_generic_inference_arg(
        &mut self,
        scopes: &mut LocalScopes,
        arg: &Expr,
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        if expr_is_closure_literal(arg) {
            return self.check_closure_literal_preserving_instance(scopes, arg, expected);
        }
        self.check_expr(scopes, arg, expected)
    }

    fn check_closure_literal_preserving_instance(
        &mut self,
        scopes: &mut LocalScopes,
        arg: &Expr,
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        match &arg.kind {
            ExprKind::Closure {
                is_async,
                params,
                body,
            } => self.check_closure_expr(scopes, arg.span, *is_async, params, body, expected),
            ExprKind::Cast { expr, ty } => {
                let ExprKind::Closure {
                    is_async,
                    params,
                    body,
                } = &expr.kind
                else {
                    return self.check_expr(scopes, arg, expected);
                };
                let target = self.lower_type(ty);
                self.check_closure_cast_allowed(&target, arg.span);
                let checked = self.check_closure_expr(
                    scopes,
                    expr.span,
                    *is_async,
                    params,
                    body,
                    Some(&target),
                )?;
                let checked = TExpr {
                    span: arg.span,
                    ..checked
                };
                Some(self.coerce_expr_to_expected(scopes, checked, Some(&target)))
            }
            _ => self.check_expr(scopes, arg, expected),
        }
    }

    fn infer_generic_function_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        sig: &FunctionSig,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<(FunctionSig, Vec<Ty>)> {
        let mut subst = HashMap::<String, Ty>::new();
        let current_subst = self.current_type_subst();
        for (idx, ty) in type_args.iter().enumerate() {
            let Some(generic) = sig.generics.get(idx) else {
                self.diagnostics.push(Diagnostic::new(
                    ty.span,
                    format!("too many type arguments for `{}`", sig.name),
                ));
                return None;
            };
            let concrete =
                self.lower_type_with_subst_preserving_meta_repr_markers(ty, &current_subst, false);
            subst.insert(generic.name.clone(), concrete);
        }
        let expected_hints = if let Some(expected) = expected {
            let mut hints = subst.clone();
            self.unify_ty_for_inference(&sig.ret, expected, &mut hints);
            hints
        } else {
            subst.clone()
        };

        if sig.params.len() != args.len() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "call expects {} arguments, got {}",
                    sig.params.len(),
                    args.len()
                ),
            ));
        }

        let mut deferred_closure_args = Vec::new();
        for (idx, arg) in args.iter().enumerate() {
            let Some(param_ty) = sig.params.get(idx) else {
                continue;
            };
            let (expected_arg, expected_for_arg) =
                self.inference_arg_expected(param_ty, &subst, &expected_hints);
            if contains_generic(&expected_arg) && expr_is_closure_literal(arg) {
                if expected_for_arg.is_none() {
                    if let Some(partial_expected) =
                        self.closure_inference_expected(param_ty, &subst, &expected_hints)
                    {
                        let checked =
                            self.check_generic_inference_arg(scopes, arg, Some(&partial_expected))?;
                        self.unify_ty_for_inference(&expected_arg, &checked.ty, &mut subst);
                        continue;
                    }
                    deferred_closure_args.push(idx);
                    continue;
                }
            }
            let checked =
                self.check_generic_inference_arg(scopes, arg, expected_for_arg.as_ref())?;
            self.unify_ty_for_inference(&expected_arg, &checked.ty, &mut subst);
        }
        for idx in deferred_closure_args {
            let Some(param_ty) = sig.params.get(idx) else {
                continue;
            };
            let Some(arg) = args.get(idx) else {
                continue;
            };
            let (expected_arg, expected_for_arg) =
                self.inference_arg_expected(param_ty, &subst, &expected_hints);
            let checked =
                self.check_generic_inference_arg(scopes, arg, expected_for_arg.as_ref())?;
            self.unify_ty_for_inference(&expected_arg, &checked.ty, &mut subst);
        }
        if let Some(expected) = expected {
            self.unify_ty_for_inference(&sig.ret, expected, &mut subst);
        }

        for generic in &sig.generics {
            if !subst.contains_key(&generic.name) {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "could not infer generic parameter `{}` for `{}`",
                        generic.name, sig.name
                    ),
                ));
                return None;
            }
            if subst
                .get(&generic.name)
                .is_some_and(|ty| contains_type_hole(&self.resolve_type_holes(ty)))
            {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "could not infer generic parameter `{}` for `{}`",
                        generic.name, sig.name
                    ),
                ));
                return None;
            }
        }

        self.check_generic_constraints(&sig.generics, &subst, span);
        let instance_args = sig
            .generics
            .iter()
            .filter_map(|generic| {
                subst
                    .get(&generic.name)
                    .map(|ty| self.resolve_type_holes(ty))
            })
            .collect::<Vec<_>>();
        let params = sig
            .params
            .iter()
            .map(|param| {
                let substituted = substitute_ty(param, &subst);
                let ty = self.preserve_meta_repr_markers(&substituted);
                self.resolve_type_holes(&ty)
            })
            .collect::<Vec<_>>();
        let ret = {
            let substituted = substitute_ty(&sig.ret, &subst);
            let ty = self.preserve_meta_repr_markers(&substituted);
            self.resolve_type_holes(&ty)
        };
        Some((
            FunctionSig {
                def_id: sig.def_id,
                module: sig.module,
                name: sig.name.clone(),
                is_unsafe: sig.is_unsafe,
                is_async: sig.is_async,
                abi: sig.abi.clone(),
                noescape: sig.noescape,
                has_body: sig.has_body,
                ret,
                params,
                generics: Vec::new(),
                exported: sig.exported,
            },
            instance_args,
        ))
    }

    fn check_generic_constraints(
        &mut self,
        generics: &[GenericInfo],
        subst: &HashMap<String, Ty>,
        span: crate::span::Span,
    ) {
        self.check_generic_constraints_impl(generics, subst, span);
    }

    fn check_generic_constraints_impl(
        &mut self,
        generics: &[GenericInfo],
        subst: &HashMap<String, Ty>,
        span: crate::span::Span,
    ) {
        for generic in generics {
            let Some(concrete) = subst.get(&generic.name) else {
                continue;
            };
            let Some(constraint) = &generic.constraint else {
                continue;
            };
            let concrete_for_constraints = self.meta_repr_storage_ty(concrete, span);
            let bounds = self.constraint_bounds(constraint, subst);
            for capability in bounds.positive {
                if !self.type_implements_capability(
                    &capability.name,
                    &capability.args,
                    &concrete_for_constraints,
                ) {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!(
                            "generic constraint not satisfied: `{}` does not implement `{}`",
                            concrete, capability.name
                        ),
                    ));
                }
            }
            for capability in bounds.negative {
                if self.type_implements_capability(
                    &capability.name,
                    &capability.args,
                    &concrete_for_constraints,
                ) {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!(
                            "generic constraint not satisfied: `{}` has forbidden capability `{}`",
                            concrete, capability.name
                        ),
                    ));
                }
            }
        }
    }

    fn instantiate_generic_template_for_mono(
        &mut self,
        template: &CheckedGenericFunction,
        instance_args: &[Ty],
        def_id: DefId,
        instance_name: String,
    ) -> Option<CheckedFunction> {
        if template.generics.len() != instance_args.len() {
            self.diagnostics.push(Diagnostic::new(
                template.function.signature.name.span,
                format!(
                    "generic function `{}` expects {} type arguments, got {}",
                    template.name,
                    template.generics.len(),
                    instance_args.len()
                ),
            ));
            return None;
        }
        let subst = template
            .generics
            .iter()
            .map(|generic| generic.name.clone())
            .zip(instance_args.iter().cloned())
            .collect::<HashMap<_, _>>();
        let generics = template
            .generics
            .iter()
            .map(|generic| GenericInfo {
                name: generic.name.clone(),
                constraint: generic.constraint.clone(),
            })
            .collect::<Vec<_>>();
        self.check_generic_constraints(&generics, &subst, template.function.signature.name.span);
        let params = template
            .function
            .signature
            .params
            .iter()
            .map(|param| {
                (
                    param.local_id,
                    param.name.name.clone(),
                    self.lower_type_with_subst(&param.ty, &subst),
                )
            })
            .collect::<Vec<_>>();
        let body_params = template
            .function
            .signature
            .params
            .iter()
            .zip(params.iter())
            .filter_map(|(param, (_, _, ty))| {
                param.local_id.map(|local_id| {
                    (
                        local_id,
                        param.name.name.clone(),
                        ty.clone(),
                        param.mutability,
                    )
                })
            })
            .collect::<Vec<_>>();
        let ret = self.lower_type_with_subst(&template.function.signature.ret, &subst);
        let instance_sig = FunctionSig {
            def_id,
            module: template.module,
            name: instance_name.clone(),
            is_unsafe: template.is_unsafe,
            is_async: template.is_async,
            abi: template.abi.clone(),
            noescape: template.noescape,
            has_body: true,
            ret: ret.clone(),
            params: params.iter().map(|(_, _, ty)| ty.clone()).collect(),
            generics: Vec::new(),
            exported: false,
        };
        self.functions_by_def.insert(def_id, instance_sig.clone());

        let body = template.function.body.as_ref().and_then(|body| {
            let previous_module = self.current_module;
            self.current_module = template.module;
            self.type_subst_stack.push(subst.clone());
            let checked_body = self.check_function_body(&instance_sig, &body_params, body);
            self.type_subst_stack.pop();
            self.current_module = previous_module;
            checked_body
        });
        body.map(|body| CheckedFunction {
            def_id,
            name: instance_name,
            is_unsafe: template.is_unsafe,
            is_async: template.is_async,
            abi: template.abi.clone(),
            noescape: template.noescape,
            exported: false,
            ret,
            params,
            body: Some(body),
        })
    }

    fn check_interface_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        def_id: DefId,
        type_args: &[Type],
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        let name = self.resolved.def(def_id).name.clone();
        let Some(interface) = self.interfaces.get(&def_id).cloned() else {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("interface alias `{name}` is not directly callable"),
            ));
            return None;
        };
        if interface.params.is_empty() || args.is_empty() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("interface call `{name}` requires a receiver argument"),
            ));
            return None;
        }

        let explicit_args = type_args
            .iter()
            .map(|arg| self.lower_type(arg))
            .collect::<Vec<_>>();
        let first_arg = self.check_expr(scopes, &args[0], None)?;
        if let Ty::DynamicInterface {
            name: dyn_name,
            args: dyn_args,
        } = &first_arg.ty
            && let Some(interface_ref) = self.dynamic_view_interface(dyn_name, dyn_args, &name)
        {
            return self.check_dynamic_interface_call(
                scopes,
                span,
                interface,
                &interface_ref.args,
                first_arg,
                &args[1..],
            );
        }

        let mut subst = interface
            .generics
            .iter()
            .cloned()
            .map(|name| (name.clone(), Ty::Generic(name)))
            .collect::<HashMap<_, _>>();
        for (idx, ty) in explicit_args.iter().enumerate() {
            let Some(generic) = interface.generics.iter().skip(1).nth(idx) else {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("too many type arguments for interface `{name}`"),
                ));
                return None;
            };
            subst.insert(generic.clone(), ty.clone());
        }
        if let Some(expected) = expected {
            let ret = self.lower_type_with_subst(&interface.ret, &subst);
            self.unify_ty_for_inference(&ret, expected, &mut subst);
        }
        let mut checked_args = vec![first_arg.clone()];
        for (idx, arg) in args.iter().enumerate() {
            let Some(param) = interface.params.get(idx) else {
                continue;
            };
            if idx == 0 {
                unify_receiver_param(
                    &self.lower_type_with_subst(&param.ty, &subst),
                    &first_arg.ty,
                    &mut subst,
                );
                continue;
            }
            let param_ty = self.lower_type_with_subst(&param.ty, &subst);
            let checked = if contains_generic(&param_ty) {
                self.check_expr(scopes, arg, None)?
            } else {
                self.check_expr(scopes, arg, Some(&param_ty))?
            };
            self.unify_ty_for_inference(&param_ty, &checked.ty, &mut subst);
            checked_args.push(checked);
        }
        if interface.params.len() != args.len() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "interface call `{name}` expects {} arguments, got {}",
                    interface.params.len(),
                    args.len()
                ),
            ));
        }
        for generic in &interface.generics {
            if subst.get(generic).is_none_or(contains_generic)
                || subst
                    .get(generic)
                    .is_some_and(|ty| contains_type_hole(&self.resolve_type_holes(ty)))
            {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("could not infer interface generic parameter `{generic}` for `{name}`"),
                ));
                return None;
            }
        }
        let interface_args = interface
            .generics
            .iter()
            .filter_map(|generic| subst.get(generic).map(|ty| self.resolve_type_holes(ty)))
            .collect::<Vec<_>>();
        let receiver_ty = subst
            .get(&interface.generics[0])
            .map(|ty| self.resolve_type_holes(ty));
        let non_receiver_args = interface_non_receiver_args(&interface_args);
        if let Some(receiver_ty) = receiver_ty.as_ref()
            && retained_closure_proves_capability(receiver_ty, &name, &non_receiver_args)
        {
            let ret = self.lower_type_with_subst(&interface.ret, &subst);
            return Some(TExpr {
                span,
                ty: ret,
                kind: TExprKind::RetainedClosureInterfaceCall {
                    interface_name: name.clone(),
                    interface_args: non_receiver_args.to_vec(),
                    receiver: Box::new(checked_args.remove(0)),
                    args: checked_args,
                },
            });
        }
        if let Some(implementation) = self.find_or_instantiate_impl_by_full_args(
            &name,
            &interface_args,
            receiver_ty.as_ref(),
            span,
        ) {
            let callee = TExpr {
                span,
                ty: Ty::Function {
                    is_unsafe: false,
                    abi: None,
                    ret: Box::new(implementation.ret.clone()),
                    params: implementation.params.clone(),
                },
                kind: TExprKind::Function(implementation.function_def, name.clone()),
            };
            return Some(TExpr {
                span,
                ty: implementation.ret.clone(),
                kind: TExprKind::Call {
                    callee: Box::new(callee),
                    args: checked_args,
                },
            });
        }

        let message = if std_id::is_std_message_clone_interface(&self.resolved, def_id) {
            receiver_ty
                .as_ref()
                .map(|ty| format!("`{ty}` does not implement `Message`"))
                .unwrap_or_else(|| format!("no impl of `{name}` for this call"))
        } else {
            format!("no impl of `{name}` for this call")
        };
        self.diagnostics.push(Diagnostic::new(span, message));
        None
    }

    fn check_dynamic_interface_call(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        interface: InterfaceSig,
        interface_args: &[Ty],
        receiver: TExpr,
        args: &[Expr],
    ) -> Option<TExpr> {
        let Ty::DynamicInterface { .. } = &receiver.ty else {
            return None;
        };
        let mut subst = HashMap::<String, Ty>::new();
        if let Some(receiver_generic) = interface.generics.first() {
            subst.insert(
                receiver_generic.clone(),
                Ty::Generic(receiver_generic.clone()),
            );
        }
        for (generic, arg) in interface.generics.iter().skip(1).zip(interface_args.iter()) {
            subst.insert(generic.clone(), arg.clone());
        }
        if interface.generics.len() > 1 && interface_args.len() != interface.generics.len() - 1 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "dynamic interface `{}` requires {} non-receiver type arguments",
                    interface.name,
                    interface.generics.len() - 1
                ),
            ));
            return None;
        }
        if args.len() + 1 != interface.params.len() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "dynamic interface call `{}` expects {} trailing arguments, got {}",
                    interface.name,
                    interface.params.len().saturating_sub(1),
                    args.len()
                ),
            ));
        }
        let mut checked_args = Vec::new();
        for (arg, param) in args.iter().zip(interface.params.iter().skip(1)) {
            let param_ty = self.lower_type_with_subst(&param.ty, &subst);
            let checked = self.check_expr(scopes, arg, Some(&param_ty))?;
            self.require_assignable(&param_ty, &checked.ty, checked.span);
            checked_args.push(checked);
        }
        let ret = self.lower_type_with_subst(&interface.ret, &subst);
        Some(TExpr {
            span,
            ty: ret,
            kind: TExprKind::DynamicInterfaceCall {
                interface_name: interface.name,
                receiver: Box::new(receiver),
                args: checked_args,
            },
        })
    }

    fn coerce_expr_to_expected(
        &mut self,
        scopes: &LocalScopes,
        expr: TExpr,
        expected: Option<&Ty>,
    ) -> TExpr {
        let Some(expected) = expected else {
            return expr;
        };
        if contains_type_hole(expected) || contains_type_hole(&expr.ty) {
            self.unify_type_holes(expected, &expr.ty);
        }
        let expected = self.resolve_type_holes(expected);
        let expr_ty = self.resolve_type_holes(&expr.ty);
        if let Ty::Closure {
            ret: expected_ret,
            params: expected_params,
            constraints: expected_constraints,
        } = &expected
            && closure_shape_satisfies(expected_ret, expected_params, &expr_ty)
        {
            if self.closure_constraints_satisfied_by_ty(
                expected_constraints,
                &expr_ty,
                expr.span,
                true,
            ) {
                let needs_retain = match &expr_ty {
                    Ty::Closure {
                        constraints: actual_constraints,
                        ..
                    } => actual_constraints != expected_constraints,
                    Ty::ClosureInstance { .. } => !expected_constraints.is_empty(),
                    _ => false,
                };
                if needs_retain {
                    return TExpr {
                        span: expr.span,
                        ty: expected,
                        kind: TExprKind::RetainClosure {
                            expr: Box::new(expr),
                            source_ty: expr_ty,
                        },
                    };
                }
                return TExpr {
                    span: expr.span,
                    ty: expected,
                    kind: expr.kind,
                };
            }
            return expr;
        }
        if closure_instance_satisfies_signature(&expected, &expr_ty) {
            return TExpr {
                span: expr.span,
                ty: expected,
                kind: expr.kind,
            };
        }
        if let (
            Ty::Slice {
                mutability: expected_mutability,
                elem: expected_elem,
            },
            Ty::Slice {
                mutability: actual_mutability,
                elem: actual_elem,
            },
        ) = (&expected, &expr_ty)
            && *expected_mutability == ViewMutability::ReadOnly
            && *actual_mutability == ViewMutability::Writable
            && expected_elem == actual_elem
        {
            return TExpr {
                span: expr.span,
                ty: expected,
                kind: TExprKind::SliceToConst(Box::new(expr)),
            };
        }
        if let (
            Ty::Slice {
                mutability: expected_mutability,
                elem: expected_elem,
            },
            Ty::Array {
                elem: actual_elem, ..
            },
        ) = (&expected, &expr_ty)
            && expected_elem == actual_elem
        {
            let access = self.texpr_lvalue_access(scopes, &expr);
            if *expected_mutability == ViewMutability::Writable
                && !matches!(access, Some(LvalueAccess::Writable))
            {
                self.diagnostics.push(Diagnostic::new(
                    expr.span,
                    format!("expected `{expected}`, got `{expr_ty}`"),
                ));
                return expr;
            }
            return TExpr {
                span: expr.span,
                ty: Ty::Slice {
                    mutability: *expected_mutability,
                    elem: expected_elem.clone(),
                },
                kind: TExprKind::ArrayToSlice(Box::new(expr)),
            };
        }
        if expected.can_assign_from(&expr_ty)
            || self.meta_repr_marker_matches_concrete(&expected, &expr_ty)
            || contains_generic(&expected)
            || matches!(expr_ty, Ty::Unknown)
        {
            return TExpr {
                span: expr.span,
                ty: if contains_type_hole(&expr.ty) {
                    expected
                } else {
                    expr.ty
                },
                kind: expr.kind,
            };
        }
        if let (
            Ty::Pointer {
                nullable: false,
                mutability: expected_mutability,
                inner: expected_inner,
            },
            Ty::Pointer {
                nullable: true,
                mutability: actual_mutability,
                inner: actual_inner,
            },
        ) = (&expected, &expr_ty)
            && expected_inner == actual_inner
            && expected_mutability == actual_mutability
            && matches!(expr.kind, TExprKind::Literal(Literal::Null))
        {
            return expr;
        }
        if let Ty::DynamicInterface { name, args } = &expected
            && self.type_satisfies_dynamic_view(name, args, &expr_ty)
        {
            return TExpr {
                span: expr.span,
                ty: expected,
                kind: TExprKind::MakeDynamicInterface {
                    concrete_ty: expr_ty,
                    expr: Box::new(expr),
                },
            };
        }
        if let Ty::Closure {
            ret: expected_ret,
            params: expected_params,
            constraints: expected_constraints,
        } = &expected
            && let Ty::Function {
                is_unsafe: false,
                abi: None,
                ret: actual_ret,
                params: actual_params,
            } = &expr_ty
            && expected_params == actual_params
            && expected_ret.can_assign_from(actual_ret)
            && self.closure_constraints_satisfied_by_ty(
                expected_constraints,
                &expr_ty,
                expr.span,
                true,
            )
        {
            return TExpr {
                span: expr.span,
                ty: expected,
                kind: TExprKind::FunctionToClosure(Box::new(expr)),
            };
        }
        self.diagnostics.push(Diagnostic::new(
            expr.span,
            format!("expected `{expected}`, got `{expr_ty}`"),
        ));
        expr
    }

    fn check_lvalue(
        &mut self,
        scopes: &mut LocalScopes,
        expr: &Expr,
        require_assigned: bool,
    ) -> Option<CheckedLvalue> {
        match &expr.kind {
            ExprKind::Name(name_ref) => {
                let Some(local_id) = self.resolved_local_id(name_ref) else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!("unresolved local `{}`", name_ref.display),
                    ));
                    return None;
                };
                let Some(binding) = scopes.get(local_id) else {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!("unresolved local `{}`", name_ref.display),
                    ));
                    return None;
                };
                let name = binding.name.clone();
                if require_assigned && !binding.init_state.is_assigned() {
                    self.diagnostics.push(Diagnostic::new(
                        expr.span,
                        format!("local `{name}` is not definitely assigned"),
                    ));
                }
                let expr = TExpr {
                    span: expr.span,
                    ty: binding.ty.clone(),
                    kind: TExprKind::Local(local_id, name),
                };
                if binding.captured {
                    Some(CheckedLvalue::read_only(
                        expr,
                        ReadOnlyReason::CapturedBinding(binding.name.clone()),
                    ))
                } else if binding.mutability == BindingMutability::Mutable {
                    Some(CheckedLvalue::writable(expr))
                } else {
                    Some(CheckedLvalue::read_only(
                        expr,
                        ReadOnlyReason::ImmutableBinding(binding.name.clone()),
                    ))
                }
            }
            ExprKind::Field { base, field } => {
                let base = self.check_lvalue(scopes, base, true)?;
                let ty = self.field_ty(&base.expr.ty, &field.name, field.span)?;
                let expr = TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Field {
                        base: Box::new(base.expr),
                        field: field.name.clone(),
                    },
                };
                Some(CheckedLvalue {
                    expr,
                    access: base.access,
                    read_only_reason: base.read_only_reason,
                })
            }
            ExprKind::Arrow { base, field } => {
                let base = self.check_expr(scopes, base, None)?;
                let (mutability, ty) = {
                    let Ty::Pointer {
                        nullable: false,
                        mutability,
                        inner,
                    } = &base.ty
                    else {
                        self.diagnostics.push(Diagnostic::new(
                            base.span,
                            format!("`->` requires non-null pointer, got `{}`", base.ty),
                        ));
                        return None;
                    };
                    (*mutability, self.field_ty(inner, &field.name, field.span)?)
                };
                let expr = TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Arrow {
                        base: Box::new(base),
                        field: field.name.clone(),
                    },
                };
                Some(CheckedLvalue::from_view(
                    expr,
                    mutability,
                    ReadOnlyReason::ReadOnlyPointer,
                ))
            }
            ExprKind::Index { base, index } => {
                let base_expr = self.check_expr(scopes, base, None)?;
                let index = self.check_expr(scopes, index, Some(&Ty::Usize))?;
                if !index.ty.is_integer() {
                    self.diagnostics
                        .push(Diagnostic::new(index.span, "index must be integer"));
                }
                match base_expr.ty.clone() {
                    Ty::Slice { mutability, elem } => {
                        let expr = TExpr {
                            span: expr.span,
                            ty: (*elem).clone(),
                            kind: TExprKind::Index {
                                base: Box::new(base_expr),
                                index: Box::new(index),
                            },
                        };
                        Some(CheckedLvalue::from_view(
                            expr,
                            mutability,
                            ReadOnlyReason::ReadOnlySlice,
                        ))
                    }
                    Ty::Array { elem, .. } => {
                        let base = self.check_lvalue(scopes, base, true)?;
                        let expr = TExpr {
                            span: expr.span,
                            ty: (*elem).clone(),
                            kind: TExprKind::Index {
                                base: Box::new(base.expr),
                                index: Box::new(index),
                            },
                        };
                        Some(CheckedLvalue {
                            expr,
                            access: base.access,
                            read_only_reason: base.read_only_reason,
                        })
                    }
                    _ => {
                        self.diagnostics.push(Diagnostic::new(
                            base_expr.span,
                            format!("cannot index `{}`", base_expr.ty),
                        ));
                        None
                    }
                }
            }
            ExprKind::Unary {
                op: UnaryOp::Deref,
                expr: inner,
            } => {
                let inner = self.check_expr(scopes, inner, None)?;
                let (ty, mutability) = match &inner.ty {
                    Ty::Pointer {
                        nullable: false,
                        mutability,
                        inner,
                        ..
                    } => ((**inner).clone(), *mutability),
                    Ty::Pointer { nullable: true, .. } => {
                        self.diagnostics.push(Diagnostic::new(
                            inner.span,
                            "cannot dereference nullable pointer without narrowing",
                        ));
                        (Ty::Unknown, ViewMutability::ReadOnly)
                    }
                    _ => {
                        self.diagnostics.push(Diagnostic::new(
                            inner.span,
                            format!("cannot dereference `{}`", inner.ty),
                        ));
                        (Ty::Unknown, ViewMutability::ReadOnly)
                    }
                };
                let expr = TExpr {
                    span: expr.span,
                    ty,
                    kind: TExprKind::Unary {
                        op: UnaryOp::Deref,
                        expr: Box::new(inner),
                    },
                };
                Some(CheckedLvalue::from_view(
                    expr,
                    mutability,
                    ReadOnlyReason::ReadOnlyPointer,
                ))
            }
            _ => {
                self.diagnostics
                    .push(Diagnostic::new(expr.span, "expression is not assignable"));
                None
            }
        }
    }

    fn validate_assignment_target(
        &mut self,
        scopes: &LocalScopes,
        target: &CheckedLvalue,
        span: crate::span::Span,
    ) -> bool {
        if target.access.is_writable() {
            return true;
        }
        if let TExprKind::Local(local_id, name) = &target.expr.kind {
            let Some(binding) = scopes.get(*local_id) else {
                return false;
            };
            if binding.captured {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("cannot mutate captured binding `{name}`"),
                ));
                return false;
            }
            if binding.mutability == BindingMutability::Immutable {
                match binding.init_state {
                    InitState::Unassigned => {
                        if binding.declared_loop_depth < self.current_loop_depth {
                            self.diagnostics.push(Diagnostic::new(
                                span,
                                format!(
                                    "cannot initialize immutable binding `{name}` from a loop body"
                                ),
                            ));
                            return false;
                        }
                        return true;
                    }
                    InitState::Assigned => {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            format!("cannot initialize immutable binding `{name}` more than once"),
                        ));
                        return false;
                    }
                    InitState::MaybeAssigned => {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            format!("cannot initialize maybe-assigned immutable binding `{name}`"),
                        ));
                        return false;
                    }
                }
            }
        }

        match target.read_only_reason.as_ref() {
            Some(ReadOnlyReason::CapturedBinding(name)) => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("cannot mutate captured binding `{name}`"),
                ));
            }
            Some(ReadOnlyReason::ImmutableBinding(name)) => {
                if let Some((local_id, _)) = lvalue_root_local(&target.expr)
                    && let Some(binding) = scopes.get(local_id)
                    && binding.init_state == InitState::Unassigned
                {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!("cannot partially initialize immutable binding `{name}`"),
                    ));
                    return false;
                }
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("cannot mutate immutable binding `{name}`"),
                ));
            }
            Some(ReadOnlyReason::ReadOnlyPointer) => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    "cannot write through read-only pointer",
                ));
            }
            Some(ReadOnlyReason::ReadOnlySlice) => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    "cannot write through read-only slice",
                ));
            }
            None => {
                self.diagnostics
                    .push(Diagnostic::new(span, "expression is not writable"));
            }
        }
        false
    }

    fn mark_assignment_complete(&mut self, scopes: &mut LocalScopes, target: &TExpr) {
        if let TExprKind::Local(local_id, _) = &target.kind
            && let Some(binding) = scopes.get_mut(*local_id)
        {
            binding.init_state = InitState::Assigned;
            binding.narrowed_ty = None;
        }
    }

    fn texpr_lvalue_access(&self, scopes: &LocalScopes, expr: &TExpr) -> Option<LvalueAccess> {
        match &expr.kind {
            TExprKind::Local(local_id, _) => scopes.get(*local_id).map(|binding| {
                if !binding.captured && binding.mutability == BindingMutability::Mutable {
                    LvalueAccess::Writable
                } else {
                    LvalueAccess::ReadOnly
                }
            }),
            TExprKind::Field { base, .. } => self.texpr_lvalue_access(scopes, base),
            TExprKind::Arrow { base, .. } => match &base.ty {
                Ty::Pointer { mutability, .. } => Some(LvalueAccess::from_view(*mutability)),
                _ => None,
            },
            TExprKind::Index { base, .. } => match &base.ty {
                Ty::Slice { mutability, .. } => Some(LvalueAccess::from_view(*mutability)),
                Ty::Array { .. } => self.texpr_lvalue_access(scopes, base),
                _ => None,
            },
            TExprKind::Unary {
                op: UnaryOp::Deref,
                expr,
            } => match &expr.ty {
                Ty::Pointer { mutability, .. } => Some(LvalueAccess::from_view(*mutability)),
                _ => None,
            },
            _ => None,
        }
    }

    fn check_variant_literal(
        &mut self,
        scopes: &mut LocalScopes,
        span: crate::span::Span,
        variant_name: &str,
        sig: VariantSig,
        args: Vec<Expr>,
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        let enum_ty = match expected {
            Some(Ty::Named { name, args }) if name == &sig.enum_name => Ty::Named {
                name: name.clone(),
                args: args.clone(),
            },
            Some(other) => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "variant `{variant_name}` constructs `{}`, not `{other}`",
                        sig.enum_name
                    ),
                ));
                return None;
            }
            None if sig.enum_generics.is_empty() => Ty::Named {
                name: sig.enum_name.clone(),
                args: Vec::new(),
            },
            None => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("generic variant `{variant_name}` requires an expected enum type"),
                ));
                return None;
            }
        };

        let Ty::Named {
            name: enum_name,
            args: enum_args,
        } = &enum_ty
        else {
            unreachable!("variant enum type is always named");
        };
        if enum_args.len() != sig.enum_generics.len() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "enum `{enum_name}` expects {} type arguments, got {}",
                    sig.enum_generics.len(),
                    enum_args.len()
                ),
            ));
            return None;
        }
        let subst = sig
            .enum_generics
            .iter()
            .cloned()
            .zip(enum_args.iter().cloned())
            .collect::<HashMap<_, _>>();
        let logical_payload_tys = sig
            .payload
            .iter()
            .map(|ty| self.lower_type_with_subst(ty, &subst))
            .collect::<Vec<_>>();
        let physical_payload_tys = logical_payload_tys
            .iter()
            .filter(|ty| !ty.is_erased_value())
            .cloned()
            .collect::<Vec<_>>();
        let use_logical_payload = args.len() == logical_payload_tys.len();
        let use_physical_payload = args.len() == physical_payload_tys.len() && !use_logical_payload;
        if !use_logical_payload && !use_physical_payload {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "variant `{variant_name}` expects {} payload values, got {}",
                    physical_payload_tys.len(),
                    args.len()
                ),
            ));
            return None;
        }

        let mut payload = Vec::new();
        let payload_inputs = if use_logical_payload {
            args.iter()
                .zip(logical_payload_tys.iter())
                .collect::<Vec<_>>()
        } else {
            args.iter()
                .zip(physical_payload_tys.iter())
                .collect::<Vec<_>>()
        };
        for (arg, expected_ty) in payload_inputs {
            let expected_ty = self.resolve_type_holes(expected_ty);
            let checked = self.check_expr(scopes, arg, Some(&expected_ty))?;
            self.require_assignable(&expected_ty, &checked.ty, checked.span);
            if use_logical_payload || !expected_ty.is_erased_value() {
                payload.push(checked);
            }
        }

        let enum_ty = self.resolve_type_holes(&enum_ty);
        self.ensure_enum_instance(&enum_ty);
        let Ty::Named {
            name: enum_name,
            args: enum_args,
        } = &enum_ty
        else {
            unreachable!("variant enum type is always named");
        };
        let type_name = enum_instance_name(enum_name, enum_args);
        Some(TExpr {
            span,
            ty: enum_ty,
            kind: TExprKind::EnumLiteral {
                type_name,
                variant_name: variant_name.to_string(),
                variant_index: sig.variant_index,
                payload,
            },
        })
    }

    fn check_literal(
        &mut self,
        span: crate::span::Span,
        literal: &Literal,
        expected: Option<&Ty>,
    ) -> Option<TExpr> {
        let ty = match literal {
            Literal::Integer(raw) => {
                let ty = expected
                    .filter(|ty| ty.is_integer() || matches!(ty, Ty::Char | Ty::CSpelling { .. }))
                    .cloned()
                    .unwrap_or(Ty::I64);
                if ty.is_integer() || matches!(ty, Ty::Char) {
                    self.check_integer_literal_range(span, raw, &ty, false);
                }
                ty
            }
            Literal::Float(raw) => {
                let ty = expected
                    .filter(|ty| matches!(ty, Ty::F32 | Ty::F64 | Ty::CSpelling { .. }))
                    .cloned()
                    .unwrap_or(Ty::F64);
                if matches!(ty, Ty::F32 | Ty::F64) {
                    self.check_float_literal_range(span, raw, &ty);
                }
                ty
            }
            Literal::Char(raw) => {
                self.check_char_literal_range(span, raw);
                Ty::Char
            }
            Literal::String(_) => Ty::Slice {
                mutability: ViewMutability::ReadOnly,
                elem: Box::new(Ty::Char),
            },
            Literal::Bool(_) => Ty::Bool,
            Literal::Null => match expected {
                Some(Ty::Pointer {
                    inner, mutability, ..
                }) => Ty::Pointer {
                    nullable: true,
                    mutability: *mutability,
                    inner: inner.clone(),
                },
                _ => {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        "`null` requires an expected nullable pointer type",
                    ));
                    Ty::Unknown
                }
            },
        };
        Some(TExpr {
            span,
            ty,
            kind: TExprKind::Literal(literal.clone()),
        })
    }

    fn check_integer_literal_range(
        &mut self,
        span: crate::span::Span,
        raw: &str,
        ty: &Ty,
        negated: bool,
    ) {
        let Some(value) = parse_integer_literal_u128(raw) else {
            self.diagnostics
                .push(Diagnostic::new(span, "integer literal is out of range"));
            return;
        };
        let Some((min_abs, max)) = integer_abs_limits(ty) else {
            return;
        };
        let limit = if negated { min_abs } else { max };
        if value > limit {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("integer literal `{raw}` is out of range for `{ty}`"),
            ));
        }
    }

    fn check_float_literal_range(&mut self, span: crate::span::Span, raw: &str, ty: &Ty) {
        let normalized = raw.replace('_', "");
        let Ok(value) = normalized.parse::<f64>() else {
            self.diagnostics
                .push(Diagnostic::new(span, "float literal is invalid"));
            return;
        };
        if matches!(ty, Ty::F32) && value.is_finite() && value.abs() > f32::MAX as f64 {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("float literal `{raw}` is out of range for `f32`"),
            ));
        }
    }

    fn check_char_literal_range(&mut self, span: crate::span::Span, raw: &str) {
        if decode_char_literal_byte(raw).is_none() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("char literal `{raw}` is not a single byte"),
            ));
        }
    }

    fn check_binary(
        &mut self,
        op: BinaryOp,
        left: &TExpr,
        right: &TExpr,
        span: crate::span::Span,
    ) -> Ty {
        use BinaryOp::*;
        match op {
            Or | And => {
                self.require_assignable(&Ty::Bool, &left.ty, left.span);
                self.require_assignable(&Ty::Bool, &right.ty, right.span);
                Ty::Bool
            }
            Eq | Ne => {
                if !left.ty.can_assign_from(&right.ty) && !right.ty.can_assign_from(&left.ty) {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!("cannot compare `{}` and `{}`", left.ty, right.ty),
                    ));
                }
                if self.is_c_aggregate_value(&left.ty) || self.is_c_aggregate_value(&right.ty) {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        "struct, enum, slice, and dynamic interface values cannot be compared directly",
                    ));
                }
                if matches!(left.ty, Ty::Closure { .. } | Ty::ClosureInstance { .. })
                    || matches!(right.ty, Ty::Closure { .. } | Ty::ClosureInstance { .. })
                {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        "closure values cannot be compared directly",
                    ));
                }
                Ty::Bool
            }
            Lt | Le | Gt | Ge => {
                if !left.ty.is_numeric() && !matches!(left.ty, Ty::Char) {
                    self.diagnostics.push(Diagnostic::new(
                        left.span,
                        format!("relational operator does not accept `{}`", left.ty),
                    ));
                }
                self.require_assignable(&left.ty, &right.ty, right.span);
                Ty::Bool
            }
            Add | Sub | Mul | Div | Rem => {
                if !left.ty.is_numeric() {
                    self.diagnostics.push(Diagnostic::new(
                        left.span,
                        format!("arithmetic operator does not accept `{}`", left.ty),
                    ));
                }
                if matches!(op, Rem) && !left.ty.is_integer() {
                    self.diagnostics
                        .push(Diagnostic::new(left.span, "`%` requires integer operands"));
                }
                self.require_assignable(&left.ty, &right.ty, right.span);
                left.ty.clone()
            }
            BitOr | BitXor | BitAnd => {
                if !left.ty.is_integer() {
                    self.diagnostics.push(Diagnostic::new(
                        left.span,
                        format!("bitwise operator does not accept `{}`", left.ty),
                    ));
                }
                self.require_same_integer_type("bitwise operator", left, right, span);
                left.ty.clone()
            }
            Shl | Shr => {
                if !left.ty.is_integer() {
                    self.diagnostics.push(Diagnostic::new(
                        left.span,
                        format!("shift operator does not accept `{}`", left.ty),
                    ));
                }
                if !right.ty.is_integer() {
                    self.diagnostics.push(Diagnostic::new(
                        right.span,
                        format!("shift count must be an integer, got `{}`", right.ty),
                    ));
                }
                self.check_constant_shift_count(left, right);
                left.ty.clone()
            }
        }
    }

    fn require_same_integer_type(
        &mut self,
        context: &str,
        left: &TExpr,
        right: &TExpr,
        span: crate::span::Span,
    ) {
        if matches!(left.ty, Ty::Unknown) || matches!(right.ty, Ty::Unknown) {
            return;
        }
        if !left.ty.is_integer() || !right.ty.is_integer() {
            return;
        }
        if left.ty != right.ty {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!(
                    "{context} requires matching integer types, got `{}` and `{}`",
                    left.ty, right.ty
                ),
            ));
        }
    }

    fn check_constant_shift_count(&mut self, left: &TExpr, right: &TExpr) {
        let Some(width) = left.ty.integer_bit_width() else {
            return;
        };
        let count = match &right.kind {
            TExprKind::Literal(Literal::Integer(raw)) => {
                let Some(count) = parse_integer_literal_u128(raw) else {
                    return;
                };
                (raw.clone(), Some(count))
            }
            TExprKind::Unary {
                op: UnaryOp::Neg,
                expr,
            } => {
                let TExprKind::Literal(Literal::Integer(raw)) = &expr.kind else {
                    return;
                };
                (format!("-{raw}"), None)
            }
            _ => return,
        };
        if count.1.is_none_or(|value| value >= u128::from(width)) {
            self.diagnostics.push(Diagnostic::new(
                right.span,
                format!(
                    "constant shift count `{}` is out of range for `{}`; expected 0..{}",
                    count.0,
                    left.ty,
                    width - 1
                ),
            ));
        }
    }

    fn field_ty(&mut self, base: &Ty, field: &str, span: crate::span::Span) -> Option<Ty> {
        match base {
            Ty::Slice { mutability, elem } if field == "ptr" => Some(Ty::Pointer {
                nullable: false,
                mutability: *mutability,
                inner: Box::new((**elem).clone()),
            }),
            Ty::Slice { .. } if field == "len" => Some(Ty::Usize),
            Ty::Named { name, args } => {
                let instance_name = enum_instance_name(name, args);
                let fields = if let Some(fields) = self.structs.get(&instance_name).cloned() {
                    fields
                } else if let Some(template) = self.struct_templates.get(name).cloned() {
                    if template.generics.len() != args.len() {
                        self.diagnostics.push(Diagnostic::new(
                            span,
                            format!(
                                "struct `{name}` expects {} type arguments, got {}",
                                template.generics.len(),
                                args.len()
                            ),
                        ));
                        return None;
                    }
                    let subst = template
                        .generics
                        .iter()
                        .cloned()
                        .zip(args.iter().cloned())
                        .collect::<HashMap<_, _>>();
                    template
                        .fields
                        .iter()
                        .map(|field| {
                            (
                                field.name.name.clone(),
                                self.lower_type_with_subst_allowing_holes(&field.ty, &subst),
                            )
                        })
                        .collect()
                } else {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!("type `{base}` has no field `{field}`"),
                    ));
                    return None;
                };
                if let Some((_, ty)) = fields.iter().find(|(candidate, _)| candidate == field) {
                    let ty = ty.clone();
                    if self.is_unsafe_struct_instance(name, args) {
                        self.require_unsafe(
                            span,
                            format!("field access on unsafe struct `{name}` requires unsafe block"),
                        );
                    }
                    Some(ty)
                } else {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!("unknown field `{field}` on `{base}`"),
                    ));
                    None
                }
            }
            _ => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("type `{base}` has no field `{field}`"),
                ));
                None
            }
        }
    }

    fn require_assignable(&mut self, expected: &Ty, actual: &Ty, span: crate::span::Span) {
        if contains_type_hole(expected) || contains_type_hole(actual) {
            self.unify_type_holes(expected, actual);
        }
        let expected = self.resolve_type_holes(expected);
        let actual = self.resolve_type_holes(actual);
        let expected = self.meta_repr_storage_ty(&expected, span);
        let actual = self.meta_repr_storage_ty(&actual, span);
        if contains_generic(&expected) || contains_generic(&actual) {
            return;
        }
        if matches!(expected, Ty::Unknown) || matches!(actual, Ty::Unknown) {
            return;
        }
        if self.meta_repr_marker_matches_concrete(&expected, &actual)
            || self.meta_repr_marker_matches_concrete(&actual, &expected)
        {
            return;
        }
        if let Ty::Closure {
            ret,
            params,
            constraints,
        } = &expected
            && closure_shape_satisfies(ret, params, &actual)
        {
            self.closure_constraints_satisfied_by_ty(constraints, &actual, span, false);
            return;
        }
        if !expected.can_assign_from(&actual) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("expected `{expected}`, got `{actual}`"),
            ));
        }
    }

    fn check_cast_allowed(&mut self, source: &Ty, target: &Ty, span: crate::span::Span) {
        let source = source;
        let target = target;
        if matches!(source, Ty::Unknown) || matches!(target, Ty::Unknown) || source == target {
            return;
        }
        if matches!(source, Ty::Bool) || matches!(target, Ty::Bool) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("cannot cast between `{source}` and `{target}`"),
            ));
            return;
        }
        if (source.is_numeric() || matches!(source, Ty::Char))
            && (target.is_numeric() || matches!(target, Ty::Char))
        {
            return;
        }
        if (source.is_numeric() || matches!(source, Ty::Char | Ty::CSpelling { .. }))
            && (target.is_numeric() || matches!(target, Ty::Char | Ty::CSpelling { .. }))
        {
            return;
        }
        if let (
            Ty::Pointer {
                nullable: source_nullable,
                mutability: source_mutability,
                inner: source_inner,
            },
            Ty::Pointer {
                nullable: target_nullable,
                mutability: target_mutability,
                inner: target_inner,
            },
        ) = (source, target)
        {
            if *source_nullable && !*target_nullable {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    "nullable pointer cannot be cast to non-null pointer without narrowing",
                ));
                return;
            }
            if source_mutability.is_read_only() && target_mutability.is_writable() {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!("cannot cast `{source}` to `{target}`"),
                ));
                return;
            }
            if matches!(&**source_inner, Ty::Void) || matches!(&**target_inner, Ty::Void) {
                return;
            }
            self.diagnostics.push(Diagnostic::new(
                span,
                "pointer casts must go through `*void` or `?*void`",
            ));
            return;
        }
        self.diagnostics.push(Diagnostic::new(
            span,
            format!("cannot cast `{source}` to `{target}`"),
        ));
    }

    fn require_unsafe_pointer_cast_through_void(
        &mut self,
        source: &Ty,
        target: &Ty,
        span: crate::span::Span,
    ) {
        let (
            Ty::Pointer {
                inner: source_inner,
                ..
            },
            Ty::Pointer {
                inner: target_inner,
                ..
            },
        ) = (source, target)
        else {
            return;
        };
        if source == target {
            return;
        }
        if matches!((&**source_inner, &**target_inner), (Ty::Void, target) if !matches!(target, Ty::Void))
        {
            self.require_unsafe(span, "raw pointer casts from `*void` require unsafe block");
        }
    }

    fn require_unsafe(&mut self, span: crate::span::Span, message: impl Into<String>) {
        if self.unsafe_depth == 0 {
            self.diagnostics.push(Diagnostic::new(span, message.into()));
        }
    }

    fn reject_invalid_plain_value_type(&mut self, ty: &Ty, span: crate::span::Span, context: &str) {
        if ty.is_never() {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{context} cannot have type `never`"),
            ));
            return;
        }
        if self.is_opaque_by_value(ty) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{context} cannot use opaque struct `{ty}` by value"),
            ));
        }
        if type_contains_plain_never_value(ty) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("{context} cannot contain `never` by value"),
            ));
        }
    }

    fn reject_invalid_return_type(&mut self, ty: &Ty, span: crate::span::Span) {
        if self.is_opaque_by_value(ty) {
            self.diagnostics.push(Diagnostic::new(
                span,
                format!("function cannot return opaque struct `{ty}` by value"),
            ));
        }
        match ty {
            Ty::Array { elem, .. } | Ty::Slice { elem, .. }
                if type_contains_plain_never_value(elem) =>
            {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    "function return type cannot contain `never` by value",
                ));
            }
            _ => {}
        }
    }

    fn is_opaque_by_value(&self, ty: &Ty) -> bool {
        matches!(ty, Ty::Named { name, args } if args.is_empty() && self.opaque_structs.contains(name))
    }

    fn is_unsafe_struct_instance(&self, name: &str, args: &[Ty]) -> bool {
        let instance_name = enum_instance_name(name, args);
        self.unsafe_structs.contains(&instance_name)
            || self
                .struct_templates
                .get(name)
                .is_some_and(|template| template.is_unsafe)
    }

    fn is_c_aggregate_value(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Named { name, args } => {
                let instance_name = enum_instance_name(name, args);
                self.structs.contains_key(&instance_name)
                    || self.checked_enums.contains_key(&instance_name)
            }
            Ty::Slice { .. } | Ty::DynamicInterface { .. } => true,
            _ => false,
        }
    }

    fn find_impl(&self, interface_name: &str, args: &[Ty], receiver_ty: &Ty) -> Option<&ImplSig> {
        self.impls.iter().find(|implementation| {
            implementation.interface_name == interface_name
                && implementation
                    .receiver_ty
                    .as_ref()
                    .is_some_and(|candidate| candidate == receiver_ty)
                && interface_non_receiver_args(&implementation.interface_args) == args
        })
    }

    fn type_implements_capability(
        &mut self,
        interface_name: &str,
        args: &[Ty],
        receiver_ty: &Ty,
    ) -> bool {
        let storage_receiver_ty = self.meta_repr_storage_ty(receiver_ty, None);
        let receiver_ty = &storage_receiver_ty;
        if self.is_std_message_capability_interface_name(interface_name)
            && args.is_empty()
            && let Ty::ClosureInstance { captures, .. } = receiver_ty
            && !captures
                .iter()
                .all(|capture| self.type_implements_capability(interface_name, args, capture))
        {
            return false;
        }
        if self.type_implements_compiler_provided_meta_marker(interface_name, args, receiver_ty) {
            return true;
        }
        if retained_closure_proves_capability(receiver_ty, interface_name, args) {
            return true;
        }
        self.find_impl(interface_name, args, receiver_ty).is_some()
            || self
                .instantiate_generic_impl_for_receiver(interface_name, args, receiver_ty, None)
                .is_some()
    }

    fn type_implements_compiler_provided_meta_marker(
        &self,
        interface_name: &str,
        args: &[Ty],
        receiver_ty: &Ty,
    ) -> bool {
        args.is_empty()
            && ((self.is_std_meta_ciel_fn_value_marker_name(interface_name)
                && matches!(
                    receiver_ty,
                    Ty::Function {
                        is_unsafe: false,
                        abi: None,
                        ..
                    }
                ))
                || (self.is_std_meta_closure_value_marker_name(interface_name)
                    && matches!(receiver_ty, Ty::ClosureInstance { .. })))
    }

    fn closure_constraints_satisfied_by_ty(
        &mut self,
        constraints: &ConstraintBounds,
        source_ty: &Ty,
        span: crate::span::Span,
        emit_diagnostics: bool,
    ) -> bool {
        let mut ok = true;
        for capability in &constraints.positive {
            if !self.type_implements_capability(&capability.name, &capability.args, source_ty) {
                ok = false;
                if emit_diagnostics {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!(
                            "closure conversion requires `{}` to implement `{}`",
                            source_ty, capability.name
                        ),
                    ));
                }
            }
        }
        for capability in &constraints.negative {
            if self.type_implements_capability(&capability.name, &capability.args, source_ty) {
                ok = false;
                if emit_diagnostics {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!(
                            "closure conversion forbids `{}` from implementing `{}`",
                            source_ty, capability.name
                        ),
                    ));
                }
            }
        }
        ok
    }

    fn type_implements_message(&mut self, ty: &Ty) -> bool {
        self.type_implements_capability(STD_MESSAGE_CLONE_INTERFACE, &[], ty)
    }

    fn meta_repr_marker_matches_concrete(&mut self, marker: &Ty, concrete: &Ty) -> bool {
        let Ty::Named { name, args } = marker else {
            return false;
        };
        if !matches!(meta_repr_marker_name(name), Some(false)) || args.len() != 1 {
            return false;
        }
        if contains_generic(&args[0]) || contains_type_hole(&args[0]) {
            return false;
        }
        self.try_meta_repr_ty(&args[0], false)
            .is_some_and(|repr_ty| repr_ty == *concrete)
    }

    fn type_implements_share_handle(&mut self, ty: &Ty) -> bool {
        if !self.is_std_message_share_handle_marker_name(STD_MESSAGE_SHARE_HANDLE_INTERFACE) {
            return false;
        }
        self.find_impl(STD_MESSAGE_SHARE_HANDLE_INTERFACE, &[], ty)
            .is_some()
            || self.generic_impl_matches_without_constraints(
                STD_MESSAGE_SHARE_HANDLE_INTERFACE,
                &[],
                ty,
            )
    }

    fn type_implements_thread_local(&mut self, ty: &Ty) -> bool {
        if !self.is_std_message_thread_local_marker_name(STD_MESSAGE_THREAD_LOCAL_INTERFACE) {
            return false;
        }
        self.find_impl(STD_MESSAGE_THREAD_LOCAL_INTERFACE, &[], ty)
            .is_some()
            || self.generic_impl_matches_without_constraints(
                STD_MESSAGE_THREAD_LOCAL_INTERFACE,
                &[],
                ty,
            )
    }

    fn generic_impl_matches_without_constraints(
        &self,
        interface_name: &str,
        non_receiver_args: &[Ty],
        receiver_ty: &Ty,
    ) -> bool {
        let interface_args = std::iter::once(receiver_ty.clone())
            .chain(non_receiver_args.iter().cloned())
            .collect::<Vec<_>>();
        self.generic_impls.iter().any(|template| {
            if template.interface_name != interface_name
                || template.interface_args.len() != interface_args.len()
            {
                return false;
            }
            if template
                .generics
                .iter()
                .any(|generic| generic.constraint.is_some())
            {
                return false;
            }
            let mut subst = template
                .generics
                .iter()
                .map(|generic| (generic.name.clone(), Ty::Generic(generic.name.clone())))
                .collect::<HashMap<_, _>>();
            template
                .interface_args
                .iter()
                .zip(interface_args.iter())
                .all(|(pattern, actual)| unify_ty(pattern, actual, &mut subst))
                && template
                    .receiver_ty
                    .as_ref()
                    .is_some_and(|pattern| unify_ty(pattern, receiver_ty, &mut subst))
                && template.generics.iter().all(|generic| {
                    subst
                        .get(&generic.name)
                        .is_some_and(|ty| !contains_generic(ty))
                })
        })
    }

    fn find_impl_by_full_args(
        &self,
        interface_name: &str,
        interface_args: &[Ty],
        receiver_ty: Option<&Ty>,
    ) -> Option<ImplSig> {
        find_impl_in(&self.impls, interface_name, interface_args, receiver_ty).cloned()
    }

    fn find_or_instantiate_impl_by_full_args(
        &mut self,
        interface_name: &str,
        interface_args: &[Ty],
        receiver_ty: Option<&Ty>,
        span: crate::span::Span,
    ) -> Option<ImplSig> {
        self.find_impl_by_full_args(interface_name, interface_args, receiver_ty)
            .or_else(|| {
                self.instantiate_generic_impl(
                    interface_name,
                    interface_args,
                    receiver_ty,
                    Some(span),
                )
            })
    }

    fn instantiate_generic_impl_for_receiver(
        &mut self,
        interface_name: &str,
        non_receiver_args: &[Ty],
        receiver_ty: &Ty,
        span: Option<crate::span::Span>,
    ) -> Option<ImplSig> {
        let interface_args = std::iter::once(receiver_ty.clone())
            .chain(non_receiver_args.iter().cloned())
            .collect::<Vec<_>>();
        self.instantiate_generic_impl(interface_name, &interface_args, Some(receiver_ty), span)
    }

    fn instantiate_generic_impl(
        &mut self,
        interface_name: &str,
        interface_args: &[Ty],
        receiver_ty: Option<&Ty>,
        span: Option<crate::span::Span>,
    ) -> Option<ImplSig> {
        if let Some(existing) =
            self.find_impl_by_full_args(interface_name, interface_args, receiver_ty)
        {
            return Some(existing);
        }
        let templates = self.generic_impls.clone();
        let mut matches = Vec::new();
        for template in templates {
            if template.interface_name != interface_name {
                continue;
            }
            let mut subst = template
                .generics
                .iter()
                .map(|generic| (generic.name.clone(), Ty::Generic(generic.name.clone())))
                .collect::<HashMap<_, _>>();
            if template.interface_args.len() != interface_args.len() {
                continue;
            }
            if !template
                .interface_args
                .iter()
                .zip(interface_args.iter())
                .all(|(pattern, actual)| unify_ty(pattern, actual, &mut subst))
            {
                continue;
            }
            if let (Some(pattern), Some(actual)) = (template.receiver_ty.as_ref(), receiver_ty)
                && !unify_ty(pattern, actual, &mut subst)
            {
                continue;
            }
            if template
                .generics
                .iter()
                .any(|generic| subst.get(&generic.name).is_none_or(contains_generic))
            {
                continue;
            }
            let diagnostic_count = self.diagnostics.len();
            self.check_generic_constraints(
                &template.generics,
                &subst,
                span.unwrap_or(template.item_span),
            );
            if self.diagnostics.len() != diagnostic_count {
                self.diagnostics.truncate(diagnostic_count);
                continue;
            }
            let instance_span = span.unwrap_or(template.item_span);
            let params = template
                .params
                .iter()
                .map(|ty| self.substitute_ty_normalized(ty, &subst, instance_span))
                .collect::<Vec<_>>();
            let ret = self.substitute_ty_normalized(&template.ret, &subst, instance_span);
            let concrete_interface_args = template
                .interface_args
                .iter()
                .map(|ty| self.substitute_ty_normalized(ty, &subst, instance_span))
                .collect::<Vec<_>>();
            let concrete_receiver = template
                .receiver_ty
                .as_ref()
                .map(|ty| self.substitute_ty_normalized(ty, &subst, instance_span));
            matches.push((
                template,
                concrete_interface_args,
                concrete_receiver,
                ret,
                params,
                subst,
            ));
        }
        if matches.len() > 1 {
            if self.is_std_message_capability_interface_name(interface_name) {
                self.diagnostics.push(Diagnostic::new(
                    span.unwrap_or(matches[0].0.item_span),
                    format!("ambiguous generic impls for marker interface `{interface_name}`"),
                ));
            }
            return None;
        }
        if let Some((template, concrete_interface_args, concrete_receiver, ret, params, subst)) =
            matches.into_iter().next()
        {
            return self.instantiate_impl_body(
                template.module,
                &template.decl,
                &template.interface_name,
                concrete_interface_args,
                concrete_receiver,
                ret,
                params,
                &subst,
            );
        }
        None
    }

    fn merge_existing_impls(&mut self, impls: &[CheckedImpl]) {
        for implementation in impls {
            if self
                .find_impl_by_full_args(
                    &implementation.interface_name,
                    &implementation.interface_args,
                    implementation.receiver_ty.as_ref(),
                )
                .is_some()
            {
                continue;
            }
            self.impls.push(ImplSig {
                interface_name: implementation.interface_name.clone(),
                interface_args: implementation.interface_args.clone(),
                receiver_ty: implementation.receiver_ty.clone(),
                function_def: implementation.function_def,
                ret: implementation.ret.clone(),
                params: implementation.params.clone(),
            });
        }
    }

    fn dynamic_view_interface(
        &mut self,
        dyn_name: &str,
        dyn_args: &[Ty],
        interface_name: &str,
    ) -> Option<InterfaceRefTy> {
        self.interface_view(dyn_name, dyn_args)
            .positive
            .into_iter()
            .find(|entry| entry.name == interface_name)
    }

    fn type_satisfies_dynamic_view(&mut self, name: &str, args: &[Ty], actual: &Ty) -> bool {
        let view = self.interface_view(name, args);
        if let Ty::DynamicInterface {
            name: actual_name,
            args: actual_args,
        } = actual
        {
            let actual_view = self.interface_view(actual_name, actual_args);
            return view
                .positive
                .iter()
                .all(|expected| actual_view.positive.contains(expected))
                && view
                    .negative
                    .iter()
                    .all(|expected| actual_view.negative.contains(expected));
        }
        let receiver_ty = receiver_ty_from_value_ty(actual);
        view.positive
            .iter()
            .all(|entry| self.type_implements_capability(&entry.name, &entry.args, &receiver_ty))
            && view.negative.iter().all(|entry| {
                !self.type_implements_capability(&entry.name, &entry.args, &receiver_ty)
            })
    }

    fn interface_view(&mut self, name: &str, args: &[Ty]) -> InterfaceView {
        self.interface_view_inner(name, args, &mut HashSet::new())
    }

    fn constraint_bounds(
        &mut self,
        expr: &ConstraintExpr,
        subst: &HashMap<String, Ty>,
    ) -> ConstraintBounds {
        let mut bounds = ConstraintBounds::default();
        for term in &expr.terms {
            let args = term
                .args
                .iter()
                .map(|arg| self.lower_type_with_subst(arg, subst))
                .collect::<Vec<_>>();
            let view = self.interface_view(&name_ref_canonical(&self.resolved, &term.name), &args);
            if term.removed {
                bounds
                    .positive
                    .retain(|entry| !view.positive.contains(entry));
                bounds
                    .negative
                    .retain(|entry| !view.negative.contains(entry));
            } else if term.negated {
                for entry in view.positive {
                    if !bounds.negative.contains(&entry) {
                        bounds.negative.push(entry);
                    }
                }
            } else {
                for entry in view.positive {
                    if !bounds.positive.contains(&entry) {
                        bounds.positive.push(entry);
                    }
                }
                for entry in view.negative {
                    if !bounds.negative.contains(&entry) {
                        bounds.negative.push(entry);
                    }
                }
            }
        }
        bounds
    }

    fn resolve_constraint_bounds_type_holes(&self, bounds: &ConstraintBounds) -> ConstraintBounds {
        ConstraintBounds {
            positive: bounds
                .positive
                .iter()
                .map(|entry| ConstraintRef {
                    name: entry.name.clone(),
                    args: entry
                        .args
                        .iter()
                        .map(|arg| self.resolve_type_holes(arg))
                        .collect(),
                })
                .collect(),
            negative: bounds
                .negative
                .iter()
                .map(|entry| ConstraintRef {
                    name: entry.name.clone(),
                    args: entry
                        .args
                        .iter()
                        .map(|arg| self.resolve_type_holes(arg))
                        .collect(),
                })
                .collect(),
        }
    }

    fn interface_view_inner(
        &mut self,
        name: &str,
        args: &[Ty],
        expanding: &mut HashSet<String>,
    ) -> InterfaceView {
        if let Some(alias) = self
            .interface_alias_names
            .get(name)
            .and_then(|def_id| self.interface_aliases.get(def_id))
            .cloned()
        {
            if alias.generics.len() != args.len() {
                self.diagnostics.push(Diagnostic::new(
                    None,
                    format!(
                        "interface alias `{name}` expects {} type arguments, got {}",
                        alias.generics.len(),
                        args.len()
                    ),
                ));
                return InterfaceView::default();
            }
            if !expanding.insert(name.to_string()) {
                return InterfaceView::default();
            }
            let subst = alias
                .generics
                .iter()
                .map(|generic| generic.name.clone())
                .zip(args.iter().cloned())
                .collect::<HashMap<_, _>>();
            let view = self.interface_view_from_expr(&alias.expr, &subst, expanding);
            expanding.remove(name);
            return view;
        }
        InterfaceView {
            positive: vec![InterfaceRefTy {
                name: name.to_string(),
                args: args.to_vec(),
            }],
            negative: Vec::new(),
        }
    }

    fn interface_view_from_expr(
        &mut self,
        expr: &InterfaceExpr,
        subst: &HashMap<String, Ty>,
        expanding: &mut HashSet<String>,
    ) -> InterfaceView {
        let mut view = InterfaceView::default();
        self.view_add_term(&mut view, &expr.first, subst, expanding);
        for (op, term) in &expr.rest {
            match op {
                InterfaceOp::Add => self.view_add_term(&mut view, term, subst, expanding),
                InterfaceOp::Sub => {
                    let removed = self.interface_view_for_term(term, subst, expanding);
                    view.positive
                        .retain(|entry| !removed.positive.contains(entry));
                    view.negative
                        .retain(|entry| !removed.negative.contains(entry));
                }
            }
        }
        view
    }

    fn view_add_term(
        &mut self,
        view: &mut InterfaceView,
        term: &InterfaceTerm,
        subst: &HashMap<String, Ty>,
        expanding: &mut HashSet<String>,
    ) {
        let term_view = self.interface_view_for_term(term, subst, expanding);
        if term.negated {
            for entry in term_view.positive {
                if !view.negative.contains(&entry) {
                    view.negative.push(entry);
                }
            }
        } else {
            for entry in term_view.positive {
                if !view.positive.contains(&entry) {
                    view.positive.push(entry);
                }
            }
            for entry in term_view.negative {
                if !view.negative.contains(&entry) {
                    view.negative.push(entry);
                }
            }
        }
    }

    fn interface_view_for_term(
        &mut self,
        term: &InterfaceTerm,
        subst: &HashMap<String, Ty>,
        expanding: &mut HashSet<String>,
    ) -> InterfaceView {
        let name = name_ref_canonical(&self.resolved, &term.name);
        let args = term
            .args
            .iter()
            .map(|ty| self.lower_type_with_subst(ty, subst))
            .collect::<Vec<_>>();
        self.interface_view_inner(&name, &args, expanding)
    }

    fn result_ok_err_tys(&self, ty: &Ty) -> Option<(Ty, Ty)> {
        let Ty::Named { name, args } = ty else {
            return None;
        };
        if name != "Result" || args.len() != 2 {
            return None;
        }
        if !std_id::module_can_see_std_result(&self.resolved, self.current_module) {
            return None;
        }
        let template = self.enum_templates.get(name)?;
        if !template.variants.iter().any(|variant| variant.name == "Ok")
            || !template
                .variants
                .iter()
                .any(|variant| variant.name == "Err")
        {
            return None;
        }
        Some((args[0].clone(), args[1].clone()))
    }

    fn future_output_ty(&self, ty: &Ty) -> Option<Ty> {
        let Ty::Named { name, args } = ty else {
            return None;
        };
        if name == "Future" && args.len() == 1 {
            Some(args[0].clone())
        } else {
            None
        }
    }

    fn is_std_message_clone_interface_name(&self, name: &str) -> bool {
        name == STD_MESSAGE_CLONE_INTERFACE
            && self.interface_names.get(name).is_some_and(|def_id| {
                std_id::is_std_message_clone_interface(&self.resolved, *def_id)
            })
    }

    fn is_std_message_share_handle_marker_name(&self, name: &str) -> bool {
        name == STD_MESSAGE_SHARE_HANDLE_INTERFACE
            && self.interface_names.get(name).is_some_and(|def_id| {
                std_id::is_std_message_interface(
                    &self.resolved,
                    *def_id,
                    STD_MESSAGE_SHARE_HANDLE_INTERFACE,
                )
            })
    }

    fn is_std_message_thread_local_marker_name(&self, name: &str) -> bool {
        name == STD_MESSAGE_THREAD_LOCAL_INTERFACE
            && self.interface_names.get(name).is_some_and(|def_id| {
                std_id::is_std_message_interface(
                    &self.resolved,
                    *def_id,
                    STD_MESSAGE_THREAD_LOCAL_INTERFACE,
                )
            })
    }

    fn is_std_meta_ciel_fn_value_marker_name(&self, name: &str) -> bool {
        name == "ciel_fn_value_marker"
            && self.interface_names.get(name).is_some_and(|def_id| {
                std_id::is_std_meta_interface(&self.resolved, *def_id, "ciel_fn_value_marker")
            })
    }

    fn is_std_meta_closure_value_marker_name(&self, name: &str) -> bool {
        name == "closure_value_marker"
            && self.interface_names.get(name).is_some_and(|def_id| {
                std_id::is_std_meta_interface(&self.resolved, *def_id, "closure_value_marker")
            })
    }

    fn is_std_error_format_interface_name(&self, name: &str) -> bool {
        name == STD_ERROR_FORMAT_INTERFACE
            && self.interface_names.get(name).is_some_and(|def_id| {
                std_id::is_std_error_interface(&self.resolved, *def_id, STD_ERROR_FORMAT_INTERFACE)
            })
    }

    fn is_std_error_ty(&self, ty: &Ty) -> bool {
        ty == &std_error_ty()
            && std_id::module_can_see_std_error(&self.resolved, self.current_module)
    }

    fn type_implements_std_error_trait(&mut self, ty: &Ty) -> bool {
        self.is_std_error_format_interface_name(STD_ERROR_FORMAT_INTERFACE)
            && self.type_implements_capability(STD_ERROR_FORMAT_INTERFACE, &[], ty)
    }

    fn is_compiler_provided_meta_marker_def(&self, def_id: DefId) -> bool {
        std_id::is_std_meta_interface(&self.resolved, def_id, "ciel_fn_value_marker")
            || std_id::is_std_meta_interface(&self.resolved, def_id, "closure_value_marker")
    }

    fn is_std_message_capability_interface_name(&self, name: &str) -> bool {
        self.is_std_message_clone_interface_name(name)
            || self.is_std_message_share_handle_marker_name(name)
            || self.is_std_message_thread_local_marker_name(name)
    }

    fn apply_condition_narrowing(&mut self, scopes: &mut LocalScopes, cond: &TExpr, truth: bool) {
        for (local_id, ty) in nullable_narrowings_from_condition(cond, truth) {
            scopes.narrow_to(local_id, ty);
        }
    }
}

fn pattern_span(pattern: &Pattern) -> crate::span::Span {
    match pattern {
        Pattern::Variant(name, _) => name.span,
        Pattern::Wildcard(span) => *span,
    }
}

fn nullable_narrowings_from_condition(cond: &TExpr, truth: bool) -> Vec<(LocalId, Ty)> {
    match &cond.kind {
        TExprKind::Binary { op, left, right } if matches!(op, BinaryOp::Eq | BinaryOp::Ne) => {
            let should_narrow =
                (matches!(op, BinaryOp::Ne) && truth) || (matches!(op, BinaryOp::Eq) && !truth);
            if !should_narrow {
                return Vec::new();
            }
            nullable_comparison_local(left, right)
                .or_else(|| nullable_comparison_local(right, left))
                .into_iter()
                .collect()
        }
        TExprKind::Binary {
            op: BinaryOp::And,
            left,
            right,
        } if truth => {
            let mut narrowings = nullable_narrowings_from_condition(left, true);
            narrowings.extend(nullable_narrowings_from_condition(right, true));
            narrowings
        }
        _ => Vec::new(),
    }
}

fn nullable_comparison_local(candidate: &TExpr, other: &TExpr) -> Option<(LocalId, Ty)> {
    if !matches!(other.kind, TExprKind::Literal(Literal::Null)) {
        return None;
    }
    let TExprKind::Local(local_id, _) = candidate.kind else {
        return None;
    };
    let Ty::Pointer {
        nullable: true,
        mutability,
        inner,
    } = &candidate.ty
    else {
        return None;
    };
    Some((
        local_id,
        Ty::Pointer {
            nullable: false,
            mutability: *mutability,
            inner: inner.clone(),
        },
    ))
}

fn bool_literal_is(expr: &TExpr, expected: bool) -> bool {
    matches!(expr.kind, TExprKind::Literal(Literal::Bool(value)) if value == expected)
}

fn expr_is_closure_literal(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Closure { .. } => true,
        ExprKind::Cast { expr, .. } => expr_is_closure_literal(expr),
        _ => false,
    }
}

fn lvalue_root_local(expr: &TExpr) -> Option<(LocalId, &str)> {
    match &expr.kind {
        TExprKind::Local(local_id, name) => Some((*local_id, name.as_str())),
        TExprKind::Field { base, .. } | TExprKind::Index { base, .. } => lvalue_root_local(base),
        _ => None,
    }
}

fn enum_instance_name(name: &str, args: &[Ty]) -> String {
    aggregate_instance_name(name, args)
}

fn unify_receiver_param(pattern: &Ty, actual: &Ty, subst: &mut HashMap<String, Ty>) -> bool {
    match pattern {
        Ty::Pointer { inner, .. } => match actual {
            Ty::Pointer {
                inner: actual_inner,
                ..
            } => unify_ty(inner, actual_inner, subst),
            _ => unify_ty(inner, actual, subst),
        },
        _ => unify_ty(pattern, actual, subst),
    }
}

fn hir_type_contains_hole(ty: &Type) -> bool {
    match &ty.kind {
        TypeKind::Hole => true,
        TypeKind::Pointer { inner, .. } | TypeKind::Slice { elem: inner, .. } => {
            hir_type_contains_hole(inner)
        }
        TypeKind::Array { elem, .. } => hir_type_contains_hole(elem),
        TypeKind::Named(_, args) => args.iter().any(hir_type_contains_hole),
        TypeKind::Function { ret, params, .. } | TypeKind::Closure { ret, params, .. } => {
            hir_type_contains_hole(ret) || params.iter().any(hir_type_contains_hole)
        }
        _ => false,
    }
}

fn hir_type_contains_generic(ty: &Type) -> bool {
    match &ty.kind {
        TypeKind::Named(name, args) => {
            matches!(name.kind, TypeNameKind::Generic(_))
                || args.iter().any(hir_type_contains_generic)
        }
        TypeKind::Pointer { inner, .. } | TypeKind::Slice { elem: inner, .. } => {
            hir_type_contains_generic(inner)
        }
        TypeKind::Array { elem, .. } => hir_type_contains_generic(elem),
        TypeKind::Function { ret, params, .. } | TypeKind::Closure { ret, params, .. } => {
            hir_type_contains_generic(ret) || params.iter().any(hir_type_contains_generic)
        }
        _ => false,
    }
}

fn type_contains_plain_never_value(ty: &Ty) -> bool {
    match ty {
        Ty::Never => true,
        Ty::Array { elem, .. } | Ty::Slice { elem, .. } => type_contains_plain_never_value(elem),
        Ty::Function { params, .. }
        | Ty::Closure { params, .. }
        | Ty::ClosureInstance { params, .. } => params.iter().any(type_contains_plain_never_value),
        Ty::Pointer { .. }
        | Ty::Named { .. }
        | Ty::DynamicInterface { .. }
        | Ty::Hole(_)
        | Ty::Generic(_)
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
        | Ty::Unknown => false,
    }
}

fn type_contains_closure(ty: &Ty) -> bool {
    match ty {
        Ty::Closure { .. } | Ty::ClosureInstance { .. } => true,
        Ty::Pointer { inner, .. } => type_contains_closure(inner),
        Ty::Array { elem, .. } | Ty::Slice { elem, .. } => type_contains_closure(elem),
        Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
            args.iter().any(type_contains_closure)
        }
        Ty::Function { ret, params, .. } => {
            type_contains_closure(ret) || params.iter().any(type_contains_closure)
        }
        Ty::Never
        | Ty::Hole(_)
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
        | Ty::Generic(_)
        | Ty::Unknown => false,
    }
}

fn parse_integer_literal_u128(raw: &str) -> Option<u128> {
    let normalized = raw.replace('_', "");
    if let Some(hex) = normalized
        .strip_prefix("0x")
        .or_else(|| normalized.strip_prefix("0X"))
    {
        u128::from_str_radix(hex, 16).ok()
    } else {
        normalized.parse::<u128>().ok()
    }
}

fn integer_abs_limits(ty: &Ty) -> Option<(u128, u128)> {
    Some(match ty {
        Ty::I8 => (128, 127),
        Ty::I16 => (32768, 32767),
        Ty::I32 => (2147483648, 2147483647),
        Ty::I64 => (9223372036854775808, 9223372036854775807),
        Ty::Char => (0, u8::MAX as u128),
        Ty::U8 => (0, u8::MAX as u128),
        Ty::U16 => (0, u16::MAX as u128),
        Ty::U32 => (0, u32::MAX as u128),
        Ty::U64 => (0, u64::MAX as u128),
        Ty::Usize => (0, usize::MAX as u128),
        _ => return None,
    })
}

fn decode_char_literal_byte(raw: &str) -> Option<u8> {
    let inner = raw.strip_prefix('\'')?.strip_suffix('\'')?;
    if let Some(escaped) = inner.strip_prefix('\\') {
        return match escaped {
            "'" => Some(b'\''),
            "\"" => Some(b'"'),
            "\\" => Some(b'\\'),
            "0" => Some(0),
            "n" => Some(b'\n'),
            "r" => Some(b'\r'),
            "t" => Some(b'\t'),
            hex if hex.starts_with('x') && hex.len() == 3 => u8::from_str_radix(&hex[1..], 16).ok(),
            _ => None,
        };
    }
    let mut chars = inner.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    (ch as u32 <= u8::MAX as u32).then_some(ch as u8)
}

fn interface_non_receiver_args(args: &[Ty]) -> &[Ty] {
    if args.is_empty() { args } else { &args[1..] }
}

fn interface_generic_placeholder(interface_name: &str, generic_name: &str) -> String {
    format!("__ciel_iface_{}_{}", interface_name, generic_name)
}

fn impl_function_name(interface_name: &str, params: &[Ty]) -> String {
    format!(
        "__impl_{}_{}",
        interface_name,
        params
            .iter()
            .map(mangle_ty_fragment)
            .collect::<Vec<_>>()
            .join("_")
    )
}

fn find_impl_in<'a>(
    impls: &'a [ImplSig],
    interface_name: &str,
    interface_args: &[Ty],
    receiver_ty: Option<&Ty>,
) -> Option<&'a ImplSig> {
    impls.iter().find(|implementation| {
        implementation.interface_name == interface_name
            && implementation.interface_args == interface_args
            && match (implementation.receiver_ty.as_ref(), receiver_ty) {
                (Some(left), Some(right)) => left == right,
                (None, None) => true,
                (Some(_), None) => true,
                _ => false,
            }
    })
}

fn marker_impl_patterns_overlap(
    left_args: &[Ty],
    left_receiver: Option<&Ty>,
    right_args: &[Ty],
    right_receiver: Option<&Ty>,
) -> bool {
    if left_args.len() != right_args.len() {
        return false;
    }
    let receiver_overlaps = match (left_receiver, right_receiver) {
        (Some(left), Some(right)) => marker_ty_patterns_overlap(left, right),
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => true,
    };
    receiver_overlaps
        && left_args
            .iter()
            .zip(right_args.iter())
            .all(|(left, right)| marker_ty_patterns_overlap(left, right))
}

fn marker_impl_domains_disjoint(
    left_domain: Option<CompilerMarkerDomain>,
    left_receiver: Option<&Ty>,
    right_domain: Option<CompilerMarkerDomain>,
    right_receiver: Option<&Ty>,
) -> bool {
    match (left_domain, right_domain) {
        (Some(left), Some(right)) if left != right => return true,
        _ => {}
    }

    if let (Some(domain), Some(receiver)) = (left_domain, right_receiver)
        && !ty_can_satisfy_compiler_marker_domain(receiver, domain)
    {
        return true;
    }
    if let (Some(domain), Some(receiver)) = (right_domain, left_receiver)
        && !ty_can_satisfy_compiler_marker_domain(receiver, domain)
    {
        return true;
    }
    false
}

fn ty_can_satisfy_compiler_marker_domain(ty: &Ty, domain: CompilerMarkerDomain) -> bool {
    match (ty, domain) {
        (Ty::Generic(_), _) => true,
        (
            Ty::Function {
                is_unsafe: false,
                abi: None,
                ..
            },
            CompilerMarkerDomain::CielFnValue,
        ) => true,
        (Ty::ClosureInstance { .. }, CompilerMarkerDomain::ClosureValue) => true,
        _ => false,
    }
}

fn marker_ty_patterns_overlap(left: &Ty, right: &Ty) -> bool {
    match (left, right) {
        (Ty::Generic(_), _) | (_, Ty::Generic(_)) => true,
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
                && marker_ty_patterns_overlap(left_inner, right_inner)
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
        ) => left_len == right_len && marker_ty_patterns_overlap(left_elem, right_elem),
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
            left_mutability == right_mutability && marker_ty_patterns_overlap(left_elem, right_elem)
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
            },
            Ty::DynamicInterface {
                name: right_name,
                args: right_args,
            },
        ) => {
            left_name == right_name
                && left_args.len() == right_args.len()
                && left_args
                    .iter()
                    .zip(right_args.iter())
                    .all(|(left, right)| marker_ty_patterns_overlap(left, right))
        }
        (
            Ty::Function {
                is_unsafe: left_is_unsafe,
                abi: left_abi,
                ret: left_ret,
                params: left_params,
            },
            Ty::Function {
                is_unsafe: right_is_unsafe,
                abi: right_abi,
                ret: right_ret,
                params: right_params,
            },
        ) => {
            left_is_unsafe == right_is_unsafe
                && left_abi == right_abi
                && left_params.len() == right_params.len()
                && marker_ty_patterns_overlap(left_ret, right_ret)
                && left_params
                    .iter()
                    .zip(right_params.iter())
                    .all(|(left, right)| marker_ty_patterns_overlap(left, right))
        }
        (
            Ty::Closure {
                ret: left_ret,
                params: left_params,
                ..
            },
            Ty::Closure {
                ret: right_ret,
                params: right_params,
                ..
            },
        ) => {
            left_params.len() == right_params.len()
                && marker_ty_patterns_overlap(left_ret, right_ret)
                && left_params
                    .iter()
                    .zip(right_params.iter())
                    .all(|(left, right)| marker_ty_patterns_overlap(left, right))
        }
        (
            Ty::ClosureInstance {
                id: left_id,
                ret: left_ret,
                params: left_params,
                captures: left_captures,
            },
            Ty::ClosureInstance {
                id: right_id,
                ret: right_ret,
                params: right_params,
                captures: right_captures,
            },
        ) => {
            left_id == right_id
                && left_params.len() == right_params.len()
                && left_captures.len() == right_captures.len()
                && marker_ty_patterns_overlap(left_ret, right_ret)
                && left_params
                    .iter()
                    .zip(right_params.iter())
                    .all(|(left, right)| marker_ty_patterns_overlap(left, right))
                && left_captures
                    .iter()
                    .zip(right_captures.iter())
                    .all(|(left, right)| marker_ty_patterns_overlap(left, right))
        }
        (left, right) => left == right,
    }
}

fn name_ref_canonical(resolved: &ResolvedProgram, name: &NameRef) -> String {
    match name.kind {
        NameRefKind::Def(def_id) => resolved.def(def_id).name.clone(),
        _ => name.display.clone(),
    }
}

fn interface_receiver_is_input(interface: &InterfaceSig) -> bool {
    let Some(receiver) = interface.generics.first() else {
        return false;
    };
    interface
        .params
        .iter()
        .any(|param| ast_type_mentions_name(&param.ty, receiver))
}

fn ast_type_mentions_name(ty: &Type, name: &str) -> bool {
    match &ty.kind {
        TypeKind::Named(type_name, args) => {
            matches!(&type_name.kind, TypeNameKind::Generic(generic) if generic == name)
                || args.iter().any(|arg| ast_type_mentions_name(arg, name))
        }
        TypeKind::Pointer { inner, .. } => ast_type_mentions_name(inner, name),
        TypeKind::Array { elem, .. } | TypeKind::Slice { elem, .. } => {
            ast_type_mentions_name(elem, name)
        }
        TypeKind::Function { ret, params, .. } => {
            ast_type_mentions_name(ret, name)
                || params
                    .iter()
                    .any(|param| ast_type_mentions_name(param, name))
        }
        TypeKind::Closure {
            ret,
            params,
            constraint,
        } => {
            ast_type_mentions_name(ret, name)
                || params
                    .iter()
                    .any(|param| ast_type_mentions_name(param, name))
                || constraint
                    .as_ref()
                    .is_some_and(|constraint| constraint_expr_mentions_name(constraint, name))
        }
        TypeKind::Hole | TypeKind::Never | TypeKind::Void | TypeKind::Primitive(_) => false,
    }
}

fn constraint_expr_mentions_name(expr: &ConstraintExpr, name: &str) -> bool {
    expr.terms.iter().any(|term| {
        term.args
            .iter()
            .any(|arg| ast_type_mentions_name(arg, name))
    })
}
