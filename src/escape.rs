use std::collections::{HashMap, HashSet};

use crate::{
    ast::UnaryOp,
    hir::LocalId,
    mono::MonoProgram,
    resolve::DefId,
    thir::{CheckedFunction, TBlock, TExpr, TExprKind, TForInit, TPattern, TStmt, TStmtKind},
    types::Ty,
};

#[derive(Clone, Debug, Default)]
pub struct EscapeProgram {
    pub functions: HashMap<DefId, FunctionEscape>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FunctionEscape {
    pub heap_locals: HashSet<LocalId>,
    pub escaping_params: HashSet<usize>,
}

pub fn analyze_escapes(program: &MonoProgram) -> EscapeProgram {
    let functions = program
        .checked
        .functions
        .iter()
        .map(|function| (function.def_id, function))
        .collect::<HashMap<_, _>>();
    let mut summaries = program
        .checked
        .functions
        .iter()
        .map(|function| {
            let escape = if function.body.is_none() {
                extern_escape_summary(function)
            } else {
                FunctionEscape::default()
            };
            (function.def_id, escape)
        })
        .collect::<HashMap<_, _>>();

    loop {
        let mut changed = false;
        for function in &program.checked.functions {
            if function.body.is_none() {
                continue;
            }
            let next = analyze_function(function, &functions, &summaries);
            let entry = summaries.entry(function.def_id).or_default();
            if *entry != next {
                *entry = next;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    EscapeProgram {
        functions: summaries,
    }
}

fn extern_escape_summary(function: &CheckedFunction) -> FunctionEscape {
    let mut escape = FunctionEscape::default();
    if function.noescape {
        return escape;
    }
    for (idx, (_, _, ty)) in function.params.iter().enumerate() {
        if ty_can_carry_pointer(ty) {
            escape.escaping_params.insert(idx);
        }
    }
    escape
}

fn analyze_function(
    function: &CheckedFunction,
    functions: &HashMap<DefId, &CheckedFunction>,
    summaries: &HashMap<DefId, FunctionEscape>,
) -> FunctionEscape {
    let mut analyzer = FunctionAnalyzer {
        functions,
        summaries,
        escape: FunctionEscape::default(),
        local_to_param: function
            .params
            .iter()
            .enumerate()
            .filter_map(|(idx, (local_id, _, _))| local_id.map(|id| (id, idx)))
            .collect(),
        aliases: HashMap::new(),
    };
    if let Some(body) = &function.body {
        analyzer.scan_block(body);
    }
    analyzer.escape
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum StorageSource {
    Local(LocalId),
    Param(usize),
}

struct FunctionAnalyzer<'a> {
    functions: &'a HashMap<DefId, &'a CheckedFunction>,
    summaries: &'a HashMap<DefId, FunctionEscape>,
    escape: FunctionEscape,
    local_to_param: HashMap<LocalId, usize>,
    aliases: HashMap<LocalId, Vec<StorageSource>>,
}

impl<'a> FunctionAnalyzer<'a> {
    fn scan_block(&mut self, block: &TBlock) {
        for stmt in &block.statements {
            self.scan_stmt(stmt);
        }
    }

    fn scan_stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::Block(block) => self.scan_block(block),
            TStmtKind::VarDecl { local_id, init, .. } => {
                if let Some(init) = init {
                    self.scan_expr(init);
                    self.update_alias(*local_id, init);
                } else {
                    self.aliases.remove(local_id);
                }
            }
            TStmtKind::Assign { target, value } => {
                self.scan_expr(target);
                self.scan_expr(value);
                if let TExprKind::Local(local_id, _) = &target.kind {
                    self.update_alias(*local_id, value);
                } else {
                    self.escape_sources(value);
                }
            }
            TStmtKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                self.scan_expr(cond);
                self.scan_block(then_block);
                if let Some(else_branch) = else_branch {
                    self.scan_stmt(else_branch);
                }
            }
            TStmtKind::While { cond, body } => {
                self.scan_expr(cond);
                self.scan_block(body);
            }
            TStmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(init) = init {
                    self.scan_for_clause(init);
                }
                if let Some(cond) = cond {
                    self.scan_expr(cond);
                }
                if let Some(step) = step {
                    self.scan_for_clause(step);
                }
                self.scan_block(body);
            }
            TStmtKind::Switch {
                expr,
                cases,
                default,
                ..
            } => {
                self.scan_expr(expr);
                let mut sources = Vec::new();
                self.collect_storage_sources(expr, &mut sources);
                for case in cases {
                    self.update_pattern_aliases(&case.pattern, &sources);
                    for stmt in &case.statements {
                        self.scan_stmt(stmt);
                    }
                }
                for stmt in default {
                    self.scan_stmt(stmt);
                }
            }
            TStmtKind::Defer(expr) | TStmtKind::ResourceCleanup(expr) | TStmtKind::Expr(expr) => {
                self.scan_expr(expr);
            }
            TStmtKind::Return(Some(expr)) => {
                self.scan_expr(expr);
                self.escape_sources(expr);
            }
            TStmtKind::Return(None)
            | TStmtKind::Break
            | TStmtKind::Continue
            | TStmtKind::Unsupported => {}
        }
    }

    fn scan_for_clause(&mut self, clause: &TForInit) {
        match clause {
            TForInit::VarDecl { local_id, init, .. } => {
                if let Some(init) = init {
                    self.scan_expr(init);
                    self.update_alias(*local_id, init);
                } else {
                    self.aliases.remove(local_id);
                }
            }
            TForInit::Assign { target, value } => {
                self.scan_expr(target);
                self.scan_expr(value);
                if let TExprKind::Local(local_id, _) = &target.kind {
                    self.update_alias(*local_id, value);
                } else {
                    self.escape_sources(value);
                }
            }
            TForInit::Expr(expr) => self.scan_expr(expr),
        }
    }

    fn update_pattern_aliases(&mut self, pattern: &TPattern, sources: &[StorageSource]) {
        match pattern {
            TPattern::Wildcard { .. } => {}
            TPattern::Binding { local_id, ty, .. } => {
                if ty_can_carry_pointer(ty) {
                    self.aliases.insert(*local_id, sources.to_vec());
                } else {
                    self.aliases.remove(local_id);
                }
            }
            TPattern::Variant { payload, .. } => {
                for pattern in payload {
                    self.update_pattern_aliases(pattern, sources);
                }
            }
        }
    }

    fn scan_closure_body(&mut self, body: &crate::thir::TClosureBody) {
        match body {
            crate::thir::TClosureBody::Expr(expr) => self.scan_expr(expr),
            crate::thir::TClosureBody::Block(block) => self.scan_block(block),
        }
    }

    fn scan_expr(&mut self, expr: &TExpr) {
        match &expr.kind {
            TExprKind::Move(expr)
            | TExprKind::Unary { expr, .. }
            | TExprKind::Cast { expr, .. } => self.scan_expr(expr),
            TExprKind::Try { expr, .. } => self.scan_expr(expr),
            TExprKind::Await { future } | TExprKind::AsyncBlockOn { future } => {
                self.scan_expr(future)
            }
            TExprKind::AsyncSelect { arms, .. } => {
                for arm in arms {
                    self.scan_expr(&arm.future);
                    self.scan_expr(&arm.body);
                }
            }
            TExprKind::AsyncSleep { ms, .. } => self.scan_expr(ms),
            TExprKind::AsyncOpFuture { op, .. } => self.scan_expr(op),
            TExprKind::AsyncSpawn { body, .. } => self.scan_expr(body),
            TExprKind::AsyncTaskCancel { task, .. }
            | TExprKind::AsyncTaskIsFinished { task, .. } => self.scan_expr(task),
            TExprKind::AsyncChannelSend { sender, value, .. }
            | TExprKind::AsyncChannelTrySend { sender, value, .. } => {
                self.scan_expr(sender);
                self.scan_expr(value);
            }
            TExprKind::AsyncChannelReserve { sender, .. } => self.scan_expr(sender),
            TExprKind::AsyncChannelRecv { receiver, .. } => self.scan_expr(receiver),
            TExprKind::AsyncChannelPermitSend { permit, value, .. } => {
                self.scan_expr(permit);
                self.scan_expr(value);
            }
            TExprKind::Binary { left, right, .. } => {
                self.scan_expr(left);
                self.scan_expr(right);
            }
            TExprKind::Call { callee, args, .. } => {
                self.scan_expr(callee);
                for arg in args {
                    self.scan_expr(arg);
                }
                self.apply_call_escape(callee, args);
            }
            TExprKind::UnsafeBlock { statements, value } => {
                for stmt in statements {
                    self.scan_stmt(stmt);
                }
                if let Some(value) = value {
                    self.scan_expr(value);
                }
            }
            TExprKind::Closure { captures, body, .. } => {
                for capture in captures {
                    let capture_expr = TExpr {
                        span: expr.span,
                        ty: capture.ty.clone(),
                        kind: TExprKind::Local(capture.local_id, capture.name.clone()),
                    };
                    self.scan_expr(&capture_expr);
                }
                self.scan_closure_body(body);
            }
            TExprKind::FunctionToClosure(inner)
            | TExprKind::RetainClosure { expr: inner, .. }
            | TExprKind::SliceToConst(inner) => self.scan_expr(inner),
            TExprKind::ArrayToSlice(expr) => self.scan_expr(expr),
            TExprKind::RawSliceFromPtr { ptr, len, .. } => {
                self.scan_expr(ptr);
                self.scan_expr(len);
            }
            TExprKind::MakeDynamicInterface { expr, .. } => {
                self.scan_expr(expr);
                self.escape_sources(expr);
            }
            TExprKind::ErrorBox { expr, .. } => {
                self.scan_expr(expr);
                self.escape_sources(expr);
            }
            TExprKind::DynamicInterfaceCall { receiver, args, .. }
            | TExprKind::RetainedClosureInterfaceCall { receiver, args, .. } => {
                self.scan_expr(receiver);
                self.escape_sources(receiver);
                for arg in args {
                    self.scan_expr(arg);
                    self.escape_sources(arg);
                }
            }
            TExprKind::CloneMessage { value, .. } => {
                self.scan_expr(value);
                self.escape_sources(value);
            }
            TExprKind::Field { base, .. } | TExprKind::Arrow { base, .. } => {
                self.scan_expr(base);
            }
            TExprKind::Index { base, index } => {
                self.scan_expr(base);
                self.scan_expr(index);
            }
            TExprKind::Slice { base, start, end } => {
                self.scan_expr(base);
                if let Some(start) = start {
                    self.scan_expr(start);
                }
                if let Some(end) = end {
                    self.scan_expr(end);
                }
            }
            TExprKind::MetaIntoRepr { value, .. } | TExprKind::MetaFromRepr { value, .. } => {
                self.scan_expr(value)
            }
            TExprKind::MetaAsRefRepr { value, .. } => {
                self.scan_expr(value);
                self.escape_sources(value);
            }
            TExprKind::ActorSpawn {
                state_arg, handler, ..
            } => {
                self.scan_expr(state_arg);
                self.scan_expr(handler);
            }
            TExprKind::ActorSend { actor, value, .. } => {
                self.scan_expr(actor);
                self.scan_expr(value);
            }
            TExprKind::ActorStop { actor, .. } | TExprKind::ActorJoin { actor, .. } => {
                self.scan_expr(actor);
            }
            TExprKind::TypeSize { .. }
            | TExprKind::TypeAlign { .. }
            | TExprKind::TypeNeedsGcScan { .. } => {}
            TExprKind::StructLiteral { fields, .. } => {
                for (_, value) in fields {
                    self.scan_expr(value);
                }
            }
            TExprKind::ArrayLiteral(elements) => {
                for element in elements {
                    self.scan_expr(element);
                }
            }
            TExprKind::ArrayRepeat { element, .. } => self.scan_expr(element),
            TExprKind::EnumLiteral { payload, .. } => {
                for value in payload {
                    self.scan_expr(value);
                }
            }
            TExprKind::Local(..)
            | TExprKind::Function(_, _)
            | TExprKind::GenericFunction { .. }
            | TExprKind::Literal(_) => {}
        }
    }

    fn apply_call_escape(&mut self, callee: &TExpr, args: &[TExpr]) {
        match &callee.kind {
            TExprKind::Function(def_id, _) => {
                let Some(callee_function) = self.functions.get(def_id) else {
                    self.escape_pointer_args(args);
                    return;
                };
                let escaping_params = self
                    .summaries
                    .get(def_id)
                    .cloned()
                    .unwrap_or_else(|| extern_escape_summary(callee_function));
                for param_idx in escaping_params.escaping_params {
                    if let Some(arg) = args.get(param_idx) {
                        self.escape_sources(arg);
                    }
                }
            }
            TExprKind::GenericFunction { .. } => self.escape_pointer_args(args),
            _ => self.escape_pointer_args(args),
        }
    }

    fn escape_pointer_args(&mut self, args: &[TExpr]) {
        for arg in args {
            if ty_can_carry_pointer(&arg.ty) {
                self.escape_sources(arg);
            }
        }
    }

    fn update_alias(&mut self, local_id: LocalId, value: &TExpr) {
        let sources = self.storage_sources(value);
        if sources.is_empty() {
            self.aliases.remove(&local_id);
        } else {
            self.aliases.insert(local_id, sources);
        }
    }

    fn escape_sources(&mut self, expr: &TExpr) {
        for source in self.storage_sources(expr) {
            match source {
                StorageSource::Local(local_id) => {
                    self.escape.heap_locals.insert(local_id);
                }
                StorageSource::Param(idx) => {
                    self.escape.escaping_params.insert(idx);
                }
            }
        }
    }

    fn storage_sources(&self, expr: &TExpr) -> Vec<StorageSource> {
        let mut out = Vec::new();
        self.collect_storage_sources(expr, &mut out);
        out.sort_by_key(storage_source_key);
        out.dedup();
        out
    }

    fn collect_storage_sources(&self, expr: &TExpr, out: &mut Vec<StorageSource>) {
        match &expr.kind {
            TExprKind::Unary {
                op: UnaryOp::Addr,
                expr,
            } => {
                if let TExprKind::Local(local_id, _) = &expr.kind {
                    if let Some(param_idx) = self.local_to_param.get(local_id) {
                        out.push(StorageSource::Param(*param_idx));
                    } else {
                        out.push(StorageSource::Local(*local_id));
                    }
                } else {
                    self.collect_storage_sources(expr, out);
                }
            }
            TExprKind::ArrayToSlice(inner) => {
                if let TExprKind::Local(local_id, _) = &inner.kind {
                    if let Some(param_idx) = self.local_to_param.get(local_id) {
                        out.push(StorageSource::Param(*param_idx));
                    } else {
                        out.push(StorageSource::Local(*local_id));
                    }
                } else {
                    self.collect_storage_sources(inner, out);
                }
            }
            TExprKind::Move(inner) | TExprKind::SliceToConst(inner) => {
                self.collect_storage_sources(inner, out)
            }
            TExprKind::RawSliceFromPtr { ptr, len, .. } => {
                self.collect_storage_sources(ptr, out);
                self.collect_storage_sources(len, out);
            }
            TExprKind::Slice { base, .. } => {
                if matches!(base.ty, Ty::Array { .. })
                    && let TExprKind::Local(local_id, _) = &base.kind
                {
                    if let Some(param_idx) = self.local_to_param.get(local_id) {
                        out.push(StorageSource::Param(*param_idx));
                    } else {
                        out.push(StorageSource::Local(*local_id));
                    }
                } else {
                    self.collect_storage_sources(base, out);
                }
            }
            TExprKind::Local(local_id, _) => {
                if let Some(sources) = self.aliases.get(local_id) {
                    out.extend(sources.iter().cloned());
                } else if self.local_to_param.contains_key(local_id)
                    && ty_can_carry_pointer(&expr.ty)
                {
                    if let Some(param_idx) = self.local_to_param.get(local_id) {
                        out.push(StorageSource::Param(*param_idx));
                    }
                }
            }
            TExprKind::StructLiteral { fields, .. } => {
                for (_, value) in fields {
                    self.collect_storage_sources(value, out);
                }
            }
            TExprKind::Closure { captures, .. } => {
                for capture in captures {
                    if let Some(sources) = self.aliases.get(&capture.local_id) {
                        out.extend(sources.iter().cloned());
                    } else {
                        let capture_expr = TExpr {
                            span: expr.span,
                            ty: capture.ty.clone(),
                            kind: TExprKind::Local(capture.local_id, capture.name.clone()),
                        };
                        self.collect_storage_sources(&capture_expr, out);
                    }
                }
            }
            TExprKind::FunctionToClosure(expr) | TExprKind::RetainClosure { expr, .. } => {
                self.collect_storage_sources(expr, out)
            }
            TExprKind::ArrayLiteral(elements) => {
                for element in elements {
                    self.collect_storage_sources(element, out);
                }
            }
            TExprKind::ArrayRepeat { element, .. } => self.collect_storage_sources(element, out),
            TExprKind::EnumLiteral { payload, .. } => {
                for value in payload {
                    self.collect_storage_sources(value, out);
                }
            }
            TExprKind::Cast { expr, .. } => self.collect_storage_sources(expr, out),
            TExprKind::Try { expr, .. } => self.collect_storage_sources(expr, out),
            TExprKind::Await { future } | TExprKind::AsyncBlockOn { future } => {
                self.collect_storage_sources(future, out)
            }
            TExprKind::AsyncSelect { arms, .. } => {
                for arm in arms {
                    self.collect_storage_sources(&arm.future, out);
                    self.collect_storage_sources(&arm.body, out);
                }
            }
            TExprKind::AsyncSleep { ms, .. } => self.collect_storage_sources(ms, out),
            TExprKind::AsyncOpFuture { op, .. } => self.collect_storage_sources(op, out),
            TExprKind::AsyncSpawn { body, .. } => self.collect_storage_sources(body, out),
            TExprKind::AsyncTaskCancel { task, .. }
            | TExprKind::AsyncTaskIsFinished { task, .. } => {
                self.collect_storage_sources(task, out)
            }
            TExprKind::AsyncChannelSend { sender, value, .. }
            | TExprKind::AsyncChannelTrySend { sender, value, .. } => {
                self.collect_storage_sources(sender, out);
                self.collect_storage_sources(value, out);
            }
            TExprKind::AsyncChannelReserve { sender, .. } => {
                self.collect_storage_sources(sender, out);
            }
            TExprKind::AsyncChannelRecv { receiver, .. } => {
                self.collect_storage_sources(receiver, out);
            }
            TExprKind::AsyncChannelPermitSend { permit, value, .. } => {
                self.collect_storage_sources(permit, out);
                self.collect_storage_sources(value, out);
            }
            TExprKind::Binary { left, right, .. } => {
                self.collect_storage_sources(left, out);
                self.collect_storage_sources(right, out);
            }
            TExprKind::MetaAsRefRepr { value, .. }
            | TExprKind::MetaIntoRepr { value, .. }
            | TExprKind::MetaFromRepr { value, .. } => {
                self.collect_storage_sources(value, out);
            }
            TExprKind::ActorSpawn {
                state_arg, handler, ..
            } => {
                self.collect_storage_sources(state_arg, out);
                self.collect_storage_sources(handler, out);
            }
            TExprKind::ActorSend { actor, value, .. } => {
                self.collect_storage_sources(actor, out);
                self.collect_storage_sources(value, out);
            }
            TExprKind::ActorStop { actor, .. } | TExprKind::ActorJoin { actor, .. } => {
                self.collect_storage_sources(actor, out);
            }
            TExprKind::TypeSize { .. }
            | TExprKind::TypeAlign { .. }
            | TExprKind::TypeNeedsGcScan { .. } => {}
            TExprKind::Call { .. }
            | TExprKind::UnsafeBlock { value: None, .. }
            | TExprKind::MakeDynamicInterface { .. }
            | TExprKind::ErrorBox { .. }
            | TExprKind::DynamicInterfaceCall { .. }
            | TExprKind::RetainedClosureInterfaceCall { .. }
            | TExprKind::CloneMessage { .. }
            | TExprKind::Field { .. }
            | TExprKind::Arrow { .. }
            | TExprKind::Index { .. }
            | TExprKind::Unary { .. }
            | TExprKind::Function(_, _)
            | TExprKind::GenericFunction { .. }
            | TExprKind::Literal(_) => {}
            TExprKind::UnsafeBlock {
                value: Some(value), ..
            } => self.collect_storage_sources(value, out),
        }
    }
}

fn storage_source_key(source: &StorageSource) -> (usize, usize) {
    match source {
        StorageSource::Local(id) => (0, id.0),
        StorageSource::Param(idx) => (1, *idx),
    }
}

fn ty_can_carry_pointer(ty: &Ty) -> bool {
    match ty {
        Ty::Pointer { .. }
        | Ty::Slice { .. }
        | Ty::DynamicInterface { .. }
        | Ty::Function { .. }
        | Ty::Closure { .. }
        | Ty::ClosureInstance { .. } => true,
        Ty::Array { elem, .. } => ty_can_carry_pointer(elem),
        Ty::Named { .. } => true,
        _ => false,
    }
}
