use logos::Logos;

use crate::{
    diagnostic::{DiagResult, Diagnostic, DiagnosticPhase, WithDiagnostics},
    span::{FileId, Span},
};

#[derive(Logos, Debug, Clone, Copy, PartialEq, Eq)]
#[logos(skip r"[ \t\r\n\f]+")]
#[logos(skip r"//[^\n]*")]
#[logos(skip r"/\*([^*]|\*[^/])*\*/")]
pub enum TokenKind {
    #[token("__CielEofSentinel__")]
    Eof,

    Error,

    #[token("#if")]
    HashIf,
    #[token("#elif")]
    HashElif,
    #[token("#else")]
    HashElse,
    #[token("#endif")]
    HashEndif,
    #[token("#c_include")]
    HashCInclude,

    #[token("extern")]
    Extern,
    #[token("export")]
    Export,
    #[token("import")]
    Import,
    #[token("as")]
    As,
    #[token("type")]
    Type,
    #[token("struct")]
    Struct,
    #[token("enum")]
    Enum,
    #[token("interface")]
    Interface,
    #[token("impl")]
    Impl,
    #[token("opaque")]
    Opaque,
    #[token("noescape")]
    Noescape,
    #[token("unsafe")]
    Unsafe,
    #[token("const")]
    Const,
    #[token("defer")]
    Defer,
    #[token("return")]
    Return,
    #[token("break")]
    Break,
    #[token("continue")]
    Continue,
    #[token("if")]
    If,
    #[token("else")]
    Else,
    #[token("while")]
    While,
    #[token("for")]
    For,
    #[token("switch")]
    Switch,
    #[token("case")]
    Case,
    #[token("default")]
    Default,
    #[token("true")]
    True,
    #[token("false")]
    False,
    #[token("null")]
    Null,
    #[token("never")]
    Never,
    #[token("void")]
    Void,
    #[token("bool")]
    Bool,
    #[token("char")]
    Char,
    #[token("i8")]
    I8,
    #[token("i16")]
    I16,
    #[token("i32")]
    I32,
    #[token("i64")]
    I64,
    #[token("u8")]
    U8,
    #[token("u16")]
    U16,
    #[token("u32")]
    U32,
    #[token("u64")]
    U64,
    #[token("usize")]
    Usize,
    #[token("f32")]
    F32,
    #[token("f64")]
    F64,

    #[regex(r#""([^"\\\n]|\\(["'\\0nrt]|x[0-9A-Fa-f]{2}))*""#)]
    String,
    #[regex(r#"'([^'\\\n]|\\(["'\\0nrt]|x[0-9A-Fa-f]{2}))'"#)]
    CharLit,
    #[regex(r"[0-9][0-9_]*\.[0-9][0-9_]*([eE][+-]?[0-9][0-9_]*)?")]
    Float,
    #[regex(r"[0-9][0-9_]*[eE][+-]?[0-9][0-9_]*")]
    FloatExp,
    #[regex(r"0x[0-9A-Fa-f][0-9A-Fa-f_]*")]
    Int,
    #[regex(r"[0-9][0-9_]*")]
    IntDec,
    #[regex(r"[A-Za-z_][A-Za-z0-9_]*")]
    Ident,

    #[token("::")]
    ColonColon,
    #[token("->")]
    Arrow,
    #[token("==")]
    EqEq,
    #[token("!=")]
    BangEq,
    #[token("<=")]
    LtEq,
    #[token(">=")]
    GtEq,
    #[token("<<")]
    LtLt,
    #[token(">>")]
    GtGt,
    #[token("&&")]
    AmpAmp,
    #[token("||")]
    PipePipe,
    #[token("?*")]
    QStar,

    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token(";")]
    Semi,
    #[token(",")]
    Comma,
    #[token(":")]
    Colon,
    #[token("..")]
    DotDot,
    #[token(".")]
    Dot,
    #[token("?")]
    Question,
    #[token("=")]
    Eq,
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("%")]
    Percent,
    #[token("!")]
    Bang,
    #[token("&")]
    Amp,
    #[token("@")]
    At,
    #[token("|")]
    Pipe,
    #[token("^")]
    Caret,
    #[token("~")]
    Tilde,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub lexeme: String,
    pub span: Span,
}

pub fn lex(file: FileId, source: &str) -> DiagResult<Vec<Token>> {
    let result = lex_lossy(file, source);
    if result.diagnostics.is_empty() {
        Ok(result.value)
    } else {
        Err(result.diagnostics)
    }
}

pub fn lex_lossy(file: FileId, source: &str) -> WithDiagnostics<Vec<Token>> {
    let mut lexer = TokenKind::lexer(source);
    let mut tokens = Vec::new();
    let mut diagnostics = Vec::new();

    while let Some(result) = lexer.next() {
        let range = lexer.span();
        match result {
            Ok(kind) => tokens.push(Token {
                kind,
                lexeme: source[range.clone()].to_string(),
                span: Span::new(file, range.start, range.end),
            }),
            Err(()) => {
                let span = Span::new(file, range.start, range.end);
                let lexeme = source[range.clone()].to_string();
                tokens.push(Token {
                    kind: TokenKind::Error,
                    lexeme: lexeme.clone(),
                    span,
                });
                diagnostics.push(
                    Diagnostic::new(span, format!("unrecognized token `{lexeme}`"))
                        .with_phase(DiagnosticPhase::Lex),
                );
            }
        }
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        lexeme: String::new(),
        span: Span::new(file, source.len(), source.len()),
    });

    WithDiagnostics {
        value: tokens,
        diagnostics,
    }
}
