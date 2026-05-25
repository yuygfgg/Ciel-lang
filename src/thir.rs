use crate::{
    ast::{BinaryOp, Literal, UnaryOp},
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
    pub generics: Vec<String>,
    pub ret: Ty,
    pub params: Vec<Ty>,
}

#[derive(Clone, Debug)]
pub struct CheckedInterfaceAlias {
    pub name: String,
    pub positive: Vec<CheckedInterfaceRef>,
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

    pub fn collect_bindings<'a>(&'a self, out: &mut Vec<(&'a LocalId, &'a String, &'a Ty)>) {
        match self {
            TPattern::Wildcard { .. } => {}
            TPattern::Binding { local_id, name, ty } => out.push((local_id, name, ty)),
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
pub enum TExprKind {
    Local(LocalId, String),
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
        id: usize,
        params: Vec<(LocalId, String, Ty)>,
        captures: Vec<TClosureCapture>,
        body: TClosureBody,
    },
    FunctionToClosure(Box<TExpr>),
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
    Call {
        callee: Box<TExpr>,
        args: Vec<TExpr>,
    },
    ArrayToSlice(Box<TExpr>),
    MakeDynamicInterface {
        expr: Box<TExpr>,
        concrete_ty: Ty,
    },
    DynamicInterfaceCall {
        interface_name: String,
        receiver: Box<TExpr>,
        args: Vec<TExpr>,
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
    Try(Box<TExpr>),
    BuiltinCloneMessage {
        value: Box<TExpr>,
        message_ty: Ty,
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
        initial_state: Box<TExpr>,
        handler: Box<TExpr>,
        state_ty: Ty,
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
