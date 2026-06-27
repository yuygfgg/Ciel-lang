use std::collections::HashMap;

use crate::resolve::{DefId, DefKind, ModuleId, ResolvedProgram};
use crate::types::{META_ARRAY_CHUNK_SIZE, STD_MESSAGE_CLONE_INTERFACE, Ty, unify_ty};

const STD_RESULT_EXPORT: &str = "/std/result";
const STD_RESULT_CORE_EXPORT: &str = "/std/result/core";
const STD_ERROR_EXPORT: &str = "/std/error";
const STD_ERROR_CORE_EXPORT: &str = "/std/error/core";
const STD_MESSAGE_EXPORT: &str = "/std/message";
const STD_RESOURCE_EXPORT: &str = "/std/resource";
const STD_ACTOR_EXPORT: &str = "/std/actor";
const STD_META_EXPORT: &str = "/std/meta";
const STD_ASYNC_EXPORT: &str = "/std/async";
const STD_ASYNC_CORE_EXPORT: &str = "/std/async/core";
const STD_ASYNC_IO_EXPORT: &str = "/std/async_io";
const STD_ASYNC_NET_EXPORT: &str = "/std/async_net";
const STD_ASYNC_TIME_EXPORT: &str = "/std/async_time";

fn module_export_matches(resolved: &ResolvedProgram, module: ModuleId, export: &str) -> bool {
    resolved.modules[module.0].std_export.as_deref() == Some(export)
}

pub fn is_std_module(resolved: &ResolvedProgram, module: ModuleId) -> bool {
    resolved.modules[module.0].std_export.is_some()
}

fn module_export_matches_any(
    resolved: &ResolvedProgram,
    module: ModuleId,
    exports: &[&str],
) -> bool {
    resolved.modules[module.0]
        .std_export
        .as_deref()
        .is_some_and(|actual| exports.contains(&actual))
}

fn module_export_matches_std_async(resolved: &ResolvedProgram, module: ModuleId) -> bool {
    module_export_matches_any(resolved, module, &[STD_ASYNC_EXPORT, STD_ASYNC_CORE_EXPORT])
}

fn def_matches(
    resolved: &ResolvedProgram,
    def_id: DefId,
    kind: DefKind,
    name: &str,
    export: &str,
) -> bool {
    let def = resolved.def(def_id);
    def.name == name && def.kind == kind && module_export_matches(resolved, def.module, export)
}

pub fn is_std_result_enum(resolved: &ResolvedProgram, def_id: DefId) -> bool {
    def_matches(resolved, def_id, DefKind::Enum, "Result", STD_RESULT_EXPORT)
        || def_matches(
            resolved,
            def_id,
            DefKind::Enum,
            "Result",
            STD_RESULT_CORE_EXPORT,
        )
}

pub fn is_std_result_type_name(resolved: &ResolvedProgram, ty_name: &str) -> bool {
    resolved.defs.iter().any(|def| {
        is_std_result_enum(resolved, def.id) && nominal_type_name(resolved, def.id) == ty_name
    })
}

pub fn module_can_see_std_result(resolved: &ResolvedProgram, module: ModuleId) -> bool {
    if module_export_matches(resolved, module, STD_RESULT_EXPORT) {
        return true;
    }
    matches!(
        resolved.lookup_bare(module, "Result", &[DefKind::Enum]),
        Ok(Some(def_id)) if is_std_result_enum(resolved, def_id)
    )
}

pub fn is_std_error_struct(resolved: &ResolvedProgram, def_id: DefId) -> bool {
    def_matches(resolved, def_id, DefKind::Struct, "Error", STD_ERROR_EXPORT)
        || def_matches(
            resolved,
            def_id,
            DefKind::Struct,
            "Error",
            STD_ERROR_CORE_EXPORT,
        )
}

pub fn module_can_see_std_error(resolved: &ResolvedProgram, module: ModuleId) -> bool {
    if module_export_matches_any(resolved, module, &[STD_ERROR_EXPORT, STD_ERROR_CORE_EXPORT]) {
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
        STD_ERROR_EXPORT,
    ) || def_matches(
        resolved,
        def_id,
        DefKind::Interface,
        expected_name,
        STD_ERROR_CORE_EXPORT,
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
        STD_MESSAGE_EXPORT,
    )
}

pub fn is_std_message_clone_interface(resolved: &ResolvedProgram, def_id: DefId) -> bool {
    is_std_message_interface(resolved, def_id, STD_MESSAGE_CLONE_INTERFACE)
}

pub fn is_std_resource_handle_type_name(resolved: &ResolvedProgram, ty_name: &str) -> bool {
    is_std_exported_struct_type_name(resolved, ty_name, STD_RESOURCE_EXPORT, "Handle")
}

pub fn is_std_resource_function(
    resolved: &ResolvedProgram,
    module: ModuleId,
    name: &str,
    expected_name: &str,
) -> bool {
    name == expected_name && module_export_matches(resolved, module, STD_RESOURCE_EXPORT)
}

pub fn is_std_actor_function(
    resolved: &ResolvedProgram,
    module: ModuleId,
    name: &str,
    expected_name: &str,
) -> bool {
    name == expected_name && module_export_matches(resolved, module, STD_ACTOR_EXPORT)
}

pub fn is_std_actor_type(resolved: &ResolvedProgram, def_id: DefId) -> bool {
    def_matches(resolved, def_id, DefKind::Struct, "Actor", STD_ACTOR_EXPORT)
}

pub fn is_std_meta_function(
    resolved: &ResolvedProgram,
    module: ModuleId,
    name: &str,
    expected_name: &str,
) -> bool {
    name == expected_name && module_export_matches(resolved, module, STD_META_EXPORT)
}

pub fn is_std_async_function(
    resolved: &ResolvedProgram,
    module: ModuleId,
    name: &str,
    expected_name: &str,
) -> bool {
    name == expected_name && module_export_matches_std_async(resolved, module)
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
        STD_ASYNC_EXPORT,
    ) || def_matches(
        resolved,
        def_id,
        DefKind::Interface,
        expected_name,
        STD_ASYNC_CORE_EXPORT,
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
    name == expected_name && module_export_matches(resolved, module, STD_ASYNC_TIME_EXPORT)
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
            && module_export_matches_std_async(resolved, def.module)
    });
    if has_std_def && ty_name == expected_name {
        return true;
    }
    let std_match = resolved.defs.iter().any(|def| {
        def.name == expected_name
            && def.kind == DefKind::Struct
            && module_export_matches_std_async(resolved, def.module)
            && nominal_type_name(resolved, def.id) == ty_name
    });
    if std_match {
        return true;
    }
    let has_user_nominal = resolved.defs.iter().any(|def| {
        def.name == expected_name
            && is_nominal_type_def_kind(&def.kind)
            && !module_export_matches_std_async(resolved, def.module)
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

fn is_std_exported_struct_type_name(
    resolved: &ResolvedProgram,
    ty_name: &str,
    export: &str,
    expected_name: &str,
) -> bool {
    let std_match = resolved.defs.iter().any(|def| {
        def.name == expected_name
            && def.kind == DefKind::Struct
            && module_export_matches(resolved, def.module, export)
            && nominal_type_name(resolved, def.id) == ty_name
    });
    if std_match {
        return true;
    }
    let has_std_def = resolved.defs.iter().any(|def| {
        def.name == expected_name
            && def.kind == DefKind::Struct
            && module_export_matches(resolved, def.module, export)
    });
    let has_user_nominal = resolved.defs.iter().any(|def| {
        def.name == expected_name
            && is_nominal_type_def_kind(&def.kind)
            && !module_export_matches(resolved, def.module, export)
    });
    has_std_def && ty_name == expected_name && !has_user_nominal
}

pub fn is_std_async_resource_runtime_handle_type_name(
    resolved: &ResolvedProgram,
    ty_name: &str,
) -> bool {
    const ASYNC_IO_HANDLES: &[&str] = &["AsyncFd", "AsyncRead", "AsyncWrite"];
    const ASYNC_NET_HANDLES: &[&str] = &[
        "AsyncTcpListener",
        "AsyncTcpStream",
        "AsyncTcpReadHalf",
        "AsyncTcpWriteHalf",
        "AsyncTcpSplit",
        "BufferedStreamReader",
        "AsyncAccept",
        "AsyncConnect",
        "AsyncTcpRead",
        "AsyncTcpWrite",
        "AsyncBufferedRead",
    ];

    ASYNC_IO_HANDLES
        .iter()
        .any(|name| is_std_exported_struct_type_name(resolved, ty_name, STD_ASYNC_IO_EXPORT, name))
        || ASYNC_NET_HANDLES.iter().any(|name| {
            is_std_exported_struct_type_name(resolved, ty_name, STD_ASYNC_NET_EXPORT, name)
        })
        || is_std_exported_struct_type_name(resolved, ty_name, STD_ASYNC_TIME_EXPORT, "AsyncSleep")
}

pub fn is_std_async_runtime_handle_ty(resolved: &ResolvedProgram, ty: &Ty) -> bool {
    let Ty::Named { name, args } = ty else {
        return false;
    };
    if args.len() == 1 {
        return is_std_async_future_type_name(resolved, name)
            || is_std_async_task_type_name(resolved, name)
            || is_std_async_sender_type_name(resolved, name)
            || is_std_async_receiver_type_name(resolved, name)
            || is_std_async_send_permit_type_name(resolved, name)
            || is_std_async_channel_pair_type_name(resolved, name)
            || is_std_async_task_group_type_name(resolved, name);
    }
    args.is_empty() && is_std_async_resource_runtime_handle_type_name(resolved, name)
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
        STD_META_EXPORT,
    ) || def_matches(
        resolved,
        def_id,
        DefKind::TypeAlias,
        expected_name,
        STD_META_EXPORT,
    )
}

pub fn is_std_meta_sop_node_name(name: &str) -> bool {
    matches!(
        name,
        "HNil"
            | "HCons"
            | "FieldRef"
            | "Field"
            | "PayloadRef"
            | "Payload"
            | "CoNil"
            | "Coproduct"
            | "VariantRef"
            | "Variant"
            | "ArrayNil"
            | "ArrayCat"
    ) || name
        .strip_prefix("ArrayChunk")
        .and_then(|suffix| suffix.parse::<usize>().ok())
        .is_some_and(|len| (1..=META_ARRAY_CHUNK_SIZE).contains(&len))
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
        STD_META_EXPORT,
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{
        ast::AstFile,
        resolve::{Def, ResolvedImport, ResolvedModule},
        span::{FileId, Span},
    };

    fn empty_module(
        id: ModuleId,
        path: &str,
        std_export: Option<&str>,
        defs: Vec<DefId>,
    ) -> ResolvedModule {
        ResolvedModule {
            id,
            path: PathBuf::from(path),
            std_export: std_export.map(str::to_string),
            ast: AstFile { items: Vec::new() },
            defs,
            imports: Vec::<ResolvedImport>::new(),
        }
    }

    fn test_def(id: DefId, module: ModuleId, name: &str, kind: DefKind) -> Def {
        Def {
            id,
            module,
            name: name.to_string(),
            kind,
            parent: None,
            exported: true,
            span: Span::new(FileId(0), 0, 0),
        }
    }

    #[test]
    fn std_identity_uses_manifest_export_not_source_path() {
        let export_module = ModuleId(0);
        let path_only_module = ModuleId(1);
        let export_def = DefId(0);
        let path_only_def = DefId(1);
        let resolved = ResolvedProgram {
            modules: vec![
                empty_module(
                    export_module,
                    "/repo/packages/result/result.ciel",
                    Some(STD_RESULT_EXPORT),
                    vec![export_def],
                ),
                empty_module(
                    path_only_module,
                    "/repo/std/result/result.ciel",
                    None,
                    vec![path_only_def],
                ),
            ],
            defs: vec![
                test_def(export_def, export_module, "Result", DefKind::Enum),
                test_def(path_only_def, path_only_module, "Result", DefKind::Enum),
            ],
            impls: Vec::new(),
        };

        assert!(is_std_result_enum(&resolved, export_def));
        assert!(!is_std_result_enum(&resolved, path_only_def));
    }
}
