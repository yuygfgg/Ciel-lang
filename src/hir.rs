use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::{
    ast,
    diagnostic::{DiagResult, Diagnostic},
    resolve::{DefId, DefKind, LookupError, ModuleId, ResolvedImport, ResolvedProgram},
    span::{FileId, Span},
};

pub use ast::{
    BinaryOp, BindingMutability, InterfaceOp, Literal, PrimitiveType, UnaryOp, ViewMutability,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalId(pub usize);

#[derive(Clone, Debug)]
pub struct HirProgram {
    pub resolved: ResolvedProgram,
    pub modules: Vec<Module>,
    pub locals: Vec<Local>,
}

#[derive(Clone, Debug)]
pub struct Local {
    pub id: LocalId,
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Module {
    pub id: ModuleId,
    pub path: PathBuf,
    pub items: Vec<Item>,
    pub imports: Vec<ResolvedImport>,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub export: bool,
    pub span: Span,
    pub def_ids: Vec<DefId>,
    pub kind: ItemKind,
}

#[derive(Clone, Debug)]
pub enum ItemKind {
    Import(ast::ImportDecl),
    CInclude(String),
    TypeAlias(TypeAliasDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Interface(InterfaceDecl),
    InterfaceAlias(InterfaceAliasDecl),
    Impl(ImplDecl),
    Function(FunctionDecl),
    ExternBlock(ExternBlock),
}

#[derive(Clone, Debug)]
pub struct TypeAliasDecl {
    pub name: ast::Ident,
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
    pub name: ast::Ident,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<FieldDecl>,
}

#[derive(Clone, Debug)]
pub struct FieldDecl {
    pub ty: Type,
    pub name: ast::Ident,
}

#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub name: ast::Ident,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<VariantDecl>,
}

#[derive(Clone, Debug)]
pub struct VariantDecl {
    pub name: ast::Ident,
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
    pub name: ast::Ident,
    pub generics: Vec<GenericParam>,
    pub expr: InterfaceExpr,
}

#[derive(Clone, Debug)]
pub struct InterfaceExpr {
    pub first: InterfaceTerm,
    pub rest: Vec<(InterfaceOp, InterfaceTerm)>,
}

#[derive(Clone, Debug)]
pub struct InterfaceTerm {
    pub negated: bool,
    pub name: NameRef,
    pub args: Vec<Type>,
}

#[derive(Clone, Debug)]
pub struct ImplDecl {
    pub is_unsafe: bool,
    pub generics: Vec<GenericParam>,
    pub name: NameRef,
    pub args: Vec<Type>,
    pub params: Vec<Param>,
    pub body: Block,
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
    pub name: ast::Ident,
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
                .and_then(|term| match &term.name.kind {
                    NameRefKind::Def(_) | NameRefKind::Error => Some(term.name.span),
                    _ => Some(term.name.span),
                })
                .map(|span| marker_span.merge(span))
                .unwrap_or(*marker_span),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReceiverSelector {
    pub receiver_param: Option<ast::Ident>,
    pub name: ast::Ident,
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
    OpaqueStruct(ast::Ident),
    Function {
        noescape: bool,
        signature: FunctionSignature,
    },
    TypeAlias(TypeAliasDecl),
}

#[derive(Clone, Debug)]
pub struct GenericParam {
    pub is_resource: bool,
    pub is_hidden: bool,
    pub name: ast::Ident,
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
    pub name: NameRef,
    pub args: Vec<ConstraintArg>,
}

#[derive(Clone, Debug)]
pub enum ConstraintArg {
    Type(Type),
    Binding {
        name: ast::Ident,
        constraint: Option<ConstraintExpr>,
        span: Span,
    },
}

#[derive(Clone, Debug)]
pub struct Param {
    pub ty: Type,
    pub name: ast::Ident,
    pub mutability: BindingMutability,
    pub local_id: Option<LocalId>,
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
    Named(TypeName, Vec<Type>),
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
pub struct TypeName {
    pub display: String,
    pub span: Span,
    pub kind: TypeNameKind,
}

#[derive(Clone, Debug)]
pub enum TypeNameKind {
    Def(DefId),
    Generic(String),
    Error,
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
        name: ast::Ident,
        mutability: BindingMutability,
        local_id: LocalId,
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
        name: ast::Ident,
        mutability: BindingMutability,
        local_id: LocalId,
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
    Variant(PatternName, Vec<Pattern>),
    Wildcard(Span),
}

#[derive(Clone, Debug)]
pub struct PatternName {
    pub path: Vec<ast::Ident>,
    pub display: String,
    pub span: Span,
    pub kind: PatternNameKind,
}

#[derive(Clone, Debug)]
pub enum PatternNameKind {
    Variant(DefId),
    VariantCandidates(Vec<DefId>),
    Binding {
        local_id: LocalId,
        mutability: BindingMutability,
    },
    Error,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub span: Span,
    pub kind: ExprKind,
}

#[derive(Clone, Debug)]
pub struct SelectArm {
    pub binding: ast::Ident,
    pub binding_local: LocalId,
    pub future: Expr,
    pub body: Expr,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Name(NameRef),
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
    Field {
        base: Box<Expr>,
        field: ast::Ident,
    },
    ReceiverSelector {
        base: Box<Expr>,
        selector: Vec<ast::Ident>,
    },
    Arrow {
        base: Box<Expr>,
        field: ast::Ident,
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
    pub name: ast::Ident,
    pub mutability: BindingMutability,
    pub local_id: LocalId,
}

#[derive(Clone, Debug)]
pub enum ClosureBody {
    Expr(Box<Expr>),
    Block(Block),
}

#[derive(Clone, Debug)]
pub struct FieldInit {
    pub name: ast::Ident,
    pub expr: Expr,
}

#[derive(Clone, Debug)]
pub struct NameRef {
    pub display: String,
    pub span: Span,
    pub kind: NameRefKind,
}

#[derive(Clone, Debug)]
pub enum NameRefKind {
    Local(LocalId),
    Def(DefId),
    VariantCandidates(Vec<DefId>),
    Error,
}

pub fn lower_to_hir(resolved: ResolvedProgram) -> DiagResult<HirProgram> {
    let modules = resolved.modules.clone();
    let (hir_modules, locals, diagnostics) = {
        let mut lowerer = Lowerer {
            resolved: &resolved,
            diagnostics: Vec::new(),
            locals: Vec::new(),
        };
        let hir_modules = modules
            .iter()
            .map(|module| lowerer.lower_module(module))
            .collect::<Vec<_>>();
        (hir_modules, lowerer.locals, lowerer.diagnostics)
    };
    if diagnostics.is_empty() {
        Ok(HirProgram {
            resolved,
            modules: hir_modules,
            locals,
        })
    } else {
        Err(diagnostics)
    }
}

struct Lowerer<'a> {
    resolved: &'a ResolvedProgram,
    diagnostics: Vec<Diagnostic>,
    locals: Vec<Local>,
}

struct ModuleLowerer<'a, 'b> {
    lowerer: &'a mut Lowerer<'b>,
    module: ModuleId,
    lexical_defs: HashMap<String, DefId>,
    local_scopes: Vec<HashMap<String, LocalId>>,
    generic_scopes: Vec<HashSet<String>>,
}

#[derive(Clone, Debug)]
struct HiddenGenericParam {
    name: ast::Ident,
    constraint: Option<ast::ConstraintExpr>,
}

impl<'a> Lowerer<'a> {
    fn lower_module(&mut self, module: &crate::resolve::ResolvedModule) -> Module {
        let mut ctx = ModuleLowerer {
            lowerer: self,
            module: module.id,
            lexical_defs: HashMap::new(),
            local_scopes: Vec::new(),
            generic_scopes: Vec::new(),
        };
        let items = module
            .ast
            .items
            .iter()
            .map(|item| ctx.lower_item(item))
            .collect();
        Module {
            id: module.id,
            path: module.path.clone(),
            items,
            imports: module.imports.clone(),
        }
    }
}

impl<'a, 'b> ModuleLowerer<'a, 'b> {
    fn lower_item(&mut self, item: &ast::Item) -> Item {
        let def_ids = self.item_def_ids(item);
        for def_id in &def_ids {
            let def = self.lowerer.resolved.def(*def_id);
            if def.kind != DefKind::EnumVariant {
                self.lexical_defs.insert(def.name.clone(), *def_id);
            }
        }
        let kind = match &item.kind {
            ast::ItemKind::Import(decl) => ItemKind::Import(decl.clone()),
            ast::ItemKind::CInclude(include) => ItemKind::CInclude(include.clone()),
            ast::ItemKind::TypeAlias(decl) => {
                let hidden = self.hidden_generics_for(&decl.generics);
                self.push_generics_with_hidden(&decl.generics, &hidden);
                let generics = self.lower_generics_with_hidden(&decl.generics, &hidden);
                let target = self.lower_type_alias_target(&decl.target);
                self.pop_generics();
                ItemKind::TypeAlias(TypeAliasDecl {
                    name: decl.name.clone(),
                    generics,
                    target,
                })
            }
            ast::ItemKind::Struct(decl) => {
                let hidden = self.hidden_generics_for(&decl.generics);
                self.push_generics_with_hidden(&decl.generics, &hidden);
                let generics = self.lower_generics_with_hidden(&decl.generics, &hidden);
                let fields = decl
                    .fields
                    .iter()
                    .map(|field| FieldDecl {
                        ty: self.lower_type(&field.ty),
                        name: field.name.clone(),
                    })
                    .collect();
                self.pop_generics();
                ItemKind::Struct(StructDecl {
                    is_resource: decl.is_resource,
                    is_unsafe: decl.is_unsafe,
                    name: decl.name.clone(),
                    generics,
                    fields,
                })
            }
            ast::ItemKind::Enum(decl) => {
                let hidden = self.hidden_generics_for(&decl.generics);
                self.push_generics_with_hidden(&decl.generics, &hidden);
                let generics = self.lower_generics_with_hidden(&decl.generics, &hidden);
                let variants = decl
                    .variants
                    .iter()
                    .map(|variant| VariantDecl {
                        name: variant.name.clone(),
                        payload: variant
                            .payload
                            .iter()
                            .map(|ty| self.lower_type(ty))
                            .collect(),
                    })
                    .collect();
                self.pop_generics();
                ItemKind::Enum(EnumDecl {
                    name: decl.name.clone(),
                    generics,
                    variants,
                })
            }
            ast::ItemKind::Interface(decl) => {
                self.push_generics(&decl.generics);
                let generics = self.lower_generics(&decl.generics);
                let signature = self.lower_signature(&decl.signature, false);
                self.pop_generics();
                ItemKind::Interface(InterfaceDecl {
                    is_unsafe: decl.is_unsafe,
                    generics,
                    determined_start: decl.determined_start,
                    signature,
                })
            }
            ast::ItemKind::InterfaceAlias(decl) => {
                self.push_generics(&decl.generics);
                let generics = self.lower_generics(&decl.generics);
                let expr = self.lower_interface_expr(&decl.expr);
                self.pop_generics();
                ItemKind::InterfaceAlias(InterfaceAliasDecl {
                    name: decl.name.clone(),
                    generics,
                    expr,
                })
            }
            ast::ItemKind::Impl(decl) => {
                let hidden = self.hidden_generics_for(&decl.generics);
                self.push_generics_with_hidden(&decl.generics, &hidden);
                self.push_scope();
                let generics = self.lower_generics_with_hidden(&decl.generics, &hidden);
                let name = self.resolve_name(&decl.name, "interface");
                self.require_def_kind(&name, &[DefKind::Interface], "interface");
                let args = decl.args.iter().map(|ty| self.lower_type(ty)).collect();
                let params = decl
                    .params
                    .iter()
                    .map(|param| self.lower_param(param, true))
                    .collect::<Vec<_>>();
                for param in &params {
                    if let Some(local_id) = param.local_id {
                        self.insert_existing_local(param.name.clone(), local_id);
                    }
                }
                let body = self.lower_block_with_existing_scope(&decl.body);
                self.pop_scope();
                self.pop_generics();
                ItemKind::Impl(ImplDecl {
                    is_unsafe: decl.is_unsafe,
                    generics,
                    name,
                    args,
                    params,
                    body,
                })
            }
            ast::ItemKind::Function(decl) => {
                let hidden = self.hidden_generics_for(&decl.signature.generics);
                self.push_generics_with_hidden(&decl.signature.generics, &hidden);
                let signature =
                    self.lower_signature_with_hidden(&decl.signature, &hidden, decl.body.is_some());
                let body = decl.body.as_ref().map(|body| {
                    self.push_scope();
                    for param in &signature.params {
                        if let Some(local_id) = param.local_id {
                            self.insert_existing_local(param.name.clone(), local_id);
                        }
                    }
                    let body = self.lower_block_with_existing_scope(body);
                    self.pop_scope();
                    body
                });
                self.pop_generics();
                ItemKind::Function(FunctionDecl {
                    is_unsafe: decl.is_unsafe,
                    is_async: decl.is_async,
                    abi: decl.abi.clone(),
                    signature,
                    body,
                })
            }
            ast::ItemKind::ExternBlock(block) => {
                ItemKind::ExternBlock(self.lower_extern_block(block))
            }
        };
        Item {
            export: item.export,
            span: item.span,
            def_ids,
            kind,
        }
    }

    fn item_def_ids(&self, item: &ast::Item) -> Vec<DefId> {
        self.item_declared_names(item)
            .into_iter()
            .filter_map(|name| {
                let kinds = all_def_kinds();
                self.lowerer.resolved.local_def(self.module, name, &kinds)
            })
            .collect()
    }

    fn item_declared_names<'c>(&self, item: &'c ast::Item) -> Vec<&'c str> {
        match &item.kind {
            ast::ItemKind::TypeAlias(decl) => vec![decl.name.name.as_str()],
            ast::ItemKind::Struct(decl) => vec![decl.name.name.as_str()],
            ast::ItemKind::Enum(decl) => vec![decl.name.name.as_str()],
            ast::ItemKind::Interface(decl) => vec![decl.signature.name.name.as_str()],
            ast::ItemKind::InterfaceAlias(decl) => vec![decl.name.name.as_str()],
            ast::ItemKind::Function(decl) => vec![decl.signature.name.name.as_str()],
            ast::ItemKind::ExternBlock(block) => block
                .items
                .iter()
                .map(|item| match item {
                    ast::ExternItem::OpaqueStruct(name) => name.name.as_str(),
                    ast::ExternItem::Function { signature, .. } => signature.name.name.as_str(),
                    ast::ExternItem::TypeAlias(alias) => alias.name.name.as_str(),
                })
                .collect(),
            ast::ItemKind::Import(_) | ast::ItemKind::Impl(_) | ast::ItemKind::CInclude(_) => {
                Vec::new()
            }
        }
    }

    fn lower_extern_block(&mut self, block: &ast::ExternBlock) -> ExternBlock {
        let items = block
            .items
            .iter()
            .map(|item| match item {
                ast::ExternItem::OpaqueStruct(name) => ExternItem::OpaqueStruct(name.clone()),
                ast::ExternItem::Function {
                    noescape,
                    signature,
                } => ExternItem::Function {
                    noescape: *noescape,
                    signature: self.lower_signature(signature, false),
                },
                ast::ExternItem::TypeAlias(alias) => {
                    let hidden = self.hidden_generics_for(&alias.generics);
                    self.push_generics_with_hidden(&alias.generics, &hidden);
                    let generics = self.lower_generics_with_hidden(&alias.generics, &hidden);
                    let target = self.lower_type_alias_target(&alias.target);
                    self.pop_generics();
                    ExternItem::TypeAlias(TypeAliasDecl {
                        name: alias.name.clone(),
                        generics,
                        target,
                    })
                }
            })
            .collect();
        ExternBlock {
            is_unsafe: block.is_unsafe,
            abi: block.abi.clone(),
            items,
        }
    }

    fn lower_type_alias_target(&mut self, target: &ast::TypeAliasTarget) -> TypeAliasTarget {
        match target {
            ast::TypeAliasTarget::Type(ty) => TypeAliasTarget::Type(self.lower_type(ty)),
            ast::TypeAliasTarget::CSpelling { abi, spelling } => TypeAliasTarget::CSpelling {
                abi: abi.clone(),
                spelling: spelling.clone(),
            },
        }
    }

    fn lower_generics(&mut self, generics: &[ast::GenericParam]) -> Vec<GenericParam> {
        self.lower_generics_with_hidden(generics, &[])
    }

    fn lower_generics_with_hidden(
        &mut self,
        generics: &[ast::GenericParam],
        hidden: &[HiddenGenericParam],
    ) -> Vec<GenericParam> {
        let mut lowered = generics
            .iter()
            .map(|param| GenericParam {
                is_resource: param.is_resource,
                is_hidden: false,
                name: param.name.clone(),
                constraint: param
                    .constraint
                    .as_ref()
                    .map(|constraint| self.lower_constraint_expr(constraint)),
            })
            .collect::<Vec<_>>();
        lowered.extend(hidden.iter().map(|param| {
            GenericParam {
                is_resource: false,
                is_hidden: true,
                name: param.name.clone(),
                constraint: param
                    .constraint
                    .as_ref()
                    .map(|constraint| self.lower_constraint_expr(constraint)),
            }
        }));
        lowered
    }

    fn lower_constraint_expr(&mut self, expr: &ast::ConstraintExpr) -> ConstraintExpr {
        ConstraintExpr {
            terms: expr
                .terms
                .iter()
                .map(|term| ConstraintTerm {
                    negated: term.negated,
                    removed: term.removed,
                    name: self.resolve_interface_name(&term.name),
                    args: term
                        .args
                        .iter()
                        .map(|arg| self.lower_constraint_arg(arg))
                        .collect(),
                })
                .collect(),
        }
    }

    fn lower_constraint_arg(&mut self, arg: &ast::ConstraintArg) -> ConstraintArg {
        match arg {
            ast::ConstraintArg::Type(ty) => ConstraintArg::Type(self.lower_type(ty)),
            ast::ConstraintArg::Binding {
                name,
                constraint,
                span,
            } => ConstraintArg::Binding {
                name: name.clone(),
                constraint: constraint
                    .as_ref()
                    .map(|constraint| self.lower_constraint_expr(constraint)),
                span: *span,
            },
        }
    }

    fn lower_interface_expr(&mut self, expr: &ast::InterfaceExpr) -> InterfaceExpr {
        InterfaceExpr {
            first: self.lower_interface_term(&expr.first),
            rest: expr
                .rest
                .iter()
                .map(|(op, term)| (op.clone(), self.lower_interface_term(term)))
                .collect(),
        }
    }

    fn lower_interface_term(&mut self, term: &ast::InterfaceTerm) -> InterfaceTerm {
        InterfaceTerm {
            negated: term.negated,
            name: self.resolve_interface_name(&term.name),
            args: term.args.iter().map(|ty| self.lower_type(ty)).collect(),
        }
    }

    fn lower_signature(
        &mut self,
        signature: &ast::FunctionSignature,
        bind_params: bool,
    ) -> FunctionSignature {
        self.lower_signature_with_hidden(signature, &[], bind_params)
    }

    fn lower_signature_with_hidden(
        &mut self,
        signature: &ast::FunctionSignature,
        hidden: &[HiddenGenericParam],
        bind_params: bool,
    ) -> FunctionSignature {
        let generics = self.lower_generics_with_hidden(&signature.generics, hidden);
        let ret = self.lower_function_return_type(&signature.ret);
        let params = signature
            .params
            .iter()
            .map(|param| self.lower_param(param, bind_params))
            .collect();
        FunctionSignature {
            ret,
            name: signature.name.clone(),
            generics,
            params,
            receiver_selector: signature.receiver_selector.clone().map(|selector| {
                ReceiverSelector {
                    receiver_param: selector.receiver_param,
                    name: selector.name,
                    span: selector.span,
                }
            }),
        }
    }

    fn lower_function_return_type(&mut self, ret: &ast::FunctionReturnType) -> FunctionReturnType {
        match ret {
            ast::FunctionReturnType::Type(ty) => FunctionReturnType::Type(self.lower_type(ty)),
            ast::FunctionReturnType::OpaqueConstraint {
                marker_span,
                constraint,
            } => FunctionReturnType::OpaqueConstraint {
                marker_span: *marker_span,
                constraint: self.lower_constraint_expr(constraint),
            },
        }
    }

    fn lower_param(&mut self, param: &ast::Param, bind: bool) -> Param {
        let ty = self.lower_type(&param.ty);
        let local_id = bind.then(|| self.alloc_local(&param.name));
        Param {
            ty,
            name: param.name.clone(),
            mutability: param.mutability,
            local_id,
        }
    }

    fn lower_type(&mut self, ty: &ast::Type) -> Type {
        let kind = match &ty.kind {
            ast::TypeKind::Never => TypeKind::Never,
            ast::TypeKind::Hole => TypeKind::Hole,
            ast::TypeKind::Void => TypeKind::Void,
            ast::TypeKind::Primitive(primitive) => TypeKind::Primitive(primitive.clone()),
            ast::TypeKind::Named(path, args) => TypeKind::Named(
                self.resolve_type_name(path, ty.span),
                args.iter().map(|arg| self.lower_type(arg)).collect(),
            ),
            ast::TypeKind::Pointer {
                nullable,
                mutability,
                inner,
            } => TypeKind::Pointer {
                nullable: *nullable,
                mutability: *mutability,
                inner: Box::new(self.lower_type(inner)),
            },
            ast::TypeKind::Array { len, elem } => TypeKind::Array {
                len: *len,
                elem: Box::new(self.lower_type(elem)),
            },
            ast::TypeKind::Slice { mutability, elem } => TypeKind::Slice {
                mutability: *mutability,
                elem: Box::new(self.lower_type(elem)),
            },
            ast::TypeKind::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => TypeKind::Function {
                is_unsafe: *is_unsafe,
                abi: abi.clone(),
                ret: Box::new(self.lower_type(ret)),
                params: params.iter().map(|param| self.lower_type(param)).collect(),
            },
            ast::TypeKind::Closure {
                ret,
                params,
                constraint,
            } => TypeKind::Closure {
                ret: Box::new(self.lower_type(ret)),
                params: params.iter().map(|param| self.lower_type(param)).collect(),
                constraint: constraint
                    .as_ref()
                    .map(|constraint| self.lower_constraint_expr(constraint)),
            },
        };
        Type {
            span: ty.span,
            kind,
        }
    }

    fn lower_block_with_existing_scope(&mut self, block: &ast::Block) -> Block {
        let statements = block
            .statements
            .iter()
            .map(|stmt| self.lower_stmt(stmt))
            .collect();
        Block {
            span: block.span,
            statements,
        }
    }

    fn lower_block(&mut self, block: &ast::Block) -> Block {
        self.push_scope();
        let block = self.lower_block_with_existing_scope(block);
        self.pop_scope();
        block
    }

    fn lower_stmt(&mut self, stmt: &ast::Stmt) -> Stmt {
        let kind = match &stmt.kind {
            ast::StmtKind::Block(block) => StmtKind::Block(self.lower_block(block)),
            ast::StmtKind::VarDecl {
                ty,
                name,
                mutability,
                init,
            } => {
                let ty = self.lower_type(ty);
                let init = init.as_ref().map(|expr| self.lower_expr(expr));
                let local_id = self.alloc_local(name);
                self.insert_existing_local(name.clone(), local_id);
                StmtKind::VarDecl {
                    ty,
                    name: name.clone(),
                    mutability: *mutability,
                    local_id,
                    init,
                }
            }
            ast::StmtKind::Assign { target, value } => StmtKind::Assign {
                target: self.lower_expr(target),
                value: self.lower_expr(value),
            },
            ast::StmtKind::If {
                cond,
                then_block,
                else_branch,
            } => StmtKind::If {
                cond: self.lower_expr(cond),
                then_block: self.lower_block(then_block),
                else_branch: else_branch
                    .as_ref()
                    .map(|stmt| Box::new(self.lower_stmt(stmt))),
            },
            ast::StmtKind::While { cond, body } => StmtKind::While {
                cond: self.lower_expr(cond),
                body: self.lower_block(body),
            },
            ast::StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                self.push_scope();
                let init = init.as_ref().map(|init| self.lower_for_init(init));
                let cond = cond.as_ref().map(|expr| self.lower_expr(expr));
                let step = step.as_ref().map(|step| self.lower_for_init(step));
                let body = self.lower_block(body);
                self.pop_scope();
                StmtKind::For {
                    init,
                    cond,
                    step,
                    body,
                }
            }
            ast::StmtKind::Switch {
                expr,
                cases,
                has_default,
                default,
            } => StmtKind::Switch {
                expr: self.lower_expr(expr),
                cases: cases
                    .iter()
                    .map(|case| {
                        self.push_scope();
                        let pattern = self.lower_pattern(&case.pattern, true);
                        let statements = case
                            .statements
                            .iter()
                            .map(|stmt| self.lower_stmt(stmt))
                            .collect();
                        self.pop_scope();
                        CaseClause {
                            pattern,
                            statements,
                        }
                    })
                    .collect(),
                has_default: *has_default,
                default: {
                    self.push_scope();
                    let default = default.iter().map(|stmt| self.lower_stmt(stmt)).collect();
                    self.pop_scope();
                    default
                },
            },
            ast::StmtKind::Defer(expr) => StmtKind::Defer(self.lower_expr(expr)),
            ast::StmtKind::Return(expr) => {
                StmtKind::Return(expr.as_ref().map(|expr| self.lower_expr(expr)))
            }
            ast::StmtKind::Break => StmtKind::Break,
            ast::StmtKind::Continue => StmtKind::Continue,
            ast::StmtKind::Expr(expr) => StmtKind::Expr(self.lower_expr(expr)),
        };
        Stmt {
            span: stmt.span,
            kind,
        }
    }

    fn lower_for_init(&mut self, init: &ast::ForInit) -> ForInit {
        match init {
            ast::ForInit::VarDecl {
                ty,
                name,
                mutability,
                init,
            } => {
                let ty = self.lower_type(ty);
                let init = init.as_ref().map(|expr| self.lower_expr(expr));
                let local_id = self.alloc_local(name);
                self.insert_existing_local(name.clone(), local_id);
                ForInit::VarDecl {
                    ty,
                    name: name.clone(),
                    mutability: *mutability,
                    local_id,
                    init,
                }
            }
            ast::ForInit::Assign { target, value } => ForInit::Assign {
                target: self.lower_expr(target),
                value: self.lower_expr(value),
            },
            ast::ForInit::Expr(expr) => ForInit::Expr(self.lower_expr(expr)),
        }
    }

    fn lower_pattern(&mut self, pattern: &ast::Pattern, is_case_head: bool) -> Pattern {
        match pattern {
            ast::Pattern::Wildcard(span) => Pattern::Wildcard(*span),
            ast::Pattern::Binding { name, mutability } => {
                let local_id = self.alloc_local(name);
                self.insert_existing_local(name.clone(), local_id);
                Pattern::Variant(
                    PatternName {
                        path: vec![name.clone()],
                        display: name.name.clone(),
                        span: name.span,
                        kind: PatternNameKind::Binding {
                            local_id,
                            mutability: *mutability,
                        },
                    },
                    Vec::new(),
                )
            }
            ast::Pattern::Variant(path, subpatterns) => {
                let display = path_display(path);
                let span = path.first().unwrap().span.merge(path.last().unwrap().span);
                let last = path.last().unwrap();
                let variant_candidates = self.resolve_visible_variant_candidates(path);
                let kind = match variant_candidates {
                    Ok(candidates) if !candidates.is_empty() => pattern_variant_kind(candidates),
                    Ok(_) => {
                        let resolved = self.try_resolve_visible_non_variant_name(path);
                        match resolved {
                            Some(def_id) if is_case_head || !subpatterns.is_empty() => {
                                let def = self.lowerer.resolved.def(def_id);
                                self.lowerer.diagnostics.push(Diagnostic::new(
                                    span,
                                    format!(
                                        "`{}` resolves to {}, not enum variant",
                                        display,
                                        def_kind_name(&def.kind)
                                    ),
                                ));
                                PatternNameKind::Error
                            }
                            Some(_) | None if is_case_head => {
                                self.lowerer.diagnostics.push(Diagnostic::new(
                                    span,
                                    "switch case must name an enum variant",
                                ));
                                PatternNameKind::Error
                            }
                            Some(_) | None if path.len() == 1 => {
                                let local_id = self.alloc_local(last);
                                self.insert_existing_local(last.clone(), local_id);
                                PatternNameKind::Binding {
                                    local_id,
                                    mutability: BindingMutability::Immutable,
                                }
                            }
                            Some(_) | None => {
                                self.lowerer.diagnostics.push(Diagnostic::new(
                                    span,
                                    format!("unknown pattern `{display}`"),
                                ));
                                PatternNameKind::Error
                            }
                        }
                    }
                    Err(error) => {
                        self.push_lookup_error(error, "pattern", span);
                        PatternNameKind::Error
                    }
                };
                Pattern::Variant(
                    PatternName {
                        path: path.clone(),
                        display,
                        span,
                        kind,
                    },
                    subpatterns
                        .iter()
                        .map(|pattern| self.lower_pattern(pattern, false))
                        .collect(),
                )
            }
        }
    }

    fn lower_expr(&mut self, expr: &ast::Expr) -> Expr {
        let kind = match &expr.kind {
            ast::ExprKind::Name(path) => ExprKind::Name(self.resolve_name(path, "name")),
            ast::ExprKind::Literal(literal) => ExprKind::Literal(literal.clone()),
            ast::ExprKind::StructLiteral(fields) => ExprKind::StructLiteral(
                fields
                    .iter()
                    .map(|field| FieldInit {
                        name: field.name.clone(),
                        expr: self.lower_expr(&field.expr),
                    })
                    .collect(),
            ),
            ast::ExprKind::ArrayLiteral(elements) => {
                ExprKind::ArrayLiteral(elements.iter().map(|expr| self.lower_expr(expr)).collect())
            }
            ast::ExprKind::ArrayRepeat { element, len } => ExprKind::ArrayRepeat {
                element: Box::new(self.lower_expr(element)),
                len: *len,
            },
            ast::ExprKind::Closure {
                is_async,
                params,
                body,
            } => {
                self.push_scope();
                let params = params
                    .iter()
                    .map(|param| {
                        let ty = param.ty.as_ref().map(|ty| self.lower_type(ty));
                        let local_id = self.alloc_local(&param.name);
                        self.insert_existing_local(param.name.clone(), local_id);
                        ClosureParam {
                            ty,
                            name: param.name.clone(),
                            mutability: param.mutability,
                            local_id,
                        }
                    })
                    .collect::<Vec<_>>();
                let body = match body {
                    ast::ClosureBody::Expr(expr) => {
                        ClosureBody::Expr(Box::new(self.lower_expr(expr)))
                    }
                    ast::ClosureBody::Block(block) => {
                        ClosureBody::Block(self.lower_block_with_existing_scope(block))
                    }
                };
                self.pop_scope();
                ExprKind::Closure {
                    is_async: *is_async,
                    params,
                    body,
                }
            }
            ast::ExprKind::Unary { op, expr } => ExprKind::Unary {
                op: *op,
                expr: Box::new(self.lower_expr(expr)),
            },
            ast::ExprKind::Binary { op, left, right } => ExprKind::Binary {
                op: *op,
                left: Box::new(self.lower_expr(left)),
                right: Box::new(self.lower_expr(right)),
            },
            ast::ExprKind::Cast { expr, ty } => ExprKind::Cast {
                expr: Box::new(self.lower_expr(expr)),
                ty: self.lower_type(ty),
            },
            ast::ExprKind::UnsafeBlock(block) => {
                ExprKind::UnsafeBlock(self.lower_expr_block(block))
            }
            ast::ExprKind::Call {
                callee,
                type_args,
                args,
            } => ExprKind::Call {
                callee: Box::new(self.lower_expr(callee)),
                type_args: type_args.iter().map(|ty| self.lower_type(ty)).collect(),
                args: args.iter().map(|expr| self.lower_expr(expr)).collect(),
            },
            ast::ExprKind::Field { base, field } => ExprKind::Field {
                base: Box::new(self.lower_expr(base)),
                field: field.clone(),
            },
            ast::ExprKind::ReceiverSelector { base, selector } => ExprKind::ReceiverSelector {
                base: Box::new(self.lower_expr(base)),
                selector: selector.clone(),
            },
            ast::ExprKind::Arrow { base, field } => ExprKind::Arrow {
                base: Box::new(self.lower_expr(base)),
                field: field.clone(),
            },
            ast::ExprKind::Index { base, index } => ExprKind::Index {
                base: Box::new(self.lower_expr(base)),
                index: Box::new(self.lower_expr(index)),
            },
            ast::ExprKind::Slice { base, start, end } => ExprKind::Slice {
                base: Box::new(self.lower_expr(base)),
                start: start.as_ref().map(|expr| Box::new(self.lower_expr(expr))),
                end: end.as_ref().map(|expr| Box::new(self.lower_expr(expr))),
            },
            ast::ExprKind::Try(inner) => ExprKind::Try(Box::new(self.lower_expr(inner))),
            ast::ExprKind::Await(inner) => ExprKind::Await(Box::new(self.lower_expr(inner))),
            ast::ExprKind::Select { biased, arms } => ExprKind::Select {
                biased: *biased,
                arms: arms
                    .iter()
                    .map(|arm| {
                        let future = self.lower_expr(&arm.future);
                        self.push_scope();
                        let binding_local = self.alloc_local(&arm.binding);
                        self.insert_existing_local(arm.binding.clone(), binding_local);
                        let body = self.lower_expr(&arm.body);
                        self.pop_scope();
                        SelectArm {
                            binding: arm.binding.clone(),
                            binding_local,
                            future,
                            body,
                        }
                    })
                    .collect(),
            },
        };
        Expr {
            span: expr.span,
            kind,
        }
    }

    fn lower_expr_block(&mut self, block: &ast::ExprBlock) -> ExprBlock {
        self.push_scope();
        let statements = block
            .statements
            .iter()
            .map(|stmt| self.lower_stmt(stmt))
            .collect();
        let value = block
            .value
            .as_ref()
            .map(|expr| Box::new(self.lower_expr(expr)));
        self.pop_scope();
        ExprBlock {
            span: block.span,
            statements,
            value,
        }
    }

    fn resolve_type_name(&mut self, path: &[ast::Ident], span: Span) -> TypeName {
        let display = path_display(path);
        if path.len() == 1 {
            let name = &path[0].name;
            if self.lookup_local(name).is_some() {
                self.lowerer.diagnostics.push(Diagnostic::new(
                    span,
                    format!("local `{name}` shadows type lookup `{name}`"),
                ));
                return TypeName {
                    display,
                    span,
                    kind: TypeNameKind::Error,
                };
            }
            if self.generic_in_scope(name) {
                return TypeName {
                    display,
                    span,
                    kind: TypeNameKind::Generic(name.clone()),
                };
            }
        }
        let name = self.resolve_global_name(path, span, "type");
        self.require_type_name_kind(&name);
        TypeName {
            display,
            span,
            kind: match name.kind {
                NameRefKind::Def(def_id) => TypeNameKind::Def(def_id),
                NameRefKind::Local(_) | NameRefKind::VariantCandidates(_) | NameRefKind::Error => {
                    TypeNameKind::Error
                }
            },
        }
    }

    fn resolve_interface_name(&mut self, path: &[ast::Ident]) -> NameRef {
        let name = self.resolve_name(path, "interface");
        self.require_def_kind(
            &name,
            &[DefKind::Interface, DefKind::InterfaceAlias],
            "interface",
        );
        name
    }

    fn resolve_name(&mut self, path: &[ast::Ident], context: &str) -> NameRef {
        let display = path_display(path);
        let span = path
            .first()
            .map(|first| first.span)
            .unwrap_or_else(|| Span::new(FileId(0), 0, 0));
        if path.len() == 1
            && let Some(local_id) = self.lookup_local(&path[0].name)
        {
            return NameRef {
                display,
                span,
                kind: NameRefKind::Local(local_id),
            };
        }
        self.resolve_global_name(path, span, context)
    }

    fn resolve_global_name(&mut self, path: &[ast::Ident], span: Span, context: &str) -> NameRef {
        let display = path_display(path);
        let non_variant_result = self.resolve_visible_non_variant_path(path);
        match non_variant_result.clone() {
            Ok(Some(def_id)) => NameRef {
                display,
                span,
                kind: NameRefKind::Def(def_id),
            },
            Ok(None) => {
                if context == "name" {
                    match self.resolve_visible_variant_candidates(path) {
                        Ok(candidates) if !candidates.is_empty() => {
                            return NameRef {
                                display,
                                span,
                                kind: name_variant_kind(candidates),
                            };
                        }
                        Ok(_) => {}
                        Err(error) => {
                            self.push_lookup_error(error, context, span);
                            return NameRef {
                                display,
                                span,
                                kind: NameRefKind::Error,
                            };
                        }
                    }
                }
                let message = if context == "type" {
                    format!("unknown type `{display}`")
                } else {
                    format!("unresolved {context} `{display}`")
                };
                self.lowerer
                    .diagnostics
                    .push(Diagnostic::new(span, message));
                NameRef {
                    display,
                    span,
                    kind: NameRefKind::Error,
                }
            }
            Err(error) => {
                if context == "name" {
                    match self.resolve_visible_variant_candidates(path) {
                        Ok(candidates) if !candidates.is_empty() => {
                            return NameRef {
                                display,
                                span,
                                kind: name_variant_kind(candidates),
                            };
                        }
                        Ok(_) => {}
                        Err(variant_error) => {
                            self.push_lookup_error(variant_error, context, span);
                            return NameRef {
                                display,
                                span,
                                kind: NameRefKind::Error,
                            };
                        }
                    }
                }
                self.push_lookup_error(error, context, span);
                NameRef {
                    display,
                    span,
                    kind: NameRefKind::Error,
                }
            }
        }
    }

    fn resolve_visible_non_variant_path(
        &mut self,
        path: &[ast::Ident],
    ) -> Result<Option<DefId>, LookupError> {
        let kinds = non_variant_def_kinds();
        match path {
            [name] => {
                if let Some(def_id) = self.lexical_defs.get(&name.name).copied() {
                    let def = self.lowerer.resolved.def(def_id);
                    if kind_matches(&def.kind, &kinds) {
                        Ok(Some(def_id))
                    } else {
                        Ok(None)
                    }
                } else {
                    self.lowerer
                        .resolved
                        .lookup_imported_bare(self.module, &name.name, &kinds)
                }
            }
            [alias, name] => {
                self.lowerer
                    .resolved
                    .lookup_qualified(self.module, &alias.name, &name.name, &kinds)
            }
            _ => Err(LookupError::TooManySegments { len: path.len() }),
        }
    }

    fn try_resolve_visible_non_variant_name(&mut self, path: &[ast::Ident]) -> Option<DefId> {
        self.resolve_visible_non_variant_path(path).ok().flatten()
    }

    fn resolve_visible_variant_candidates(
        &mut self,
        path: &[ast::Ident],
    ) -> Result<Vec<DefId>, LookupError> {
        match path {
            [name] => {
                let mut candidates = self.local_visible_variant_defs(&name.name);
                candidates.extend(
                    self.lowerer
                        .resolved
                        .lookup_imported_bare_variants(self.module, &name.name)?,
                );
                dedup_def_ids(&mut candidates);
                Ok(candidates)
            }
            [head, tail] => {
                if let Some(enum_def_id) = self.resolve_visible_enum_path(&[head.clone()])? {
                    return Ok(self
                        .lowerer
                        .resolved
                        .enum_variant_def(enum_def_id, &tail.name)
                        .into_iter()
                        .collect());
                }
                self.lowerer
                    .resolved
                    .lookup_qualified_variants(self.module, &head.name, &tail.name)
            }
            [alias, enum_name, variant] => {
                if let Some(enum_def_id) =
                    self.resolve_visible_enum_path(&[alias.clone(), enum_name.clone()])?
                {
                    Ok(self
                        .lowerer
                        .resolved
                        .enum_variant_def(enum_def_id, &variant.name)
                        .into_iter()
                        .collect())
                } else {
                    Ok(Vec::new())
                }
            }
            _ => Err(LookupError::TooManySegments { len: path.len() }),
        }
    }

    fn resolve_visible_enum_path(
        &mut self,
        path: &[ast::Ident],
    ) -> Result<Option<DefId>, LookupError> {
        match path {
            [name] => {
                if let Some(def_id) = self.lexical_defs.get(&name.name).copied() {
                    let def = self.lowerer.resolved.def(def_id);
                    return Ok((def.kind == DefKind::Enum).then_some(def_id));
                }
                self.lowerer.resolved.lookup_imported_bare(
                    self.module,
                    &name.name,
                    &[DefKind::Enum],
                )
            }
            [alias, name] => self.lowerer.resolved.lookup_qualified(
                self.module,
                &alias.name,
                &name.name,
                &[DefKind::Enum],
            ),
            _ => Err(LookupError::TooManySegments { len: path.len() }),
        }
    }

    fn local_visible_variant_defs(&self, name: &str) -> Vec<DefId> {
        let visible_enums = self
            .lexical_defs
            .values()
            .copied()
            .filter(|def_id| self.lowerer.resolved.def(*def_id).kind == DefKind::Enum)
            .collect::<HashSet<_>>();
        self.lowerer.resolved.modules[self.module.0]
            .defs
            .iter()
            .copied()
            .filter(|def_id| {
                let def = self.lowerer.resolved.def(*def_id);
                def.kind == DefKind::EnumVariant
                    && def.name == name
                    && def
                        .parent
                        .is_some_and(|enum_def| visible_enums.contains(&enum_def))
            })
            .collect()
    }

    fn require_type_name_kind(&mut self, name: &NameRef) {
        self.require_def_kind(
            name,
            &[
                DefKind::TypeAlias,
                DefKind::Struct,
                DefKind::Enum,
                DefKind::Interface,
                DefKind::InterfaceAlias,
                DefKind::OpaqueStruct,
            ],
            "type",
        );
    }

    fn require_def_kind(&mut self, name: &NameRef, allowed: &[DefKind], expected: &str) {
        let NameRefKind::Def(def_id) = name.kind else {
            return;
        };
        let def = self.lowerer.resolved.def(def_id);
        if !allowed.iter().any(|kind| *kind == def.kind) {
            self.lowerer.diagnostics.push(Diagnostic::new(
                name.span,
                format!(
                    "`{}` resolves to {}, not {expected}",
                    name.display,
                    def_kind_name(&def.kind)
                ),
            ));
        }
    }

    fn push_lookup_error(&mut self, error: LookupError, context: &str, span: Span) {
        match error {
            LookupError::Ambiguous { name, candidates } => {
                self.lowerer.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "ambiguous {context} lookup `{name}` ({} candidates)",
                        candidates.len()
                    ),
                ));
            }
            LookupError::UnknownAlias { alias } => {
                self.lowerer.diagnostics.push(Diagnostic::new(
                    span,
                    format!("unknown import alias `{alias}`"),
                ));
            }
            LookupError::NotExported { name } => {
                self.lowerer
                    .diagnostics
                    .push(Diagnostic::new(span, format!("`{name}` is not exported")));
            }
            LookupError::UnresolvedImport { path } => {
                self.lowerer
                    .diagnostics
                    .push(Diagnostic::new(span, format!("unresolved import `{path}`")));
            }
            LookupError::TooManySegments { len } => {
                self.lowerer.diagnostics.push(Diagnostic::new(
                    span,
                    format!("unsupported qualified lookup with {len} segments"),
                ));
            }
        }
    }

    fn hidden_generics_for(&mut self, generics: &[ast::GenericParam]) -> Vec<HiddenGenericParam> {
        let mut hidden = Vec::new();
        let mut seen = generics
            .iter()
            .map(|generic| generic.name.name.clone())
            .collect::<HashSet<_>>();
        for generic in generics {
            if let Some(constraint) = &generic.constraint {
                self.collect_hidden_generics_from_constraint(constraint, &mut seen, &mut hidden);
            }
        }
        hidden
    }

    fn collect_hidden_generics_from_constraint(
        &mut self,
        constraint: &ast::ConstraintExpr,
        seen: &mut HashSet<String>,
        hidden: &mut Vec<HiddenGenericParam>,
    ) {
        for term in &constraint.terms {
            for arg in &term.args {
                self.collect_hidden_generics_from_constraint_arg(arg, seen, hidden);
            }
        }
    }

    fn collect_hidden_generics_from_constraint_arg(
        &mut self,
        arg: &ast::ConstraintArg,
        seen: &mut HashSet<String>,
        hidden: &mut Vec<HiddenGenericParam>,
    ) {
        let ast::ConstraintArg::Binding {
            name, constraint, ..
        } = arg
        else {
            return;
        };
        if !seen.insert(name.name.clone()) {
            self.lowerer.diagnostics.push(Diagnostic::new(
                name.span,
                format!("duplicate hidden generic parameter `{}`", name.name),
            ));
            return;
        }
        hidden.push(HiddenGenericParam {
            name: name.clone(),
            constraint: constraint.clone(),
        });
        if let Some(constraint) = constraint {
            self.collect_hidden_generics_from_constraint(constraint, seen, hidden);
        }
    }

    fn push_generics(&mut self, generics: &[ast::GenericParam]) {
        self.push_generics_with_hidden(generics, &[]);
    }

    fn push_generics_with_hidden(
        &mut self,
        generics: &[ast::GenericParam],
        hidden: &[HiddenGenericParam],
    ) {
        let mut scope = HashSet::new();
        for generic in generics {
            if !scope.insert(generic.name.name.clone()) {
                self.lowerer.diagnostics.push(Diagnostic::new(
                    generic.name.span,
                    format!("duplicate generic parameter `{}`", generic.name.name),
                ));
            }
        }
        for generic in hidden {
            if !scope.insert(generic.name.name.clone()) {
                self.lowerer.diagnostics.push(Diagnostic::new(
                    generic.name.span,
                    format!("duplicate generic parameter `{}`", generic.name.name),
                ));
            }
        }
        self.generic_scopes.push(scope);
    }

    fn pop_generics(&mut self) {
        self.generic_scopes.pop();
    }

    fn generic_in_scope(&self, name: &str) -> bool {
        self.generic_scopes
            .iter()
            .rev()
            .any(|scope| scope.contains(name))
    }

    fn push_scope(&mut self) {
        self.local_scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.local_scopes.pop();
    }

    fn alloc_local(&mut self, name: &ast::Ident) -> LocalId {
        let id = LocalId(self.lowerer.locals.len());
        self.lowerer.locals.push(Local {
            id,
            name: name.name.clone(),
            span: name.span,
        });
        id
    }

    fn insert_existing_local(&mut self, name: ast::Ident, local_id: LocalId) {
        let Some(scope) = self.local_scopes.last_mut() else {
            return;
        };
        if scope.contains_key(&name.name) {
            self.lowerer.diagnostics.push(Diagnostic::new(
                name.span,
                format!("duplicate local `{}`", name.name),
            ));
        } else {
            scope.insert(name.name, local_id);
        }
    }

    fn lookup_local(&self, name: &str) -> Option<LocalId> {
        self.local_scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }
}

fn all_def_kinds() -> [DefKind; 9] {
    [
        DefKind::TypeAlias,
        DefKind::Struct,
        DefKind::Enum,
        DefKind::EnumVariant,
        DefKind::Interface,
        DefKind::InterfaceAlias,
        DefKind::Function,
        DefKind::ExternFunction,
        DefKind::OpaqueStruct,
    ]
}

fn non_variant_def_kinds() -> [DefKind; 8] {
    [
        DefKind::TypeAlias,
        DefKind::Struct,
        DefKind::Enum,
        DefKind::Interface,
        DefKind::InterfaceAlias,
        DefKind::Function,
        DefKind::ExternFunction,
        DefKind::OpaqueStruct,
    ]
}

fn kind_matches(kind: &DefKind, kinds: &[DefKind]) -> bool {
    kinds.iter().any(|candidate| candidate == kind)
}

fn name_variant_kind(candidates: Vec<DefId>) -> NameRefKind {
    let mut candidates = candidates;
    dedup_def_ids(&mut candidates);
    if candidates.len() == 1 {
        NameRefKind::Def(candidates[0])
    } else {
        NameRefKind::VariantCandidates(candidates)
    }
}

fn pattern_variant_kind(candidates: Vec<DefId>) -> PatternNameKind {
    let mut candidates = candidates;
    dedup_def_ids(&mut candidates);
    if candidates.len() == 1 {
        PatternNameKind::Variant(candidates[0])
    } else {
        PatternNameKind::VariantCandidates(candidates)
    }
}

fn dedup_def_ids(defs: &mut Vec<DefId>) {
    let mut seen = HashSet::new();
    defs.retain(|id| seen.insert(*id));
}

fn def_kind_name(kind: &DefKind) -> &'static str {
    match kind {
        DefKind::TypeAlias => "type alias",
        DefKind::Struct => "struct",
        DefKind::Enum => "enum",
        DefKind::EnumVariant => "enum variant",
        DefKind::Interface => "interface",
        DefKind::InterfaceAlias => "interface alias",
        DefKind::Function => "function",
        DefKind::ExternFunction => "extern function",
        DefKind::OpaqueStruct => "opaque struct",
    }
}

fn path_display(path: &[ast::Ident]) -> String {
    path.iter()
        .map(|ident| ident.name.as_str())
        .collect::<Vec<_>>()
        .join("::")
}
