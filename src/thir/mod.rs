use std::collections::HashSet;

use crate::{
    ast::{BinaryOp, BindingMutability, Literal, UnaryOp},
    hir::{ConstraintExpr, FunctionDecl, LocalId},
    resolve::{DefId, ModuleId},
    span::Span,
    types::Ty,
};

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
    pub def_id: DefId,
    pub name: String,
    pub is_unsafe: bool,
    pub generics: Vec<String>,
    pub ret: Ty,
    pub params: Vec<Ty>,
}

#[derive(Clone, Debug)]
pub struct CheckedInterfaceAlias {
    pub def_id: DefId,
    pub name: String,
    pub generics: Vec<String>,
    pub positive: Vec<CheckedInterfaceRef>,
    pub negative: Vec<CheckedInterfaceRef>,
}

#[derive(Clone, Debug)]
pub struct CheckedInterfaceRef {
    pub def_id: DefId,
    pub name: String,
    pub args: Vec<Ty>,
}

#[derive(Clone, Debug)]
pub struct CheckedImpl {
    pub interface_def: DefId,
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
    pub async_facts: Option<AsyncFacts>,
    pub abi: Option<String>,
    pub noescape: bool,
    pub exported: bool,
    pub ret: Ty,
    pub params: Vec<(Option<LocalId>, String, Ty)>,
    pub body: Option<TBlock>,
}

#[derive(Clone, Debug)]
pub struct AsyncFacts {
    pub frame_locals: Vec<AsyncFrameLocal>,
    pub live_across_await: HashSet<LocalId>,
    pub await_output_tys: Vec<Ty>,
    pub defer_args: Vec<AsyncDeferArg>,
}

#[derive(Clone, Debug)]
pub struct AsyncFrameLocal {
    pub id: LocalId,
    pub ty: Ty,
    pub field: String,
    pub heap: bool,
}

#[derive(Clone, Debug)]
pub struct AsyncDeferArg {
    pub ty: Ty,
    pub field: String,
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
    pub is_hidden: bool,
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
        async_facts: Option<AsyncFacts>,
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
    RawSliceFromPtr {
        ptr: Box<TExpr>,
        len: Box<TExpr>,
        elem_ty: Ty,
    },
    MakeDynamicInterface {
        expr: Box<TExpr>,
        concrete_ty: Ty,
    },
    ErrorBox {
        expr: Box<TExpr>,
        concrete_ty: Ty,
    },
    DynamicInterfaceCall {
        interface_def: DefId,
        interface_name: String,
        receiver: Box<TExpr>,
        args: Vec<TExpr>,
    },
    RetainedClosureInterfaceCall {
        interface_def: DefId,
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
        raw_operation_def: DefId,
        poll_done_def: DefId,
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
    TypeNeedsGcScan {
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

mod visit;
pub(crate) use visit::*;
