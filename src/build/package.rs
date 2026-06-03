use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
};

use super::manifest::{PackageKind, PackageManifest};

#[derive(Clone, Debug, Default)]
pub struct PackageIndex {
    manifests: Vec<PackageManifest>,
    by_export: HashMap<String, PathBuf>,
    by_source: HashMap<PathBuf, usize>,
    export_by_source: HashMap<PathBuf, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageLoadError {
    pub path: PathBuf,
    pub message: String,
}

impl PackageIndex {
    pub fn load_std(std_paths: &[PathBuf]) -> Result<Self, Vec<PackageLoadError>> {
        let mut manifest_paths = Vec::new();
        let mut scanned_roots = HashSet::new();
        for std_path in std_paths {
            for root in std_manifest_roots(std_path) {
                if scanned_roots.insert(root.clone()) {
                    collect_manifest_paths(&root, &mut manifest_paths)?;
                }
            }
        }
        manifest_paths.sort();
        manifest_paths.dedup();

        let mut index = Self::default();
        let mut errors = Vec::new();
        for path in manifest_paths {
            match PackageManifest::load(&path) {
                Ok(manifest) if manifest.package.kind == PackageKind::Stdlib => {
                    if let Err(error) = index.insert_manifest(manifest) {
                        errors.push(error);
                    }
                }
                Ok(_) => {}
                Err(error) => errors.push(PackageLoadError {
                    path,
                    message: error.to_string(),
                }),
            }
        }

        if errors.is_empty() {
            Ok(index)
        } else {
            Err(errors)
        }
    }

    pub fn resolve_export(&self, export: &str) -> Option<&Path> {
        self.by_export.get(export).map(PathBuf::as_path)
    }

    pub fn manifest_for_source(&self, source: &Path) -> Option<&PackageManifest> {
        self.by_source
            .get(&normalize_path(source))
            .map(|idx| &self.manifests[*idx])
    }

    pub fn export_for_source(&self, source: &Path) -> Option<&str> {
        self.export_by_source
            .get(&normalize_path(source))
            .map(String::as_str)
    }

    fn insert_manifest(&mut self, manifest: PackageManifest) -> Result<(), PackageLoadError> {
        let idx = self.manifests.len();
        if let Some(ciel) = &manifest.ciel {
            for export in ciel.exports.keys() {
                if let Some(existing) = self.by_export.get(export) {
                    let path = manifest_error_path(&manifest);
                    return Err(PackageLoadError {
                        path,
                        message: format!(
                            "duplicate ciel export `{export}` already maps to `{}`",
                            existing.display()
                        ),
                    });
                }
            }
        }
        if let Some(ciel) = &manifest.ciel {
            for (export, source) in &ciel.exports {
                let source = normalize_path(source);
                self.export_by_source
                    .entry(source.clone())
                    .or_insert_with(|| export.clone());
                self.by_export.insert(export.clone(), source.clone());
                self.by_source.entry(source).or_insert(idx);
            }
        }
        self.manifests.push(manifest);
        Ok(())
    }
}

fn manifest_error_path(manifest: &PackageManifest) -> PathBuf {
    manifest
        .manifest_path
        .clone()
        .unwrap_or_else(|| manifest.package.root.join("ciel.toml"))
}

fn std_manifest_roots(std_path: &Path) -> Vec<PathBuf> {
    let root = normalize_path(std_path);
    let mut roots = Vec::new();
    let std_child = root.join("std");
    if std_child.is_dir() {
        roots.push(std_child);
    }
    if root.file_name().is_some_and(|name| name == "std") && root.is_dir() {
        roots.push(root);
    }
    roots
}

fn collect_manifest_paths(
    root: &Path,
    manifest_paths: &mut Vec<PathBuf>,
) -> Result<(), Vec<PackageLoadError>> {
    let mut errors = Vec::new();
    collect_manifest_paths_inner(root, manifest_paths, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn collect_manifest_paths_inner(
    dir: &Path,
    manifest_paths: &mut Vec<PathBuf>,
    errors: &mut Vec<PackageLoadError>,
) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            errors.push(PackageLoadError {
                path: dir.to_path_buf(),
                message: format!("failed to scan package manifests: {error}"),
            });
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(PackageLoadError {
                    path: dir.to_path_buf(),
                    message: format!("failed to scan package manifest entry: {error}"),
                });
                continue;
            }
        };
        let path = entry.path();
        let file_name = entry.file_name();
        if path.is_dir() {
            if file_name == "build" {
                continue;
            }
            collect_manifest_paths_inner(&path, manifest_paths, errors);
        } else if file_name == "ciel.toml" {
            manifest_paths.push(normalize_path(&path));
        }
    }
}

fn normalize_path(path: &Path) -> PathBuf {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_package_index_loads_shipped_native_manifests() {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let index = PackageIndex::load_std(&[repo]).unwrap();

        assert!(
            index
                .resolve_export("/std/crypto")
                .is_some_and(|path| path.ends_with("std/crypto/crypto.ciel"))
        );
        assert!(
            index
                .manifest_for_source(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("std/atomic/atomic.ciel")
                        .as_path()
                )
                .is_some_and(|manifest| manifest.package.name == "std.atomic")
        );
        assert_eq!(
            index.export_for_source(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("std/atomic/atomic.ciel")
                    .as_path()
            ),
            Some("/std/atomic")
        );
    }

    #[test]
    fn std_package_index_maps_multiple_exports_to_matching_sources() {
        let mut index = PackageIndex::default();
        let manifest = PackageManifest::parse_str(
            r#"
manifest_version = 1

[package]
name = "std.result"
kind = "stdlib"
root = "."

[ciel.exports]
"/std/result" = "result.ciel"
"/std/result/core" = "core.ciel"
"#,
            "/repo/std/result",
        )
        .unwrap();
        index.insert_manifest(manifest).unwrap();

        assert_eq!(
            index.resolve_export("/std/result"),
            Some(Path::new("/repo/std/result/result.ciel"))
        );
        assert_eq!(
            index.resolve_export("/std/result/core"),
            Some(Path::new("/repo/std/result/core.ciel"))
        );
    }

    #[test]
    fn package_index_rejects_duplicate_exports() {
        let mut index = PackageIndex::default();
        let first = PackageManifest::parse_str(
            r#"
manifest_version = 1

[package]
name = "std.first"
kind = "stdlib"
root = "."

[ciel.exports]
"/std/shared" = "first.ciel"
"#,
            "/repo/std/first",
        )
        .unwrap();
        let second = PackageManifest::parse_str(
            r#"
manifest_version = 1

[package]
name = "std.second"
kind = "stdlib"
root = "."

[ciel.exports]
"/std/shared" = "second.ciel"
"#,
            "/repo/std/second",
        )
        .unwrap();

        index.insert_manifest(first).unwrap();
        let error = index.insert_manifest(second).unwrap_err();
        assert!(error.message.contains("duplicate ciel export"), "{error:?}");
    }
}
