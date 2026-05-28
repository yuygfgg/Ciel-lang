use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use crate::{
    codegen::generate_c,
    diagnostic::{DiagResult, Diagnostic},
    escape::analyze_escapes,
    hir::lower_to_hir,
    lexer::{Token, TokenKind, lex},
    mono::monomorphize,
    parser::parse_file,
    resolve::{ModuleId, ParsedModule, resolve_modules},
    source::SourceMap,
    typeck::type_check,
};

#[derive(Clone, Debug)]
pub struct CompileOptions {
    pub entry: PathBuf,
    pub project_root: PathBuf,
    pub std_paths: Vec<PathBuf>,
    pub target_os: String,
    pub target_arch: String,
    pub features: HashSet<String>,
}

impl CompileOptions {
    pub fn new(entry: impl Into<PathBuf>) -> Self {
        let entry = entry.into();
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            entry,
            project_root: project_root.clone(),
            std_paths: vec![project_root],
            target_os: std::env::consts::OS.to_string(),
            target_arch: std::env::consts::ARCH.to_string(),
            features: HashSet::new(),
        }
    }

    pub fn with_project_root(mut self, project_root: impl Into<PathBuf>) -> Self {
        let old_root = self.project_root.clone();
        let project_root = project_root.into();
        if self.std_paths == [old_root] {
            self.std_paths = vec![project_root.clone()];
        }
        self.project_root = project_root;
        self
    }

    pub fn with_std_path(mut self, std_path: impl Into<PathBuf>) -> Self {
        self.std_paths.push(std_path.into());
        self
    }

    pub fn with_target_os(mut self, target_os: impl Into<String>) -> Self {
        self.target_os = target_os.into();
        self
    }

    pub fn with_target_arch(mut self, target_arch: impl Into<String>) -> Self {
        self.target_arch = target_arch.into();
        self
    }

    pub fn with_feature(mut self, feature: impl Into<String>) -> Self {
        self.features.insert(feature.into());
        self
    }
}

pub fn compile_to_c(options: CompileOptions) -> DiagResult<String> {
    let config = ConfigEnv::from_options(&options);
    let mut loader = ModuleLoader::new(options.project_root, options.std_paths, config);
    let modules = loader.load_entry(&options.entry)?;
    let resolved = resolve_modules(modules)?;
    let hir = lower_to_hir(resolved)?;
    let checked = type_check(hir)?;
    let mono = monomorphize(checked)?;
    let escapes = analyze_escapes(&mono);
    generate_c(&mono, &escapes, &loader.source_map)
}

pub fn compile_to_c_with_sources(
    options: CompileOptions,
) -> Result<(String, SourceMap), (Vec<Diagnostic>, SourceMap)> {
    let config = ConfigEnv::from_options(&options);
    let mut loader = ModuleLoader::new(options.project_root, options.std_paths, config);
    match loader.load_entry(&options.entry) {
        Ok(modules) => {
            let resolved = match resolve_modules(modules) {
                Ok(resolved) => resolved,
                Err(diags) => return Err((diags, loader.source_map)),
            };
            let hir = match lower_to_hir(resolved) {
                Ok(hir) => hir,
                Err(diags) => return Err((diags, loader.source_map)),
            };
            let checked = match type_check(hir) {
                Ok(checked) => checked,
                Err(diags) => return Err((diags, loader.source_map)),
            };
            let mono = match monomorphize(checked) {
                Ok(mono) => mono,
                Err(diags) => return Err((diags, loader.source_map)),
            };
            let result = {
                let escapes = analyze_escapes(&mono);
                generate_c(&mono, &escapes, &loader.source_map)
            };
            match result {
                Ok(c) => Ok((c, loader.source_map)),
                Err(diags) => Err((diags, loader.source_map)),
            }
        }
        Err(diags) => Err((diags, loader.source_map)),
    }
}

struct ModuleLoader {
    project_root: PathBuf,
    std_paths: Vec<PathBuf>,
    config: ConfigEnv,
    source_map: SourceMap,
    loaded: HashMap<PathBuf, ModuleId>,
    loading: HashSet<PathBuf>,
    modules: Vec<ParsedModule>,
}

impl ModuleLoader {
    fn new(project_root: PathBuf, std_paths: Vec<PathBuf>, config: ConfigEnv) -> Self {
        Self {
            project_root,
            std_paths,
            config,
            source_map: SourceMap::default(),
            loaded: HashMap::new(),
            loading: HashSet::new(),
            modules: Vec::new(),
        }
    }

    fn load_entry(&mut self, entry: &Path) -> DiagResult<Vec<ParsedModule>> {
        let path = self.normalize_path(entry);
        self.load_file(&path)?;
        Ok(std::mem::take(&mut self.modules))
    }

    fn load_file(&mut self, path: &Path) -> DiagResult<ModuleId> {
        let path = self.normalize_path(path);
        if let Some(id) = self.loaded.get(&path) {
            return Ok(*id);
        }
        if !self.loading.insert(path.clone()) {
            return Err(vec![Diagnostic::new(
                None,
                format!("import cycle involving `{}`", path.display()),
            )]);
        }

        let text = fs::read_to_string(&path).map_err(|error| {
            vec![Diagnostic::new(
                None,
                format!("failed to read `{}`: {error}", path.display()),
            )]
        })?;
        let file_id = self.source_map.add(path.clone(), text.clone());
        let tokens = preprocess_config(lex(file_id, &text)?, &self.config)?;
        let ast = parse_file(tokens)?;
        let id = ModuleId(self.modules.len());
        self.loaded.insert(path.clone(), id);
        self.modules.push(ParsedModule {
            id,
            path: path.clone(),
            ast: ast.clone(),
        });

        let parent = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        for item in &ast.items {
            if let crate::ast::ItemKind::Import(import) = &item.kind {
                let import_path = self.resolve_import_path(&parent, &import.path.raw);
                self.load_file(&import_path)?;
            }
        }

        self.loading.remove(&self.normalize_path(&path));
        Ok(id)
    }

    fn resolve_import_path(&self, parent: &Path, raw: &str) -> PathBuf {
        let mut path = if let Some(rest) = raw.strip_prefix('/') {
            let mut candidates = self.std_paths.iter().map(|root| root.join(rest));
            let first = candidates
                .next()
                .unwrap_or_else(|| self.project_root.join(rest));
            candidates
                .find(|path| path.with_extension("ciel").exists())
                .unwrap_or(first)
        } else if let Some(rest) = raw.strip_prefix("./") {
            parent.join(rest)
        } else {
            parent.join(raw)
        };
        path.set_extension("ciel");
        path
    }

    fn normalize_path(&self, path: &Path) -> PathBuf {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.project_root.join(path)
        };
        path.components().collect()
    }
}

#[derive(Clone, Debug)]
struct ConfigEnv {
    target_os: String,
    target_arch: String,
    features: HashSet<String>,
}

impl ConfigEnv {
    fn from_options(options: &CompileOptions) -> Self {
        Self {
            target_os: options.target_os.clone(),
            target_arch: options.target_arch.clone(),
            features: options.features.clone(),
        }
    }

    fn eval_call(&self, name: &str, value: &str) -> Option<bool> {
        match name {
            "has_feature" => Some(self.features.contains(value)),
            "is_target_os" => Some(self.target_os == value),
            "is_target_arch" => Some(self.target_arch == value),
            _ => None,
        }
    }
}

fn preprocess_config(tokens: Vec<Token>, config: &ConfigEnv) -> DiagResult<Vec<Token>> {
    let Some(eof) = tokens.last().cloned() else {
        return Ok(tokens);
    };
    let mut preprocessor = ConfigPreprocessor {
        tokens: &tokens,
        config,
        diagnostics: Vec::new(),
    };
    let (mut out, pos) = preprocessor.process_range(0, &[TokenKind::Eof]);
    if !matches!(
        tokens.get(pos).map(|token| token.kind),
        Some(TokenKind::Eof)
    ) {
        preprocessor.diagnostics.push(Diagnostic::new(
            tokens.get(pos).map(|token| token.span),
            "unexpected configuration directive",
        ));
    }
    out.push(eof);
    if preprocessor.diagnostics.is_empty() {
        Ok(out)
    } else {
        Err(preprocessor.diagnostics)
    }
}

struct ConfigPreprocessor<'a> {
    tokens: &'a [Token],
    config: &'a ConfigEnv,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> ConfigPreprocessor<'a> {
    fn process_range(&mut self, mut pos: usize, stops: &[TokenKind]) -> (Vec<Token>, usize) {
        let mut out = Vec::new();
        let mut paren = 0usize;
        let mut brace = 0usize;
        let mut bracket = 0usize;
        while pos < self.tokens.len() {
            let kind = self.tokens[pos].kind;
            if paren == 0 && brace == 0 && bracket == 0 && stops.contains(&kind) {
                break;
            }
            match kind {
                TokenKind::HashIf if paren == 0 && brace == 0 && bracket == 0 => {
                    pos = self.process_if(pos, &mut out);
                    continue;
                }
                TokenKind::HashIf
                | TokenKind::HashElif
                | TokenKind::HashElse
                | TokenKind::HashEndif => {
                    self.diagnostics.push(Diagnostic::new(
                        self.tokens[pos].span,
                        "configuration gates are allowed only at item level",
                    ));
                }
                TokenKind::LParen => paren += 1,
                TokenKind::RParen => paren = paren.saturating_sub(1),
                TokenKind::LBrace => brace += 1,
                TokenKind::RBrace => brace = brace.saturating_sub(1),
                TokenKind::LBracket => bracket += 1,
                TokenKind::RBracket => bracket = bracket.saturating_sub(1),
                TokenKind::Eof => break,
                _ => {}
            }
            out.push(self.tokens[pos].clone());
            pos += 1;
        }
        (out, pos)
    }

    fn process_if(&mut self, mut pos: usize, out: &mut Vec<Token>) -> usize {
        let mut selected = false;
        let mut emitted = false;
        loop {
            let directive = self.tokens[pos].kind;
            let active = match directive {
                TokenKind::HashIf | TokenKind::HashElif => {
                    let (value, after_expr) = self.parse_config_expr(pos + 1);
                    pos = after_expr;
                    !selected && value
                }
                TokenKind::HashElse => {
                    pos += 1;
                    !selected
                }
                _ => unreachable!("process_if starts on a config directive"),
            };
            let end = self.find_branch_end(pos);
            if active && !emitted {
                let (branch, branch_end) = self.process_range(
                    pos,
                    &[
                        TokenKind::HashElif,
                        TokenKind::HashElse,
                        TokenKind::HashEndif,
                    ],
                );
                out.extend(branch);
                if branch_end != end {
                    pos = branch_end;
                } else {
                    pos = end;
                }
                selected = true;
                emitted = true;
            } else {
                pos = end;
            }

            match self.tokens.get(pos).map(|token| token.kind) {
                Some(TokenKind::HashElif) => continue,
                Some(TokenKind::HashElse) => continue,
                Some(TokenKind::HashEndif) => return pos + 1,
                Some(TokenKind::Eof) | None => {
                    self.diagnostics.push(Diagnostic::new(
                        self.tokens.get(pos).map(|token| token.span),
                        "unterminated configuration gate",
                    ));
                    return pos;
                }
                _ => return pos,
            }
        }
    }

    fn find_branch_end(&mut self, mut pos: usize) -> usize {
        let mut nested = 0usize;
        while pos < self.tokens.len() {
            let kind = self.tokens[pos].kind;
            match kind {
                TokenKind::HashIf => nested += 1,
                TokenKind::HashElif | TokenKind::HashElse | TokenKind::HashEndif => {
                    if nested == 0 {
                        return pos;
                    }
                    if kind == TokenKind::HashEndif {
                        nested -= 1;
                    }
                }
                TokenKind::Eof => return pos,
                _ => {}
            }
            pos += 1;
        }
        pos
    }

    fn parse_config_expr(&mut self, pos: usize) -> (bool, usize) {
        ConfigExprParser {
            tokens: self.tokens,
            pos,
            config: self.config,
            diagnostics: &mut self.diagnostics,
        }
        .parse()
    }
}

struct ConfigExprParser<'a, 'b> {
    tokens: &'a [Token],
    pos: usize,
    config: &'a ConfigEnv,
    diagnostics: &'b mut Vec<Diagnostic>,
}

impl<'a, 'b> ConfigExprParser<'a, 'b> {
    fn parse(mut self) -> (bool, usize) {
        let value = self.parse_or();
        (value, self.pos)
    }

    fn parse_or(&mut self) -> bool {
        let mut value = self.parse_and();
        while self.at(TokenKind::PipePipe) {
            self.pos += 1;
            value = self.parse_and() || value;
        }
        value
    }

    fn parse_and(&mut self) -> bool {
        let mut value = self.parse_unary();
        while self.at(TokenKind::AmpAmp) {
            self.pos += 1;
            value = self.parse_unary() && value;
        }
        value
    }

    fn parse_unary(&mut self) -> bool {
        if self.at(TokenKind::Bang) {
            self.pos += 1;
            !self.parse_unary()
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> bool {
        if self.at(TokenKind::LParen) {
            self.pos += 1;
            let value = self.parse_or();
            self.expect(
                TokenKind::RParen,
                "expected `)` in configuration expression",
            );
            return value;
        }
        let Some(name) = self.take_ident() else {
            self.diagnostics.push(Diagnostic::new(
                self.tokens.get(self.pos).map(|token| token.span),
                "expected restricted configuration function",
            ));
            return false;
        };
        self.expect(
            TokenKind::LParen,
            "expected `(` after configuration function",
        );
        let Some(value) = self.take_string() else {
            self.diagnostics.push(Diagnostic::new(
                self.tokens.get(self.pos).map(|token| token.span),
                "expected string argument in configuration expression",
            ));
            return false;
        };
        self.expect(
            TokenKind::RParen,
            "expected `)` after configuration argument",
        );
        match self.config.eval_call(&name, &decode_config_string(&value)) {
            Some(value) => value,
            None => {
                self.diagnostics.push(Diagnostic::new(
                    self.tokens
                        .get(self.pos.saturating_sub(1))
                        .map(|token| token.span),
                    format!("unknown configuration function `{name}`"),
                ));
                false
            }
        }
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.tokens
            .get(self.pos)
            .is_some_and(|token| token.kind == kind)
    }

    fn expect(&mut self, kind: TokenKind, message: &str) {
        if self.at(kind) {
            self.pos += 1;
        } else {
            self.diagnostics.push(Diagnostic::new(
                self.tokens.get(self.pos).map(|token| token.span),
                message,
            ));
        }
    }

    fn take_ident(&mut self) -> Option<String> {
        let token = self.tokens.get(self.pos)?;
        if token.kind != TokenKind::Ident {
            return None;
        }
        self.pos += 1;
        Some(token.lexeme.clone())
    }

    fn take_string(&mut self) -> Option<String> {
        let token = self.tokens.get(self.pos)?;
        if token.kind != TokenKind::String {
            return None;
        }
        self.pos += 1;
        Some(token.lexeme.clone())
    }
}

fn decode_config_string(raw: &str) -> String {
    raw.strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(raw)
        .to_string()
}
