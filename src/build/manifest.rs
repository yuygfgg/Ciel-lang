use std::{
    collections::{BTreeMap, HashSet},
    fmt, fs,
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;

use super::requirements::CmakeTarget;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageManifest {
    pub manifest_path: Option<PathBuf>,
    pub package: PackageInfo,
    pub ciel: Option<CielSection>,
    pub native: NativeSection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageInfo {
    pub name: String,
    pub kind: PackageKind,
    pub root: PathBuf,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PackageKind {
    Project,
    Stdlib,
    Runtime,
    Library,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CielSection {
    pub exports: BTreeMap<String, PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeSection {
    pub cmake: Vec<NativeCmake>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeCmake {
    pub cmake_file: PathBuf,
    pub target: String,
    pub when: TargetFilter,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TargetFilter {
    pub os: Option<Vec<String>>,
}

impl TargetFilter {
    pub fn matches_target(&self, target_os: &str) -> bool {
        self.os
            .as_ref()
            .is_none_or(|values| values.iter().any(|value| os_matches(value, target_os)))
    }
}

impl PackageManifest {
    pub fn parse_str(
        source: &str,
        manifest_dir: impl Into<PathBuf>,
    ) -> Result<Self, ManifestError> {
        let raw: RawManifest = toml::from_str(source).map_err(|error| toml_error(source, error))?;
        raw.validate(manifest_dir.into(), None)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let path = path.as_ref();
        let source = fs::read_to_string(path).map_err(|error| ManifestError {
            line: None,
            message: format!("failed to read manifest `{}`: {error}", path.display()),
        })?;
        let manifest_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let raw: RawManifest =
            toml::from_str(&source).map_err(|error| toml_error(&source, error))?;
        raw.validate(manifest_dir, Some(path.to_path_buf()))
    }

    pub fn cmake_targets(&self, target_os: &str) -> Vec<CmakeTarget> {
        self.native
            .cmake
            .iter()
            .filter(|target| target.when.matches_target(target_os))
            .map(|target| CmakeTarget {
                package_root: self.package.root.clone(),
                cmake_file: target.cmake_file.clone(),
                target: target.target.clone(),
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestError {
    pub line: Option<usize>,
    pub message: String,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(line) = self.line {
            write!(f, "line {line}: {}", self.message)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl std::error::Error for ManifestError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    manifest_version: u32,
    package: RawPackage,
    ciel: Option<RawCiel>,
    #[serde(default)]
    native: RawNative,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPackage {
    name: String,
    kind: PackageKind,
    #[serde(default = "default_package_root")]
    root: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCiel {
    exports: BTreeMap<String, String>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNative {
    #[serde(default)]
    cmake: Vec<RawNativeCmake>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNativeCmake {
    path: String,
    target: String,
    #[serde(default)]
    when: TargetFilter,
}

impl RawManifest {
    fn validate(
        self,
        manifest_dir: PathBuf,
        manifest_path: Option<PathBuf>,
    ) -> Result<PackageManifest, ManifestError> {
        if self.manifest_version != 1 {
            return Err(ManifestError {
                line: None,
                message: format!(
                    "unsupported manifest_version {}; expected 1",
                    self.manifest_version
                ),
            });
        }
        validate_package_name(&self.package.name)?;
        let root = clean_relative_path(&self.package.root, "package.root")?;
        let package_root = normalize_joined_path(&manifest_dir.join(root));

        let ciel = self
            .ciel
            .map(|section| {
                if section.exports.is_empty() {
                    return Err(ManifestError {
                        line: None,
                        message: "`ciel.exports` must not be empty".to_string(),
                    });
                }
                let mut exports = BTreeMap::new();
                let mut exported_sources = HashSet::new();
                for (export, path) in section.exports {
                    validate_export_path(&export)?;
                    let field = format!("ciel.exports.{export}");
                    let rel = clean_relative_path(&path, &field)?;
                    if rel.extension().and_then(|ext| ext.to_str()) != Some("ciel") {
                        return Err(ManifestError {
                            line: None,
                            message: format!(
                                "ciel export source `{path}` for `{export}` must use the .ciel extension"
                            ),
                        });
                    }
                    let source = normalize_joined_path(&package_root.join(rel));
                    if !exported_sources.insert(source.clone()) {
                        return Err(ManifestError {
                            line: None,
                            message: format!(
                                "ciel source `{}` must not be exported more than once",
                                source.display()
                            ),
                        });
                    }
                    exports.insert(export, source);
                }
                Ok(CielSection { exports })
            })
            .transpose()?;

        let native = NativeSection {
            cmake: self
                .native
                .cmake
                .into_iter()
                .map(|target| {
                    validate_non_empty(&target.target, "native.cmake.target")?;
                    Ok(NativeCmake {
                        cmake_file: normalize_joined_path(
                            &package_root
                                .join(clean_relative_path(&target.path, "native.cmake.path")?),
                        ),
                        target: target.target,
                        when: target.when,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
        };

        Ok(PackageManifest {
            manifest_path,
            package: PackageInfo {
                name: self.package.name,
                kind: self.package.kind,
                root: package_root,
            },
            ciel,
            native,
        })
    }
}

fn validate_package_name(name: &str) -> Result<(), ManifestError> {
    validate_non_empty(name, "package.name")?;
    for segment in name.split('.') {
        if segment.is_empty() {
            return Err(ManifestError {
                line: None,
                message: "package.name contains an empty segment".to_string(),
            });
        }
        if !segment
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
        {
            return Err(ManifestError {
                line: None,
                message: "package.name must use lowercase ascii segments separated by ."
                    .to_string(),
            });
        }
    }
    Ok(())
}

fn validate_non_empty(value: &str, field: &str) -> Result<(), ManifestError> {
    if value.is_empty() {
        Err(ManifestError {
            line: None,
            message: format!("`{field}` must not be empty"),
        })
    } else {
        Ok(())
    }
}

fn validate_export_path(export: &str) -> Result<(), ManifestError> {
    if !export.starts_with('/') {
        return Err(ManifestError {
            line: None,
            message: format!("ciel export `{export}` must start with /"),
        });
    }
    if export == "/" {
        return Err(ManifestError {
            line: None,
            message: "ciel export `/` must name a module path".to_string(),
        });
    }
    for segment in export.split('/').skip(1) {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(ManifestError {
                line: None,
                message: format!("ciel export `{export}` contains an invalid path segment"),
            });
        }
    }
    Ok(())
}

fn clean_relative_path(raw: &str, field: &str) -> Result<PathBuf, ManifestError> {
    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(ManifestError {
            line: None,
            message: format!("`{field}` must be relative"),
        });
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_os_string()),
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(ManifestError {
                        line: None,
                        message: format!("`{field}` must not escape the package root"),
                    });
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(ManifestError {
                    line: None,
                    message: format!("`{field}` must be relative"),
                });
            }
        }
    }
    let mut out = PathBuf::new();
    for part in parts {
        out.push(part);
    }
    if out.as_os_str().is_empty() {
        Ok(PathBuf::from("."))
    } else {
        Ok(out)
    }
}

fn normalize_joined_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
            Component::RootDir | Component::Prefix(_) => out.push(component.as_os_str()),
        }
    }
    out
}

fn os_matches(filter: &str, target_os: &str) -> bool {
    normalize_os(filter) == normalize_os(target_os)
}

fn normalize_os(os: &str) -> &str {
    match os {
        "darwin" => "macos",
        other => other,
    }
}

fn default_package_root() -> String {
    ".".to_string()
}

fn toml_error(source: &str, error: toml::de::Error) -> ManifestError {
    ManifestError {
        line: error.span().map(|span| line_for_byte(source, span.start)),
        message: error.message().to_string(),
    }
}

fn line_for_byte(source: &str, byte: usize) -> usize {
    source[..byte.min(source.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_manifest_and_filters_cmake_targets() {
        let manifest = PackageManifest::parse_str(
            r#"
manifest_version = 1

[package]
name = "std.async_net"
kind = "stdlib"
root = "."

[ciel.exports]
"/std/async_net" = "async_net.ciel"

[[native.cmake]]
path = "native/CMakeLists.txt"
target = "ciel_std_async_net"
when = { os = ["linux", "macos"] }
"#,
            "/repo/std/async_net",
        )
        .unwrap();

        assert_eq!(manifest.package.name, "std.async_net");
        assert_eq!(manifest.package.kind, PackageKind::Stdlib);
        assert_eq!(
            manifest
                .ciel
                .as_ref()
                .unwrap()
                .exports
                .get("/std/async_net"),
            Some(&PathBuf::from("/repo/std/async_net/async_net.ciel"))
        );

        let cmake = manifest.cmake_targets("darwin");
        assert_eq!(cmake.len(), 1);
        assert_eq!(
            cmake[0].cmake_file,
            PathBuf::from("/repo/std/async_net/native/CMakeLists.txt")
        );
        assert!(manifest.cmake_targets("windows").is_empty());
    }

    #[test]
    fn rejects_manifest_paths_that_escape_package_root() {
        let error = PackageManifest::parse_str(
            r#"
manifest_version = 1

[package]
name = "bad.path"
kind = "library"

[ciel.exports]
"/bad/path" = "../escape.ciel"
"#,
            "/repo/pkg",
        )
        .unwrap_err();

        assert!(
            error.message.contains("must not escape the package root"),
            "{error}"
        );
    }

    #[test]
    fn rejects_multiple_exports_for_the_same_source() {
        let error = PackageManifest::parse_str(
            r#"
manifest_version = 1

[package]
name = "bad.alias"
kind = "library"

[ciel.exports]
"/bad/one" = "same.ciel"
"/bad/two" = "same.ciel"
"#,
            "/repo/pkg",
        )
        .unwrap_err();

        assert!(
            error
                .message
                .contains("must not be exported more than once"),
            "{error}"
        );
    }

    #[test]
    fn package_root_must_not_escape_manifest_directory() {
        let error = PackageManifest::parse_str(
            r#"
manifest_version = 1

[package]
name = "std.crypto"
kind = "stdlib"
root = ".."

[ciel.exports]
"/std/crypto" = "crypto.ciel"
"#,
            "/repo/std/crypto",
        )
        .unwrap_err();

        assert!(
            error
                .message
                .contains("`package.root` must not escape the package root"),
            "{error}"
        );
    }

    #[test]
    fn rejects_invalid_package_names() {
        let error = PackageManifest::parse_str(
            r#"
manifest_version = 1

[package]
name = "Bad.Name"
kind = "library"
"#,
            "/repo/pkg",
        )
        .unwrap_err();

        assert!(
            error.message.contains("lowercase ascii segments"),
            "{error}"
        );
    }

    #[test]
    fn toml_parser_rejects_unknown_manifest_fields() {
        let error = PackageManifest::parse_str(
            r#"
manifest_version = 1
unexpected = true

[package]
name = "ok.name"
kind = "library"
"#,
            "/repo/pkg",
        )
        .unwrap_err();

        assert!(error.message.contains("unknown field"), "{error}");
    }
}
