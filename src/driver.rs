use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
};

use crate::{
    build::{
        BuildPlan, BuildProfile,
        package::{LoadedPackageManifest, PackageIndex, PackageLoadError},
        planner::build_plan_for_generated_c_with_packages,
    },
    codegen::generate_c,
    diagnostic::{DiagResult, Diagnostic, DiagnosticPhase, WithDiagnostics},
    escape::analyze_escapes,
    hir::lower_to_hir_lossy,
    lexer::{Token, TokenKind, lex_lossy},
    mono::monomorphize,
    parser::parse_file_lossy,
    resolve::{ModuleId, ParsedModule, resolve_modules_lossy},
    source::SourceMap,
    typeck::type_check_lossy,
};

const COMPILER_PRELUDE_IMPORTS: &[&str] =
    &["/std/result", "/std/error", "/std/panic", "/std/async"];

#[derive(Clone, Debug)]
pub struct CompileOptions {
    pub entry: PathBuf,
    pub project_manifest: Option<PathBuf>,
    pub std_paths: Vec<PathBuf>,
    pub package_roots: Vec<PathBuf>,
    pub target_os: String,
    pub target_arch: String,
    pub build_profile: BuildProfile,
    pub allow_native_build: bool,
    pub features: HashSet<String>,
}

impl CompileOptions {
    pub fn new(entry: impl Into<PathBuf>) -> Self {
        let entry = entry.into();
        let std_root = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            entry,
            project_manifest: None,
            std_paths: vec![std_root],
            package_roots: Vec::new(),
            target_os: env::consts::OS.to_string(),
            target_arch: env::consts::ARCH.to_string(),
            build_profile: BuildProfile::Debug,
            allow_native_build: false,
            features: HashSet::new(),
        }
    }

    pub fn with_project_manifest(mut self, manifest_path: impl Into<PathBuf>) -> Self {
        self.project_manifest = Some(manifest_path.into());
        self
    }

    pub fn with_std_path(mut self, std_path: impl Into<PathBuf>) -> Self {
        self.std_paths.push(std_path.into());
        self
    }

    pub fn with_package_root(mut self, package_root: impl Into<PathBuf>) -> Self {
        self.package_roots.push(package_root.into());
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

    pub fn with_build_profile(mut self, build_profile: BuildProfile) -> Self {
        self.build_profile = build_profile;
        self
    }

    pub fn with_allow_native_build(mut self, allow_native_build: bool) -> Self {
        self.allow_native_build = allow_native_build;
        self
    }

    pub fn with_feature(mut self, feature: impl Into<String>) -> Self {
        self.features.insert(feature.into());
        self
    }
}

pub fn compile_to_c(options: CompileOptions) -> DiagResult<String> {
    compile_to_c_with_sources(options)
        .map(|(generated_c, _source_map)| generated_c)
        .map_err(|(diagnostics, _source_map)| diagnostics)
}

pub fn compile_to_c_with_sources(
    options: CompileOptions,
) -> Result<(String, SourceMap), (Vec<Diagnostic>, SourceMap)> {
    compile_to_c_context(options).map(|output| (output.generated_c, output.source_map))
}

struct CompileOutput {
    generated_c: String,
    source_map: SourceMap,
    package_manifests: Vec<LoadedPackageManifest>,
}

fn compile_to_c_context(
    options: CompileOptions,
) -> Result<CompileOutput, (Vec<Diagnostic>, SourceMap)> {
    let config = ConfigEnv::from_options(&options);
    let mut loader = match ModuleLoader::new(
        options.project_manifest,
        options.std_paths,
        options.package_roots,
        config,
    ) {
        Ok(loader) => loader,
        Err(diagnostics) => return Err((diagnostics, SourceMap::default())),
    };
    let loaded = loader.load_entry_lossy(&options.entry);
    let mut diagnostics = loaded.diagnostics;

    let resolved = resolve_modules_lossy(loaded.value);
    diagnostics.extend(resolved.diagnostics);

    let hir = lower_to_hir_lossy(resolved.value);
    diagnostics.extend(hir.diagnostics);

    let checked = type_check_lossy(hir.value);
    diagnostics.extend(checked.diagnostics);

    // The default compile path keeps recovery alive through type checking so
    // users see as many actionable diagnostics as possible before we reject.
    if !diagnostics.is_empty() {
        return Err((diagnostics, loader.source_map));
    }

    let mono = match monomorphize(checked.value) {
        Ok(mono) => mono,
        Err(diags) => return Err((diags, loader.source_map)),
    };
    let result = {
        let escapes = analyze_escapes(&mono);
        generate_c(&mono, &escapes, &loader.source_map)
    };
    match result {
        Ok(generated_c) => Ok(CompileOutput {
            generated_c,
            source_map: loader.source_map,
            package_manifests: loader.loaded_package_manifests,
        }),
        Err(diags) => Err((diags, loader.source_map)),
    }
}

pub fn compile_to_build_plan(options: CompileOptions) -> DiagResult<BuildPlan> {
    compile_to_build_plan_with_sources(options)
        .map(|(plan, _source_map)| plan)
        .map_err(|(diagnostics, _source_map)| diagnostics)
}

pub fn compile_to_build_plan_with_sources(
    options: CompileOptions,
) -> Result<(BuildPlan, SourceMap), (Vec<Diagnostic>, SourceMap)> {
    let profile = options.build_profile;
    let target_os = options.target_os.clone();
    let allow_native_build = options.allow_native_build;
    compile_to_c_context(options).map(|output| {
        let package_inputs = output
            .source_map
            .files()
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let source_map = output.source_map;
        (
            build_plan_for_generated_c_with_packages(
                output.generated_c,
                profile,
                allow_native_build,
                &target_os,
                package_inputs,
                &output.package_manifests,
            ),
            source_map,
        )
    })
}

struct ModuleLoader {
    cwd: PathBuf,
    std_paths: Vec<PathBuf>,
    config: ConfigEnv,
    std_package_index: PackageIndex,
    user_package_index: PackageIndex,
    source_map: SourceMap,
    loaded: HashMap<PathBuf, ModuleId>,
    loading: HashSet<PathBuf>,
    modules: Vec<ParsedModule>,
    loaded_package_keys: HashSet<PathBuf>,
    loaded_package_manifests: Vec<LoadedPackageManifest>,
}

impl ModuleLoader {
    fn new(
        project_manifest: Option<PathBuf>,
        std_paths: Vec<PathBuf>,
        package_roots: Vec<PathBuf>,
        config: ConfigEnv,
    ) -> Result<Self, Vec<Diagnostic>> {
        let std_package_index =
            PackageIndex::load_std(&std_paths).map_err(package_load_errors_to_diagnostics)?;
        let user_package_index = PackageIndex::load_project_manifest_and_package_roots(
            project_manifest.as_deref(),
            &package_roots,
        )
        .map_err(package_load_errors_to_diagnostics)?;
        Ok(Self {
            cwd: env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            std_paths,
            config,
            std_package_index,
            user_package_index,
            source_map: SourceMap::default(),
            loaded: HashMap::new(),
            loading: HashSet::new(),
            modules: Vec::new(),
            loaded_package_keys: HashSet::new(),
            loaded_package_manifests: Vec::new(),
        })
    }

    fn load_entry_lossy(&mut self, entry: &Path) -> WithDiagnostics<Vec<ParsedModule>> {
        let mut diagnostics = Vec::new();
        for import in COMPILER_PRELUDE_IMPORTS {
            let import_path = self.resolve_import_path(Path::new("."), import);
            self.load_file_lossy(&import_path, &mut diagnostics);
        }
        let path = self.normalize_path(entry);
        self.load_file_lossy(&path, &mut diagnostics);
        WithDiagnostics {
            value: std::mem::take(&mut self.modules),
            diagnostics,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn load_file_lossy(
        &mut self,
        path: &Path,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<ModuleId> {
        let path = self.normalize_path(path);
        if let Some(id) = self.loaded.get(&path) {
            return Some(*id);
        }
        if !self.loading.insert(path.clone()) {
            diagnostics.push(
                Diagnostic::new(None, format!("import cycle involving `{}`", path.display()))
                    .with_phase(DiagnosticPhase::Resolve),
            );
            return None;
        }

        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                diagnostics.push(
                    Diagnostic::new(
                        None,
                        format!("failed to read `{}`: {error}", path.display()),
                    )
                    .with_phase(DiagnosticPhase::Resolve),
                );
                self.loading.remove(&path);
                return None;
            }
        };
        let file_id = self.source_map.add(path.clone(), text.clone());
        let lexed = lex_lossy(file_id, &text);
        diagnostics.extend(lexed.diagnostics);
        let preprocessed = preprocess_config_lossy(lexed.value, &self.config);
        diagnostics.extend(preprocessed.diagnostics);
        let parsed = parse_file_lossy(preprocessed.value);
        diagnostics.extend(parsed.diagnostics);
        let ast = parsed.value;
        let id = ModuleId(self.modules.len());
        self.loaded.insert(path.clone(), id);
        self.record_package_for_source(&path);
        let std_export = self
            .std_package_index
            .export_for_source(&path)
            .map(str::to_string);
        let parent = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let mut import_paths = Vec::new();
        for item in &ast.items {
            if let crate::ast::ItemKind::Import(import) = &item.kind {
                import_paths.push(
                    self.normalize_path(&self.resolve_import_path(&parent, &import.path.raw)),
                );
            }
        }

        self.modules.push(ParsedModule {
            id,
            path: path.clone(),
            std_export,
            import_paths: import_paths.clone(),
            ast: ast.clone(),
        });

        for import_path in import_paths {
            self.load_file_lossy(&import_path, diagnostics);
        }

        self.loading.remove(&path);
        Some(id)
    }

    fn resolve_import_path(&self, parent: &Path, raw: &str) -> PathBuf {
        let mut path = if let Some(rest) = raw.strip_prefix('/') {
            if let Some(source) = self.std_package_index.resolve_export(raw) {
                return source.to_path_buf();
            }
            if let Some(source) = self.user_package_index.resolve_export(raw) {
                return source.to_path_buf();
            }
            let mut candidates = self.std_paths.iter().map(|root| root.join(rest));
            let first = candidates.next().unwrap_or_else(|| PathBuf::from(rest));
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

    fn record_package_for_source(&mut self, path: &Path) {
        let manifest = self
            .std_package_index
            .manifest_for_source(path)
            .or_else(|| self.user_package_index.manifest_for_source(path));
        let Some(manifest) = manifest else {
            return;
        };
        let key = manifest.manifest.manifest_path.clone().unwrap_or_else(|| {
            manifest
                .manifest
                .package
                .root
                .join(&manifest.manifest.package.name)
        });
        if self.loaded_package_keys.insert(key) {
            self.loaded_package_manifests.push(manifest);
        }
    }

    fn normalize_path(&self, path: &Path) -> PathBuf {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        };
        path.components().collect()
    }
}

fn package_load_errors_to_diagnostics(errors: Vec<PackageLoadError>) -> Vec<Diagnostic> {
    errors
        .into_iter()
        .map(|error| {
            Diagnostic::new(
                None,
                format!(
                    "failed to load package manifest `{}`: {}",
                    error.path.display(),
                    error.message
                ),
            )
        })
        .collect()
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

fn preprocess_config_lossy(tokens: Vec<Token>, config: &ConfigEnv) -> WithDiagnostics<Vec<Token>> {
    let Some(eof) = tokens.last().cloned() else {
        return WithDiagnostics {
            value: tokens,
            diagnostics: Vec::new(),
        };
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
    for diagnostic in &mut preprocessor.diagnostics {
        if diagnostic.phase.is_none() {
            diagnostic.phase = Some(DiagnosticPhase::Parse);
        }
    }
    WithDiagnostics {
        value: out,
        diagnostics: preprocessor.diagnostics,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ciel-driver-recovery-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lossy_module_loading_keeps_valid_imports_after_malformed_import() {
        let dir = temp_dir("imports");
        let good = dir.join("good.ciel");
        let main = dir.join("main.ciel");
        fs::write(&good, "void good() {}\n").unwrap();
        fs::write(
            &main,
            "import ./broken as ;\nimport ./good;\nvoid main() {}\n",
        )
        .unwrap();

        let config = ConfigEnv {
            target_os: std::env::consts::OS.to_string(),
            target_arch: std::env::consts::ARCH.to_string(),
            features: HashSet::new(),
        };
        let mut loader = ModuleLoader::new(
            None,
            vec![PathBuf::from(env!("CARGO_MANIFEST_DIR"))],
            Vec::new(),
            config,
        )
        .unwrap();
        let mut diagnostics = Vec::new();
        loader.load_file_lossy(&main, &mut diagnostics);
        let normalized_good = loader.normalize_path(&good);

        assert!(!diagnostics.is_empty());
        assert!(
            loader
                .modules
                .iter()
                .any(|module| module.path == normalized_good)
        );
        let entry = loader
            .modules
            .iter()
            .find(|module| module.path == loader.normalize_path(&main))
            .unwrap();
        assert_eq!(entry.import_paths, vec![normalized_good]);
    }
}
