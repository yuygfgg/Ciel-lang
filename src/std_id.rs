use std::path::Path;

use crate::resolve::{DefId, DefKind, ModuleId, ResolvedProgram};
use crate::types::STD_MESSAGE_CLONE_INTERFACE;

const STD_RESULT_PATH: &str = "std/result.ciel";
const STD_RESULT_CORE_PATH: &str = "std/result/core.ciel";
const STD_ERROR_PATH: &str = "std/error.ciel";
const STD_ERROR_CORE_PATH: &str = "std/error/core.ciel";
const STD_MESSAGE_PATH: &str = "std/message.ciel";
const STD_ACTOR_PATH: &str = "std/actor.ciel";
const STD_META_PATH: &str = "std/meta.ciel";
const STD_ASYNC_PATH: &str = "std/async.ciel";
const STD_ASYNC_TIME_PATH: &str = "std/async_time.ciel";

fn module_path_matches(resolved: &ResolvedProgram, module: ModuleId, suffix: &str) -> bool {
    resolved.modules[module.0].path.ends_with(Path::new(suffix))
}

fn def_matches(
    resolved: &ResolvedProgram,
    def_id: DefId,
    kind: DefKind,
    name: &str,
    suffix: &str,
) -> bool {
    let def = resolved.def(def_id);
    def.name == name && def.kind == kind && module_path_matches(resolved, def.module, suffix)
}

pub fn is_std_result_enum(resolved: &ResolvedProgram, def_id: DefId) -> bool {
    def_matches(resolved, def_id, DefKind::Enum, "Result", STD_RESULT_PATH)
        || def_matches(
            resolved,
            def_id,
            DefKind::Enum,
            "Result",
            STD_RESULT_CORE_PATH,
        )
}

pub fn module_can_see_std_result(resolved: &ResolvedProgram, module: ModuleId) -> bool {
    if module_path_matches(resolved, module, STD_RESULT_PATH) {
        return true;
    }
    matches!(
        resolved.lookup_bare(module, "Result", &[DefKind::Enum]),
        Ok(Some(def_id)) if is_std_result_enum(resolved, def_id)
    )
}

pub fn is_std_error_struct(resolved: &ResolvedProgram, def_id: DefId) -> bool {
    def_matches(resolved, def_id, DefKind::Struct, "Error", STD_ERROR_PATH)
        || def_matches(
            resolved,
            def_id,
            DefKind::Struct,
            "Error",
            STD_ERROR_CORE_PATH,
        )
}

pub fn module_can_see_std_error(resolved: &ResolvedProgram, module: ModuleId) -> bool {
    if module_path_matches(resolved, module, STD_ERROR_PATH)
        || module_path_matches(resolved, module, STD_ERROR_CORE_PATH)
    {
        return true;
    }
    matches!(
        resolved.lookup_bare(module, "Error", &[DefKind::Struct]),
        Ok(Some(def_id)) if is_std_error_struct(resolved, def_id)
    )
}

pub fn is_std_error_interface(
    resolved: &ResolvedProgram,
    def_id: DefId,
    expected_name: &str,
) -> bool {
    def_matches(
        resolved,
        def_id,
        DefKind::Interface,
        expected_name,
        STD_ERROR_PATH,
    ) || def_matches(
        resolved,
        def_id,
        DefKind::Interface,
        expected_name,
        STD_ERROR_CORE_PATH,
    )
}

pub fn is_std_message_interface(
    resolved: &ResolvedProgram,
    def_id: DefId,
    expected_name: &str,
) -> bool {
    def_matches(
        resolved,
        def_id,
        DefKind::Interface,
        expected_name,
        STD_MESSAGE_PATH,
    )
}

pub fn is_std_message_clone_interface(resolved: &ResolvedProgram, def_id: DefId) -> bool {
    is_std_message_interface(resolved, def_id, STD_MESSAGE_CLONE_INTERFACE)
}

pub fn is_std_actor_function(
    resolved: &ResolvedProgram,
    module: ModuleId,
    name: &str,
    expected_name: &str,
) -> bool {
    name == expected_name && module_path_matches(resolved, module, STD_ACTOR_PATH)
}

pub fn is_std_meta_function(
    resolved: &ResolvedProgram,
    module: ModuleId,
    name: &str,
    expected_name: &str,
) -> bool {
    name == expected_name && module_path_matches(resolved, module, STD_META_PATH)
}

pub fn is_std_async_function(
    resolved: &ResolvedProgram,
    module: ModuleId,
    name: &str,
    expected_name: &str,
) -> bool {
    name == expected_name && module_path_matches(resolved, module, STD_ASYNC_PATH)
}

pub fn is_std_async_time_function(
    resolved: &ResolvedProgram,
    module: ModuleId,
    name: &str,
    expected_name: &str,
) -> bool {
    name == expected_name && module_path_matches(resolved, module, STD_ASYNC_TIME_PATH)
}

fn is_nominal_type_def_kind(kind: &DefKind) -> bool {
    matches!(kind, DefKind::Struct | DefKind::Enum)
}

fn nominal_type_name(resolved: &ResolvedProgram, def_id: DefId) -> String {
    let def = resolved.def(def_id);
    let has_same_named_nominal = resolved.defs.iter().any(|other| {
        other.id != def.id && other.name == def.name && is_nominal_type_def_kind(&other.kind)
    });
    if has_same_named_nominal {
        format!("{}__def{}", def.name, def.id.0)
    } else {
        def.name.clone()
    }
}

pub fn is_std_async_type_name(
    resolved: &ResolvedProgram,
    ty_name: &str,
    expected_name: &str,
) -> bool {
    let has_std_def = resolved.defs.iter().any(|def| {
        def.name == expected_name
            && def.kind == DefKind::Struct
            && module_path_matches(resolved, def.module, STD_ASYNC_PATH)
    });
    if has_std_def && ty_name == expected_name {
        return true;
    }
    let std_match = resolved.defs.iter().any(|def| {
        def.name == expected_name
            && def.kind == DefKind::Struct
            && module_path_matches(resolved, def.module, STD_ASYNC_PATH)
            && nominal_type_name(resolved, def.id) == ty_name
    });
    if std_match {
        return true;
    }
    let has_user_nominal = resolved.defs.iter().any(|def| {
        def.name == expected_name
            && is_nominal_type_def_kind(&def.kind)
            && !module_path_matches(resolved, def.module, STD_ASYNC_PATH)
    });
    ty_name == expected_name && !has_user_nominal
}

pub fn is_std_meta_type(resolved: &ResolvedProgram, def_id: DefId, expected_name: &str) -> bool {
    def_matches(
        resolved,
        def_id,
        DefKind::Struct,
        expected_name,
        STD_META_PATH,
    ) || def_matches(
        resolved,
        def_id,
        DefKind::TypeAlias,
        expected_name,
        STD_META_PATH,
    )
}

pub fn is_std_meta_interface(
    resolved: &ResolvedProgram,
    def_id: DefId,
    expected_name: &str,
) -> bool {
    def_matches(
        resolved,
        def_id,
        DefKind::Interface,
        expected_name,
        STD_META_PATH,
    )
}
