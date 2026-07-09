use std::{
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{self, Command},
};

use cielc::{
    BuildPlan, BuildProfile, CompileOptions,
    build::{
        default_c_compiler,
        manifest::{PackageKind, PackageManifest},
        native::{CmakeOutput, CmakeOutputKind, build_cmake_output, cmake_include_flags},
    },
    diagnostic::render_diagnostics,
    driver::compile_to_build_plan_with_sources,
    formatter::{FormatOptions, format_source},
};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use serde::Deserialize;

#[derive(Clone, Debug, Parser)]
#[command(
    name = "cielc",
    about = "Compile and format Ciel source code",
    args_conflicts_with_subcommands = true,
    disable_help_subcommand = true
)]
struct CielCli {
    #[command(subcommand)]
    command: Option<CielCommand>,
    #[command(flatten)]
    compile: CliOptions,
}

#[derive(Clone, Debug, Subcommand)]
enum CielCommand {
    Fmt(FmtOptions),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum EmitMode {
    #[value(name = "c")]
    C,
    #[value(alias = "exe", alias = "bin")]
    Executable,
    #[value(alias = "obj")]
    Object,
    #[value(
        name = "shared-library",
        alias = "shared",
        alias = "dylib",
        alias = "so"
    )]
    SharedLibrary,
}

fn main() {
    if env::args_os().len() == 1 {
        let mut command = CielCli::command();
        command.print_help().expect("failed to print help");
        eprintln!();
        return;
    }

    let mut cli = CielCli::parse();
    match cli.command {
        Some(CielCommand::Fmt(mut fmt)) => {
            normalize_fmt_cli(&mut fmt);
            run_fmt_command(fmt);
        }
        None => {
            cli.compile.normalize();
            run_compile_command(cli.compile);
        }
    }
}

fn run_compile_command(cli: CliOptions) {
    let selection = resolve_cli_project(&cli).unwrap_or_else(|message| {
        eprintln!("{message}");
        process::exit(2);
    });

    let mut options = CompileOptions::new(&selection.input);
    if let Some(manifest_path) = &selection.project_manifest {
        options = options.with_project_manifest(manifest_path.clone());
    }
    for std_path in &cli.std_paths {
        options = options.with_std_path(std_path.clone());
    }
    for package_root in &cli.package_roots {
        options = options.with_package_root(package_root.clone());
    }
    if let Some(target_os) = &cli.target_os {
        options = options.with_target_os(target_os.clone());
    }
    if let Some(target_arch) = &cli.target_arch {
        options = options.with_target_arch(target_arch.clone());
    }
    options = options.with_build_profile(cli.profile());
    options = options.with_allow_native_build(cli.allow_native_build);
    for feature in &cli.features {
        options = options.with_feature(feature.clone());
    }

    let target_os = options.target_os.clone();
    match compile_to_build_plan_with_sources(options) {
        Ok((plan, _source_map)) => {
            if cli.emit == EmitMode::C {
                emit_c_output(&plan.generated_c, cli.output.as_deref());
                return;
            }
            compile_generated_c(&selection.input, &plan, &target_os, &cli);
        }
        Err((diagnostics, source_map)) => {
            eprint!("{}", render_diagnostics(&source_map, &diagnostics));
            process::exit(1);
        }
    }
}

#[derive(Clone, Debug, Args)]
struct CliOptions {
    #[arg(value_name = "input.ciel")]
    input: Option<PathBuf>,
    #[arg(long, value_name = "name")]
    entry: Option<String>,
    #[arg(long = "manifest-path", value_name = "path/to/ciel.toml")]
    manifest_path: Option<PathBuf>,
    #[arg(short, long, value_name = "output")]
    output: Option<PathBuf>,
    #[arg(long = "save-c", value_name = "path")]
    save_c: Option<PathBuf>,
    #[arg(long = "std-path", value_name = "root")]
    std_paths: Vec<PathBuf>,
    #[arg(long = "package-root", value_name = "root")]
    package_roots: Vec<PathBuf>,
    #[arg(long = "target-os", value_name = "os")]
    target_os: Option<String>,
    #[arg(long = "target-arch", value_name = "arch")]
    target_arch: Option<String>,
    #[arg(long = "feature", value_name = "name")]
    features: Vec<String>,
    #[arg(long = "allow-native-build")]
    allow_native_build: bool,
    #[arg(long = "emit-c", conflicts_with = "emit")]
    emit_c: bool,
    #[arg(
        long,
        value_enum,
        default_value = "executable",
        ignore_case = true,
        value_name = "MODE"
    )]
    emit: EmitMode,
    #[arg(long, conflicts_with = "release")]
    debug: bool,
    #[arg(long, conflicts_with = "debug")]
    release: bool,
    #[arg(long = "cc", value_name = "cc", default_value_t = default_c_compiler())]
    c_compiler: String,
    #[arg(long = "cflag", value_name = "flag", allow_hyphen_values = true)]
    c_flags: Vec<String>,
    #[arg(long = "ldflag", value_name = "flag", allow_hyphen_values = true)]
    link_flags: Vec<String>,
}

impl CliOptions {
    fn normalize(&mut self) {
        if self.emit_c {
            self.emit = EmitMode::C;
        }
    }

    fn profile(&self) -> BuildProfile {
        if self.debug {
            BuildProfile::Debug
        } else if self.release {
            BuildProfile::Release
        } else {
            BuildProfile::Debug
        }
    }
}

#[derive(Clone, Debug, Parser)]
#[command(name = "cielc fmt", about = "Format Ciel source files")]
struct FmtOptions {
    #[arg(value_name = "input.ciel|-")]
    input: Option<PathBuf>,
    #[arg(long, conflicts_with = "write")]
    check: bool,
    #[arg(short, long)]
    write: bool,
    #[arg(
        long = "config",
        value_name = ".ciel-format",
        conflicts_with = "no_config"
    )]
    config_path: Option<PathBuf>,
    #[arg(long)]
    no_config: bool,
    #[arg(long = "line-width", visible_alias = "width", value_name = "n")]
    line_width: Option<usize>,
    #[arg(long = "indent-width", value_name = "n")]
    indent_width: Option<usize>,
    #[arg(long = "chain-call-break-threshold", value_name = "n")]
    chain_call_break_threshold: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FmtConfig {
    #[serde(alias = "line-width")]
    line_width: Option<usize>,
    #[serde(alias = "indent-width")]
    indent_width: Option<usize>,
    #[serde(alias = "chain-call-break-threshold")]
    chain_call_break_threshold: Option<usize>,
}

fn run_fmt_command(cli: FmtOptions) {
    let format_options = resolve_fmt_options(&cli).unwrap_or_else(|message| {
        eprintln!("{message}");
        process::exit(2);
    });

    let source = match read_fmt_source(cli.input.as_deref()) {
        Ok(source) => source,
        Err(message) => {
            eprintln!("{message}");
            process::exit(1);
        }
    };
    let formatted = match format_source(&source, format_options) {
        Ok(formatted) => formatted,
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    };

    if cli.check {
        if formatted != source {
            if let Some(input) = &cli.input {
                eprintln!("{} is not formatted", input.display());
            } else {
                eprintln!("stdin is not formatted");
            }
            process::exit(1);
        }
        return;
    }

    if cli.write {
        let Some(input) = &cli.input else {
            eprintln!("--write requires an input file");
            process::exit(2);
        };
        if formatted != source {
            if let Err(error) = fs::write(input, formatted) {
                eprintln!("failed to write `{}`: {error}", input.display());
                process::exit(1);
            }
        }
        return;
    }

    print!("{formatted}");
}

fn normalize_fmt_cli(options: &mut FmtOptions) {
    if options.input.as_deref() == Some(Path::new("-")) {
        options.input = None;
    }
}

fn resolve_fmt_options(cli: &FmtOptions) -> Result<FormatOptions, String> {
    let mut options = FormatOptions::default();

    if !cli.no_config {
        if let Some(config_path) = resolve_fmt_config_path(cli)? {
            let config = load_fmt_config(&config_path)?;
            apply_fmt_config(&mut options, config);
        }
    }

    if let Some(line_width) = cli.line_width {
        options.line_width = line_width;
    }
    if let Some(indent_width) = cli.indent_width {
        options.indent_width = indent_width;
    }
    if let Some(threshold) = cli.chain_call_break_threshold {
        options.chain_call_break_threshold = threshold;
    }
    validate_fmt_options(options)?;
    Ok(options)
}

fn resolve_fmt_config_path(cli: &FmtOptions) -> Result<Option<PathBuf>, String> {
    if let Some(path) = &cli.config_path {
        return Ok(Some(normalize_cli_path(path)));
    }

    let start = cli
        .input
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    Ok(find_fmt_config_upwards(&start))
}

fn find_fmt_config_upwards(start: &Path) -> Option<PathBuf> {
    let mut dir = normalize_cli_path(start);
    loop {
        let config = dir.join(".ciel-format");
        if config.exists() {
            return Some(config);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn load_fmt_config(path: &Path) -> Result<FmtConfig, String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("failed to read format config `{}`: {error}", path.display()))?;
    toml::from_str(&source).map_err(|error| {
        format!(
            "failed to parse format config `{}`: {error}",
            path.display()
        )
    })
}

fn apply_fmt_config(options: &mut FormatOptions, config: FmtConfig) {
    if let Some(line_width) = config.line_width {
        options.line_width = line_width;
    }
    if let Some(indent_width) = config.indent_width {
        options.indent_width = indent_width;
    }
    if let Some(threshold) = config.chain_call_break_threshold {
        options.chain_call_break_threshold = threshold;
    }
}

fn validate_fmt_options(options: FormatOptions) -> Result<(), String> {
    if options.line_width == 0 {
        return Err("fmt line_width must be greater than 0".to_string());
    }
    if options.indent_width == 0 {
        return Err("fmt indent_width must be greater than 0".to_string());
    }
    if options.chain_call_break_threshold == 0 {
        return Err("fmt chain_call_break_threshold must be greater than 0".to_string());
    }
    Ok(())
}

fn read_fmt_source(input: Option<&Path>) -> Result<String, String> {
    if let Some(input) = input {
        return fs::read_to_string(input)
            .map_err(|error| format!("failed to read `{}`: {error}", input.display()));
    }

    let mut source = String::new();
    io::stdin()
        .read_to_string(&mut source)
        .map_err(|error| format!("failed to read stdin: {error}"))?;
    Ok(source)
}

struct ProjectSelection {
    input: PathBuf,
    project_manifest: Option<PathBuf>,
}

fn resolve_cli_project(cli: &CliOptions) -> Result<ProjectSelection, String> {
    if cli.input.is_some() && cli.entry.is_some() {
        return Err("cannot combine an input file with --entry".to_string());
    }

    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let manifest_path = resolve_manifest_path(cli, &cwd);
    let Some(manifest_path) = manifest_path else {
        if let Some(input) = &cli.input {
            return Ok(ProjectSelection {
                input: input.clone(),
                project_manifest: None,
            });
        }
        return Err(
            "missing input file and no ciel.toml found; pass input.ciel or --manifest-path <ciel.toml>"
                .to_string(),
        );
    };

    let manifest = PackageManifest::load(&manifest_path).map_err(|error| {
        format!(
            "failed to load project manifest `{}`: {error}",
            manifest_path.display()
        )
    })?;
    if manifest.package.kind != PackageKind::Project {
        return Err(format!(
            "project manifest `{}` has kind {:?}; expected project",
            manifest_path.display(),
            manifest.package.kind
        ));
    }
    let input = if let Some(input) = &cli.input {
        input.clone()
    } else {
        resolve_project_entry(&manifest, &manifest_path, cli.entry.as_deref())?
    };

    Ok(ProjectSelection {
        input,
        project_manifest: Some(manifest_path),
    })
}

fn resolve_manifest_path(cli: &CliOptions, cwd: &Path) -> Option<PathBuf> {
    if let Some(path) = &cli.manifest_path {
        return Some(normalize_cli_path(path));
    }
    if cli.input.is_none() {
        return find_manifest_upwards(cwd);
    }
    None
}

fn find_manifest_upwards(start: &Path) -> Option<PathBuf> {
    let mut dir = normalize_cli_path(start);
    loop {
        let manifest = dir.join("ciel.toml");
        if manifest.exists() {
            return Some(manifest);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn normalize_cli_path(path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    path.components().collect()
}

fn resolve_project_entry(
    manifest: &PackageManifest,
    manifest_path: &Path,
    entry: Option<&str>,
) -> Result<PathBuf, String> {
    let project = manifest.project.as_ref().ok_or_else(|| {
        format!(
            "project manifest `{}` has no project section",
            manifest_path.display()
        )
    })?;
    let entry_name = match entry {
        Some(entry) => entry,
        None => match project.default.as_deref() {
            Some(default) => default,
            None if project.entries.len() == 1 => project.entries.keys().next().unwrap(),
            None => {
                return Err(format!(
                    "project manifest `{}` has multiple entries; pass --entry <name>",
                    manifest_path.display()
                ));
            }
        },
    };
    project.entries.get(entry_name).cloned().ok_or_else(|| {
        let mut names = project.entries.keys().cloned().collect::<Vec<_>>();
        names.sort();
        format!(
            "project manifest `{}` has no entry `{}`; available entries: {}",
            manifest_path.display(),
            entry_name,
            names.join(", ")
        )
    })
}

fn emit_c_output(c: &str, output: Option<&Path>) {
    if let Some(output) = output {
        if let Err(error) = fs::write(output, c) {
            eprintln!("failed to write `{}`: {error}", output.display());
            process::exit(1);
        }
    } else {
        print!("{c}");
    }
}

fn compile_generated_c(input: &Path, plan: &BuildPlan, target_os: &str, cli: &CliOptions) {
    let c_path = cli
        .save_c
        .clone()
        .unwrap_or_else(|| temp_c_path(input, cli.emit));
    if let Err(error) = fs::write(&c_path, &plan.generated_c) {
        eprintln!("failed to write `{}`: {error}", c_path.display());
        process::exit(1);
    }

    let output = cli
        .output
        .clone()
        .unwrap_or_else(|| default_output_path(input, cli.emit, target_os));
    let result = invoke_c_compiler(&c_path, &output, target_os, cli, plan);
    if cli.save_c.is_none() {
        let _ = fs::remove_file(&c_path);
    }
    if let Err(message) = result {
        eprintln!("{message}");
        process::exit(1);
    }
}

fn invoke_c_compiler(
    c_path: &Path,
    output: &Path,
    target_os: &str,
    cli: &CliOptions,
    plan: &BuildPlan,
) -> Result<(), String> {
    if matches!(cli.emit, EmitMode::Executable | EmitMode::SharedLibrary) {
        let kind = match cli.emit {
            EmitMode::Executable => CmakeOutputKind::Executable,
            EmitMode::SharedLibrary => CmakeOutputKind::SharedLibrary,
            EmitMode::C | EmitMode::Object => unreachable!("handled outside CMake"),
        };
        return build_cmake_output(
            plan,
            &CmakeOutput {
                source_path: c_path,
                output_path: output,
                kind,
                c_compiler: &cli.c_compiler,
                compile_flags: cli.c_flags.clone(),
                link_flags: cli.link_flags.clone(),
                target_os,
            },
        );
    }

    let mut args = Vec::<String>::new();
    args.extend(profile_c_flags(plan.profile, target_os));
    args.extend(cmake_include_flags(&plan.cmake_targets));
    args.extend(cli.c_flags.clone());

    match cli.emit {
        EmitMode::Object => {
            args.push("-c".to_string());
            args.push(c_path.display().to_string());
            args.push("-o".to_string());
            args.push(output.display().to_string());
        }
        EmitMode::Executable | EmitMode::SharedLibrary => unreachable!("handled by CMake"),
        EmitMode::C => unreachable!("C output is handled without invoking cc"),
    }

    let output_result = Command::new(&cli.c_compiler)
        .args(&args)
        .output()
        .map_err(|error| format!("failed to invoke `{}`: {error}", cli.c_compiler))?;
    if output_result.status.success() {
        return Ok(());
    }
    Err(format!(
        "C compiler failed\ncommand: {} {}\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        cli.c_compiler,
        args.join(" "),
        output_result.status,
        String::from_utf8_lossy(&output_result.stdout),
        String::from_utf8_lossy(&output_result.stderr)
    ))
}

fn profile_c_flags(profile: BuildProfile, target_os: &str) -> Vec<String> {
    match profile {
        BuildProfile::Debug => vec![
            "-g".to_string(),
            "-O0".to_string(),
            "-DCIEL_DEBUG=1".to_string(),
        ],
        BuildProfile::Release => {
            let mut flags = vec![
                "-O3".to_string(),
                "-DNDEBUG".to_string(),
                "-DCIEL_RELEASE=1".to_string(),
            ];
            if is_linux_target(target_os) {
                flags.push("-ffunction-sections".to_string());
                flags.push("-fdata-sections".to_string());
            }
            flags
        }
    }
}

fn is_linux_target(target_os: &str) -> bool {
    target_os == "linux"
}

fn temp_c_path(input: &Path, mode: EmitMode) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("input")
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    env::temp_dir().join(format!(
        "cielc-{}-{stem}-{}.c",
        process::id(),
        match mode {
            EmitMode::Executable => "exe",
            EmitMode::Object => "obj",
            EmitMode::SharedLibrary => "shared",
            EmitMode::C => "emit",
        }
    ))
}

fn default_output_path(input: &Path, mode: EmitMode, target_os: &str) -> PathBuf {
    match mode {
        EmitMode::Executable => input.with_extension(""),
        EmitMode::Object => input.with_extension("o"),
        EmitMode::SharedLibrary => {
            input.with_extension(if target_os == "macos" { "dylib" } else { "so" })
        }
        EmitMode::C => input.with_extension("c"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_option_overrides_default_c_compiler() {
        let cli =
            CielCli::try_parse_from(["cielc", "--cc", "toolchain-clang", "main.ciel"]).unwrap();
        assert_eq!(cli.compile.c_compiler, "toolchain-clang");
    }

    #[test]
    fn cli_emit_aliases_match_legacy_parser() {
        for (value, expected) in [
            ("C", EmitMode::C),
            ("exe", EmitMode::Executable),
            ("bin", EmitMode::Executable),
            ("obj", EmitMode::Object),
            ("shared", EmitMode::SharedLibrary),
            ("dylib", EmitMode::SharedLibrary),
            ("so", EmitMode::SharedLibrary),
        ] {
            let cli = CielCli::try_parse_from(["cielc", "--emit", value, "main.ciel"]).unwrap();
            assert_eq!(cli.compile.emit, expected);
        }
    }

    #[test]
    fn cli_emit_c_sets_emit_mode() {
        let mut cli = CielCli::try_parse_from(["cielc", "--emit-c", "main.ciel"]).unwrap();
        cli.compile.normalize();
        assert_eq!(cli.compile.emit, EmitMode::C);
    }

    #[test]
    fn fmt_options_default_to_eighty_columns() {
        let cli = CielCli::try_parse_from(["cielc", "fmt"]).unwrap();
        let Some(CielCommand::Fmt(fmt)) = cli.command else {
            panic!("expected fmt command");
        };
        let options = resolve_fmt_options(&fmt).unwrap();
        assert_eq!(options.line_width, 80);
        assert_eq!(options.indent_width, 4);
        assert_eq!(options.chain_call_break_threshold, 3);
    }

    #[test]
    fn fmt_config_is_loaded_and_cli_overrides_it() {
        let dir = env::temp_dir().join(format!("ciel-format-config-test-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(".ciel-format"),
            "line_width = 120\nindent_width = 2\nchain_call_break_threshold = 4\n",
        )
        .unwrap();

        let input = dir.join("main.ciel");
        let cli = CielCli::try_parse_from([
            "cielc",
            "fmt",
            "--line-width",
            "90",
            &input.display().to_string(),
        ])
        .unwrap();
        let Some(CielCommand::Fmt(fmt)) = cli.command else {
            panic!("expected fmt command");
        };
        let options = resolve_fmt_options(&fmt).unwrap();
        assert_eq!(options.line_width, 90);
        assert_eq!(options.indent_width, 2);
        assert_eq!(options.chain_call_break_threshold, 4);

        fs::remove_dir_all(&dir).unwrap();
    }
}
