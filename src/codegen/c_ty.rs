use super::*;

pub(super) fn c_base_decl(base: &str, name: &str) -> String {
    if name.is_empty() {
        base.to_string()
    } else {
        format!("{base} {name}")
    }
}

pub(super) fn c_qualified_base(base: &str, top_const: bool) -> String {
    if top_const {
        format!("const {base}")
    } else {
        base.to_string()
    }
}

pub(super) fn c_pointer_name(name: &str, pointer_const: bool, parenthesize: bool) -> String {
    let pointer = if pointer_const {
        if name.is_empty() {
            "* const".to_string()
        } else {
            format!("* const {name}")
        }
    } else {
        format!("*{name}")
    };
    if parenthesize {
        format!("({pointer})")
    } else {
        pointer
    }
}

pub(super) fn c_function_pointer_name(name: &str, pointer_const: bool) -> String {
    if pointer_const {
        if name.is_empty() {
            "(* const)".to_string()
        } else {
            format!("(* const {name})")
        }
    } else {
        format!("(*{name})")
    }
}

pub(super) fn prelude_defines_slice_type(name: &str) -> bool {
    matches!(
        name,
        "CielSlice_u8" | "CielSlice_char" | "CielConstSlice_char"
    )
}

pub(super) fn string_literal_len(raw: &str) -> usize {
    let mut len = 0;
    let mut chars = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw)
        .chars()
        .peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('x') => {
                    chars.next();
                    chars.next();
                    len += 1;
                }
                Some(_) => len += 1,
                None => {}
            }
        } else {
            len += 1;
        }
    }
    len
}

pub(super) fn span_key(span: crate::span::Span) -> (usize, usize, usize) {
    (span.file.0, span.start, span.end)
}

pub(super) fn expr_needs_stmt_lowering(expr: &TExpr) -> bool {
    match &expr.kind {
        TExprKind::Try { .. }
        | TExprKind::Slice { .. }
        | TExprKind::Move(_)
        | TExprKind::MakeDynamicInterface { .. }
        | TExprKind::ErrorBox { .. }
        | TExprKind::MetaAsRefRepr { .. }
        | TExprKind::MetaIntoRepr { .. }
        | TExprKind::MetaFromRepr { .. }
        | TExprKind::MetaSchema { .. }
        | TExprKind::ActorSpawn { .. }
        | TExprKind::ActorSend { .. }
        | TExprKind::ActorStop { .. }
        | TExprKind::ActorJoin { .. }
        | TExprKind::FunctionToClosure(_)
        | TExprKind::RetainClosure { .. }
        | TExprKind::Await { .. }
        | TExprKind::AsyncSelect { .. }
        | TExprKind::AsyncBlockOn { .. }
        | TExprKind::AsyncSleep { .. }
        | TExprKind::AsyncSpawn { .. }
        | TExprKind::AsyncTaskCancel { .. }
        | TExprKind::AsyncTaskIsFinished { .. }
        | TExprKind::CloneMessage { .. }
        | TExprKind::UnsafeBlock { .. } => true,
        TExprKind::Unary { expr, .. } | TExprKind::Cast { expr, .. } => {
            expr_needs_stmt_lowering(expr)
        }
        TExprKind::Binary { left, right, .. } => {
            expr_needs_stmt_lowering(left) || expr_needs_stmt_lowering(right)
        }
        TExprKind::Call { callee, args, .. } => {
            expr_needs_stmt_lowering(callee)
                || args
                    .iter()
                    .any(|arg| arg.ty.is_erased_value() || expr_needs_stmt_lowering(arg))
        }
        TExprKind::Closure { captures, .. } => !captures.is_empty(),
        TExprKind::ArrayToSlice(inner) | TExprKind::SliceToConst(inner) => {
            expr_needs_stmt_lowering(inner)
        }
        TExprKind::RawSliceFromPtr { ptr, len, .. } => {
            expr_needs_stmt_lowering(ptr) || expr_needs_stmt_lowering(len)
        }
        TExprKind::DynamicInterfaceCall { receiver, args, .. }
        | TExprKind::RetainedClosureInterfaceCall { receiver, args, .. } => {
            expr_needs_stmt_lowering(receiver)
                || args
                    .iter()
                    .any(|arg| arg.ty.is_erased_value() || expr_needs_stmt_lowering(arg))
        }
        TExprKind::Field { base, .. } | TExprKind::Arrow { base, .. } => {
            expr.ty.is_erased_value() || expr_needs_stmt_lowering(base)
        }
        TExprKind::Index { base, index } => {
            expr_needs_stmt_lowering(base) || expr_needs_stmt_lowering(index)
        }
        TExprKind::TypeSize { .. }
        | TExprKind::TypeAlign { .. }
        | TExprKind::TypeNeedsGcScan { .. } => false,
        TExprKind::StructLiteral { fields, .. } => fields
            .iter()
            .any(|(_, value)| value.ty.is_erased_value() || expr_needs_stmt_lowering(value)),
        TExprKind::EnumLiteral { payload, .. } => payload
            .iter()
            .any(|value| value.ty.is_erased_value() || expr_needs_stmt_lowering(value)),
        TExprKind::ArrayLiteral(elements) => {
            expr.ty.is_erased_value() || elements.iter().any(expr_needs_stmt_lowering)
        }
        TExprKind::ArrayRepeat { element, .. } => {
            expr.ty.is_erased_value() || expr_needs_stmt_lowering(element)
        }
        TExprKind::Local(..)
        | TExprKind::Function(_, _)
        | TExprKind::GenericFunction { .. }
        | TExprKind::Literal(_) => false,
    }
}

pub(super) fn for_stmt_needs_stmt_lowering(
    init: Option<&TForInit>,
    cond: Option<&TExpr>,
    step: Option<&TForInit>,
) -> bool {
    init.is_some_and(for_clause_needs_stmt_lowering)
        || cond.is_some_and(expr_needs_stmt_lowering)
        || step.is_some_and(for_clause_needs_stmt_lowering)
}

pub(super) fn for_clause_needs_stmt_lowering(clause: &TForInit) -> bool {
    match clause {
        TForInit::VarDecl { init, .. } => init.as_ref().is_some_and(expr_needs_stmt_lowering),
        TForInit::Assign { target, value } => {
            expr_needs_stmt_lowering(target) || expr_needs_stmt_lowering(value)
        }
        TForInit::Expr(expr) => expr_needs_stmt_lowering(expr),
    }
}

pub(super) fn escape_c_include(include: &str) -> String {
    include.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(super) fn escape_c_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(super) fn checked_integer_op_helper(op: &str, ty: &Ty) -> Option<String> {
    let prefix = match op {
        "+" => "add",
        "-" => "sub",
        "*" => "mul",
        "/" => "div",
        "%" => "rem",
        _ => return None,
    };
    let suffix = checked_integer_helper_suffix(ty)?;
    Some(format!("ciel_{prefix}_{suffix}"))
}

pub(super) fn checked_integer_helper_suffix(ty: &Ty) -> Option<&'static str> {
    Some(match ty {
        Ty::I8 => "i8",
        Ty::I16 => "i16",
        Ty::I32 => "i32",
        Ty::I64 => "i64",
        Ty::U8 => "u8",
        Ty::U16 => "u16",
        Ty::U32 => "u32",
        Ty::U64 => "u64",
        Ty::Usize => "usize",
        _ => return None,
    })
}

pub(super) fn checked_integer_unary_helper(ty: &Ty) -> Option<String> {
    if ty.is_signed_integer() {
        Some(format!("ciel_neg_{}", checked_integer_helper_suffix(ty)?))
    } else {
        None
    }
}

pub(super) fn shift_integer_op_helper(op: BinaryOp, ty: &Ty) -> Option<String> {
    let prefix = match op {
        BinaryOp::Shl => "shl",
        BinaryOp::Shr => "shr",
        _ => return None,
    };
    Some(format!(
        "ciel_{prefix}_{}",
        checked_integer_helper_suffix(ty)?
    ))
}

pub(super) fn integer_result_cast(ty: &Ty, expr: String) -> String {
    match ty {
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize => {
            format!("(({})({expr}))", c_scalar_type(ty))
        }
        _ => format!("({expr})"),
    }
}

pub(super) fn c_scalar_type(ty: &Ty) -> &'static str {
    match ty {
        Ty::I8 => "int8_t",
        Ty::I16 => "int16_t",
        Ty::I32 => "int32_t",
        Ty::I64 => "int64_t",
        Ty::U8 => "uint8_t",
        Ty::U16 => "uint16_t",
        Ty::U32 => "uint32_t",
        Ty::U64 => "uint64_t",
        Ty::Usize => "size_t",
        _ => unreachable!("not a C scalar integer type"),
    }
}
