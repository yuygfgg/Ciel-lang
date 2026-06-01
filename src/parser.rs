use crate::{
    ast::*,
    diagnostic::{DiagResult, Diagnostic},
    lexer::{Token, TokenKind},
};

pub fn parse_file(tokens: Vec<Token>) -> DiagResult<AstFile> {
    Parser::new(tokens).parse_file()
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    diagnostics: Vec<Diagnostic>,
    inherited_type_abi: Option<String>,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            pos: 0,
            diagnostics: Vec::new(),
            inherited_type_abi: None,
        }
    }

    fn parse_file(mut self) -> DiagResult<AstFile> {
        let mut items = Vec::new();
        while !self.at(TokenKind::Eof) {
            match self.parse_item() {
                Ok(item) => items.push(item),
                Err(diagnostic) => {
                    self.diagnostics.push(diagnostic);
                    self.synchronize_item();
                }
            }
        }

        if self.diagnostics.is_empty() {
            Ok(AstFile { items })
        } else {
            Err(self.diagnostics)
        }
    }

    fn parse_item(&mut self) -> Result<Item, Diagnostic> {
        let export = self.eat(TokenKind::Export).is_some();
        let start = self.peek().span;
        let is_unsafe = self.eat(TokenKind::Unsafe).is_some();

        let kind = match self.peek().kind {
            TokenKind::Import | TokenKind::HashCInclude | TokenKind::Type | TokenKind::Enum
                if is_unsafe =>
            {
                return Err(Diagnostic::new(
                    start,
                    "`unsafe` is valid only on functions, extern blocks, interfaces, impls, structs, and unsafe blocks",
                ));
            }
            TokenKind::Import => ItemKind::Import(self.parse_import_decl()?),
            TokenKind::HashCInclude => {
                self.advance();
                let include = self.expect_string("expected include string after #c_include")?;
                self.eat(TokenKind::Semi);
                ItemKind::CInclude(include)
            }
            TokenKind::Type => ItemKind::TypeAlias(self.parse_type_alias_decl(None)?),
            TokenKind::Struct => ItemKind::Struct(self.parse_struct_decl(is_unsafe)?),
            TokenKind::Enum => ItemKind::Enum(self.parse_enum_decl()?),
            TokenKind::Interface => self.parse_interface_item(is_unsafe)?,
            TokenKind::Impl => {
                if export {
                    return Err(Diagnostic::new(
                        start,
                        "`impl` declarations cannot be exported",
                    ));
                }
                ItemKind::Impl(self.parse_impl_decl(is_unsafe)?)
            }
            TokenKind::Extern => self.parse_extern_or_function_item(is_unsafe)?,
            TokenKind::Noescape => {
                return Err(Diagnostic::new(
                    start,
                    "`noescape` is allowed only inside `extern \"C\"` blocks",
                ));
            }
            TokenKind::HashIf => {
                return Err(Diagnostic::new(
                    start,
                    "configuration gates are not implemented in this compiler slice",
                ));
            }
            _ => ItemKind::Function(self.parse_function_decl(None, is_unsafe)?),
        };

        let end = self.previous().span;
        Ok(Item {
            export,
            span: start.merge(end),
            kind,
        })
    }

    fn parse_import_decl(&mut self) -> Result<ImportDecl, Diagnostic> {
        self.expect(TokenKind::Import, "expected `import`")?;
        let path = self.parse_module_path()?;
        let alias = if self.eat(TokenKind::As).is_some() {
            Some(self.expect_ident("expected import alias")?)
        } else {
            None
        };
        self.expect(TokenKind::Semi, "expected `;` after import")?;
        Ok(ImportDecl { path, alias })
    }

    fn parse_module_path(&mut self) -> Result<ModulePath, Diagnostic> {
        let absolute = self.eat(TokenKind::Slash).is_some();
        let mut raw = String::new();
        if absolute {
            raw.push('/');
        } else if self.at(TokenKind::Dot) {
            self.advance();
            self.expect(TokenKind::Slash, "expected `/` after `.` in module path")?;
            raw.push_str("./");
        }

        let first = self.expect_ident("expected module path segment")?;
        raw.push_str(&first.name);
        while self.eat(TokenKind::Slash).is_some() {
            raw.push('/');
            let segment = self.expect_ident("expected module path segment")?;
            raw.push_str(&segment.name);
        }

        Ok(ModulePath { absolute, raw })
    }

    fn parse_type_alias_decl(
        &mut self,
        c_spelling_abi: Option<String>,
    ) -> Result<TypeAliasDecl, Diagnostic> {
        self.expect(TokenKind::Type, "expected `type`")?;
        let name = self.expect_ident("expected type alias name")?;
        let generics = self.parse_generic_param_list_opt()?;
        self.expect(TokenKind::Eq, "expected `=` in type alias")?;
        let target = if self.at(TokenKind::String) {
            let spelling = self.expect_string("expected C type spelling")?;
            let Some(abi) = c_spelling_abi else {
                return Err(Diagnostic::new(
                    name.span,
                    "C spelling type aliases require `extern \"C\"`",
                ));
            };
            if !generics.is_empty() {
                return Err(Diagnostic::new(
                    name.span,
                    "C spelling type aliases cannot be generic",
                ));
            }
            TypeAliasTarget::CSpelling { abi, spelling }
        } else {
            TypeAliasTarget::Type(self.parse_type()?)
        };
        self.expect(TokenKind::Semi, "expected `;` after type alias")?;
        Ok(TypeAliasDecl {
            name,
            generics,
            target,
        })
    }

    fn parse_struct_decl(&mut self, is_unsafe: bool) -> Result<StructDecl, Diagnostic> {
        self.expect(TokenKind::Struct, "expected `struct`")?;
        let name = self.expect_ident("expected struct name")?;
        let generics = self.parse_generic_param_list_opt()?;
        self.expect(TokenKind::LBrace, "expected `{` in struct declaration")?;
        let mut fields = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let ty = self.parse_type()?;
            let name = self.expect_ident("expected field name")?;
            self.expect(TokenKind::Semi, "expected `;` after field")?;
            fields.push(FieldDecl { ty, name });
        }
        self.expect(TokenKind::RBrace, "expected `}` after struct declaration")?;
        Ok(StructDecl {
            is_unsafe,
            name,
            generics,
            fields,
        })
    }

    fn parse_enum_decl(&mut self) -> Result<EnumDecl, Diagnostic> {
        self.expect(TokenKind::Enum, "expected `enum`")?;
        let name = self.expect_ident("expected enum name")?;
        let generics = self.parse_generic_param_list_opt()?;
        self.expect(TokenKind::LBrace, "expected `{` in enum declaration")?;
        let mut variants = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let variant = self.expect_ident("expected enum variant name")?;
            let payload = if self.eat(TokenKind::LParen).is_some() {
                let tys = if self.at(TokenKind::RParen) {
                    Vec::new()
                } else {
                    self.parse_type_list()?
                };
                self.expect(TokenKind::RParen, "expected `)` after variant payload")?;
                tys
            } else {
                Vec::new()
            };
            variants.push(VariantDecl {
                name: variant,
                payload,
            });
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
        }
        self.expect(TokenKind::RBrace, "expected `}` after enum declaration")?;
        Ok(EnumDecl {
            name,
            generics,
            variants,
        })
    }

    fn parse_interface_item(&mut self, is_unsafe: bool) -> Result<ItemKind, Diagnostic> {
        self.expect(TokenKind::Interface, "expected `interface`")?;
        if self.at(TokenKind::Ident) {
            if is_unsafe {
                return Err(Diagnostic::new(
                    self.peek().span,
                    "interface aliases cannot be declared `unsafe`",
                ));
            }
            let name = self.expect_ident("expected interface alias name")?;
            let generics = self.parse_generic_param_list_opt()?;
            self.expect(TokenKind::Eq, "expected `=` in interface alias")?;
            let expr = self.parse_interface_expr()?;
            self.expect(TokenKind::Semi, "expected `;` after interface alias")?;
            return Ok(ItemKind::InterfaceAlias(InterfaceAliasDecl {
                name,
                generics,
                expr,
            }));
        }

        let generics = self.parse_generic_param_list()?;
        let signature = self.parse_function_signature()?;
        self.expect(TokenKind::Semi, "expected `;` after interface declaration")?;
        Ok(ItemKind::Interface(InterfaceDecl {
            is_unsafe,
            generics,
            signature,
        }))
    }

    fn parse_interface_expr(&mut self) -> Result<InterfaceExpr, Diagnostic> {
        let first = self.parse_interface_term(false)?;
        let mut rest = Vec::new();
        while self.at(TokenKind::Plus) || self.at(TokenKind::Minus) {
            let (op, removed) = if self.eat(TokenKind::Plus).is_some() {
                (InterfaceOp::Add, false)
            } else {
                self.expect(TokenKind::Minus, "expected interface operator")?;
                (InterfaceOp::Sub, true)
            };
            rest.push((op, self.parse_interface_term(removed)?));
        }
        Ok(InterfaceExpr { first, rest })
    }

    fn parse_interface_term(&mut self, removed: bool) -> Result<InterfaceTerm, Diagnostic> {
        let negated = self.eat(TokenKind::Bang).is_some();
        if removed && negated {
            return Err(Diagnostic::new(
                self.previous().span,
                "`- !Capability` is not a valid interface expression",
            ));
        }
        let name = self.expect_ident("expected interface name")?;
        let args = self.parse_type_arg_list_opt()?;
        Ok(InterfaceTerm {
            negated,
            name,
            args,
        })
    }

    fn parse_impl_decl(&mut self, is_unsafe: bool) -> Result<ImplDecl, Diagnostic> {
        self.expect(TokenKind::Impl, "expected `impl`")?;
        let generics = self.parse_generic_param_list_opt()?;
        let name = self.expect_ident("expected interface name after `impl`")?;
        let args = self.parse_type_arg_list_opt()?;
        self.expect(TokenKind::LParen, "expected `(` after impl name")?;
        let params = self.parse_param_list_until_rparen()?;
        let body = self.parse_block()?;
        Ok(ImplDecl {
            is_unsafe,
            generics,
            name,
            args,
            params,
            body,
        })
    }

    fn parse_extern_or_function_item(&mut self, is_unsafe: bool) -> Result<ItemKind, Diagnostic> {
        let save = self.pos;
        self.expect(TokenKind::Extern, "expected `extern`")?;
        let abi = self.expect_abi_string("expected ABI string after `extern`")?;
        if self.eat(TokenKind::LBrace).is_some() {
            self.with_inherited_type_abi(Some(abi.clone()), |parser| {
                let mut items = Vec::new();
                while !parser.at(TokenKind::RBrace) && !parser.at(TokenKind::Eof) {
                    if parser.eat(TokenKind::Opaque).is_some() {
                        parser.expect(TokenKind::Struct, "expected `struct` after `opaque`")?;
                        let name = parser.expect_ident("expected opaque struct name")?;
                        parser.expect(TokenKind::Semi, "expected `;` after opaque struct")?;
                        items.push(ExternItem::OpaqueStruct(name));
                        continue;
                    }
                    if parser.at(TokenKind::Type) {
                        items.push(ExternItem::TypeAlias(
                            parser.parse_type_alias_decl(Some(abi.clone()))?,
                        ));
                        continue;
                    }
                    let mut noescape = false;
                    while parser.eat(TokenKind::Noescape).is_some() {
                        noescape = true;
                    }
                    let signature = parser.parse_function_signature()?;
                    parser.expect(TokenKind::Semi, "expected `;` after extern function")?;
                    items.push(ExternItem::Function {
                        noescape,
                        signature,
                    });
                }
                parser.expect(TokenKind::RBrace, "expected `}` after extern block")?;
                Ok(ItemKind::ExternBlock(ExternBlock {
                    is_unsafe,
                    abi,
                    items,
                }))
            })
        } else {
            if self.at(TokenKind::Type) {
                if is_unsafe {
                    return Err(Diagnostic::new(
                        self.peek().span,
                        "C spelling type aliases cannot be declared `unsafe`",
                    ));
                }
                self.with_inherited_type_abi(Some(abi.clone()), |parser| {
                    parser
                        .parse_type_alias_decl(Some(abi))
                        .map(ItemKind::TypeAlias)
                })
            } else {
                self.pos = save;
                Ok(ItemKind::Function(
                    self.parse_function_decl(None, is_unsafe)?,
                ))
            }
        }
    }

    fn parse_function_decl(
        &mut self,
        inherited_abi: Option<String>,
        is_unsafe: bool,
    ) -> Result<FunctionDecl, Diagnostic> {
        let abi = if self.at(TokenKind::Extern) {
            self.expect(TokenKind::Extern, "expected `extern`")?;
            Some(self.expect_abi_string("expected ABI string after `extern`")?)
        } else {
            inherited_abi
        };
        let signature = self.parse_function_signature()?;
        let body = if self.eat(TokenKind::Semi).is_some() {
            None
        } else {
            Some(self.parse_block()?)
        };
        Ok(FunctionDecl {
            is_unsafe,
            abi,
            signature,
            body,
        })
    }

    fn parse_function_signature(&mut self) -> Result<FunctionSignature, Diagnostic> {
        let ret = self.parse_type()?;
        let name = self.expect_ident("expected function name")?;
        let generics = self.parse_generic_param_list_opt()?;
        self.expect(TokenKind::LParen, "expected `(` after function name")?;
        let params = self.parse_param_list_until_rparen()?;
        Ok(FunctionSignature {
            ret,
            name,
            generics,
            params,
        })
    }

    fn parse_param_list_until_rparen(&mut self) -> Result<Vec<Param>, Diagnostic> {
        let mut params = Vec::new();
        if self.eat(TokenKind::RParen).is_some() {
            return Ok(params);
        }
        loop {
            let ty = self.parse_type()?;
            let (name, mutability) = self.parse_binding_name("expected parameter name")?;
            params.push(Param {
                ty,
                name,
                mutability,
            });
            if self.eat(TokenKind::Comma).is_some() {
                if self.eat(TokenKind::RParen).is_some() {
                    break;
                }
            } else {
                self.expect(TokenKind::RParen, "expected `)` after parameter list")?;
                break;
            }
        }
        Ok(params)
    }

    fn parse_binding_name(
        &mut self,
        message: &'static str,
    ) -> Result<(Ident, BindingMutability), Diagnostic> {
        let mutability = if self.eat(TokenKind::At).is_some() {
            BindingMutability::Mutable
        } else {
            BindingMutability::Immutable
        };
        let name = self.expect_ident(message)?;
        Ok((name, mutability))
    }

    fn parse_generic_param_list_opt(&mut self) -> Result<Vec<GenericParam>, Diagnostic> {
        if self.at(TokenKind::Lt) {
            self.parse_generic_param_list()
        } else {
            Ok(Vec::new())
        }
    }

    fn parse_generic_param_list(&mut self) -> Result<Vec<GenericParam>, Diagnostic> {
        self.expect(TokenKind::Lt, "expected `<`")?;
        let mut params = Vec::new();
        if self.eat_type_gt().is_some() {
            return Ok(params);
        }
        loop {
            let name = self.expect_ident("expected generic parameter name")?;
            let constraint = if self.eat(TokenKind::Colon).is_some() {
                Some(self.parse_constraint_expr()?)
            } else {
                None
            };
            params.push(GenericParam { name, constraint });
            if self.eat(TokenKind::Comma).is_some() {
                if self.eat_type_gt().is_some() {
                    break;
                }
            } else {
                self.expect_type_gt("expected `>` after generic parameter list")?;
                break;
            }
        }
        Ok(params)
    }

    fn parse_constraint_expr(&mut self) -> Result<ConstraintExpr, Diagnostic> {
        let mut terms = vec![self.parse_constraint_term(false)?];
        while self.at(TokenKind::Plus) || self.at(TokenKind::Minus) {
            let removed = if self.eat(TokenKind::Plus).is_some() {
                false
            } else {
                self.expect(TokenKind::Minus, "expected capability operator")?;
                true
            };
            terms.push(self.parse_constraint_term(removed)?);
        }
        Ok(ConstraintExpr { terms })
    }

    fn parse_constraint_term(&mut self, removed: bool) -> Result<ConstraintTerm, Diagnostic> {
        let negated = self.eat(TokenKind::Bang).is_some();
        if removed && negated {
            return Err(Diagnostic::new(
                self.previous().span,
                "`- !Capability` is not a valid capability expression",
            ));
        }
        let name = self.expect_ident("expected capability name")?;
        let args = self.parse_type_arg_list_opt()?;
        Ok(ConstraintTerm {
            negated,
            removed,
            name,
            args,
        })
    }

    fn parse_type_arg_list_opt(&mut self) -> Result<Vec<Type>, Diagnostic> {
        if self.at(TokenKind::Lt) {
            self.expect(TokenKind::Lt, "expected `<`")?;
            let list = self.parse_type_list()?;
            self.expect_type_gt("expected `>` after type arguments")?;
            Ok(list)
        } else {
            Ok(Vec::new())
        }
    }

    fn parse_type_list(&mut self) -> Result<Vec<Type>, Diagnostic> {
        let mut list = Vec::new();
        loop {
            list.push(self.parse_type()?);
            if self.eat(TokenKind::Comma).is_some() {
                if self.at_type_gt() || self.at(TokenKind::RParen) {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(list)
    }

    fn parse_type(&mut self) -> Result<Type, Diagnostic> {
        let is_unsafe = self.eat(TokenKind::Unsafe).is_some();
        let explicit_abi = if self.at(TokenKind::Extern) {
            self.expect(TokenKind::Extern, "expected `extern`")?;
            Some(self.expect_abi_string("expected ABI string after `extern`")?)
        } else {
            None
        };
        let abi = explicit_abi
            .clone()
            .or_else(|| self.inherited_type_abi.clone());
        let mut ty = self.parse_prefix_type()?;
        while self.at_ident_named("fn") || self.at(TokenKind::Pipe) {
            if self.at_ident_named("fn") {
                self.advance();
                self.expect(
                    TokenKind::LParen,
                    "expected `(` after `fn` in function type",
                )?;
                let params = if self.at(TokenKind::RParen) {
                    Vec::new()
                } else {
                    self.parse_type_list()?
                };
                let end =
                    self.expect(TokenKind::RParen, "expected `)` after function type params")?;
                let span = ty.span.merge(end.span);
                ty = Type {
                    span,
                    kind: TypeKind::Function {
                        is_unsafe,
                        abi: abi.clone(),
                        ret: Box::new(ty),
                        params,
                    },
                };
            } else {
                self.expect(TokenKind::Pipe, "expected `|` in closure type")?;
                self.expect(TokenKind::LParen, "expected `(` after `|` in closure type")?;
                let params = if self.at(TokenKind::RParen) {
                    Vec::new()
                } else {
                    self.parse_type_list()?
                };
                self.expect(TokenKind::RParen, "expected `)` after closure type params")?;
                let constraint = if self.eat(TokenKind::Colon).is_some() {
                    Some(self.parse_constraint_expr()?)
                } else {
                    None
                };
                let end = self.expect(TokenKind::Pipe, "expected `|` after closure type params")?;
                let span = ty.span.merge(end.span);
                ty = Type {
                    span,
                    kind: TypeKind::Closure {
                        ret: Box::new(ty),
                        params,
                        constraint,
                    },
                };
            }
        }
        if explicit_abi.is_some() && !matches!(ty.kind, TypeKind::Function { .. }) {
            return Err(Diagnostic::new(
                ty.span,
                "ABI specifier is valid only on function types",
            ));
        }
        if is_unsafe && !matches!(ty.kind, TypeKind::Function { .. }) {
            return Err(Diagnostic::new(
                ty.span,
                "`unsafe` is valid only on function types",
            ));
        }
        Ok(ty)
    }

    fn parse_prefix_type(&mut self) -> Result<Type, Diagnostic> {
        if self.eat(TokenKind::Star).is_some() {
            let start = self.previous().span;
            let mutability = if self.eat(TokenKind::Const).is_some() {
                ViewMutability::ReadOnly
            } else {
                ViewMutability::Writable
            };
            let inner = self.parse_prefix_type()?;
            return Ok(Type {
                span: start.merge(inner.span),
                kind: TypeKind::Pointer {
                    nullable: false,
                    mutability,
                    inner: Box::new(inner),
                },
            });
        }
        if self.eat(TokenKind::QStar).is_some() {
            let start = self.previous().span;
            let mutability = if self.eat(TokenKind::Const).is_some() {
                ViewMutability::ReadOnly
            } else {
                ViewMutability::Writable
            };
            let inner = self.parse_prefix_type()?;
            return Ok(Type {
                span: start.merge(inner.span),
                kind: TypeKind::Pointer {
                    nullable: true,
                    mutability,
                    inner: Box::new(inner),
                },
            });
        }
        if self.at(TokenKind::Const) {
            return Err(Diagnostic::new(
                self.peek().span,
                "standalone `const T` is not a Ciel type; use `*const T` or `[]const T`",
            ));
        }
        self.parse_primary_type()
    }

    fn parse_primary_type(&mut self) -> Result<Type, Diagnostic> {
        let token = self.peek().clone();
        if token.kind == TokenKind::Ident && token.lexeme == "_" {
            self.advance();
            return Ok(Type {
                span: token.span,
                kind: TypeKind::Hole,
            });
        }
        match token.kind {
            TokenKind::Never => {
                self.advance();
                Ok(Type {
                    span: token.span,
                    kind: TypeKind::Never,
                })
            }
            TokenKind::Void => {
                self.advance();
                Ok(Type {
                    span: token.span,
                    kind: TypeKind::Void,
                })
            }
            TokenKind::Bool
            | TokenKind::Char
            | TokenKind::I8
            | TokenKind::I16
            | TokenKind::I32
            | TokenKind::I64
            | TokenKind::U8
            | TokenKind::U16
            | TokenKind::U32
            | TokenKind::U64
            | TokenKind::Usize
            | TokenKind::F32
            | TokenKind::F64 => {
                self.advance();
                Ok(Type {
                    span: token.span,
                    kind: TypeKind::Primitive(self.primitive_from_token(token.kind)),
                })
            }
            TokenKind::Ident => {
                let mut path = vec![self.expect_ident("expected type name")?];
                while self.eat(TokenKind::ColonColon).is_some() {
                    path.push(self.expect_ident("expected qualified type name segment")?);
                }
                let args = self.parse_type_arg_list_opt()?;
                let span = if let Some(last) = args.last() {
                    path.first().unwrap().span.merge(last.span)
                } else {
                    path.first().unwrap().span.merge(path.last().unwrap().span)
                };
                Ok(Type {
                    span,
                    kind: TypeKind::Named(path, args),
                })
            }
            TokenKind::LBracket => {
                let start = self.expect(TokenKind::LBracket, "expected `[`")?.span;
                if self.eat(TokenKind::RBracket).is_some() {
                    let mutability = if self.eat(TokenKind::Const).is_some() {
                        ViewMutability::ReadOnly
                    } else {
                        ViewMutability::Writable
                    };
                    let elem = self.parse_type()?;
                    Ok(Type {
                        span: start.merge(elem.span),
                        kind: TypeKind::Slice {
                            mutability,
                            elem: Box::new(elem),
                        },
                    })
                } else {
                    let len_token = self.expect_any(
                        &[TokenKind::Int, TokenKind::IntDec],
                        "expected array length",
                    )?;
                    let len = parse_usize_literal(&len_token.lexeme).ok_or_else(|| {
                        Diagnostic::new(len_token.span, "array length is not a valid usize")
                    })?;
                    self.expect(TokenKind::RBracket, "expected `]` after array length")?;
                    let elem = self.parse_type()?;
                    Ok(Type {
                        span: start.merge(elem.span),
                        kind: TypeKind::Array {
                            len,
                            elem: Box::new(elem),
                        },
                    })
                }
            }
            TokenKind::LParen => {
                self.advance();
                let ty = self.parse_type()?;
                self.expect(TokenKind::RParen, "expected `)` after type")?;
                Ok(ty)
            }
            _ => Err(Diagnostic::new(token.span, "expected type")),
        }
    }

    fn primitive_from_token(&self, kind: TokenKind) -> PrimitiveType {
        match kind {
            TokenKind::Bool => PrimitiveType::Bool,
            TokenKind::Char => PrimitiveType::Char,
            TokenKind::I8 => PrimitiveType::I8,
            TokenKind::I16 => PrimitiveType::I16,
            TokenKind::I32 => PrimitiveType::I32,
            TokenKind::I64 => PrimitiveType::I64,
            TokenKind::U8 => PrimitiveType::U8,
            TokenKind::U16 => PrimitiveType::U16,
            TokenKind::U32 => PrimitiveType::U32,
            TokenKind::U64 => PrimitiveType::U64,
            TokenKind::Usize => PrimitiveType::Usize,
            TokenKind::F32 => PrimitiveType::F32,
            TokenKind::F64 => PrimitiveType::F64,
            _ => unreachable!("not a primitive token"),
        }
    }

    fn parse_block(&mut self) -> Result<Block, Diagnostic> {
        let start = self.expect(TokenKind::LBrace, "expected `{`")?.span;
        let mut statements = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            statements.push(self.parse_statement()?);
        }
        let end = self.expect(TokenKind::RBrace, "expected `}`")?.span;
        Ok(Block {
            span: start.merge(end),
            statements,
        })
    }

    fn parse_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.peek().span;
        match self.peek().kind {
            TokenKind::LBrace => {
                let block = self.parse_block()?;
                Ok(Stmt {
                    span: block.span,
                    kind: StmtKind::Block(block),
                })
            }
            TokenKind::If => self.parse_if_statement(),
            TokenKind::While => self.parse_while_statement(),
            TokenKind::For => self.parse_for_statement(),
            TokenKind::Switch => self.parse_switch_statement(),
            TokenKind::Defer => {
                self.advance();
                let call = self.parse_expr()?;
                self.expect(TokenKind::Semi, "expected `;` after defer")?;
                Ok(Stmt {
                    span: start.merge(call.span),
                    kind: StmtKind::Defer(call),
                })
            }
            TokenKind::Return => {
                self.advance();
                let expr = if self.at(TokenKind::Semi) {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                let end = self
                    .expect(TokenKind::Semi, "expected `;` after return")?
                    .span;
                Ok(Stmt {
                    span: start.merge(end),
                    kind: StmtKind::Return(expr),
                })
            }
            TokenKind::Break => {
                self.advance();
                let end = self
                    .expect(TokenKind::Semi, "expected `;` after break")?
                    .span;
                Ok(Stmt {
                    span: start.merge(end),
                    kind: StmtKind::Break,
                })
            }
            TokenKind::Continue => {
                self.advance();
                let end = self
                    .expect(TokenKind::Semi, "expected `;` after continue")?
                    .span;
                Ok(Stmt {
                    span: start.merge(end),
                    kind: StmtKind::Continue,
                })
            }
            _ => {
                if let Some(stmt) = self.try_parse_var_decl_statement()? {
                    return Ok(stmt);
                }
                let expr = self.parse_expr()?;
                if self.eat(TokenKind::Eq).is_some() {
                    let value = self.parse_expr()?;
                    let end = self
                        .expect(TokenKind::Semi, "expected `;` after assignment")?
                        .span;
                    Ok(Stmt {
                        span: expr.span.merge(end),
                        kind: StmtKind::Assign {
                            target: expr,
                            value,
                        },
                    })
                } else {
                    let end = self
                        .expect(TokenKind::Semi, "expected `;` after expression")?
                        .span;
                    Ok(Stmt {
                        span: expr.span.merge(end),
                        kind: StmtKind::Expr(expr),
                    })
                }
            }
        }
    }

    fn try_parse_var_decl_statement(&mut self) -> Result<Option<Stmt>, Diagnostic> {
        if !self.can_start_type() {
            return Ok(None);
        }
        if self.at(TokenKind::Const) {
            self.parse_type()?;
        }
        let save = self.pos;
        let ty = match self.parse_type() {
            Ok(ty) => ty,
            Err(_) => {
                self.pos = save;
                return Ok(None);
            }
        };
        if !(self.at(TokenKind::Ident) || self.at(TokenKind::At)) {
            self.pos = save;
            return Ok(None);
        }
        let (name, mutability) = self.parse_binding_name("expected local name")?;
        if !(self.at(TokenKind::Eq) || self.at(TokenKind::Semi)) {
            self.pos = save;
            return Ok(None);
        }
        let init = if self.eat(TokenKind::Eq).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let end = self
            .expect(TokenKind::Semi, "expected `;` after local declaration")?
            .span;
        Ok(Some(Stmt {
            span: ty.span.merge(end),
            kind: StmtKind::VarDecl {
                ty,
                name,
                mutability,
                init,
            },
        }))
    }

    fn parse_if_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.expect(TokenKind::If, "expected `if`")?.span;
        self.expect(TokenKind::LParen, "expected `(` after `if`")?;
        let cond = self.parse_expr()?;
        self.expect(TokenKind::RParen, "expected `)` after if condition")?;
        let then_block = self.parse_block()?;
        let else_branch = if self.eat(TokenKind::Else).is_some() {
            if self.at(TokenKind::If) {
                Some(Box::new(self.parse_if_statement()?))
            } else {
                let block = self.parse_block()?;
                Some(Box::new(Stmt {
                    span: block.span,
                    kind: StmtKind::Block(block),
                }))
            }
        } else {
            None
        };
        let span = if let Some(else_stmt) = &else_branch {
            start.merge(else_stmt.span)
        } else {
            start.merge(then_block.span)
        };
        Ok(Stmt {
            span,
            kind: StmtKind::If {
                cond,
                then_block,
                else_branch,
            },
        })
    }

    fn parse_while_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.expect(TokenKind::While, "expected `while`")?.span;
        self.expect(TokenKind::LParen, "expected `(` after `while`")?;
        let cond = self.parse_expr()?;
        self.expect(TokenKind::RParen, "expected `)` after while condition")?;
        let body = self.parse_block()?;
        Ok(Stmt {
            span: start.merge(body.span),
            kind: StmtKind::While { cond, body },
        })
    }

    fn parse_for_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.expect(TokenKind::For, "expected `for`")?.span;
        self.expect(TokenKind::LParen, "expected `(` after `for`")?;
        let init = if self.at(TokenKind::Semi) {
            None
        } else {
            Some(self.parse_for_init()?)
        };
        self.expect(TokenKind::Semi, "expected `;` after for initializer")?;
        let cond = if self.at(TokenKind::Semi) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect(TokenKind::Semi, "expected `;` after for condition")?;
        let step = if self.at(TokenKind::RParen) {
            None
        } else {
            Some(self.parse_for_step()?)
        };
        self.expect(TokenKind::RParen, "expected `)` after for clauses")?;
        let body = self.parse_block()?;
        Ok(Stmt {
            span: start.merge(body.span),
            kind: StmtKind::For {
                init,
                cond,
                step,
                body,
            },
        })
    }

    fn parse_for_init(&mut self) -> Result<ForInit, Diagnostic> {
        if self.can_start_type() {
            if self.at(TokenKind::Const) {
                self.parse_type()?;
            }
            let save = self.pos;
            if let Ok(ty) = self.parse_type() {
                if self.at(TokenKind::Ident) || self.at(TokenKind::At) {
                    let (name, mutability) =
                        self.parse_binding_name("expected for variable name")?;
                    let init = if self.eat(TokenKind::Eq).is_some() {
                        Some(self.parse_expr()?)
                    } else {
                        None
                    };
                    return Ok(ForInit::VarDecl {
                        ty,
                        name,
                        mutability,
                        init,
                    });
                }
            }
            self.pos = save;
        }
        let expr = self.parse_expr()?;
        if self.eat(TokenKind::Eq).is_some() {
            let value = self.parse_expr()?;
            Ok(ForInit::Assign {
                target: expr,
                value,
            })
        } else {
            Ok(ForInit::Expr(expr))
        }
    }

    fn parse_for_step(&mut self) -> Result<ForInit, Diagnostic> {
        let expr = self.parse_expr()?;
        if self.eat(TokenKind::Eq).is_some() {
            let value = self.parse_expr()?;
            Ok(ForInit::Assign {
                target: expr,
                value,
            })
        } else {
            Ok(ForInit::Expr(expr))
        }
    }

    fn parse_switch_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.expect(TokenKind::Switch, "expected `switch`")?.span;
        self.expect(TokenKind::LParen, "expected `(` after `switch`")?;
        let expr = self.parse_expr()?;
        self.expect(TokenKind::RParen, "expected `)` after switch expression")?;
        self.expect(TokenKind::LBrace, "expected `{` after switch expression")?;
        let mut cases = Vec::new();
        let mut has_default = false;
        let mut default = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            if self.eat(TokenKind::Case).is_some() {
                let pattern = self.parse_pattern()?;
                self.expect(TokenKind::Colon, "expected `:` after case pattern")?;
                let statements = self.parse_case_body()?;
                cases.push(CaseClause {
                    pattern,
                    statements,
                });
            } else if self.eat(TokenKind::Default).is_some() {
                has_default = true;
                self.expect(TokenKind::Colon, "expected `:` after default")?;
                default = self.parse_case_body()?;
            } else {
                return Err(Diagnostic::new(
                    self.peek().span,
                    "expected case or default",
                ));
            }
        }
        let end = self
            .expect(TokenKind::RBrace, "expected `}` after switch")?
            .span;
        Ok(Stmt {
            span: start.merge(end),
            kind: StmtKind::Switch {
                expr,
                cases,
                has_default,
                default,
            },
        })
    }

    fn parse_case_body(&mut self) -> Result<Vec<Stmt>, Diagnostic> {
        let mut statements = Vec::new();
        while !self.at(TokenKind::Case)
            && !self.at(TokenKind::Default)
            && !self.at(TokenKind::RBrace)
            && !self.at(TokenKind::Eof)
        {
            statements.push(self.parse_statement()?);
        }
        Ok(statements)
    }

    fn parse_pattern(&mut self) -> Result<Pattern, Diagnostic> {
        if self.at(TokenKind::At) {
            let (name, mutability) = self.parse_binding_name("expected pattern binding name")?;
            return Ok(Pattern::Binding { name, mutability });
        }
        if self.at_ident_named("_") {
            let ident = self.expect_ident("expected pattern")?;
            return Ok(Pattern::Wildcard(ident.span));
        }
        let mut path = vec![self.expect_ident("expected pattern name")?];
        while self.eat(TokenKind::ColonColon).is_some() {
            path.push(self.expect_ident("expected qualified pattern name segment")?);
        }
        let args = if self.eat(TokenKind::LParen).is_some() {
            let mut args = Vec::new();
            if !self.at(TokenKind::RParen) {
                loop {
                    args.push(self.parse_pattern()?);
                    if self.eat(TokenKind::Comma).is_some() {
                        if self.at(TokenKind::RParen) {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RParen, "expected `)` after pattern")?;
            args
        } else {
            Vec::new()
        };
        Ok(Pattern::Variant(path, args))
    }

    fn parse_expr(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_logical_or()
    }

    fn parse_logical_or(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.parse_logical_and()?;
        while self.eat(TokenKind::PipePipe).is_some() {
            let right = self.parse_logical_and()?;
            expr = self.binary(expr, BinaryOp::Or, right);
        }
        Ok(expr)
    }

    fn parse_logical_and(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.parse_bitwise_or()?;
        while self.eat(TokenKind::AmpAmp).is_some() {
            let right = self.parse_bitwise_or()?;
            expr = self.binary(expr, BinaryOp::And, right);
        }
        Ok(expr)
    }

    fn parse_bitwise_or(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.parse_bitwise_xor()?;
        while self.eat(TokenKind::Pipe).is_some() {
            let right = self.parse_bitwise_xor()?;
            expr = self.binary(expr, BinaryOp::BitOr, right);
        }
        Ok(expr)
    }

    fn parse_bitwise_xor(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.parse_bitwise_and()?;
        while self.eat(TokenKind::Caret).is_some() {
            let right = self.parse_bitwise_and()?;
            expr = self.binary(expr, BinaryOp::BitXor, right);
        }
        Ok(expr)
    }

    fn parse_bitwise_and(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.parse_equality()?;
        while self.eat(TokenKind::Amp).is_some() {
            let right = self.parse_equality()?;
            expr = self.binary(expr, BinaryOp::BitAnd, right);
        }
        Ok(expr)
    }

    fn parse_equality(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.parse_relational()?;
        while self.at(TokenKind::EqEq) || self.at(TokenKind::BangEq) {
            let op = if self.eat(TokenKind::EqEq).is_some() {
                BinaryOp::Eq
            } else {
                self.expect(TokenKind::BangEq, "expected equality operator")?;
                BinaryOp::Ne
            };
            let right = self.parse_relational()?;
            expr = self.binary(expr, op, right);
        }
        Ok(expr)
    }

    fn parse_relational(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.parse_shift()?;
        while matches!(
            self.peek().kind,
            TokenKind::Lt | TokenKind::LtEq | TokenKind::Gt | TokenKind::GtEq
        ) {
            let op = match self.advance().kind {
                TokenKind::Lt => BinaryOp::Lt,
                TokenKind::LtEq => BinaryOp::Le,
                TokenKind::Gt => BinaryOp::Gt,
                TokenKind::GtEq => BinaryOp::Ge,
                _ => unreachable!(),
            };
            let right = self.parse_shift()?;
            expr = self.binary(expr, op, right);
        }
        Ok(expr)
    }

    fn parse_shift(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.parse_additive()?;
        while self.at(TokenKind::LtLt) || self.at(TokenKind::GtGt) {
            let op = if self.eat(TokenKind::LtLt).is_some() {
                BinaryOp::Shl
            } else {
                self.expect(TokenKind::GtGt, "expected shift operator")?;
                BinaryOp::Shr
            };
            let right = self.parse_additive()?;
            expr = self.binary(expr, op, right);
        }
        Ok(expr)
    }

    fn parse_additive(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.parse_multiplicative()?;
        while self.at(TokenKind::Plus) || self.at(TokenKind::Minus) {
            let op = if self.eat(TokenKind::Plus).is_some() {
                BinaryOp::Add
            } else {
                self.expect(TokenKind::Minus, "expected additive operator")?;
                BinaryOp::Sub
            };
            let right = self.parse_multiplicative()?;
            expr = self.binary(expr, op, right);
        }
        Ok(expr)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.parse_cast()?;
        while matches!(
            self.peek().kind,
            TokenKind::Star | TokenKind::Slash | TokenKind::Percent
        ) {
            let op = match self.advance().kind {
                TokenKind::Star => BinaryOp::Mul,
                TokenKind::Slash => BinaryOp::Div,
                TokenKind::Percent => BinaryOp::Rem,
                _ => unreachable!(),
            };
            let right = self.parse_cast()?;
            expr = self.binary(expr, op, right);
        }
        Ok(expr)
    }

    fn parse_cast(&mut self) -> Result<Expr, Diagnostic> {
        let expr = self.parse_unary()?;
        if self.eat(TokenKind::As).is_some() {
            let ty = self.parse_type()?;
            let span = expr.span.merge(ty.span);
            Ok(Expr {
                span,
                kind: ExprKind::Cast {
                    expr: Box::new(expr),
                    ty,
                },
            })
        } else {
            Ok(expr)
        }
    }

    fn parse_unary(&mut self) -> Result<Expr, Diagnostic> {
        let token = self.peek().clone();
        let op = match token.kind {
            TokenKind::Bang => Some(UnaryOp::Not),
            TokenKind::Minus => Some(UnaryOp::Neg),
            TokenKind::Tilde => Some(UnaryOp::BitNot),
            TokenKind::Amp => Some(UnaryOp::Addr),
            TokenKind::Star => Some(UnaryOp::Deref),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let expr = self.parse_unary()?;
            let span = token.span.merge(expr.span);
            Ok(Expr {
                span,
                kind: ExprKind::Unary {
                    op,
                    expr: Box::new(expr),
                },
            })
        } else {
            self.parse_postfix()
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.at(TokenKind::Lt) {
                let save = self.pos;
                let token_save = self.tokens.clone();
                if let Ok(type_args) = self.parse_type_arg_list_opt() {
                    if self.eat(TokenKind::LParen).is_some() {
                        let args = self.parse_arg_list_after_lparen()?;
                        let span = expr.span.merge(self.previous().span);
                        expr = Expr {
                            span,
                            kind: ExprKind::Call {
                                callee: Box::new(expr),
                                type_args,
                                args,
                            },
                        };
                        continue;
                    }
                }
                self.pos = save;
                self.tokens = token_save;
            }
            if self.eat(TokenKind::LParen).is_some() {
                let args = self.parse_arg_list_after_lparen()?;
                let span = expr.span.merge(self.previous().span);
                expr = Expr {
                    span,
                    kind: ExprKind::Call {
                        callee: Box::new(expr),
                        type_args: Vec::new(),
                        args,
                    },
                };
            } else if self.eat(TokenKind::Dot).is_some() {
                let field = self.expect_ident("expected field name")?;
                expr = Expr {
                    span: expr.span.merge(field.span),
                    kind: ExprKind::Field {
                        base: Box::new(expr),
                        field,
                    },
                };
            } else if self.eat(TokenKind::Arrow).is_some() {
                let field = self.expect_ident("expected field name")?;
                expr = Expr {
                    span: expr.span.merge(field.span),
                    kind: ExprKind::Arrow {
                        base: Box::new(expr),
                        field,
                    },
                };
            } else if self.eat(TokenKind::LBracket).is_some() {
                if self.eat(TokenKind::DotDot).is_some() {
                    let range_end = if self.at(TokenKind::RBracket) {
                        None
                    } else {
                        Some(Box::new(self.parse_expr()?))
                    };
                    let end = self
                        .expect(TokenKind::RBracket, "expected `]` after slice range")?
                        .span;
                    expr = Expr {
                        span: expr.span.merge(end),
                        kind: ExprKind::Slice {
                            base: Box::new(expr),
                            start: None,
                            end: range_end,
                        },
                    };
                } else {
                    let first = self.parse_expr()?;
                    if self.eat(TokenKind::DotDot).is_some() {
                        let range_end = if self.at(TokenKind::RBracket) {
                            None
                        } else {
                            Some(Box::new(self.parse_expr()?))
                        };
                        let end = self
                            .expect(TokenKind::RBracket, "expected `]` after slice range")?
                            .span;
                        expr = Expr {
                            span: expr.span.merge(end),
                            kind: ExprKind::Slice {
                                base: Box::new(expr),
                                start: Some(Box::new(first)),
                                end: range_end,
                            },
                        };
                    } else {
                        let end = self
                            .expect(TokenKind::RBracket, "expected `]` after index")?
                            .span;
                        expr = Expr {
                            span: expr.span.merge(end),
                            kind: ExprKind::Index {
                                base: Box::new(expr),
                                index: Box::new(first),
                            },
                        };
                    }
                }
            } else if self.eat(TokenKind::Question).is_some() {
                expr = Expr {
                    span: expr.span.merge(self.previous().span),
                    kind: ExprKind::Try(Box::new(expr)),
                };
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_arg_list_after_lparen(&mut self) -> Result<Vec<Expr>, Diagnostic> {
        let mut args = Vec::new();
        if self.eat(TokenKind::RParen).is_some() {
            return Ok(args);
        }
        loop {
            args.push(self.parse_expr()?);
            if self.eat(TokenKind::Comma).is_some() {
                if self.eat(TokenKind::RParen).is_some() {
                    break;
                }
            } else {
                self.expect(TokenKind::RParen, "expected `)` after argument list")?;
                break;
            }
        }
        Ok(args)
    }

    fn parse_primary(&mut self) -> Result<Expr, Diagnostic> {
        let token = self.peek().clone();
        match token.kind {
            TokenKind::Ident => {
                let mut path = vec![self.expect_ident("expected identifier")?];
                while self.eat(TokenKind::ColonColon).is_some() {
                    path.push(self.expect_ident("expected qualified name segment")?);
                }
                let span = path.first().unwrap().span.merge(path.last().unwrap().span);
                Ok(Expr {
                    span,
                    kind: ExprKind::Name(path),
                })
            }
            TokenKind::Int | TokenKind::IntDec => {
                self.advance();
                Ok(Expr {
                    span: token.span,
                    kind: ExprKind::Literal(Literal::Integer(token.lexeme)),
                })
            }
            TokenKind::Float | TokenKind::FloatExp => {
                self.advance();
                Ok(Expr {
                    span: token.span,
                    kind: ExprKind::Literal(Literal::Float(token.lexeme)),
                })
            }
            TokenKind::CharLit => {
                self.advance();
                Ok(Expr {
                    span: token.span,
                    kind: ExprKind::Literal(Literal::Char(token.lexeme)),
                })
            }
            TokenKind::String => {
                self.advance();
                Ok(Expr {
                    span: token.span,
                    kind: ExprKind::Literal(Literal::String(token.lexeme)),
                })
            }
            TokenKind::True | TokenKind::False => {
                self.advance();
                Ok(Expr {
                    span: token.span,
                    kind: ExprKind::Literal(Literal::Bool(token.kind == TokenKind::True)),
                })
            }
            TokenKind::Null => {
                self.advance();
                Ok(Expr {
                    span: token.span,
                    kind: ExprKind::Literal(Literal::Null),
                })
            }
            TokenKind::PipePipe | TokenKind::Pipe => self.parse_closure_expr(),
            TokenKind::LBrace => self.parse_struct_literal(),
            TokenKind::LBracket => self.parse_array_literal(),
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(TokenKind::RParen, "expected `)` after expression")?;
                Ok(expr)
            }
            TokenKind::Unsafe => self.parse_unsafe_block_expr(),
            _ => Err(Diagnostic::new(token.span, "expected expression")),
        }
    }

    fn parse_unsafe_block_expr(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.expect(TokenKind::Unsafe, "expected `unsafe`")?.span;
        let block = self.parse_expr_block_after_unsafe(start)?;
        let span = block.span;
        Ok(Expr {
            span,
            kind: ExprKind::UnsafeBlock(block),
        })
    }

    fn parse_expr_block_after_unsafe(
        &mut self,
        start: crate::span::Span,
    ) -> Result<ExprBlock, Diagnostic> {
        self.expect(TokenKind::LBrace, "expected `{` after `unsafe`")?;
        let mut statements = Vec::new();
        let mut value = None;
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            if self.expr_block_next_is_statement_head() {
                statements.push(self.parse_statement()?);
                continue;
            }
            if let Some(stmt) = self.try_parse_var_decl_statement()? {
                statements.push(stmt);
                continue;
            }
            let expr = self.parse_expr()?;
            if self.eat(TokenKind::Eq).is_some() {
                let rhs = self.parse_expr()?;
                let end = self
                    .expect(TokenKind::Semi, "expected `;` after assignment")?
                    .span;
                statements.push(Stmt {
                    span: expr.span.merge(end),
                    kind: StmtKind::Assign {
                        target: expr,
                        value: rhs,
                    },
                });
            } else if self.eat(TokenKind::Semi).is_some() {
                let end = self.previous().span;
                statements.push(Stmt {
                    span: expr.span.merge(end),
                    kind: StmtKind::Expr(expr),
                });
            } else {
                value = Some(Box::new(expr));
                break;
            }
        }
        let end = self
            .expect(TokenKind::RBrace, "expected `}` after unsafe block")?
            .span;
        Ok(ExprBlock {
            span: start.merge(end),
            statements,
            value,
        })
    }

    fn expr_block_next_is_statement_head(&self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::LBrace if !self.lbrace_starts_struct_literal()
        ) || matches!(
            self.peek().kind,
            TokenKind::If
                | TokenKind::While
                | TokenKind::For
                | TokenKind::Switch
                | TokenKind::Defer
                | TokenKind::Return
                | TokenKind::Break
                | TokenKind::Continue
        )
    }

    fn lbrace_starts_struct_literal(&self) -> bool {
        self.at(TokenKind::LBrace)
            && self.peek_next().kind == TokenKind::Ident
            && self.peek_n(2).kind == TokenKind::Colon
    }

    fn parse_closure_expr(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek().span;
        let params = if self.eat(TokenKind::PipePipe).is_some() {
            Vec::new()
        } else {
            self.expect(TokenKind::Pipe, "expected `|` to start closure")?;
            let mut params = Vec::new();
            if !self.at(TokenKind::Pipe) {
                loop {
                    params.push(self.parse_closure_param()?);
                    if self.eat(TokenKind::Comma).is_some() {
                        if self.at(TokenKind::Pipe) {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
            self.expect(TokenKind::Pipe, "expected `|` after closure parameters")?;
            params
        };
        let body = if self.at(TokenKind::LBrace) {
            ClosureBody::Block(self.parse_block()?)
        } else {
            ClosureBody::Expr(Box::new(self.parse_expr()?))
        };
        let end = match &body {
            ClosureBody::Expr(expr) => expr.span,
            ClosureBody::Block(block) => block.span,
        };
        Ok(Expr {
            span: start.merge(end),
            kind: ExprKind::Closure { params, body },
        })
    }

    fn parse_closure_param(&mut self) -> Result<ClosureParam, Diagnostic> {
        if (self.at(TokenKind::Ident) || self.at(TokenKind::At))
            && (self.at(TokenKind::At)
                || matches!(self.peek_next().kind, TokenKind::Comma | TokenKind::Pipe))
        {
            let (name, mutability) = self.parse_binding_name("expected closure parameter name")?;
            return Ok(ClosureParam {
                ty: None,
                name,
                mutability,
            });
        }
        let ty = self.parse_type()?;
        let (name, mutability) = self.parse_binding_name("expected closure parameter name")?;
        Ok(ClosureParam {
            ty: Some(ty),
            name,
            mutability,
        })
    }

    fn parse_struct_literal(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.expect(TokenKind::LBrace, "expected `{`")?.span;
        let mut fields = Vec::new();
        if !self.at(TokenKind::RBrace) {
            loop {
                let name = self.expect_ident("expected field name in struct literal")?;
                self.expect(TokenKind::Colon, "expected `:` after field name")?;
                let expr = self.parse_expr()?;
                fields.push(FieldInit { name, expr });
                if self.eat(TokenKind::Comma).is_some() {
                    if self.at(TokenKind::RBrace) {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        let end = self
            .expect(TokenKind::RBrace, "expected `}` after struct literal")?
            .span;
        Ok(Expr {
            span: start.merge(end),
            kind: ExprKind::StructLiteral(fields),
        })
    }

    fn parse_array_literal(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.expect(TokenKind::LBracket, "expected `[`")?.span;
        let mut elements = Vec::new();
        if !self.at(TokenKind::RBracket) {
            let first = self.parse_expr()?;
            if self.eat(TokenKind::Semi).is_some() {
                let len = if self.at(TokenKind::RBracket) {
                    None
                } else {
                    let len_token = self.expect_any(
                        &[TokenKind::Int, TokenKind::IntDec],
                        "expected array repeat length",
                    )?;
                    Some(parse_usize_literal(&len_token.lexeme).ok_or_else(|| {
                        Diagnostic::new(len_token.span, "array repeat length is not a valid usize")
                    })?)
                };
                let end = self
                    .expect(
                        TokenKind::RBracket,
                        "expected `]` after array repeat literal",
                    )?
                    .span;
                return Ok(Expr {
                    span: start.merge(end),
                    kind: ExprKind::ArrayRepeat {
                        element: Box::new(first),
                        len,
                    },
                });
            }
            elements.push(first);
            loop {
                if self.eat(TokenKind::Comma).is_some() {
                    if self.at(TokenKind::RBracket) {
                        break;
                    }
                    elements.push(self.parse_expr()?);
                } else {
                    break;
                }
            }
        }
        let end = self
            .expect(TokenKind::RBracket, "expected `]` after array literal")?
            .span;
        Ok(Expr {
            span: start.merge(end),
            kind: ExprKind::ArrayLiteral(elements),
        })
    }

    fn binary(&self, left: Expr, op: BinaryOp, right: Expr) -> Expr {
        let span = left.span.merge(right.span);
        Expr {
            span,
            kind: ExprKind::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            },
        }
    }

    fn can_start_type(&self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::Unsafe
                | TokenKind::Extern
                | TokenKind::Star
                | TokenKind::QStar
                | TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::Ident
                | TokenKind::Never
                | TokenKind::Void
                | TokenKind::Const
                | TokenKind::Bool
                | TokenKind::Char
                | TokenKind::I8
                | TokenKind::I16
                | TokenKind::I32
                | TokenKind::I64
                | TokenKind::U8
                | TokenKind::U16
                | TokenKind::U32
                | TokenKind::U64
                | TokenKind::Usize
                | TokenKind::F32
                | TokenKind::F64
        )
    }

    fn synchronize_item(&mut self) {
        let start = self.pos;
        while !self.at(TokenKind::Eof) {
            if self.pos > start
                && (self.previous().kind == TokenKind::Semi
                    || self.previous().kind == TokenKind::RBrace)
            {
                return;
            }
            self.advance();
        }
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.peek().kind == kind
    }

    fn at_ident_named(&self, name: &str) -> bool {
        self.peek().kind == TokenKind::Ident && self.peek().lexeme == name
    }

    fn eat(&mut self, kind: TokenKind) -> Option<Token> {
        if self.at(kind) {
            Some(self.advance().clone())
        } else {
            None
        }
    }

    fn expect(&mut self, kind: TokenKind, message: &str) -> Result<Token, Diagnostic> {
        if self.at(kind) {
            Ok(self.advance().clone())
        } else {
            Err(Diagnostic::new(self.peek().span, message))
        }
    }

    fn at_type_gt(&self) -> bool {
        self.at(TokenKind::Gt) || self.at(TokenKind::GtGt)
    }

    fn eat_type_gt(&mut self) -> Option<Token> {
        if self.at(TokenKind::Gt) {
            Some(self.advance().clone())
        } else if self.at(TokenKind::GtGt) {
            Some(self.split_gtgt_token())
        } else {
            None
        }
    }

    fn expect_type_gt(&mut self, message: &str) -> Result<Token, Diagnostic> {
        self.eat_type_gt()
            .ok_or_else(|| Diagnostic::new(self.peek().span, message))
    }

    fn split_gtgt_token(&mut self) -> Token {
        let token = self.peek().clone();
        debug_assert_eq!(token.kind, TokenKind::GtGt);
        let first_end = token.span.start + 1;
        self.tokens[self.pos] = Token {
            kind: TokenKind::Gt,
            lexeme: ">".to_string(),
            span: crate::span::Span::new(token.span.file, first_end, token.span.end),
        };
        Token {
            kind: TokenKind::Gt,
            lexeme: ">".to_string(),
            span: crate::span::Span::new(token.span.file, token.span.start, first_end),
        }
    }

    fn expect_any(&mut self, kinds: &[TokenKind], message: &str) -> Result<Token, Diagnostic> {
        if kinds.iter().any(|kind| self.at(*kind)) {
            Ok(self.advance().clone())
        } else {
            Err(Diagnostic::new(self.peek().span, message))
        }
    }

    fn expect_ident(&mut self, message: &str) -> Result<Ident, Diagnostic> {
        if self.at(TokenKind::Ident) {
            let token = self.advance().clone();
            Ok(Ident {
                name: token.lexeme,
                span: token.span,
            })
        } else {
            Err(Diagnostic::new(self.peek().span, message))
        }
    }

    fn expect_string(&mut self, message: &str) -> Result<String, Diagnostic> {
        if self.at(TokenKind::String) {
            let token = self.advance().clone();
            Ok(unquote_string_literal(&token.lexeme))
        } else {
            Err(Diagnostic::new(self.peek().span, message))
        }
    }

    fn expect_abi_string(&mut self, message: &str) -> Result<String, Diagnostic> {
        if self.at(TokenKind::String) {
            let token = self.advance().clone();
            let abi = unquote_string_literal(&token.lexeme);
            if abi != "C" {
                return Err(Diagnostic::new(
                    token.span,
                    format!("unsupported ABI `{abi}`; only `extern \"C\"` is supported"),
                ));
            }
            Ok(abi)
        } else {
            Err(Diagnostic::new(self.peek().span, message))
        }
    }

    fn with_inherited_type_abi<T>(
        &mut self,
        abi: Option<String>,
        parse: impl FnOnce(&mut Self) -> Result<T, Diagnostic>,
    ) -> Result<T, Diagnostic> {
        let previous = self.inherited_type_abi.clone();
        self.inherited_type_abi = abi;
        let result = parse(self);
        self.inherited_type_abi = previous;
        result
    }

    fn advance(&mut self) -> &Token {
        if !self.at(TokenKind::Eof) {
            self.pos += 1;
        }
        self.previous()
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn peek_next(&self) -> &Token {
        self.tokens.get(self.pos + 1).unwrap_or_else(|| self.peek())
    }

    fn peek_n(&self, n: usize) -> &Token {
        self.tokens.get(self.pos + n).unwrap_or_else(|| self.peek())
    }

    fn previous(&self) -> &Token {
        &self.tokens[self.pos.saturating_sub(1)]
    }
}

fn parse_usize_literal(raw: &str) -> Option<usize> {
    let cleaned = raw.replace('_', "");
    if let Some(hex) = cleaned.strip_prefix("0x") {
        usize::from_str_radix(hex, 16).ok()
    } else {
        cleaned.parse().ok()
    }
}

pub fn unquote_string_literal(raw: &str) -> String {
    raw.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw)
        .to_string()
}
