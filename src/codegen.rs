use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{
    ast::{BinaryOp, Literal, UnaryOp},
    diagnostic::{DiagResult, Diagnostic},
    escape::EscapeProgram,
    hir::LocalId,
    interfaces::{
        checked_interface_view, dynamic_interface_signature, impl_matches_dynamic_interface,
        impl_matches_interface_receiver, retained_closure_interface_signature,
    },
    mono::MonoProgram,
    retained::{
        retained_closure_can_forward_source_witness,
        retained_closure_can_reuse_source_witness_field, retained_closure_needs_wrapper,
        retained_closure_required_witnesses,
    },
    resolve::DefId,
    source::SourceMap,
    thir::{
        CheckedFunction, CheckedImpl, CheckedInterfaceRef, CheckedVariant, TBlock, TClosureBody,
        TClosureCapture, TExpr, TExprKind, TForInit, TPattern, TStmt, TStmtKind,
    },
    types::{
        ConstraintBounds, ConstraintRef, Ty, is_clone_message_capability, mangle_constraint_ref,
        mangle_ty_fragment, meta_named, meta_sum_ty, retained_closure_capabilities, std_error_ty,
        std_result_ty,
    },
};

const C_RUNTIME_PRELUDE: &str = include_str!("runtime_prelude.c");

pub fn generate_c(
    program: &MonoProgram,
    escapes: &EscapeProgram,
    source_map: &SourceMap,
) -> DiagResult<String> {
    let mut generator = CGenerator::new(program, escapes, source_map);
    generator.prepare_plan_data();
    generator.emit()
}

struct CGenerator<'a> {
    program: &'a MonoProgram,
    escapes: &'a EscapeProgram,
    source_map: &'a SourceMap,
    out: String,
    plan: CodegenPlanData,
    current_heap_locals: HashSet<LocalId>,
    current_param_locals: HashMap<LocalId, String>,
    current_capture_locals: HashMap<LocalId, String>,
    current_closure_owner: Option<DefId>,
    defer_stack: Vec<Vec<String>>,
    loop_defer_starts: Vec<usize>,
    continue_targets: Vec<Option<String>>,
    current_return_ty: Ty,
    temp_counter: usize,
}

#[derive(Clone, Debug, Default)]
struct CodegenPlanData {
    slice_types: BTreeMap<String, Ty>,
    dynamic_types: BTreeMap<String, Ty>,
    dynamic_impls: BTreeMap<String, DynamicImplUse>,
    closure_types: BTreeMap<String, Ty>,
    closure_defs: BTreeMap<(usize, usize), ClosureDef>,
    function_closure_wrappers: BTreeMap<String, FunctionClosureWrapper>,
    retained_closure_wrappers: BTreeMap<String, RetainedClosureWrapper>,
    retained_closure_witnesses: BTreeMap<String, RetainedClosureWitness>,
    actor_dispatches: BTreeMap<String, ActorDispatch>,
    string_literals: BTreeMap<(usize, usize, usize), String>,
    string_literal_names: HashMap<(usize, usize, usize), String>,
    source_locations: BTreeMap<(usize, usize), SourceLocation>,
    name_map: HashMap<DefId, String>,
}

#[derive(Clone, Debug)]
struct DynamicImplUse {
    dyn_ty: Ty,
    concrete_ty: Ty,
}

#[derive(Clone, Debug)]
struct ClosureDef {
    id: usize,
    owner: DefId,
    ty: Ty,
    params: Vec<(LocalId, String, Ty)>,
    captures: Vec<TClosureCapture>,
    body: TClosureBody,
}

#[derive(Clone, Debug)]
struct FunctionClosureWrapper {
    closure_ty: Ty,
    function_ty: Ty,
}

#[derive(Clone, Debug)]
struct RetainedClosureWrapper {
    target_ty: Ty,
    source_ty: Ty,
}

#[derive(Clone, Debug)]
struct RetainedClosureWitness {
    target_ty: Ty,
    source_ty: Ty,
    capability: ConstraintRef,
    span: crate::span::Span,
}

#[derive(Clone, Debug)]
struct ActorDispatch {
    name: String,
    state_ty: Ty,
    message_ty: Ty,
    handler_ty: Ty,
}

#[derive(Clone, Debug)]
struct SourceLocation {
    name: String,
    file: String,
    line: usize,
}

#[derive(Clone, Debug)]
struct MetaProductField {
    name: String,
    ty: Ty,
    value_expr: String,
}

#[derive(Clone, Debug)]
struct MetaCaptureField {
    index: usize,
    ty: Ty,
}

#[derive(Clone, Debug)]
struct MetaPayloadField {
    index: usize,
    ty: Ty,
    value_expr: String,
}

#[derive(Clone, Debug)]
struct ResultLayout {
    c_type: String,
    ok_index: usize,
    ok_name: String,
    ok_has_payload: bool,
    err_name: String,
    err_index: usize,
    err_has_payload: bool,
}

#[derive(Clone, Copy, Debug)]
enum AggregateLayoutRef {
    Struct(usize),
    Enum(usize),
}

impl<'a> CGenerator<'a> {
    fn new(
        program: &'a MonoProgram,
        escapes: &'a EscapeProgram,
        source_map: &'a SourceMap,
    ) -> Self {
        Self {
            program,
            escapes,
            source_map,
            out: String::new(),
            plan: CodegenPlanData::default(),
            current_heap_locals: HashSet::new(),
            current_param_locals: HashMap::new(),
            current_capture_locals: HashMap::new(),
            current_closure_owner: None,
            defer_stack: Vec::new(),
            loop_defer_starts: Vec::new(),
            continue_targets: Vec::new(),
            current_return_ty: Ty::Void,
            temp_counter: 0,
        }
    }

    fn emit(mut self) -> DiagResult<String> {
        let _escape_summary_count = self.escapes.functions.len();

        self.line("/* generated by cielc */");
        self.emit_runtime();
        self.line("");
        self.emit_c_includes();
        self.line("");
        self.emit_source_location_table();

        for (key, raw) in self.plan.string_literals.clone() {
            let name = self
                .plan
                .string_literal_names
                .get(&key)
                .cloned()
                .unwrap_or_else(|| self.next_temp("str"));
            self.line(&format!("static char {name}[] = {raw};"));
        }
        if !self.plan.string_literals.is_empty() {
            self.line("");
        }

        for (name, ty) in self.plan.dynamic_types.clone() {
            let vtable = self.dynamic_vtable_name(&ty);
            self.line(&format!("typedef struct {vtable} {vtable};"));
            self.line(&format!(
                "typedef struct {{ void *data; const {vtable} *vtable; }} {name};"
            ));
        }
        if !self.plan.dynamic_types.is_empty() {
            self.line("");
        }

        for (slice, elem) in self.plan.slice_types.clone() {
            let c_ty = self.c_type(&elem);
            self.line(&format!(
                "typedef struct {{ {} *ptr; size_t len; }} {};",
                c_ty, slice
            ));
        }
        if !self.plan.slice_types.is_empty() {
            self.line("");
        }

        for name in self.plan.closure_types.keys().cloned().collect::<Vec<_>>() {
            self.line(&format!("typedef struct {name} {name};"));
        }
        for closure in self.plan.closure_defs.values().cloned().collect::<Vec<_>>() {
            if !closure.captures.is_empty() {
                self.line(&format!(
                    "typedef struct {} {};",
                    self.closure_env_name(closure.owner, closure.id),
                    self.closure_env_name(closure.owner, closure.id)
                ));
            }
        }
        for wrapper in self
            .plan
            .function_closure_wrappers
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            self.line(&format!(
                "typedef struct {} {};",
                self.function_closure_env_name(&wrapper.closure_ty, &wrapper.function_ty),
                self.function_closure_env_name(&wrapper.closure_ty, &wrapper.function_ty)
            ));
        }
        for wrapper in self
            .plan
            .retained_closure_wrappers
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            self.line(&format!(
                "typedef struct {} {};",
                self.retained_closure_env_name(&wrapper.target_ty, &wrapper.source_ty),
                self.retained_closure_env_name(&wrapper.target_ty, &wrapper.source_ty)
            ));
        }
        if !self.plan.closure_types.is_empty()
            || !self.plan.closure_defs.is_empty()
            || !self.plan.function_closure_wrappers.is_empty()
            || !self.plan.retained_closure_wrappers.is_empty()
        {
            self.line("");
        }

        for opaque in &self.program.checked.opaque_structs {
            self.line(&format!("typedef struct {} {};", opaque.name, opaque.name));
        }
        for strukt in &self.program.checked.structs {
            self.line(&format!("typedef struct {} {};", strukt.name, strukt.name));
        }
        for enm in &self.program.checked.enums {
            self.line(&format!("typedef struct {} {};", enm.name, enm.name));
        }
        if !self.program.checked.opaque_structs.is_empty()
            || !self.program.checked.structs.is_empty()
            || !self.program.checked.enums.is_empty()
        {
            self.line("");
        }

        self.emit_closure_value_layouts();

        for aggregate in self.aggregate_layout_order() {
            match aggregate {
                AggregateLayoutRef::Struct(idx) => self.emit_struct_layout(idx),
                AggregateLayoutRef::Enum(idx) => self.emit_enum_layout(idx),
            }
        }

        self.emit_closure_environment_layouts();

        self.emit_dynamic_vtable_layouts();

        let mut emitted_prototypes = HashSet::new();
        for function in &self.program.checked.functions {
            let prototype = format!("{};", self.function_decl(function, true));
            if emitted_prototypes.insert(prototype.clone()) {
                self.line(&prototype);
            }
        }
        self.emit_closure_prototypes();
        self.emit_retained_closure_witness_prototypes();
        self.emit_actor_dispatch_prototypes();
        self.emit_dynamic_shim_prototypes();
        self.line("");

        self.emit_dynamic_shims_and_tables();
        if !self.plan.dynamic_impls.is_empty() {
            self.line("");
        }

        self.emit_closure_thunks_and_wrappers()?;
        if !self.plan.closure_defs.is_empty()
            || !self.plan.function_closure_wrappers.is_empty()
            || !self.plan.retained_closure_wrappers.is_empty()
        {
            self.line("");
        }

        self.emit_retained_closure_witnesses()?;
        if !self.plan.retained_closure_witnesses.is_empty() {
            self.line("");
        }

        self.emit_actor_dispatches()?;
        if !self.plan.actor_dispatches.is_empty() {
            self.line("");
        }

        let functions = self.program.checked.functions.clone();
        for function in &functions {
            if function.body.is_some() {
                self.gen_function(function)?;
                self.line("");
            }
        }

        if let Some(main_id) = self.find_ciel_main().map(|main| main.def_id) {
            self.line("int main(void) {");
            self.line("    ciel_runtime_init();");
            self.line(&format!("    return (int){}();", self.c_name(main_id)));
            self.line("}");
        }

        Ok(std::mem::take(&mut self.out))
    }

    fn prepare_plan_data(&mut self) {
        self.collect_names();
        self.collect_slice_types();
        self.collect_dynamic_interfaces();
        self.collect_closures();
        self.collect_string_literals();
        self.collect_source_locations();
    }

    fn aggregate_layout_order(&self) -> Vec<AggregateLayoutRef> {
        let mut by_name = HashMap::new();
        for (idx, strukt) in self.program.checked.structs.iter().enumerate() {
            by_name.insert(strukt.name.clone(), AggregateLayoutRef::Struct(idx));
        }
        for (idx, enm) in self.program.checked.enums.iter().enumerate() {
            by_name.insert(enm.name.clone(), AggregateLayoutRef::Enum(idx));
        }

        let aggregate_names = by_name.keys().cloned().collect::<HashSet<_>>();
        let mut visited = HashSet::new();
        let mut visiting = HashSet::new();
        let mut out = Vec::new();
        for strukt in &self.program.checked.structs {
            self.visit_aggregate_layout(
                &strukt.name,
                &by_name,
                &aggregate_names,
                &mut visiting,
                &mut visited,
                &mut out,
            );
        }
        for enm in &self.program.checked.enums {
            self.visit_aggregate_layout(
                &enm.name,
                &by_name,
                &aggregate_names,
                &mut visiting,
                &mut visited,
                &mut out,
            );
        }
        out
    }

    fn visit_aggregate_layout(
        &self,
        name: &str,
        by_name: &HashMap<String, AggregateLayoutRef>,
        aggregate_names: &HashSet<String>,
        visiting: &mut HashSet<String>,
        visited: &mut HashSet<String>,
        out: &mut Vec<AggregateLayoutRef>,
    ) {
        if visited.contains(name) || !visiting.insert(name.to_string()) {
            return;
        }
        let Some(item) = by_name.get(name).copied() else {
            visiting.remove(name);
            return;
        };
        for dep in self.aggregate_layout_deps(item, aggregate_names) {
            self.visit_aggregate_layout(&dep, by_name, aggregate_names, visiting, visited, out);
        }
        visiting.remove(name);
        visited.insert(name.to_string());
        out.push(item);
    }

    fn aggregate_layout_deps(
        &self,
        item: AggregateLayoutRef,
        aggregate_names: &HashSet<String>,
    ) -> Vec<String> {
        let mut deps = Vec::new();
        match item {
            AggregateLayoutRef::Struct(idx) => {
                for (_, ty) in &self.program.checked.structs[idx].fields {
                    self.collect_aggregate_value_deps(ty, aggregate_names, &mut deps);
                }
            }
            AggregateLayoutRef::Enum(idx) => {
                for variant in &self.program.checked.enums[idx].variants {
                    for ty in &variant.payload {
                        self.collect_aggregate_value_deps(ty, aggregate_names, &mut deps);
                    }
                }
            }
        }
        deps.sort();
        deps.dedup();
        deps
    }

    fn collect_aggregate_value_deps(
        &self,
        ty: &Ty,
        aggregate_names: &HashSet<String>,
        out: &mut Vec<String>,
    ) {
        match ty.unqualified() {
            Ty::Const(inner) => self.collect_aggregate_value_deps(inner, aggregate_names, out),
            Ty::Array { elem, .. } => self.collect_aggregate_value_deps(elem, aggregate_names, out),
            Ty::Named { name, args } => {
                let c_name = self.c_named_type(name, args);
                if aggregate_names.contains(&c_name) {
                    out.push(c_name);
                }
            }
            _ => {}
        }
    }

    fn emit_struct_layout(&mut self, idx: usize) {
        let strukt = self.program.checked.structs[idx].clone();
        self.line(&format!("struct {} {{", strukt.name));
        let mut emitted_field = false;
        for (field, ty) in &strukt.fields {
            if ty.is_erased_value() {
                continue;
            }
            emitted_field = true;
            self.line(&format!("    {};", self.c_decl(ty, field)));
        }
        if !emitted_field {
            self.line("    char _ciel_empty;");
        }
        self.line("};");
        self.line("");
    }

    fn emit_enum_layout(&mut self, idx: usize) {
        let enm = self.program.checked.enums[idx].clone();
        self.line(&format!("struct {} {{", enm.name));
        self.line("    int tag;");
        if enm
            .variants
            .iter()
            .any(|variant| !variant.payload.is_empty())
        {
            self.line("    union {");
            for variant in &enm.variants {
                if variant.payload.is_empty() {
                    continue;
                }
                self.line("        struct {");
                for (idx, ty) in variant.payload.iter().enumerate() {
                    self.line(&format!(
                        "            {};",
                        self.c_decl(ty, &format!("_{idx}"))
                    ));
                }
                self.line(&format!("        }} {};", variant.name));
            }
            self.line("    } as;");
        }
        self.line("};");
        self.line("");
    }

    fn emit_c_includes(&mut self) {
        let mut includes = Vec::new();
        for module in &self.program.checked.hir_modules {
            for item in &module.items {
                if let crate::hir::ItemKind::CInclude(include) = &item.kind {
                    includes.push(include.clone());
                }
            }
        }
        includes.sort();
        includes.dedup();
        for include in includes {
            self.line(&format!("#include \"{}\"", escape_c_include(&include)));
        }
    }

    fn emit_runtime(&mut self) {
        self.out.push_str(C_RUNTIME_PRELUDE);
        if !C_RUNTIME_PRELUDE.ends_with('\n') {
            self.out.push('\n');
        }
    }

    fn emit_source_location_table(&mut self) {
        if self.plan.source_locations.is_empty() {
            return;
        }
        self.line(
            "typedef struct CielSourceLocation { char *file; size_t line; } CielSourceLocation;",
        );
        let locations = self.plan.source_locations.values().cloned().collect::<Vec<_>>();
        for location in locations {
            self.line(&format!(
                "static CIEL_MAYBE_UNUSED const CielSourceLocation {} = {{ \"{}\", {} }};",
                location.name,
                escape_c_string(&location.file),
                location.line
            ));
        }
        self.line("");
    }

    fn collect_names(&mut self) {
        for function in &self.program.checked.functions {
            let c_name = if function.abi.as_deref() == Some("C")
                && (function.exported || function.body.is_none())
            {
                function.name.clone()
            } else {
                format!("ciel_{}_{}", function.def_id.0, function.name)
            };
            self.plan.name_map.insert(function.def_id, c_name);
        }
    }

    fn collect_slice_types(&mut self) {
        self.collect_program_types_and_bodies(
            |this, ty| this.collect_ty_slice(ty),
            |this, body, _| this.collect_block_slices(body),
        );
    }

    fn collect_ty_slice(&mut self, ty: &Ty) {
        match ty {
            Ty::Const(inner) => self.collect_ty_slice(inner),
            Ty::Slice(elem) => {
                self.plan.slice_types
                    .insert(self.slice_name(elem), (**elem).clone());
                self.collect_ty_slice(elem);
            }
            Ty::Pointer { inner, .. } | Ty::Array { elem: inner, .. } => {
                self.collect_ty_slice(inner)
            }
            Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
                for arg in args {
                    self.collect_ty_slice(arg);
                }
            }
            Ty::Function { ret, params, .. } => {
                self.collect_ty_slice(ret);
                for param in params {
                    self.collect_ty_slice(param);
                }
            }
            Ty::Closure { ret, params, .. } | Ty::ClosureInstance { ret, params, .. } => {
                self.collect_ty_slice(ret);
                for param in params {
                    self.collect_ty_slice(param);
                }
            }
            _ => {}
        }
    }

    fn collect_dynamic_interfaces(&mut self) {
        self.collect_program_types_and_bodies(
            |this, ty| this.collect_ty_dynamic(ty),
            |this, body, _| this.collect_block_dynamic(body),
        );
    }

    fn collect_closures(&mut self) {
        self.collect_program_types_and_bodies(
            |this, ty| this.collect_ty_closure(ty),
            |this, body, owner| this.collect_block_closures(owner, body),
        );
    }

    fn collect_program_types_and_bodies(
        &mut self,
        mut collect_ty: impl FnMut(&mut Self, &Ty),
        mut collect_body: impl FnMut(&mut Self, &TBlock, DefId),
    ) {
        for strukt in &self.program.checked.structs {
            for (_, ty) in &strukt.fields {
                collect_ty(self, ty);
            }
        }
        for enm in &self.program.checked.enums {
            for variant in &enm.variants {
                for ty in &variant.payload {
                    collect_ty(self, ty);
                }
            }
        }
        let functions = self.program.checked.functions.clone();
        for function in &functions {
            collect_ty(self, &function.ret);
            for (_, _, ty) in &function.params {
                collect_ty(self, ty);
            }
            if let Some(body) = &function.body {
                collect_body(self, body, function.def_id);
            }
        }
    }

    fn collect_ty_closure(&mut self, ty: &Ty) {
        match ty {
            Ty::Const(inner) => self.collect_ty_closure(inner),
            Ty::Closure {
                ret,
                params,
                constraints,
            } => {
                self.plan.closure_types
                    .insert(self.closure_type_name(ty), ty.clone());
                self.collect_ty_closure(ret);
                for param in params {
                    self.collect_ty_closure(param);
                }
                self.collect_constraint_bounds_closures(constraints);
            }
            Ty::ClosureInstance { ret, params, .. } => {
                self.plan.closure_types
                    .insert(self.closure_type_name(ty), ty.clone());
                self.collect_ty_closure(ret);
                for param in params {
                    self.collect_ty_closure(param);
                }
            }
            Ty::Pointer { inner, .. } | Ty::Array { elem: inner, .. } | Ty::Slice(inner) => {
                self.collect_ty_closure(inner)
            }
            Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
                for arg in args {
                    self.collect_ty_closure(arg);
                }
            }
            Ty::Function { ret, params, .. } => {
                self.collect_ty_closure(ret);
                for param in params {
                    self.collect_ty_closure(param);
                }
            }
            _ => {}
        }
    }

    fn collect_constraint_bounds_closures(&mut self, bounds: &ConstraintBounds) {
        for entry in bounds.positive.iter().chain(bounds.negative.iter()) {
            for arg in &entry.args {
                self.collect_ty_closure(arg);
            }
        }
    }

    fn collect_retained_closure_witnesses(
        &mut self,
        target_ty: &Ty,
        source_ty: &Ty,
        span: crate::span::Span,
    ) {
        for capability in retained_closure_required_witnesses(target_ty, source_ty) {
            let key = self.retained_closure_witness_name(target_ty, source_ty, &capability);
            self.plan.retained_closure_witnesses
                .entry(key)
                .or_insert_with(|| RetainedClosureWitness {
                    target_ty: target_ty.clone(),
                    source_ty: source_ty.clone(),
                    capability,
                    span,
                });
        }
    }

    fn collect_retained_closure_wrapper(&mut self, target_ty: &Ty, source_ty: &Ty) {
        if !retained_closure_needs_wrapper(target_ty, source_ty) {
            return;
        }
        let key = self.retained_closure_wrapper_key(target_ty, source_ty);
        self.plan.retained_closure_wrappers
            .entry(key)
            .or_insert_with(|| RetainedClosureWrapper {
                target_ty: target_ty.clone(),
                source_ty: source_ty.clone(),
            });
    }

    fn collect_block_closures(&mut self, owner: DefId, block: &TBlock) {
        for stmt in &block.statements {
            self.collect_stmt_closures(owner, stmt);
        }
    }

    fn collect_stmt_closures(&mut self, owner: DefId, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::Block(block) => self.collect_block_closures(owner, block),
            TStmtKind::VarDecl { ty, init, .. } => {
                self.collect_ty_closure(ty);
                if let Some(init) = init {
                    self.collect_expr_closures(owner, init);
                }
            }
            TStmtKind::Assign { target, value } => {
                self.collect_expr_closures(owner, target);
                self.collect_expr_closures(owner, value);
            }
            TStmtKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                self.collect_expr_closures(owner, cond);
                self.collect_block_closures(owner, then_block);
                if let Some(else_branch) = else_branch {
                    self.collect_stmt_closures(owner, else_branch);
                }
            }
            TStmtKind::While { cond, body } => {
                self.collect_expr_closures(owner, cond);
                self.collect_block_closures(owner, body);
            }
            TStmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(init) = init {
                    self.collect_for_clause_closures(owner, init);
                }
                if let Some(cond) = cond {
                    self.collect_expr_closures(owner, cond);
                }
                if let Some(step) = step {
                    self.collect_for_clause_closures(owner, step);
                }
                self.collect_block_closures(owner, body);
            }
            TStmtKind::Switch {
                expr,
                cases,
                default,
                ..
            } => {
                self.collect_expr_closures(owner, expr);
                for case in cases {
                    for stmt in &case.statements {
                        self.collect_stmt_closures(owner, stmt);
                    }
                }
                for stmt in default {
                    self.collect_stmt_closures(owner, stmt);
                }
            }
            TStmtKind::Defer(expr) | TStmtKind::Return(Some(expr)) | TStmtKind::Expr(expr) => {
                self.collect_expr_closures(owner, expr);
            }
            TStmtKind::Return(None)
            | TStmtKind::Break
            | TStmtKind::Continue
            | TStmtKind::Unsupported => {}
        }
    }

    fn collect_for_clause_closures(&mut self, owner: DefId, clause: &TForInit) {
        match clause {
            TForInit::VarDecl { ty, init, .. } => {
                self.collect_ty_closure(ty);
                if let Some(init) = init {
                    self.collect_expr_closures(owner, init);
                }
            }
            TForInit::Assign { target, value } => {
                self.collect_expr_closures(owner, target);
                self.collect_expr_closures(owner, value);
            }
            TForInit::Expr(expr) => self.collect_expr_closures(owner, expr),
        }
    }

    fn collect_expr_closures(&mut self, owner: DefId, expr: &TExpr) {
        self.collect_ty_closure(&expr.ty);
        match &expr.kind {
            TExprKind::Closure {
                id,
                params,
                captures,
                body,
            } => {
                self.plan.closure_defs
                    .entry((owner.0, *id))
                    .or_insert_with(|| ClosureDef {
                        id: *id,
                        owner,
                        ty: expr.ty.clone(),
                        params: params.clone(),
                        captures: captures.clone(),
                        body: body.clone(),
                    });
                for (_, _, ty) in params {
                    self.collect_ty_closure(ty);
                }
                for capture in captures {
                    self.collect_ty_closure(&capture.ty);
                }
                self.collect_closure_body_closures(owner, body);
            }
            TExprKind::FunctionToClosure(inner) => {
                self.collect_expr_closures(owner, inner);
                self.collect_retained_closure_witnesses(&expr.ty, &inner.ty, expr.span);
                let key = self.function_closure_wrapper_key(&expr.ty, &inner.ty);
                self.plan.function_closure_wrappers
                    .entry(key)
                    .or_insert_with(|| FunctionClosureWrapper {
                        closure_ty: expr.ty.clone(),
                        function_ty: inner.ty.clone(),
                    });
            }
            TExprKind::RetainClosure {
                expr: inner,
                source_ty,
            } => {
                self.collect_ty_closure(source_ty);
                self.collect_retained_closure_wrapper(&expr.ty, source_ty);
                self.collect_retained_closure_witnesses(&expr.ty, source_ty, expr.span);
                self.collect_expr_closures(owner, inner);
            }
            TExprKind::Unary { expr, .. } | TExprKind::Cast { expr, .. } | TExprKind::Try(expr) => {
                self.collect_expr_closures(owner, expr)
            }
            TExprKind::Binary { left, right, .. } => {
                self.collect_expr_closures(owner, left);
                self.collect_expr_closures(owner, right);
            }
            TExprKind::Call { callee, args, .. } => {
                self.collect_expr_closures(owner, callee);
                for arg in args {
                    self.collect_expr_closures(owner, arg);
                }
            }
            TExprKind::ArrayToSlice(inner) => self.collect_expr_closures(owner, inner),
            TExprKind::MakeDynamicInterface { expr, concrete_ty } => {
                self.collect_ty_closure(concrete_ty);
                self.collect_expr_closures(owner, expr);
            }
            TExprKind::DynamicInterfaceCall { receiver, args, .. }
            | TExprKind::RetainedClosureInterfaceCall { receiver, args, .. } => {
                self.collect_expr_closures(owner, receiver);
                for arg in args {
                    self.collect_expr_closures(owner, arg);
                }
            }
            TExprKind::Field { base, .. } | TExprKind::Arrow { base, .. } => {
                self.collect_expr_closures(owner, base)
            }
            TExprKind::Index { base, index } => {
                self.collect_expr_closures(owner, base);
                self.collect_expr_closures(owner, index);
            }
            TExprKind::Slice { base, start, end } => {
                self.collect_expr_closures(owner, base);
                if let Some(start) = start {
                    self.collect_expr_closures(owner, start);
                }
                if let Some(end) = end {
                    self.collect_expr_closures(owner, end);
                }
            }
            TExprKind::BuiltinCloneMessage { value, message_ty } => {
                self.collect_ty_closure(message_ty);
                self.collect_expr_closures(owner, value);
            }
            TExprKind::MetaAsRefRepr { value, source_ty }
            | TExprKind::MetaIntoRepr { value, source_ty } => {
                self.collect_ty_closure(source_ty);
                self.collect_expr_closures(owner, value);
            }
            TExprKind::MetaFromRepr { value, target_ty } => {
                self.collect_ty_closure(target_ty);
                self.collect_expr_closures(owner, value);
            }
            TExprKind::ActorSpawn {
                initial_state,
                handler,
                state_ty,
                message_ty,
                handler_ty,
            } => {
                self.collect_ty_closure(state_ty);
                self.collect_ty_closure(message_ty);
                self.collect_ty_closure(handler_ty);
                self.collect_expr_closures(owner, initial_state);
                self.collect_expr_closures(owner, handler);
                let name = self.actor_dispatch_name(state_ty, message_ty, handler_ty);
                self.plan.actor_dispatches
                    .entry(name.clone())
                    .or_insert_with(|| ActorDispatch {
                        name,
                        state_ty: state_ty.clone(),
                        message_ty: message_ty.clone(),
                        handler_ty: handler_ty.clone(),
                    });
            }
            TExprKind::ActorSend {
                actor,
                value,
                message_ty,
            } => {
                self.collect_ty_closure(message_ty);
                self.collect_expr_closures(owner, actor);
                self.collect_expr_closures(owner, value);
            }
            TExprKind::ActorStop { actor, message_ty }
            | TExprKind::ActorJoin { actor, message_ty } => {
                self.collect_ty_closure(message_ty);
                self.collect_expr_closures(owner, actor);
            }
            TExprKind::TypeSize { ty } | TExprKind::TypeAlign { ty } => {
                self.collect_ty_closure(ty);
            }
            TExprKind::StructLiteral { fields, .. } => {
                for (_, value) in fields {
                    self.collect_expr_closures(owner, value);
                }
            }
            TExprKind::EnumLiteral { payload, .. } => {
                for value in payload {
                    self.collect_expr_closures(owner, value);
                }
            }
            TExprKind::ArrayLiteral(elements) => {
                for element in elements {
                    self.collect_expr_closures(owner, element);
                }
            }
            TExprKind::ArrayRepeat { element, .. } => self.collect_expr_closures(owner, element),
            TExprKind::Local(..)
            | TExprKind::Function(_, _)
            | TExprKind::GenericFunction { .. }
            | TExprKind::Literal(_) => {}
        }
    }

    fn collect_closure_body_closures(&mut self, owner: DefId, body: &TClosureBody) {
        match body {
            TClosureBody::Expr(expr) => self.collect_expr_closures(owner, expr),
            TClosureBody::Block(block) => self.collect_block_closures(owner, block),
        }
    }

    fn collect_ty_dynamic(&mut self, ty: &Ty) {
        match ty {
            Ty::Const(inner) => self.collect_ty_dynamic(inner),
            Ty::DynamicInterface { .. } => {
                let name = self.dynamic_type_name(ty);
                self.plan.dynamic_types.insert(name, ty.clone());
                if let Ty::DynamicInterface { args, .. } = ty {
                    for arg in args {
                        self.collect_ty_dynamic(arg);
                    }
                }
            }
            Ty::Pointer { inner, .. } | Ty::Array { elem: inner, .. } | Ty::Slice(inner) => {
                self.collect_ty_dynamic(inner)
            }
            Ty::Named { args, .. } => {
                for arg in args {
                    self.collect_ty_dynamic(arg);
                }
            }
            Ty::Function { ret, params, .. } => {
                self.collect_ty_dynamic(ret);
                for param in params {
                    self.collect_ty_dynamic(param);
                }
            }
            Ty::Closure { ret, params, .. } | Ty::ClosureInstance { ret, params, .. } => {
                self.collect_ty_dynamic(ret);
                for param in params {
                    self.collect_ty_dynamic(param);
                }
            }
            _ => {}
        }
    }

    fn collect_block_dynamic(&mut self, block: &TBlock) {
        for stmt in &block.statements {
            match &stmt.kind {
                TStmtKind::Block(block) => self.collect_block_dynamic(block),
                TStmtKind::VarDecl { ty, init, .. } => {
                    self.collect_ty_dynamic(ty);
                    if let Some(init) = init {
                        self.collect_expr_dynamic(init);
                    }
                }
                TStmtKind::Assign { target, value } => {
                    self.collect_expr_dynamic(target);
                    self.collect_expr_dynamic(value);
                }
                TStmtKind::If {
                    cond,
                    then_block,
                    else_branch,
                } => {
                    self.collect_expr_dynamic(cond);
                    self.collect_block_dynamic(then_block);
                    if let Some(else_branch) = else_branch {
                        self.collect_stmt_dynamic(else_branch);
                    }
                }
                TStmtKind::While { cond, body } => {
                    self.collect_expr_dynamic(cond);
                    self.collect_block_dynamic(body);
                }
                TStmtKind::For {
                    init,
                    cond,
                    step,
                    body,
                } => {
                    if let Some(init) = init {
                        self.collect_for_clause_dynamic(init);
                    }
                    if let Some(cond) = cond {
                        self.collect_expr_dynamic(cond);
                    }
                    if let Some(step) = step {
                        self.collect_for_clause_dynamic(step);
                    }
                    self.collect_block_dynamic(body);
                }
                TStmtKind::Switch {
                    expr,
                    cases,
                    default,
                    ..
                } => {
                    self.collect_expr_dynamic(expr);
                    for case in cases {
                        for stmt in &case.statements {
                            self.collect_stmt_dynamic(stmt);
                        }
                    }
                    for stmt in default {
                        self.collect_stmt_dynamic(stmt);
                    }
                }
                TStmtKind::Defer(expr) | TStmtKind::Return(Some(expr)) | TStmtKind::Expr(expr) => {
                    self.collect_expr_dynamic(expr)
                }
                TStmtKind::Return(None)
                | TStmtKind::Break
                | TStmtKind::Continue
                | TStmtKind::Unsupported => {}
            }
        }
    }

    fn collect_stmt_dynamic(&mut self, stmt: &TStmt) {
        let fake = TBlock {
            span: stmt.span,
            statements: vec![stmt.clone()],
        };
        self.collect_block_dynamic(&fake);
    }

    fn collect_for_clause_dynamic(&mut self, clause: &TForInit) {
        match clause {
            TForInit::VarDecl { ty, init, .. } => {
                self.collect_ty_dynamic(ty);
                if let Some(init) = init {
                    self.collect_expr_dynamic(init);
                }
            }
            TForInit::Assign { target, value } => {
                self.collect_expr_dynamic(target);
                self.collect_expr_dynamic(value);
            }
            TForInit::Expr(expr) => self.collect_expr_dynamic(expr),
        }
    }

    fn collect_expr_dynamic(&mut self, expr: &TExpr) {
        self.collect_ty_dynamic(&expr.ty);
        match &expr.kind {
            TExprKind::MakeDynamicInterface {
                expr: inner,
                concrete_ty,
            } => {
                self.collect_expr_dynamic(inner);
                self.collect_ty_dynamic(concrete_ty);
                if !matches!(concrete_ty.unqualified(), Ty::DynamicInterface { .. }) {
                    self.plan.dynamic_impls.insert(
                        self.dynamic_impl_key(&expr.ty, concrete_ty),
                        DynamicImplUse {
                            dyn_ty: expr.ty.clone(),
                            concrete_ty: concrete_ty.clone(),
                        },
                    );
                }
            }
            TExprKind::DynamicInterfaceCall { receiver, args, .. }
            | TExprKind::RetainedClosureInterfaceCall { receiver, args, .. } => {
                self.collect_expr_dynamic(receiver);
                for arg in args {
                    self.collect_expr_dynamic(arg);
                }
            }
            TExprKind::Unary { expr, .. } | TExprKind::Cast { expr, .. } | TExprKind::Try(expr) => {
                self.collect_expr_dynamic(expr)
            }
            TExprKind::Binary { left, right, .. } => {
                self.collect_expr_dynamic(left);
                self.collect_expr_dynamic(right);
            }
            TExprKind::Call { callee, args, .. } => {
                self.collect_expr_dynamic(callee);
                for arg in args {
                    self.collect_expr_dynamic(arg);
                }
            }
            TExprKind::Closure { body, .. } => self.collect_closure_body_dynamic(body),
            TExprKind::FunctionToClosure(inner) => self.collect_expr_dynamic(inner),
            TExprKind::RetainClosure { expr, source_ty } => {
                self.collect_ty_dynamic(source_ty);
                self.collect_expr_dynamic(expr);
            }
            TExprKind::ArrayToSlice(inner) => self.collect_expr_dynamic(inner),
            TExprKind::Field { base, .. } | TExprKind::Arrow { base, .. } => {
                self.collect_expr_dynamic(base)
            }
            TExprKind::Index { base, index } => {
                self.collect_expr_dynamic(base);
                self.collect_expr_dynamic(index);
            }
            TExprKind::Slice { base, start, end } => {
                self.collect_expr_dynamic(base);
                if let Some(start) = start {
                    self.collect_expr_dynamic(start);
                }
                if let Some(end) = end {
                    self.collect_expr_dynamic(end);
                }
            }
            TExprKind::BuiltinCloneMessage { value, .. }
            | TExprKind::MetaAsRefRepr { value, .. }
            | TExprKind::MetaIntoRepr { value, .. }
            | TExprKind::MetaFromRepr { value, .. } => self.collect_expr_dynamic(value),
            TExprKind::ActorSpawn {
                initial_state,
                handler,
                ..
            } => {
                self.collect_expr_dynamic(initial_state);
                self.collect_expr_dynamic(handler);
            }
            TExprKind::ActorSend { actor, value, .. } => {
                self.collect_expr_dynamic(actor);
                self.collect_expr_dynamic(value);
            }
            TExprKind::ActorStop { actor, .. } | TExprKind::ActorJoin { actor, .. } => {
                self.collect_expr_dynamic(actor);
            }
            TExprKind::TypeSize { .. } | TExprKind::TypeAlign { .. } => {}
            TExprKind::StructLiteral { fields, .. } => {
                for (_, value) in fields {
                    self.collect_expr_dynamic(value);
                }
            }
            TExprKind::EnumLiteral { payload, .. } => {
                for value in payload {
                    self.collect_expr_dynamic(value);
                }
            }
            TExprKind::ArrayLiteral(elements) => {
                for element in elements {
                    self.collect_expr_dynamic(element);
                }
            }
            TExprKind::ArrayRepeat { element, .. } => self.collect_expr_dynamic(element),
            TExprKind::Local(..)
            | TExprKind::Function(_, _)
            | TExprKind::GenericFunction { .. }
            | TExprKind::Literal(_) => {}
        }
    }

    fn collect_closure_body_dynamic(&mut self, body: &TClosureBody) {
        match body {
            TClosureBody::Expr(expr) => self.collect_expr_dynamic(expr),
            TClosureBody::Block(block) => self.collect_block_dynamic(block),
        }
    }

    fn collect_string_literals(&mut self) {
        let functions = self.program.checked.functions.clone();
        for function in &functions {
            if let Some(body) = &function.body {
                self.collect_block_string_literals(body);
            }
        }
        let keys = self.plan.string_literals.keys().copied().collect::<Vec<_>>();
        for (idx, key) in keys.into_iter().enumerate() {
            self.plan.string_literal_names
                .insert(key, format!("ciel_str_{idx}"));
        }
    }

    fn collect_source_locations(&mut self) {
        let functions = self.program.checked.functions.clone();
        for function in &functions {
            if let Some(body) = &function.body {
                self.collect_block_locations(body);
            }
        }
        let keys = self.plan.source_locations.keys().copied().collect::<Vec<_>>();
        for (idx, key) in keys.into_iter().enumerate() {
            if let Some(location) = self.plan.source_locations.get_mut(&key) {
                location.name = format!("ciel_loc_{idx}");
            }
        }
    }

    fn collect_block_locations(&mut self, block: &TBlock) {
        self.register_source_location(block.span);
        for stmt in &block.statements {
            self.collect_stmt_locations(stmt);
        }
    }

    fn collect_stmt_locations(&mut self, stmt: &TStmt) {
        self.register_source_location(stmt.span);
        match &stmt.kind {
            TStmtKind::Block(block) => self.collect_block_locations(block),
            TStmtKind::VarDecl { init, .. } => {
                if let Some(init) = init {
                    self.collect_expr_locations(init);
                }
            }
            TStmtKind::Assign { target, value } => {
                self.collect_expr_locations(target);
                self.collect_expr_locations(value);
            }
            TStmtKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                self.collect_expr_locations(cond);
                self.collect_block_locations(then_block);
                if let Some(else_branch) = else_branch {
                    self.collect_stmt_locations(else_branch);
                }
            }
            TStmtKind::While { cond, body } => {
                self.collect_expr_locations(cond);
                self.collect_block_locations(body);
            }
            TStmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(init) = init {
                    self.collect_for_clause_locations(init);
                }
                if let Some(cond) = cond {
                    self.collect_expr_locations(cond);
                }
                if let Some(step) = step {
                    self.collect_for_clause_locations(step);
                }
                self.collect_block_locations(body);
            }
            TStmtKind::Switch {
                expr,
                cases,
                default,
                ..
            } => {
                self.collect_expr_locations(expr);
                for case in cases {
                    self.collect_pattern_locations(&case.pattern);
                    for stmt in &case.statements {
                        self.collect_stmt_locations(stmt);
                    }
                }
                for stmt in default {
                    self.collect_stmt_locations(stmt);
                }
            }
            TStmtKind::Defer(expr) | TStmtKind::Return(Some(expr)) | TStmtKind::Expr(expr) => {
                self.collect_expr_locations(expr);
            }
            TStmtKind::Return(None)
            | TStmtKind::Break
            | TStmtKind::Continue
            | TStmtKind::Unsupported => {}
        }
    }

    fn collect_for_clause_locations(&mut self, clause: &TForInit) {
        match clause {
            TForInit::VarDecl { init, .. } => {
                if let Some(init) = init {
                    self.collect_expr_locations(init);
                }
            }
            TForInit::Assign { target, value } => {
                self.collect_expr_locations(target);
                self.collect_expr_locations(value);
            }
            TForInit::Expr(expr) => self.collect_expr_locations(expr),
        }
    }

    fn collect_expr_locations(&mut self, expr: &TExpr) {
        self.register_source_location(expr.span);
        match &expr.kind {
            TExprKind::Unary { expr, .. } | TExprKind::Cast { expr, .. } | TExprKind::Try(expr) => {
                self.collect_expr_locations(expr)
            }
            TExprKind::Binary { left, right, .. } => {
                self.collect_expr_locations(left);
                self.collect_expr_locations(right);
            }
            TExprKind::Call { callee, args, .. } => {
                self.collect_expr_locations(callee);
                for arg in args {
                    self.collect_expr_locations(arg);
                }
            }
            TExprKind::Closure { body, .. } => self.collect_closure_body_locations(body),
            TExprKind::FunctionToClosure(inner) => self.collect_expr_locations(inner),
            TExprKind::RetainClosure { expr, .. } => self.collect_expr_locations(expr),
            TExprKind::ArrayToSlice(inner) => self.collect_expr_locations(inner),
            TExprKind::MakeDynamicInterface { expr, .. } => self.collect_expr_locations(expr),
            TExprKind::DynamicInterfaceCall { receiver, args, .. }
            | TExprKind::RetainedClosureInterfaceCall { receiver, args, .. } => {
                self.collect_expr_locations(receiver);
                for arg in args {
                    self.collect_expr_locations(arg);
                }
            }
            TExprKind::Field { base, .. } | TExprKind::Arrow { base, .. } => {
                self.collect_expr_locations(base)
            }
            TExprKind::Index { base, index } => {
                self.collect_expr_locations(base);
                self.collect_expr_locations(index);
            }
            TExprKind::Slice { base, start, end } => {
                self.collect_expr_locations(base);
                if let Some(start) = start {
                    self.collect_expr_locations(start);
                }
                if let Some(end) = end {
                    self.collect_expr_locations(end);
                }
            }
            TExprKind::BuiltinCloneMessage { value, .. }
            | TExprKind::MetaAsRefRepr { value, .. }
            | TExprKind::MetaIntoRepr { value, .. }
            | TExprKind::MetaFromRepr { value, .. } => self.collect_expr_locations(value),
            TExprKind::ActorSpawn {
                initial_state,
                handler,
                ..
            } => {
                self.collect_expr_locations(initial_state);
                self.collect_expr_locations(handler);
            }
            TExprKind::ActorSend { actor, value, .. } => {
                self.collect_expr_locations(actor);
                self.collect_expr_locations(value);
            }
            TExprKind::ActorStop { actor, .. } | TExprKind::ActorJoin { actor, .. } => {
                self.collect_expr_locations(actor);
            }
            TExprKind::TypeSize { .. } | TExprKind::TypeAlign { .. } => {}
            TExprKind::StructLiteral { fields, .. } => {
                for (_, value) in fields {
                    self.collect_expr_locations(value);
                }
            }
            TExprKind::EnumLiteral { payload, .. } => {
                for value in payload {
                    self.collect_expr_locations(value);
                }
            }
            TExprKind::ArrayLiteral(elements) => {
                for element in elements {
                    self.collect_expr_locations(element);
                }
            }
            TExprKind::ArrayRepeat { element, .. } => self.collect_expr_locations(element),
            TExprKind::Local(..)
            | TExprKind::Function(_, _)
            | TExprKind::GenericFunction { .. }
            | TExprKind::Literal(_) => {}
        }
    }

    fn collect_closure_body_locations(&mut self, body: &TClosureBody) {
        match body {
            TClosureBody::Expr(expr) => self.collect_expr_locations(expr),
            TClosureBody::Block(block) => self.collect_block_locations(block),
        }
    }

    fn collect_pattern_locations(&mut self, pattern: &TPattern) {
        match pattern {
            TPattern::Wildcard { .. } | TPattern::Binding { .. } => {}
            TPattern::Variant { payload, .. } => {
                for pattern in payload {
                    self.collect_pattern_locations(pattern);
                }
            }
        }
    }

    fn register_source_location(&mut self, span: crate::span::Span) {
        let (line, _) = self.source_map.line_col(span.file, span.start);
        let key = (span.file.0, line);
        self.plan.source_locations.entry(key).or_insert_with(|| {
            let file = self.source_map.file_path(span.file).display().to_string();
            SourceLocation {
                name: String::new(),
                file,
                line,
            }
        });
    }

    fn collect_block_string_literals(&mut self, block: &TBlock) {
        for stmt in &block.statements {
            match &stmt.kind {
                TStmtKind::Block(block) => self.collect_block_string_literals(block),
                TStmtKind::VarDecl { init, .. } => {
                    if let Some(init) = init {
                        self.collect_expr_string_literals(init);
                    }
                }
                TStmtKind::Assign { target, value } => {
                    self.collect_expr_string_literals(target);
                    self.collect_expr_string_literals(value);
                }
                TStmtKind::If {
                    cond,
                    then_block,
                    else_branch,
                } => {
                    self.collect_expr_string_literals(cond);
                    self.collect_block_string_literals(then_block);
                    if let Some(else_branch) = else_branch {
                        self.collect_stmt_string_literals(else_branch);
                    }
                }
                TStmtKind::While { cond, body } => {
                    self.collect_expr_string_literals(cond);
                    self.collect_block_string_literals(body);
                }
                TStmtKind::For {
                    init,
                    cond,
                    step,
                    body,
                } => {
                    if let Some(init) = init {
                        match init {
                            TForInit::VarDecl { init, .. } => {
                                if let Some(init) = init {
                                    self.collect_expr_string_literals(init);
                                }
                            }
                            TForInit::Assign { target, value } => {
                                self.collect_expr_string_literals(target);
                                self.collect_expr_string_literals(value);
                            }
                            TForInit::Expr(expr) => self.collect_expr_string_literals(expr),
                        }
                    }
                    if let Some(cond) = cond {
                        self.collect_expr_string_literals(cond);
                    }
                    if let Some(step) = step {
                        self.collect_for_clause_string_literals(step);
                    }
                    self.collect_block_string_literals(body);
                }
                TStmtKind::Switch {
                    expr,
                    cases,
                    default,
                    ..
                } => {
                    self.collect_expr_string_literals(expr);
                    for case in cases {
                        for stmt in &case.statements {
                            self.collect_stmt_string_literals(stmt);
                        }
                    }
                    for stmt in default {
                        self.collect_stmt_string_literals(stmt);
                    }
                }
                TStmtKind::Defer(expr) | TStmtKind::Return(Some(expr)) | TStmtKind::Expr(expr) => {
                    self.collect_expr_string_literals(expr)
                }
                TStmtKind::Return(None)
                | TStmtKind::Break
                | TStmtKind::Continue
                | TStmtKind::Unsupported => {}
            }
        }
    }

    fn collect_stmt_string_literals(&mut self, stmt: &TStmt) {
        let fake = TBlock {
            span: stmt.span,
            statements: vec![stmt.clone()],
        };
        self.collect_block_string_literals(&fake);
    }

    fn collect_for_clause_string_literals(&mut self, clause: &TForInit) {
        match clause {
            TForInit::VarDecl { init, .. } => {
                if let Some(init) = init {
                    self.collect_expr_string_literals(init);
                }
            }
            TForInit::Assign { target, value } => {
                self.collect_expr_string_literals(target);
                self.collect_expr_string_literals(value);
            }
            TForInit::Expr(expr) => self.collect_expr_string_literals(expr),
        }
    }

    fn collect_expr_string_literals(&mut self, expr: &TExpr) {
        if let TExprKind::Literal(crate::ast::Literal::String(raw)) = &expr.kind {
            self.plan.string_literals
                .insert(span_key(expr.span), raw.clone());
        }
        match &expr.kind {
            TExprKind::Unary { expr, .. } | TExprKind::Cast { expr, .. } | TExprKind::Try(expr) => {
                self.collect_expr_string_literals(expr)
            }
            TExprKind::Binary { left, right, .. } => {
                self.collect_expr_string_literals(left);
                self.collect_expr_string_literals(right);
            }
            TExprKind::Call { callee, args, .. } => {
                self.collect_expr_string_literals(callee);
                for arg in args {
                    self.collect_expr_string_literals(arg);
                }
            }
            TExprKind::Closure { body, .. } => self.collect_closure_body_string_literals(body),
            TExprKind::FunctionToClosure(inner) => self.collect_expr_string_literals(inner),
            TExprKind::RetainClosure { expr, .. } => self.collect_expr_string_literals(expr),
            TExprKind::ArrayToSlice(inner) => self.collect_expr_string_literals(inner),
            TExprKind::MakeDynamicInterface { expr, .. } => self.collect_expr_string_literals(expr),
            TExprKind::DynamicInterfaceCall { receiver, args, .. }
            | TExprKind::RetainedClosureInterfaceCall { receiver, args, .. } => {
                self.collect_expr_string_literals(receiver);
                for arg in args {
                    self.collect_expr_string_literals(arg);
                }
            }
            TExprKind::Field { base, .. } | TExprKind::Arrow { base, .. } => {
                self.collect_expr_string_literals(base)
            }
            TExprKind::Index { base, index } => {
                self.collect_expr_string_literals(base);
                self.collect_expr_string_literals(index);
            }
            TExprKind::Slice { base, start, end } => {
                self.collect_expr_string_literals(base);
                if let Some(start) = start {
                    self.collect_expr_string_literals(start);
                }
                if let Some(end) = end {
                    self.collect_expr_string_literals(end);
                }
            }
            TExprKind::BuiltinCloneMessage { value, .. }
            | TExprKind::MetaAsRefRepr { value, .. }
            | TExprKind::MetaIntoRepr { value, .. }
            | TExprKind::MetaFromRepr { value, .. } => self.collect_expr_string_literals(value),
            TExprKind::ActorSpawn {
                initial_state,
                handler,
                ..
            } => {
                self.collect_expr_string_literals(initial_state);
                self.collect_expr_string_literals(handler);
            }
            TExprKind::ActorSend { actor, value, .. } => {
                self.collect_expr_string_literals(actor);
                self.collect_expr_string_literals(value);
            }
            TExprKind::ActorStop { actor, .. } | TExprKind::ActorJoin { actor, .. } => {
                self.collect_expr_string_literals(actor);
            }
            TExprKind::TypeSize { .. } | TExprKind::TypeAlign { .. } => {}
            TExprKind::StructLiteral { fields, .. } => {
                for (_, value) in fields {
                    self.collect_expr_string_literals(value);
                }
            }
            TExprKind::EnumLiteral { payload, .. } => {
                for value in payload {
                    self.collect_expr_string_literals(value);
                }
            }
            TExprKind::ArrayLiteral(elements) => {
                for element in elements {
                    self.collect_expr_string_literals(element);
                }
            }
            TExprKind::ArrayRepeat { element, .. } => self.collect_expr_string_literals(element),
            TExprKind::Local(..)
            | TExprKind::Function(_, _)
            | TExprKind::GenericFunction { .. }
            | TExprKind::Literal(_) => {}
        }
    }

    fn collect_closure_body_string_literals(&mut self, body: &TClosureBody) {
        match body {
            TClosureBody::Expr(expr) => self.collect_expr_string_literals(expr),
            TClosureBody::Block(block) => self.collect_block_string_literals(block),
        }
    }

    fn collect_block_slices(&mut self, block: &TBlock) {
        for stmt in &block.statements {
            match &stmt.kind {
                TStmtKind::Block(block) => self.collect_block_slices(block),
                TStmtKind::VarDecl { ty, init, .. } => {
                    self.collect_ty_slice(ty);
                    if let Some(init) = init {
                        self.collect_expr_slices(init);
                    }
                }
                TStmtKind::Assign { target, value } => {
                    self.collect_expr_slices(target);
                    self.collect_expr_slices(value);
                }
                TStmtKind::If {
                    cond,
                    then_block,
                    else_branch,
                } => {
                    self.collect_expr_slices(cond);
                    self.collect_block_slices(then_block);
                    if let Some(else_branch) = else_branch {
                        self.collect_stmt_slices(else_branch);
                    }
                }
                TStmtKind::While { cond, body } => {
                    self.collect_expr_slices(cond);
                    self.collect_block_slices(body);
                }
                TStmtKind::For {
                    init,
                    cond,
                    step,
                    body,
                } => {
                    if let Some(init) = init {
                        match init {
                            TForInit::VarDecl { ty, init, .. } => {
                                self.collect_ty_slice(ty);
                                if let Some(init) = init {
                                    self.collect_expr_slices(init);
                                }
                            }
                            TForInit::Assign { target, value } => {
                                self.collect_expr_slices(target);
                                self.collect_expr_slices(value);
                            }
                            TForInit::Expr(expr) => self.collect_expr_slices(expr),
                        }
                    }
                    if let Some(cond) = cond {
                        self.collect_expr_slices(cond);
                    }
                    if let Some(step) = step {
                        self.collect_for_clause_slices(step);
                    }
                    self.collect_block_slices(body);
                }
                TStmtKind::Switch {
                    expr,
                    cases,
                    default,
                    ..
                } => {
                    self.collect_expr_slices(expr);
                    for case in cases {
                        for stmt in &case.statements {
                            self.collect_stmt_slices(stmt);
                        }
                    }
                    for stmt in default {
                        self.collect_stmt_slices(stmt);
                    }
                }
                TStmtKind::Defer(expr) | TStmtKind::Return(Some(expr)) | TStmtKind::Expr(expr) => {
                    self.collect_expr_slices(expr)
                }
                TStmtKind::Return(None)
                | TStmtKind::Break
                | TStmtKind::Continue
                | TStmtKind::Unsupported => {}
            }
        }
    }

    fn collect_stmt_slices(&mut self, stmt: &TStmt) {
        let fake = TBlock {
            span: stmt.span,
            statements: vec![stmt.clone()],
        };
        self.collect_block_slices(&fake);
    }

    fn collect_for_clause_slices(&mut self, clause: &TForInit) {
        match clause {
            TForInit::VarDecl { ty, init, .. } => {
                self.collect_ty_slice(ty);
                if let Some(init) = init {
                    self.collect_expr_slices(init);
                }
            }
            TForInit::Assign { target, value } => {
                self.collect_expr_slices(target);
                self.collect_expr_slices(value);
            }
            TForInit::Expr(expr) => self.collect_expr_slices(expr),
        }
    }

    fn collect_expr_slices(&mut self, expr: &TExpr) {
        self.collect_ty_slice(&expr.ty);
        match &expr.kind {
            TExprKind::Unary { expr, .. } | TExprKind::Cast { expr, .. } | TExprKind::Try(expr) => {
                self.collect_expr_slices(expr)
            }
            TExprKind::Binary { left, right, .. } => {
                self.collect_expr_slices(left);
                self.collect_expr_slices(right);
            }
            TExprKind::Call { callee, args, .. } => {
                self.collect_expr_slices(callee);
                for arg in args {
                    self.collect_expr_slices(arg);
                }
            }
            TExprKind::Closure { body, .. } => self.collect_closure_body_slices(body),
            TExprKind::FunctionToClosure(inner) => self.collect_expr_slices(inner),
            TExprKind::RetainClosure { expr, source_ty } => {
                self.collect_ty_slice(source_ty);
                self.collect_expr_slices(expr);
            }
            TExprKind::ArrayToSlice(inner) => self.collect_expr_slices(inner),
            TExprKind::MakeDynamicInterface { expr, concrete_ty } => {
                self.collect_ty_slice(concrete_ty);
                self.collect_expr_slices(expr);
            }
            TExprKind::DynamicInterfaceCall { receiver, args, .. }
            | TExprKind::RetainedClosureInterfaceCall { receiver, args, .. } => {
                self.collect_expr_slices(receiver);
                for arg in args {
                    self.collect_expr_slices(arg);
                }
            }
            TExprKind::Field { base, .. } | TExprKind::Arrow { base, .. } => {
                self.collect_expr_slices(base)
            }
            TExprKind::Index { base, index } => {
                self.collect_expr_slices(base);
                self.collect_expr_slices(index);
            }
            TExprKind::Slice { base, start, end } => {
                self.collect_ty_slice(&expr.ty);
                self.collect_expr_slices(base);
                if let Some(start) = start {
                    self.collect_expr_slices(start);
                }
                if let Some(end) = end {
                    self.collect_expr_slices(end);
                }
            }
            TExprKind::BuiltinCloneMessage { value, message_ty } => {
                self.collect_ty_slice(message_ty);
                self.collect_expr_slices(value);
            }
            TExprKind::MetaAsRefRepr { value, source_ty }
            | TExprKind::MetaIntoRepr { value, source_ty } => {
                self.collect_ty_slice(source_ty);
                self.collect_expr_slices(value);
            }
            TExprKind::MetaFromRepr { value, target_ty } => {
                self.collect_ty_slice(target_ty);
                self.collect_expr_slices(value);
            }
            TExprKind::ActorSpawn {
                initial_state,
                handler,
                state_ty,
                message_ty,
                handler_ty,
            } => {
                self.collect_ty_slice(state_ty);
                self.collect_ty_slice(message_ty);
                self.collect_ty_slice(handler_ty);
                self.collect_expr_slices(initial_state);
                self.collect_expr_slices(handler);
            }
            TExprKind::ActorSend {
                actor,
                value,
                message_ty,
            } => {
                self.collect_ty_slice(message_ty);
                self.collect_expr_slices(actor);
                self.collect_expr_slices(value);
            }
            TExprKind::ActorStop { actor, message_ty }
            | TExprKind::ActorJoin { actor, message_ty } => {
                self.collect_ty_slice(message_ty);
                self.collect_expr_slices(actor);
            }
            TExprKind::TypeSize { ty } | TExprKind::TypeAlign { ty } => {
                self.collect_ty_slice(ty);
            }
            TExprKind::StructLiteral { fields, .. } => {
                for (_, value) in fields {
                    self.collect_expr_slices(value);
                }
            }
            TExprKind::EnumLiteral { payload, .. } => {
                for value in payload {
                    self.collect_expr_slices(value);
                }
            }
            TExprKind::ArrayLiteral(elements) => {
                for element in elements {
                    self.collect_expr_slices(element);
                }
            }
            TExprKind::ArrayRepeat { element, .. } => self.collect_expr_slices(element),
            TExprKind::Local(..)
            | TExprKind::Function(_, _)
            | TExprKind::GenericFunction { .. }
            | TExprKind::Literal(_) => {}
        }
    }

    fn collect_closure_body_slices(&mut self, body: &TClosureBody) {
        match body {
            TClosureBody::Expr(expr) => self.collect_expr_slices(expr),
            TClosureBody::Block(block) => self.collect_block_slices(block),
        }
    }

    fn gen_function(&mut self, function: &CheckedFunction) -> DiagResult<()> {
        let Some(body) = &function.body else {
            return Ok(());
        };
        self.emit_line_directive(body.span);
        self.line(&format!("{} {{", self.function_decl(function, false)));
        self.defer_stack.clear();
        self.loop_defer_starts.clear();
        self.current_return_ty = function.ret.clone();
        self.current_heap_locals = self
            .escapes
            .functions
            .get(&function.def_id)
            .map(|escape| escape.heap_locals.clone())
            .unwrap_or_default();
        self.current_param_locals = function
            .params
            .iter()
            .filter_map(|(local_id, name, _)| local_id.map(|id| (id, name.clone())))
            .collect();
        self.current_closure_owner = Some(function.def_id);
        let falls_through = self.gen_block_inner(body, 1)?;
        if falls_through && function.ret.is_never() {
            self.line_indent(1, "ciel_panic(NULL, 0);");
        } else if falls_through && !function.ret.is_erased_value() {
            self.line_indent(1, "ciel_panic(NULL, 0);");
            self.line_indent(1, &format!("return {};", self.zero_value(&function.ret)));
        }
        self.current_heap_locals.clear();
        self.current_param_locals.clear();
        self.current_closure_owner = None;
        self.current_return_ty = Ty::Void;
        self.line("}");
        Ok(())
    }

    fn gen_block(&mut self, block: &TBlock, indent: usize) -> DiagResult<bool> {
        self.line_indent(indent, "{");
        let falls_through = self.gen_block_inner(block, indent + 1)?;
        self.line_indent(indent, "}");
        Ok(falls_through)
    }

    fn gen_block_inner(&mut self, block: &TBlock, indent: usize) -> DiagResult<bool> {
        self.defer_stack.push(Vec::new());
        let mut falls_through = true;
        for stmt in &block.statements {
            if !self.gen_stmt(stmt, indent)? {
                falls_through = false;
                break;
            }
        }
        if falls_through {
            self.emit_current_defers(indent);
        }
        self.defer_stack.pop();
        Ok(falls_through)
    }

    fn gen_stmt(&mut self, stmt: &TStmt, indent: usize) -> DiagResult<bool> {
        self.emit_line_directive(stmt.span);
        match &stmt.kind {
            TStmtKind::Block(block) => self.gen_block(block, indent),
            TStmtKind::VarDecl {
                ty,
                name,
                local_id,
                init,
            } => {
                let cname = self.local_c_name(*local_id, name);
                if ty.is_erased_value() {
                    if let Some(init) = init {
                        let value = self.gen_expr_in_stmt(init, indent)?;
                        self.line_indent(indent, &format!("(void)({value});"));
                    }
                    return Ok(true);
                }
                if self.local_is_heap(*local_id) {
                    self.line_indent(
                        indent,
                        &format!(
                            "{} = ({})ciel_alloc(sizeof({}));",
                            self.c_pointer_decl(ty, &cname),
                            self.c_pointer_type(ty),
                            self.c_sizeof_type(ty)
                        ),
                    );
                    if let Some(init) = init
                        && let Ty::Array { .. } = ty
                        && let TExprKind::ArrayLiteral(elements) = &init.kind
                    {
                        self.emit_heap_array_init(&cname, elements, indent)?;
                        return Ok(true);
                    }
                    if let Some(init) = init
                        && let Ty::Array { elem, .. } = ty
                        && let TExprKind::ArrayRepeat { element, len } = &init.kind
                    {
                        self.emit_array_repeat_init(
                            &format!("(*{cname})"),
                            elem,
                            element,
                            *len,
                            indent,
                        )?;
                        return Ok(true);
                    }
                    if let Some(init) = init
                        && let Ty::Slice(_) = ty
                        && let TExprKind::ArrayLiteral(elements) = &init.kind
                    {
                        let value = self.emit_slice_literal_temp(ty, elements, indent)?;
                        self.line_indent(indent, &format!("*{cname} = {value};"));
                        return Ok(true);
                    }
                    if let Some(init) = init
                        && let Ty::Slice(_) = ty
                        && let TExprKind::ArrayRepeat { element, len } = &init.kind
                    {
                        let value = self.emit_slice_repeat_temp(ty, element, *len, indent)?;
                        self.line_indent(indent, &format!("*{cname} = {value};"));
                        return Ok(true);
                    }
                    if let Some(init) = init {
                        let value = self.gen_expr_in_stmt(init, indent)?;
                        self.line_indent(indent, &format!("*{cname} = {value};"));
                    }
                    return Ok(true);
                }
                if let Some(init) = init
                    && let Ty::Slice(_) = ty
                    && let TExprKind::ArrayLiteral(elements) = &init.kind
                {
                    let value = self.emit_slice_literal_temp(ty, elements, indent)?;
                    self.line_indent(indent, &format!("{} = {value};", self.c_decl(ty, &cname)));
                    return Ok(true);
                }
                if let Some(init) = init
                    && let Ty::Array { elem, .. } = ty
                    && let TExprKind::ArrayRepeat { element, len } = &init.kind
                {
                    self.line_indent(indent, &format!("{};", self.c_decl(ty, &cname)));
                    self.emit_array_repeat_init(&cname, elem, element, *len, indent)?;
                    return Ok(true);
                }
                if let Some(init) = init
                    && let Ty::Slice(_) = ty
                    && let TExprKind::ArrayRepeat { element, len } = &init.kind
                {
                    let value = self.emit_slice_repeat_temp(ty, element, *len, indent)?;
                    self.line_indent(indent, &format!("{} = {value};", self.c_decl(ty, &cname)));
                    return Ok(true);
                }
                let code = if let Some(init) = init {
                    format!(
                        "{} = {};",
                        self.c_decl(ty, &cname),
                        self.gen_expr_in_stmt(init, indent)?
                    )
                } else {
                    format!("{};", self.c_decl(ty, &cname))
                };
                self.line_indent(indent, &code);
                Ok(true)
            }
            TStmtKind::Assign { target, value } => {
                if target.ty.is_erased_value() {
                    let target = self.gen_expr_in_stmt(target, indent)?;
                    let value = self.gen_expr_in_stmt(value, indent)?;
                    self.line_indent(indent, &format!("(void)({target});"));
                    self.line_indent(indent, &format!("(void)({value});"));
                    return Ok(true);
                }
                let target = self.gen_expr_in_stmt(target, indent)?;
                if let Ty::Slice(_) = value.ty
                    && let TExprKind::ArrayLiteral(elements) = &value.kind
                {
                    let value = self.emit_slice_literal_temp(&value.ty, elements, indent)?;
                    self.line_indent(indent, &format!("{target} = {value};"));
                    return Ok(true);
                }
                if let Ty::Slice(_) = value.ty
                    && let TExprKind::ArrayRepeat { element, len } = &value.kind
                {
                    let value = self.emit_slice_repeat_temp(&value.ty, element, *len, indent)?;
                    self.line_indent(indent, &format!("{target} = {value};"));
                    return Ok(true);
                }
                if let Ty::Array { elem, .. } = &value.ty
                    && let TExprKind::ArrayRepeat { element, len } = &value.kind
                {
                    self.emit_array_repeat_init(&target, elem, element, *len, indent)?;
                    return Ok(true);
                }
                let value = self.gen_expr_in_stmt(value, indent)?;
                self.line_indent(indent, &format!("{target} = {value};"));
                Ok(true)
            }
            TStmtKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                let cond = self.gen_expr_in_stmt(cond, indent)?;
                self.line_indent(indent, &format!("if ({cond})"));
                let then_falls_through = self.gen_block(then_block, indent)?;
                let else_falls_through = if let Some(else_branch) = else_branch {
                    self.line_indent(indent, "else");
                    self.gen_stmt(else_branch, indent)?
                } else {
                    true
                };
                Ok(then_falls_through || else_falls_through)
            }
            TStmtKind::While { cond, body } => {
                if expr_needs_stmt_lowering(cond) {
                    self.line_indent(indent, "while (true)");
                    self.line_indent(indent, "{");
                    let cond = self.gen_expr_in_stmt(cond, indent + 1)?;
                    self.line_indent(indent + 1, &format!("if (!({cond})) break;"));
                    self.loop_defer_starts.push(self.defer_stack.len());
                    self.continue_targets.push(None);
                    self.gen_block(body, indent + 1)?;
                    self.continue_targets.pop();
                    self.loop_defer_starts.pop();
                    self.line_indent(indent, "}");
                } else {
                    let cond = self.gen_expr(cond)?;
                    self.line_indent(indent, &format!("while ({cond})"));
                    self.loop_defer_starts.push(self.defer_stack.len());
                    self.continue_targets.push(None);
                    self.gen_block(body, indent)?;
                    self.continue_targets.pop();
                    self.loop_defer_starts.pop();
                }
                Ok(true)
            }
            TStmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if for_stmt_needs_stmt_lowering(init.as_ref(), cond.as_ref(), step.as_ref()) {
                    return self.gen_lowered_for_stmt(
                        init.as_ref(),
                        cond.as_ref(),
                        step.as_ref(),
                        body,
                        indent,
                    );
                }
                let init = if let Some(TForInit::VarDecl {
                    ty,
                    name,
                    local_id,
                    init,
                }) = init
                    && self.local_is_heap(*local_id)
                {
                    self.gen_heap_local_decl(ty, name, *local_id, init.as_ref(), indent)?;
                    String::new()
                } else {
                    init.as_ref()
                        .map(|init| self.gen_for_init(init))
                        .transpose()?
                        .unwrap_or_default()
                };
                let cond = cond
                    .as_ref()
                    .map(|expr| self.gen_expr(expr))
                    .transpose()?
                    .unwrap_or_default();
                let step = step
                    .as_ref()
                    .map(|step| self.gen_for_init(step))
                    .transpose()?
                    .unwrap_or_default();
                self.line_indent(indent, &format!("for ({init}; {cond}; {step})"));
                self.loop_defer_starts.push(self.defer_stack.len());
                self.continue_targets.push(None);
                self.gen_block(body, indent)?;
                self.continue_targets.pop();
                self.loop_defer_starts.pop();
                Ok(true)
            }
            TStmtKind::Switch {
                expr,
                enum_type_name,
                cases,
                has_default,
                default,
                can_fallthrough,
            } => {
                let temp = self.next_temp("switch");
                let expr_code = self.gen_expr_in_stmt(expr, indent)?;
                self.line_indent(indent, &format!("{enum_type_name} {temp} = {expr_code};"));
                let matched = has_default.then(|| self.next_temp("matched"));
                if let Some(matched) = &matched {
                    self.line_indent(indent, &format!("bool {matched} = false;"));
                }
                self.line_indent(indent, &format!("switch ({temp}.tag) {{"));
                let mut grouped = BTreeMap::<usize, Vec<&crate::thir::TCase>>::new();
                for case in cases {
                    grouped.entry(case.variant_index).or_default().push(case);
                }
                for (variant_index, cases) in grouped {
                    self.line_indent(indent + 1, &format!("case {variant_index}: {{"));
                    for case in cases {
                        let mut conditions = Vec::new();
                        self.collect_pattern_conditions(
                            &case.pattern,
                            &temp,
                            true,
                            &mut conditions,
                        );
                        let condition = if conditions.is_empty() {
                            "true".to_string()
                        } else {
                            conditions.join(" && ")
                        };
                        self.line_indent(indent + 2, &format!("if ({condition}) {{"));
                        if let Some(matched) = &matched {
                            self.line_indent(indent + 3, &format!("{matched} = true;"));
                        }
                        self.emit_pattern_bindings(&case.pattern, &temp, indent + 3)?;
                        let mut branch_falls_through = true;
                        for stmt in &case.statements {
                            if !self.gen_stmt(stmt, indent + 3)? {
                                branch_falls_through = false;
                                break;
                            }
                        }
                        if branch_falls_through {
                            self.line_indent(indent + 3, "break;");
                        }
                        self.line_indent(indent + 2, "}");
                    }
                    self.line_indent(indent + 2, "break;");
                    self.line_indent(indent + 1, "}");
                }
                self.line_indent(indent, "}");
                if let Some(matched) = &matched {
                    self.line_indent(indent, &format!("if (!{matched}) {{"));
                    for stmt in default {
                        if !self.gen_stmt(stmt, indent + 1)? {
                            break;
                        }
                    }
                    self.line_indent(indent, "}");
                }
                Ok(*can_fallthrough)
            }
            TStmtKind::Defer(expr) => {
                let call = self.gen_defer_call(expr, indent)?;
                self.defer_stack
                    .last_mut()
                    .expect("defer stack is not empty")
                    .push(call);
                Ok(true)
            }
            TStmtKind::Return(expr) => {
                if let Some(expr) = expr {
                    if self.current_return_ty.is_erased_value() {
                        let value = self.gen_expr_in_stmt(expr, indent)?;
                        self.line_indent(indent, &format!("(void)({value});"));
                        self.emit_all_defers(indent);
                        self.line_indent(indent, "return;");
                        return Ok(false);
                    }
                    if let Ty::Slice(_) = expr.ty
                        && let TExprKind::ArrayLiteral(elements) = &expr.kind
                    {
                        let value = self.emit_slice_literal_temp(&expr.ty, elements, indent)?;
                        let temp = self.next_temp("return");
                        self.line_indent(
                            indent,
                            &format!("{} = {value};", self.c_decl(&expr.ty, &temp)),
                        );
                        self.emit_all_defers(indent);
                        self.line_indent(indent, &format!("return {temp};"));
                        return Ok(false);
                    }
                    if let Ty::Slice(_) = expr.ty
                        && let TExprKind::ArrayRepeat { element, len } = &expr.kind
                    {
                        let value = self.emit_slice_repeat_temp(&expr.ty, element, *len, indent)?;
                        let temp = self.next_temp("return");
                        self.line_indent(
                            indent,
                            &format!("{} = {value};", self.c_decl(&expr.ty, &temp)),
                        );
                        self.emit_all_defers(indent);
                        self.line_indent(indent, &format!("return {temp};"));
                        return Ok(false);
                    }
                    let value = self.gen_expr_in_stmt(expr, indent)?;
                    let temp = self.next_temp("return");
                    self.line_indent(
                        indent,
                        &format!("{} = {value};", self.c_decl(&expr.ty, &temp)),
                    );
                    self.emit_all_defers(indent);
                    self.line_indent(indent, &format!("return {temp};"));
                } else {
                    self.emit_all_defers(indent);
                    self.line_indent(indent, "return;");
                }
                Ok(false)
            }
            TStmtKind::Break => {
                self.emit_loop_defers(indent);
                self.line_indent(indent, "break;");
                Ok(false)
            }
            TStmtKind::Continue => {
                self.emit_loop_defers(indent);
                if let Some(label) = self.continue_targets.last().and_then(|label| label.clone()) {
                    self.line_indent(indent, &format!("goto {label};"));
                } else {
                    self.line_indent(indent, "continue;");
                }
                Ok(false)
            }
            TStmtKind::Expr(expr) => {
                let terminates = expr.is_never();
                let expr = self.gen_expr_in_stmt(expr, indent)?;
                self.line_indent(indent, &format!("(void)({expr});"));
                Ok(!terminates)
            }
            TStmtKind::Unsupported => Err(vec![Diagnostic::new(
                stmt.span,
                "cannot generate C for unsupported statement",
            )]),
        }
    }

    fn collect_pattern_conditions(
        &self,
        pattern: &TPattern,
        value_expr: &str,
        skip_current: bool,
        out: &mut Vec<String>,
    ) {
        match pattern {
            TPattern::Wildcard { .. } | TPattern::Binding { .. } => {}
            TPattern::Variant {
                variant_name,
                variant_index,
                payload,
                ..
            } => {
                if !skip_current {
                    out.push(format!("{value_expr}.tag == {variant_index}"));
                }
                let mut physical_idx = 0;
                for pattern in payload {
                    if pattern.ty().is_erased_value() {
                        continue;
                    }
                    let idx = physical_idx;
                    physical_idx += 1;
                    let child = format!("{value_expr}.as.{variant_name}._{idx}");
                    self.collect_pattern_conditions(pattern, &child, false, out);
                }
            }
        }
    }

    fn emit_pattern_bindings(
        &mut self,
        pattern: &TPattern,
        value_expr: &str,
        indent: usize,
    ) -> DiagResult<()> {
        match pattern {
            TPattern::Wildcard { .. } => {}
            TPattern::Binding { local_id, name, ty } => {
                if ty.is_erased_value() {
                    return Ok(());
                }
                let cname = self.local_c_name(*local_id, name);
                if self.local_is_heap(*local_id) {
                    self.line_indent(
                        indent,
                        &format!(
                            "{} = ({})ciel_alloc(sizeof({}));",
                            self.c_pointer_decl(ty, &cname),
                            self.c_pointer_type(ty),
                            self.c_sizeof_type(ty)
                        ),
                    );
                    self.line_indent(indent, &format!("*{cname} = {value_expr};"));
                } else {
                    self.line_indent(
                        indent,
                        &format!("{} = {value_expr};", self.c_decl(ty, &cname)),
                    );
                }
            }
            TPattern::Variant {
                variant_name,
                payload,
                ..
            } => {
                let mut physical_idx = 0;
                for pattern in payload {
                    if pattern.ty().is_erased_value() {
                        continue;
                    }
                    let idx = physical_idx;
                    physical_idx += 1;
                    let child = format!("{value_expr}.as.{variant_name}._{idx}");
                    self.emit_pattern_bindings(pattern, &child, indent)?;
                }
            }
        }
        Ok(())
    }

    fn gen_for_init(&mut self, init: &TForInit) -> DiagResult<String> {
        match init {
            TForInit::VarDecl {
                ty,
                name,
                local_id,
                init,
            } => {
                let cname = self.local_c_name(*local_id, name);
                if ty.is_erased_value() {
                    return if let Some(init) = init {
                        Ok(format!("(void)({})", self.gen_expr(init)?))
                    } else {
                        Ok(String::new())
                    };
                }
                if let Some(init) = init {
                    Ok(format!(
                        "{} = {}",
                        self.c_decl(ty, &cname),
                        self.gen_expr(init)?
                    ))
                } else {
                    Ok(self.c_decl(ty, &cname))
                }
            }
            TForInit::Assign { target, value } => {
                if target.ty.is_erased_value() {
                    Ok(format!(
                        "(void)({}), (void)({})",
                        self.gen_expr(target)?,
                        self.gen_expr(value)?
                    ))
                } else {
                    Ok(format!(
                        "{} = {}",
                        self.gen_expr(target)?,
                        self.gen_expr(value)?
                    ))
                }
            }
            TForInit::Expr(expr) => self.gen_expr(expr),
        }
    }

    fn gen_lowered_for_stmt(
        &mut self,
        init: Option<&TForInit>,
        cond: Option<&TExpr>,
        step: Option<&TForInit>,
        body: &TBlock,
        indent: usize,
    ) -> DiagResult<bool> {
        self.line_indent(indent, "{");
        if let Some(init) = init {
            self.gen_for_init_stmt(init, indent + 1)?;
        }
        self.line_indent(indent + 1, "while (true)");
        self.line_indent(indent + 1, "{");
        if let Some(cond) = cond {
            let cond = self.gen_expr_in_stmt(cond, indent + 2)?;
            self.line_indent(indent + 2, &format!("if (!({cond})) break;"));
        }
        let step_label = self.next_temp("for_step");
        self.loop_defer_starts.push(self.defer_stack.len());
        self.continue_targets.push(Some(step_label.clone()));
        self.gen_block(body, indent + 2)?;
        self.continue_targets.pop();
        self.loop_defer_starts.pop();
        self.line_indent(indent + 2, &format!("{step_label}:;"));
        if let Some(step) = step {
            self.gen_for_init_stmt(step, indent + 2)?;
        }
        self.line_indent(indent + 1, "}");
        self.line_indent(indent, "}");
        Ok(true)
    }

    fn gen_for_init_stmt(&mut self, init: &TForInit, indent: usize) -> DiagResult<()> {
        match init {
            TForInit::VarDecl {
                ty,
                name,
                local_id,
                init,
            } => {
                let cname = self.local_c_name(*local_id, name);
                if ty.is_erased_value() {
                    if let Some(init) = init {
                        let value = self.gen_expr_in_stmt(init, indent)?;
                        self.line_indent(indent, &format!("(void)({value});"));
                    }
                    return Ok(());
                }
                if self.local_is_heap(*local_id) {
                    return self.gen_heap_local_decl(ty, name, *local_id, init.as_ref(), indent);
                }
                if let Some(init) = init
                    && let Ty::Slice(_) = ty
                    && let TExprKind::ArrayLiteral(elements) = &init.kind
                {
                    let value = self.emit_slice_literal_temp(ty, elements, indent)?;
                    self.line_indent(indent, &format!("{} = {value};", self.c_decl(ty, &cname)));
                    return Ok(());
                }
                if let Some(init) = init
                    && let Ty::Array { elem, .. } = ty
                    && let TExprKind::ArrayRepeat { element, len } = &init.kind
                {
                    self.line_indent(indent, &format!("{};", self.c_decl(ty, &cname)));
                    self.emit_array_repeat_init(&cname, elem, element, *len, indent)?;
                    return Ok(());
                }
                if let Some(init) = init
                    && let Ty::Slice(_) = ty
                    && let TExprKind::ArrayRepeat { element, len } = &init.kind
                {
                    let value = self.emit_slice_repeat_temp(ty, element, *len, indent)?;
                    self.line_indent(indent, &format!("{} = {value};", self.c_decl(ty, &cname)));
                    return Ok(());
                }
                if let Some(init) = init {
                    let value = self.gen_expr_in_stmt(init, indent)?;
                    self.line_indent(indent, &format!("{} = {value};", self.c_decl(ty, &cname)));
                } else {
                    self.line_indent(indent, &format!("{};", self.c_decl(ty, &cname)));
                }
                Ok(())
            }
            TForInit::Assign { target, value } => {
                if target.ty.is_erased_value() {
                    let target = self.gen_expr_in_stmt(target, indent)?;
                    let value = self.gen_expr_in_stmt(value, indent)?;
                    self.line_indent(indent, &format!("(void)({target});"));
                    self.line_indent(indent, &format!("(void)({value});"));
                    return Ok(());
                }
                let target = self.gen_expr_in_stmt(target, indent)?;
                if let Ty::Slice(_) = value.ty
                    && let TExprKind::ArrayLiteral(elements) = &value.kind
                {
                    let value = self.emit_slice_literal_temp(&value.ty, elements, indent)?;
                    self.line_indent(indent, &format!("{target} = {value};"));
                    return Ok(());
                }
                if let Ty::Slice(_) = value.ty
                    && let TExprKind::ArrayRepeat { element, len } = &value.kind
                {
                    let value = self.emit_slice_repeat_temp(&value.ty, element, *len, indent)?;
                    self.line_indent(indent, &format!("{target} = {value};"));
                    return Ok(());
                }
                if let Ty::Array { elem, .. } = &value.ty
                    && let TExprKind::ArrayRepeat { element, len } = &value.kind
                {
                    self.emit_array_repeat_init(&target, elem, element, *len, indent)?;
                    return Ok(());
                }
                let value = self.gen_expr_in_stmt(value, indent)?;
                self.line_indent(indent, &format!("{target} = {value};"));
                Ok(())
            }
            TForInit::Expr(expr) => {
                let value = self.gen_expr_in_stmt(expr, indent)?;
                self.line_indent(indent, &format!("(void)({value});"));
                Ok(())
            }
        }
    }

    fn gen_heap_local_decl(
        &mut self,
        ty: &Ty,
        name: &str,
        local_id: LocalId,
        init: Option<&TExpr>,
        indent: usize,
    ) -> DiagResult<()> {
        let cname = self.local_c_name(local_id, name);
        self.line_indent(
            indent,
            &format!(
                "{} = ({})ciel_alloc(sizeof({}));",
                self.c_pointer_decl(ty, &cname),
                self.c_pointer_type(ty),
                self.c_sizeof_type(ty)
            ),
        );
        if let Some(init) = init
            && let Ty::Array { .. } = ty
            && let TExprKind::ArrayLiteral(elements) = &init.kind
        {
            self.emit_heap_array_init(&cname, elements, indent)?;
            return Ok(());
        }
        if let Some(init) = init
            && let Ty::Array { elem, .. } = ty
            && let TExprKind::ArrayRepeat { element, len } = &init.kind
        {
            self.emit_array_repeat_init(&format!("(*{cname})"), elem, element, *len, indent)?;
            return Ok(());
        }
        if let Some(init) = init
            && let Ty::Slice(_) = ty
            && let TExprKind::ArrayRepeat { element, len } = &init.kind
        {
            let value = self.emit_slice_repeat_temp(ty, element, *len, indent)?;
            self.line_indent(indent, &format!("*{cname} = {value};"));
            return Ok(());
        }
        if let Some(init) = init {
            let value = self.gen_expr_in_stmt(init, indent)?;
            self.line_indent(indent, &format!("*{cname} = {value};"));
        }
        Ok(())
    }

    fn gen_expr(&mut self, expr: &TExpr) -> DiagResult<String> {
        self.gen_expr_with_lowering(expr, None)
    }

    fn gen_expr_in_stmt(&mut self, expr: &TExpr, indent: usize) -> DiagResult<String> {
        self.gen_expr_with_lowering(expr, Some(indent))
    }

    fn gen_call_args(
        &mut self,
        args: &[TExpr],
        stmt_indent: Option<usize>,
    ) -> DiagResult<Vec<String>> {
        if args.iter().any(|arg| arg.ty.is_erased_value()) {
            let Some(indent) = stmt_indent else {
                return Err(vec![Diagnostic::new(
                    args.iter()
                        .find(|arg| arg.ty.is_erased_value())
                        .map(|arg| arg.span),
                    "erased void argument needs statement lowering",
                )]);
            };
            let mut out = Vec::new();
            for arg in args {
                let value = self.gen_expr_in_stmt(arg, indent)?;
                if arg.ty.is_erased_value() {
                    self.line_indent(indent, &format!("(void)({value});"));
                } else {
                    let temp = self.next_temp("call_arg");
                    self.line_indent(
                        indent,
                        &format!("{} = {value};", self.c_decl(&arg.ty, &temp)),
                    );
                    out.push(temp);
                }
            }
            return Ok(out);
        }

        let mut out = Vec::new();
        for arg in args {
            let value = self.gen_expr_with_lowering(arg, stmt_indent)?;
            out.push(value);
        }
        Ok(out)
    }

    fn gen_expr_with_lowering(
        &mut self,
        expr: &TExpr,
        stmt_indent: Option<usize>,
    ) -> DiagResult<String> {
        let code = match &expr.kind {
            TExprKind::Local(local_id, name) => {
                if expr.ty.is_erased_value() {
                    return Ok("((void)0)".to_string());
                }
                if let Some(captured) = self.current_capture_locals.get(local_id) {
                    captured.clone()
                } else {
                    let cname = self.local_c_name(*local_id, name);
                    if self.local_is_heap(*local_id) {
                        format!("(*{cname})")
                    } else {
                        cname
                    }
                }
            }
            TExprKind::Function(def_id, name) => self
                .plan
                .name_map
                .get(def_id)
                .cloned()
                .unwrap_or_else(|| name.clone()),
            TExprKind::GenericFunction { name, .. } => {
                return Err(vec![Diagnostic::new(
                    expr.span,
                    format!(
                        "internal error: unmonomorphized generic function `{name}` reached C codegen"
                    ),
                )]);
            }
            TExprKind::Literal(literal) => self.gen_literal(expr.span, literal, &expr.ty),
            TExprKind::StructLiteral { type_name, fields } => {
                let mut emitted_fields = Vec::new();
                for (name, value) in fields {
                    let value_code = self.gen_expr_with_lowering(value, stmt_indent)?;
                    if value.ty.is_erased_value() {
                        if let Some(indent) = stmt_indent {
                            self.line_indent(indent, &format!("(void)({value_code});"));
                        }
                        continue;
                    }
                    emitted_fields.push(format!(".{} = {}", name, value_code));
                }
                if emitted_fields.is_empty() {
                    format!("({type_name}){{0}}")
                } else {
                    format!("({type_name}){{ {} }}", emitted_fields.join(", "))
                }
            }
            TExprKind::EnumLiteral {
                type_name,
                variant_name,
                variant_index,
                payload,
            } => {
                let mut payload_fields = Vec::new();
                for value in payload {
                    let value_code = self.gen_expr_with_lowering(value, stmt_indent)?;
                    if value.ty.is_erased_value() {
                        if let Some(indent) = stmt_indent {
                            self.line_indent(indent, &format!("(void)({value_code});"));
                        }
                        continue;
                    }
                    let idx = payload_fields.len();
                    payload_fields.push(format!("._{} = {}", idx, value_code));
                }
                let payload = payload_fields.join(", ");
                if payload.is_empty() {
                    format!("({type_name}){{ .tag = {variant_index} }}")
                } else {
                    format!(
                        "({type_name}){{ .tag = {variant_index}, .as.{variant_name} = {{ {payload} }} }}"
                    )
                }
            }
            TExprKind::ArrayLiteral(elements) => {
                if matches!(expr.ty, Ty::Slice(_)) {
                    if let Some(indent) = stmt_indent {
                        return self.emit_slice_literal_temp(&expr.ty, elements, indent);
                    }
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "slice array literal needs statement lowering in this compiler slice",
                    )]);
                }
                if expr.ty.is_erased_value() {
                    if let Some(indent) = stmt_indent {
                        for element in elements {
                            let value = self.gen_expr_in_stmt(element, indent)?;
                            self.line_indent(indent, &format!("(void)({value});"));
                        }
                    }
                    return Ok("((void)0)".to_string());
                }
                let elements = elements
                    .iter()
                    .map(|element| self.gen_expr_with_lowering(element, stmt_indent))
                    .collect::<DiagResult<Vec<_>>>()?
                    .join(", ");
                format!("{{ {elements} }}")
            }
            TExprKind::ArrayRepeat { element, len } => {
                if matches!(expr.ty, Ty::Slice(_)) {
                    if let Some(indent) = stmt_indent {
                        return self.emit_slice_repeat_temp(&expr.ty, element, *len, indent);
                    }
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "slice array repeat literal needs statement lowering in this compiler slice",
                    )]);
                }
                if expr.ty.is_erased_value() {
                    if let Some(indent) = stmt_indent {
                        let value = self.gen_expr_in_stmt(element, indent)?;
                        self.line_indent(indent, &format!("(void)({value});"));
                    }
                    return Ok("((void)0)".to_string());
                }
                if let Ty::Array { elem, .. } = &expr.ty
                    && let Some(indent) = stmt_indent
                {
                    let temp = self.next_temp("array_repeat");
                    self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &temp)));
                    self.emit_array_repeat_init(&temp, elem, element, *len, indent)?;
                    temp
                } else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "array repeat literal needs statement lowering in this context",
                    )]);
                }
            }
            TExprKind::Closure { id, captures, .. } => {
                self.emit_closure_value(expr, *id, captures, stmt_indent)?
            }
            TExprKind::FunctionToClosure(inner) => {
                self.emit_function_to_closure_value(expr, inner, stmt_indent)?
            }
            TExprKind::RetainClosure {
                expr: inner,
                source_ty,
            } => self.emit_retain_closure_value(expr, inner, source_ty, stmt_indent)?,
            TExprKind::Unary { op, expr } => {
                let inner = self.gen_expr_with_lowering(expr, stmt_indent)?;
                match op {
                    UnaryOp::Not => format!("(!{inner})"),
                    UnaryOp::Neg => {
                        if matches!(expr.kind, TExprKind::Literal(Literal::Integer(_))) {
                            format!("(-{inner})")
                        } else if expr.ty.is_integer()
                            && let Some(helper) = checked_integer_unary_helper(&expr.ty)
                        {
                            let (file, line) = self.location_args(expr.span);
                            format!("{helper}({inner}, {file}, {line})")
                        } else {
                            format!("(-{inner})")
                        }
                    }
                    UnaryOp::Addr => {
                        if let TExprKind::Local(local_id, name) = &expr.kind
                            && self.local_is_heap(*local_id)
                        {
                            self.local_c_name(*local_id, name)
                        } else {
                            format!("(&{inner})")
                        }
                    }
                    UnaryOp::Deref => format!("(*{inner})"),
                }
            }
            TExprKind::Binary { op, left, right } => {
                let op = match op {
                    BinaryOp::Or => "||",
                    BinaryOp::And => "&&",
                    BinaryOp::Eq => "==",
                    BinaryOp::Ne => "!=",
                    BinaryOp::Lt => "<",
                    BinaryOp::Le => "<=",
                    BinaryOp::Gt => ">",
                    BinaryOp::Ge => ">=",
                    BinaryOp::Add => "+",
                    BinaryOp::Sub => "-",
                    BinaryOp::Mul => "*",
                    BinaryOp::Div => "/",
                    BinaryOp::Rem => "%",
                };
                let left_code = self.gen_expr_with_lowering(left, stmt_indent)?;
                let right_code = self.gen_expr_with_lowering(right, stmt_indent)?;
                if left.ty.is_integer()
                    && let Some(helper) = checked_integer_op_helper(op, &left.ty)
                {
                    let (file, line) = self.location_args(expr.span);
                    format!("{helper}({left_code}, {right_code}, {file}, {line})")
                } else if matches!(op, "/" | "%") && left.ty.is_integer() {
                    let helper = checked_integer_op_helper(op, &left.ty).ok_or_else(|| {
                        vec![Diagnostic::new(
                            left.span,
                            format!("no checked integer helper for `{}`", left.ty),
                        )]
                    })?;
                    let (file, line) = self.location_args(expr.span);
                    format!("{helper}({left_code}, {right_code}, {file}, {line})")
                } else {
                    format!("({left_code} {op} {right_code})")
                }
            }
            TExprKind::Cast { expr, ty } => {
                format!(
                    "(({}){})",
                    self.c_type(ty),
                    self.gen_expr_with_lowering(expr, stmt_indent)?
                )
            }
            TExprKind::Call { callee, args, .. } => {
                if matches!(&callee.kind, TExprKind::Function(_, name) if name == "ciel_panic")
                    && args.len() == 2
                {
                    let args = args
                        .iter()
                        .map(|arg| self.gen_expr_with_lowering(arg, stmt_indent))
                        .collect::<DiagResult<Vec<_>>>()?;
                    let (file, line) = self.location_args(expr.span);
                    return Ok(format!(
                        "ciel_panic_at({}, {}, {file}, {line})",
                        args[0], args[1]
                    ));
                }
                if matches!(
                    callee.ty.unqualified(),
                    Ty::Closure { .. } | Ty::ClosureInstance { .. }
                ) {
                    return self.emit_closure_call(callee, args, stmt_indent);
                }
                let callee = self.gen_expr_with_lowering(callee, stmt_indent)?;
                let args = self.gen_call_args(args, stmt_indent)?.join(", ");
                format!("{callee}({args})")
            }
            TExprKind::ArrayToSlice(inner) => {
                let Ty::Slice(elem) = &expr.ty else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "internal error: array-to-slice conversion has non-slice type",
                    )]);
                };
                let Ty::Array { len, .. } = &inner.ty else {
                    return Err(vec![Diagnostic::new(
                        inner.span,
                        "internal error: array-to-slice conversion has non-array source",
                    )]);
                };
                if elem.is_erased_value() {
                    return Ok(format!(
                        "({}){{ .ptr = NULL, .len = {len} }}",
                        self.slice_name(elem)
                    ));
                }
                let inner_code = self.gen_expr_with_lowering(inner, stmt_indent)?;
                format!(
                    "({}){{ .ptr = {inner_code}, .len = {len} }}",
                    self.slice_name(elem)
                )
            }
            TExprKind::MakeDynamicInterface {
                expr: inner,
                concrete_ty,
            } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "dynamic interface conversion needs statement lowering",
                    )]);
                };
                self.emit_dynamic_interface_value(expr, inner, concrete_ty, indent)?
            }
            TExprKind::DynamicInterfaceCall {
                interface_name,
                receiver,
                args,
            } => {
                let receiver_code = self.gen_expr_with_lowering(receiver, stmt_indent)?;
                let receiver_code = if let Some(indent) = stmt_indent {
                    let temp = self.next_temp("dyn_recv");
                    self.line_indent(
                        indent,
                        &format!("{} = {receiver_code};", self.c_decl(&receiver.ty, &temp)),
                    );
                    temp
                } else {
                    receiver_code
                };
                let mut call_args = vec![format!("({receiver_code}).data")];
                call_args.extend(self.gen_call_args(args, stmt_indent)?);
                format!(
                    "({receiver_code}).vtable->{}({})",
                    interface_name,
                    call_args.join(", ")
                )
            }
            TExprKind::RetainedClosureInterfaceCall {
                interface_name,
                interface_args,
                receiver,
                args,
            } => self.emit_retained_closure_interface_call(
                interface_name,
                interface_args,
                receiver,
                args,
                stmt_indent,
            )?,
            TExprKind::Field { base, field } => {
                if expr.ty.is_erased_value() {
                    let base = self.gen_expr_with_lowering(base, stmt_indent)?;
                    if let Some(indent) = stmt_indent {
                        self.line_indent(indent, &format!("(void)({base});"));
                    }
                    return Ok("((void)0)".to_string());
                }
                format!(
                    "({}).{}",
                    self.gen_expr_with_lowering(base, stmt_indent)?,
                    field
                )
            }
            TExprKind::Arrow { base, field } => {
                if expr.ty.is_erased_value() {
                    let base = self.gen_expr_with_lowering(base, stmt_indent)?;
                    if let Some(indent) = stmt_indent {
                        self.line_indent(indent, &format!("(void)({base});"));
                    }
                    return Ok("((void)0)".to_string());
                }
                format!(
                    "({})->{}",
                    self.gen_expr_with_lowering(base, stmt_indent)?,
                    field
                )
            }
            TExprKind::Index { base, index } => {
                let base_code = self.gen_expr_with_lowering(base, stmt_indent)?;
                let index_code = self.gen_expr_with_lowering(index, stmt_indent)?;
                match &base.ty {
                    Ty::Slice(_) => {
                        let (file, line) = self.location_args(expr.span);
                        if expr.ty.is_erased_value() {
                            format!(
                                "((void)({base_code}), (void)ciel_bounds_check((size_t)({index_code}), ({base_code}).len, {file}, {line}), (void)0)"
                            )
                        } else {
                            format!(
                                "({base_code}).ptr[ciel_bounds_check((size_t)({index_code}), ({base_code}).len, {file}, {line})]"
                            )
                        }
                    }
                    Ty::Array { len, .. } => {
                        let (file, line) = self.location_args(expr.span);
                        if expr.ty.is_erased_value() {
                            format!(
                                "((void)({base_code}), (void)ciel_bounds_check((size_t)({index_code}), {len}, {file}, {line}), (void)0)"
                            )
                        } else {
                            format!(
                                "({base_code})[ciel_bounds_check((size_t)({index_code}), {len}, {file}, {line})]"
                            )
                        }
                    }
                    _ => format!("({base_code})[{index_code}]"),
                }
            }
            TExprKind::Slice { base, start, end } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "slice subview needs statement lowering in this context",
                    )]);
                };
                self.emit_slice_subview_temp(expr, base, start.as_deref(), end.as_deref(), indent)?
            }
            TExprKind::Try(inner) => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "`?` needs statement lowering in this context",
                    )]);
                };
                self.emit_try_expr(expr, inner, indent)?
            }
            TExprKind::BuiltinCloneMessage { value, message_ty } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "clone_message needs statement lowering in this context",
                    )]);
                };
                let value_code = self.gen_expr_in_stmt(value, indent)?;
                let value_temp = self.next_temp("message_src");
                self.line_indent(
                    indent,
                    &format!("{} = {value_code};", self.c_decl(&value.ty, &value_temp)),
                );
                self.emit_clone_message_result_from_ptr(message_ty, &value_temp, indent, expr.span)?
            }
            TExprKind::MetaAsRefRepr { value, source_ty } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "as_ref_repr needs statement lowering in this context",
                    )]);
                };
                self.emit_meta_as_ref_repr_expr(expr, value, source_ty, indent)?
            }
            TExprKind::MetaIntoRepr { value, source_ty } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "into_repr needs statement lowering in this context",
                    )]);
                };
                self.emit_meta_into_repr_expr(expr, value, source_ty, indent)?
            }
            TExprKind::MetaFromRepr { value, target_ty } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "from_repr needs statement lowering in this context",
                    )]);
                };
                self.emit_meta_from_repr_expr(expr, value, target_ty, indent)?
            }
            TExprKind::ActorSpawn {
                initial_state,
                handler,
                state_ty,
                message_ty,
                handler_ty,
            } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "spawn_actor needs statement lowering in this context",
                    )]);
                };
                self.emit_actor_spawn_expr(
                    expr,
                    initial_state,
                    handler,
                    state_ty,
                    message_ty,
                    handler_ty,
                    indent,
                )?
            }
            TExprKind::ActorSend {
                actor,
                value,
                message_ty,
            } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "send needs statement lowering in this context",
                    )]);
                };
                self.emit_actor_send_expr(expr, actor, value, message_ty, indent)?
            }
            TExprKind::ActorStop { actor, .. } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "stop needs statement lowering in this context",
                    )]);
                };
                self.emit_actor_lifecycle_expr(expr, actor, "ciel_actor_stop", indent)?
            }
            TExprKind::ActorJoin { actor, .. } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        expr.span,
                        "join needs statement lowering in this context",
                    )]);
                };
                self.emit_actor_lifecycle_expr(expr, actor, "ciel_actor_join", indent)?
            }
            TExprKind::TypeSize { ty } => {
                if ty.is_erased_value() {
                    "0".to_string()
                } else {
                    format!("sizeof({})", self.c_sizeof_type(ty))
                }
            }
            TExprKind::TypeAlign { ty } => {
                if ty.is_erased_value() {
                    "1".to_string()
                } else {
                    format!("CIEL_ALIGNOF({})", self.c_sizeof_type(ty))
                }
            }
        };
        Ok(code)
    }

    fn emit_meta_as_ref_repr_expr(
        &mut self,
        expr: &TExpr,
        value: &TExpr,
        source_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let value_code = self.gen_expr_in_stmt(value, indent)?;
        let value_temp = self.next_temp("meta_ref_src");
        self.line_indent(
            indent,
            &format!("{} = {value_code};", self.c_decl(&value.ty, &value_temp)),
        );
        if let Ok(fields) = self.struct_fields_for_ty(expr.span, source_ty) {
            let fields = fields
                .into_iter()
                .map(|(name, ty)| MetaProductField {
                    value_expr: format!("&({value_temp})->{name}"),
                    name,
                    ty,
                })
                .collect::<Vec<_>>();
            let (_, literal) = self.meta_named_product_literal(&fields, "FieldRef")?;
            return Ok(literal);
        }
        if let Ok(variants) = self.enum_variants_for_ty(expr.span, source_ty) {
            return self.emit_meta_enum_ref_repr(expr, &value_temp, &variants, indent);
        }
        if matches!(source_ty.unqualified(), Ty::ClosureInstance { .. }) {
            return self.emit_meta_closure_ref_repr(expr, &value_temp, source_ty, indent);
        }
        Err(vec![Diagnostic::new(
            expr.span,
            format!("internal error: unsupported as_ref_repr source `{source_ty}`"),
        )])
    }

    fn emit_meta_into_repr_expr(
        &mut self,
        expr: &TExpr,
        value: &TExpr,
        source_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let value_code = self.gen_expr_in_stmt(value, indent)?;
        let value_temp = self.next_temp("meta_owned_src");
        self.line_indent(
            indent,
            &format!("{} = {value_code};", self.c_decl(&value.ty, &value_temp)),
        );
        if let Ok(fields) = self.struct_fields_for_ty(expr.span, source_ty) {
            let fields = fields
                .into_iter()
                .map(|(name, ty)| MetaProductField {
                    value_expr: format!("({value_temp}).{name}"),
                    name,
                    ty,
                })
                .collect::<Vec<_>>();
            let (_, literal) = self.meta_named_product_literal(&fields, "Field")?;
            return Ok(literal);
        }
        if let Ok(variants) = self.enum_variants_for_ty(expr.span, source_ty) {
            return self.emit_meta_enum_owned_repr(expr, &value_temp, &variants, indent);
        }
        if matches!(source_ty.unqualified(), Ty::ClosureInstance { .. }) {
            return self.emit_meta_closure_owned_repr(expr, &value_temp, source_ty, indent);
        }
        Err(vec![Diagnostic::new(
            expr.span,
            format!("internal error: unsupported into_repr source `{source_ty}`"),
        )])
    }

    fn emit_meta_from_repr_expr(
        &mut self,
        expr: &TExpr,
        value: &TExpr,
        target_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let value_code = self.gen_expr_in_stmt(value, indent)?;
        let value_temp = self.next_temp("meta_repr_src");
        self.line_indent(
            indent,
            &format!("{} = {value_code};", self.c_decl(&value.ty, &value_temp)),
        );
        if let Ok(fields) = self.struct_fields_for_ty(expr.span, target_ty) {
            return Ok(self.meta_struct_from_repr_literal(target_ty, &fields, &value_temp));
        }
        if let Ok(variants) = self.enum_variants_for_ty(expr.span, target_ty) {
            return self.emit_meta_enum_from_repr(target_ty, &variants, &value_temp, indent);
        }
        if matches!(target_ty.unqualified(), Ty::ClosureInstance { .. }) {
            return self.emit_meta_closure_from_repr(expr, target_ty, &value_temp, indent);
        }
        Err(vec![Diagnostic::new(
            expr.span,
            format!("internal error: unsupported from_repr target `{target_ty}`"),
        )])
    }

    fn value_initializer_from_expr(&self, ty: &Ty, expr: &str) -> String {
        match ty.unqualified() {
            Ty::Array { len, elem } => {
                let elements = (0..*len)
                    .map(|idx| self.value_initializer_from_expr(elem, &format!("({expr})[{idx}]")))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{ {elements} }}")
            }
            _ => expr.to_string(),
        }
    }

    fn emit_value_copy(&mut self, target: &str, source: &str, ty: &Ty, indent: usize) {
        if matches!(ty.unqualified(), Ty::Array { .. }) {
            self.line_indent(
                indent,
                &format!("memcpy({target}, {source}, sizeof({target}));"),
            );
        } else {
            self.line_indent(indent, &format!("{target} = {source};"));
        }
    }

    fn meta_struct_from_repr_literal(
        &self,
        target_ty: &Ty,
        fields: &[(String, Ty)],
        value_temp: &str,
    ) -> String {
        let struct_name = self.c_type(target_ty);
        let mut emitted = Vec::new();
        let mut cursor = value_temp.to_string();
        for (field, ty) in fields {
            let head = format!("({cursor}).head");
            if !ty.is_erased_value() {
                emitted.push(format!(
                    ".{field} = {}",
                    self.value_initializer_from_expr(ty, &format!("{head}.value"))
                ));
            }
            cursor = format!("({cursor}).tail");
        }
        if emitted.is_empty() {
            format!("({struct_name}){{0}}")
        } else {
            format!("({struct_name}){{ {} }}", emitted.join(", "))
        }
    }

    fn emit_meta_enum_ref_repr(
        &mut self,
        expr: &TExpr,
        source_ptr: &str,
        variants: &[CheckedVariant],
        indent: usize,
    ) -> DiagResult<String> {
        let result = self.next_temp("meta_ref_repr");
        self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &result)));
        self.line_indent(indent, &format!("switch (({source_ptr})->tag) {{"));
        for idx in 0..variants.len() {
            let (_, literal) = self.meta_ref_sum_branch_literal(variants, idx, source_ptr)?;
            self.line_indent(indent + 1, &format!("case {idx}:"));
            self.line_indent(indent + 2, &format!("{result} = {literal};"));
            self.line_indent(indent + 2, "break;");
        }
        self.line_indent(indent + 1, "default:");
        self.line_indent(indent + 2, "ciel_panic(NULL, 0);");
        self.line_indent(
            indent + 2,
            &format!("{result} = {};", self.zero_value(&expr.ty)),
        );
        self.line_indent(indent + 2, "break;");
        self.line_indent(indent, "}");
        Ok(result)
    }

    fn emit_meta_enum_owned_repr(
        &mut self,
        expr: &TExpr,
        source_value: &str,
        variants: &[CheckedVariant],
        indent: usize,
    ) -> DiagResult<String> {
        let result = self.next_temp("meta_owned_repr");
        self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &result)));
        self.line_indent(indent, &format!("switch (({source_value}).tag) {{"));
        for idx in 0..variants.len() {
            let (_, literal) = self.meta_owned_sum_branch_literal(variants, idx, source_value)?;
            self.line_indent(indent + 1, &format!("case {idx}:"));
            self.line_indent(indent + 2, &format!("{result} = {literal};"));
            self.line_indent(indent + 2, "break;");
        }
        self.line_indent(indent + 1, "default:");
        self.line_indent(indent + 2, "ciel_panic(NULL, 0);");
        self.line_indent(
            indent + 2,
            &format!("{result} = {};", self.zero_value(&expr.ty)),
        );
        self.line_indent(indent + 2, "break;");
        self.line_indent(indent, "}");
        Ok(result)
    }

    fn emit_meta_enum_from_repr(
        &mut self,
        target_ty: &Ty,
        variants: &[CheckedVariant],
        value_temp: &str,
        indent: usize,
    ) -> DiagResult<String> {
        let result = self.next_temp("meta_enum_value");
        self.line_indent(indent, &format!("{};", self.c_decl(target_ty, &result)));
        self.emit_meta_enum_from_repr_cases(variants, 0, value_temp, target_ty, &result, indent)?;
        Ok(result)
    }

    fn emit_meta_enum_from_repr_cases(
        &mut self,
        variants: &[CheckedVariant],
        variant_offset: usize,
        cursor: &str,
        target_ty: &Ty,
        result: &str,
        indent: usize,
    ) -> DiagResult<()> {
        if variants.is_empty() {
            self.line_indent(indent, "ciel_panic(NULL, 0);");
            self.line_indent(
                indent,
                &format!("{result} = {};", self.zero_value(target_ty)),
            );
            return Ok(());
        }
        self.line_indent(indent, &format!("switch (({cursor}).tag) {{"));
        self.line_indent(indent + 1, "case 0:");
        let literal =
            self.meta_enum_value_from_variant_repr(target_ty, &variants[0], variant_offset, cursor);
        self.line_indent(indent + 2, &format!("{result} = {literal};"));
        self.line_indent(indent + 2, "break;");
        self.line_indent(indent + 1, "case 1:");
        if variants.len() == 1 {
            self.line_indent(indent + 2, "ciel_panic(NULL, 0);");
            self.line_indent(
                indent + 2,
                &format!("{result} = {};", self.zero_value(target_ty)),
            );
        } else {
            let tail = format!("({cursor}).as.Next._0");
            self.emit_meta_enum_from_repr_cases(
                &variants[1..],
                variant_offset + 1,
                &tail,
                target_ty,
                result,
                indent + 2,
            )?;
        }
        self.line_indent(indent + 2, "break;");
        self.line_indent(indent + 1, "default:");
        self.line_indent(indent + 2, "ciel_panic(NULL, 0);");
        self.line_indent(
            indent + 2,
            &format!("{result} = {};", self.zero_value(target_ty)),
        );
        self.line_indent(indent + 2, "break;");
        self.line_indent(indent, "}");
        Ok(())
    }

    fn meta_enum_value_from_variant_repr(
        &self,
        target_ty: &Ty,
        variant: &CheckedVariant,
        variant_index: usize,
        cursor: &str,
    ) -> String {
        let target_name = self.c_type(target_ty);
        let mut payload_fields = Vec::new();
        let mut payload_cursor = format!("(({cursor}).as.This._0).payload");
        for (idx, ty) in variant.payload.iter().enumerate() {
            if !ty.is_erased_value() {
                payload_fields.push(format!(
                    "._{idx} = {}",
                    self.value_initializer_from_expr(ty, &format!("({payload_cursor}).head.value"))
                ));
            }
            payload_cursor = format!("({payload_cursor}).tail");
        }
        if payload_fields.is_empty() {
            format!("({target_name}){{ .tag = {variant_index} }}")
        } else {
            format!(
                "({target_name}){{ .tag = {variant_index}, .as.{} = {{ {} }} }}",
                variant.name,
                payload_fields.join(", ")
            )
        }
    }

    fn emit_meta_closure_ref_repr(
        &mut self,
        expr: &TExpr,
        source_ptr: &str,
        source_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let captures = self.meta_capture_fields_for_ty(expr.span, source_ty)?;
        if captures.is_empty() {
            let (_, literal) = self.meta_named_product_literal(&[], "FieldRef")?;
            return Ok(literal);
        }
        let (owner, id) = self.closure_instance_owner_id(expr.span, source_ty)?;
        let env_name = self.closure_env_name(owner, id);
        let env_temp = self.next_temp("meta_ref_env");
        self.line_indent(
            indent,
            &format!("{env_name} *{env_temp} = ({env_name} *)({source_ptr})->env;"),
        );
        let fields = captures
            .into_iter()
            .map(|capture| MetaProductField {
                name: format!("capture#{}", capture.index),
                ty: capture.ty,
                value_expr: format!("&({env_temp})->cap{}", capture.index),
            })
            .collect::<Vec<_>>();
        let (_, literal) = self.meta_named_product_literal(&fields, "FieldRef")?;
        Ok(literal)
    }

    fn emit_meta_closure_owned_repr(
        &mut self,
        expr: &TExpr,
        source_value: &str,
        source_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let captures = self.meta_capture_fields_for_ty(expr.span, source_ty)?;
        if captures.is_empty() {
            let (_, literal) = self.meta_named_product_literal(&[], "Field")?;
            return Ok(literal);
        }
        let (owner, id) = self.closure_instance_owner_id(expr.span, source_ty)?;
        let env_name = self.closure_env_name(owner, id);
        let env_temp = self.next_temp("meta_owned_env");
        self.line_indent(
            indent,
            &format!("{env_name} *{env_temp} = ({env_name} *)({source_value}).env;"),
        );
        let fields = captures
            .into_iter()
            .map(|capture| MetaProductField {
                name: format!("capture#{}", capture.index),
                ty: capture.ty,
                value_expr: format!("({env_temp})->cap{}", capture.index),
            })
            .collect::<Vec<_>>();
        let (_, literal) = self.meta_named_product_literal(&fields, "Field")?;
        Ok(literal)
    }

    fn emit_meta_closure_from_repr(
        &mut self,
        expr: &TExpr,
        target_ty: &Ty,
        value_temp: &str,
        indent: usize,
    ) -> DiagResult<String> {
        let captures = self.meta_capture_fields_for_ty(expr.span, target_ty)?;
        let (owner, id) = self.closure_instance_owner_id(expr.span, target_ty)?;
        if captures.is_empty() {
            return Ok(format!(
                "({}){{ .call = {}, .env = NULL }}",
                self.c_type(target_ty),
                self.closure_thunk_name(owner, id)
            ));
        }
        let env_name = self.closure_env_name(owner, id);
        let env_temp = self.next_temp("meta_closure_env");
        self.line_indent(
            indent,
            &format!("{env_name} *{env_temp} = ({env_name} *)ciel_alloc(sizeof({env_name}));"),
        );
        let mut cursor = value_temp.to_string();
        for capture in captures {
            self.emit_value_copy(
                &format!("{env_temp}->cap{}", capture.index),
                &format!("({cursor}).head.value"),
                &capture.ty,
                indent,
            );
            cursor = format!("({cursor}).tail");
        }
        Ok(format!(
            "({}){{ .call = {}, .env = (void *){} }}",
            self.c_type(target_ty),
            self.closure_thunk_name(owner, id),
            env_temp
        ))
    }

    fn meta_named_product_literal(
        &self,
        fields: &[MetaProductField],
        head_name: &str,
    ) -> DiagResult<(Ty, String)> {
        let Some((field, rest)) = fields.split_first() else {
            let ty = meta_named("HNil", Vec::new());
            return Ok((ty.clone(), format!("({}){{0}}", self.c_type(&ty))));
        };
        let (tail_ty, tail) = self.meta_named_product_literal(rest, head_name)?;
        let head_ty = meta_named(head_name, vec![field.ty.clone()]);
        let ty = meta_named("HCons", vec![head_ty.clone(), tail_ty]);
        let mut head_fields = vec![format!(
            ".name = {}",
            self.meta_name_slice_literal(&field.name)
        )];
        if field.ty.is_erased_value() {
            if head_name == "FieldRef" {
                return Err(vec![Diagnostic::new(
                    None,
                    format!(
                        "internal error: cannot borrow erased meta field `{}`",
                        field.name
                    ),
                )]);
            }
        } else {
            let value = if head_name == "FieldRef" {
                field.value_expr.clone()
            } else {
                self.value_initializer_from_expr(&field.ty, &field.value_expr)
            };
            head_fields.push(format!(".value = {}", value));
        }
        let head = format!(
            "({}){{ {} }}",
            self.c_type(&head_ty),
            head_fields.join(", ")
        );
        Ok((
            ty.clone(),
            format!("({}){{ .head = {head}, .tail = {tail} }}", self.c_type(&ty)),
        ))
    }

    fn meta_payload_product_literal(
        &self,
        fields: &[MetaPayloadField],
        head_name: &str,
    ) -> DiagResult<(Ty, String)> {
        let Some((field, rest)) = fields.split_first() else {
            let ty = meta_named("HNil", Vec::new());
            return Ok((ty.clone(), format!("({}){{0}}", self.c_type(&ty))));
        };
        let (tail_ty, tail) = self.meta_payload_product_literal(rest, head_name)?;
        let head_ty = meta_named(head_name, vec![field.ty.clone()]);
        let ty = meta_named("HCons", vec![head_ty.clone(), tail_ty]);
        let value = if head_name == "PayloadRef" {
            field.value_expr.clone()
        } else {
            self.value_initializer_from_expr(&field.ty, &field.value_expr)
        };
        let head = format!(
            "({}){{ .index = {}, .value = {} }}",
            self.c_type(&head_ty),
            field.index,
            value
        );
        Ok((
            ty.clone(),
            format!("({}){{ .head = {head}, .tail = {tail} }}", self.c_type(&ty)),
        ))
    }

    fn meta_ref_sum_branch_literal(
        &self,
        variants: &[CheckedVariant],
        active_idx: usize,
        source_ptr: &str,
    ) -> DiagResult<(Ty, String)> {
        self.meta_sum_branch_literal(
            variants,
            active_idx,
            |variant| {
                variant
                    .payload
                    .iter()
                    .enumerate()
                    .map(|(idx, ty)| MetaPayloadField {
                        index: idx,
                        ty: ty.clone(),
                        value_expr: format!("&({source_ptr})->as.{}._{idx}", variant.name),
                    })
                    .collect::<Vec<_>>()
            },
            true,
        )
    }

    fn meta_owned_sum_branch_literal(
        &self,
        variants: &[CheckedVariant],
        active_idx: usize,
        source_value: &str,
    ) -> DiagResult<(Ty, String)> {
        self.meta_sum_branch_literal(
            variants,
            active_idx,
            |variant| {
                variant
                    .payload
                    .iter()
                    .enumerate()
                    .map(|(idx, ty)| MetaPayloadField {
                        index: idx,
                        ty: ty.clone(),
                        value_expr: format!("({source_value}).as.{}._{idx}", variant.name),
                    })
                    .collect::<Vec<_>>()
            },
            false,
        )
    }

    fn meta_sum_branch_literal<F>(
        &self,
        variants: &[CheckedVariant],
        active_idx: usize,
        payloads_for: F,
        borrowed: bool,
    ) -> DiagResult<(Ty, String)>
    where
        F: Fn(&CheckedVariant) -> Vec<MetaPayloadField> + Copy,
    {
        let Some((variant, rest)) = variants.split_first() else {
            return Err(vec![Diagnostic::new(
                None,
                "internal error: cannot construct meta CoNil branch",
            )]);
        };
        let payload_head = if borrowed { "PayloadRef" } else { "Payload" };
        let variant_head = if borrowed { "VariantRef" } else { "Variant" };
        let payloads = payloads_for(variant);
        let (payload_ty, payload_literal) =
            self.meta_payload_product_literal(&payloads, payload_head)?;
        let head_ty = meta_named(variant_head, vec![payload_ty]);
        let tail_ty = meta_sum_ty(rest.iter().map(|variant| variant.payload.clone()), borrowed);
        let ty = meta_named("Coproduct", vec![head_ty.clone(), tail_ty]);
        if active_idx == 0 {
            let head = format!(
                "({}){{ .name = {}, .payload = {payload_literal} }}",
                self.c_type(&head_ty),
                self.meta_name_slice_literal(&variant.name)
            );
            Ok((
                ty.clone(),
                format!(
                    "({}){{ .tag = 0, .as.This = {{ ._0 = {head} }} }}",
                    self.c_type(&ty)
                ),
            ))
        } else {
            let (_, tail_literal) =
                self.meta_sum_branch_literal(rest, active_idx - 1, payloads_for, borrowed)?;
            Ok((
                ty.clone(),
                format!(
                    "({}){{ .tag = 1, .as.Next = {{ ._0 = {tail_literal} }} }}",
                    self.c_type(&ty)
                ),
            ))
        }
    }

    fn meta_name_slice_literal(&self, name: &str) -> String {
        format!(
            "({}){{ .ptr = \"{}\", .len = {} }}",
            self.slice_name(&Ty::Char),
            escape_c_string(name),
            name.len()
        )
    }

    fn struct_fields_for_ty(
        &self,
        span: crate::span::Span,
        ty: &Ty,
    ) -> DiagResult<Vec<(String, Ty)>> {
        let Ty::Named { name, args } = ty.unqualified() else {
            return Err(vec![Diagnostic::new(
                span,
                format!("internal error: expected struct type for meta representation, got `{ty}`"),
            )]);
        };
        let c_name = self.c_named_type(name, args);
        self.program
            .checked
            .structs
            .iter()
            .find(|strukt| strukt.name == c_name)
            .map(|strukt| strukt.fields.clone())
            .ok_or_else(|| {
                vec![Diagnostic::new(
                    span,
                    format!(
                        "internal error: missing struct layout `{c_name}` for meta representation"
                    ),
                )]
            })
    }

    fn enum_variants_for_ty(
        &self,
        span: crate::span::Span,
        ty: &Ty,
    ) -> DiagResult<Vec<CheckedVariant>> {
        let Ty::Named { name, args } = ty.unqualified() else {
            return Err(vec![Diagnostic::new(
                span,
                format!("internal error: expected enum type for meta representation, got `{ty}`"),
            )]);
        };
        let c_name = self.c_named_type(name, args);
        self.program
            .checked
            .enums
            .iter()
            .find(|enm| enm.name == c_name)
            .map(|enm| enm.variants.clone())
            .ok_or_else(|| {
                vec![Diagnostic::new(
                    span,
                    format!(
                        "internal error: missing enum layout `{c_name}` for meta representation"
                    ),
                )]
            })
    }

    fn meta_capture_fields_for_ty(
        &self,
        span: crate::span::Span,
        ty: &Ty,
    ) -> DiagResult<Vec<MetaCaptureField>> {
        let Ty::ClosureInstance { captures, .. } = ty.unqualified() else {
            return Err(vec![Diagnostic::new(
                span,
                format!(
                    "internal error: expected concrete closure type for meta representation, got `{ty}`"
                ),
            )]);
        };
        Ok(captures
            .iter()
            .enumerate()
            .filter(|(_, ty)| !ty.is_erased_value())
            .map(|(index, ty)| MetaCaptureField {
                index,
                ty: ty.clone(),
            })
            .collect())
    }

    fn closure_instance_owner_id(
        &self,
        span: crate::span::Span,
        ty: &Ty,
    ) -> DiagResult<(DefId, usize)> {
        let Ty::ClosureInstance { id, .. } = ty.unqualified() else {
            return Err(vec![Diagnostic::new(
                span,
                format!("internal error: expected concrete closure type, got `{ty}`"),
            )]);
        };
        let matches = self
            .plan
            .closure_defs
            .values()
            .filter(|closure| closure.id == *id && closure.ty.unqualified() == ty.unqualified())
            .collect::<Vec<_>>();
        if let Some(owner) = self.current_closure_owner
            && let Some(closure) = matches.iter().find(|closure| closure.owner == owner)
        {
            return Ok((closure.owner, closure.id));
        }
        matches
            .first()
            .map(|closure| (closure.owner, closure.id))
            .ok_or_else(|| {
                vec![Diagnostic::new(
                    span,
                    format!("internal error: missing closure metadata for `{ty}`"),
                )]
            })
    }

    fn gen_literal(&mut self, span: crate::span::Span, literal: &Literal, ty: &Ty) -> String {
        match literal {
            Literal::Integer(raw) | Literal::Float(raw) => raw.replace('_', ""),
            Literal::Char(raw) => raw.clone(),
            Literal::Bool(value) => {
                if *value {
                    "true".to_string()
                } else {
                    "false".to_string()
                }
            }
            Literal::Null => "NULL".to_string(),
            Literal::String(raw) => {
                let len = string_literal_len(raw);
                let slice = self.slice_name(match ty {
                    Ty::Slice(elem) => elem,
                    _ => &Ty::Char,
                });
                let name = self
                    .plan
                    .string_literal_names
                    .get(&span_key(span))
                    .cloned()
                    .unwrap_or_else(|| raw.clone());
                format!("({slice}){{ .ptr = {name}, .len = {len} }}")
            }
        }
    }

    fn emit_try_expr(&mut self, expr: &TExpr, inner: &TExpr, indent: usize) -> DiagResult<String> {
        let inner_layout = self.result_layout(&inner.ty, inner.span)?;
        let return_ty = self.current_return_ty.clone();
        let return_layout = self.result_layout(&return_ty, expr.span)?;
        let value = self.gen_expr_in_stmt(inner, indent)?;
        let temp = self.next_temp("try");
        self.line_indent(
            indent,
            &format!("{} {temp} = {value};", inner_layout.c_type),
        );
        self.line_indent(
            indent,
            &format!("if ({temp}.tag == {}) {{", inner_layout.err_index),
        );
        self.emit_all_defers(indent + 1);
        self.line_indent(
            indent + 1,
            &format!(
                "return {};",
                self.result_err_literal(&return_layout, &inner_layout, &temp)
            ),
        );
        self.line_indent(indent, "}");
        if expr.ty.is_erased_value() || !inner_layout.ok_has_payload {
            Ok("((void)0)".to_string())
        } else {
            Ok(format!("{temp}.as.{}._0", inner_layout.ok_name))
        }
    }

    fn result_layout(&self, ty: &Ty, span: crate::span::Span) -> DiagResult<ResultLayout> {
        let Ty::Named { name, args } = ty else {
            return Err(vec![Diagnostic::new(
                span,
                format!("internal error: expected Result type, got `{ty}`"),
            )]);
        };
        let c_type = self.c_named_type(name, args);
        let Some(enm) = self
            .program
            .checked
            .enums
            .iter()
            .find(|enm| enm.name == c_type)
        else {
            return Err(vec![Diagnostic::new(
                span,
                format!("internal error: missing enum layout for `{ty}`"),
            )]);
        };
        let Some((ok_index, ok_variant)) = enm
            .variants
            .iter()
            .enumerate()
            .find(|(_, variant)| variant.name == "Ok")
        else {
            return Err(vec![Diagnostic::new(
                span,
                format!(
                    "internal error: Result layout `{}` has no Ok variant",
                    enm.name
                ),
            )]);
        };
        let Some((err_index, err_variant)) = enm
            .variants
            .iter()
            .enumerate()
            .find(|(_, variant)| variant.name == "Err")
        else {
            return Err(vec![Diagnostic::new(
                span,
                format!(
                    "internal error: Result layout `{}` has no Err variant",
                    enm.name
                ),
            )]);
        };
        Ok(ResultLayout {
            c_type,
            ok_index,
            ok_name: ok_variant.name.clone(),
            ok_has_payload: !ok_variant.payload.is_empty(),
            err_name: err_variant.name.clone(),
            err_index,
            err_has_payload: !err_variant.payload.is_empty(),
        })
    }

    fn result_err_literal(
        &self,
        return_layout: &ResultLayout,
        inner_layout: &ResultLayout,
        temp: &str,
    ) -> String {
        if return_layout.err_has_payload {
            format!(
                "({}){{ .tag = {}, .as.{} = {{ ._0 = {temp}.as.{}._0 }} }}",
                return_layout.c_type,
                return_layout.err_index,
                return_layout.err_name,
                inner_layout.err_name
            )
        } else {
            format!(
                "({}){{ .tag = {} }}",
                return_layout.c_type, return_layout.err_index
            )
        }
    }

    fn result_ok_literal(&self, layout: &ResultLayout, value: Option<&str>) -> String {
        if layout.ok_has_payload {
            format!(
                "({}){{ .tag = {}, .as.{} = {{ ._0 = {} }} }}",
                layout.c_type,
                layout.ok_index,
                layout.ok_name,
                value.unwrap_or("0")
            )
        } else {
            format!("({}){{ .tag = {} }}", layout.c_type, layout.ok_index)
        }
    }

    fn result_err_from_error_literal(&self, layout: &ResultLayout, error: &str) -> String {
        if layout.err_has_payload {
            format!(
                "({}){{ .tag = {}, .as.{} = {{ ._0 = {error} }} }}",
                layout.c_type, layout.err_index, layout.err_name
            )
        } else {
            format!("({}){{ .tag = {} }}", layout.c_type, layout.err_index)
        }
    }

    fn error_code_literal(&self, code: &str) -> String {
        let c_type = self.c_named_type("Error", &[]);
        let (variant_index, variant_name) = self
            .program
            .checked
            .enums
            .iter()
            .find(|enm| enm.name == c_type)
            .and_then(|enm| {
                enm.variants
                    .iter()
                    .enumerate()
                    .find(|(_, variant)| variant.name == "Code")
                    .map(|(idx, variant)| (idx, variant.name.clone()))
            })
            .unwrap_or_else(|| (1, "Code".to_string()));
        format!(
            "({c_type}){{ .tag = {variant_index}, .as.{variant_name} = {{ ._0 = (int64_t)({code}) }} }}"
        )
    }

    fn emit_clone_message_result_from_ptr(
        &mut self,
        message_ty: &Ty,
        source_ptr: &str,
        indent: usize,
        span: crate::span::Span,
    ) -> DiagResult<String> {
        let result_ty = std_result_ty(message_ty.clone(), std_error_ty());
        let result_layout = self.result_layout(&result_ty, span)?;
        let result_temp = self.next_temp("clone_result");
        let done_label = self.next_temp("clone_done");
        self.line_indent(
            indent,
            &format!("{};", self.c_decl(&result_ty, &result_temp)),
        );
        if let Some(function_def) = self
            .clone_message_impl(message_ty)
            .map(|implementation| implementation.function_def)
        {
            self.line_indent(
                indent,
                &format!(
                    "{result_temp} = {}({source_ptr});",
                    self.c_name(function_def)
                ),
            );
            return Ok(result_temp);
        }
        if result_layout.ok_has_payload {
            let target = format!("{result_temp}.as.{}._0", result_layout.ok_name);
            self.emit_clone_value_into(
                message_ty,
                source_ptr,
                &target,
                &result_temp,
                &result_layout,
                &done_label,
                indent,
                span,
            )?;
            self.line_indent(
                indent,
                &format!("{result_temp}.tag = {};", result_layout.ok_index),
            );
        } else {
            self.line_indent(
                indent,
                &format!(
                    "{result_temp} = {};",
                    self.result_ok_literal(&result_layout, None)
                ),
            );
        }
        self.line_indent(indent, &format!("{done_label}:;"));
        Ok(result_temp)
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_clone_value_into(
        &mut self,
        ty: &Ty,
        source_ptr: &str,
        target: &str,
        result_temp: &str,
        result_layout: &ResultLayout,
        done_label: &str,
        indent: usize,
        span: crate::span::Span,
    ) -> DiagResult<()> {
        let ty = ty.unqualified();
        if let Some(function_def) = self
            .clone_message_impl(ty)
            .map(|implementation| implementation.function_def)
        {
            let clone_result_ty = std_result_ty(ty.clone(), std_error_ty());
            let clone_layout = self.result_layout(&clone_result_ty, span)?;
            let clone_temp = self.next_temp("field_clone");
            self.line_indent(
                indent,
                &format!(
                    "{} {clone_temp} = {}({source_ptr});",
                    clone_layout.c_type,
                    self.c_name(function_def)
                ),
            );
            self.line_indent(
                indent,
                &format!("if ({clone_temp}.tag == {}) {{", clone_layout.err_index),
            );
            self.line_indent(
                indent + 1,
                &format!(
                    "{result_temp} = {};",
                    self.result_err_literal(result_layout, &clone_layout, &clone_temp)
                ),
            );
            self.line_indent(indent + 1, &format!("goto {done_label};"));
            self.line_indent(indent, "}");
            self.line_indent(
                indent,
                &format!("{target} = {clone_temp}.as.{}._0;", clone_layout.ok_name),
            );
            return Ok(());
        }

        match ty {
            Ty::Const(inner) => self.emit_clone_value_into(
                inner,
                source_ptr,
                target,
                result_temp,
                result_layout,
                done_label,
                indent,
                span,
            ),
            Ty::Void => Ok(()),
            Ty::Bool
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
            | Ty::Function { abi: None, .. } => {
                self.line_indent(indent, &format!("{target} = (*({source_ptr}));"));
                Ok(())
            }
            Ty::Array { len, elem } => {
                let idx = self.next_temp("clone_i");
                self.line_indent(
                    indent,
                    &format!("for (size_t {idx} = 0; {idx} < {len}; {idx}++) {{"),
                );
                let source = format!("&((*({source_ptr}))[{idx}])");
                let target = format!("({target})[{idx}]");
                self.emit_clone_value_into(
                    elem,
                    &source,
                    &target,
                    result_temp,
                    result_layout,
                    done_label,
                    indent + 1,
                    span,
                )?;
                self.line_indent(indent, "}");
                Ok(())
            }
            Ty::Named { name, args } => {
                let instance_name = self.c_named_type(name, args);
                if let Some(fields) = self
                    .program
                    .checked
                    .structs
                    .iter()
                    .find(|strukt| strukt.name == instance_name)
                    .map(|strukt| strukt.fields.clone())
                {
                    for (field, field_ty) in fields {
                        let source = format!("&((*({source_ptr})).{field})");
                        let target = format!("({target}).{field}");
                        self.emit_clone_value_into(
                            &field_ty,
                            &source,
                            &target,
                            result_temp,
                            result_layout,
                            done_label,
                            indent,
                            span,
                        )?;
                    }
                    return Ok(());
                }
                if let Some(enm) = self
                    .program
                    .checked
                    .enums
                    .iter()
                    .find(|enm| enm.name == instance_name)
                    .cloned()
                {
                    self.line_indent(indent, &format!("({target}).tag = (*({source_ptr})).tag;"));
                    self.line_indent(indent, &format!("switch ((*({source_ptr})).tag) {{"));
                    for (variant_index, variant) in enm.variants.iter().enumerate() {
                        self.line_indent(indent, &format!("case {variant_index}:"));
                        for (idx, payload_ty) in variant.payload.iter().enumerate() {
                            let source = format!("&((*({source_ptr})).as.{}._{idx})", variant.name);
                            let target = format!("({target}).as.{}._{idx}", variant.name);
                            self.emit_clone_value_into(
                                payload_ty,
                                &source,
                                &target,
                                result_temp,
                                result_layout,
                                done_label,
                                indent + 1,
                                span,
                            )?;
                        }
                        self.line_indent(indent + 1, "break;");
                    }
                    self.line_indent(indent, "}");
                    return Ok(());
                }
                Err(vec![Diagnostic::new(
                    span,
                    format!("internal error: cannot clone message type `{ty}`"),
                )])
            }
            Ty::ClosureInstance { .. } => self.emit_clone_closure_value_into(
                ty,
                source_ptr,
                target,
                result_temp,
                result_layout,
                done_label,
                indent,
                span,
            ),
            Ty::Closure { constraints, .. }
                if constraints.positive.iter().any(is_clone_message_capability) =>
            {
                let capability = ConstraintRef {
                    name: "clone_message".to_string(),
                    args: Vec::new(),
                };
                let field = self.retained_closure_witness_field_name(&capability);
                let clone_result_ty = std_result_ty(ty.clone(), std_error_ty());
                let clone_layout = self.result_layout(&clone_result_ty, span)?;
                let clone_temp = self.next_temp("closure_clone");
                self.line_indent(
                    indent,
                    &format!(
                        "{} {clone_temp} = (*({source_ptr})).{field}((void *)({source_ptr}));",
                        clone_layout.c_type
                    ),
                );
                self.line_indent(
                    indent,
                    &format!("if ({clone_temp}.tag == {}) {{", clone_layout.err_index),
                );
                self.line_indent(
                    indent + 1,
                    &format!(
                        "{result_temp} = {};",
                        self.result_err_literal(result_layout, &clone_layout, &clone_temp)
                    ),
                );
                self.line_indent(indent + 1, &format!("goto {done_label};"));
                self.line_indent(indent, "}");
                self.line_indent(
                    indent,
                    &format!("{target} = {clone_temp}.as.{}._0;", clone_layout.ok_name),
                );
                Ok(())
            }
            _ => Err(vec![Diagnostic::new(
                span,
                format!("internal error: cannot clone non-message type `{ty}`"),
            )]),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_clone_closure_value_into(
        &mut self,
        closure_ty: &Ty,
        source_ptr: &str,
        target: &str,
        result_temp: &str,
        result_layout: &ResultLayout,
        done_label: &str,
        indent: usize,
        span: crate::span::Span,
    ) -> DiagResult<()> {
        self.line_indent(
            indent,
            &format!("({target}).call = (*({source_ptr})).call;"),
        );
        let Ty::ClosureInstance {
            id: _, captures, ..
        } = closure_ty.unqualified()
        else {
            return Err(vec![Diagnostic::new(
                span,
                format!("internal error: expected concrete closure type, got `{closure_ty}`"),
            )]);
        };
        if captures.is_empty() {
            self.line_indent(indent, &format!("({target}).env = NULL;"));
            return Ok(());
        }
        let (owner, closure_id) = self.closure_instance_owner_id(span, closure_ty)?;
        let env_name = self.closure_env_name(owner, closure_id);
        let old_env = self.next_temp("closure_env_old");
        let new_env = self.next_temp("closure_env_new");
        self.line_indent(
            indent,
            &format!("{env_name} *{old_env} = ({env_name} *)(*({source_ptr})).env;"),
        );
        self.line_indent(
            indent,
            &format!("{env_name} *{new_env} = ({env_name} *)ciel_alloc(sizeof({env_name}));"),
        );
        for (idx, capture_ty) in captures.iter().enumerate() {
            let source = format!("&({old_env}->cap{idx})");
            let target = format!("{new_env}->cap{idx}");
            self.emit_clone_value_into(
                capture_ty,
                &source,
                &target,
                result_temp,
                result_layout,
                done_label,
                indent,
                span,
            )?;
        }
        self.line_indent(indent, &format!("({target}).env = (void *){new_env};"));
        Ok(())
    }

    fn clone_message_impl(&self, ty: &Ty) -> Option<&CheckedImpl> {
        self.program.checked.impls.iter().find(|implementation| {
            implementation.interface_name == "clone_message"
                && implementation
                    .receiver_ty
                    .as_ref()
                    .is_some_and(|receiver| receiver == ty.unqualified())
                && implementation.interface_args.get(1..) == Some(&[][..])
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_actor_spawn_expr(
        &mut self,
        expr: &TExpr,
        initial_state: &TExpr,
        handler: &TExpr,
        state_ty: &Ty,
        message_ty: &Ty,
        handler_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let result_layout = self.result_layout(&expr.ty, expr.span)?;
        let result_temp = self.next_temp("actor_spawn_result");
        let done_label = self.next_temp("actor_spawn_done");
        self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &result_temp)));

        let state_value = self.gen_expr_in_stmt(initial_state, indent)?;
        let state_src = self.next_temp("actor_state_src");
        self.line_indent(
            indent,
            &format!("{} = {state_value};", self.c_decl(state_ty, &state_src)),
        );
        let state_clone = self.emit_clone_message_result_from_ptr(
            state_ty,
            &format!("&{state_src}"),
            indent,
            expr.span,
        )?;
        self.emit_clone_error_jump(
            &result_temp,
            &result_layout,
            &state_clone,
            state_ty,
            &done_label,
            indent,
            expr.span,
        )?;
        let state_box = self.next_temp("actor_state_box");
        self.line_indent(
            indent,
            &format!(
                "{} = ciel_alloc(sizeof(*{state_box}));",
                self.c_pointer_decl(state_ty, &state_box)
            ),
        );
        let state_clone_layout =
            self.result_layout(&std_result_ty(state_ty.clone(), std_error_ty()), expr.span)?;
        self.line_indent(
            indent,
            &format!(
                "*{state_box} = {state_clone}.as.{}._0;",
                state_clone_layout.ok_name
            ),
        );

        let handler_value = self.gen_expr_in_stmt(handler, indent)?;
        let handler_src = self.next_temp("actor_handler_src");
        self.line_indent(
            indent,
            &format!(
                "{} = {handler_value};",
                self.c_decl(handler_ty, &handler_src)
            ),
        );
        let handler_clone = self.emit_clone_message_result_from_ptr(
            handler_ty,
            &format!("&{handler_src}"),
            indent,
            expr.span,
        )?;
        self.emit_clone_error_jump(
            &result_temp,
            &result_layout,
            &handler_clone,
            handler_ty,
            &done_label,
            indent,
            expr.span,
        )?;
        let handler_box = self.next_temp("actor_handler_box");
        self.line_indent(
            indent,
            &format!(
                "{} = ciel_alloc(sizeof(*{handler_box}));",
                self.c_pointer_decl(handler_ty, &handler_box)
            ),
        );
        let handler_clone_layout = self.result_layout(
            &std_result_ty(handler_ty.clone(), std_error_ty()),
            expr.span,
        )?;
        self.line_indent(
            indent,
            &format!(
                "*{handler_box} = {handler_clone}.as.{}._0;",
                handler_clone_layout.ok_name
            ),
        );

        let raw_actor = self.next_temp("actor_raw");
        let rc = self.next_temp("actor_rc");
        let dispatch = self.actor_dispatch_name(state_ty, message_ty, handler_ty);
        self.line_indent(indent, &format!("CielActor *{raw_actor} = NULL;"));
        self.line_indent(
            indent,
            &format!(
                "int32_t {rc} = ciel_actor_spawn(&{raw_actor}, (void *){state_box}, (void *){handler_box}, {dispatch});"
            ),
        );
        self.line_indent(indent, &format!("if ({rc} != 0) {{"));
        self.line_indent(
            indent + 1,
            &format!(
                "{result_temp} = {};",
                self.result_err_from_error_literal(&result_layout, &self.error_code_literal(&rc))
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done_label};"));
        self.line_indent(indent, "}");
        let actor_ty = Ty::Named {
            name: "Actor".to_string(),
            args: vec![message_ty.clone()],
        };
        let actor_value = format!(
            "({}){{ .handle = (void *){raw_actor} }}",
            self.c_type(&actor_ty)
        );
        self.line_indent(
            indent,
            &format!(
                "{result_temp} = {};",
                self.result_ok_literal(&result_layout, Some(&actor_value))
            ),
        );
        self.line_indent(indent, &format!("{done_label}:;"));
        Ok(result_temp)
    }

    fn emit_actor_send_expr(
        &mut self,
        expr: &TExpr,
        actor: &TExpr,
        value: &TExpr,
        message_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let result_layout = self.result_layout(&expr.ty, expr.span)?;
        let result_temp = self.next_temp("actor_send_result");
        let done_label = self.next_temp("actor_send_done");
        self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &result_temp)));

        let value_code = self.gen_expr_in_stmt(value, indent)?;
        let value_src = self.next_temp("actor_msg_src");
        self.line_indent(
            indent,
            &format!("{} = {value_code};", self.c_decl(message_ty, &value_src)),
        );
        let clone_result = self.emit_clone_message_result_from_ptr(
            message_ty,
            &format!("&{value_src}"),
            indent,
            expr.span,
        )?;
        self.emit_clone_error_jump(
            &result_temp,
            &result_layout,
            &clone_result,
            message_ty,
            &done_label,
            indent,
            expr.span,
        )?;
        let clone_layout = self.result_layout(
            &std_result_ty(message_ty.clone(), std_error_ty()),
            expr.span,
        )?;
        let msg_box = self.next_temp("actor_msg_box");
        self.line_indent(
            indent,
            &format!(
                "{} = ciel_alloc(sizeof(*{msg_box}));",
                self.c_pointer_decl(message_ty, &msg_box)
            ),
        );
        self.line_indent(
            indent,
            &format!(
                "*{msg_box} = {clone_result}.as.{}._0;",
                clone_layout.ok_name
            ),
        );
        let handle = self.emit_actor_handle(actor, indent)?;
        let rc = self.next_temp("actor_send_rc");
        self.line_indent(
            indent,
            &format!("int32_t {rc} = ciel_actor_send({handle}, (void *){msg_box});"),
        );
        self.emit_runtime_result_from_rc(&result_temp, &result_layout, &rc, &done_label, indent);
        self.line_indent(indent, &format!("{done_label}:;"));
        Ok(result_temp)
    }

    fn emit_actor_lifecycle_expr(
        &mut self,
        expr: &TExpr,
        actor: &TExpr,
        runtime_fn: &str,
        indent: usize,
    ) -> DiagResult<String> {
        let result_layout = self.result_layout(&expr.ty, expr.span)?;
        let result_temp = self.next_temp("actor_lifecycle_result");
        let done_label = self.next_temp("actor_lifecycle_done");
        self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &result_temp)));
        let handle = self.emit_actor_handle(actor, indent)?;
        let rc = self.next_temp("actor_lifecycle_rc");
        self.line_indent(indent, &format!("int32_t {rc} = {runtime_fn}({handle});"));
        self.emit_runtime_result_from_rc(&result_temp, &result_layout, &rc, &done_label, indent);
        self.line_indent(indent, &format!("{done_label}:;"));
        Ok(result_temp)
    }

    fn emit_clone_error_jump(
        &mut self,
        result_temp: &str,
        result_layout: &ResultLayout,
        clone_result: &str,
        cloned_ty: &Ty,
        done_label: &str,
        indent: usize,
        span: crate::span::Span,
    ) -> DiagResult<()> {
        let clone_layout =
            self.result_layout(&std_result_ty(cloned_ty.clone(), std_error_ty()), span)?;
        self.line_indent(
            indent,
            &format!("if ({clone_result}.tag == {}) {{", clone_layout.err_index),
        );
        self.line_indent(
            indent + 1,
            &format!(
                "{result_temp} = {};",
                self.result_err_literal(result_layout, &clone_layout, clone_result)
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done_label};"));
        self.line_indent(indent, "}");
        Ok(())
    }

    fn emit_runtime_result_from_rc(
        &mut self,
        result_temp: &str,
        result_layout: &ResultLayout,
        rc: &str,
        done_label: &str,
        indent: usize,
    ) {
        self.line_indent(indent, &format!("if ({rc} != 0) {{"));
        self.line_indent(
            indent + 1,
            &format!(
                "{result_temp} = {};",
                self.result_err_from_error_literal(result_layout, &self.error_code_literal(rc))
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done_label};"));
        self.line_indent(indent, "}");
        self.line_indent(
            indent,
            &format!(
                "{result_temp} = {};",
                self.result_ok_literal(result_layout, None)
            ),
        );
    }

    fn emit_actor_handle(&mut self, actor: &TExpr, indent: usize) -> DiagResult<String> {
        let actor_code = self.gen_expr_in_stmt(actor, indent)?;
        let actor_temp = self.next_temp("actor_ref");
        self.line_indent(
            indent,
            &format!("{} = {actor_code};", self.c_decl(&actor.ty, &actor_temp)),
        );
        Ok(format!("(CielActor *)({actor_temp}->handle)"))
    }

    fn gen_defer_call(&mut self, expr: &TExpr, indent: usize) -> DiagResult<String> {
        let TExprKind::Call { callee, args, .. } = &expr.kind else {
            return self.gen_expr_in_stmt(expr, indent);
        };
        let callee = self.gen_expr_in_stmt(callee, indent)?;
        let mut temp_args = Vec::new();
        for arg in args {
            let value = self.gen_expr_in_stmt(arg, indent)?;
            if arg.ty.is_erased_value() {
                self.line_indent(indent, &format!("(void)({value});"));
                continue;
            }
            let temp = self.next_temp("defer_arg");
            self.line_indent(
                indent,
                &format!("{} = {value};", self.c_decl(&arg.ty, &temp)),
            );
            temp_args.push(temp);
        }
        Ok(format!("{callee}({})", temp_args.join(", ")))
    }

    fn emit_slice_literal_temp(
        &mut self,
        ty: &Ty,
        elements: &[TExpr],
        indent: usize,
    ) -> DiagResult<String> {
        let Ty::Slice(elem) = ty else {
            return Err(vec![Diagnostic::new(
                None,
                "internal error: slice literal emitted for non-slice type",
            )]);
        };
        let data = self.next_temp("slice_data");
        let slice = self.next_temp("slice");
        if elem.is_erased_value() {
            for element in elements {
                let value = self.gen_expr_in_stmt(element, indent)?;
                self.line_indent(indent, &format!("(void)({value});"));
            }
            self.line_indent(
                indent,
                &format!(
                    "{} {slice} = ({}){{ .ptr = NULL, .len = {} }};",
                    self.c_type(ty),
                    self.c_type(ty),
                    elements.len()
                ),
            );
            return Ok(slice);
        }
        self.line_indent(
            indent,
            &format!(
                "{elem_c} *{data} = ({elem_c} *)ciel_alloc_array(sizeof({elem_c}), {len});",
                elem_c = self.c_type(elem),
                len = elements.len()
            ),
        );
        for (idx, element) in elements.iter().enumerate() {
            let value = self.gen_expr_in_stmt(element, indent)?;
            self.line_indent(indent, &format!("{data}[{idx}] = {value};"));
        }
        self.line_indent(
            indent,
            &format!(
                "{} {slice} = ({}){{ .ptr = {data}, .len = {} }};",
                self.c_type(ty),
                self.c_type(ty),
                elements.len()
            ),
        );
        Ok(slice)
    }

    fn emit_slice_repeat_temp(
        &mut self,
        ty: &Ty,
        element: &TExpr,
        len: usize,
        indent: usize,
    ) -> DiagResult<String> {
        let Ty::Slice(elem) = ty else {
            return Err(vec![Diagnostic::new(
                None,
                "internal error: slice repeat literal emitted for non-slice type",
            )]);
        };
        let data = self.next_temp("slice_data");
        let slice = self.next_temp("slice");
        if elem.is_erased_value() {
            let value = self.gen_expr_in_stmt(element, indent)?;
            self.line_indent(indent, &format!("(void)({value});"));
            self.line_indent(
                indent,
                &format!(
                    "{} {slice} = ({}){{ .ptr = NULL, .len = {len} }};",
                    self.c_type(ty),
                    self.c_type(ty),
                ),
            );
            return Ok(slice);
        }
        self.line_indent(
            indent,
            &format!(
                "{elem_c} *{data} = ({elem_c} *)ciel_alloc_array(sizeof({elem_c}), {len});",
                elem_c = self.c_type(elem),
            ),
        );
        self.emit_array_repeat_init(data.as_str(), elem, element, len, indent)?;
        self.line_indent(
            indent,
            &format!(
                "{} {slice} = ({}){{ .ptr = {data}, .len = {len} }};",
                self.c_type(ty),
                self.c_type(ty),
            ),
        );
        Ok(slice)
    }

    fn emit_slice_subview_temp(
        &mut self,
        expr: &TExpr,
        base: &TExpr,
        start: Option<&TExpr>,
        end: Option<&TExpr>,
        indent: usize,
    ) -> DiagResult<String> {
        enum SliceBase {
            Slice,
            Array { len: usize, elem: Ty },
        }

        let source = match base.ty.unqualified() {
            Ty::Slice(_) => SliceBase::Slice,
            Ty::Array { len, elem } => SliceBase::Array {
                len: *len,
                elem: (**elem).clone(),
            },
            other => {
                return Err(vec![Diagnostic::new(
                    base.span,
                    format!("internal error: cannot emit slice subview for `{other}`"),
                )]);
            }
        };

        let base_code = self.gen_expr_in_stmt(base, indent)?;
        let (ptr_code, len_code) = match source {
            SliceBase::Slice => {
                let base_temp = self.next_temp("slice_base");
                self.line_indent(
                    indent,
                    &format!("{} = {base_code};", self.c_decl(&base.ty, &base_temp)),
                );
                (format!("{base_temp}.ptr"), format!("{base_temp}.len"))
            }
            SliceBase::Array { len, elem } => {
                let base_temp = self.next_temp("slice_array");
                let array_ty = Ty::Array {
                    len,
                    elem: Box::new(elem),
                };
                self.line_indent(
                    indent,
                    &format!(
                        "{} = &({base_code});",
                        self.c_pointer_decl(&array_ty, &base_temp)
                    ),
                );
                (format!("(*{base_temp})"), len.to_string())
            }
        };

        let start_temp = self.next_temp("slice_start");
        let start_code = match start {
            Some(start) => self.gen_expr_in_stmt(start, indent)?,
            None => "0".to_string(),
        };
        self.line_indent(
            indent,
            &format!("size_t {start_temp} = (size_t)({start_code});"),
        );

        let end_temp = self.next_temp("slice_end");
        let end_code = match end {
            Some(end) => self.gen_expr_in_stmt(end, indent)?,
            None => len_code.clone(),
        };
        self.line_indent(
            indent,
            &format!("size_t {end_temp} = (size_t)({end_code});"),
        );

        let offset_temp = self.next_temp("slice_offset");
        let (file, line) = self.location_args(expr.span);
        self.line_indent(
            indent,
            &format!(
                "size_t {offset_temp} = ciel_slice_range_check({start_temp}, {end_temp}, {len_code}, {file}, {line});"
            ),
        );

        let slice_temp = self.next_temp("slice");
        let slice_ty = self.c_type(&expr.ty);
        self.line_indent(
            indent,
            &format!(
                "{} = ({slice_ty}){{ .ptr = ({ptr_code}) + {offset_temp}, .len = {end_temp} - {start_temp} }};",
                self.c_decl(&expr.ty, &slice_temp)
            ),
        );
        Ok(slice_temp)
    }

    fn emit_heap_array_init(
        &mut self,
        cname: &str,
        elements: &[TExpr],
        indent: usize,
    ) -> DiagResult<()> {
        for (idx, element) in elements.iter().enumerate() {
            let value = self.gen_expr_in_stmt(element, indent)?;
            if element.ty.is_erased_value() {
                self.line_indent(indent, &format!("(void)({value});"));
                continue;
            }
            self.line_indent(indent, &format!("(*{cname})[{idx}] = {value};"));
        }
        Ok(())
    }

    fn emit_array_repeat_init(
        &mut self,
        target: &str,
        elem_ty: &Ty,
        element: &TExpr,
        len: usize,
        indent: usize,
    ) -> DiagResult<()> {
        if elem_ty.is_erased_value() {
            let value = self.gen_expr_in_stmt(element, indent)?;
            self.line_indent(indent, &format!("(void)({value});"));
            return Ok(());
        }
        let value_temp = self.next_temp("repeat_value");
        let index_temp = self.next_temp("repeat_i");
        let value = self.gen_expr_in_stmt(element, indent)?;
        self.line_indent(
            indent,
            &format!("{} = {value};", self.c_decl(elem_ty, &value_temp)),
        );
        self.line_indent(
            indent,
            &format!("for (size_t {index_temp} = 0; {index_temp} < {len}; {index_temp}++) {{"),
        );
        self.line_indent(
            indent + 1,
            &format!("{target}[{index_temp}] = {value_temp};"),
        );
        self.line_indent(indent, "}");
        Ok(())
    }

    fn emit_dynamic_interface_value(
        &mut self,
        expr: &TExpr,
        inner: &TExpr,
        concrete_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        if matches!(concrete_ty.unqualified(), Ty::DynamicInterface { .. }) {
            return self.emit_dynamic_interface_reerasure(expr, inner, concrete_ty, indent);
        }
        let data_expr = self.gen_expr_in_stmt(inner, indent)?;
        let data_ptr = if matches!(concrete_ty, Ty::Pointer { .. }) {
            format!("(void *)({data_expr})")
        } else {
            let temp = self.next_temp("dyn_data");
            self.line_indent(
                indent,
                &format!(
                    "{} *{temp} = ({})ciel_alloc(sizeof({}));",
                    self.c_type(concrete_ty),
                    self.c_pointer_type(concrete_ty),
                    self.c_sizeof_type(concrete_ty)
                ),
            );
            self.line_indent(indent, &format!("*{temp} = {data_expr};"));
            format!("(void *){temp}")
        };
        let dyn_c = self.c_type(&expr.ty);
        let vtable = self.dynamic_table_name(&expr.ty, concrete_ty);
        Ok(format!(
            "({dyn_c}){{ .data = {data_ptr}, .vtable = &{vtable} }}"
        ))
    }

    fn emit_dynamic_interface_reerasure(
        &mut self,
        expr: &TExpr,
        inner: &TExpr,
        concrete_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let Ty::DynamicInterface { name, args } = &expr.ty else {
            return Err(vec![Diagnostic::new(
                expr.span,
                "internal error: dynamic re-erasure target is not dynamic",
            )]);
        };
        let source_code = self.gen_expr_in_stmt(inner, indent)?;
        let source_temp = self.next_temp("dyn_source");
        self.line_indent(
            indent,
            &format!(
                "{} = {source_code};",
                self.c_decl(concrete_ty, &source_temp)
            ),
        );
        let vtable_ty = self.dynamic_vtable_name(&expr.ty);
        let vtable_temp = self.next_temp("dyn_vtable");
        self.line_indent(
            indent,
            &format!(
                "{vtable_ty} *{vtable_temp} = ({vtable_ty} *)ciel_alloc(sizeof({vtable_ty}));"
            ),
        );
        for interface in self.dynamic_view_interfaces(name, args) {
            self.line_indent(
                indent,
                &format!(
                    "{vtable_temp}->{} = ({source_temp}).vtable->{};",
                    interface.name, interface.name
                ),
            );
        }
        let dyn_c = self.c_type(&expr.ty);
        Ok(format!(
            "({dyn_c}){{ .data = ({source_temp}).data, .vtable = {vtable_temp} }}"
        ))
    }

    fn emit_closure_value(
        &mut self,
        expr: &TExpr,
        id: usize,
        captures: &[TClosureCapture],
        stmt_indent: Option<usize>,
    ) -> DiagResult<String> {
        let owner = self.current_closure_owner.ok_or_else(|| {
            vec![Diagnostic::new(
                expr.span,
                "internal error: closure emitted outside a function",
            )]
        })?;
        if matches!(expr.ty.unqualified(), Ty::Function { .. }) {
            return Ok(self.closure_thunk_name(owner, id));
        }
        let (Ty::Closure { .. } | Ty::ClosureInstance { .. }) = expr.ty.unqualified() else {
            return Err(vec![Diagnostic::new(
                expr.span,
                "internal error: closure literal has non-closure type",
            )]);
        };
        let env = if captures.is_empty() {
            "NULL".to_string()
        } else {
            let Some(indent) = stmt_indent else {
                return Err(vec![Diagnostic::new(
                    expr.span,
                    "capturing closure needs statement lowering",
                )]);
            };
            let env_name = self.closure_env_name(owner, id);
            let temp = self.next_temp("closure_env");
            self.line_indent(
                indent,
                &format!("{env_name} *{temp} = ({env_name} *)ciel_alloc(sizeof({env_name}));"),
            );
            for (idx, capture) in captures.iter().enumerate() {
                if capture.ty.is_erased_value() {
                    continue;
                }
                let value = TExpr {
                    span: expr.span,
                    ty: capture.ty.clone(),
                    kind: TExprKind::Local(capture.local_id, capture.name.clone()),
                };
                let value = self.gen_expr_in_stmt(&value, indent)?;
                self.emit_value_copy(&format!("{temp}->cap{idx}"), &value, &capture.ty, indent);
            }
            format!("(void *){temp}")
        };
        Ok(format!(
            "({}){{ .call = {}, .env = {env} }}",
            self.c_type(&expr.ty),
            self.closure_thunk_name(owner, id)
        ))
    }

    fn emit_function_to_closure_value(
        &mut self,
        expr: &TExpr,
        inner: &TExpr,
        stmt_indent: Option<usize>,
    ) -> DiagResult<String> {
        let Some(indent) = stmt_indent else {
            return Err(vec![Diagnostic::new(
                expr.span,
                "function-to-closure conversion needs statement lowering",
            )]);
        };
        let function_value = self.gen_expr_in_stmt(inner, indent)?;
        self.emit_closure_value_from_source(&expr.ty, &inner.ty, &function_value, indent)
    }

    fn emit_retain_closure_value(
        &mut self,
        expr: &TExpr,
        inner: &TExpr,
        source_ty: &Ty,
        stmt_indent: Option<usize>,
    ) -> DiagResult<String> {
        let Some(indent) = stmt_indent else {
            return Err(vec![Diagnostic::new(
                expr.span,
                "retained closure conversion needs statement lowering",
            )]);
        };
        let source_code = self.gen_expr_in_stmt(inner, indent)?;
        let source_temp = self.next_temp("closure_source");
        self.line_indent(
            indent,
            &format!("{} = {source_code};", self.c_decl(source_ty, &source_temp)),
        );
        self.emit_closure_value_from_source(&expr.ty, source_ty, &source_temp, indent)
    }

    fn emit_closure_value_from_source(
        &mut self,
        target_ty: &Ty,
        source_ty: &Ty,
        source_value: &str,
        indent: usize,
    ) -> DiagResult<String> {
        if matches!(source_ty.unqualified(), Ty::Function { abi: None, .. }) {
            return self.emit_function_closure_value_from_source(
                target_ty,
                source_ty,
                source_value,
                indent,
            );
        }
        if retained_closure_needs_wrapper(target_ty, source_ty) {
            return self.emit_wrapped_retained_closure_value_from_source(
                target_ty,
                source_ty,
                source_value,
                indent,
            );
        }
        let mut fields = vec![
            format!(".call = ({source_value}).call"),
            format!(".env = ({source_value}).env"),
        ];
        fields.extend(self.retained_closure_witness_initializers(
            target_ty,
            source_ty,
            source_value,
        ));
        Ok(format!(
            "({}){{ {} }}",
            self.c_type(target_ty),
            fields.join(", ")
        ))
    }

    fn emit_function_closure_value_from_source(
        &mut self,
        target_ty: &Ty,
        source_ty: &Ty,
        source_value: &str,
        indent: usize,
    ) -> DiagResult<String> {
        let env_name = self.function_closure_env_name(target_ty, source_ty);
        let temp = self.next_temp("closure_fn_env");
        self.line_indent(
            indent,
            &format!("{env_name} *{temp} = ({env_name} *)ciel_alloc(sizeof({env_name}));"),
        );
        self.line_indent(indent, &format!("{temp}->func = {source_value};"));
        let mut fields = vec![
            format!(
                ".call = {}",
                self.function_closure_thunk_name(target_ty, source_ty)
            ),
            format!(".env = (void *){temp}"),
        ];
        fields.extend(self.retained_closure_witness_initializers(target_ty, source_ty, ""));
        Ok(format!(
            "({}){{ {} }}",
            self.c_type(target_ty),
            fields.join(", ")
        ))
    }

    fn emit_wrapped_retained_closure_value_from_source(
        &mut self,
        target_ty: &Ty,
        source_ty: &Ty,
        source_value: &str,
        indent: usize,
    ) -> DiagResult<String> {
        let env_name = self.retained_closure_env_name(target_ty, source_ty);
        let temp = self.next_temp("closure_retain_env");
        self.line_indent(
            indent,
            &format!("{env_name} *{temp} = ({env_name} *)ciel_alloc(sizeof({env_name}));"),
        );
        self.emit_value_copy(&format!("{temp}->source"), source_value, source_ty, indent);
        let mut fields = vec![
            format!(
                ".call = {}",
                self.retained_closure_thunk_name(target_ty, source_ty)
            ),
            format!(".env = (void *){temp}"),
        ];
        fields.extend(self.retained_closure_witness_initializers(
            target_ty,
            source_ty,
            source_value,
        ));
        Ok(format!(
            "({}){{ {} }}",
            self.c_type(target_ty),
            fields.join(", ")
        ))
    }

    fn retained_closure_witness_initializers(
        &self,
        target_ty: &Ty,
        source_ty: &Ty,
        source_value: &str,
    ) -> Vec<String> {
        retained_closure_capabilities(target_ty)
            .into_iter()
            .map(|capability| {
                let field = self.retained_closure_witness_field_name(&capability);
                let value = self.retained_closure_witness_value(
                    target_ty,
                    source_ty,
                    &capability,
                    Some(source_value),
                );
                format!(".{field} = {value}")
            })
            .collect()
    }

    fn retained_closure_witness_value(
        &self,
        target_ty: &Ty,
        source_ty: &Ty,
        capability: &ConstraintRef,
        source_value: Option<&str>,
    ) -> String {
        if retained_closure_can_reuse_source_witness_field(target_ty, source_ty, capability)
            && let Some(source_value) = source_value
        {
            return format!(
                "({source_value}).{}",
                self.retained_closure_witness_field_name(capability)
            );
        }
        self.retained_closure_witness_name(target_ty, source_ty, capability)
    }

    fn emit_closure_call(
        &mut self,
        callee: &TExpr,
        args: &[TExpr],
        stmt_indent: Option<usize>,
    ) -> DiagResult<String> {
        let callee_code = self.gen_expr_with_lowering(callee, stmt_indent)?;
        let receiver = if let Some(indent) = stmt_indent {
            let temp = self.next_temp("closure");
            self.line_indent(
                indent,
                &format!("{} = {callee_code};", self.c_decl(&callee.ty, &temp)),
            );
            temp
        } else {
            callee_code
        };
        let mut call_args = vec![format!("({receiver}).env")];
        call_args.extend(self.gen_call_args(args, stmt_indent)?);
        Ok(format!("({receiver}).call({})", call_args.join(", ")))
    }

    fn emit_retained_closure_interface_call(
        &mut self,
        interface_name: &str,
        interface_args: &[Ty],
        receiver: &TExpr,
        args: &[TExpr],
        stmt_indent: Option<usize>,
    ) -> DiagResult<String> {
        let capability = ConstraintRef {
            name: interface_name.to_string(),
            args: interface_args.to_vec(),
        };
        let receiver_code = self.gen_expr_with_lowering(receiver, stmt_indent)?;
        let (receiver_ref, receiver_value) = match receiver.ty.unqualified() {
            Ty::Pointer { inner, .. } if matches!(inner.unqualified(), Ty::Closure { .. }) => {
                let receiver_ref = if let Some(indent) = stmt_indent {
                    let temp = self.next_temp("retained_recv_ptr");
                    self.line_indent(
                        indent,
                        &format!("{} = {receiver_code};", self.c_decl(&receiver.ty, &temp)),
                    );
                    temp
                } else {
                    receiver_code
                };
                (receiver_ref.clone(), format!("*({receiver_ref})"))
            }
            Ty::Closure { .. } => {
                let Some(indent) = stmt_indent else {
                    return Err(vec![Diagnostic::new(
                        receiver.span,
                        "retained closure interface call needs statement lowering",
                    )]);
                };
                let temp = self.next_temp("retained_recv");
                self.line_indent(
                    indent,
                    &format!("{} = {receiver_code};", self.c_decl(&receiver.ty, &temp)),
                );
                (format!("&{temp}"), temp)
            }
            other => {
                return Err(vec![Diagnostic::new(
                    receiver.span,
                    format!(
                        "internal error: retained closure interface receiver has type `{other}`"
                    ),
                )]);
            }
        };
        let mut call_args = vec![format!("(void *)({receiver_ref})")];
        call_args.extend(self.gen_call_args(args, stmt_indent)?);
        Ok(format!(
            "({receiver_value}).{}({})",
            self.retained_closure_witness_field_name(&capability),
            call_args.join(", ")
        ))
    }

    fn emit_closure_value_layouts(&mut self) {
        let closure_types = self.plan.closure_types.clone();
        for (name, ty) in closure_types {
            self.line(&format!("struct {name} {{"));
            self.line(&format!("    {};", self.closure_call_field_decl(&ty)));
            self.line("    void *env;");
            for capability in retained_closure_capabilities(&ty) {
                self.line(&format!(
                    "    {};",
                    self.retained_closure_witness_field_decl(&ty, &capability)
                ));
            }
            self.line("};");
            self.line("");
        }
    }

    fn emit_closure_environment_layouts(&mut self) {
        let closure_defs = self.plan.closure_defs.clone();
        for closure in closure_defs.values() {
            if closure.captures.is_empty() {
                continue;
            }
            let env_name = self.closure_env_name(closure.owner, closure.id);
            self.line(&format!("struct {env_name} {{"));
            let mut emitted_capture = false;
            for (idx, capture) in closure.captures.iter().enumerate() {
                if capture.ty.is_erased_value() {
                    continue;
                }
                emitted_capture = true;
                self.line(&format!(
                    "    {};",
                    self.c_decl(&capture.ty, &format!("cap{idx}"))
                ));
            }
            if !emitted_capture {
                self.line("    char _ciel_empty;");
            }
            self.line("};");
            self.line("");
        }

        let wrappers = self.plan.function_closure_wrappers.clone();
        for wrapper in wrappers.values() {
            let env_name =
                self.function_closure_env_name(&wrapper.closure_ty, &wrapper.function_ty);
            self.line(&format!("struct {env_name} {{"));
            self.line(&format!(
                "    {};",
                self.c_decl(&wrapper.function_ty, "func")
            ));
            self.line("};");
            self.line("");
        }

        let wrappers = self.plan.retained_closure_wrappers.clone();
        for wrapper in wrappers.values() {
            let env_name = self.retained_closure_env_name(&wrapper.target_ty, &wrapper.source_ty);
            self.line(&format!("struct {env_name} {{"));
            self.line(&format!(
                "    {};",
                self.c_decl(&wrapper.source_ty, "source")
            ));
            self.line("};");
            self.line("");
        }
    }

    fn emit_closure_prototypes(&mut self) {
        let closures = self.plan.closure_defs.clone();
        for closure in closures.values() {
            self.line(&format!("{};", self.closure_thunk_decl(closure)));
        }
        let wrappers = self.plan.function_closure_wrappers.clone();
        for wrapper in wrappers.values() {
            self.line(&format!(
                "{};",
                self.function_closure_thunk_decl(&wrapper.closure_ty, &wrapper.function_ty)
            ));
        }
        let wrappers = self.plan.retained_closure_wrappers.clone();
        for wrapper in wrappers.values() {
            self.line(&format!(
                "{};",
                self.retained_closure_thunk_decl(&wrapper.target_ty, &wrapper.source_ty)
            ));
        }
    }

    fn emit_retained_closure_witness_prototypes(&mut self) {
        let witnesses = self.plan.retained_closure_witnesses.clone();
        for witness in witnesses.values() {
            self.line(&format!("{};", self.retained_closure_witness_decl(witness)));
        }
    }

    fn emit_actor_dispatch_prototypes(&mut self) {
        let dispatches = self.plan.actor_dispatches.clone();
        for dispatch in dispatches.values() {
            self.line(&format!(
                "static void {}(void *state_raw, void *handler_raw, void *message_raw, int32_t *failed);",
                dispatch.name
            ));
        }
    }

    fn emit_closure_thunks_and_wrappers(&mut self) -> DiagResult<()> {
        let closures = self.plan.closure_defs.clone();
        for closure in closures.values() {
            self.emit_closure_thunk(closure)?;
            self.line("");
        }
        let wrappers = self.plan.function_closure_wrappers.clone();
        for wrapper in wrappers.values() {
            self.emit_function_closure_wrapper(wrapper);
            self.line("");
        }
        let wrappers = self.plan.retained_closure_wrappers.clone();
        for wrapper in wrappers.values() {
            self.emit_retained_closure_wrapper(wrapper)?;
            self.line("");
        }
        Ok(())
    }

    fn emit_retained_closure_witnesses(&mut self) -> DiagResult<()> {
        let witnesses = self.plan.retained_closure_witnesses.clone();
        for witness in witnesses.values() {
            self.emit_retained_closure_witness(witness)?;
            self.line("");
        }
        Ok(())
    }

    fn retained_closure_witness_decl(&self, witness: &RetainedClosureWitness) -> String {
        let ret = self.retained_closure_interface_ret(&witness.target_ty, &witness.capability);
        let params =
            self.retained_closure_interface_params(&witness.target_ty, &witness.capability);
        let params = params
            .iter()
            .filter(|ty| !ty.is_erased_value())
            .enumerate()
            .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}")))
            .collect::<Vec<_>>()
            .join(", ");
        self.c_return_decl(
            &ret,
            &format!(
                "{}({})",
                self.retained_closure_witness_name(
                    &witness.target_ty,
                    &witness.source_ty,
                    &witness.capability
                ),
                params
            ),
        )
    }

    fn emit_retained_closure_witness(
        &mut self,
        witness: &RetainedClosureWitness,
    ) -> DiagResult<()> {
        if is_clone_message_capability(&witness.capability) {
            return self.emit_retained_closure_clone_witness(witness);
        }
        if retained_closure_can_forward_source_witness(&witness.source_ty, &witness.capability) {
            return self.emit_retained_closure_forwarding_witness(witness);
        }
        let Some(implementation) = self
            .impl_for_retained_closure_witness(&witness.capability, &witness.source_ty)
            .cloned()
        else {
            return Err(vec![Diagnostic::new(
                None,
                format!(
                    "internal error: missing retained closure witness implementation for `{}` on `{}`",
                    witness.capability.name, witness.source_ty
                ),
            )]);
        };
        let ret = self.retained_closure_interface_ret(&witness.target_ty, &witness.capability);
        let params =
            self.retained_closure_interface_params(&witness.target_ty, &witness.capability);
        self.line(&format!(
            "{} {{",
            self.retained_closure_witness_decl(witness)
        ));
        let first_param = implementation
            .params
            .first()
            .cloned()
            .unwrap_or(Ty::Unknown);
        let mut args = Vec::new();
        if matches!(first_param, Ty::Pointer { .. }) {
            args.push(self.retained_closure_source_pointer_expr(witness));
        } else {
            let source_ptr = self.retained_closure_source_pointer_expr(witness);
            args.push(format!("*({source_ptr})"));
        }
        let mut physical_idx = 1;
        for (target_param, source_param) in params
            .iter()
            .skip(1)
            .zip(implementation.params.iter().skip(1))
        {
            if target_param.is_erased_value() {
                continue;
            }
            let arg = format!("arg{physical_idx}");
            physical_idx += 1;
            if source_param.is_erased_value() {
                continue;
            }
            let adapted = self.emit_retained_closure_adapt_value(
                witness,
                target_param,
                source_param,
                &arg,
                1,
            )?;
            args.push(adapted);
        }
        let call = format!(
            "{}({})",
            self.c_name(implementation.function_def),
            args.join(", ")
        );
        let source_ret = implementation.ret.clone();
        self.emit_retained_closure_adapted_return(witness, &source_ret, &ret, &call, 1)?;
        self.line("}");
        Ok(())
    }

    fn emit_retained_closure_forwarding_witness(
        &mut self,
        witness: &RetainedClosureWitness,
    ) -> DiagResult<()> {
        let target_ret =
            self.retained_closure_interface_ret(&witness.target_ty, &witness.capability);
        let source_ret =
            self.retained_closure_interface_ret(&witness.source_ty, &witness.capability);
        let params =
            self.retained_closure_interface_params(&witness.target_ty, &witness.capability);
        self.line(&format!(
            "{} {{",
            self.retained_closure_witness_decl(witness)
        ));
        let source_ptr = self.retained_closure_source_pointer_expr(witness);
        let mut args = vec![format!("(void *)({source_ptr})")];
        let source_params =
            self.retained_closure_interface_params(&witness.source_ty, &witness.capability);
        let mut physical_idx = 1;
        for (target_param, source_param) in params.iter().skip(1).zip(source_params.iter().skip(1))
        {
            if target_param.is_erased_value() {
                continue;
            }
            let arg = format!("arg{physical_idx}");
            physical_idx += 1;
            if source_param.is_erased_value() {
                continue;
            }
            let adapted = self.emit_retained_closure_adapt_value(
                witness,
                target_param,
                source_param,
                &arg,
                1,
            )?;
            args.push(adapted);
        }
        let field = self.retained_closure_witness_field_name(&witness.capability);
        let call = format!("(*({source_ptr})).{field}({})", args.join(", "));
        self.emit_retained_closure_adapted_return(witness, &source_ret, &target_ret, &call, 1)?;
        self.line("}");
        Ok(())
    }

    fn emit_retained_closure_adapted_return(
        &mut self,
        witness: &RetainedClosureWitness,
        source_ret: &Ty,
        target_ret: &Ty,
        call: &str,
        indent: usize,
    ) -> DiagResult<()> {
        if target_ret.is_erased_value() {
            self.line_indent(indent, &format!("{call};"));
            self.line_indent(indent, "return;");
            return Ok(());
        }
        if source_ret == target_ret {
            self.line_indent(indent, &format!("return {call};"));
            return Ok(());
        }
        let source_temp = self.next_temp("retained_source_ret");
        self.line_indent(
            indent,
            &format!("{} = {call};", self.c_decl(source_ret, &source_temp)),
        );
        let adapted = self.emit_retained_closure_adapt_value(
            witness,
            source_ret,
            target_ret,
            &source_temp,
            indent,
        )?;
        self.line_indent(indent, &format!("return {adapted};"));
        Ok(())
    }

    fn emit_retained_closure_adapt_value(
        &mut self,
        witness: &RetainedClosureWitness,
        source_ty: &Ty,
        target_ty: &Ty,
        source_value: &str,
        indent: usize,
    ) -> DiagResult<String> {
        if source_ty == target_ty {
            return Ok(source_value.to_string());
        }
        if source_ty == &witness.source_ty && target_ty == &witness.target_ty {
            return self.emit_closure_value_from_source(
                &witness.target_ty,
                &witness.source_ty,
                source_value,
                indent,
            );
        }
        if source_ty == &witness.target_ty && target_ty == &witness.source_ty {
            return self.emit_retained_closure_source_value_from_target(
                witness,
                source_value,
                indent,
            );
        }
        if let (Some((source_ok, source_err)), Some((target_ok, target_err))) =
            (result_args(source_ty), result_args(target_ty))
            && source_err == target_err
        {
            let source_layout = self.result_layout(source_ty, witness.span)?;
            let target_layout = self.result_layout(target_ty, witness.span)?;
            let target_temp = self.next_temp("retained_target_ret");
            self.line_indent(
                indent,
                &format!("{};", self.c_decl(target_ty, &target_temp)),
            );
            self.line_indent(
                indent,
                &format!("if ({source_value}.tag == {}) {{", source_layout.err_index),
            );
            self.line_indent(
                indent + 1,
                &format!(
                    "{target_temp} = {};",
                    self.result_err_literal(&target_layout, &source_layout, source_value)
                ),
            );
            self.line_indent(indent, "} else {");
            if target_layout.ok_has_payload {
                let source_ok_value = format!("{source_value}.as.{}._0", source_layout.ok_name);
                let adapted_ok = self.emit_retained_closure_adapt_value(
                    witness,
                    source_ok,
                    target_ok,
                    &source_ok_value,
                    indent + 1,
                )?;
                self.line_indent(
                    indent + 1,
                    &format!(
                        "{target_temp} = {};",
                        self.result_ok_literal(&target_layout, Some(&adapted_ok))
                    ),
                );
            } else {
                self.line_indent(
                    indent + 1,
                    &format!(
                        "{target_temp} = {};",
                        self.result_ok_literal(&target_layout, None)
                    ),
                );
            }
            self.line_indent(indent, "}");
            return Ok(target_temp);
        }
        if let Some(adapted) = self.emit_retained_closure_adapt_struct_value(
            witness,
            source_ty,
            target_ty,
            source_value,
            indent,
        )? {
            return Ok(adapted);
        }
        if let Some(adapted) = self.emit_retained_closure_adapt_enum_value(
            witness,
            source_ty,
            target_ty,
            source_value,
            indent,
        )? {
            return Ok(adapted);
        }
        if let (
            Ty::Array {
                len: source_len,
                elem: source_elem,
            },
            Ty::Array {
                len: target_len,
                elem: target_elem,
            },
        ) = (source_ty.unqualified(), target_ty.unqualified())
            && source_len == target_len
        {
            let target_temp = self.next_temp("retained_target_array");
            self.line_indent(
                indent,
                &format!("{};", self.c_decl(target_ty, &target_temp)),
            );
            let idx = self.next_temp("retained_i");
            self.line_indent(
                indent,
                &format!("for (size_t {idx} = 0; {idx} < {target_len}; {idx}++) {{"),
            );
            let source_item = format!("({source_value})[{idx}]");
            let adapted_item = self.emit_retained_closure_adapt_value(
                witness,
                source_elem,
                target_elem,
                &source_item,
                indent + 1,
            )?;
            self.line_indent(
                indent + 1,
                &format!("({target_temp})[{idx}] = {adapted_item};"),
            );
            self.line_indent(indent, "}");
            return Ok(target_temp);
        }
        if let (Ty::Slice(source_elem), Ty::Slice(target_elem)) =
            (source_ty.unqualified(), target_ty.unqualified())
        {
            let target_temp = self.next_temp("retained_target_slice");
            self.line_indent(
                indent,
                &format!("{};", self.c_decl(target_ty, &target_temp)),
            );
            self.line_indent(
                indent,
                &format!("{target_temp}.len = ({source_value}).len;"),
            );
            if target_elem.is_erased_value() {
                self.line_indent(indent, &format!("{target_temp}.ptr = NULL;"));
                return Ok(target_temp);
            }
            self.line_indent(
                indent,
                &format!(
                    "{target_temp}.ptr = ({})ciel_alloc_array(sizeof({}), ({source_value}).len);",
                    self.c_pointer_type(target_elem),
                    self.c_sizeof_type(target_elem)
                ),
            );
            let idx = self.next_temp("retained_i");
            self.line_indent(
                indent,
                &format!("for (size_t {idx} = 0; {idx} < ({source_value}).len; {idx}++) {{"),
            );
            let source_item = format!("({source_value}).ptr[{idx}]");
            let adapted_item = self.emit_retained_closure_adapt_value(
                witness,
                source_elem,
                target_elem,
                &source_item,
                indent + 1,
            )?;
            self.line_indent(
                indent + 1,
                &format!("{target_temp}.ptr[{idx}] = {adapted_item};"),
            );
            self.line_indent(indent, "}");
            return Ok(target_temp);
        }
        Err(vec![Diagnostic::new(
            witness.span,
            format!(
                "internal error: cannot adapt retained closure witness return `{source_ty}` to `{target_ty}`"
            ),
        )])
    }

    fn emit_retained_closure_source_value_from_target(
        &mut self,
        witness: &RetainedClosureWitness,
        target_value: &str,
        indent: usize,
    ) -> DiagResult<String> {
        let target_temp = self.next_temp("retained_target_value");
        self.line_indent(
            indent,
            &format!(
                "{} = {target_value};",
                self.c_decl(&witness.target_ty, &target_temp)
            ),
        );
        let target_ptr = format!("&{target_temp}");
        let source_ptr =
            self.retained_closure_source_pointer_expr_from_target_ptr(witness, &target_ptr);
        let source_temp = self.next_temp("retained_source_value");
        self.line_indent(
            indent,
            &format!(
                "{} = *({source_ptr});",
                self.c_decl(&witness.source_ty, &source_temp)
            ),
        );
        Ok(source_temp)
    }

    fn emit_retained_closure_adapt_struct_value(
        &mut self,
        witness: &RetainedClosureWitness,
        source_ty: &Ty,
        target_ty: &Ty,
        source_value: &str,
        indent: usize,
    ) -> DiagResult<Option<String>> {
        let (
            Ty::Named {
                name: source_name,
                args: source_args,
            },
            Ty::Named {
                name: target_name,
                args: target_args,
            },
        ) = (source_ty.unqualified(), target_ty.unqualified())
        else {
            return Ok(None);
        };
        if source_name != target_name || source_args.len() != target_args.len() {
            return Ok(None);
        }
        let source_instance = self.c_named_type(source_name, source_args);
        let target_instance = self.c_named_type(target_name, target_args);
        let Some(source_fields) = self
            .program
            .checked
            .structs
            .iter()
            .find(|strukt| strukt.name == source_instance)
            .map(|strukt| strukt.fields.clone())
        else {
            return Ok(None);
        };
        let Some(target_fields) = self
            .program
            .checked
            .structs
            .iter()
            .find(|strukt| strukt.name == target_instance)
            .map(|strukt| strukt.fields.clone())
        else {
            return Ok(None);
        };
        if source_fields.len() != target_fields.len() {
            return Ok(None);
        }
        let target_temp = self.next_temp("retained_target_struct");
        self.line_indent(
            indent,
            &format!(
                "{} = {};",
                self.c_decl(target_ty, &target_temp),
                self.zero_value(target_ty)
            ),
        );
        for ((source_field, source_field_ty), (target_field, target_field_ty)) in
            source_fields.iter().zip(target_fields.iter())
        {
            if source_field != target_field {
                return Ok(None);
            }
            if target_field_ty.is_erased_value() {
                continue;
            }
            if source_field_ty.is_erased_value() {
                return Ok(None);
            }
            let source_field_value = format!("({source_value}).{source_field}");
            let adapted = self.emit_retained_closure_adapt_value(
                witness,
                source_field_ty,
                target_field_ty,
                &source_field_value,
                indent,
            )?;
            self.line_indent(
                indent,
                &format!("{target_temp}.{target_field} = {adapted};"),
            );
        }
        Ok(Some(target_temp))
    }

    fn emit_retained_closure_adapt_enum_value(
        &mut self,
        witness: &RetainedClosureWitness,
        source_ty: &Ty,
        target_ty: &Ty,
        source_value: &str,
        indent: usize,
    ) -> DiagResult<Option<String>> {
        let (
            Ty::Named {
                name: source_name,
                args: source_args,
            },
            Ty::Named {
                name: target_name,
                args: target_args,
            },
        ) = (source_ty.unqualified(), target_ty.unqualified())
        else {
            return Ok(None);
        };
        if source_name != target_name || source_args.len() != target_args.len() {
            return Ok(None);
        }
        let source_instance = self.c_named_type(source_name, source_args);
        let target_instance = self.c_named_type(target_name, target_args);
        let Some(source_variants) = self
            .program
            .checked
            .enums
            .iter()
            .find(|enm| enm.name == source_instance)
            .map(|enm| enm.variants.clone())
        else {
            return Ok(None);
        };
        let Some(target_variants) = self
            .program
            .checked
            .enums
            .iter()
            .find(|enm| enm.name == target_instance)
            .map(|enm| enm.variants.clone())
        else {
            return Ok(None);
        };
        if source_variants.len() != target_variants.len() {
            return Ok(None);
        }
        let target_temp = self.next_temp("retained_target_enum");
        self.line_indent(
            indent,
            &format!(
                "{} = {};",
                self.c_decl(target_ty, &target_temp),
                self.zero_value(target_ty)
            ),
        );
        self.line_indent(indent, &format!("switch (({source_value}).tag) {{"));
        for (idx, (source_variant, target_variant)) in source_variants
            .iter()
            .zip(target_variants.iter())
            .enumerate()
        {
            if source_variant.name != target_variant.name {
                return Ok(None);
            }
            if source_variant.payload.len() != target_variant.payload.len() {
                return Ok(None);
            }
            self.line_indent(indent, &format!("case {idx}:"));
            self.line_indent(indent + 1, &format!("{target_temp}.tag = {idx};"));
            for (payload_idx, (source_payload, target_payload)) in source_variant
                .payload
                .iter()
                .zip(target_variant.payload.iter())
                .enumerate()
            {
                let source_payload_value =
                    format!("({source_value}).as.{}._{payload_idx}", source_variant.name);
                let adapted = self.emit_retained_closure_adapt_value(
                    witness,
                    source_payload,
                    target_payload,
                    &source_payload_value,
                    indent + 1,
                )?;
                self.line_indent(
                    indent + 1,
                    &format!(
                        "{target_temp}.as.{}._{payload_idx} = {adapted};",
                        target_variant.name
                    ),
                );
            }
            self.line_indent(indent + 1, "break;");
        }
        self.line_indent(indent, "}");
        Ok(Some(target_temp))
    }

    fn emit_retained_closure_clone_witness(
        &mut self,
        witness: &RetainedClosureWitness,
    ) -> DiagResult<()> {
        let result_ty =
            self.retained_closure_interface_ret(&witness.target_ty, &witness.capability);
        let result_layout = self.result_layout(&result_ty, witness.span)?;
        let result_temp = self.next_temp("retained_clone_result");
        let done_label = self.next_temp("retained_clone_done");
        self.line(&format!(
            "{} {{",
            self.retained_closure_witness_decl(witness)
        ));
        self.line_indent(1, &format!("{};", self.c_decl(&result_ty, &result_temp)));
        if result_layout.ok_has_payload {
            let target = format!("{result_temp}.as.{}._0", result_layout.ok_name);
            let source_clone = self.emit_retained_closure_clone_source_value(
                witness,
                &result_temp,
                &result_layout,
                &done_label,
                1,
            )?;
            let target_value = self.emit_closure_value_from_source(
                &witness.target_ty,
                &witness.source_ty,
                &source_clone,
                1,
            )?;
            self.line_indent(1, &format!("{target} = {target_value};"));
            self.line_indent(
                1,
                &format!("{result_temp}.tag = {};", result_layout.ok_index),
            );
        } else {
            self.line_indent(
                1,
                &format!(
                    "{result_temp} = {};",
                    self.result_ok_literal(&result_layout, None)
                ),
            );
        }
        self.line_indent(1, &format!("{done_label}:;"));
        self.line_indent(1, &format!("return {result_temp};"));
        self.line("}");
        Ok(())
    }

    fn emit_retained_closure_clone_source_value(
        &mut self,
        witness: &RetainedClosureWitness,
        result_temp: &str,
        result_layout: &ResultLayout,
        done_label: &str,
        indent: usize,
    ) -> DiagResult<String> {
        let source_ptr = self.retained_closure_source_pointer_expr(witness);
        let source_temp = self.next_temp("retained_clone_source");
        self.line_indent(
            indent,
            &format!("{};", self.c_decl(&witness.source_ty, &source_temp)),
        );
        match witness.source_ty.unqualified() {
            Ty::Function { abi: None, .. } => {
                self.line_indent(indent, &format!("{source_temp} = *({source_ptr});"));
            }
            Ty::Closure { constraints, .. }
                if constraints.positive.iter().any(is_clone_message_capability) =>
            {
                let capability = ConstraintRef {
                    name: "clone_message".to_string(),
                    args: Vec::new(),
                };
                let field = self.retained_closure_witness_field_name(&capability);
                let clone_result_ty = std_result_ty(witness.source_ty.clone(), std_error_ty());
                let clone_layout = self.result_layout(&clone_result_ty, witness.span)?;
                let clone_temp = self.next_temp("retained_source_clone");
                self.line_indent(
                    indent,
                    &format!(
                        "{} {clone_temp} = (*({source_ptr})).{field}((void *)({source_ptr}));",
                        clone_layout.c_type
                    ),
                );
                self.line_indent(
                    indent,
                    &format!("if ({clone_temp}.tag == {}) {{", clone_layout.err_index),
                );
                self.line_indent(
                    indent + 1,
                    &format!(
                        "{result_temp} = {};",
                        self.result_err_literal(result_layout, &clone_layout, &clone_temp)
                    ),
                );
                self.line_indent(indent + 1, &format!("goto {done_label};"));
                self.line_indent(indent, "}");
                self.line_indent(
                    indent,
                    &format!(
                        "{source_temp} = {clone_temp}.as.{}._0;",
                        clone_layout.ok_name
                    ),
                );
            }
            Ty::ClosureInstance { .. } => {
                self.emit_clone_closure_value_into(
                    &witness.source_ty,
                    &source_ptr,
                    &source_temp,
                    result_temp,
                    result_layout,
                    done_label,
                    indent,
                    witness.span,
                )?;
            }
            other => {
                return Err(vec![Diagnostic::new(
                    witness.span,
                    format!("internal error: cannot clone retained closure source type `{other}`"),
                )]);
            }
        }
        Ok(source_temp)
    }

    fn emit_actor_dispatches(&mut self) -> DiagResult<()> {
        let dispatches = self.plan.actor_dispatches.clone();
        for dispatch in dispatches.values() {
            self.emit_actor_dispatch(dispatch)?;
            self.line("");
        }
        Ok(())
    }

    fn emit_actor_dispatch(&mut self, dispatch: &ActorDispatch) -> DiagResult<()> {
        let result_ty = std_result_ty(dispatch.state_ty.clone(), std_error_ty());
        let result_layout = self.result_layout(
            &result_ty,
            crate::span::Span::new(crate::span::FileId(0), 0, 0),
        )?;
        self.line(&format!(
            "static void {}(void *state_raw, void *handler_raw, void *message_raw, int32_t *failed) {{",
            dispatch.name
        ));
        self.line_indent(
            1,
            &format!(
                "{} = state_raw;",
                self.c_pointer_decl(&dispatch.state_ty, "state")
            ),
        );
        self.line_indent(
            1,
            &format!(
                "{} = handler_raw;",
                self.c_pointer_decl(&dispatch.handler_ty, "handler")
            ),
        );
        self.line_indent(
            1,
            &format!(
                "{} = message_raw;",
                self.c_pointer_decl(&dispatch.message_ty, "message")
            ),
        );
        let call = self.actor_handler_call_expr(
            &dispatch.handler_ty,
            "(*handler)",
            "(*state)",
            "(*message)",
        )?;
        self.line_indent(
            1,
            &format!("{} result = {call};", self.c_decl(&result_ty, "")),
        );
        self.line_indent(
            1,
            &format!("if (result.tag == {}) {{", result_layout.err_index),
        );
        self.line_indent(2, "*failed = 1;");
        self.line_indent(2, "return;");
        self.line_indent(1, "}");
        if result_layout.ok_has_payload {
            self.line_indent(
                1,
                &format!("*state = result.as.{}._0;", result_layout.ok_name),
            );
        }
        self.line("}");
        Ok(())
    }

    fn emit_closure_thunk(&mut self, closure: &ClosureDef) -> DiagResult<()> {
        let (ret, _) = self.callable_ret_params(&closure.ty)?;
        self.line(&format!("{} {{", self.closure_thunk_decl(closure)));

        let previous_return_ty = std::mem::replace(&mut self.current_return_ty, ret.clone());
        let previous_heap_locals = std::mem::replace(
            &mut self.current_heap_locals,
            self.escapes
                .functions
                .get(&closure.owner)
                .map(|escape| escape.heap_locals.clone())
                .unwrap_or_default(),
        );
        let previous_param_locals = std::mem::replace(
            &mut self.current_param_locals,
            closure
                .params
                .iter()
                .filter(|(_, _, ty)| !ty.is_erased_value())
                .enumerate()
                .map(|(idx, (local_id, _, _))| (*local_id, format!("arg{idx}")))
                .collect(),
        );
        let previous_capture_locals = std::mem::take(&mut self.current_capture_locals);
        let previous_closure_owner = self.current_closure_owner.replace(closure.owner);
        self.defer_stack.clear();
        self.loop_defer_starts.clear();

        if matches!(closure.ty, Ty::Closure { .. } | Ty::ClosureInstance { .. })
            && !closure.captures.is_empty()
        {
            let env_name = self.closure_env_name(closure.owner, closure.id);
            self.line_indent(1, &format!("{env_name} *env = ({env_name} *)env_raw;"));
            self.current_capture_locals = closure
                .captures
                .iter()
                .enumerate()
                .map(|(idx, capture)| (capture.local_id, format!("env->cap{idx}")))
                .collect();
        }

        match &closure.body {
            TClosureBody::Expr(expr) => {
                let value = self.gen_expr_in_stmt(expr, 1)?;
                if ret.is_erased_value() {
                    self.line_indent(1, &format!("(void)({value});"));
                    self.line_indent(1, "return;");
                } else {
                    self.line_indent(1, &format!("return {value};"));
                }
            }
            TClosureBody::Block(block) => {
                let falls_through = self.gen_block_inner(block, 1)?;
                if falls_through && ret.is_never() {
                    self.line_indent(1, "ciel_panic(NULL, 0);");
                } else if falls_through && !ret.is_erased_value() {
                    self.line_indent(1, "ciel_panic(NULL, 0);");
                    self.line_indent(1, &format!("return {};", self.zero_value(&ret)));
                }
            }
        }

        self.current_return_ty = previous_return_ty;
        self.current_heap_locals = previous_heap_locals;
        self.current_param_locals = previous_param_locals;
        self.current_capture_locals = previous_capture_locals;
        self.current_closure_owner = previous_closure_owner;
        self.defer_stack.clear();
        self.loop_defer_starts.clear();
        self.line("}");
        Ok(())
    }

    fn emit_function_closure_wrapper(&mut self, wrapper: &FunctionClosureWrapper) {
        let (ret, params) = self
            .callable_ret_params(&wrapper.closure_ty)
            .expect("wrapper closure type is callable");
        self.line(&format!(
            "{} {{",
            self.function_closure_thunk_decl(&wrapper.closure_ty, &wrapper.function_ty)
        ));
        let env_name = self.function_closure_env_name(&wrapper.closure_ty, &wrapper.function_ty);
        self.line_indent(1, &format!("{env_name} *env = ({env_name} *)env_raw;"));
        let args = (0..params.iter().filter(|ty| !ty.is_erased_value()).count())
            .map(|idx| format!("arg{idx}"))
            .collect::<Vec<_>>()
            .join(", ");
        if ret.is_erased_value() {
            self.line_indent(1, &format!("env->func({args});"));
            self.line_indent(1, "return;");
        } else {
            self.line_indent(1, &format!("return env->func({args});"));
        }
        self.line("}");
    }

    fn emit_retained_closure_wrapper(
        &mut self,
        wrapper: &RetainedClosureWrapper,
    ) -> DiagResult<()> {
        let (ret, params) = self.callable_ret_params(&wrapper.target_ty)?;
        self.line(&format!(
            "{} {{",
            self.retained_closure_thunk_decl(&wrapper.target_ty, &wrapper.source_ty)
        ));
        let env_name = self.retained_closure_env_name(&wrapper.target_ty, &wrapper.source_ty);
        self.line_indent(1, &format!("{env_name} *env = ({env_name} *)env_raw;"));
        let args = (0..params.iter().filter(|ty| !ty.is_erased_value()).count())
            .map(|idx| format!("arg{idx}"))
            .collect::<Vec<_>>()
            .join(", ");
        let call_args = if args.is_empty() {
            "env->source.env".to_string()
        } else {
            format!("env->source.env, {args}")
        };
        if ret.is_erased_value() {
            self.line_indent(1, &format!("env->source.call({call_args});"));
            self.line_indent(1, "return;");
        } else {
            self.line_indent(1, &format!("return env->source.call({call_args});"));
        }
        self.line("}");
        Ok(())
    }

    fn closure_call_field_decl(&self, ty: &Ty) -> String {
        let (ret, params) = self
            .callable_ret_params(ty)
            .expect("closure value type is callable");
        let mut decls = vec!["void *env".to_string()];
        decls.extend(
            params
                .iter()
                .filter(|ty| !ty.is_erased_value())
                .enumerate()
                .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}"))),
        );
        self.c_return_decl(&ret, &format!("(*call)({})", decls.join(", ")))
    }

    fn retained_closure_witness_field_decl(&self, ty: &Ty, capability: &ConstraintRef) -> String {
        let ret = self.retained_closure_interface_ret(ty, capability);
        let params = self.retained_closure_interface_params(ty, capability);
        let params = params
            .iter()
            .filter(|ty| !ty.is_erased_value())
            .enumerate()
            .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}")))
            .collect::<Vec<_>>()
            .join(", ");
        self.c_return_decl(
            &ret,
            &format!(
                "(*{})({})",
                self.retained_closure_witness_field_name(capability),
                params
            ),
        )
    }

    fn retained_closure_interface_ret(&self, receiver_ty: &Ty, capability: &ConstraintRef) -> Ty {
        retained_closure_interface_signature(
            &self.program.checked.interfaces,
            receiver_ty,
            capability,
        )
        .map(|signature| signature.ret)
        .unwrap_or(Ty::Unknown)
    }

    fn retained_closure_interface_params(
        &self,
        receiver_ty: &Ty,
        capability: &ConstraintRef,
    ) -> Vec<Ty> {
        retained_closure_interface_signature(
            &self.program.checked.interfaces,
            receiver_ty,
            capability,
        )
        .map(|signature| signature.params)
        .unwrap_or_else(|| vec![Ty::pointer_to(Ty::Void)])
    }

    fn closure_thunk_decl(&self, closure: &ClosureDef) -> String {
        let (ret, params) = self
            .callable_ret_params(&closure.ty)
            .expect("closure thunk type is callable");
        let mut decls = Vec::new();
        if matches!(closure.ty, Ty::Closure { .. } | Ty::ClosureInstance { .. }) {
            decls.push("void *env_raw".to_string());
        }
        decls.extend(
            params
                .iter()
                .filter(|ty| !ty.is_erased_value())
                .enumerate()
                .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}"))),
        );
        let params = if decls.is_empty() {
            "void".to_string()
        } else {
            decls.join(", ")
        };
        self.c_return_decl(
            &ret,
            &format!(
                "{}({params})",
                self.closure_thunk_name(closure.owner, closure.id)
            ),
        )
    }

    fn function_closure_thunk_decl(&self, closure_ty: &Ty, function_ty: &Ty) -> String {
        let (ret, params) = self
            .callable_ret_params(closure_ty)
            .expect("function wrapper closure type is callable");
        let mut decls = vec!["void *env_raw".to_string()];
        decls.extend(
            params
                .iter()
                .filter(|ty| !ty.is_erased_value())
                .enumerate()
                .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}"))),
        );
        self.c_return_decl(
            &ret,
            &format!(
                "{}({})",
                self.function_closure_thunk_name(closure_ty, function_ty),
                decls.join(", ")
            ),
        )
    }

    fn retained_closure_thunk_decl(&self, target_ty: &Ty, source_ty: &Ty) -> String {
        let (ret, params) = self
            .callable_ret_params(target_ty)
            .expect("retained wrapper closure type is callable");
        let mut decls = vec!["void *env_raw".to_string()];
        decls.extend(
            params
                .iter()
                .filter(|ty| !ty.is_erased_value())
                .enumerate()
                .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}"))),
        );
        self.c_return_decl(
            &ret,
            &format!(
                "{}({})",
                self.retained_closure_thunk_name(target_ty, source_ty),
                decls.join(", ")
            ),
        )
    }

    fn callable_ret_params(&self, ty: &Ty) -> DiagResult<(Ty, Vec<Ty>)> {
        match ty.unqualified() {
            Ty::Closure { ret, params, .. }
            | Ty::ClosureInstance { ret, params, .. }
            | Ty::Function { ret, params, .. } => Ok(((**ret).clone(), params.clone())),
            other => Err(vec![Diagnostic::new(
                None,
                format!("internal error: `{other}` is not callable"),
            )]),
        }
    }

    fn emit_dynamic_vtable_layouts(&mut self) {
        let dynamic_types = self.plan.dynamic_types.clone();
        for (_, ty) in dynamic_types {
            let Ty::DynamicInterface { name, args } = &ty else {
                continue;
            };
            let vtable = self.dynamic_vtable_name(&ty);
            self.line(&format!("struct {vtable} {{"));
            for interface in self.dynamic_view_interfaces(name, args) {
                let field_ret = self.dynamic_interface_ret(&interface);
                let field_params = self.dynamic_interface_params(&interface);
                let params = field_params
                    .iter()
                    .filter(|ty| !ty.is_erased_value())
                    .enumerate()
                    .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}")))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.line(&format!(
                    "    {};",
                    self.c_return_decl(&field_ret, &format!("(*{})({})", interface.name, params)),
                ));
            }
            self.line("};");
            self.line("");
        }
    }

    fn emit_dynamic_shim_prototypes(&mut self) {
        let uses = self.plan.dynamic_impls.clone();
        for (_, dynamic_use) in uses {
            for interface in self.dynamic_use_interfaces(&dynamic_use) {
                if self
                    .impl_for_dynamic(&interface, &dynamic_use.concrete_ty)
                    .is_some()
                {
                    let ret = self.dynamic_interface_ret(&interface);
                    let params = self.dynamic_interface_params(&interface);
                    let params = params
                        .iter()
                        .filter(|ty| !ty.is_erased_value())
                        .enumerate()
                        .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}")))
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.line(&format!(
                        "{};",
                        self.c_return_decl(
                            &ret,
                            &format!(
                                "{}({})",
                                self.dynamic_shim_name(
                                    &dynamic_use.dyn_ty,
                                    &dynamic_use.concrete_ty,
                                    &interface.name
                                ),
                                params
                            )
                        )
                    ));
                }
            }
        }
    }

    fn emit_dynamic_shims_and_tables(&mut self) {
        let uses = self.plan.dynamic_impls.clone();
        for (_, dynamic_use) in uses {
            for interface in self.dynamic_use_interfaces(&dynamic_use) {
                let Some(implementation) = self
                    .impl_for_dynamic(&interface, &dynamic_use.concrete_ty)
                    .cloned()
                else {
                    continue;
                };
                let ret = self.dynamic_interface_ret(&interface);
                let params = self.dynamic_interface_params(&interface);
                let params_decl = params
                    .iter()
                    .filter(|ty| !ty.is_erased_value())
                    .enumerate()
                    .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}")))
                    .collect::<Vec<_>>()
                    .join(", ");
                let shim_name = self.dynamic_shim_name(
                    &dynamic_use.dyn_ty,
                    &dynamic_use.concrete_ty,
                    &interface.name,
                );
                self.line(&format!(
                    "{} {{",
                    self.c_return_decl(&ret, &format!("{shim_name}({params_decl})"))
                ));
                let mut args = Vec::new();
                let first_param = implementation
                    .params
                    .first()
                    .cloned()
                    .unwrap_or(Ty::Unknown);
                if matches!(first_param, Ty::Pointer { .. }) {
                    args.push(format!("({})arg0", self.c_type(&first_param)));
                } else {
                    args.push(format!("*({} *)arg0", self.c_type(&first_param)));
                }
                let mut physical_idx = 1;
                for param in implementation.params.iter().skip(1) {
                    if param.is_erased_value() {
                        continue;
                    }
                    args.push(format!("arg{physical_idx}"));
                    physical_idx += 1;
                }
                let call = format!(
                    "{}({})",
                    self.c_name(implementation.function_def),
                    args.join(", ")
                );
                if ret.is_erased_value() {
                    self.line_indent(1, &format!("{call};"));
                } else {
                    self.line_indent(1, &format!("return {call};"));
                }
                self.line("}");
            }
            let vtable = self.dynamic_vtable_name(&dynamic_use.dyn_ty);
            let table = self.dynamic_table_name(&dynamic_use.dyn_ty, &dynamic_use.concrete_ty);
            self.line(&format!("static const {vtable} {table} = {{"));
            for interface in self.dynamic_use_interfaces(&dynamic_use) {
                self.line(&format!(
                    "    .{} = {},",
                    interface.name,
                    self.dynamic_shim_name(
                        &dynamic_use.dyn_ty,
                        &dynamic_use.concrete_ty,
                        &interface.name
                    )
                ));
            }
            self.line("};");
        }
    }

    fn function_decl(&self, function: &CheckedFunction, _prototype: bool) -> String {
        let name = self.c_name(function.def_id);
        let params = function
            .params
            .iter()
            .filter(|(_, _, ty)| !ty.is_erased_value())
            .map(|(_, name, ty)| self.c_decl(ty, name))
            .collect::<Vec<_>>();
        let params = if params.is_empty() {
            "void".to_string()
        } else {
            params.join(", ")
        };
        self.c_return_decl(&function.ret, &format!("{name}({params})"))
    }

    fn c_name(&self, def_id: DefId) -> String {
        self.plan.name_map
            .get(&def_id)
            .cloned()
            .unwrap_or_else(|| format!("ciel_missing_{}", def_id.0))
    }

    fn c_return_decl(&self, ty: &Ty, name: &str) -> String {
        if ty.is_erased_value() {
            c_base_decl("void", name)
        } else {
            self.c_decl(ty, name)
        }
    }

    fn c_decl(&self, ty: &Ty, name: &str) -> String {
        match ty {
            Ty::Const(inner) => format!("const {}", self.c_decl(inner, name)),
            Ty::Never => c_base_decl("void", name),
            Ty::Void => c_base_decl("void", name),
            Ty::Bool => c_base_decl("bool", name),
            Ty::Char => c_base_decl("char", name),
            Ty::I8 => c_base_decl("int8_t", name),
            Ty::I16 => c_base_decl("int16_t", name),
            Ty::I32 => c_base_decl("int32_t", name),
            Ty::I64 => c_base_decl("int64_t", name),
            Ty::U8 => c_base_decl("uint8_t", name),
            Ty::U16 => c_base_decl("uint16_t", name),
            Ty::U32 => c_base_decl("uint32_t", name),
            Ty::U64 => c_base_decl("uint64_t", name),
            Ty::Usize => c_base_decl("size_t", name),
            Ty::F32 => c_base_decl("float", name),
            Ty::F64 => c_base_decl("double", name),
            Ty::CSpelling { spelling, .. } => c_base_decl(spelling, name),
            Ty::Pointer { inner, .. } => {
                let ptr_name = if matches!(**inner, Ty::Array { .. } | Ty::Function { .. }) {
                    format!("(*{name})")
                } else {
                    format!("*{name}")
                };
                self.c_decl(inner, &ptr_name)
            }
            Ty::Array { len, elem } => self.c_decl(elem, &format!("{name}[{len}]")),
            Ty::Slice(elem) => c_base_decl(&self.slice_name(elem), name),
            Ty::Named {
                name: ty_name,
                args,
            } => c_base_decl(&self.c_named_type(ty_name, args), name),
            Ty::DynamicInterface { .. } => c_base_decl(&self.dynamic_type_name(ty), name),
            Ty::Closure { .. } | Ty::ClosureInstance { .. } => {
                c_base_decl(&self.closure_type_name(ty), name)
            }
            Ty::Function { ret, params, .. } => {
                let params = params
                    .iter()
                    .filter(|ty| !ty.is_erased_value())
                    .enumerate()
                    .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}")))
                    .collect::<Vec<_>>();
                let params = if params.is_empty() {
                    "void".to_string()
                } else {
                    params.join(", ")
                };
                self.c_return_decl(ret, &format!("(*{name})({params})"))
            }
            Ty::Hole(_) | Ty::Generic(_) | Ty::Unknown => c_base_decl("void", name),
        }
    }

    fn c_pointer_decl(&self, ty: &Ty, name: &str) -> String {
        self.c_decl(
            &Ty::Pointer {
                nullable: false,
                inner: Box::new(ty.clone()),
            },
            name,
        )
    }

    fn c_pointer_type(&self, ty: &Ty) -> String {
        self.c_type(&Ty::Pointer {
            nullable: false,
            inner: Box::new(ty.clone()),
        })
    }

    fn c_sizeof_type(&self, ty: &Ty) -> String {
        match ty {
            Ty::Array { len, elem } => format!("{}[{}]", self.c_type(elem), len),
            _ => self.c_type(ty),
        }
    }

    fn c_type(&self, ty: &Ty) -> String {
        self.c_decl(ty, "").trim().to_string()
    }

    fn c_named_type(&self, name: &str, args: &[Ty]) -> String {
        if args.is_empty() {
            name.to_string()
        } else {
            format!(
                "{}_{}",
                name,
                args.iter()
                    .map(mangle_ty_fragment)
                    .collect::<Vec<_>>()
                    .join("_")
            )
        }
    }

    fn zero_value(&self, ty: &Ty) -> String {
        if ty.is_erased_value() {
            return String::new();
        }
        match ty {
            Ty::Const(inner) => self.zero_value(inner),
            Ty::Never => String::new(),
            Ty::Void => String::new(),
            Ty::Bool => "false".to_string(),
            Ty::Pointer { .. } | Ty::Function { .. } => "NULL".to_string(),
            Ty::I8
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
            | Ty::Char => "0".to_string(),
            Ty::Array { .. }
            | Ty::Slice(_)
            | Ty::Named { .. }
            | Ty::DynamicInterface { .. }
            | Ty::Closure { .. }
            | Ty::ClosureInstance { .. }
            | Ty::Hole(_)
            | Ty::Generic(_)
            | Ty::Unknown => {
                format!("({}){{0}}", self.c_type(ty))
            }
        }
    }

    fn slice_name(&self, elem: &Ty) -> String {
        format!("CielSlice_{}", mangle_ty_fragment(elem))
    }

    fn dynamic_type_name(&self, ty: &Ty) -> String {
        match ty {
            Ty::DynamicInterface { name, args } => {
                if args.is_empty() {
                    format!("CielDyn_{name}")
                } else {
                    format!(
                        "CielDyn_{}_{}",
                        name,
                        args.iter()
                            .map(mangle_ty_fragment)
                            .collect::<Vec<_>>()
                            .join("_")
                    )
                }
            }
            _ => "CielDyn_unknown".to_string(),
        }
    }

    fn dynamic_vtable_name(&self, ty: &Ty) -> String {
        format!("{}VTable", self.dynamic_type_name(ty))
    }

    fn dynamic_impl_key(&self, dyn_ty: &Ty, concrete_ty: &Ty) -> String {
        format!(
            "{}__{}",
            self.dynamic_type_name(dyn_ty),
            mangle_ty_fragment(concrete_ty)
        )
    }

    fn closure_type_name(&self, ty: &Ty) -> String {
        match ty {
            Ty::Closure {
                ret,
                params,
                constraints,
            } => {
                let sig_ty = Ty::Closure {
                    ret: ret.clone(),
                    params: params.clone(),
                    constraints: constraints.clone(),
                };
                format!("CielClosure_{}", mangle_ty_fragment(&sig_ty))
            }
            Ty::ClosureInstance { ret, params, .. } => {
                let sig_ty = Ty::Closure {
                    ret: ret.clone(),
                    params: params.clone(),
                    constraints: ConstraintBounds::default(),
                };
                format!("CielClosure_{}", mangle_ty_fragment(&sig_ty))
            }
            _ => "CielClosure_unknown".to_string(),
        }
    }

    fn retained_closure_witness_field_name(&self, capability: &ConstraintRef) -> String {
        format!("cap_{}", mangle_constraint_ref(capability))
    }

    fn retained_closure_witness_name(
        &self,
        target_ty: &Ty,
        source_ty: &Ty,
        capability: &ConstraintRef,
    ) -> String {
        format!(
            "ciel_retained_closure_witness_{}_{}_{}",
            mangle_constraint_ref(capability),
            mangle_ty_fragment(target_ty),
            mangle_ty_fragment(source_ty)
        )
    }

    fn closure_env_name(&self, owner: DefId, id: usize) -> String {
        format!("CielClosureEnv_{}_{}", owner.0, id)
    }

    fn closure_thunk_name(&self, owner: DefId, id: usize) -> String {
        format!("ciel_closure_thunk_{}_{}", owner.0, id)
    }

    fn function_closure_wrapper_key(&self, closure_ty: &Ty, function_ty: &Ty) -> String {
        format!(
            "{}__{}",
            mangle_ty_fragment(closure_ty),
            mangle_ty_fragment(function_ty)
        )
    }

    fn function_closure_env_name(&self, closure_ty: &Ty, function_ty: &Ty) -> String {
        format!(
            "CielClosureFnEnv_{}_{}",
            mangle_ty_fragment(closure_ty),
            mangle_ty_fragment(function_ty)
        )
    }

    fn function_closure_thunk_name(&self, closure_ty: &Ty, function_ty: &Ty) -> String {
        format!(
            "ciel_function_to_closure_{}_{}",
            mangle_ty_fragment(closure_ty),
            mangle_ty_fragment(function_ty)
        )
    }

    fn retained_closure_wrapper_key(&self, target_ty: &Ty, source_ty: &Ty) -> String {
        format!(
            "{}__{}",
            mangle_ty_fragment(target_ty),
            mangle_ty_fragment(source_ty)
        )
    }

    fn retained_closure_env_name(&self, target_ty: &Ty, source_ty: &Ty) -> String {
        format!(
            "CielRetainedClosureEnv_{}_{}",
            mangle_ty_fragment(target_ty),
            mangle_ty_fragment(source_ty)
        )
    }

    fn retained_closure_thunk_name(&self, target_ty: &Ty, source_ty: &Ty) -> String {
        format!(
            "ciel_retained_closure_to_closure_{}_{}",
            mangle_ty_fragment(target_ty),
            mangle_ty_fragment(source_ty)
        )
    }

    fn retained_closure_source_pointer_expr(&self, witness: &RetainedClosureWitness) -> String {
        let target_ptr = format!("({})arg0", self.c_pointer_type(&witness.target_ty));
        self.retained_closure_source_pointer_expr_from_target_ptr(witness, &target_ptr)
    }

    fn retained_closure_source_pointer_expr_from_target_ptr(
        &self,
        witness: &RetainedClosureWitness,
        target_ptr: &str,
    ) -> String {
        match witness.source_ty.unqualified() {
            Ty::Function { abi: None, .. } => {
                let env_name =
                    self.function_closure_env_name(&witness.target_ty, &witness.source_ty);
                format!("&((({env_name} *)(({target_ptr})->env))->func)")
            }
            Ty::Closure { .. }
                if retained_closure_needs_wrapper(&witness.target_ty, &witness.source_ty) =>
            {
                let env_name =
                    self.retained_closure_env_name(&witness.target_ty, &witness.source_ty);
                format!("&((({env_name} *)(({target_ptr})->env))->source)")
            }
            _ => format!(
                "({})({target_ptr})",
                self.c_pointer_type(&witness.source_ty)
            ),
        }
    }

    fn actor_dispatch_name(&self, state_ty: &Ty, message_ty: &Ty, handler_ty: &Ty) -> String {
        format!(
            "ciel_actor_dispatch_{}_{}_{}",
            mangle_ty_fragment(state_ty),
            mangle_ty_fragment(message_ty),
            mangle_ty_fragment(handler_ty)
        )
    }

    fn actor_handler_call_expr(
        &self,
        handler_ty: &Ty,
        handler: &str,
        state: &str,
        message: &str,
    ) -> DiagResult<String> {
        match handler_ty.unqualified() {
            Ty::Function { .. } => Ok(format!("({handler})({state}, {message})")),
            Ty::Closure { .. } | Ty::ClosureInstance { .. } => Ok(format!(
                "({handler}).call(({handler}).env, {state}, {message})"
            )),
            other => Err(vec![Diagnostic::new(
                None,
                format!("internal error: actor handler `{other}` is not callable"),
            )]),
        }
    }

    fn dynamic_table_name(&self, dyn_ty: &Ty, concrete_ty: &Ty) -> String {
        format!(
            "ciel_vtable_{}_{}",
            mangle_ty_fragment(dyn_ty),
            mangle_ty_fragment(concrete_ty)
        )
    }

    fn dynamic_shim_name(&self, dyn_ty: &Ty, concrete_ty: &Ty, interface_name: &str) -> String {
        format!(
            "ciel_dyn_shim_{}_{}_{}",
            interface_name,
            mangle_ty_fragment(dyn_ty),
            mangle_ty_fragment(concrete_ty)
        )
    }

    fn dynamic_use_interfaces(&self, dynamic_use: &DynamicImplUse) -> Vec<CheckedInterfaceRef> {
        let Ty::DynamicInterface { name, args } = &dynamic_use.dyn_ty else {
            return Vec::new();
        };
        self.dynamic_view_interfaces(name, args)
    }

    fn dynamic_view_interfaces(&self, name: &str, args: &[Ty]) -> Vec<CheckedInterfaceRef> {
        checked_interface_view(
            &self.program.checked.interfaces,
            &self.program.checked.interface_aliases,
            name,
            args,
        )
    }

    fn impl_for_dynamic(
        &self,
        interface: &CheckedInterfaceRef,
        concrete_ty: &Ty,
    ) -> Option<&CheckedImpl> {
        self.program.checked.impls.iter().find(|implementation| {
            impl_matches_dynamic_interface(implementation, interface, concrete_ty)
        })
    }

    fn impl_for_retained_closure_witness(
        &self,
        capability: &ConstraintRef,
        source_ty: &Ty,
    ) -> Option<&CheckedImpl> {
        self.program.checked.impls.iter().find(|implementation| {
            impl_matches_interface_receiver(
                implementation,
                &capability.name,
                &capability.args,
                source_ty.unqualified(),
            )
        })
    }

    fn dynamic_interface_ret(&self, interface_ref: &CheckedInterfaceRef) -> Ty {
        dynamic_interface_signature(&self.program.checked.interfaces, interface_ref)
            .map(|signature| signature.ret)
            .unwrap_or(Ty::Unknown)
    }

    fn dynamic_interface_params(&self, interface_ref: &CheckedInterfaceRef) -> Vec<Ty> {
        dynamic_interface_signature(&self.program.checked.interfaces, interface_ref)
            .map(|signature| signature.params)
            .unwrap_or_else(|| vec![Ty::pointer_to(Ty::Void)])
    }

    fn find_ciel_main(&self) -> Option<&CheckedFunction> {
        self.program
            .checked
            .functions
            .iter()
            .find(|function| function.name == "main" && function.body.is_some())
    }

    fn emit_current_defers(&mut self, indent: usize) {
        if let Some(frame) = self.defer_stack.last() {
            let calls = frame.clone();
            for call in calls.iter().rev() {
                self.line_indent(indent, &format!("{call};"));
            }
        }
    }

    fn emit_all_defers(&mut self, indent: usize) {
        let frames = self.defer_stack.clone();
        for frame in frames.iter().rev() {
            for call in frame.iter().rev() {
                self.line_indent(indent, &format!("{call};"));
            }
        }
    }

    fn emit_loop_defers(&mut self, indent: usize) {
        let start = self.loop_defer_starts.last().copied().unwrap_or(0);
        let frames = self.defer_stack.clone();
        for frame in frames.iter().skip(start).rev() {
            for call in frame.iter().rev() {
                self.line_indent(indent, &format!("{call};"));
            }
        }
    }

    fn next_temp(&mut self, prefix: &str) -> String {
        let id = self.temp_counter;
        self.temp_counter += 1;
        format!("ciel_{prefix}_{id}")
    }

    fn local_is_heap(&self, id: LocalId) -> bool {
        self.current_heap_locals.contains(&id)
    }

    fn local_c_name(&self, id: LocalId, source_name: &str) -> String {
        self.current_param_locals
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("{source_name}__{}", id.0))
    }

    fn emit_line_directive(&mut self, span: crate::span::Span) {
        let file = self.source_map.file_path(span.file).display().to_string();
        let (line, _) = self.source_map.line_col(span.file, span.start);
        self.line(&format!("#line {line} \"{}\"", escape_c_string(&file)));
    }

    fn location_args(&self, span: crate::span::Span) -> (String, String) {
        let file = self.source_map.file_path(span.file).display().to_string();
        let (line, _) = self.source_map.line_col(span.file, span.start);
        if let Some(location) = self.plan.source_locations.get(&(span.file.0, line)) {
            (
                format!("{}.file", location.name),
                format!("{}.line", location.name),
            )
        } else {
            (format!("\"{}\"", escape_c_string(&file)), line.to_string())
        }
    }

    fn line(&mut self, text: &str) {
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn line_indent(&mut self, indent: usize, text: &str) {
        self.out.push_str(&"    ".repeat(indent));
        self.line(text);
    }
}

fn c_base_decl(base: &str, name: &str) -> String {
    if name.is_empty() {
        base.to_string()
    } else {
        format!("{base} {name}")
    }
}

fn result_args(ty: &Ty) -> Option<(&Ty, &Ty)> {
    let Ty::Named { name, args } = ty.unqualified() else {
        return None;
    };
    if name == "Result" && args.len() == 2 {
        Some((&args[0], &args[1]))
    } else {
        None
    }
}

fn string_literal_len(raw: &str) -> usize {
    let mut len = 0;
    let mut chars = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw)
        .chars()
        .peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('x') => {
                    chars.next();
                    chars.next();
                    len += 1;
                }
                Some(_) => len += 1,
                None => {}
            }
        } else {
            len += 1;
        }
    }
    len
}

fn span_key(span: crate::span::Span) -> (usize, usize, usize) {
    (span.file.0, span.start, span.end)
}

fn expr_needs_stmt_lowering(expr: &TExpr) -> bool {
    match &expr.kind {
        TExprKind::Try(_)
        | TExprKind::Slice { .. }
        | TExprKind::MakeDynamicInterface { .. }
        | TExprKind::BuiltinCloneMessage { .. }
        | TExprKind::MetaAsRefRepr { .. }
        | TExprKind::MetaIntoRepr { .. }
        | TExprKind::MetaFromRepr { .. }
        | TExprKind::ActorSpawn { .. }
        | TExprKind::ActorSend { .. }
        | TExprKind::ActorStop { .. }
        | TExprKind::ActorJoin { .. }
        | TExprKind::FunctionToClosure(_)
        | TExprKind::RetainClosure { .. } => true,
        TExprKind::Unary { expr, .. } | TExprKind::Cast { expr, .. } => {
            expr_needs_stmt_lowering(expr)
        }
        TExprKind::Binary { left, right, .. } => {
            expr_needs_stmt_lowering(left) || expr_needs_stmt_lowering(right)
        }
        TExprKind::Call { callee, args, .. } => {
            expr_needs_stmt_lowering(callee)
                || args
                    .iter()
                    .any(|arg| arg.ty.is_erased_value() || expr_needs_stmt_lowering(arg))
        }
        TExprKind::Closure { captures, .. } => !captures.is_empty(),
        TExprKind::ArrayToSlice(inner) => expr_needs_stmt_lowering(inner),
        TExprKind::DynamicInterfaceCall { receiver, args, .. }
        | TExprKind::RetainedClosureInterfaceCall { receiver, args, .. } => {
            expr_needs_stmt_lowering(receiver)
                || args
                    .iter()
                    .any(|arg| arg.ty.is_erased_value() || expr_needs_stmt_lowering(arg))
        }
        TExprKind::Field { base, .. } | TExprKind::Arrow { base, .. } => {
            expr.ty.is_erased_value() || expr_needs_stmt_lowering(base)
        }
        TExprKind::Index { base, index } => {
            expr_needs_stmt_lowering(base) || expr_needs_stmt_lowering(index)
        }
        TExprKind::TypeSize { .. } | TExprKind::TypeAlign { .. } => false,
        TExprKind::StructLiteral { fields, .. } => fields
            .iter()
            .any(|(_, value)| value.ty.is_erased_value() || expr_needs_stmt_lowering(value)),
        TExprKind::EnumLiteral { payload, .. } => payload
            .iter()
            .any(|value| value.ty.is_erased_value() || expr_needs_stmt_lowering(value)),
        TExprKind::ArrayLiteral(elements) => {
            expr.ty.is_erased_value() || elements.iter().any(expr_needs_stmt_lowering)
        }
        TExprKind::ArrayRepeat { element, .. } => {
            expr.ty.is_erased_value() || expr_needs_stmt_lowering(element)
        }
        TExprKind::Local(..)
        | TExprKind::Function(_, _)
        | TExprKind::GenericFunction { .. }
        | TExprKind::Literal(_) => false,
    }
}

fn for_stmt_needs_stmt_lowering(
    init: Option<&TForInit>,
    cond: Option<&TExpr>,
    step: Option<&TForInit>,
) -> bool {
    init.is_some_and(for_clause_needs_stmt_lowering)
        || cond.is_some_and(expr_needs_stmt_lowering)
        || step.is_some_and(for_clause_needs_stmt_lowering)
}

fn for_clause_needs_stmt_lowering(clause: &TForInit) -> bool {
    match clause {
        TForInit::VarDecl { init, .. } => init.as_ref().is_some_and(expr_needs_stmt_lowering),
        TForInit::Assign { target, value } => {
            expr_needs_stmt_lowering(target) || expr_needs_stmt_lowering(value)
        }
        TForInit::Expr(expr) => expr_needs_stmt_lowering(expr),
    }
}

fn escape_c_include(include: &str) -> String {
    include.replace('\\', "\\\\").replace('"', "\\\"")
}

fn escape_c_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn checked_integer_op_helper(op: &str, ty: &Ty) -> Option<String> {
    let prefix = match op {
        "+" => "add",
        "-" => "sub",
        "*" => "mul",
        "/" => "div",
        "%" => "rem",
        _ => return None,
    };
    let suffix = checked_integer_helper_suffix(ty)?;
    Some(format!("ciel_{prefix}_{suffix}"))
}

fn checked_integer_helper_suffix(ty: &Ty) -> Option<&'static str> {
    Some(match ty.unqualified() {
        Ty::I8 => "i8",
        Ty::I16 => "i16",
        Ty::I32 => "i32",
        Ty::I64 => "i64",
        Ty::U8 => "u8",
        Ty::U16 => "u16",
        Ty::U32 => "u32",
        Ty::U64 => "u64",
        Ty::Usize => "usize",
        _ => return None,
    })
}

fn checked_integer_unary_helper(ty: &Ty) -> Option<String> {
    if ty.unqualified().is_signed_integer() {
        Some(format!("ciel_neg_{}", checked_integer_helper_suffix(ty)?))
    } else {
        None
    }
}
