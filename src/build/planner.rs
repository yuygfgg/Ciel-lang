use std::path::PathBuf;

use super::{
    package::{LoadedPackageManifest, PackageOrigin},
    requirements::{BuildPlan, BuildProfile, CmakeTarget},
};

#[cfg(test)]
fn build_plan_for_generated_c(
    generated_c: String,
    profile: BuildProfile,
    target_os: &str,
    package_inputs: Vec<PathBuf>,
) -> BuildPlan {
    build_plan_for_generated_c_with_packages(
        generated_c,
        profile,
        false,
        target_os,
        package_inputs,
        &[],
    )
}

pub(crate) fn build_plan_for_generated_c_with_packages(
    generated_c: String,
    profile: BuildProfile,
    allow_native_build: bool,
    target_os: &str,
    package_inputs: Vec<PathBuf>,
    package_manifests: &[LoadedPackageManifest],
) -> BuildPlan {
    let mut plan = BuildPlan::new(generated_c, profile, allow_native_build);
    plan.package_inputs.extend(package_inputs);
    for loaded in package_manifests {
        let requires_allow_native_build = loaded.origin == PackageOrigin::User;
        plan.cmake_targets.extend(
            loaded
                .manifest
                .cmake_targets(target_os, requires_allow_native_build),
        );
        if let Some(path) = &loaded.manifest.manifest_path {
            plan.package_inputs.push(path.clone());
        }
    }
    plan.cmake_targets
        .extend(synthetic_runtime_cmake_targets(target_os));
    plan.deduplicate();
    plan
}

fn synthetic_runtime_cmake_targets(_target_os: &str) -> Vec<CmakeTarget> {
    let package_root = runtime_package_root();
    vec![CmakeTarget {
        package_root: package_root.clone(),
        cmake_file: package_root.join("CMakeLists.txt"),
        target: "ciel_runtime".to_string(),
        requires_allow_native_build: false,
    }]
}

fn runtime_package_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_plan_carries_profile_and_dedupes_inputs() {
        let plan = build_plan_for_generated_c(
            "int main(void) { return 0; }".to_string(),
            BuildProfile::Release,
            "windows",
            vec![PathBuf::from("main.ciel"), PathBuf::from("main.ciel")],
        );

        assert_eq!(plan.profile, BuildProfile::Release);
        assert_eq!(plan.package_inputs, vec![PathBuf::from("main.ciel")]);
        assert_eq!(plan.cmake_targets.len(), 1);
        assert_eq!(plan.cmake_targets[0].target, "ciel_runtime");
    }

    #[test]
    fn loaded_package_manifests_extend_build_plan() {
        let manifest = crate::build::manifest::PackageManifest::parse_str(
            r#"
manifest_version = 1

[package]
name = "std.crypto"
kind = "stdlib"
root = "."

[ciel.exports]
"/std/crypto" = "crypto.ciel"

[[native.cmake]]
path = "CMakeLists.txt"
target = "ciel_std_crypto"
"#,
            "/repo/std/crypto",
        )
        .unwrap();
        let plan = build_plan_for_generated_c_with_packages(
            String::new(),
            BuildProfile::Debug,
            false,
            "linux",
            Vec::new(),
            &[LoadedPackageManifest {
                manifest,
                origin: PackageOrigin::Builtin,
            }],
        );

        assert!(plan.cmake_targets.iter().any(|target| {
            target.target == "ciel_std_crypto"
                && target.cmake_file.ends_with("std/crypto/CMakeLists.txt")
                && !target.requires_allow_native_build
        }));
    }

    #[test]
    fn user_package_native_targets_require_explicit_allow_policy() {
        let manifest = crate::build::manifest::PackageManifest::parse_str(
            r#"
manifest_version = 1

[package]
name = "sqlite"
kind = "library"
root = "."

[ciel.exports]
"/sqlite" = "sqlite.ciel"

[[native.cmake]]
path = "CMakeLists.txt"
target = "ciel_lib_sqlite"
"#,
            "/repo/libs/sqlite",
        )
        .unwrap();
        let plan = build_plan_for_generated_c_with_packages(
            String::new(),
            BuildProfile::Debug,
            true,
            "linux",
            Vec::new(),
            &[LoadedPackageManifest {
                manifest,
                origin: PackageOrigin::User,
            }],
        );

        assert!(plan.allow_native_build);
        assert!(plan.cmake_targets.iter().any(|target| {
            target.target == "ciel_lib_sqlite" && target.requires_allow_native_build
        }));
    }

    #[test]
    fn synthetic_runtime_cmake_target_points_at_packaged_runtime() {
        let targets = synthetic_runtime_cmake_targets("linux");
        assert_eq!(targets.len(), 1);
        assert!(targets[0].package_root.ends_with("runtime"));
        assert!(targets[0].cmake_file.ends_with("runtime/CMakeLists.txt"));
        assert_eq!(targets[0].target, "ciel_runtime");
    }
}
