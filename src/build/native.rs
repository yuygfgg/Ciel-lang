use std::{
    collections::HashSet,
    fs,
    path::Path,
    process::Command,
    sync::{Mutex, OnceLock},
};

use super::requirements::{BuildProfile, CmakeTarget, LinkRequirement};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeBuildFlags {
    pub compile_flags: Vec<String>,
    pub link_flags: Vec<String>,
}

pub fn resolve_native_flags(
    requirements: &[LinkRequirement],
    target_os: &str,
) -> Result<NativeBuildFlags, String> {
    let mut flags = NativeBuildFlags::default();
    for requirement in requirements {
        match requirement {
            LinkRequirement::PkgConfig { name, required } => {
                let args = pkg_config_args(name, *required)?;
                let (compile, link) = split_c_and_link_args(args);
                flags.compile_flags.extend(compile);
                flags.link_flags.extend(link);
            }
            LinkRequirement::SystemLib { name } => {
                add_system_lib(&mut flags, name, target_os);
            }
            LinkRequirement::Framework { name } => {
                flags.link_flags.push("-framework".to_string());
                flags.link_flags.push(name.clone());
            }
        }
    }
    dedupe_non_framework_flags(&mut flags.compile_flags);
    dedupe_non_framework_flags(&mut flags.link_flags);
    Ok(flags)
}

pub fn build_cmake_targets(targets: &[CmakeTarget], profile: BuildProfile) -> Result<(), String> {
    if targets.is_empty() {
        return Ok(());
    }
    let lock = CMAKE_BUILD_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| "runtime CMake build lock was poisoned".to_string())?;
    for target in targets {
        build_cmake_target(target, profile)?;
    }
    Ok(())
}

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

static CMAKE_BUILD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn build_cmake_target(target: &CmakeTarget, profile: BuildProfile) -> Result<(), String> {
    let build_dir = target.artifact.parent().ok_or_else(|| {
        format!(
            "CMake target `{}` artifact `{}` has no parent directory",
            target.target,
            target.artifact.display()
        )
    })?;
    fs::create_dir_all(build_dir)
        .map_err(|error| format!("failed to create `{}`: {error}", build_dir.display()))?;
    let source_dir = target.cmake_file.parent().ok_or_else(|| {
        format!(
            "CMake target `{}` file `{}` has no parent directory",
            target.target,
            target.cmake_file.display()
        )
    })?;
    run_cmake_configure(source_dir, build_dir, profile)?;
    run_cmake_build(build_dir, &target.target, profile)?;
    if !target.artifact.exists() {
        return Err(format!(
            "CMake target `{}` finished but artifact `{}` was not produced",
            target.target,
            target.artifact.display()
        ));
    }
    Ok(())
}

fn run_cmake_configure(
    source_dir: &Path,
    build_dir: &Path,
    profile: BuildProfile,
) -> Result<(), String> {
    let build_type = cmake_build_type(profile);
    let output = Command::new("cmake")
        .arg("-S")
        .arg(source_dir)
        .arg("-B")
        .arg(build_dir)
        .arg(format!("-DCMAKE_BUILD_TYPE={build_type}"))
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

fn cmake_build_type(profile: BuildProfile) -> &'static str {
    match profile {
        BuildProfile::Debug => "Debug",
        BuildProfile::Release => "Release",
    }
}

fn format_command_error(label: &str, output: &std::process::Output) -> String {
    format!(
        "{label} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn add_system_lib(flags: &mut NativeBuildFlags, name: &str, target_os: &str) {
    match name {
        "pthread" => {
            if !is_windows_target(target_os) {
                flags.compile_flags.push("-pthread".to_string());
                flags.link_flags.push("-pthread".to_string());
            }
        }
        "dispatch" => {
            if !is_windows_target(target_os) {
                flags.compile_flags.push("-fblocks".to_string());
                if !is_macos_target(target_os) {
                    flags.link_flags.push("-ldispatch".to_string());
                }
            }
        }
        other => flags.link_flags.push(format!("-l{other}")),
    }
}

fn pkg_config_args(package: &str, required: bool) -> Result<Vec<String>, String> {
    let output = Command::new("pkg-config")
        .arg("--cflags")
        .arg("--libs")
        .arg(package)
        .output();
    match output {
        Ok(output) if output.status.success() => Ok(String::from_utf8(output.stdout)
            .unwrap_or_default()
            .split_whitespace()
            .filter(|arg| !arg.starts_with("-stdlib="))
            .map(str::to_string)
            .collect()),
        Ok(output) if required => Err(format!(
            "pkg-config package `{package}` was required but not found\nstatus: {}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )),
        Err(error) if required => Err(format!(
            "failed to invoke pkg-config for required package `{package}`: {error}"
        )),
        _ => Ok(pkg_config_fallback_args(package)),
    }
}

fn pkg_config_fallback_args(package: &str) -> Vec<String> {
    match package {
        "bdw-gc" => vec!["-lgc".to_string()],
        "botan-3" => vec!["-lbotan-3".to_string()],
        _ => Vec::new(),
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

fn dedupe_non_framework_flags(flags: &mut Vec<String>) {
    let mut out = Vec::with_capacity(flags.len());
    let mut idx = 0;
    while idx < flags.len() {
        if flags[idx] == "-framework" && idx + 1 < flags.len() {
            out.push(flags[idx].clone());
            out.push(flags[idx + 1].clone());
            idx += 2;
            continue;
        }
        if !out.contains(&flags[idx]) {
            out.push(flags[idx].clone());
        }
        idx += 1;
    }
    *flags = out;
}

fn is_macos_target(target_os: &str) -> bool {
    target_os == "macos" || target_os == "darwin"
}

fn is_windows_target(target_os: &str) -> bool {
    target_os == "windows"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn synthetic_dispatch_flags_preserve_current_platform_behavior() {
        let linux = resolve_native_flags(
            &[
                LinkRequirement::SystemLib {
                    name: "pthread".to_string(),
                },
                LinkRequirement::SystemLib {
                    name: "dispatch".to_string(),
                },
                LinkRequirement::SystemLib {
                    name: "BlocksRuntime".to_string(),
                },
            ],
            "linux",
        )
        .unwrap();
        assert!(linux.compile_flags.contains(&"-pthread".to_string()));
        assert!(linux.compile_flags.contains(&"-fblocks".to_string()));
        assert!(linux.link_flags.contains(&"-pthread".to_string()));
        assert!(linux.link_flags.contains(&"-ldispatch".to_string()));
        assert!(linux.link_flags.contains(&"-lBlocksRuntime".to_string()));

        let macos = resolve_native_flags(
            &[LinkRequirement::SystemLib {
                name: "dispatch".to_string(),
            }],
            "macos",
        )
        .unwrap();
        assert_eq!(macos.compile_flags, vec!["-fblocks"]);
        assert!(macos.link_flags.is_empty());
    }

    #[test]
    fn cmake_include_flags_use_package_include_dirs_once() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime");
        let targets = vec![
            CmakeTarget {
                package_root: root.clone(),
                cmake_file: root.join("CMakeLists.txt"),
                target: "ciel_runtime".to_string(),
                artifact: root.join("build/libciel_runtime.a"),
            },
            CmakeTarget {
                package_root: root.clone(),
                cmake_file: root.join("CMakeLists.txt"),
                target: "ciel_runtime".to_string(),
                artifact: root.join("build/libciel_runtime.a"),
            },
        ];

        assert_eq!(
            cmake_include_flags(&targets),
            vec![format!("-I{}", root.join("include").display())]
        );
    }

    #[test]
    fn runtime_subheaders_do_not_forward_to_umbrella_header() {
        let include_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("runtime")
            .join("include");
        let subheaders = [
            "ciel_base.h",
            "ciel_core.h",
            "ciel_checks.h",
            "ciel_gc.h",
            "ciel_async.h",
            "ciel_actor.h",
            "ciel_net.h",
            "ciel_crypto.h",
            "ciel_atomic.h",
            "ciel_sync.h",
            "ciel_io.h",
        ];
        for header in subheaders {
            let contents = fs::read_to_string(include_dir.join(header)).unwrap();
            assert!(
                !contents.contains("#include \"ciel_runtime.h\""),
                "{header} must not include the umbrella header"
            );
        }

        let umbrella = fs::read_to_string(include_dir.join("ciel_runtime.h")).unwrap();
        for header in subheaders {
            assert!(
                umbrella.contains(&format!("#include \"{header}\"")),
                "umbrella header must include {header}"
            );
        }
    }
}
