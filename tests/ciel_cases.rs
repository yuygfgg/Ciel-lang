use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use cielc::{
    BuildPlan, BuildProfile, CompileOptions,
    build::native::{CmakeOutput, CmakeOutputKind, build_cmake_output},
    compile_to_build_plan, compile_to_c,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TestKind {
    Compile,
    Run,
    Error,
    Host,
    Dependency,
    Manual,
    KnownFailCompile,
    KnownFailCc,
    KnownFailRun,
    KnownFailAccepts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sanitizer {
    Address,
    Thread,
}

impl Sanitizer {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "address" | "asan" => Ok(Self::Address),
            "thread" | "tsan" => Ok(Self::Thread),
            other => Err(format!(
                "unsupported sanitizer `{other}`; expected `address` or `thread`"
            )),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Address => "address",
            Self::Thread => "thread",
        }
    }

    fn flags(self) -> Vec<String> {
        let mut flags = match self {
            Self::Address => vec![
                "-fsanitize=address".to_string(),
                "-fno-omit-frame-pointer".to_string(),
                "-g".to_string(),
            ],
            Self::Thread => vec![
                "-fsanitize=thread".to_string(),
                "-fno-omit-frame-pointer".to_string(),
                "-g".to_string(),
            ],
        };
        if cfg!(target_os = "macos") {
            flags.push("-Wl,-no_warn_duplicate_libraries".to_string());
        }
        flags
    }
}

#[derive(Clone, Debug)]
struct CCount {
    needle: String,
    expected: usize,
}

#[derive(Clone, Debug)]
struct Case {
    path: PathBuf,
    kind: TestKind,
    expect_exit: Option<i32>,
    expect_stdout: Option<String>,
    expect_stderr_contains: Vec<String>,
    expect_errors: Vec<String>,
    expect_c_contains: Vec<String>,
    expect_c_not_contains: Vec<String>,
    expect_c_counts: Vec<CCount>,
    run_args: Vec<String>,
    features: Vec<String>,
    package_roots: Vec<PathBuf>,
    allow_native_build: bool,
    sanitizers: Vec<Sanitizer>,
    warning_clean: bool,
    host: Option<PathBuf>,
    known_fail_reason: Option<String>,
}

#[test]
fn ciel_case_metadata_is_valid() {
    let cases = load_cases().unwrap_or_else(|errors| panic!("{}", errors.join("\n\n")));
    let active = cases
        .iter()
        .filter(|case| !matches!(case.kind, TestKind::Dependency | TestKind::Manual))
        .count();
    assert!(active > 0, "no active Ciel fixture cases were discovered");
}

fn run_generated_case(relative_path: &'static str) {
    run_with_large_stack(relative_path, move || {
        let path = cases_root().join(relative_path);
        let case = parse_case(&path)
            .and_then(|case| {
                validate_case(&case)?;
                Ok(case)
            })
            .unwrap_or_else(|error| panic!("{}: {error}", case_label(&path)));
        assert!(
            !matches!(case.kind, TestKind::Dependency | TestKind::Manual),
            "generated test targeted inactive fixture `{}`",
            case_label(&path)
        );
        if let Err(error) = run_case(&case) {
            panic!("{}:\n{error}", case_label(&case.path));
        }
    });
}

mod generated_ciel_cases {
    include!(concat!(env!("OUT_DIR"), "/ciel_case_tests.rs"));
}

fn run_with_large_stack<F>(name: &str, f: F)
where
    F: FnOnce() + Send + 'static,
{
    let handle = std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap_or_else(|error| panic!("failed to spawn `{name}` test thread: {error}"));
    if let Err(payload) = handle.join() {
        std::panic::resume_unwind(payload);
    }
}

#[test]
fn cli_full_compile_debug_and_release_control_overflow() {
    let path = cases_root()
        .join("cli/cli_full_compile_debug_and_release_control_overflow/cli_overflow.ciel");

    let debug_exe = temp_artifact(&path, "debug.exe");
    let debug_build = Command::new(cielc_bin())
        .arg(&path)
        .arg("-o")
        .arg(&debug_exe)
        .output()
        .unwrap();
    assert_cli_success(&debug_build);
    let debug_run = Command::new(&debug_exe).output().unwrap();
    assert_eq!(debug_run.status.code(), Some(101));
    assert!(
        String::from_utf8_lossy(&debug_run.stderr).contains("integer overflow"),
        "stderr:\n{}",
        String::from_utf8_lossy(&debug_run.stderr)
    );

    let release_exe = temp_artifact(&path, "release.exe");
    let release_build = Command::new(cielc_bin())
        .arg("--release")
        .arg(&path)
        .arg("-o")
        .arg(&release_exe)
        .output()
        .unwrap();
    assert_cli_success(&release_build);
    let release_run = Command::new(&release_exe).output().unwrap();
    assert_eq!(release_run.status.code(), Some(128));
}

#[test]
fn cli_output_modes_std_path_and_preserved_c_work() {
    let dir = cases_root().join("cli/cli_output_modes_std_path_and_preserved_c_work");
    let simple = dir.join("cli_modes.ciel");
    let emit_c = temp_artifact(&simple, "emit.c");
    let emit = Command::new(cielc_bin())
        .arg("--emit-c")
        .arg(&simple)
        .arg("-o")
        .arg(&emit_c)
        .output()
        .unwrap();
    assert_cli_success(&emit);
    let emitted = fs::read_to_string(&emit_c).unwrap();
    assert!(emitted.contains("/* generated by cielc */"));

    let saved_c = temp_artifact(&simple, "saved.c");
    let exe = temp_artifact(&simple, "saved.exe");
    let compile = Command::new(cielc_bin())
        .arg("--save-c")
        .arg(&saved_c)
        .arg(&simple)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert_cli_success(&compile);
    assert!(saved_c.exists());
    assert!(fs::read_to_string(&saved_c).unwrap().contains("#line"));
    assert_eq!(Command::new(&exe).output().unwrap().status.code(), Some(3));

    let repo_root = repo_root();
    let std_user = dir.join("cli_std_path.ciel");
    let object = temp_artifact(&std_user, "o");
    let obj = Command::new(cielc_bin())
        .arg("--emit")
        .arg("obj")
        .arg("--std-path")
        .arg(&repo_root)
        .arg("--cflag")
        .arg("-DCIEL_CLI_TEST=1")
        .arg(&std_user)
        .arg("-o")
        .arg(&object)
        .output()
        .unwrap();
    assert_cli_success(&obj);
    assert!(object.exists());

    let shared_source = dir.join("cli_shared.ciel");
    let shared = temp_artifact(
        &shared_source,
        if std::env::consts::OS == "macos" {
            "dylib"
        } else {
            "so"
        },
    );
    let dylib = Command::new(cielc_bin())
        .arg("--emit")
        .arg("shared")
        .arg(&shared_source)
        .arg("-o")
        .arg(&shared)
        .output()
        .unwrap();
    assert_cli_success(&dylib);
    assert!(shared.exists());
}

#[test]
fn cli_uses_project_manifest_entry() {
    let repo_root = repo_root();
    let project_dir = repo_root.join("examples/intranet_tunnel");
    let manifest = project_dir.join("ciel.toml");
    let emit_c = temp_artifact(&project_dir.join("test/frame_test.ciel"), "project_entry.c");
    let output = Command::new(cielc_bin())
        .arg("--emit-c")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--std-path")
        .arg(&repo_root)
        .arg("--entry")
        .arg("frame_test")
        .arg("-o")
        .arg(&emit_c)
        .output()
        .unwrap();
    assert_cli_success(&output);
    assert!(
        fs::read_to_string(&emit_c)
            .unwrap()
            .contains("/* generated by cielc */")
    );
}

#[test]
fn cli_uses_project_manifest_default_entry() {
    let repo_root = repo_root();
    let project_dir = repo_root.join("examples/intranet_tunnel");
    let manifest = project_dir.join("ciel.toml");
    let emit_c = temp_artifact(
        &project_dir.join("main_server.ciel"),
        "project_default_entry.c",
    );
    let output = Command::new(cielc_bin())
        .arg("--emit-c")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--std-path")
        .arg(&repo_root)
        .arg("-o")
        .arg(&emit_c)
        .output()
        .unwrap();
    assert_cli_success(&output);
    assert!(
        fs::read_to_string(&emit_c)
            .unwrap()
            .contains("tunnel-server")
    );
}

#[test]
fn cli_discovers_project_manifest_from_current_dir() {
    let repo_root = repo_root();
    let project_dir = repo_root.join("examples/intranet_tunnel");
    let emit_c = temp_artifact(
        &project_dir.join("test/codec_test.ciel"),
        "project_discovery.c",
    );
    let output = Command::new(cielc_bin())
        .current_dir(&project_dir)
        .arg("--emit-c")
        .arg("--std-path")
        .arg(&repo_root)
        .arg("--entry")
        .arg("codec_test")
        .arg("-o")
        .arg(&emit_c)
        .output()
        .unwrap();
    assert_cli_success(&output);
    assert!(
        fs::read_to_string(&emit_c)
            .unwrap()
            .contains("/* generated by cielc */")
    );
}

#[test]
fn project_manifest_tracks_project_entry_inputs() {
    let repo_root = repo_root();
    let project_dir = repo_root.join("examples/intranet_tunnel");
    let manifest = project_dir.join("ciel.toml");
    let path = project_dir.join("test/frame_test.ciel");
    let options = CompileOptions::new(&path)
        .with_project_manifest(&manifest)
        .with_std_path(&repo_root);
    let plan = compile_to_build_plan(options).unwrap();

    assert!(
        plan.package_inputs
            .iter()
            .any(|path| path.ends_with("examples/intranet_tunnel/ciel.toml"))
    );
}

#[test]
fn build_plan_wraps_generated_c_profile_inputs_and_runtime_requirements() {
    let path = cases_root().join("backend/compiles_basic_main_to_c/main.ciel");
    let options = CompileOptions::new(&path)
        .with_std_path(repo_root())
        .with_target_os("linux")
        .with_build_profile(BuildProfile::Release);
    let c = compile_to_c(options.clone()).unwrap();
    let plan = compile_to_build_plan(options).unwrap();

    assert_eq!(plan.generated_c, c);
    assert!(plan.generated_c.contains("#include \"ciel_runtime.h\""));
    assert!(!plan.generated_c.contains("#include <gc/gc.h>"));
    assert_eq!(plan.profile, BuildProfile::Release);
    assert!(plan.package_inputs.contains(&path));
    assert!(
        plan.package_inputs
            .iter()
            .any(|path| path.ends_with("std/async/async.ciel"))
    );
    assert_eq!(plan.cmake_targets.len(), 1);
    assert_eq!(plan.cmake_targets[0].target, "ciel_runtime");
    assert!(
        plan.cmake_targets[0]
            .cmake_file
            .ends_with("runtime/CMakeLists.txt")
    );
}

#[test]
fn build_plan_adds_native_requirements_from_imported_std_packages() {
    let path = cases_root().join("std_crypto/rng_hash_mac_and_constant_time/crypto_basic.ciel");
    let options = CompileOptions::new(&path)
        .with_std_path(repo_root())
        .with_target_os("linux");
    let plan = compile_to_build_plan(options).unwrap();

    assert!(
        plan.cmake_targets
            .iter()
            .any(|target| target.target == "ciel_std_crypto")
    );
    assert!(
        plan.package_inputs
            .iter()
            .any(|path| path.ends_with("std/crypto/ciel.toml"))
    );
}

#[test]
fn package_root_imports_user_native_package_and_requires_allow_policy() {
    let path = cases_root().join("package/sqlite/sqlite.ciel");
    let package_root = repo_root().join("libs/sqlite");
    let blocked_exe = temp_artifact(&path, "blocked.exe");
    let blocked = Command::new(cielc_bin())
        .arg("--package-root")
        .arg(&package_root)
        .arg(&path)
        .arg("-o")
        .arg(&blocked_exe)
        .output()
        .unwrap();
    assert!(
        !blocked.status.success(),
        "cielc unexpectedly allowed third-party native build\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&blocked.stdout),
        String::from_utf8_lossy(&blocked.stderr)
    );
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("--allow-native-build"),
        "stderr:\n{}",
        String::from_utf8_lossy(&blocked.stderr)
    );

    let options = CompileOptions::new(&path)
        .with_std_path(repo_root())
        .with_package_root(package_root)
        .with_allow_native_build(true);
    let plan = compile_to_build_plan(options).unwrap();
    assert!(plan.allow_native_build);
    assert!(plan.cmake_targets.iter().any(|target| {
        target.target == "ciel_lib_sqlite" && target.requires_allow_native_build
    }));
    assert!(
        plan.package_inputs
            .iter()
            .any(|path| path.ends_with("libs/sqlite/ciel.toml"))
    );
}

fn load_cases() -> Result<Vec<Case>, Vec<String>> {
    let mut paths = Vec::new();
    collect_ciel_files(&cases_root(), &mut paths).map_err(|error| vec![error])?;
    paths.sort();

    let mut cases = Vec::new();
    let mut errors = Vec::new();
    for path in paths {
        match parse_case(&path).and_then(|case| {
            validate_case(&case)?;
            Ok(case)
        }) {
            Ok(case) => cases.push(case),
            Err(error) => errors.push(format!("{}: {error}", case_label(&path))),
        }
    }

    if errors.is_empty() {
        Ok(cases)
    } else {
        Err(errors)
    }
}

fn collect_ciel_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in
        fs::read_dir(dir).map_err(|error| format!("failed to read `{}`: {error}", dir.display()))?
    {
        let entry =
            entry.map_err(|error| format!("failed to read `{}` entry: {error}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_ciel_files(&path, out)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("ciel") {
            out.push(path);
        }
    }
    Ok(())
}

fn parse_case(path: &Path) -> Result<Case, String> {
    let source =
        fs::read_to_string(path).map_err(|error| format!("failed to read fixture: {error}"))?;
    let mut kind = None;
    let mut expect_exit = None;
    let mut expect_stdout = None;
    let mut expect_stderr_contains = Vec::new();
    let mut expect_errors = Vec::new();
    let mut expect_c_contains = Vec::new();
    let mut expect_c_not_contains = Vec::new();
    let mut expect_c_counts = Vec::new();
    let mut run_args = Vec::new();
    let mut features = Vec::new();
    let mut package_roots = Vec::new();
    let mut allow_native_build = false;
    let mut sanitizers = Vec::new();
    let mut warning_clean = false;
    let mut host = None;
    let mut known_fail_reason = None;

    for line in source.lines() {
        let Some(comment) = line.trim_start().strip_prefix("//") else {
            continue;
        };
        let Some((key, raw_value)) = comment.trim_start().split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = raw_value.trim();
        match key {
            "ciel-test" => {
                if kind.is_some() {
                    return Err("duplicate `ciel-test` metadata".to_string());
                }
                kind = Some(match value {
                    "compile" => TestKind::Compile,
                    "run" => TestKind::Run,
                    "error" => TestKind::Error,
                    "host" => TestKind::Host,
                    "dependency" => TestKind::Dependency,
                    "manual" => TestKind::Manual,
                    "known-fail-compile" => TestKind::KnownFailCompile,
                    "known-fail-cc" => TestKind::KnownFailCc,
                    "known-fail-run" => TestKind::KnownFailRun,
                    "known-fail-accepts" => TestKind::KnownFailAccepts,
                    _ => return Err(format!("unknown ciel-test kind `{value}`")),
                });
            }
            "expect-exit" => {
                expect_exit = Some(
                    value
                        .parse::<i32>()
                        .map_err(|error| format!("invalid expect-exit `{value}`: {error}"))?,
                );
            }
            "expect-stdout" => expect_stdout = Some(raw_value.trim_start().to_string()),
            "expect-stderr-contains" => expect_stderr_contains.push(value.to_string()),
            "expect-error" => expect_errors.push(value.to_string()),
            "expect-c-contains" => expect_c_contains.push(value.to_string()),
            "expect-c-not-contains" => expect_c_not_contains.push(value.to_string()),
            "run-arg" => run_args.push(raw_value.trim_start().to_string()),
            "expect-c-count" => {
                let (needle, count) = value
                    .rsplit_once("=>")
                    .ok_or_else(|| "expect-c-count must use `needle => count`".to_string())?;
                expect_c_counts.push(CCount {
                    needle: needle.trim().to_string(),
                    expected: count.trim().parse::<usize>().map_err(|error| {
                        format!("invalid expect-c-count `{}`: {error}", count.trim())
                    })?,
                });
            }
            "feature" => features.push(value.to_string()),
            "package-root" => package_roots.push(metadata_path(path, value)),
            "allow-native-build" => {
                allow_native_build = match value {
                    "true" => true,
                    "false" => false,
                    _ => return Err(format!("invalid allow-native-build value `{value}`")),
                };
            }
            "sanitizer" => {
                let sanitizer = Sanitizer::parse(value)?;
                if sanitizers.contains(&sanitizer) {
                    return Err(format!("duplicate sanitizer `{}`", sanitizer.label()));
                }
                sanitizers.push(sanitizer);
            }
            "known-fail-reason" => known_fail_reason = Some(value.to_string()),
            "warning-clean" => {
                warning_clean = match value {
                    "true" => true,
                    "false" => false,
                    _ => return Err(format!("invalid warning-clean value `{value}`")),
                };
            }
            "host" => host = Some(path.parent().unwrap().join(value)),
            _ => {}
        }
    }

    Ok(Case {
        path: path.to_path_buf(),
        kind: kind.ok_or_else(|| "missing `// ciel-test: ...` metadata".to_string())?,
        expect_exit,
        expect_stdout,
        expect_stderr_contains,
        expect_errors,
        expect_c_contains,
        expect_c_not_contains,
        expect_c_counts,
        run_args,
        features,
        package_roots,
        allow_native_build,
        sanitizers,
        warning_clean,
        host,
        known_fail_reason,
    })
}

fn validate_case(case: &Case) -> Result<(), String> {
    match case.kind {
        TestKind::Dependency | TestKind::Manual => {
            if case.expect_exit.is_some()
                || case.expect_stdout.is_some()
                || !case.expect_stderr_contains.is_empty()
                || !case.expect_errors.is_empty()
                || !case.expect_c_contains.is_empty()
                || !case.expect_c_not_contains.is_empty()
                || !case.expect_c_counts.is_empty()
                || case.warning_clean
                || case.host.is_some()
                || case.known_fail_reason.is_some()
                || !case.features.is_empty()
                || !case.package_roots.is_empty()
                || case.allow_native_build
                || !case.sanitizers.is_empty()
                || !case.run_args.is_empty()
            {
                return Err(
                    "dependency/manual fixtures must not declare expectations or sanitizer metadata"
                        .to_string(),
                );
            }
        }
        TestKind::KnownFailCompile => {
            if case.known_fail_reason.is_none() {
                return Err("known-fail-compile fixtures require `known-fail-reason`".to_string());
            }
            if case.expect_exit.is_some()
                || case.expect_stdout.is_some()
                || !case.expect_stderr_contains.is_empty()
                || !case.expect_c_contains.is_empty()
                || !case.expect_c_not_contains.is_empty()
                || !case.expect_c_counts.is_empty()
                || case.warning_clean
                || case.host.is_some()
                || !case.sanitizers.is_empty()
                || !case.package_roots.is_empty()
                || case.allow_native_build
                || !case.run_args.is_empty()
            {
                return Err(
                    "known-fail-compile fixtures cannot declare C, runtime, host, sanitizer, or warning expectations"
                        .to_string(),
                );
            }
        }
        TestKind::KnownFailCc => {
            if case.known_fail_reason.is_none() {
                return Err("known-fail-cc fixtures require `known-fail-reason`".to_string());
            }
            if case.expect_exit.is_some()
                || case.expect_stdout.is_some()
                || !case.expect_stderr_contains.is_empty()
                || !case.expect_c_contains.is_empty()
                || !case.expect_c_not_contains.is_empty()
                || !case.expect_c_counts.is_empty()
                || case.warning_clean
                || case.host.is_some()
                || !case.sanitizers.is_empty()
                || !case.package_roots.is_empty()
                || case.allow_native_build
                || !case.run_args.is_empty()
            {
                return Err(
                    "known-fail-cc fixtures cannot declare C, runtime, host, sanitizer, or warning expectations"
                        .to_string(),
                );
            }
        }
        TestKind::KnownFailRun => {
            if case.known_fail_reason.is_none() {
                return Err("known-fail-run fixtures require `known-fail-reason`".to_string());
            }
            if case.expect_exit.is_none() {
                return Err("known-fail-run fixtures require `expect-exit`".to_string());
            }
            if !case.expect_errors.is_empty()
                || !case.expect_c_contains.is_empty()
                || !case.expect_c_not_contains.is_empty()
                || !case.expect_c_counts.is_empty()
                || case.warning_clean
                || case.host.is_some()
                || !case.sanitizers.is_empty()
                || !case.package_roots.is_empty()
                || case.allow_native_build
                || !case.run_args.is_empty()
            {
                return Err(
                    "known-fail-run fixtures cannot declare C, error, host, sanitizer, or warning expectations"
                        .to_string(),
                );
            }
        }
        TestKind::KnownFailAccepts => {
            if case.known_fail_reason.is_none() {
                return Err("known-fail-accepts fixtures require `known-fail-reason`".to_string());
            }
            if case.expect_exit.is_some()
                || case.expect_stdout.is_some()
                || !case.expect_stderr_contains.is_empty()
                || !case.expect_errors.is_empty()
                || !case.expect_c_contains.is_empty()
                || !case.expect_c_not_contains.is_empty()
                || !case.expect_c_counts.is_empty()
                || case.warning_clean
                || case.host.is_some()
                || !case.sanitizers.is_empty()
                || !case.package_roots.is_empty()
                || case.allow_native_build
                || !case.run_args.is_empty()
            {
                return Err(
                    "known-fail-accepts fixtures cannot declare C, error, runtime, host, sanitizer, or warning expectations"
                        .to_string(),
                );
            }
        }
        TestKind::Run => {
            if case.expect_exit.is_none() {
                return Err("run fixtures require `expect-exit`".to_string());
            }
            if !case.expect_errors.is_empty() || case.host.is_some() {
                return Err("run fixtures cannot declare error or host expectations".to_string());
            }
        }
        TestKind::Host => {
            if case.expect_exit.is_none() {
                return Err("host fixtures require `expect-exit`".to_string());
            }
            let host = case
                .host
                .as_ref()
                .ok_or_else(|| "host fixtures require `host`".to_string())?;
            if !host.exists() {
                return Err(format!("host fixture `{}` does not exist", host.display()));
            }
            if !case.expect_errors.is_empty() || !case.run_args.is_empty() {
                return Err(
                    "host fixtures cannot declare error expectations or run arguments".to_string(),
                );
            }
        }
        TestKind::Compile => {
            if case.expect_exit.is_some()
                || case.expect_stdout.is_some()
                || !case.expect_stderr_contains.is_empty()
                || !case.expect_errors.is_empty()
                || case.host.is_some()
                || case.known_fail_reason.is_some()
                || !case.run_args.is_empty()
            {
                return Err(
                    "compile fixtures cannot declare runtime, host, or error expectations"
                        .to_string(),
                );
            }
        }
        TestKind::Error => {
            if case.expect_errors.is_empty() {
                return Err("error fixtures require at least one `expect-error`".to_string());
            }
            if case.expect_exit.is_some()
                || case.expect_stdout.is_some()
                || !case.expect_stderr_contains.is_empty()
                || !case.expect_c_contains.is_empty()
                || !case.expect_c_not_contains.is_empty()
                || !case.expect_c_counts.is_empty()
                || case.warning_clean
                || case.host.is_some()
                || case.known_fail_reason.is_some()
                || !case.sanitizers.is_empty()
                || !case.package_roots.is_empty()
                || case.allow_native_build
                || !case.run_args.is_empty()
            {
                return Err(
                    "error fixtures cannot declare C, runtime, host, sanitizer, or warning expectations"
                        .to_string(),
                );
            }
        }
    }
    Ok(())
}

fn run_case(case: &Case) -> Result<(), String> {
    match case.kind {
        TestKind::Compile => {
            let plan = compile_case(case)?;
            check_c_expectations(case, &plan.generated_c)?;
            compile_warning_clean(case, &plan)?;
            for (suffix, flags) in sanitizer_c_flag_variants(case, "compile.exe") {
                let flag_refs = flags.iter().map(String::as_str).collect::<Vec<_>>();
                compile_c(&case.path, &plan, &suffix, &flag_refs)?;
            }
            Ok(())
        }
        TestKind::Run => {
            let plan = compile_case(case)?;
            check_c_expectations(case, &plan.generated_c)?;
            let run_args = resolve_run_args(case)?;
            compile_warning_clean(case, &plan)?;
            for (suffix, flags) in c_flag_variants(case, "run.exe") {
                let flag_refs = flags.iter().map(String::as_str).collect::<Vec<_>>();
                let exe = compile_c(&case.path, &plan, &suffix, &flag_refs)?;
                let output = Command::new(&exe)
                    .args(&run_args)
                    .output()
                    .map_err(|error| format!("failed to run `{}`: {error}", exe.display()))?;
                check_output(case, &output)?;
            }
            Ok(())
        }
        TestKind::Host => {
            let plan = compile_case(case)?;
            check_c_expectations(case, &plan.generated_c)?;
            for (suffix, flags) in c_flag_variants(case, "host.exe") {
                let flag_refs = flags.iter().map(String::as_str).collect::<Vec<_>>();
                let exe = compile_host_c(case, &plan, &suffix, &flag_refs)?;
                let output = Command::new(&exe)
                    .output()
                    .map_err(|error| format!("failed to run `{}`: {error}", exe.display()))?;
                check_output(case, &output)?;
            }
            Ok(())
        }
        TestKind::Error => match compile_case(case) {
            Ok(_) => Err("expected compilation to fail, but it succeeded".to_string()),
            Err(diagnostics) => {
                for expected in &case.expect_errors {
                    if !diagnostics.contains(expected) {
                        return Err(format!(
                            "expected diagnostic containing `{expected}`\nactual diagnostics:\n{diagnostics}"
                        ));
                    }
                }
                Ok(())
            }
        },
        TestKind::KnownFailCompile => match compile_case(case) {
            Ok(_) => Err(format!(
                "known-fail now compiles; promote this fixture. recorded reason: {}",
                case.known_fail_reason.as_deref().unwrap()
            )),
            Err(diagnostics) => {
                for expected in &case.expect_errors {
                    if !diagnostics.contains(expected) {
                        return Err(format!(
                            "known-fail diagnostic no longer contains `{expected}`\nactual diagnostics:\n{diagnostics}"
                        ));
                    }
                }
                Ok(())
            }
        },
        TestKind::KnownFailCc => {
            let plan = compile_case(case)?;
            match compile_c(&case.path, &plan, "known_fail.exe", &[]) {
                Ok(_) => Err(format!(
                    "known-fail-cc now passes C compilation; promote this fixture. recorded reason: {}",
                    case.known_fail_reason.as_deref().unwrap()
                )),
                Err(error) => {
                    for expected in &case.expect_errors {
                        if !error.contains(expected) {
                            return Err(format!(
                                "known-fail C compiler error no longer contains `{expected}`\nactual error:\n{error}"
                            ));
                        }
                    }
                    Ok(())
                }
            }
        }
        TestKind::KnownFailRun => {
            let plan = compile_case(case)?;
            let exe = compile_c(&case.path, &plan, "known_fail_run.exe", &[])?;
            let run_args = resolve_run_args(case)?;
            let output = Command::new(&exe)
                .args(&run_args)
                .output()
                .map_err(|error| format!("failed to run `{}`: {error}", exe.display()))?;
            match check_output(case, &output) {
                Ok(()) => Err(format!(
                    "known-fail-run now passes; promote this fixture. recorded reason: {}",
                    case.known_fail_reason.as_deref().unwrap()
                )),
                Err(_) => Ok(()),
            }
        }
        TestKind::KnownFailAccepts => match compile_case(case) {
            Ok(_) => Ok(()),
            Err(diagnostics) => Err(format!(
                "known-fail-accepts no longer reproduces; promote this fixture to an error test. recorded reason: {}\nactual diagnostics:\n{diagnostics}",
                case.known_fail_reason.as_deref().unwrap()
            )),
        },
        TestKind::Dependency | TestKind::Manual => Ok(()),
    }
}

fn resolve_run_args(case: &Case) -> Result<Vec<String>, String> {
    let mut resolved = Vec::new();
    for arg in &case.run_args {
        let Some(suffix) = arg.strip_prefix("@tmp/") else {
            resolved.push(arg.clone());
            continue;
        };
        let tmp_dir = temp_artifact(&case.path, "tmp");
        fs::create_dir_all(&tmp_dir)
            .map_err(|error| format!("failed to create `{}`: {error}", tmp_dir.display()))?;
        let path = tmp_dir.join(suffix);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create `{}`: {error}", parent.display()))?;
        }
        resolved.push(path.display().to_string());
    }
    Ok(resolved)
}

fn compile_case(case: &Case) -> Result<BuildPlan, String> {
    let mut options = CompileOptions::new(&case.path).with_std_path(repo_root());
    for feature in &case.features {
        options = options.with_feature(feature.clone());
    }
    for package_root in &case.package_roots {
        options = options.with_package_root(package_root.clone());
    }
    options = options.with_allow_native_build(case.allow_native_build);
    compile_to_build_plan(options).map_err(|diagnostics| {
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect::<Vec<_>>()
            .join("\n")
    })
}

fn metadata_path(case_path: &Path, value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("@repo/") {
        return repo_root().join(rest);
    }
    case_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(value)
}

fn compile_warning_clean(case: &Case, plan: &BuildPlan) -> Result<(), String> {
    if case.warning_clean {
        compile_c(
            &case.path,
            plan,
            "warn.exe",
            &["-Wall", "-Wextra", "-Werror"],
        )?;
    }
    Ok(())
}

fn c_flag_variants(case: &Case, plain_suffix: &str) -> Vec<(String, Vec<String>)> {
    let mut variants = vec![(plain_suffix.to_string(), Vec::new())];
    variants.extend(sanitizer_c_flag_variants(case, plain_suffix));
    variants
}

fn sanitizer_c_flag_variants(case: &Case, plain_suffix: &str) -> Vec<(String, Vec<String>)> {
    case.sanitizers
        .iter()
        .copied()
        .map(|sanitizer| (sanitizer_suffix(plain_suffix, sanitizer), sanitizer.flags()))
        .collect()
}

fn sanitizer_suffix(plain_suffix: &str, sanitizer: Sanitizer) -> String {
    let label = sanitizer.label();
    if let Some(stem) = plain_suffix.strip_suffix(".exe") {
        format!("{stem}.{label}.exe")
    } else {
        format!("{plain_suffix}.{label}")
    }
}

fn check_c_expectations(case: &Case, c: &str) -> Result<(), String> {
    for needle in &case.expect_c_contains {
        if !c.contains(needle) {
            return Err(format!("generated C did not contain `{needle}`"));
        }
    }
    for needle in &case.expect_c_not_contains {
        if c.contains(needle) {
            return Err(format!("generated C unexpectedly contained `{needle}`"));
        }
    }
    for count in &case.expect_c_counts {
        let actual = c.matches(&count.needle).count();
        if actual != count.expected {
            return Err(format!(
                "generated C contained `{}` {actual} time(s), expected {}",
                count.needle, count.expected
            ));
        }
    }
    Ok(())
}

fn check_output(case: &Case, output: &Output) -> Result<(), String> {
    let expected = case.expect_exit.unwrap();
    if output.status.code() != Some(expected) {
        return Err(format!(
            "exit status was {:?}, expected {expected}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    if let Some(expected_stdout) = &case.expect_stdout {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout != *expected_stdout {
            return Err(format!(
                "stdout was `{stdout}`, expected `{expected_stdout}`\nstderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    for needle in &case.expect_stderr_contains {
        if !stderr.contains(needle) {
            return Err(format!(
                "stderr did not contain `{needle}`\nstdout:\n{}\nstderr:\n{stderr}",
                String::from_utf8_lossy(&output.stdout)
            ));
        }
    }
    Ok(())
}

fn compile_c(
    source_path: &Path,
    plan: &BuildPlan,
    suffix: &str,
    extra_flags: &[&str],
) -> Result<PathBuf, String> {
    let c_path = temp_artifact(source_path, "c");
    let exe_path = temp_artifact(source_path, suffix);
    fs::write(&c_path, &plan.generated_c)
        .map_err(|error| format!("failed to write `{}`: {error}", c_path.display()))?;
    run_cc(&c_path, &exe_path, plan, extra_flags)?;
    Ok(exe_path)
}

fn compile_host_c(
    case: &Case,
    plan: &BuildPlan,
    suffix: &str,
    extra_flags: &[&str],
) -> Result<PathBuf, String> {
    let generated_c = temp_artifact(&case.path, "generated.c");
    let host_c = temp_artifact(&case.path, "host.c");
    let exe = temp_artifact(&case.path, suffix);
    fs::write(&generated_c, &plan.generated_c)
        .map_err(|error| format!("failed to write `{}`: {error}", generated_c.display()))?;
    let host_source = fs::read_to_string(case.host.as_ref().unwrap())
        .map_err(|error| format!("failed to read host fixture: {error}"))?;
    let generated_name = generated_c.file_name().unwrap().to_str().unwrap();
    fs::write(
        &host_c,
        format!("#include \"{generated_name}\"\n{host_source}"),
    )
    .map_err(|error| format!("failed to write `{}`: {error}", host_c.display()))?;
    run_cc(&host_c, &exe, plan, extra_flags)?;
    Ok(exe)
}

fn run_cc(
    c_path: &Path,
    output: &Path,
    plan: &BuildPlan,
    extra_flags: &[&str],
) -> Result<(), String> {
    let flags = extra_flags
        .iter()
        .map(|flag| (*flag).to_string())
        .collect::<Vec<_>>();
    build_cmake_output(
        plan,
        &CmakeOutput {
            source_path: c_path,
            output_path: output,
            kind: CmakeOutputKind::Executable,
            c_compiler: "cc",
            compile_flags: flags.clone(),
            link_flags: flags,
            target_os: std::env::consts::OS,
        },
    )
}

fn cielc_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cielc"))
}

fn assert_cli_success(output: &Output) {
    assert!(
        output.status.success(),
        "cielc failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn cases_root() -> PathBuf {
    repo_root().join("tests/cases")
}

fn temp_artifact(source_path: &Path, suffix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cielc_test_{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{}.{}", artifact_stem(source_path), suffix))
}

fn artifact_stem(path: &Path) -> String {
    let root = cases_root();
    let rel = path.strip_prefix(&root).unwrap_or(path);
    rel.to_string_lossy()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn case_label(path: &Path) -> String {
    let root = cases_root();
    path.strip_prefix(&root)
        .unwrap_or(path)
        .display()
        .to_string()
}
