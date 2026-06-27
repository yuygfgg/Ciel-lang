use crate::{
    ast::{BinaryOp, BindingMutability, Literal, UnaryOp},
    hir::{ConstraintExpr, FunctionDecl, Local, LocalId, Module},
    resolve::{DefId, ModuleId, ResolvedProgram},
    span::Span,
    types::Ty,
};

#[derive(Clone, Debug)]
pub struct CheckedProgram {
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
}

#[derive(Clone, Debug)]
pub struct CheckedOpaqueStruct {
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct CheckedStruct {
    pub name: String,
    pub is_resource: bool,
    pub fields: Vec<(String, Ty)>,
}

#[derive(Clone, Debug)]
pub struct CheckedEnum {
    pub name: String,
    pub variants: Vec<CheckedVariant>,
}

#[derive(Clone, Debug)]
pub struct CheckedVariant {
    pub name: String,
    pub payload: Vec<Ty>,
}

#[derive(Clone, Debug)]
pub struct CheckedInterface {
    pub name: String,
    pub is_unsafe: bool,
    pub generics: Vec<String>,
    pub ret: Ty,
    pub params: Vec<Ty>,
}

#[derive(Clone, Debug)]
pub struct CheckedInterfaceAlias {
    pub name: String,
    pub generics: Vec<String>,
    pub positive: Vec<CheckedInterfaceRef>,
    pub negative: Vec<CheckedInterfaceRef>,
}

#[derive(Clone, Debug)]
pub struct CheckedInterfaceRef {
    pub name: String,
    pub args: Vec<Ty>,
}

#[derive(Clone, Debug)]
pub struct CheckedImpl {
    pub interface_name: String,
    pub interface_args: Vec<Ty>,
    pub receiver_ty: Option<Ty>,
    pub function_def: DefId,
    pub ret: Ty,
    pub params: Vec<Ty>,
}

#[derive(Clone, Debug)]
pub struct CheckedFunction {
    pub def_id: DefId,
    pub name: String,
    pub is_unsafe: bool,
    pub is_async: bool,
    pub abi: Option<String>,
    pub noescape: bool,
    pub exported: bool,
    pub ret: Ty,
    pub params: Vec<(Option<LocalId>, String, Ty)>,
    pub body: Option<TBlock>,
}

#[derive(Clone, Debug)]
pub struct CheckedGenericFunction {
    pub def_id: DefId,
    pub module: ModuleId,
    pub name: String,
    pub is_unsafe: bool,
    pub is_async: bool,
    pub abi: Option<String>,
    pub noescape: bool,
    pub exported: bool,
    pub generics: Vec<CheckedGenericParam>,
    pub ret: Ty,
    pub params: Vec<Ty>,
    pub function: FunctionDecl,
}

#[derive(Clone, Debug)]
pub struct CheckedGenericParam {
    pub name: String,
    pub is_resource: bool,
    pub constraint: Option<ConstraintExpr>,
}

#[derive(Clone, Debug)]
pub struct TBlock {
    pub span: Span,
    pub statements: Vec<TStmt>,
}

#[derive(Clone, Debug)]
pub struct TStmt {
    pub span: Span,
    pub kind: TStmtKind,
}

#[derive(Clone, Debug)]
pub enum TStmtKind {
    Block(TBlock),
    VarDecl {
        ty: Ty,
        name: String,
        local_id: LocalId,
        init: Option<TExpr>,
    },
    Assign {
        target: TExpr,
        value: TExpr,
    },
    If {
        cond: TExpr,
        then_block: TBlock,
        else_branch: Option<Box<TStmt>>,
    },
    While {
        cond: TExpr,
        body: TBlock,
    },
    For {
        init: Option<TForInit>,
        cond: Option<TExpr>,
        step: Option<TForInit>,
        body: TBlock,
    },
    Switch {
        expr: TExpr,
        enum_type_name: String,
        cases: Vec<TCase>,
        has_default: bool,
        default: Vec<TStmt>,
        can_fallthrough: bool,
    },
    Defer(TExpr),
    ResourceCleanup(TExpr),
    Return(Option<TExpr>),
    Break,
    Continue,
    Expr(TExpr),
    Unsupported,
}

#[derive(Clone, Debug)]
pub enum TForInit {
    VarDecl {
        ty: Ty,
        name: String,
        local_id: LocalId,
        init: Option<TExpr>,
    },
    Assign {
        target: TExpr,
        value: TExpr,
    },
    Expr(TExpr),
}

#[derive(Clone, Debug)]
pub struct TCase {
    pub variant_name: String,
    pub variant_index: usize,
    pub pattern: TPattern,
    pub statements: Vec<TStmt>,
}

#[derive(Clone, Debug)]
pub enum TPattern {
    Wildcard {
        ty: Ty,
    },
    Binding {
        local_id: LocalId,
        name: String,
        mutability: BindingMutability,
        ty: Ty,
    },
    Variant {
        ty: Ty,
        enum_type_name: String,
        variant_name: String,
        variant_index: usize,
        payload: Vec<TPattern>,
    },
}

impl TPattern {
    pub fn ty(&self) -> &Ty {
        match self {
            TPattern::Wildcard { ty }
            | TPattern::Binding { ty, .. }
            | TPattern::Variant { ty, .. } => ty,
        }
    }

    pub fn collect_bindings<'a>(
        &'a self,
        out: &mut Vec<(&'a LocalId, &'a String, BindingMutability, &'a Ty)>,
    ) {
        match self {
            TPattern::Wildcard { .. } => {}
            TPattern::Binding {
                local_id,
                name,
                mutability,
                ty,
            } => out.push((local_id, name, *mutability, ty)),
            TPattern::Variant { payload, .. } => {
                for pattern in payload {
                    pattern.collect_bindings(out);
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct TExpr {
    pub span: Span,
    pub ty: Ty,
    pub kind: TExprKind,
}

impl TExpr {
    pub fn is_never(&self) -> bool {
        self.ty.is_never()
    }
}

#[derive(Clone, Debug)]
pub enum ActorSpawnMode {
    Cloned,
    State,
}

#[derive(Clone, Debug)]
pub struct TSelectArm {
    pub binding_local: LocalId,
    pub binding_name: String,
    pub future: TExpr,
    pub future_output_ty: Ty,
    pub body: TExpr,
}

#[derive(Clone, Debug)]
pub enum TExprKind {
    Local(LocalId, String),
    Move(Box<TExpr>),
    Function(DefId, String),
    GenericFunction {
        def_id: DefId,
        name: String,
        type_args: Vec<Ty>,
    },
    Literal(Literal),
    StructLiteral {
        type_name: String,
        fields: Vec<(String, TExpr)>,
    },
    EnumLiteral {
        type_name: String,
        variant_name: String,
        variant_index: usize,
        payload: Vec<TExpr>,
    },
    ArrayLiteral(Vec<TExpr>),
    ArrayRepeat {
        element: Box<TExpr>,
        len: usize,
    },
    Closure {
        is_async: bool,
        id: usize,
        params: Vec<(LocalId, String, Ty)>,
        captures: Vec<TClosureCapture>,
        body: TClosureBody,
    },
    FunctionToClosure(Box<TExpr>),
    RetainClosure {
        expr: Box<TExpr>,
        source_ty: Ty,
    },
    Unary {
        op: UnaryOp,
        expr: Box<TExpr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<TExpr>,
        right: Box<TExpr>,
    },
    Cast {
        expr: Box<TExpr>,
        ty: Ty,
    },
    UnsafeBlock {
        statements: Vec<TStmt>,
        value: Option<Box<TExpr>>,
    },
    Call {
        callee: Box<TExpr>,
        args: Vec<TExpr>,
    },
    ArrayToSlice(Box<TExpr>),
    SliceToConst(Box<TExpr>),
    MakeDynamicInterface {
        expr: Box<TExpr>,
        concrete_ty: Ty,
    },
    DynamicInterfaceCall {
        interface_name: String,
        receiver: Box<TExpr>,
        args: Vec<TExpr>,
    },
    RetainedClosureInterfaceCall {
        interface_name: String,
        interface_args: Vec<Ty>,
        receiver: Box<TExpr>,
        args: Vec<TExpr>,
    },
    CloneMessage {
        value: Box<TExpr>,
        message_ty: Ty,
    },
    Field {
        base: Box<TExpr>,
        field: String,
    },
    Arrow {
        base: Box<TExpr>,
        field: String,
    },
    Index {
        base: Box<TExpr>,
        index: Box<TExpr>,
    },
    Slice {
        base: Box<TExpr>,
        start: Option<Box<TExpr>>,
        end: Option<Box<TExpr>>,
    },
    Try {
        expr: Box<TExpr>,
        propagation: TryPropagation,
    },
    Await {
        future: Box<TExpr>,
    },
    AsyncSelect {
        biased: bool,
        arms: Vec<TSelectArm>,
    },
    AsyncBlockOn {
        future: Box<TExpr>,
    },
    AsyncSleep {
        ms: Box<TExpr>,
        output_ty: Ty,
    },
    AsyncOpFuture {
        op: Box<TExpr>,
        output_ty: Ty,
    },
    AsyncSpawn {
        body: Box<TExpr>,
        task_output_ty: Ty,
    },
    AsyncTaskCancel {
        task: Box<TExpr>,
        task_output_ty: Ty,
    },
    AsyncTaskIsFinished {
        task: Box<TExpr>,
        task_output_ty: Ty,
    },
    AsyncChannelSend {
        sender: Box<TExpr>,
        value: Box<TExpr>,
        payload_ty: Ty,
    },
    AsyncChannelTrySend {
        sender: Box<TExpr>,
        value: Box<TExpr>,
        payload_ty: Ty,
    },
    AsyncChannelReserve {
        sender: Box<TExpr>,
        payload_ty: Ty,
    },
    AsyncChannelPermitSend {
        permit: Box<TExpr>,
        value: Box<TExpr>,
        payload_ty: Ty,
    },
    AsyncChannelRecv {
        receiver: Box<TExpr>,
        payload_ty: Ty,
    },
    MetaAsRefRepr {
        value: Box<TExpr>,
        source_ty: Ty,
    },
    MetaIntoRepr {
        value: Box<TExpr>,
        source_ty: Ty,
    },
    MetaFromRepr {
        value: Box<TExpr>,
        target_ty: Ty,
    },
    ActorSpawn {
        mode: ActorSpawnMode,
        state_arg: Box<TExpr>,
        handler: Box<TExpr>,
        state_ty: Ty,
        handle_message_ty: Ty,
        message_ty: Ty,
        handler_ty: Ty,
    },
    ActorSend {
        actor: Box<TExpr>,
        value: Box<TExpr>,
        message_ty: Ty,
    },
    ActorStop {
        actor: Box<TExpr>,
        message_ty: Ty,
    },
    ActorJoin {
        actor: Box<TExpr>,
        message_ty: Ty,
    },
    TypeSize {
        ty: Ty,
    },
    TypeAlign {
        ty: Ty,
    },
}

#[derive(Clone, Debug)]
pub enum TryPropagation {
    Exact,
    ErrorBox,
}

#[derive(Clone, Debug)]
pub struct TClosureCapture {
    pub local_id: LocalId,
    pub name: String,
    pub ty: Ty,
}

#[derive(Clone, Debug)]
pub enum TClosureBody {
    Expr(Box<TExpr>),
    Block(TBlock),
}

pub trait ThirVisitor {
    fn visit_block(&mut self, block: &TBlock) {
        walk_block(self, block);
    }

    fn visit_stmt(&mut self, stmt: &TStmt) {
        walk_stmt(self, stmt);
    }

    fn visit_for_init(&mut self, init: &TForInit) {
        walk_for_init(self, init);
    }

    fn visit_case(&mut self, case: &TCase) {
        walk_case(self, case);
    }

    fn visit_pattern(&mut self, pattern: &TPattern) {
        walk_pattern(self, pattern);
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        walk_expr(self, expr);
    }

    fn visit_closure_body(&mut self, body: &TClosureBody) {
        walk_closure_body(self, body);
    }
}

pub fn walk_block<V: ThirVisitor + ?Sized>(visitor: &mut V, block: &TBlock) {
    for stmt in &block.statements {
        visitor.visit_stmt(stmt);
    }
}

pub fn walk_stmt<V: ThirVisitor + ?Sized>(visitor: &mut V, stmt: &TStmt) {
    match &stmt.kind {
        TStmtKind::Block(block) => visitor.visit_block(block),
        TStmtKind::VarDecl { init, .. } => {
            if let Some(init) = init {
                visitor.visit_expr(init);
            }
        }
        TStmtKind::Assign { target, value } => {
            visitor.visit_expr(target);
            visitor.visit_expr(value);
        }
        TStmtKind::If {
            cond,
            then_block,
            else_branch,
        } => {
            visitor.visit_expr(cond);
            visitor.visit_block(then_block);
            if let Some(else_branch) = else_branch {
                visitor.visit_stmt(else_branch);
            }
        }
        TStmtKind::While { cond, body } => {
            visitor.visit_expr(cond);
            visitor.visit_block(body);
        }
        TStmtKind::For {
            init,
            cond,
            step,
            body,
        } => {
            if let Some(init) = init {
                visitor.visit_for_init(init);
            }
            if let Some(cond) = cond {
                visitor.visit_expr(cond);
            }
            if let Some(step) = step {
                visitor.visit_for_init(step);
            }
            visitor.visit_block(body);
        }
        TStmtKind::Switch {
            expr,
            cases,
            default,
            ..
        } => {
            visitor.visit_expr(expr);
            for case in cases {
                visitor.visit_case(case);
            }
            for stmt in default {
                visitor.visit_stmt(stmt);
            }
        }
        TStmtKind::Defer(expr)
        | TStmtKind::ResourceCleanup(expr)
        | TStmtKind::Return(Some(expr))
        | TStmtKind::Expr(expr) => {
            visitor.visit_expr(expr);
        }
        TStmtKind::Return(None)
        | TStmtKind::Break
        | TStmtKind::Continue
        | TStmtKind::Unsupported => {}
    }
}

pub fn walk_for_init<V: ThirVisitor + ?Sized>(visitor: &mut V, init: &TForInit) {
    match init {
        TForInit::VarDecl { init, .. } => {
            if let Some(init) = init {
                visitor.visit_expr(init);
            }
        }
        TForInit::Assign { target, value } => {
            visitor.visit_expr(target);
            visitor.visit_expr(value);
        }
        TForInit::Expr(expr) => visitor.visit_expr(expr),
    }
}

pub fn walk_case<V: ThirVisitor + ?Sized>(visitor: &mut V, case: &TCase) {
    visitor.visit_pattern(&case.pattern);
    for stmt in &case.statements {
        visitor.visit_stmt(stmt);
    }
}

pub fn walk_pattern<V: ThirVisitor + ?Sized>(visitor: &mut V, pattern: &TPattern) {
    match pattern {
        TPattern::Wildcard { .. } | TPattern::Binding { .. } => {}
        TPattern::Variant { payload, .. } => {
            for pattern in payload {
                visitor.visit_pattern(pattern);
            }
        }
    }
}

pub fn walk_expr<V: ThirVisitor + ?Sized>(visitor: &mut V, expr: &TExpr) {
    match &expr.kind {
        TExprKind::Local(..)
        | TExprKind::Function(..)
        | TExprKind::GenericFunction { .. }
        | TExprKind::Literal(_)
        | TExprKind::TypeSize { .. }
        | TExprKind::TypeAlign { .. } => {}
        TExprKind::StructLiteral { fields, .. } => {
            for (_, value) in fields {
                visitor.visit_expr(value);
            }
        }
        TExprKind::EnumLiteral { payload, .. } => {
            for value in payload {
                visitor.visit_expr(value);
            }
        }
        TExprKind::ArrayLiteral(elements) => {
            for element in elements {
                visitor.visit_expr(element);
            }
        }
        TExprKind::ArrayRepeat { element, .. } => visitor.visit_expr(element),
        TExprKind::Closure { body, .. } => visitor.visit_closure_body(body),
        TExprKind::Move(inner)
        | TExprKind::FunctionToClosure(inner)
        | TExprKind::RetainClosure { expr: inner, .. }
        | TExprKind::Unary { expr: inner, .. }
        | TExprKind::Cast { expr: inner, .. }
        | TExprKind::Try { expr: inner, .. }
        | TExprKind::AsyncBlockOn { future: inner }
        | TExprKind::ArrayToSlice(inner)
        | TExprKind::SliceToConst(inner)
        | TExprKind::MakeDynamicInterface { expr: inner, .. } => visitor.visit_expr(inner),
        TExprKind::Await { future: inner } => visitor.visit_expr(inner),
        TExprKind::AsyncSelect { arms, .. } => {
            for arm in arms {
                visitor.visit_expr(&arm.future);
                visitor.visit_expr(&arm.body);
            }
        }
        TExprKind::AsyncSleep { ms, output_ty: _ } => visitor.visit_expr(ms),
        TExprKind::AsyncOpFuture { op, .. } => visitor.visit_expr(op),
        TExprKind::AsyncSpawn { body, .. } => visitor.visit_expr(body),
        TExprKind::AsyncTaskCancel { task, .. } | TExprKind::AsyncTaskIsFinished { task, .. } => {
            visitor.visit_expr(task)
        }
        TExprKind::AsyncChannelSend { sender, value, .. }
        | TExprKind::AsyncChannelTrySend { sender, value, .. } => {
            visitor.visit_expr(sender);
            visitor.visit_expr(value);
        }
        TExprKind::AsyncChannelReserve { sender, .. } => visitor.visit_expr(sender),
        TExprKind::AsyncChannelRecv { receiver, .. } => visitor.visit_expr(receiver),
        TExprKind::AsyncChannelPermitSend { permit, value, .. } => {
            visitor.visit_expr(permit);
            visitor.visit_expr(value);
        }
        TExprKind::UnsafeBlock { statements, value } => {
            for stmt in statements {
                visitor.visit_stmt(stmt);
            }
            if let Some(value) = value {
                visitor.visit_expr(value);
            }
        }
        TExprKind::Binary { left, right, .. } => {
            visitor.visit_expr(left);
            visitor.visit_expr(right);
        }
        TExprKind::Call { callee, args } => {
            visitor.visit_expr(callee);
            for arg in args {
                visitor.visit_expr(arg);
            }
        }
        TExprKind::DynamicInterfaceCall { receiver, args, .. }
        | TExprKind::RetainedClosureInterfaceCall { receiver, args, .. } => {
            visitor.visit_expr(receiver);
            for arg in args {
                visitor.visit_expr(arg);
            }
        }
        TExprKind::CloneMessage { value, .. } => visitor.visit_expr(value),
        TExprKind::Field { base, .. } | TExprKind::Arrow { base, .. } => {
            visitor.visit_expr(base);
        }
        TExprKind::Index { base, index } => {
            visitor.visit_expr(base);
            visitor.visit_expr(index);
        }
        TExprKind::Slice { base, start, end } => {
            visitor.visit_expr(base);
            if let Some(start) = start {
                visitor.visit_expr(start);
            }
            if let Some(end) = end {
                visitor.visit_expr(end);
            }
        }
        TExprKind::MetaAsRefRepr { value, .. }
        | TExprKind::MetaIntoRepr { value, .. }
        | TExprKind::MetaFromRepr { value, .. } => visitor.visit_expr(value),
        TExprKind::ActorSpawn {
            state_arg, handler, ..
        } => {
            visitor.visit_expr(state_arg);
            visitor.visit_expr(handler);
        }
        TExprKind::ActorSend { actor, value, .. } => {
            visitor.visit_expr(actor);
            visitor.visit_expr(value);
        }
        TExprKind::ActorStop { actor, .. } | TExprKind::ActorJoin { actor, .. } => {
            visitor.visit_expr(actor);
        }
    }
}

pub fn walk_closure_body<V: ThirVisitor + ?Sized>(visitor: &mut V, body: &TClosureBody) {
    match body {
        TClosureBody::Expr(expr) => visitor.visit_expr(expr),
        TClosureBody::Block(block) => visitor.visit_block(block),
    }
}
