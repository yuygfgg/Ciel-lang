use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};

use crate::common::normalize_path;

use super::requirements::{BuildPlan, BuildProfile, CmakeTarget};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CmakeOutputKind {
    Executable,
    SharedLibrary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmakeOutput<'a> {
    pub source_path: &'a Path,
    pub output_path: &'a Path,
    pub kind: CmakeOutputKind,
    pub c_compiler: &'a str,
    pub compile_flags: Vec<String>,
    pub link_flags: Vec<String>,
    pub target_os: &'a str,
}

pub fn build_cmake_output(plan: &BuildPlan, output: &CmakeOutput<'_>) -> Result<(), String> {
    check_native_build_policy(plan)?;
    if output.kind == CmakeOutputKind::Executable {
        return build_reusable_cmake_executable(plan, output);
    }

    let lock = CMAKE_OUTPUT_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| "CMake output build lock was poisoned".to_string())?;
    let build_root = cmake_output_build_root(plan, output);
    let source_dir = build_root.join("source");
    let binary_dir = build_root.join("build");
    fs::create_dir_all(&source_dir)
        .map_err(|error| format!("failed to create `{}`: {error}", source_dir.display()))?;
    if let Some(parent) = output.output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create `{}`: {error}", parent.display()))?;
    }

    let cmake_lists = render_output_cmake(plan, output)?;
    fs::write(source_dir.join("CMakeLists.txt"), cmake_lists).map_err(|error| {
        format!(
            "failed to write `{}`: {error}",
            source_dir.join("CMakeLists.txt").display()
        )
    })?;

    run_cmake_configure(&source_dir, &binary_dir, plan.profile, output.c_compiler)?;
    run_cmake_build(&binary_dir, "ciel_output", plan.profile)?;
    if !output.output_path.exists() {
        return Err(format!(
            "CMake target `ciel_output` finished but output `{}` was not produced",
            output.output_path.display()
        ));
    }
    Ok(())
}

fn check_native_build_policy(plan: &BuildPlan) -> Result<(), String> {
    if plan.allow_native_build {
        return Ok(());
    }
    let blocked = plan
        .cmake_targets
        .iter()
        .find(|target| target.requires_allow_native_build);
    let Some(target) = blocked else {
        return Ok(());
    };
    Err(format!(
        "third-party native CMake target `{}` from `{}` requires --allow-native-build",
        target.target,
        target.package_root.display()
    ))
}

static CMAKE_OUTPUT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn build_reusable_cmake_executable(
    plan: &BuildPlan,
    output: &CmakeOutput<'_>,
) -> Result<(), String> {
    let cache_key = reusable_cmake_output_key(plan, output);
    let lock = reusable_cmake_output_lock(cache_key)?;
    let _guard = lock
        .lock()
        .map_err(|_| "reusable CMake output build lock was poisoned".to_string())?;

    let build_root = reusable_cmake_output_build_root(output, cache_key);
    let source_dir = build_root.join("source");
    let binary_dir = build_root.join("build");
    let output_dir = build_root.join("out");
    let cached_source = source_dir.join("ciel_output_input.c");
    let cached_output = output_dir.join("ciel_output");

    fs::create_dir_all(&source_dir)
        .map_err(|error| format!("failed to create `{}`: {error}", source_dir.display()))?;
    fs::create_dir_all(&output_dir)
        .map_err(|error| format!("failed to create `{}`: {error}", output_dir.display()))?;
    if let Some(parent) = output.output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create `{}`: {error}", parent.display()))?;
    }

    fs::write(
        &cached_source,
        format!("#include {}\n", c_include_quote(output.source_path)),
    )
    .map_err(|error| {
        format!(
            "failed to write cached C source `{}` from `{}`: {error}",
            cached_source.display(),
            output.source_path.display(),
        )
    })?;

    let cached = CmakeOutput {
        source_path: &cached_source,
        output_path: &cached_output,
        kind: CmakeOutputKind::Executable,
        c_compiler: output.c_compiler,
        compile_flags: output.compile_flags.clone(),
        link_flags: output.link_flags.clone(),
        target_os: output.target_os,
    };
    let cmake_lists = render_output_cmake_with_native_dirs(
        plan,
        &cached,
        NativeBinaryDirs::Local { root: &build_root },
    )?;
    let cmake_lists_path = source_dir.join("CMakeLists.txt");
    let needs_configure = write_if_changed(&cmake_lists_path, &cmake_lists)?
        || !binary_dir.join("CMakeCache.txt").exists();
    if needs_configure {
        run_cmake_configure(&source_dir, &binary_dir, plan.profile, output.c_compiler)?;
    }
    run_cmake_build(&binary_dir, "ciel_output", plan.profile)?;
    let cached_output = cmake_output_path(&cached_output, output.target_os)?;
    if !cached_output.exists() {
        return Err(format!(
            "CMake target `ciel_output` finished but cached output `{}` was not produced",
            cached_output.display()
        ));
    }

    fs::copy(&cached_output, output.output_path).map_err(|error| {
        format!(
            "failed to copy cached executable `{}` to `{}`: {error}",
            cached_output.display(),
            output.output_path.display()
        )
    })?;
    #[cfg(unix)]
    {
        let permissions = fs::metadata(&cached_output)
            .map_err(|error| format!("failed to stat `{}`: {error}", cached_output.display()))?
            .permissions();
        fs::set_permissions(output.output_path, permissions).map_err(|error| {
            format!(
                "failed to set permissions on `{}`: {error}",
                output.output_path.display()
            )
        })?;
    }

    Ok(())
}

fn reusable_cmake_output_lock(key: u64) -> Result<Arc<Mutex<()>>, String> {
    let locks = REUSABLE_CMAKE_OUTPUT_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .map_err(|_| "reusable CMake output lock map was poisoned".to_string())?;
    Ok(locks
        .entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone())
}

static REUSABLE_CMAKE_OUTPUT_LOCKS: OnceLock<Mutex<HashMap<u64, Arc<Mutex<()>>>>> = OnceLock::new();

pub fn cmake_include_flags(targets: &[CmakeTarget]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut flags = Vec::new();
    for target in targets {
        let include_dir = target.package_root.join("include");
        if include_dir.is_dir() && seen.insert(include_dir.clone()) {
            flags.push(format!("-I{}", include_dir.display()));
        }
    }
    flags
}

fn run_cmake_configure(
    source_dir: &Path,
    build_dir: &Path,
    profile: BuildProfile,
    c_compiler: &str,
) -> Result<(), String> {
    let build_type = cmake_build_type(profile);
    let runtime_include_dir = ciel_runtime_include_dir();
    let mut command = Command::new("cmake");
    if let Some(generator) = preferred_cmake_generator() {
        command.arg("-G").arg(generator);
    }
    let output = command
        .arg("-S")
        .arg(source_dir)
        .arg("-B")
        .arg(build_dir)
        .arg(format!("-DCMAKE_BUILD_TYPE={build_type}"))
        .arg(format!("-DCMAKE_C_COMPILER={c_compiler}"))
        .arg("-DCMAKE_EXPORT_COMPILE_COMMANDS=ON")
        .arg(format!(
            "-DCIEL_BUILD_PROFILE={}",
            build_type.to_ascii_lowercase()
        ))
        .arg(format!(
            "-DCIEL_RUNTIME_INCLUDE_DIR={}",
            runtime_include_dir.display()
        ))
        .output()
        .map_err(|error| format!("failed to invoke cmake configure: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format_command_error("cmake configure", &output))
}

fn run_cmake_build(build_dir: &Path, target: &str, profile: BuildProfile) -> Result<(), String> {
    let build_type = cmake_build_type(profile);
    let output = Command::new("cmake")
        .arg("--build")
        .arg(build_dir)
        .arg("--target")
        .arg(target)
        .arg("--config")
        .arg(build_type)
        .output()
        .map_err(|error| format!("failed to invoke cmake build: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format_command_error("cmake build", &output))
}

fn render_output_cmake(plan: &BuildPlan, output: &CmakeOutput<'_>) -> Result<String, String> {
    render_output_cmake_with_native_dirs(plan, output, NativeBinaryDirs::Shared)
}

enum NativeBinaryDirs<'a> {
    Shared,
    Local { root: &'a Path },
}

fn render_output_cmake_with_native_dirs(
    plan: &BuildPlan,
    output: &CmakeOutput<'_>,
    native_dirs: NativeBinaryDirs<'_>,
) -> Result<String, String> {
    let mut out = String::new();
    out.push_str("cmake_minimum_required(VERSION 3.16)\n");
    out.push_str("project(ciel_generated_output C)\n\n");
    out.push_str("set(CMAKE_C_STANDARD 11)\n");
    out.push_str("set(CMAKE_C_STANDARD_REQUIRED ON)\n");
    out.push_str("set(CMAKE_C_EXTENSIONS ON)\n");
    out.push_str(&format!(
        "set(CIEL_RUNTIME_INCLUDE_DIR {} CACHE PATH \"Ciel runtime include directory\" FORCE)\n\n",
        cmake_quote(&ciel_runtime_include_dir())
    ));

    let mut source_dirs = HashMap::<PathBuf, PathBuf>::new();
    for target in &plan.cmake_targets {
        let source_dir = target.cmake_file.parent().ok_or_else(|| {
            format!(
                "CMake target `{}` file `{}` has no parent directory",
                target.target,
                target.cmake_file.display()
            )
        })?;
        let source_dir = normalize_path(source_dir);
        source_dirs
            .entry(source_dir.clone())
            .or_insert_with(|| native_binary_dir(&native_dirs, &source_dir, plan.profile, output));
    }
    for (source_dir, target_binary_dir) in &source_dirs {
        out.push_str(&format!(
            "add_subdirectory({} {})\n",
            cmake_quote(source_dir),
            cmake_quote(target_binary_dir)
        ));
    }
    if !source_dirs.is_empty() {
        out.push('\n');
    }

    match output.kind {
        CmakeOutputKind::Executable => {
            out.push_str(&format!(
                "add_executable(ciel_output {})\n",
                cmake_quote(output.source_path)
            ));
        }
        CmakeOutputKind::SharedLibrary => {
            out.push_str(&format!(
                "add_library(ciel_output SHARED {})\n",
                cmake_quote(output.source_path)
            ));
        }
    }

    if !plan.cmake_targets.is_empty() {
        out.push_str("target_link_libraries(ciel_output PRIVATE\n");
        for target in &plan.cmake_targets {
            out.push_str(&format!("    {}\n", target.target));
        }
        out.push_str(")\n");
    }
    if is_linux_target(output.target_os) || is_macos_target(output.target_os) {
        out.push_str("target_link_libraries(ciel_output PRIVATE m)\n");
    }

    render_profile_options(&mut out, plan.profile, output.target_os);
    render_extra_options(&mut out, "target_compile_options", &output.compile_flags);
    render_extra_options(&mut out, "target_link_options", &output.link_flags);
    render_output_properties(&mut out, output)?;
    Ok(out)
}

fn native_binary_dir(
    dirs: &NativeBinaryDirs<'_>,
    source_dir: &Path,
    profile: BuildProfile,
    output: &CmakeOutput<'_>,
) -> PathBuf {
    match dirs {
        NativeBinaryDirs::Shared => shared_native_binary_dir(source_dir, profile, output),
        NativeBinaryDirs::Local { root } => local_native_binary_dir(root, source_dir, profile),
    }
}

fn shared_native_binary_dir(
    source_dir: &Path,
    profile: BuildProfile,
    output: &CmakeOutput<'_>,
) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    source_dir.hash(&mut hasher);
    profile.hash(&mut hasher);
    output.target_os.hash(&mut hasher);
    output.c_compiler.hash(&mut hasher);
    preferred_cmake_generator().hash(&mut hasher);
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ciel-native")
        .join(std::process::id().to_string())
        .join(output.target_os)
        .join(cmake_build_type(profile).to_ascii_lowercase())
        .join(format!("{:x}", hasher.finish()))
}

fn local_native_binary_dir(root: &Path, source_dir: &Path, profile: BuildProfile) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    source_dir.hash(&mut hasher);
    profile.hash(&mut hasher);
    root.join("native").join(format!("{:x}", hasher.finish()))
}

fn render_profile_options(out: &mut String, profile: BuildProfile, target_os: &str) {
    match profile {
        BuildProfile::Debug => {
            out.push_str("target_compile_definitions(ciel_output PRIVATE CIEL_DEBUG=1)\n");
            out.push_str("target_compile_options(ciel_output PRIVATE -O0)\n");
        }
        BuildProfile::Release => {
            out.push_str("target_compile_definitions(ciel_output PRIVATE CIEL_RELEASE=1)\n");
            if is_linux_target(target_os) {
                out.push_str(
                    "target_compile_options(ciel_output PRIVATE -ffunction-sections -fdata-sections)\n",
                );
                out.push_str("target_link_options(ciel_output PRIVATE -Wl,--gc-sections)\n");
            } else if is_macos_target(target_os) {
                out.push_str("target_link_options(ciel_output PRIVATE -Wl,-dead_strip)\n");
            }
        }
    }
}

fn render_extra_options(out: &mut String, command: &str, flags: &[String]) {
    if flags.is_empty() {
        return;
    }
    out.push_str(&format!("{command}(ciel_output PRIVATE\n"));
    for flag in flags {
        out.push_str(&format!("    {}\n", cmake_quote_raw(flag)));
    }
    out.push_str(")\n");
}

fn render_output_properties(out: &mut String, output: &CmakeOutput<'_>) -> Result<(), String> {
    let output_dir = output
        .output_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let file_name = output
        .output_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "output path `{}` must end with a valid UTF-8 file name",
                output.output_path.display()
            )
        })?;
    out.push_str("set_target_properties(ciel_output PROPERTIES\n");
    out.push_str(&format!(
        "    RUNTIME_OUTPUT_DIRECTORY {}\n",
        cmake_quote(output_dir)
    ));
    out.push_str(&format!(
        "    LIBRARY_OUTPUT_DIRECTORY {}\n",
        cmake_quote(output_dir)
    ));
    out.push_str(&format!(
        "    ARCHIVE_OUTPUT_DIRECTORY {}\n",
        cmake_quote(output_dir)
    ));
    match output.kind {
        CmakeOutputKind::Executable => {
            out.push_str(&format!("    OUTPUT_NAME {}\n", cmake_quote_raw(file_name)));
        }
        CmakeOutputKind::SharedLibrary => {
            let stem = output
                .output_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| {
                    format!(
                        "shared-library output path `{}` must have a valid UTF-8 stem",
                        output.output_path.display()
                    )
                })?;
            let suffix = output
                .output_path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| format!(".{ext}"))
                .unwrap_or_default();
            out.push_str(&format!("    PREFIX \"\"\n"));
            out.push_str(&format!("    OUTPUT_NAME {}\n", cmake_quote_raw(stem)));
            out.push_str(&format!("    SUFFIX {}\n", cmake_quote_raw(&suffix)));
        }
    }
    out.push_str(")\n");
    Ok(())
}

fn cmake_output_build_root(plan: &BuildPlan, output: &CmakeOutput<'_>) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    output.source_path.hash(&mut hasher);
    output.output_path.hash(&mut hasher);
    output.kind.hash(&mut hasher);
    output.c_compiler.hash(&mut hasher);
    output.compile_flags.hash(&mut hasher);
    output.link_flags.hash(&mut hasher);
    output.target_os.hash(&mut hasher);
    plan.profile.hash(&mut hasher);
    plan.cmake_targets.hash(&mut hasher);
    preferred_cmake_generator().hash(&mut hasher);
    let hash = hasher.finish();
    let stem = output
        .output_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("output")
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    output
        .output_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{stem}.ciel-cmake-{hash:x}"))
}

fn reusable_cmake_output_key(plan: &BuildPlan, output: &CmakeOutput<'_>) -> u64 {
    let mut hasher = DefaultHasher::new();
    output.kind.hash(&mut hasher);
    output.c_compiler.hash(&mut hasher);
    output.compile_flags.hash(&mut hasher);
    output.link_flags.hash(&mut hasher);
    output.target_os.hash(&mut hasher);
    plan.profile.hash(&mut hasher);
    plan.cmake_targets.hash(&mut hasher);
    preferred_cmake_generator().hash(&mut hasher);
    hasher.finish()
}

fn reusable_cmake_output_build_root(output: &CmakeOutput<'_>, cache_key: u64) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ciel-output-cache")
        .join(std::process::id().to_string())
        .join(output.target_os)
        .join(format!("{cache_key:x}"))
}

fn cmake_output_path(path: &Path, target_os: &str) -> Result<PathBuf, String> {
    if path.exists() || target_os != "windows" {
        return Ok(path.to_path_buf());
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "output path `{}` must end with a valid UTF-8 file name",
                path.display()
            )
        })?;
    Ok(path.with_file_name(format!("{file_name}.exe")))
}

fn write_if_changed(path: &Path, contents: &str) -> Result<bool, String> {
    if fs::read_to_string(path).is_ok_and(|existing| existing == contents) {
        return Ok(false);
    }
    fs::write(path, contents)
        .map_err(|error| format!("failed to write `{}`: {error}", path.display()))?;
    Ok(true)
}

fn cmake_build_type(profile: BuildProfile) -> &'static str {
    match profile {
        BuildProfile::Debug => "Debug",
        BuildProfile::Release => "Release",
    }
}

fn preferred_cmake_generator() -> Option<&'static str> {
    if *NINJA_AVAILABLE.get_or_init(ninja_available) {
        Some("Ninja")
    } else {
        None
    }
}

static NINJA_AVAILABLE: OnceLock<bool> = OnceLock::new();

fn ninja_available() -> bool {
    Command::new("ninja")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn ciel_runtime_include_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("runtime")
        .join("include")
}

fn format_command_error(label: &str, output: &std::process::Output) -> String {
    format!(
        "{label} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn cmake_quote(path: &Path) -> String {
    cmake_quote_raw(&path.display().to_string())
}

fn cmake_quote_raw(raw: &str) -> String {
    let escaped = raw
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$");
    format!("\"{escaped}\"")
}

fn c_include_quote(path: &Path) -> String {
    let escaped = path
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn is_macos_target(target_os: &str) -> bool {
    target_os == "macos" || target_os == "darwin"
}

fn is_linux_target(target_os: &str) -> bool {
    target_os == "linux"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn cmake_include_flags_use_package_include_dirs_once() {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let runtime_root = repo.join("runtime");
        let atomic_root = repo.join("std/atomic");
        let targets = vec![
            CmakeTarget {
                package_root: runtime_root.clone(),
                cmake_file: runtime_root.join("CMakeLists.txt"),
                target: "ciel_runtime".to_string(),
                requires_allow_native_build: false,
            },
            CmakeTarget {
                package_root: runtime_root.clone(),
                cmake_file: runtime_root.join("CMakeLists.txt"),
                target: "ciel_runtime".to_string(),
                requires_allow_native_build: false,
            },
            CmakeTarget {
                package_root: atomic_root.clone(),
                cmake_file: atomic_root.join("CMakeLists.txt"),
                target: "ciel_std_atomic".to_string(),
                requires_allow_native_build: false,
            },
            CmakeTarget {
                package_root: atomic_root.clone(),
                cmake_file: atomic_root.join("CMakeLists.txt"),
                target: "ciel_std_atomic".to_string(),
                requires_allow_native_build: false,
            },
        ];

        assert_eq!(
            cmake_include_flags(&targets),
            vec![
                format!("-I{}", runtime_root.join("include").display()),
                format!("-I{}", atomic_root.join("include").display()),
            ]
        );
    }

    #[test]
    fn std_native_cmake_uses_driver_supplied_runtime_include_dir() {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cmake_files = [
            repo.join("std/atomic/CMakeLists.txt"),
            repo.join("std/sync/CMakeLists.txt"),
            repo.join("std/crypto/CMakeLists.txt"),
        ];
        for path in cmake_files {
            let contents = fs::read_to_string(&path).unwrap();
            assert!(
                contents.contains("CIEL_RUNTIME_INCLUDE_DIR"),
                "{} must consume driver-supplied CIEL_RUNTIME_INCLUDE_DIR",
                path.display()
            );
            assert!(
                contents.contains("${CMAKE_CURRENT_SOURCE_DIR}/include"),
                "{} must expose its package-owned include directory",
                path.display()
            );
            assert!(
                !contents.contains("../../runtime/include"),
                "{} must not derive runtime include path from package-relative layout",
                path.display()
            );
        }
    }

    #[test]
    fn native_cmake_projects_export_compile_commands() {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut cmake_files = Vec::new();
        for root in ["runtime", "std", "libs"] {
            collect_cmake_files(&repo.join(root), &mut cmake_files);
        }
        assert!(
            !cmake_files.is_empty(),
            "native CMakeLists.txt discovery must find at least one file"
        );
        for path in cmake_files {
            let contents = fs::read_to_string(&path).unwrap();
            assert!(
                contents.contains("CMAKE_EXPORT_COMPILE_COMMANDS"),
                "{} must export compile_commands.json for clangd",
                path.display()
            );
        }
    }

    fn collect_cmake_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                collect_cmake_files(&path, out);
            } else if path
                .file_name()
                .is_some_and(|name| name == "CMakeLists.txt")
            {
                out.push(path);
            }
        }
    }

    #[test]
    fn runtime_subheaders_do_not_forward_to_umbrella_header() {
        let include_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("runtime")
            .join("include");
        let runtime_headers = [
            "ciel_base.h",
            "ciel_core.h",
            "ciel_checks.h",
            "ciel_gc.h",
            "ciel_async.h",
            "ciel_actor.h",
            "ciel_net.h",
            "ciel_io.h",
        ];
        let std_native_headers = ["ciel_crypto.h", "ciel_atomic.h", "ciel_sync.h"];
        for header in runtime_headers {
            let contents = fs::read_to_string(include_dir.join(header)).unwrap();
            assert!(
                !contents.contains("#include \"ciel_runtime.h\""),
                "{header} must not include the umbrella header"
            );
        }

        let umbrella = fs::read_to_string(include_dir.join("ciel_runtime.h")).unwrap();
        for header in runtime_headers {
            assert!(
                umbrella.contains(&format!("#include \"{header}\"")),
                "umbrella header must include {header}"
            );
        }
        for header in std_native_headers {
            assert!(
                !umbrella.contains(&format!("#include \"{header}\"")),
                "umbrella header must not include std native header {header}"
            );
        }
    }

    #[test]
    fn std_native_headers_live_with_their_packages() {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let runtime_include_dir = repo.join("runtime").join("include");
        let std_native_headers = [
            ("std/atomic/include/ciel_atomic.h", "ciel_atomic.h"),
            ("std/crypto/include/ciel_crypto.h", "ciel_crypto.h"),
            ("std/sync/include/ciel_sync.h", "ciel_sync.h"),
        ];

        for (package_header, header) in std_native_headers {
            let contents = fs::read_to_string(repo.join(package_header)).unwrap();
            assert!(
                !contents.contains("#include \"ciel_runtime.h\""),
                "{package_header} must not include the umbrella header"
            );
            assert!(
                !runtime_include_dir.join(header).exists(),
                "{header} must be owned by its std package, not runtime/include"
            );
        }
    }

    #[test]
    fn native_build_policy_rejects_user_targets_without_allow_flag() {
        let root = PathBuf::from("/repo/libs/sqlite");
        let mut plan = BuildPlan::new(String::new(), BuildProfile::Debug, false);
        plan.cmake_targets.push(CmakeTarget {
            package_root: root.clone(),
            cmake_file: root.join("CMakeLists.txt"),
            target: "ciel_lib_sqlite".to_string(),
            requires_allow_native_build: true,
        });

        let error = build_cmake_output(
            &plan,
            &CmakeOutput {
                source_path: Path::new("/tmp/input.c"),
                output_path: Path::new("/tmp/output"),
                kind: CmakeOutputKind::Executable,
                c_compiler: "cc",
                compile_flags: Vec::new(),
                link_flags: Vec::new(),
                target_os: "linux",
            },
        )
        .unwrap_err();

        assert!(
            error.contains("--allow-native-build") && error.contains("ciel_lib_sqlite"),
            "{error}"
        );
    }
}
