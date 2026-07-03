use crate::span::Span;

#[derive(Clone, Debug)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct AstFile {
    pub items: Vec<Item>,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub export: bool,
    pub span: Span,
    pub kind: ItemKind,
}

#[derive(Clone, Debug)]
pub enum ItemKind {
    Import(ImportDecl),
    CInclude(String),
    TypeAlias(TypeAliasDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Interface(InterfaceDecl),
    InterfaceAlias(InterfaceAliasDecl),
    Impl(ImplDecl),
    DerivableImpl(DerivableImplDecl),
    Derive(DeriveDecl),
    Function(FunctionDecl),
    ExternBlock(ExternBlock),
}

#[derive(Clone, Debug)]
pub struct ImportDecl {
    pub path: ModulePath,
    pub alias: Option<Ident>,
}

#[derive(Clone, Debug)]
pub struct ModulePath {
    pub absolute: bool,
    pub raw: String,
}

#[derive(Clone, Debug)]
pub struct TypeAliasDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub target: TypeAliasTarget,
}

#[derive(Clone, Debug)]
pub enum TypeAliasTarget {
    Type(Type),
    CSpelling { abi: String, spelling: String },
}

#[derive(Clone, Debug)]
pub struct StructDecl {
    pub is_resource: bool,
    pub is_unsafe: bool,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<FieldDecl>,
}

#[derive(Clone, Debug)]
pub struct FieldDecl {
    pub ty: Type,
    pub name: Ident,
}

#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<VariantDecl>,
}

#[derive(Clone, Debug)]
pub struct VariantDecl {
    pub name: Ident,
    pub payload: Vec<Type>,
}

#[derive(Clone, Debug)]
pub struct InterfaceDecl {
    pub is_unsafe: bool,
    pub generics: Vec<GenericParam>,
    pub determined_start: Option<usize>,
    pub signature: FunctionSignature,
}

#[derive(Clone, Debug)]
pub struct InterfaceAliasDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub expr: InterfaceExpr,
}

#[derive(Clone, Debug)]
pub struct InterfaceExpr {
    pub first: InterfaceTerm,
    pub rest: Vec<(InterfaceOp, InterfaceTerm)>,
}

#[derive(Clone, Debug)]
pub enum InterfaceOp {
    Add,
    Sub,
}

#[derive(Clone, Debug)]
pub struct InterfaceTerm {
    pub negated: bool,
    pub name: Vec<Ident>,
    pub args: Vec<Type>,
}

#[derive(Clone, Debug)]
pub struct ImplDecl {
    pub is_unsafe: bool,
    pub generics: Vec<GenericParam>,
    pub name: Vec<Ident>,
    pub args: Vec<Type>,
    pub params: Vec<Param>,
    pub body: Block,
}

#[derive(Clone, Debug)]
pub struct DerivableImplDecl {
    pub requires_unsafe: bool,
    pub impl_decl: ImplDecl,
}

#[derive(Clone, Debug)]
pub struct DeriveDecl {
    pub is_unsafe: bool,
    pub generics: Vec<GenericParam>,
    pub name: Vec<Ident>,
    pub args: Vec<Type>,
}

#[derive(Clone, Debug)]
pub struct FunctionDecl {
    pub is_unsafe: bool,
    pub is_async: bool,
    pub abi: Option<String>,
    pub signature: FunctionSignature,
    pub body: Option<Block>,
}

#[derive(Clone, Debug)]
pub struct FunctionSignature {
    pub ret: FunctionReturnType,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub receiver_selector: Option<ReceiverSelector>,
}

#[derive(Clone, Debug)]
pub enum FunctionReturnType {
    Type(Type),
    OpaqueConstraint {
        marker_span: Span,
        constraint: ConstraintExpr,
    },
}

impl FunctionReturnType {
    pub fn span(&self) -> Span {
        match self {
            FunctionReturnType::Type(ty) => ty.span,
            FunctionReturnType::OpaqueConstraint {
                marker_span,
                constraint,
            } => constraint
                .terms
                .last()
                .and_then(|term| term.name.last())
                .map(|name| marker_span.merge(name.span))
                .unwrap_or(*marker_span),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReceiverSelector {
    pub receiver_param: Option<Ident>,
    pub name: Ident,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ExternBlock {
    pub is_unsafe: bool,
    pub abi: String,
    pub items: Vec<ExternItem>,
}

#[derive(Clone, Debug)]
pub enum ExternItem {
    OpaqueStruct(Ident),
    Function {
        noescape: bool,
        signature: FunctionSignature,
    },
    TypeAlias(TypeAliasDecl),
}

#[derive(Clone, Debug)]
pub struct GenericParam {
    pub is_resource: bool,
    pub name: Ident,
    pub constraint: Option<ConstraintExpr>,
}

#[derive(Clone, Debug)]
pub struct ConstraintExpr {
    pub terms: Vec<ConstraintTerm>,
}

#[derive(Clone, Debug)]
pub struct ConstraintTerm {
    pub negated: bool,
    pub removed: bool,
    pub name: Vec<Ident>,
    pub args: Vec<ConstraintArg>,
}

#[derive(Clone, Debug)]
pub enum ConstraintArg {
    Type(Type),
    Binding {
        name: Ident,
        constraint: Option<ConstraintExpr>,
        span: Span,
    },
}

#[derive(Clone, Debug)]
pub struct Param {
    pub ty: Type,
    pub name: Ident,
    pub mutability: BindingMutability,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BindingMutability {
    Immutable,
    Mutable,
}

impl BindingMutability {
    pub fn is_mutable(self) -> bool {
        matches!(self, BindingMutability::Mutable)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewMutability {
    ReadOnly,
    Writable,
}

impl ViewMutability {
    pub fn is_writable(self) -> bool {
        matches!(self, ViewMutability::Writable)
    }

    pub fn is_read_only(self) -> bool {
        matches!(self, ViewMutability::ReadOnly)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrimitiveType {
    Bool,
    Char,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Usize,
    F32,
    F64,
}

#[derive(Clone, Debug)]
pub struct Type {
    pub span: Span,
    pub kind: TypeKind,
}

#[derive(Clone, Debug)]
pub enum TypeKind {
    Hole,
    Never,
    Void,
    Primitive(PrimitiveType),
    Named(Vec<Ident>, Vec<Type>),
    Pointer {
        nullable: bool,
        mutability: ViewMutability,
        inner: Box<Type>,
    },
    Array {
        len: usize,
        elem: Box<Type>,
    },
    Slice {
        mutability: ViewMutability,
        elem: Box<Type>,
    },
    Function {
        is_unsafe: bool,
        abi: Option<String>,
        ret: Box<Type>,
        params: Vec<Type>,
    },
    Closure {
        ret: Box<Type>,
        params: Vec<Type>,
        constraint: Option<ConstraintExpr>,
    },
}

#[derive(Clone, Debug)]
pub struct Block {
    pub span: Span,
    pub statements: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct ExprBlock {
    pub span: Span,
    pub statements: Vec<Stmt>,
    pub value: Option<Box<Expr>>,
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub span: Span,
    pub kind: StmtKind,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    Block(Block),
    VarDecl {
        ty: Type,
        name: Ident,
        mutability: BindingMutability,
        init: Option<Expr>,
    },
    Assign {
        target: Expr,
        value: Expr,
    },
    If {
        cond: Expr,
        then_block: Block,
        else_branch: Option<Box<Stmt>>,
    },
    While {
        cond: Expr,
        body: Block,
    },
    For {
        init: Option<ForInit>,
        cond: Option<Expr>,
        step: Option<ForInit>,
        body: Block,
    },
    Switch {
        expr: Expr,
        cases: Vec<CaseClause>,
        has_default: bool,
        default: Vec<Stmt>,
    },
    Defer(Expr),
    Return(Option<Expr>),
    Break,
    Continue,
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub enum ForInit {
    VarDecl {
        ty: Type,
        name: Ident,
        mutability: BindingMutability,
        init: Option<Expr>,
    },
    Assign {
        target: Expr,
        value: Expr,
    },
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub struct CaseClause {
    pub pattern: Pattern,
    pub statements: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub enum Pattern {
    Variant(Vec<Ident>, Vec<Pattern>),
    Binding {
        name: Ident,
        mutability: BindingMutability,
    },
    Wildcard(Span),
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub span: Span,
    pub kind: ExprKind,
}

#[derive(Clone, Debug)]
pub struct SelectArm {
    pub binding: Ident,
    pub future: Expr,
    pub body: Expr,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Name(Vec<Ident>),
    Literal(Literal),
    StructLiteral(Vec<FieldInit>),
    ArrayLiteral(Vec<Expr>),
    ArrayRepeat {
        element: Box<Expr>,
        len: Option<usize>,
    },
    Closure {
        is_async: bool,
        params: Vec<ClosureParam>,
        body: ClosureBody,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Cast {
        expr: Box<Expr>,
        ty: Type,
    },
    UnsafeBlock(ExprBlock),
    Call {
        callee: Box<Expr>,
        type_args: Vec<Type>,
        args: Vec<Expr>,
    },
    GenericValue {
        callee: Box<Expr>,
        type_args: Vec<Type>,
    },
    Field {
        base: Box<Expr>,
        field: Ident,
    },
    ReceiverSelector {
        base: Box<Expr>,
        selector: Vec<Ident>,
    },
    Arrow {
        base: Box<Expr>,
        field: Ident,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Slice {
        base: Box<Expr>,
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
    },
    Try(Box<Expr>),
    Await(Box<Expr>),
    Select {
        biased: bool,
        arms: Vec<SelectArm>,
    },
}

#[derive(Clone, Debug)]
pub struct ClosureParam {
    pub ty: Option<Type>,
    pub name: Ident,
    pub mutability: BindingMutability,
}

#[derive(Clone, Debug)]
pub enum ClosureBody {
    Expr(Box<Expr>),
    Block(Block),
}

#[derive(Clone, Debug)]
pub struct FieldInit {
    pub name: Ident,
    pub expr: Expr,
}

#[derive(Clone, Debug)]
pub enum Literal {
    Integer(String),
    Float(String),
    Char(String),
    String(String),
    Bool(bool),
    Null,
}

#[derive(Clone, Copy, Debug)]
pub enum UnaryOp {
    Not,
    Neg,
    BitNot,
    Addr,
    Deref,
}

#[derive(Clone, Copy, Debug)]
pub enum BinaryOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    BitOr,
    BitXor,
    BitAnd,
    Shl,
    Shr,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

impl BinaryOp {
    pub fn is_equality(self) -> bool {
        matches!(self, BinaryOp::Eq | BinaryOp::Ne)
    }

    pub fn is_bitwise(self) -> bool {
        matches!(self, BinaryOp::BitOr | BinaryOp::BitXor | BinaryOp::BitAnd)
    }

    pub fn is_shift(self) -> bool {
        matches!(self, BinaryOp::Shl | BinaryOp::Shr)
    }
}
