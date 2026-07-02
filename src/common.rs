use std::path::{Component, Path, PathBuf};

use crate::{
    hir::{NameRef, NameRefKind},
    resolve::{DefId, DefKind, ResolvedProgram},
};

pub fn is_nominal_type_def_kind(kind: &DefKind) -> bool {
    matches!(kind, DefKind::Struct | DefKind::Enum)
}

pub fn nominal_type_name(resolved: &ResolvedProgram, def_id: DefId) -> String {
    let def = resolved.def(def_id);
    if resolved.modules[def.module.0].std_export.as_deref() == Some("/std/meta") {
        return def.name.clone();
    }
    let has_same_named_nominal = resolved.defs.iter().any(|other| {
        other.id != def.id && other.name == def.name && is_nominal_type_def_kind(&other.kind)
    });
    if has_same_named_nominal {
        format!("{}__def{}", def.name, def.id.0)
    } else {
        def.name.clone()
    }
}

pub fn name_ref_canonical(resolved: &ResolvedProgram, name: &NameRef) -> String {
    match name.kind {
        NameRefKind::Def(def_id) => resolved.def(def_id).name.clone(),
        _ => name.display.clone(),
    }
}

pub fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}
