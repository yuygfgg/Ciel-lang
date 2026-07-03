use std::{
    collections::{HashMap, HashSet},
    fmt,
};

use crate::{
    ast::{PrimitiveType, Type, TypeKind, ViewMutability},
    resolve::DefId,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ConstraintBounds {
    pub positive: Vec<ConstraintRef>,
    pub negative: Vec<ConstraintRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConstraintRef {
    pub def_id: DefId,
    pub name: String,
    pub args: Vec<Ty>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OpaqueReturnKey {
    pub def_id: DefId,
    pub args: Vec<Ty>,
}

pub const STD_ERROR_FORMAT_INTERFACE: &str = "format_error";
pub const STD_ERROR_TRAIT_ALIAS: &str = "ErrorTrait";
pub const STD_ERROR_CODE_TYPE: &str = "CodeError";
pub const STD_MESSAGE_CLONE_INTERFACE: &str = "clone_message";
pub const STD_MESSAGE_SHARE_HANDLE_INTERFACE: &str = "share_handle_marker";
pub const STD_MESSAGE_THREAD_LOCAL_INTERFACE: &str = "thread_local_marker";
pub const STD_MESSAGE_ASYNC_FRAME_OPT_IN_INTERFACE: &str = "async_frame_opt_in_marker";
pub const STD_ASYNC_AWAITABLE_FUTURE_INTERFACE: &str = "awaitable_future";
pub const STD_ASYNC_CANCEL_SAFE_INTERFACE: &str = "cancel_safe_marker";
pub const STD_ASYNC_ABORT_FUTURE_INTERFACE: &str = "abort_future";

pub fn clone_message_capability(def_id: DefId) -> ConstraintRef {
    ConstraintRef {
        def_id,
        name: STD_MESSAGE_CLONE_INTERFACE.to_string(),
        args: Vec::new(),
    }
}

impl ConstraintBounds {
    pub fn is_empty(&self) -> bool {
        self.positive.is_empty() && self.negative.is_empty()
    }

    pub fn proves_all(&self, required: &ConstraintBounds) -> bool {
        required
            .positive
            .iter()
            .all(|capability| self.positive.contains(capability))
            && required
                .negative
                .iter()
                .all(|capability| self.negative.contains(capability))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    Hole(usize),
    Never,
    Void,
    Bool,
    Char,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Usize,
    F32,
    F64,
    CSpelling {
        abi: String,
        spelling: String,
    },
    Pointer {
        nullable: bool,
        mutability: ViewMutability,
        inner: Box<Ty>,
    },
    Array {
        len: usize,
        elem: Box<Ty>,
    },
    Slice {
        mutability: ViewMutability,
        elem: Box<Ty>,
    },
    Named {
        name: String,
        args: Vec<Ty>,
    },
    GeneratedFuture {
        name: String,
        output: Box<Ty>,
        cancel_safe: bool,
        abortable: bool,
        affine_state: bool,
    },
    Generic(String),
    DynamicInterface {
        def_id: DefId,
        name: String,
        args: Vec<Ty>,
    },
    OpaqueReturn {
        key: OpaqueReturnKey,
        bounds: ConstraintBounds,
    },
    Function {
        is_unsafe: bool,
        abi: Option<String>,
        ret: Box<Ty>,
        params: Vec<Ty>,
    },
    Closure {
        ret: Box<Ty>,
        params: Vec<Ty>,
        constraints: ConstraintBounds,
    },
    ClosureInstance {
        id: usize,
        ret: Box<Ty>,
        params: Vec<Ty>,
        captures: Vec<Ty>,
    },
    Unknown,
}

impl Ty {
    pub fn from_ast(ty: &Type) -> Self {
        match &ty.kind {
            TypeKind::Error => Ty::Unknown,
            TypeKind::Never => Ty::Never,
            TypeKind::Hole => Ty::Unknown,
            TypeKind::Void => Ty::Void,
            TypeKind::Primitive(primitive) => match primitive {
                PrimitiveType::Bool => Ty::Bool,
                PrimitiveType::Char => Ty::Char,
                PrimitiveType::I8 => Ty::I8,
                PrimitiveType::I16 => Ty::I16,
                PrimitiveType::I32 => Ty::I32,
                PrimitiveType::I64 => Ty::I64,
                PrimitiveType::U8 => Ty::U8,
                PrimitiveType::U16 => Ty::U16,
                PrimitiveType::U32 => Ty::U32,
                PrimitiveType::U64 => Ty::U64,
                PrimitiveType::Usize => Ty::Usize,
                PrimitiveType::F32 => Ty::F32,
                PrimitiveType::F64 => Ty::F64,
            },
            TypeKind::Named(path, args) => Ty::Named {
                name: path
                    .last()
                    .map(|name| name.name.clone())
                    .unwrap_or_default(),
                args: args.iter().map(Ty::from_ast).collect(),
            },
            TypeKind::Pointer {
                nullable,
                mutability,
                inner,
            } => Ty::Pointer {
                nullable: *nullable,
                mutability: *mutability,
                inner: Box::new(Ty::from_ast(inner)),
            },
            TypeKind::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(Ty::from_ast(elem)),
            },
            TypeKind::Slice { mutability, elem } => Ty::Slice {
                mutability: *mutability,
                elem: Box::new(Ty::from_ast(elem)),
            },
            TypeKind::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => Ty::Function {
                is_unsafe: *is_unsafe,
                abi: abi.clone(),
                ret: Box::new(Ty::from_ast(ret)),
                params: params.iter().map(Ty::from_ast).collect(),
            },
            TypeKind::Closure { ret, params, .. } => Ty::Closure {
                ret: Box::new(Ty::from_ast(ret)),
                params: params.iter().map(Ty::from_ast).collect(),
                constraints: ConstraintBounds::default(),
            },
        }
    }

    pub fn is_integer(&self) -> bool {
        matches!(
            self,
            Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize
        )
    }

    pub fn is_signed_integer(&self) -> bool {
        matches!(self, Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64)
    }

    pub fn integer_bit_width(&self) -> Option<u32> {
        Some(match self {
            Ty::I8 | Ty::U8 => 8,
            Ty::I16 | Ty::U16 => 16,
            Ty::I32 | Ty::U32 => 32,
            Ty::I64 | Ty::U64 => 64,
            Ty::Usize => usize::BITS,
            _ => return None,
        })
    }

    pub fn is_numeric(&self) -> bool {
        self.is_integer() || matches!(self, Ty::F32 | Ty::F64)
    }

    pub fn is_void(&self) -> bool {
        matches!(self, Ty::Void)
    }

    pub fn is_erased_value(&self) -> bool {
        match self {
            Ty::Void => true,
            Ty::Array { elem, .. } => elem.is_erased_value(),
            _ => false,
        }
    }

    pub fn is_never(&self) -> bool {
        matches!(self, Ty::Never)
    }

    pub fn pointer_to(inner: Ty) -> Self {
        Ty::Pointer {
            nullable: false,
            mutability: ViewMutability::Writable,
            inner: Box::new(inner),
        }
    }

    pub fn const_pointer_to(inner: Ty) -> Self {
        Ty::Pointer {
            nullable: false,
            mutability: ViewMutability::ReadOnly,
            inner: Box::new(inner),
        }
    }

    pub fn nullable_pointer_to(inner: Ty) -> Self {
        Ty::Pointer {
            nullable: true,
            mutability: ViewMutability::Writable,
            inner: Box::new(inner),
        }
    }

    pub fn nullable_const_pointer_to(inner: Ty) -> Self {
        Ty::Pointer {
            nullable: true,
            mutability: ViewMutability::ReadOnly,
            inner: Box::new(inner),
        }
    }

    pub fn can_assign_from(&self, source: &Ty) -> bool {
        if matches!(self, Ty::Hole(_)) || matches!(source, Ty::Hole(_)) {
            return true;
        }
        if self == source {
            return true;
        }
        matches!(
            (self, source),
            (
                Ty::Pointer {
                    nullable: true,
                    mutability: expected_mutability,
                    inner: expected,
                },
                Ty::Pointer {
                    nullable: false,
                    mutability: actual_mutability,
                    inner: actual,
                },
            ) if pointer_view_can_weaken(*expected_mutability, *actual_mutability)
                && expected == actual
        ) || matches!(
            (self, source),
            (
                Ty::Pointer {
                    nullable: expected_nullable,
                    mutability: expected_mutability,
                    inner: expected,
                },
                Ty::Pointer {
                    nullable: actual_nullable,
                    mutability: actual_mutability,
                    inner: actual,
                },
        ) if expected_nullable == actual_nullable
                && pointer_view_can_weaken(*expected_mutability, *actual_mutability)
                && expected == actual
        ) || matches!(
            (self, source),
            (
                Ty::Slice {
                    mutability: expected_mutability,
                    elem: expected,
                },
                Ty::Slice {
                    mutability: actual_mutability,
                    elem: actual,
                },
            ) if pointer_view_can_weaken(*expected_mutability, *actual_mutability)
                && expected == actual
        ) || matches!(
            (self, source),
            (
                Ty::Function {
                    is_unsafe: expected_unsafe,
                    abi: expected_abi,
                    ret: expected_ret,
                    params: expected_params,
                },
                Ty::Function {
                    is_unsafe: actual_unsafe,
                    abi: actual_abi,
                    ret: actual_ret,
                    params: actual_params,
                },
            ) if expected_abi == actual_abi
                && expected_params == actual_params
                && expected_ret.can_assign_from(actual_ret)
                && (!actual_unsafe || *expected_unsafe)
        ) || matches!(
            (self, source),
            (
                Ty::Closure {
                    ret: expected_ret,
                    params: expected_params,
                    constraints: expected_constraints,
                },
                Ty::Closure {
                    ret: actual_ret,
                    params: actual_params,
                    constraints: actual_constraints,
                },
            ) if expected_params == actual_params
                && expected_ret.can_assign_from(actual_ret)
                && actual_constraints.proves_all(expected_constraints)
        ) || matches!(
            (self, source),
            (
                Ty::Closure {
                    ret: expected_ret,
                    params: expected_params,
                    constraints: expected_constraints,
                },
                Ty::ClosureInstance {
                    ret: actual_ret,
                    params: actual_params,
                    ..
                },
            ) if expected_constraints.is_empty()
                && expected_params == actual_params
                && expected_ret.can_assign_from(actual_ret)
        )
    }
}

pub fn pointer_view_can_weaken(expected: ViewMutability, actual: ViewMutability) -> bool {
    expected == actual
        || (expected == ViewMutability::ReadOnly && actual == ViewMutability::Writable)
}

pub fn ty_from_primitive(primitive: &PrimitiveType) -> Ty {
    match primitive {
        PrimitiveType::Bool => Ty::Bool,
        PrimitiveType::Char => Ty::Char,
        PrimitiveType::I8 => Ty::I8,
        PrimitiveType::I16 => Ty::I16,
        PrimitiveType::I32 => Ty::I32,
        PrimitiveType::I64 => Ty::I64,
        PrimitiveType::U8 => Ty::U8,
        PrimitiveType::U16 => Ty::U16,
        PrimitiveType::U32 => Ty::U32,
        PrimitiveType::U64 => Ty::U64,
        PrimitiveType::Usize => Ty::Usize,
        PrimitiveType::F32 => Ty::F32,
        PrimitiveType::F64 => Ty::F64,
    }
}

pub fn substitute_ty(ty: &Ty, subst: &HashMap<String, Ty>) -> Ty {
    match ty {
        Ty::Hole(id) => Ty::Hole(*id),
        Ty::Generic(name) => subst
            .get(name)
            .cloned()
            .unwrap_or_else(|| Ty::Generic(name.clone())),
        Ty::Pointer {
            nullable,
            mutability,
            inner,
        } => Ty::Pointer {
            nullable: *nullable,
            mutability: *mutability,
            inner: Box::new(substitute_ty(inner, subst)),
        },
        Ty::Array { len, elem } => Ty::Array {
            len: *len,
            elem: Box::new(substitute_ty(elem, subst)),
        },
        Ty::Slice { mutability, elem } => Ty::Slice {
            mutability: *mutability,
            elem: Box::new(substitute_ty(elem, subst)),
        },
        Ty::Named { name, args } => Ty::Named {
            name: name.clone(),
            args: args.iter().map(|arg| substitute_ty(arg, subst)).collect(),
        },
        Ty::GeneratedFuture {
            name,
            output,
            cancel_safe,
            abortable,
            affine_state,
        } => Ty::GeneratedFuture {
            name: name.clone(),
            output: Box::new(substitute_ty(output, subst)),
            cancel_safe: *cancel_safe,
            abortable: *abortable,
            affine_state: *affine_state,
        },
        Ty::DynamicInterface { def_id, name, args } => Ty::DynamicInterface {
            def_id: *def_id,
            name: name.clone(),
            args: args.iter().map(|arg| substitute_ty(arg, subst)).collect(),
        },
        Ty::OpaqueReturn { key, bounds } => Ty::OpaqueReturn {
            key: OpaqueReturnKey {
                def_id: key.def_id,
                args: key
                    .args
                    .iter()
                    .map(|arg| substitute_ty(arg, subst))
                    .collect(),
            },
            bounds: substitute_constraint_bounds(bounds, subst),
        },
        Ty::Function {
            is_unsafe,
            abi,
            ret,
            params,
        } => Ty::Function {
            is_unsafe: *is_unsafe,
            abi: abi.clone(),
            ret: Box::new(substitute_ty(ret, subst)),
            params: params
                .iter()
                .map(|param| substitute_ty(param, subst))
                .collect(),
        },
        Ty::Closure {
            ret,
            params,
            constraints,
        } => Ty::Closure {
            ret: Box::new(substitute_ty(ret, subst)),
            params: params
                .iter()
                .map(|param| substitute_ty(param, subst))
                .collect(),
            constraints: substitute_constraint_bounds(constraints, subst),
        },
        Ty::ClosureInstance {
            id,
            ret,
            params,
            captures,
        } => Ty::ClosureInstance {
            id: *id,
            ret: Box::new(substitute_ty(ret, subst)),
            params: params
                .iter()
                .map(|param| substitute_ty(param, subst))
                .collect(),
            captures: captures
                .iter()
                .map(|capture| substitute_ty(capture, subst))
                .collect(),
        },
        other => other.clone(),
    }
}

pub fn substitute_constraint_bounds(
    bounds: &ConstraintBounds,
    subst: &HashMap<String, Ty>,
) -> ConstraintBounds {
    ConstraintBounds {
        positive: bounds
            .positive
            .iter()
            .map(|entry| substitute_constraint_ref(entry, subst))
            .collect(),
        negative: bounds
            .negative
            .iter()
            .map(|entry| substitute_constraint_ref(entry, subst))
            .collect(),
    }
}

pub fn substitute_constraint_ref(
    entry: &ConstraintRef,
    subst: &HashMap<String, Ty>,
) -> ConstraintRef {
    ConstraintRef {
        def_id: entry.def_id,
        name: entry.name.clone(),
        args: entry
            .args
            .iter()
            .map(|arg| substitute_ty(arg, subst))
            .collect(),
    }
}

pub fn unify_ty(pattern: &Ty, actual: &Ty, subst: &mut HashMap<String, Ty>) -> bool {
    match pattern {
        Ty::Hole(_) => true,
        Ty::Generic(name) => match subst.get(name) {
            Some(Ty::Generic(existing)) if existing == name => {
                subst.insert(name.clone(), actual.clone());
                true
            }
            Some(existing) => existing == actual,
            None => {
                subst.insert(name.clone(), actual.clone());
                true
            }
        },
        Ty::Pointer {
            nullable,
            mutability,
            inner: pattern_inner,
        } => match actual {
            Ty::Pointer {
                nullable: actual_nullable,
                mutability: actual_mutability,
                inner: actual_inner,
            } if nullable == actual_nullable && mutability == actual_mutability => {
                unify_ty(pattern_inner, actual_inner, subst)
            }
            _ => false,
        },
        Ty::Array {
            len,
            elem: pattern_elem,
        } => match actual {
            Ty::Array {
                len: actual_len,
                elem: actual_elem,
            } if len == actual_len => unify_ty(pattern_elem, actual_elem, subst),
            _ => false,
        },
        Ty::Slice {
            mutability,
            elem: pattern_elem,
        } => match actual {
            Ty::Slice {
                mutability: actual_mutability,
                elem: actual_elem,
            } if mutability == actual_mutability => unify_ty(pattern_elem, actual_elem, subst),
            _ => false,
        },
        Ty::Named { name, args } => match actual {
            Ty::Named {
                name: actual_name,
                args: actual_args,
            } if name == actual_name && args.len() == actual_args.len() => args
                .iter()
                .zip(actual_args.iter())
                .all(|(pattern, actual)| unify_ty(pattern, actual, subst)),
            _ => false,
        },
        Ty::GeneratedFuture { name, output, .. } => match actual {
            Ty::GeneratedFuture {
                name: actual_name,
                output: actual_output,
                ..
            } if name == actual_name => unify_ty(output, actual_output, subst),
            _ => false,
        },
        Ty::DynamicInterface { def_id, args, .. } => match actual {
            Ty::DynamicInterface {
                def_id: actual_def_id,
                args: actual_args,
                ..
            } if def_id == actual_def_id && args.len() == actual_args.len() => args
                .iter()
                .zip(actual_args.iter())
                .all(|(pattern, actual)| unify_ty(pattern, actual, subst)),
            _ => false,
        },
        Ty::OpaqueReturn { key, .. } => match actual {
            Ty::OpaqueReturn {
                key: actual_key, ..
            } if key.def_id == actual_key.def_id && key.args.len() == actual_key.args.len() => key
                .args
                .iter()
                .zip(actual_key.args.iter())
                .all(|(pattern, actual)| unify_ty(pattern, actual, subst)),
            _ => false,
        },
        Ty::Function {
            is_unsafe,
            abi,
            ret,
            params,
        } => match actual {
            Ty::Function {
                is_unsafe: actual_is_unsafe,
                abi: actual_abi,
                ret: actual_ret,
                params: actual_params,
            } if is_unsafe == actual_is_unsafe
                && abi == actual_abi
                && params.len() == actual_params.len() =>
            {
                unify_ty(ret, actual_ret, subst)
                    && params
                        .iter()
                        .zip(actual_params.iter())
                        .all(|(pattern, actual)| unify_ty(pattern, actual, subst))
            }
            _ => false,
        },
        Ty::Closure {
            ret,
            params,
            constraints,
        } => match actual {
            Ty::Closure {
                ret: actual_ret,
                params: actual_params,
                constraints: actual_constraints,
            } if params.len() == actual_params.len() => {
                unify_ty(ret, actual_ret, subst)
                    && params
                        .iter()
                        .zip(actual_params.iter())
                        .all(|(pattern, actual)| unify_ty(pattern, actual, subst))
                    && unify_constraint_bounds(constraints, actual_constraints, subst)
            }
            Ty::ClosureInstance {
                ret: actual_ret,
                params: actual_params,
                ..
            } if constraints.is_empty() && params.len() == actual_params.len() => {
                unify_ty(ret, actual_ret, subst)
                    && params
                        .iter()
                        .zip(actual_params.iter())
                        .all(|(pattern, actual)| unify_ty(pattern, actual, subst))
            }
            _ => false,
        },
        Ty::ClosureInstance {
            id,
            ret,
            params,
            captures,
        } => match actual {
            Ty::ClosureInstance {
                id: actual_id,
                ret: actual_ret,
                params: actual_params,
                captures: actual_captures,
            } if id == actual_id
                && params.len() == actual_params.len()
                && captures.len() == actual_captures.len() =>
            {
                unify_ty(ret, actual_ret, subst)
                    && params
                        .iter()
                        .zip(actual_params.iter())
                        .all(|(pattern, actual)| unify_ty(pattern, actual, subst))
                    && captures
                        .iter()
                        .zip(actual_captures.iter())
                        .all(|(pattern, actual)| unify_ty(pattern, actual, subst))
            }
            _ => false,
        },
        other => other == actual,
    }
}

pub fn unify_constraint_bounds(
    pattern: &ConstraintBounds,
    actual: &ConstraintBounds,
    subst: &mut HashMap<String, Ty>,
) -> bool {
    let mut trial = subst.clone();
    if !unify_constraint_refs(&pattern.positive, &actual.positive, &mut trial) {
        return false;
    }
    if !unify_constraint_refs(&pattern.negative, &actual.negative, &mut trial) {
        return false;
    }
    *subst = trial;
    true
}

fn unify_constraint_refs(
    pattern: &[ConstraintRef],
    actual: &[ConstraintRef],
    subst: &mut HashMap<String, Ty>,
) -> bool {
    let Some((first, rest)) = pattern.split_first() else {
        return true;
    };
    for candidate in actual {
        if first.name != candidate.name || first.args.len() != candidate.args.len() {
            continue;
        }
        let mut trial = subst.clone();
        if !first
            .args
            .iter()
            .zip(candidate.args.iter())
            .all(|(pattern_arg, actual_arg)| unify_ty(pattern_arg, actual_arg, &mut trial))
        {
            continue;
        }
        if unify_constraint_refs(rest, actual, &mut trial) {
            *subst = trial;
            return true;
        }
    }
    false
}

pub fn closure_instance_satisfies_signature(expected: &Ty, actual: &Ty) -> bool {
    match (expected, actual) {
        (
            Ty::Closure {
                ret: expected_ret,
                params: expected_params,
                constraints: expected_constraints,
            },
            Ty::ClosureInstance {
                ret: actual_ret,
                params: actual_params,
                ..
            },
        ) => {
            expected_constraints.is_empty()
                && expected_params == actual_params
                && expected_ret.can_assign_from(actual_ret)
        }
        _ => false,
    }
}

pub fn closure_shape_satisfies(expected_ret: &Ty, expected_params: &[Ty], actual: &Ty) -> bool {
    match actual {
        Ty::Closure {
            ret: actual_ret,
            params: actual_params,
            ..
        }
        | Ty::ClosureInstance {
            ret: actual_ret,
            params: actual_params,
            ..
        } => expected_params == actual_params && expected_ret.can_assign_from(actual_ret),
        _ => false,
    }
}

pub fn callable_ret_params_ty(ty: &Ty) -> Option<(Ty, Vec<Ty>)> {
    match ty {
        Ty::Function { ret, params, .. }
        | Ty::Closure { ret, params, .. }
        | Ty::ClosureInstance { ret, params, .. } => Some(((**ret).clone(), params.clone())),
        _ => None,
    }
}

pub fn contains_generic(ty: &Ty) -> bool {
    let mut stack = vec![ty];
    while let Some(ty) = stack.pop() {
        match ty {
            Ty::Generic(_) => return true,
            Ty::Pointer { inner, .. } => stack.push(inner),
            Ty::Array { elem, .. } | Ty::Slice { elem, .. } => stack.push(elem),
            Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
                stack.extend(args.iter());
            }
            Ty::OpaqueReturn { key, bounds } => {
                stack.extend(key.args.iter());
                for entry in bounds.positive.iter().chain(bounds.negative.iter()) {
                    stack.extend(entry.args.iter());
                }
            }
            Ty::GeneratedFuture { output, .. } => stack.push(output),
            Ty::Function { ret, params, .. } => {
                stack.push(ret);
                stack.extend(params.iter());
            }
            Ty::Closure {
                ret,
                params,
                constraints,
            } => {
                stack.push(ret);
                stack.extend(params.iter());
                for entry in constraints
                    .positive
                    .iter()
                    .chain(constraints.negative.iter())
                {
                    stack.extend(entry.args.iter());
                }
            }
            Ty::ClosureInstance {
                ret,
                params,
                captures,
                ..
            } => {
                stack.push(ret);
                stack.extend(params.iter());
                stack.extend(captures.iter());
            }
            _ => {}
        }
    }
    false
}

pub fn constraint_bounds_contains_generic(bounds: &ConstraintBounds) -> bool {
    bounds
        .positive
        .iter()
        .chain(bounds.negative.iter())
        .any(|entry| entry.args.iter().any(contains_generic))
}

pub fn contains_type_hole(ty: &Ty) -> bool {
    let mut stack = vec![ty];
    while let Some(ty) = stack.pop() {
        match ty {
            Ty::Hole(_) => return true,
            Ty::Pointer { inner, .. } => stack.push(inner),
            Ty::Array { elem, .. } | Ty::Slice { elem, .. } => stack.push(elem),
            Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
                stack.extend(args.iter());
            }
            Ty::OpaqueReturn { key, bounds } => {
                stack.extend(key.args.iter());
                for entry in bounds.positive.iter().chain(bounds.negative.iter()) {
                    stack.extend(entry.args.iter());
                }
            }
            Ty::GeneratedFuture { output, .. } => stack.push(output),
            Ty::Function { ret, params, .. } => {
                stack.push(ret);
                stack.extend(params.iter());
            }
            Ty::Closure {
                ret,
                params,
                constraints,
            } => {
                stack.push(ret);
                stack.extend(params.iter());
                for entry in constraints
                    .positive
                    .iter()
                    .chain(constraints.negative.iter())
                {
                    stack.extend(entry.args.iter());
                }
            }
            Ty::ClosureInstance {
                ret,
                params,
                captures,
                ..
            } => {
                stack.push(ret);
                stack.extend(params.iter());
                stack.extend(captures.iter());
            }
            _ => {}
        }
    }
    false
}

pub fn constraint_bounds_contains_type_hole(bounds: &ConstraintBounds) -> bool {
    bounds
        .positive
        .iter()
        .chain(bounds.negative.iter())
        .any(|entry| entry.args.iter().any(contains_type_hole))
}

pub fn contains_any_generic_name(ty: &Ty, names: &HashSet<String>) -> bool {
    match ty {
        Ty::Generic(name) => names.contains(name),
        Ty::Pointer { inner, .. } => contains_any_generic_name(inner, names),
        Ty::Array { elem, .. } | Ty::Slice { elem, .. } => contains_any_generic_name(elem, names),
        Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
            args.iter().any(|arg| contains_any_generic_name(arg, names))
        }
        Ty::OpaqueReturn { key, bounds } => {
            key.args
                .iter()
                .any(|arg| contains_any_generic_name(arg, names))
                || constraint_bounds_contains_any_generic_name(bounds, names)
        }
        Ty::GeneratedFuture { output, .. } => contains_any_generic_name(output, names),
        Ty::Function { ret, params, .. } => {
            contains_any_generic_name(ret, names)
                || params
                    .iter()
                    .any(|param| contains_any_generic_name(param, names))
        }
        Ty::Closure {
            ret,
            params,
            constraints,
        } => {
            contains_any_generic_name(ret, names)
                || params
                    .iter()
                    .any(|param| contains_any_generic_name(param, names))
                || constraint_bounds_contains_any_generic_name(constraints, names)
        }
        Ty::ClosureInstance {
            ret,
            params,
            captures,
            ..
        } => {
            contains_any_generic_name(ret, names)
                || params
                    .iter()
                    .any(|param| contains_any_generic_name(param, names))
                || captures
                    .iter()
                    .any(|capture| contains_any_generic_name(capture, names))
        }
        _ => false,
    }
}

pub fn constraint_bounds_contains_any_generic_name(
    bounds: &ConstraintBounds,
    names: &HashSet<String>,
) -> bool {
    bounds
        .positive
        .iter()
        .chain(bounds.negative.iter())
        .any(|entry| {
            entry
                .args
                .iter()
                .any(|arg| contains_any_generic_name(arg, names))
        })
}

pub fn type_complexity(ty: &Ty) -> usize {
    match ty {
        Ty::Pointer { inner, .. } => 1 + type_complexity(inner),
        Ty::Array { elem, .. } | Ty::Slice { elem, .. } => 1 + type_complexity(elem),
        Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
            1 + args.iter().map(type_complexity).sum::<usize>()
        }
        Ty::OpaqueReturn { key, bounds } => {
            1 + key.args.iter().map(type_complexity).sum::<usize>()
                + constraint_bounds_complexity(bounds)
        }
        Ty::GeneratedFuture { output, .. } => 1 + type_complexity(output),
        Ty::Function { ret, params, .. } => {
            1 + type_complexity(ret) + params.iter().map(type_complexity).sum::<usize>()
        }
        Ty::Closure {
            ret,
            params,
            constraints,
        } => {
            1 + type_complexity(ret)
                + params.iter().map(type_complexity).sum::<usize>()
                + constraint_bounds_complexity(constraints)
        }
        Ty::ClosureInstance {
            ret,
            params,
            captures,
            ..
        } => {
            1 + type_complexity(ret)
                + params.iter().map(type_complexity).sum::<usize>()
                + captures.iter().map(type_complexity).sum::<usize>()
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
        | Ty::Unknown => 0,
    }
}

pub fn constraint_bounds_complexity(bounds: &ConstraintBounds) -> usize {
    bounds
        .positive
        .iter()
        .chain(bounds.negative.iter())
        .map(|entry| 1 + entry.args.iter().map(type_complexity).sum::<usize>())
        .sum()
}

pub fn ty_contains(container: &Ty, needle: &Ty) -> bool {
    if container == needle {
        return true;
    }
    match container {
        Ty::Pointer { inner, .. } => ty_contains(inner, needle),
        Ty::Array { elem, .. } | Ty::Slice { elem, .. } => ty_contains(elem, needle),
        Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
            args.iter().any(|arg| ty_contains(arg, needle))
        }
        Ty::OpaqueReturn { key, bounds } => {
            key.args.iter().any(|arg| ty_contains(arg, needle))
                || constraint_bounds_contains_ty(bounds, needle)
        }
        Ty::GeneratedFuture { output, .. } => ty_contains(output, needle),
        Ty::Function { ret, params, .. } => {
            ty_contains(ret, needle) || params.iter().any(|param| ty_contains(param, needle))
        }
        Ty::Closure {
            ret,
            params,
            constraints,
        } => {
            ty_contains(ret, needle)
                || params.iter().any(|param| ty_contains(param, needle))
                || constraint_bounds_contains_ty(constraints, needle)
        }
        Ty::ClosureInstance {
            ret,
            params,
            captures,
            ..
        } => {
            ty_contains(ret, needle)
                || params.iter().any(|param| ty_contains(param, needle))
                || captures.iter().any(|capture| ty_contains(capture, needle))
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

pub fn constraint_bounds_contains_ty(bounds: &ConstraintBounds, needle: &Ty) -> bool {
    bounds
        .positive
        .iter()
        .chain(bounds.negative.iter())
        .any(|entry| entry.args.iter().any(|arg| ty_contains(arg, needle)))
}

pub fn retained_closure_capabilities(ty: &Ty) -> Vec<ConstraintRef> {
    match ty {
        Ty::Closure { constraints, .. } => constraints.positive.clone(),
        _ => Vec::new(),
    }
}

pub fn retained_closure_has_capability(ty: &Ty, capability: &ConstraintRef) -> bool {
    let Ty::Closure { constraints, .. } = ty else {
        return false;
    };
    constraints.positive.iter().any(|entry| entry == capability)
}

pub fn retained_closure_proves_capability(ty: &Ty, interface_def: DefId, args: &[Ty]) -> bool {
    let Ty::Closure { constraints, .. } = ty else {
        return false;
    };
    constraints
        .positive
        .iter()
        .any(|entry| entry.def_id == interface_def && entry.args == args)
}

pub fn receiver_ty_from_value_ty(ty: &Ty) -> Ty {
    match ty {
        Ty::Pointer { inner, .. } => (**inner).clone(),
        other => other.clone(),
    }
}

pub fn std_error_ty() -> Ty {
    Ty::Named {
        name: "Error".to_string(),
        args: Vec::new(),
    }
}

pub fn std_error_trait_ty(def_id: DefId) -> Ty {
    Ty::DynamicInterface {
        def_id,
        name: STD_ERROR_TRAIT_ALIAS.to_string(),
        args: Vec::new(),
    }
}

pub fn std_error_code_ty() -> Ty {
    Ty::Named {
        name: STD_ERROR_CODE_TYPE.to_string(),
        args: Vec::new(),
    }
}

pub fn std_async_error_ty() -> Ty {
    Ty::Named {
        name: "AsyncError".to_string(),
        args: Vec::new(),
    }
}

pub fn std_resource_error_ty() -> Ty {
    Ty::Named {
        name: "ResourceError".to_string(),
        args: Vec::new(),
    }
}

pub fn std_result_ty(ok_ty: Ty, err_ty: Ty) -> Ty {
    Ty::Named {
        name: "Result".to_string(),
        args: vec![ok_ty, err_ty],
    }
}

pub fn std_message_result_ty(ok_ty: Ty) -> Ty {
    std_result_ty(ok_ty, std_error_ty())
}

pub fn std_actor_ty(message_ty: Ty) -> Ty {
    Ty::Named {
        name: "Actor".to_string(),
        args: vec![message_ty],
    }
}

pub fn std_future_ty(output_ty: Ty) -> Ty {
    Ty::Named {
        name: "Future".to_string(),
        args: vec![output_ty],
    }
}

pub fn generated_future_ty(
    name: impl Into<String>,
    output_ty: Ty,
    cancel_safe: bool,
    abortable: bool,
) -> Ty {
    generated_future_ty_with_affine_state(name, output_ty, cancel_safe, abortable, false)
}

pub fn generated_future_ty_with_affine_state(
    name: impl Into<String>,
    output_ty: Ty,
    cancel_safe: bool,
    abortable: bool,
    affine_state: bool,
) -> Ty {
    Ty::GeneratedFuture {
        name: name.into(),
        output: Box::new(output_ty),
        cancel_safe,
        abortable,
        affine_state,
    }
}

pub fn generated_future_output_ty(ty: &Ty) -> Option<Ty> {
    let Ty::GeneratedFuture { output, .. } = ty else {
        return None;
    };
    Some((**output).clone())
}

pub fn std_task_ty(output_ty: Ty) -> Ty {
    Ty::Named {
        name: "Task".to_string(),
        args: vec![output_ty],
    }
}

pub fn std_sender_ty(payload_ty: Ty) -> Ty {
    Ty::Named {
        name: "Sender".to_string(),
        args: vec![payload_ty],
    }
}

pub fn std_receiver_ty(payload_ty: Ty) -> Ty {
    Ty::Named {
        name: "Receiver".to_string(),
        args: vec![payload_ty],
    }
}

pub fn std_send_permit_ty(payload_ty: Ty) -> Ty {
    Ty::Named {
        name: "SendPermit".to_string(),
        args: vec![payload_ty],
    }
}

pub fn std_task_group_ty(payload_ty: Ty) -> Ty {
    Ty::Named {
        name: "TaskGroup".to_string(),
        args: vec![payload_ty],
    }
}

pub const STD_META_REF_REPR_MARKER: &str = "__ciel_std_meta_RefRepr";
pub const STD_META_REPR_MARKER: &str = "__ciel_std_meta_Repr";
pub const STD_META_SCHEMA_MARKER: &str = "__ciel_std_meta_Schema";
pub const META_ARRAY_CHUNK_SIZE: usize = 16;
pub const META_ARRAY_EXPANSION_BUDGET: usize = 4096;

pub fn meta_named(name: &str, args: Vec<Ty>) -> Ty {
    Ty::Named {
        name: name.to_string(),
        args,
    }
}

pub fn meta_product_ty<I>(fields: I, head_name: &str) -> Ty
where
    I: IntoIterator<Item = Ty>,
    I::IntoIter: DoubleEndedIterator,
{
    fields
        .into_iter()
        .rev()
        .fold(meta_named("HNil", Vec::new()), |tail, field_ty| {
            let head = meta_named(head_name, vec![field_ty]);
            meta_named("HCons", vec![head, tail])
        })
}

pub fn meta_sum_ty<I>(payloads: I, borrowed: bool) -> Ty
where
    I: IntoIterator,
    I::Item: IntoIterator<Item = Ty>,
    I::IntoIter: DoubleEndedIterator,
    <I::Item as IntoIterator>::IntoIter: DoubleEndedIterator,
{
    payloads
        .into_iter()
        .rev()
        .fold(meta_named("CoNil", Vec::new()), |tail, payload| {
            let payload_head = if borrowed { "PayloadRef" } else { "Payload" };
            let payload = meta_product_ty(payload, payload_head);
            let variant_head = if borrowed { "VariantRef" } else { "Variant" };
            let head = meta_named(variant_head, vec![payload]);
            meta_named("Coproduct", vec![head, tail])
        })
}

pub fn meta_type_tag_ty(ty: Ty) -> Ty {
    meta_named("Type", vec![ty])
}

pub fn meta_schema_product_ty<I>(fields: I) -> Ty
where
    I: IntoIterator<Item = (Ty, Ty)>,
    I::IntoIter: DoubleEndedIterator,
{
    fields.into_iter().rev().fold(
        meta_named("HNil", Vec::new()),
        |tail, (source_ty, repr_ty)| {
            let head = meta_named("FieldSchema", vec![source_ty, repr_ty]);
            meta_named("HCons", vec![head, tail])
        },
    )
}

pub fn meta_schema_payload_ty<I>(payloads: I) -> Ty
where
    I: IntoIterator<Item = (Ty, Ty)>,
    I::IntoIter: DoubleEndedIterator,
{
    payloads.into_iter().rev().fold(
        meta_named("HNil", Vec::new()),
        |tail, (source_ty, repr_ty)| {
            let head = meta_named("PayloadSchema", vec![source_ty, repr_ty]);
            meta_named("HCons", vec![head, tail])
        },
    )
}

pub fn meta_schema_sum_ty<I>(variants: I) -> Ty
where
    I: IntoIterator,
    I::Item: IntoIterator<Item = (Ty, Ty)>,
    I::IntoIter: DoubleEndedIterator,
    <I::Item as IntoIterator>::IntoIter: DoubleEndedIterator,
{
    variants
        .into_iter()
        .rev()
        .fold(meta_named("HNil", Vec::new()), |tail, payload| {
            let payload = meta_schema_payload_ty(payload);
            let head = meta_named("VariantSchema", vec![payload]);
            meta_named("HCons", vec![head, tail])
        })
}

pub fn meta_repr_owned_leaf_ty(ty: &Ty) -> Ty {
    match ty {
        Ty::Array { len, elem } => meta_array_repr_ty(*len, elem, false),
        other => other.clone(),
    }
}

pub fn meta_repr_borrowed_array_leaf_ty(ty: &Ty) -> Ty {
    ty.clone()
}

pub fn meta_repr_borrowed_array_item_ty(ty: &Ty) -> Ty {
    Ty::pointer_to(ty.clone())
}

pub fn meta_ref_array_repr_ty(len: usize, elem: &Ty) -> Ty {
    if len == 0 {
        return meta_named("ArrayNil", Vec::new());
    }
    if len <= META_ARRAY_CHUNK_SIZE {
        return meta_named(
            &format!("ArrayChunk{len}"),
            vec![meta_repr_borrowed_array_item_ty(elem)],
        );
    }
    let split = meta_array_split_len(len);
    meta_named(
        "ArrayCat",
        vec![
            meta_ref_array_repr_ty(split, elem),
            meta_ref_array_repr_ty(len - split, elem),
        ],
    )
}

pub fn meta_array_repr_ty(len: usize, elem: &Ty, borrowed: bool) -> Ty {
    meta_array_repr_ty_with_leaf(len, elem, borrowed, &mut meta_repr_owned_leaf_ty)
}

pub fn meta_array_repr_ty_with_leaf<F>(
    len: usize,
    elem: &Ty,
    borrowed: bool,
    owned_leaf: &mut F,
) -> Ty
where
    F: FnMut(&Ty) -> Ty,
{
    if len == 0 {
        return meta_named("ArrayNil", Vec::new());
    }
    if len <= META_ARRAY_CHUNK_SIZE {
        let elem_ty = if borrowed {
            meta_repr_borrowed_array_leaf_ty(elem)
        } else {
            owned_leaf(elem)
        };
        return meta_named(&format!("ArrayChunk{len}"), vec![elem_ty]);
    }
    let split = meta_array_split_len(len);
    meta_named(
        "ArrayCat",
        vec![
            meta_array_repr_ty_with_leaf(split, elem, borrowed, owned_leaf),
            meta_array_repr_ty_with_leaf(len - split, elem, borrowed, owned_leaf),
        ],
    )
}

pub fn meta_array_split_len(len: usize) -> usize {
    if len <= META_ARRAY_CHUNK_SIZE {
        return len;
    }
    let chunks = len.div_ceil(META_ARRAY_CHUNK_SIZE);
    let left_chunks = chunks / 2;
    (left_chunks * META_ARRAY_CHUNK_SIZE).min(len - 1).max(1)
}

pub fn meta_array_expansion_cost(len: usize, elem: &Ty) -> Option<usize> {
    let elem_cost = match elem {
        Ty::Array {
            len: elem_len,
            elem: inner,
        } => meta_array_expansion_cost(*elem_len, inner)?,
        _ => 1,
    };
    let element_cost = len.checked_mul(elem_cost)?;
    if len == 0 {
        return Some(1);
    }
    let chunks = len.div_ceil(META_ARRAY_CHUNK_SIZE);
    let cats = chunks.saturating_sub(1);
    element_cost.checked_add(chunks)?.checked_add(cats)
}

pub fn std_meta_repr_marker_ty(borrowed: bool, source_ty: Ty) -> Ty {
    Ty::Named {
        name: if borrowed {
            STD_META_REF_REPR_MARKER
        } else {
            STD_META_REPR_MARKER
        }
        .to_string(),
        args: vec![source_ty],
    }
}

pub fn std_meta_schema_marker_ty(source_ty: Ty) -> Ty {
    Ty::Named {
        name: STD_META_SCHEMA_MARKER.to_string(),
        args: vec![source_ty],
    }
}

pub fn std_meta_repr_source_name(name: &str) -> Option<bool> {
    match name {
        "RefRepr" => Some(true),
        "Repr" => Some(false),
        _ => None,
    }
}

pub fn std_meta_schema_source_name(name: &str) -> bool {
    name == "Schema"
}

pub fn meta_repr_marker_name(name: &str) -> Option<bool> {
    match name {
        STD_META_REF_REPR_MARKER => Some(true),
        STD_META_REPR_MARKER => Some(false),
        _ => None,
    }
}

pub fn meta_repr_marker_source(ty: &Ty) -> Option<(bool, &Ty)> {
    let Ty::Named { name, args } = ty else {
        return None;
    };
    let borrowed = meta_repr_marker_name(name)?;
    if args.len() != 1 {
        return None;
    }
    Some((borrowed, &args[0]))
}

pub fn meta_schema_marker_name(name: &str) -> bool {
    name == STD_META_SCHEMA_MARKER
}

pub fn meta_schema_marker_source(ty: &Ty) -> Option<&Ty> {
    let Ty::Named { name, args } = ty else {
        return None;
    };
    if !meta_schema_marker_name(name) || args.len() != 1 {
        return None;
    }
    Some(&args[0])
}

pub fn contains_meta_marker(ty: &Ty) -> bool {
    match ty {
        Ty::Named { name, args } => {
            meta_repr_marker_name(name).is_some()
                || meta_schema_marker_name(name)
                || args.iter().any(contains_meta_marker)
        }
        Ty::GeneratedFuture { output, .. } => contains_meta_marker(output),
        Ty::Pointer { inner, .. } => contains_meta_marker(inner),
        Ty::Array { elem, .. } | Ty::Slice { elem, .. } => contains_meta_marker(elem),
        Ty::DynamicInterface { args, .. } => args.iter().any(contains_meta_marker),
        Ty::OpaqueReturn { key, bounds } => {
            key.args.iter().any(contains_meta_marker)
                || bounds
                    .positive
                    .iter()
                    .chain(bounds.negative.iter())
                    .any(|entry| entry.args.iter().any(contains_meta_marker))
        }
        Ty::Function { ret, params, .. } | Ty::Closure { ret, params, .. } => {
            contains_meta_marker(ret) || params.iter().any(contains_meta_marker)
        }
        Ty::ClosureInstance {
            ret,
            params,
            captures,
            ..
        } => {
            contains_meta_marker(ret)
                || params.iter().any(contains_meta_marker)
                || captures.iter().any(contains_meta_marker)
        }
        _ => false,
    }
}

pub fn contains_meta_repr_marker(ty: &Ty) -> bool {
    match ty {
        Ty::Named { name, args } => {
            meta_repr_marker_name(name).is_some() || args.iter().any(contains_meta_repr_marker)
        }
        Ty::GeneratedFuture { output, .. } => contains_meta_repr_marker(output),
        Ty::Pointer { inner, .. } => contains_meta_repr_marker(inner),
        Ty::Array { elem, .. } | Ty::Slice { elem, .. } => contains_meta_repr_marker(elem),
        Ty::DynamicInterface { args, .. } => args.iter().any(contains_meta_repr_marker),
        Ty::OpaqueReturn { key, bounds } => {
            key.args.iter().any(contains_meta_repr_marker)
                || bounds
                    .positive
                    .iter()
                    .chain(bounds.negative.iter())
                    .any(|entry| entry.args.iter().any(contains_meta_repr_marker))
        }
        Ty::Function { ret, params, .. } | Ty::Closure { ret, params, .. } => {
            contains_meta_repr_marker(ret) || params.iter().any(contains_meta_repr_marker)
        }
        Ty::ClosureInstance {
            ret,
            params,
            captures,
            ..
        } => {
            contains_meta_repr_marker(ret)
                || params.iter().any(contains_meta_repr_marker)
                || captures.iter().any(contains_meta_repr_marker)
        }
        _ => false,
    }
}

pub fn map_ty_children<F>(ty: &Ty, mut map_child: F) -> Ty
where
    F: FnMut(&Ty) -> Ty,
{
    match ty {
        Ty::Pointer {
            nullable,
            mutability,
            inner,
        } => Ty::Pointer {
            nullable: *nullable,
            mutability: *mutability,
            inner: Box::new(map_child(inner)),
        },
        Ty::Array { len, elem } => Ty::Array {
            len: *len,
            elem: Box::new(map_child(elem)),
        },
        Ty::Slice { mutability, elem } => Ty::Slice {
            mutability: *mutability,
            elem: Box::new(map_child(elem)),
        },
        Ty::Named { name, args } => Ty::Named {
            name: name.clone(),
            args: args.iter().map(map_child).collect(),
        },
        Ty::GeneratedFuture {
            name,
            output,
            cancel_safe,
            abortable,
            affine_state,
        } => Ty::GeneratedFuture {
            name: name.clone(),
            output: Box::new(map_child(output)),
            cancel_safe: *cancel_safe,
            abortable: *abortable,
            affine_state: *affine_state,
        },
        Ty::DynamicInterface { def_id, name, args } => Ty::DynamicInterface {
            def_id: *def_id,
            name: name.clone(),
            args: args.iter().map(map_child).collect(),
        },
        Ty::OpaqueReturn { key, bounds } => Ty::OpaqueReturn {
            key: OpaqueReturnKey {
                def_id: key.def_id,
                args: key.args.iter().map(&mut map_child).collect(),
            },
            bounds: ConstraintBounds {
                positive: bounds
                    .positive
                    .iter()
                    .map(|entry| ConstraintRef {
                        def_id: entry.def_id,
                        name: entry.name.clone(),
                        args: entry.args.iter().map(&mut map_child).collect(),
                    })
                    .collect(),
                negative: bounds
                    .negative
                    .iter()
                    .map(|entry| ConstraintRef {
                        def_id: entry.def_id,
                        name: entry.name.clone(),
                        args: entry.args.iter().map(&mut map_child).collect(),
                    })
                    .collect(),
            },
        },
        Ty::Function {
            is_unsafe,
            abi,
            ret,
            params,
        } => Ty::Function {
            is_unsafe: *is_unsafe,
            abi: abi.clone(),
            ret: Box::new(map_child(ret)),
            params: params.iter().map(map_child).collect(),
        },
        Ty::Closure {
            ret,
            params,
            constraints,
        } => Ty::Closure {
            ret: Box::new(map_child(ret)),
            params: params.iter().map(map_child).collect(),
            constraints: constraints.clone(),
        },
        Ty::ClosureInstance {
            id,
            ret,
            params,
            captures,
        } => Ty::ClosureInstance {
            id: *id,
            ret: Box::new(map_child(ret)),
            params: params.iter().map(&mut map_child).collect(),
            captures: captures.iter().map(map_child).collect(),
        },
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
        | Ty::Unknown => ty.clone(),
    }
}

pub fn aggregate_instance_name(name: &str, args: &[Ty]) -> String {
    if args.is_empty() {
        name.to_string()
    } else {
        format!(
            "{}_{}",
            name,
            args.iter()
                .map(mangle_ty_fragment)
                .collect::<Vec<_>>()
                .join("_")
        )
    }
}

fn display_nominal_name(name: &str) -> &str {
    let Some((base, suffix)) = name.rsplit_once("__def") else {
        return name;
    };
    if suffix.is_empty() || !suffix.chars().all(|ch| ch.is_ascii_digit()) {
        return name;
    }
    base
}

pub fn mangle_ty_fragment(ty: &Ty) -> String {
    match ty {
        Ty::Hole(_) => "hole".to_string(),
        Ty::Never => "never".to_string(),
        Ty::Void => "void".to_string(),
        Ty::Bool => "bool".to_string(),
        Ty::Char => "char".to_string(),
        Ty::I8 => "i8".to_string(),
        Ty::I16 => "i16".to_string(),
        Ty::I32 => "i32".to_string(),
        Ty::I64 => "i64".to_string(),
        Ty::U8 => "u8".to_string(),
        Ty::U16 => "u16".to_string(),
        Ty::U32 => "u32".to_string(),
        Ty::U64 => "u64".to_string(),
        Ty::Usize => "usize".to_string(),
        Ty::F32 => "f32".to_string(),
        Ty::F64 => "f64".to_string(),
        Ty::CSpelling { abi, spelling } => {
            format!(
                "c_{}_{}",
                mangle_abi_fragment(Some(abi)),
                sanitize_mangle_fragment(spelling)
            )
        }
        Ty::Pointer {
            inner,
            nullable,
            mutability,
        } => {
            let prefix = match (*nullable, *mutability) {
                (false, ViewMutability::Writable) => "ptr",
                (false, ViewMutability::ReadOnly) => "cptr",
                (true, ViewMutability::Writable) => "qptr",
                (true, ViewMutability::ReadOnly) => "qcptr",
            };
            format!("{prefix}_{}", mangle_ty_fragment(inner))
        }
        Ty::Array { len, elem } => format!("arr{len}_{}", mangle_ty_fragment(elem)),
        Ty::Slice { mutability, elem } => {
            let prefix = match mutability {
                ViewMutability::Writable => "slice",
                ViewMutability::ReadOnly => "cslice",
            };
            format!("{prefix}_{}", mangle_ty_fragment(elem))
        }
        Ty::Named { name, args } => aggregate_instance_name(name, args),
        Ty::GeneratedFuture {
            name,
            output,
            affine_state,
            ..
        } => {
            let prefix = if *affine_state {
                "generated_affine_future"
            } else {
                "generated_future"
            };
            format!("{prefix}_{name}_{}", mangle_ty_fragment(output))
        }
        Ty::Generic(name) => format!("gen_{name}"),
        Ty::DynamicInterface { def_id, name, args } => {
            if args.is_empty() {
                format!("dyn_{name}_def{}", def_id.0)
            } else {
                format!(
                    "dyn_{}_def{}_{}",
                    name,
                    def_id.0,
                    args.iter()
                        .map(mangle_ty_fragment)
                        .collect::<Vec<_>>()
                        .join("_")
                )
            }
        }
        Ty::OpaqueReturn { key, bounds } => {
            let args = if key.args.is_empty() {
                "void".to_string()
            } else {
                key.args
                    .iter()
                    .map(mangle_ty_fragment)
                    .collect::<Vec<_>>()
                    .join("_")
            };
            format!(
                "opaque_ret_def{}_args_{}_caps_{}",
                key.def_id.0,
                args,
                mangle_constraint_bounds(bounds)
            )
        }
        Ty::Function {
            is_unsafe,
            abi,
            ret,
            params,
        } => {
            let params = if params.is_empty() {
                "void".to_string()
            } else {
                params
                    .iter()
                    .map(mangle_ty_fragment)
                    .collect::<Vec<_>>()
                    .join("_")
            };
            format!(
                "fn_{}_{}_ret_{}_args_{}",
                if *is_unsafe { "unsafe" } else { "safe" },
                mangle_abi_fragment(abi.as_deref()),
                mangle_ty_fragment(ret),
                params
            )
        }
        Ty::Closure {
            ret,
            params,
            constraints,
        } => {
            let params = if params.is_empty() {
                "void".to_string()
            } else {
                params
                    .iter()
                    .map(mangle_ty_fragment)
                    .collect::<Vec<_>>()
                    .join("_")
            };
            format!(
                "closure_ret_{}_args_{}_caps_{}",
                mangle_ty_fragment(ret),
                params,
                mangle_constraint_bounds(constraints)
            )
        }
        Ty::ClosureInstance {
            id,
            ret,
            params,
            captures,
        } => {
            let params = if params.is_empty() {
                "void".to_string()
            } else {
                params
                    .iter()
                    .map(mangle_ty_fragment)
                    .collect::<Vec<_>>()
                    .join("_")
            };
            let captures = if captures.is_empty() {
                "empty".to_string()
            } else {
                captures
                    .iter()
                    .map(mangle_ty_fragment)
                    .collect::<Vec<_>>()
                    .join("_")
            };
            format!(
                "closure_inst{id}_ret_{}_args_{}_caps_{}",
                mangle_ty_fragment(ret),
                params,
                captures
            )
        }
        Ty::Unknown => "unknown".to_string(),
    }
}

pub fn mangle_constraint_bounds(bounds: &ConstraintBounds) -> String {
    if bounds.is_empty() {
        return "none".to_string();
    }
    let mut parts = bounds
        .positive
        .iter()
        .map(|entry| format!("pos_{}", mangle_constraint_ref(entry)))
        .collect::<Vec<_>>();
    parts.extend(
        bounds
            .negative
            .iter()
            .map(|entry| format!("neg_{}", mangle_constraint_ref(entry))),
    );
    parts.join("_")
}

pub fn mangle_constraint_ref(entry: &ConstraintRef) -> String {
    if entry.args.is_empty() {
        format!(
            "{}_def{}",
            sanitize_mangle_fragment(&entry.name),
            entry.def_id.0
        )
    } else {
        format!(
            "{}_def{}_{}",
            sanitize_mangle_fragment(&entry.name),
            entry.def_id.0,
            entry
                .args
                .iter()
                .map(mangle_ty_fragment)
                .collect::<Vec<_>>()
                .join("_")
        )
    }
}

pub fn mangle_abi_fragment(abi: Option<&str>) -> String {
    let Some(abi) = abi else {
        return "ciel".to_string();
    };
    let out = abi
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    if out.is_empty() {
        "abi".to_string()
    } else {
        out
    }
}

pub fn sanitize_mangle_fragment(value: &str) -> String {
    let out = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    if out.is_empty() {
        "type".to_string()
    } else {
        out
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Never => write!(f, "never"),
            Ty::Hole(_) => write!(f, "_"),
            Ty::Void => write!(f, "void"),
            Ty::Bool => write!(f, "bool"),
            Ty::Char => write!(f, "char"),
            Ty::I8 => write!(f, "i8"),
            Ty::I16 => write!(f, "i16"),
            Ty::I32 => write!(f, "i32"),
            Ty::I64 => write!(f, "i64"),
            Ty::U8 => write!(f, "u8"),
            Ty::U16 => write!(f, "u16"),
            Ty::U32 => write!(f, "u32"),
            Ty::U64 => write!(f, "u64"),
            Ty::Usize => write!(f, "usize"),
            Ty::F32 => write!(f, "f32"),
            Ty::F64 => write!(f, "f64"),
            Ty::CSpelling { spelling, .. } => write!(f, "{spelling}"),
            Ty::Pointer {
                nullable,
                mutability,
                inner,
            } => {
                let prefix = match (*nullable, *mutability) {
                    (false, ViewMutability::Writable) => "*",
                    (false, ViewMutability::ReadOnly) => "*const ",
                    (true, ViewMutability::Writable) => "?*",
                    (true, ViewMutability::ReadOnly) => "?*const ",
                };
                write!(f, "{prefix}{inner}")
            }
            Ty::Array { len, elem } => write!(f, "[{len}]{elem}"),
            Ty::Slice { mutability, elem } => match mutability {
                ViewMutability::Writable => write!(f, "[]{elem}"),
                ViewMutability::ReadOnly => write!(f, "[]const {elem}"),
            },
            Ty::Named { name, args } => {
                let display_name = match name.as_str() {
                    "__ciel_std_meta_RefRepr" => "meta::RefRepr",
                    "__ciel_std_meta_Repr" => "meta::Repr",
                    "__ciel_std_meta_Schema" => "meta::Schema",
                    _ => display_nominal_name(name),
                };
                if args.is_empty() {
                    write!(f, "{display_name}")
                } else {
                    write!(
                        f,
                        "{}<{}>",
                        display_name,
                        args.iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Ty::GeneratedFuture { output, .. } => write!(f, "Future<{output}>"),
            Ty::Generic(name) => write!(f, "{name}"),
            Ty::DynamicInterface { name, args, .. } => {
                if args.is_empty() {
                    write!(f, "{name}")
                } else {
                    write!(
                        f,
                        "{}<{}>",
                        name,
                        args.iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Ty::OpaqueReturn { bounds, .. } => {
                write!(f, "_: {}", display_constraint_bounds(bounds))
            }
            Ty::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => {
                if *is_unsafe {
                    write!(f, "unsafe ")?;
                }
                if let Some(abi) = abi {
                    write!(f, "extern \"{abi}\" ")?;
                }
                write!(
                    f,
                    "{} fn({})",
                    ret,
                    params
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            Ty::Closure {
                ret,
                params,
                constraints,
            } => {
                let capability_suffix = closure_constraint_suffix(constraints);
                write!(
                    f,
                    "{} |({}){}|",
                    ret,
                    params
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                    capability_suffix
                )
            }
            Ty::ClosureInstance {
                id, ret, params, ..
            } => {
                write!(
                    f,
                    "closure#{id}<{} |({})|>",
                    ret,
                    params
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            Ty::Unknown => write!(f, "<unknown>"),
        }
    }
}

fn closure_constraint_suffix(constraints: &ConstraintBounds) -> String {
    if constraints.is_empty() {
        return String::new();
    }
    format!(": {}", display_constraint_bounds(constraints))
}

pub fn display_constraint_bounds(constraints: &ConstraintBounds) -> String {
    let mut parts = constraints
        .positive
        .iter()
        .map(display_constraint_ref)
        .collect::<Vec<_>>();
    parts.extend(
        constraints
            .negative
            .iter()
            .map(|capability| format!("!{}", display_constraint_ref(capability))),
    );
    parts.join(" + ")
}

pub fn display_constraint_ref(capability: &ConstraintRef) -> String {
    if capability.args.is_empty() {
        capability.name.clone()
    } else {
        format!(
            "{}<{}>",
            capability.name,
            capability
                .args
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}
