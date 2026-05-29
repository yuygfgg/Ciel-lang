use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{self, Command},
};

use cielc::{CompileOptions, diagnostic::render_diagnostics, driver::compile_to_c_with_sources};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmitMode {
    C,
    Executable,
    Object,
    SharedLibrary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BuildProfile {
    Debug,
    Release,
}

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_usage();
        return;
    }

    let cli = parse_args(&args).unwrap_or_else(|message| {
        eprintln!("{message}");
        process::exit(2);
    });

    let Some(input) = cli.input.clone() else {
        eprintln!("missing input file");
        process::exit(2);
    };

    let mut options = CompileOptions::new(&input);
    if let Some(project_root) = &cli.project_root {
        options = options.with_project_root(project_root.clone());
    }
    for std_path in &cli.std_paths {
        options = options.with_std_path(std_path.clone());
    }
    if let Some(target_os) = &cli.target_os {
        options = options.with_target_os(target_os.clone());
    }
    if let Some(target_arch) = &cli.target_arch {
        options = options.with_target_arch(target_arch.clone());
    }
    for feature in &cli.features {
        options = options.with_feature(feature.clone());
    }

    let target_os = options.target_os.clone();
    match compile_to_c_with_sources(options) {
        Ok((c, _source_map)) => {
            if cli.emit == EmitMode::C {
                emit_c_output(&c, cli.output.as_deref());
                return;
            }
            compile_generated_c(&input, &c, &target_os, &cli);
        }
        Err((diagnostics, source_map)) => {
            eprint!("{}", render_diagnostics(&source_map, &diagnostics));
            process::exit(1);
        }
    }
}

#[derive(Clone, Debug)]
struct CliOptions {
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    save_c: Option<PathBuf>,
    project_root: Option<PathBuf>,
    std_paths: Vec<PathBuf>,
    target_os: Option<String>,
    target_arch: Option<String>,
    features: Vec<String>,
    emit: EmitMode,
    profile: BuildProfile,
    c_compiler: String,
    c_flags: Vec<String>,
    link_flags: Vec<String>,
}

fn parse_args(args: &[String]) -> Result<CliOptions, String> {
    let mut cli = CliOptions {
        input: None,
        output: None,
        save_c: None,
        project_root: None,
        std_paths: Vec::new(),
        target_os: None,
        target_arch: None,
        features: Vec::new(),
        emit: EmitMode::Executable,
        profile: BuildProfile::Debug,
        c_compiler: env::var("CC").unwrap_or_else(|_| "cc".to_string()),
        c_flags: Vec::new(),
        link_flags: Vec::new(),
    };

    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "-o" | "--output" => cli.output = Some(PathBuf::from(take_value(args, &mut idx)?)),
            "--emit-c" => cli.emit = EmitMode::C,
            "--emit" => cli.emit = parse_emit_mode(&take_value(args, &mut idx)?)?,
            "--save-c" => cli.save_c = Some(PathBuf::from(take_value(args, &mut idx)?)),
            "--cc" => cli.c_compiler = take_value(args, &mut idx)?,
            "--cflag" => cli.c_flags.push(take_value(args, &mut idx)?),
            "--ldflag" => cli.link_flags.push(take_value(args, &mut idx)?),
            "--debug" => cli.profile = BuildProfile::Debug,
            "--release" => cli.profile = BuildProfile::Release,
            "--project-root" => cli.project_root = Some(PathBuf::from(take_value(args, &mut idx)?)),
            "--std-path" => cli
                .std_paths
                .push(PathBuf::from(take_value(args, &mut idx)?)),
            "--target-os" => cli.target_os = Some(take_value(args, &mut idx)?),
            "--target-arch" => cli.target_arch = Some(take_value(args, &mut idx)?),
            "--feature" => cli.features.push(take_value(args, &mut idx)?),
            arg if arg.starts_with("--emit=") => {
                cli.emit = parse_emit_mode(arg.trim_start_matches("--emit="))?
            }
            arg if arg.starts_with('-') => return Err(format!("unknown option `{arg}`")),
            path => {
                if cli.input.replace(PathBuf::from(path)).is_some() {
                    return Err("multiple input files were provided".to_string());
                }
            }
        }
        idx += 1;
    }
    Ok(cli)
}

fn take_value(args: &[String], idx: &mut usize) -> Result<String, String> {
    let option = args[*idx].clone();
    *idx += 1;
    args.get(*idx)
        .cloned()
        .ok_or_else(|| format!("missing value after {option}"))
}

fn parse_emit_mode(value: &str) -> Result<EmitMode, String> {
    match value {
        "c" | "C" => Ok(EmitMode::C),
        "exe" | "executable" | "bin" => Ok(EmitMode::Executable),
        "obj" | "object" => Ok(EmitMode::Object),
        "shared" | "shared-library" | "dylib" | "so" => Ok(EmitMode::SharedLibrary),
        _ => Err(format!(
            "unknown emit mode `{value}`; expected c, exe, obj, or shared"
        )),
    }
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

fn compile_generated_c(input: &Path, c: &str, target_os: &str, cli: &CliOptions) {
    let c_path = cli
        .save_c
        .clone()
        .unwrap_or_else(|| temp_c_path(input, cli.emit));
    if let Err(error) = fs::write(&c_path, c) {
        eprintln!("failed to write `{}`: {error}", c_path.display());
        process::exit(1);
    }

    let output = cli
        .output
        .clone()
        .unwrap_or_else(|| default_output_path(input, cli.emit, target_os));
    let result = invoke_c_compiler(&c_path, &output, target_os, cli);
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
) -> Result<(), String> {
    let (pkg_compile_flags, pkg_link_flags) = bdwgc_cc_args();
    let mut args = Vec::<String>::new();
    args.extend(profile_c_flags(cli.profile, target_os));
    args.extend(pkg_compile_flags);
    args.extend(cli.c_flags.clone());

    match cli.emit {
        EmitMode::Object => {
            args.push("-c".to_string());
            args.push(c_path.display().to_string());
            args.push("-o".to_string());
            args.push(output.display().to_string());
        }
        EmitMode::SharedLibrary => {
            args.push("-fPIC".to_string());
            args.push(c_path.display().to_string());
            args.push(shared_library_flag(target_os).to_string());
            args.push("-o".to_string());
            args.push(output.display().to_string());
            args.extend(profile_link_flags(cli.profile, target_os));
            args.extend(pkg_link_flags);
            args.extend(cli.link_flags.clone());
        }
        EmitMode::Executable => {
            args.push(c_path.display().to_string());
            args.push("-o".to_string());
            args.push(output.display().to_string());
            args.extend(profile_link_flags(cli.profile, target_os));
            args.extend(pkg_link_flags);
            args.extend(cli.link_flags.clone());
        }
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

fn profile_link_flags(profile: BuildProfile, target_os: &str) -> Vec<String> {
    match profile {
        BuildProfile::Debug => Vec::new(),
        BuildProfile::Release if is_macos_target(target_os) => vec!["-Wl,-dead_strip".to_string()],
        BuildProfile::Release if is_linux_target(target_os) => {
            vec!["-Wl,--gc-sections".to_string()]
        }
        BuildProfile::Release => Vec::new(),
    }
}

fn bdwgc_cc_args() -> (Vec<String>, Vec<String>) {
    let mut args = Vec::new();
    args.extend(pkg_config_args("bdw-gc", &["-lgc"]));
    args.extend(pkg_config_args("botan-3", &["-lbotan-3"]));
    if !cfg!(windows) && !args.iter().any(|arg| arg == "-pthread") {
        args.push("-pthread".to_string());
    }
    if !cfg!(windows) {
        args.push("-fblocks".to_string());
        if !cfg!(target_os = "macos") {
            args.push("-ldispatch".to_string());
            args.push("-lBlocksRuntime".to_string());
        }
    }
    split_c_and_link_args(args)
}

fn pkg_config_args(package: &str, fallback: &[&str]) -> Vec<String> {
    let output = Command::new("pkg-config")
        .arg("--cflags")
        .arg("--libs")
        .arg(package)
        .output();
    match output {
        Ok(output) if output.status.success() => String::from_utf8(output.stdout)
            .unwrap_or_default()
            .split_whitespace()
            .filter(|arg| !arg.starts_with("-stdlib="))
            .map(str::to_string)
            .collect::<Vec<_>>(),
        _ => fallback.iter().map(|arg| (*arg).to_string()).collect(),
    }
}

fn split_c_and_link_args(args: Vec<String>) -> (Vec<String>, Vec<String>) {
    let mut compile = Vec::new();
    let mut link = Vec::new();
    for arg in args {
        if arg == "-pthread" || arg.starts_with("-stdlib=") {
            compile.push(arg.clone());
            link.push(arg);
        } else if is_link_arg(&arg) {
            link.push(arg);
        } else {
            compile.push(arg);
        }
    }
    (compile, link)
}

fn is_link_arg(arg: &str) -> bool {
    arg.starts_with("-l")
        || arg.starts_with("-L")
        || arg.starts_with("-Wl,")
        || arg == "-framework"
        || arg == "Dispatch"
}

fn shared_library_flag(target_os: &str) -> &'static str {
    if is_macos_target(target_os) {
        "-dynamiclib"
    } else {
        "-shared"
    }
}

fn is_macos_target(target_os: &str) -> bool {
    target_os == "macos" || target_os == "darwin"
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

fn print_usage() {
    eprintln!(
        "usage: cielc [--emit MODE|--emit-c] [--debug|--release] [--cc cc] [--cflag flag] [--ldflag flag] [--save-c path] [--project-root root] [--std-path root] [--target-os os] [--target-arch arch] [--feature name] <input.ciel> [-o output]"
    );
}
