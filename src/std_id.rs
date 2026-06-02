use std::{collections::HashMap, path::Path};

use crate::resolve::{DefId, DefKind, ModuleId, ResolvedProgram};
use crate::types::{STD_MESSAGE_CLONE_INTERFACE, Ty, unify_ty};

const STD_RESULT_PATH: &str = "std/result.ciel";
const STD_RESULT_CORE_PATH: &str = "std/result/core.ciel";
const STD_ERROR_PATH: &str = "std/error.ciel";
const STD_ERROR_CORE_PATH: &str = "std/error/core.ciel";
const STD_MESSAGE_PATH: &str = "std/message.ciel";
const STD_ACTOR_PATH: &str = "std/actor.ciel";
const STD_META_PATH: &str = "std/meta.ciel";
const STD_ASYNC_PATH: &str = "std/async.ciel";
const STD_ASYNC_CORE_PATH: &str = "std/async/core.ciel";
const STD_ASYNC_TIME_PATH: &str = "std/async_time.ciel";

fn module_path_matches(resolved: &ResolvedProgram, module: ModuleId, suffix: &str) -> bool {
    resolved.modules[module.0].path.ends_with(Path::new(suffix))
}

fn module_path_matches_std_async(resolved: &ResolvedProgram, module: ModuleId) -> bool {
    module_path_matches(resolved, module, STD_ASYNC_PATH)
        || module_path_matches(resolved, module, STD_ASYNC_CORE_PATH)
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
    name == expected_name && module_path_matches_std_async(resolved, module)
}

pub fn is_std_async_interface(
    resolved: &ResolvedProgram,
    def_id: DefId,
    expected_name: &str,
) -> bool {
    def_matches(
        resolved,
        def_id,
        DefKind::Interface,
        expected_name,
        STD_ASYNC_PATH,
    ) || def_matches(
        resolved,
        def_id,
        DefKind::Interface,
        expected_name,
        STD_ASYNC_CORE_PATH,
    )
}

pub fn has_std_async_interface(resolved: &ResolvedProgram, expected_name: &str) -> bool {
    resolved
        .defs
        .iter()
        .any(|def| is_std_async_interface(resolved, def.id, expected_name))
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StdAsyncType {
    Future,
    Task,
    Sender,
    Receiver,
    SendPermit,
    ChannelPair,
    TaskGroup,
}

impl StdAsyncType {
    fn name(self) -> &'static str {
        match self {
            StdAsyncType::Future => "Future",
            StdAsyncType::Task => "Task",
            StdAsyncType::Sender => "Sender",
            StdAsyncType::Receiver => "Receiver",
            StdAsyncType::SendPermit => "SendPermit",
            StdAsyncType::ChannelPair => "ChannelPair",
            StdAsyncType::TaskGroup => "TaskGroup",
        }
    }
}

pub fn is_std_async_type(
    resolved: &ResolvedProgram,
    ty_name: &str,
    expected: StdAsyncType,
) -> bool {
    let expected_name = expected.name();
    let has_std_def = resolved.defs.iter().any(|def| {
        def.name == expected_name
            && def.kind == DefKind::Struct
            && module_path_matches_std_async(resolved, def.module)
    });
    if has_std_def && ty_name == expected_name {
        return true;
    }
    let std_match = resolved.defs.iter().any(|def| {
        def.name == expected_name
            && def.kind == DefKind::Struct
            && module_path_matches_std_async(resolved, def.module)
            && nominal_type_name(resolved, def.id) == ty_name
    });
    if std_match {
        return true;
    }
    let has_user_nominal = resolved.defs.iter().any(|def| {
        def.name == expected_name
            && is_nominal_type_def_kind(&def.kind)
            && !module_path_matches_std_async(resolved, def.module)
    });
    ty_name == expected_name && !has_user_nominal
}

pub fn is_std_async_future_type_name(resolved: &ResolvedProgram, ty_name: &str) -> bool {
    is_std_async_type(resolved, ty_name, StdAsyncType::Future)
}

pub fn is_std_async_task_type_name(resolved: &ResolvedProgram, ty_name: &str) -> bool {
    is_std_async_type(resolved, ty_name, StdAsyncType::Task)
}

pub fn is_std_async_sender_type_name(resolved: &ResolvedProgram, ty_name: &str) -> bool {
    is_std_async_type(resolved, ty_name, StdAsyncType::Sender)
}

pub fn is_std_async_receiver_type_name(resolved: &ResolvedProgram, ty_name: &str) -> bool {
    is_std_async_type(resolved, ty_name, StdAsyncType::Receiver)
}

pub fn is_std_async_send_permit_type_name(resolved: &ResolvedProgram, ty_name: &str) -> bool {
    is_std_async_type(resolved, ty_name, StdAsyncType::SendPermit)
}

pub fn is_std_async_channel_pair_type_name(resolved: &ResolvedProgram, ty_name: &str) -> bool {
    is_std_async_type(resolved, ty_name, StdAsyncType::ChannelPair)
}

pub fn is_std_async_task_group_type_name(resolved: &ResolvedProgram, ty_name: &str) -> bool {
    is_std_async_type(resolved, ty_name, StdAsyncType::TaskGroup)
}

pub fn is_std_async_runtime_handle_ty(resolved: &ResolvedProgram, ty: &Ty) -> bool {
    let Ty::Named { name, args } = ty else {
        return false;
    };
    args.len() == 1
        && (is_std_async_future_type_name(resolved, name)
            || is_std_async_task_type_name(resolved, name)
            || is_std_async_sender_type_name(resolved, name)
            || is_std_async_receiver_type_name(resolved, name)
            || is_std_async_send_permit_type_name(resolved, name)
            || is_std_async_channel_pair_type_name(resolved, name)
            || is_std_async_task_group_type_name(resolved, name))
}

pub fn std_async_future_output_arg<'a>(resolved: &ResolvedProgram, ty: &'a Ty) -> Option<&'a Ty> {
    let Ty::Named { name, args } = ty else {
        return None;
    };
    if args.len() == 1 && is_std_async_future_type_name(resolved, name) {
        args.first()
    } else {
        None
    }
}

pub fn std_async_task_output_arg<'a>(resolved: &ResolvedProgram, ty: &'a Ty) -> Option<&'a Ty> {
    let Ty::Named { name, args } = ty else {
        return None;
    };
    if args.len() == 1 && is_std_async_task_type_name(resolved, name) {
        args.first()
    } else {
        None
    }
}

pub fn std_async_sender_payload_arg<'a>(resolved: &ResolvedProgram, ty: &'a Ty) -> Option<&'a Ty> {
    let Ty::Named { name, args } = ty else {
        return None;
    };
    if args.len() == 1 && is_std_async_sender_type_name(resolved, name) {
        args.first()
    } else {
        None
    }
}

pub fn std_async_receiver_payload_arg<'a>(
    resolved: &ResolvedProgram,
    ty: &'a Ty,
) -> Option<&'a Ty> {
    let Ty::Named { name, args } = ty else {
        return None;
    };
    if args.len() == 1 && is_std_async_receiver_type_name(resolved, name) {
        args.first()
    } else {
        None
    }
}

pub fn std_async_send_permit_payload_arg<'a>(
    resolved: &ResolvedProgram,
    ty: &'a Ty,
) -> Option<&'a Ty> {
    let Ty::Named { name, args } = ty else {
        return None;
    };
    if args.len() == 1 && is_std_async_send_permit_type_name(resolved, name) {
        args.first()
    } else {
        None
    }
}

pub fn std_async_task_group_payload_arg<'a>(
    resolved: &ResolvedProgram,
    ty: &'a Ty,
) -> Option<&'a Ty> {
    let Ty::Named { name, args } = ty else {
        return None;
    };
    if args.len() == 1 && is_std_async_task_group_type_name(resolved, name) {
        args.first()
    } else {
        None
    }
}

pub fn is_std_async_future_or_task_ty(resolved: &ResolvedProgram, ty: &Ty) -> bool {
    std_async_future_output_arg(resolved, ty).is_some()
        || std_async_task_output_arg(resolved, ty).is_some()
}

pub fn std_async_future_accepts_generated(
    resolved: &ResolvedProgram,
    expected: &Ty,
    actual: &Ty,
) -> bool {
    let Some(expected_output) = std_async_future_output_arg(resolved, expected) else {
        return false;
    };
    let Ty::GeneratedFuture { output, .. } = actual else {
        return false;
    };
    expected_output == output.as_ref()
}

pub fn unify_std_async_future_with_generated(
    resolved: &ResolvedProgram,
    pattern: &Ty,
    actual: &Ty,
    subst: &mut HashMap<String, Ty>,
) -> bool {
    let Some(pattern_output) = std_async_future_output_arg(resolved, pattern) else {
        return false;
    };
    let Ty::GeneratedFuture { output, .. } = actual else {
        return false;
    };
    unify_ty(pattern_output, output, subst)
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
