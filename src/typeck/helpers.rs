use super::*;

pub(super) fn pattern_span(pattern: &Pattern) -> crate::span::Span {
    match pattern {
        Pattern::Variant(name, _) => name.span,
        Pattern::Wildcard(span) => *span,
    }
}

pub(super) fn nullable_narrowings_from_condition(cond: &TExpr, truth: bool) -> Vec<(LocalId, Ty)> {
    match &cond.kind {
        TExprKind::Binary { op, left, right } if matches!(op, BinaryOp::Eq | BinaryOp::Ne) => {
            let should_narrow =
                (matches!(op, BinaryOp::Ne) && truth) || (matches!(op, BinaryOp::Eq) && !truth);
            if !should_narrow {
                return Vec::new();
            }
            nullable_comparison_local(left, right)
                .or_else(|| nullable_comparison_local(right, left))
                .into_iter()
                .collect()
        }
        TExprKind::Binary {
            op: BinaryOp::And,
            left,
            right,
        } if truth => {
            let mut narrowings = nullable_narrowings_from_condition(left, true);
            narrowings.extend(nullable_narrowings_from_condition(right, true));
            narrowings
        }
        _ => Vec::new(),
    }
}

pub(super) fn nullable_comparison_local(candidate: &TExpr, other: &TExpr) -> Option<(LocalId, Ty)> {
    if !matches!(other.kind, TExprKind::Literal(Literal::Null)) {
        return None;
    }
    let TExprKind::Local(local_id, _) = candidate.kind else {
        return None;
    };
    let Ty::Pointer {
        nullable: true,
        mutability,
        inner,
    } = &candidate.ty
    else {
        return None;
    };
    Some((
        local_id,
        Ty::Pointer {
            nullable: false,
            mutability: *mutability,
            inner: inner.clone(),
        },
    ))
}

pub(super) fn bool_literal_is(expr: &TExpr, expected: bool) -> bool {
    matches!(expr.kind, TExprKind::Literal(Literal::Bool(value)) if value == expected)
}

pub(super) fn expr_is_closure_literal(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Closure { .. } => true,
        ExprKind::Cast { expr, .. } => expr_is_closure_literal(expr),
        _ => false,
    }
}

pub(super) fn lvalue_root_local(expr: &TExpr) -> Option<(LocalId, &str)> {
    match &expr.kind {
        TExprKind::Local(local_id, name) => Some((*local_id, name.as_str())),
        TExprKind::Field { base, .. } | TExprKind::Index { base, .. } => lvalue_root_local(base),
        _ => None,
    }
}

pub(super) fn receiver_selector_path_display(selector: &[crate::ast::Ident]) -> String {
    match selector {
        [name] => format!(".{}", name.name),
        [alias, name] => format!(".{}::{}", alias.name, name.name),
        _ => ".<invalid>".to_string(),
    }
}

pub(super) fn receiver_selector_desugared_args(
    receiver: &Expr,
    args: &[Expr],
    param_len: usize,
    receiver_index: usize,
    adaptation: ReceiverAdaptation,
) -> Vec<Expr> {
    let receiver_arg = match adaptation {
        ReceiverAdaptation::Direct => receiver.clone(),
        ReceiverAdaptation::Address => Expr {
            span: receiver.span,
            kind: ExprKind::Unary {
                op: UnaryOp::Addr,
                expr: Box::new(receiver.clone()),
            },
        },
    };
    let mut explicit = args.iter();
    let mut out = Vec::new();
    for idx in 0..param_len {
        if idx == receiver_index {
            out.push(receiver_arg.clone());
        } else if let Some(arg) = explicit.next() {
            out.push(arg.clone());
        }
    }
    out.extend(explicit.cloned());
    out
}

pub(super) fn receiver_selector_adaptation(
    param_ty: &Ty,
    receiver_ty: &Ty,
) -> Option<ReceiverAdaptation> {
    let mut subst = HashMap::new();
    if selector_pattern_assignable(param_ty, receiver_ty, &mut subst) {
        return Some(ReceiverAdaptation::Direct);
    }
    let Ty::Pointer { inner, .. } = param_ty else {
        return None;
    };
    let mut subst = HashMap::new();
    if selector_pattern_assignable(inner, receiver_ty, &mut subst) {
        Some(ReceiverAdaptation::Address)
    } else {
        None
    }
}

pub(super) fn selector_pattern_assignable(
    expected: &Ty,
    actual: &Ty,
    subst: &mut HashMap<String, Ty>,
) -> bool {
    let mut trial = subst.clone();
    if unify_ty(expected, actual, &mut trial) {
        *subst = trial;
        return true;
    }
    match (expected, actual) {
        (
            Ty::Pointer {
                nullable: expected_nullable,
                mutability: expected_mutability,
                inner: expected_inner,
            },
            Ty::Pointer {
                nullable: actual_nullable,
                mutability: actual_mutability,
                inner: actual_inner,
            },
        ) if (*expected_nullable == *actual_nullable
            || (*expected_nullable && !*actual_nullable))
            && pointer_view_can_weaken(*expected_mutability, *actual_mutability) =>
        {
            selector_pattern_assignable(expected_inner, actual_inner, subst)
        }
        (
            Ty::Slice {
                mutability: expected_mutability,
                elem: expected_elem,
            },
            Ty::Slice {
                mutability: actual_mutability,
                elem: actual_elem,
            },
        ) if pointer_view_can_weaken(*expected_mutability, *actual_mutability) => {
            selector_pattern_assignable(expected_elem, actual_elem, subst)
        }
        _ => false,
    }
}

pub(super) fn receiver_selector_root_ty(ty: &Ty) -> Ty {
    match ty {
        Ty::Pointer { inner, .. } => receiver_selector_root_ty(inner),
        other => other.clone(),
    }
}

pub(super) fn dedup_receiver_selectors(selectors: &mut Vec<ReceiverSelectorSig>) {
    let mut seen = HashSet::<(String, ModuleId, usize, &'static str)>::new();
    selectors.retain(|selector| {
        let (id, kind) = match selector.callable {
            ReceiverSelectorCallable::Function(def_id) => (def_id.0, "function"),
            ReceiverSelectorCallable::Interface(def_id) => (def_id.0, "interface"),
        };
        seen.insert((selector.selector.clone(), selector.module, id, kind))
    });
}

pub(super) fn dedup_modules(modules: &mut Vec<ModuleId>) {
    let mut seen = HashSet::new();
    modules.retain(|module| seen.insert(*module));
}

pub(super) fn enum_instance_name(name: &str, args: &[Ty]) -> String {
    aggregate_instance_name(name, args)
}

pub(super) fn unify_receiver_param(
    pattern: &Ty,
    actual: &Ty,
    subst: &mut HashMap<String, Ty>,
) -> bool {
    match pattern {
        Ty::Pointer { inner, .. } => match actual {
            Ty::Pointer {
                inner: actual_inner,
                ..
            } => unify_ty(inner, actual_inner, subst),
            _ => unify_ty(inner, actual, subst),
        },
        _ => unify_ty(pattern, actual, subst),
    }
}

pub(super) fn hir_type_contains_hole(ty: &Type) -> bool {
    match &ty.kind {
        TypeKind::Hole => true,
        TypeKind::Pointer { inner, .. } | TypeKind::Slice { elem: inner, .. } => {
            hir_type_contains_hole(inner)
        }
        TypeKind::Array { elem, .. } => hir_type_contains_hole(elem),
        TypeKind::Named(_, args) => args.iter().any(hir_type_contains_hole),
        TypeKind::Function { ret, params, .. } | TypeKind::Closure { ret, params, .. } => {
            hir_type_contains_hole(ret) || params.iter().any(hir_type_contains_hole)
        }
        _ => false,
    }
}

pub(super) fn hir_type_contains_generic(ty: &Type) -> bool {
    match &ty.kind {
        TypeKind::Named(name, args) => {
            matches!(name.kind, TypeNameKind::Generic(_))
                || args.iter().any(hir_type_contains_generic)
        }
        TypeKind::Pointer { inner, .. } | TypeKind::Slice { elem: inner, .. } => {
            hir_type_contains_generic(inner)
        }
        TypeKind::Array { elem, .. } => hir_type_contains_generic(elem),
        TypeKind::Function { ret, params, .. } | TypeKind::Closure { ret, params, .. } => {
            hir_type_contains_generic(ret) || params.iter().any(hir_type_contains_generic)
        }
        _ => false,
    }
}

pub(super) fn type_contains_plain_never_value(ty: &Ty) -> bool {
    match ty {
        Ty::Never => true,
        Ty::Array { elem, .. } | Ty::Slice { elem, .. } => type_contains_plain_never_value(elem),
        Ty::GeneratedFuture { output, .. } => type_contains_plain_never_value(output),
        Ty::Function { params, .. }
        | Ty::Closure { params, .. }
        | Ty::ClosureInstance { params, .. } => params.iter().any(type_contains_plain_never_value),
        Ty::Pointer { .. }
        | Ty::Named { .. }
        | Ty::DynamicInterface { .. }
        | Ty::OpaqueReturn { .. }
        | Ty::Hole(_)
        | Ty::Generic(_)
        | Ty::Void
        | Ty::Bool
        | Ty::Char
        | Ty::I8
        | Ty::I16
        | Ty::I32
        | Ty::I64
        | Ty::U8
        | Ty::U16
        | Ty::U32
        | Ty::U64
        | Ty::Usize
        | Ty::F32
        | Ty::F64
        | Ty::CSpelling { .. }
        | Ty::Unknown => false,
    }
}

pub(super) fn type_contains_closure(ty: &Ty) -> bool {
    match ty {
        Ty::Closure { .. } | Ty::ClosureInstance { .. } => true,
        Ty::Pointer { inner, .. } => type_contains_closure(inner),
        Ty::Array { elem, .. } | Ty::Slice { elem, .. } => type_contains_closure(elem),
        Ty::GeneratedFuture { output, .. } => type_contains_closure(output),
        Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
            args.iter().any(type_contains_closure)
        }
        Ty::OpaqueReturn { key, bounds } => {
            key.args.iter().any(type_contains_closure)
                || bounds
                    .positive
                    .iter()
                    .chain(bounds.negative.iter())
                    .any(|entry| entry.args.iter().any(type_contains_closure))
        }
        Ty::Function { ret, params, .. } => {
            type_contains_closure(ret) || params.iter().any(type_contains_closure)
        }
        Ty::Never
        | Ty::Hole(_)
        | Ty::Void
        | Ty::Bool
        | Ty::Char
        | Ty::I8
        | Ty::I16
        | Ty::I32
        | Ty::I64
        | Ty::U8
        | Ty::U16
        | Ty::U32
        | Ty::U64
        | Ty::Usize
        | Ty::F32
        | Ty::F64
        | Ty::CSpelling { .. }
        | Ty::Generic(_)
        | Ty::Unknown => false,
    }
}

pub(super) fn parse_integer_literal_u128(raw: &str) -> Option<u128> {
    let normalized = raw.replace('_', "");
    if let Some(hex) = normalized
        .strip_prefix("0x")
        .or_else(|| normalized.strip_prefix("0X"))
    {
        u128::from_str_radix(hex, 16).ok()
    } else {
        normalized.parse::<u128>().ok()
    }
}

pub(super) fn integer_abs_limits(ty: &Ty) -> Option<(u128, u128)> {
    Some(match ty {
        Ty::I8 => (128, 127),
        Ty::I16 => (32768, 32767),
        Ty::I32 => (2147483648, 2147483647),
        Ty::I64 => (9223372036854775808, 9223372036854775807),
        Ty::Char => (0, u8::MAX as u128),
        Ty::U8 => (0, u8::MAX as u128),
        Ty::U16 => (0, u16::MAX as u128),
        Ty::U32 => (0, u32::MAX as u128),
        Ty::U64 => (0, u64::MAX as u128),
        Ty::Usize => (0, usize::MAX as u128),
        _ => return None,
    })
}

pub(super) fn decode_char_literal_byte(raw: &str) -> Option<u8> {
    let inner = raw.strip_prefix('\'')?.strip_suffix('\'')?;
    if let Some(escaped) = inner.strip_prefix('\\') {
        return match escaped {
            "'" => Some(b'\''),
            "\"" => Some(b'"'),
            "\\" => Some(b'\\'),
            "0" => Some(0),
            "n" => Some(b'\n'),
            "r" => Some(b'\r'),
            "t" => Some(b'\t'),
            hex if hex.starts_with('x') && hex.len() == 3 => u8::from_str_radix(&hex[1..], 16).ok(),
            _ => None,
        };
    }
    let mut chars = inner.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    (ch as u32 <= u8::MAX as u32).then_some(ch as u8)
}

pub(super) fn interface_non_receiver_args(args: &[Ty]) -> &[Ty] {
    if args.is_empty() { args } else { &args[1..] }
}

pub(super) fn ty_generic_names(ty: &Ty) -> HashSet<String> {
    let mut names = HashSet::new();
    collect_ty_generic_names(ty, &mut names);
    names
}

pub(super) fn collect_ty_generic_names(ty: &Ty, names: &mut HashSet<String>) {
    match ty {
        Ty::Generic(name) => {
            names.insert(name.clone());
        }
        Ty::Pointer { inner, .. } => collect_ty_generic_names(inner, names),
        Ty::Array { elem, .. } | Ty::Slice { elem, .. } => collect_ty_generic_names(elem, names),
        Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
            for arg in args {
                collect_ty_generic_names(arg, names);
            }
        }
        Ty::Function { ret, params, .. } | Ty::Closure { ret, params, .. } => {
            collect_ty_generic_names(ret, names);
            for param in params {
                collect_ty_generic_names(param, names);
            }
        }
        Ty::ClosureInstance {
            ret,
            params,
            captures,
            ..
        } => {
            collect_ty_generic_names(ret, names);
            for param in params {
                collect_ty_generic_names(param, names);
            }
            for capture in captures {
                collect_ty_generic_names(capture, names);
            }
        }
        Ty::GeneratedFuture { output, .. } => collect_ty_generic_names(output, names),
        _ => {}
    }
}

pub(super) fn known_ty_matches(left: &Ty, right: &Ty) -> bool {
    match (left, right) {
        (Ty::Generic(left), Ty::Generic(right)) => left == right,
        (
            Ty::Pointer {
                nullable,
                mutability,
                inner,
            },
            Ty::Pointer {
                nullable: right_nullable,
                mutability: right_mutability,
                inner: right_inner,
            },
        ) => {
            nullable == right_nullable
                && mutability == right_mutability
                && known_ty_matches(inner, right_inner)
        }
        (
            Ty::Array { len, elem },
            Ty::Array {
                len: right_len,
                elem: right_elem,
            },
        ) => len == right_len && known_ty_matches(elem, right_elem),
        (
            Ty::Slice { mutability, elem },
            Ty::Slice {
                mutability: right_mutability,
                elem: right_elem,
            },
        ) => mutability == right_mutability && known_ty_matches(elem, right_elem),
        (
            Ty::Named { name, args },
            Ty::Named {
                name: right_name,
                args: right_args,
            },
        )
        | (
            Ty::DynamicInterface { name, args },
            Ty::DynamicInterface {
                name: right_name,
                args: right_args,
            },
        ) => {
            name == right_name
                && args.len() == right_args.len()
                && args
                    .iter()
                    .zip(right_args.iter())
                    .all(|(left, right)| known_ty_matches(left, right))
        }
        (
            Ty::Function {
                is_unsafe,
                abi,
                ret,
                params,
            },
            Ty::Function {
                is_unsafe: right_is_unsafe,
                abi: right_abi,
                ret: right_ret,
                params: right_params,
            },
        ) => {
            is_unsafe == right_is_unsafe
                && abi == right_abi
                && params.len() == right_params.len()
                && known_ty_matches(ret, right_ret)
                && params
                    .iter()
                    .zip(right_params.iter())
                    .all(|(left, right)| known_ty_matches(left, right))
        }
        _ => left == right,
    }
}

pub(super) fn opaque_return_concrete_ty_is_recursive(
    opaque_ty: &Ty,
    concrete_ty: &Ty,
    opaque_returns: &HashMap<OpaqueReturnKey, Ty>,
) -> bool {
    let Ty::OpaqueReturn { key, .. } = opaque_ty else {
        return false;
    };
    let mut seen = HashSet::new();
    opaque_return_ty_reaches_via_lowering(key, concrete_ty, opaque_returns, &mut seen)
}

pub(super) fn opaque_return_ty_reaches_via_lowering(
    target: &OpaqueReturnKey,
    ty: &Ty,
    opaque_returns: &HashMap<OpaqueReturnKey, Ty>,
    seen: &mut HashSet<OpaqueReturnKey>,
) -> bool {
    match ty {
        Ty::Pointer { inner, .. } => {
            opaque_return_ty_reaches_via_lowering(target, inner, opaque_returns, seen)
        }
        Ty::Array { elem, .. } | Ty::Slice { elem, .. } => {
            opaque_return_ty_reaches_via_lowering(target, elem, opaque_returns, seen)
        }
        Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => args
            .iter()
            .any(|arg| opaque_return_ty_reaches_via_lowering(target, arg, opaque_returns, seen)),
        Ty::GeneratedFuture { output, .. } => {
            opaque_return_ty_reaches_via_lowering(target, output, opaque_returns, seen)
        }
        Ty::OpaqueReturn { key, .. } => {
            if key == target {
                return true;
            }
            if !seen.insert(key.clone()) {
                return false;
            }
            let reaches = opaque_returns.get(key).is_some_and(|concrete| {
                opaque_return_ty_reaches_via_lowering(target, concrete, opaque_returns, seen)
            });
            seen.remove(key);
            reaches
        }
        Ty::Function { ret, params, .. } | Ty::Closure { ret, params, .. } => {
            opaque_return_ty_reaches_via_lowering(target, ret, opaque_returns, seen)
                || params.iter().any(|param| {
                    opaque_return_ty_reaches_via_lowering(target, param, opaque_returns, seen)
                })
        }
        Ty::ClosureInstance {
            ret,
            params,
            captures,
            ..
        } => {
            opaque_return_ty_reaches_via_lowering(target, ret, opaque_returns, seen)
                || params.iter().any(|param| {
                    opaque_return_ty_reaches_via_lowering(target, param, opaque_returns, seen)
                })
                || captures.iter().any(|capture| {
                    opaque_return_ty_reaches_via_lowering(target, capture, opaque_returns, seen)
                })
        }
        Ty::Hole(_)
        | Ty::Never
        | Ty::Void
        | Ty::Bool
        | Ty::Char
        | Ty::I8
        | Ty::I16
        | Ty::I32
        | Ty::I64
        | Ty::U8
        | Ty::U16
        | Ty::U32
        | Ty::U64
        | Ty::Usize
        | Ty::F32
        | Ty::F64
        | Ty::CSpelling { .. }
        | Ty::Generic(_)
        | Ty::Unknown => false,
    }
}

pub(super) fn interface_generic_placeholder(interface_name: &str, generic_name: &str) -> String {
    format!("__ciel_iface_{}_{}", interface_name, generic_name)
}

pub(super) fn impl_function_name(interface_name: &str, params: &[Ty]) -> String {
    format!(
        "__impl_{}_{}",
        interface_name,
        params
            .iter()
            .map(mangle_ty_fragment)
            .collect::<Vec<_>>()
            .join("_")
    )
}

pub(super) fn interface_receiver_is_input(interface: &InterfaceSig) -> bool {
    let Some(receiver) = interface.generics.first() else {
        return false;
    };
    interface
        .params
        .iter()
        .any(|param| ast_type_mentions_name(&param.ty, receiver))
}

pub(super) fn ast_type_mentions_name(ty: &Type, name: &str) -> bool {
    match &ty.kind {
        TypeKind::Named(type_name, args) => {
            matches!(&type_name.kind, TypeNameKind::Generic(generic) if generic == name)
                || args.iter().any(|arg| ast_type_mentions_name(arg, name))
        }
        TypeKind::Pointer { inner, .. } => ast_type_mentions_name(inner, name),
        TypeKind::Array { elem, .. } | TypeKind::Slice { elem, .. } => {
            ast_type_mentions_name(elem, name)
        }
        TypeKind::Function { ret, params, .. } => {
            ast_type_mentions_name(ret, name)
                || params
                    .iter()
                    .any(|param| ast_type_mentions_name(param, name))
        }
        TypeKind::Closure {
            ret,
            params,
            constraint,
        } => {
            ast_type_mentions_name(ret, name)
                || params
                    .iter()
                    .any(|param| ast_type_mentions_name(param, name))
                || constraint
                    .as_ref()
                    .is_some_and(|constraint| constraint_expr_mentions_name(constraint, name))
        }
        TypeKind::Hole | TypeKind::Never | TypeKind::Void | TypeKind::Primitive(_) => false,
    }
}

pub(super) fn constraint_expr_mentions_name(expr: &ConstraintExpr, name: &str) -> bool {
    expr.terms.iter().any(|term| {
        term.args.iter().any(|arg| match arg {
            ConstraintArg::Type(ty) => ast_type_mentions_name(ty, name),
            ConstraintArg::Binding {
                name: binding,
                constraint,
                ..
            } => {
                binding.name == name
                    || constraint
                        .as_ref()
                        .is_some_and(|constraint| constraint_expr_mentions_name(constraint, name))
            }
        })
    })
}
