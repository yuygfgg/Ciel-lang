use std::path::PathBuf;

use super::{
    manifest::{NativeLinkKind, PackageKind, TargetFilter},
    requirements::{BuildPlan, BuildProfile, CmakeTarget, LinkRequirement},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntheticRuntimePackage {
    pub name: &'static str,
    pub kind: PackageKind,
    pub links: Vec<SyntheticNativeLink>,
    pub pkg_config: Vec<SyntheticPkgConfig>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntheticNativeLink {
    pub kind: NativeLinkKind,
    pub name: &'static str,
    pub when: TargetFilter,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntheticPkgConfig {
    pub name: &'static str,
    pub required: bool,
    pub when: TargetFilter,
}

pub fn build_plan_for_generated_c(
    generated_c: String,
    profile: BuildProfile,
    target_os: &str,
    package_inputs: Vec<PathBuf>,
) -> BuildPlan {
    let mut plan = BuildPlan::new(generated_c, profile);
    plan.cmake_targets
        .extend(synthetic_runtime_cmake_targets(profile, target_os));
    plan.link_requirements
        .extend(synthetic_runtime_link_requirements(target_os));
    plan.package_inputs.extend(package_inputs);
    plan.deduplicate();
    plan
}

pub fn synthetic_runtime_cmake_targets(profile: BuildProfile, target_os: &str) -> Vec<CmakeTarget> {
    let package_root = runtime_package_root();
    let build_dir = runtime_build_dir(profile, target_os);
    vec![CmakeTarget {
        package_root: package_root.clone(),
        cmake_file: package_root.join("CMakeLists.txt"),
        target: "ciel_runtime".to_string(),
        artifact: build_dir.join(static_library_name("ciel_runtime", target_os)),
    }]
}

pub fn runtime_package_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime")
}

fn runtime_build_dir(profile: BuildProfile, target_os: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ciel-runtime")
        .join(target_os)
        .join(profile_dir(profile))
}

fn profile_dir(profile: BuildProfile) -> &'static str {
    match profile {
        BuildProfile::Debug => "debug",
        BuildProfile::Release => "release",
    }
}

fn static_library_name(target: &str, target_os: &str) -> String {
    if target_os == "windows" {
        format!("{target}.lib")
    } else {
        format!("lib{target}.a")
    }
}

pub fn synthetic_runtime_link_requirements(target_os: &str) -> Vec<LinkRequirement> {
    let mut requirements = Vec::new();
    let packages = synthetic_runtime_packages();
    for package in &packages {
        requirements.extend(
            package
                .pkg_config
                .iter()
                .filter(|pkg| pkg.when.matches_target(target_os))
                .map(|pkg| LinkRequirement::PkgConfig {
                    name: pkg.name.to_string(),
                    required: pkg.required,
                }),
        );
    }
    for package in &packages {
        requirements.extend(
            package
                .links
                .iter()
                .filter(|link| link.when.matches_target(target_os))
                .map(|link| match link.kind {
                    NativeLinkKind::System => LinkRequirement::SystemLib {
                        name: link.name.to_string(),
                    },
                    NativeLinkKind::Framework => LinkRequirement::Framework {
                        name: link.name.to_string(),
                    },
                }),
        );
    }
    let mut plan = BuildPlan::new(String::new(), BuildProfile::Debug);
    plan.link_requirements = requirements;
    plan.deduplicate();
    plan.link_requirements
}

pub fn synthetic_runtime_packages() -> Vec<SyntheticRuntimePackage> {
    vec![
        SyntheticRuntimePackage {
            name: "runtime.gc",
            kind: PackageKind::Runtime,
            links: vec![SyntheticNativeLink {
                kind: NativeLinkKind::System,
                name: "pthread",
                when: TargetFilter {
                    os: Some(vec!["linux".to_string(), "macos".to_string()]),
                },
            }],
            pkg_config: vec![SyntheticPkgConfig {
                name: "bdw-gc",
                required: false,
                when: TargetFilter::default(),
            }],
        },
        SyntheticRuntimePackage {
            name: "runtime.crypto_botan",
            kind: PackageKind::Runtime,
            links: Vec::new(),
            pkg_config: vec![SyntheticPkgConfig {
                name: "botan-3",
                required: false,
                when: TargetFilter::default(),
            }],
        },
        SyntheticRuntimePackage {
            name: "runtime.dispatch",
            kind: PackageKind::Runtime,
            links: vec![
                SyntheticNativeLink {
                    kind: NativeLinkKind::System,
                    name: "dispatch",
                    when: TargetFilter {
                        os: Some(vec!["linux".to_string(), "macos".to_string()]),
                    },
                },
                SyntheticNativeLink {
                    kind: NativeLinkKind::System,
                    name: "BlocksRuntime",
                    when: TargetFilter {
                        os: Some(vec!["linux".to_string()]),
                    },
                },
            ],
            pkg_config: Vec::new(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_runtime_requirements_match_current_platform_shape() {
        assert_eq!(
            synthetic_runtime_link_requirements("linux"),
            vec![
                LinkRequirement::PkgConfig {
                    name: "bdw-gc".to_string(),
                    required: false,
                },
                LinkRequirement::PkgConfig {
                    name: "botan-3".to_string(),
                    required: false,
                },
                LinkRequirement::SystemLib {
                    name: "pthread".to_string()
                },
                LinkRequirement::SystemLib {
                    name: "dispatch".to_string()
                },
                LinkRequirement::SystemLib {
                    name: "BlocksRuntime".to_string()
                },
            ]
        );
        assert_eq!(
            synthetic_runtime_link_requirements("macos"),
            vec![
                LinkRequirement::PkgConfig {
                    name: "bdw-gc".to_string(),
                    required: false,
                },
                LinkRequirement::PkgConfig {
                    name: "botan-3".to_string(),
                    required: false,
                },
                LinkRequirement::SystemLib {
                    name: "pthread".to_string()
                },
                LinkRequirement::SystemLib {
                    name: "dispatch".to_string()
                },
            ]
        );
    }

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
        assert!(
            plan.cmake_targets[0]
                .artifact
                .ends_with("windows/release/ciel_runtime.lib")
        );
        assert_eq!(
            plan.link_requirements,
            vec![
                LinkRequirement::PkgConfig {
                    name: "bdw-gc".to_string(),
                    required: false,
                },
                LinkRequirement::PkgConfig {
                    name: "botan-3".to_string(),
                    required: false,
                }
            ]
        );
    }

    #[test]
    fn synthetic_runtime_cmake_target_points_at_packaged_runtime() {
        let targets = synthetic_runtime_cmake_targets(BuildProfile::Debug, "linux");
        assert_eq!(targets.len(), 1);
        assert!(targets[0].package_root.ends_with("runtime"));
        assert!(targets[0].cmake_file.ends_with("runtime/CMakeLists.txt"));
        assert_eq!(targets[0].target, "ciel_runtime");
        assert!(
            targets[0]
                .artifact
                .ends_with("target/ciel-runtime/linux/debug/libciel_runtime.a")
        );
    }
}
