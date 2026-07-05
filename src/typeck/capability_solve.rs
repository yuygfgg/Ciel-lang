use std::collections::{HashMap, HashSet};

use crate::{
    diagnostic::Diagnostic,
    resolve::DefId,
    std_id,
    types::{
        ConstraintBounds, ConstraintRef, Ty, contains_generic, map_ty_children,
        substitute_constraint_bounds, substitute_ty, unify_ty,
    },
};

use super::{
    capability::CapabilityTable, env::TyCtx, interface_non_receiver_args, known_ty_matches,
    ty_generic_names,
};

#[derive(Clone, Debug)]
pub(super) struct HiddenConstraint {
    pub(super) receiver: Ty,
    pub(super) capability: ConstraintRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum HiddenSolveResult {
    NoSolution,
    Unique(Vec<(String, Ty)>),
    Ambiguous,
}

pub(super) fn solve_hidden_from_capability(
    ctx: &TyCtx,
    receiver_ty: &Ty,
    capability: &ConstraintRef,
    hidden_names: &HashSet<String>,
    assumptions: &[HiddenConstraint],
) -> HiddenSolveResult {
    let Some(interface) = ctx.interfaces.get(&capability.def_id) else {
        return HiddenSolveResult::NoSolution;
    };
    let Some(determined_start) = interface.determined_start else {
        return HiddenSolveResult::NoSolution;
    };
    let full_args = std::iter::once(receiver_ty.clone())
        .chain(capability.args.iter().cloned())
        .collect::<Vec<_>>();
    if full_args.len() != interface.generics.len() {
        return HiddenSolveResult::NoSolution;
    }

    let mut candidates = Vec::new();
    candidates.extend(hidden_candidates_from_assumptions(
        capability.def_id,
        determined_start,
        &full_args,
        hidden_names,
        assumptions,
    ));
    candidates.extend(hidden_candidates_from_impls(
        ctx,
        capability.def_id,
        determined_start,
        &full_args,
        hidden_names,
    ));
    candidates.extend(hidden_candidates_from_generic_impls(
        ctx,
        capability.def_id,
        determined_start,
        &full_args,
        hidden_names,
    ));

    unique_candidate(candidates)
}

pub fn check_determined_coherence(table: &CapabilityTable<'_>) -> Vec<Diagnostic> {
    let ctx = table.ctx();
    let mut diagnostics = Vec::new();
    let mut impls = Vec::new();
    for implementation in table.impls() {
        impls.push(CoherenceImpl {
            interface_def: implementation.interface_def,
            interface_name: implementation.interface_name.clone(),
            interface_args: implementation.interface_args.clone(),
            span: None,
        });
    }
    for template in table.generic_impls() {
        if let Some(interface) = ctx.interfaces.get(&template.interface_def)
            && let Some(determined_start) = interface.determined_start
            && generic_impl_has_unfixed_determined_generics(ctx, template, determined_start)
        {
            diagnostics.push(Diagnostic::new(
                template.item_span,
                format!(
                    "interface `{}` generic impl has determined parameters not fixed by determinant parameters",
                    template.interface_name
                ),
            ));
        }
        impls.push(CoherenceImpl {
            interface_def: template.interface_def,
            interface_name: template.interface_name.clone(),
            interface_args: template.interface_args.clone(),
            span: Some(template.item_span),
        });
    }

    for (idx, left) in impls.iter().enumerate() {
        let Some(interface) = ctx.interfaces.get(&left.interface_def) else {
            continue;
        };
        let Some(determined_start) = interface.determined_start else {
            continue;
        };
        for right in impls.iter().skip(idx + 1) {
            if left.interface_def != right.interface_def
                || left.interface_args.len() != right.interface_args.len()
            {
                continue;
            }
            let Some(determined_equivalent) = overlapping_determined_patterns_equivalent(
                &left.interface_args,
                &right.interface_args,
                determined_start,
            ) else {
                continue;
            };
            if determined_equivalent {
                continue;
            }
            diagnostics.push(Diagnostic::new(
                right.span.or(left.span),
                format!(
                    "interface `{}` has overlapping impls with conflicting determined parameters",
                    left.interface_name
                ),
            ));
        }
    }
    diagnostics
}

#[derive(Clone, Debug)]
struct CoherenceImpl {
    interface_def: DefId,
    interface_name: String,
    interface_args: Vec<Ty>,
    span: Option<crate::span::Span>,
}

fn hidden_candidates_from_assumptions(
    interface_def: DefId,
    determined_start: usize,
    full_args: &[Ty],
    hidden_names: &HashSet<String>,
    assumptions: &[HiddenConstraint],
) -> Vec<Vec<(String, Ty)>> {
    let mut candidates = Vec::new();
    for assumption in assumptions {
        if assumption.capability.def_id != interface_def {
            continue;
        }
        let candidate = std::iter::once(assumption.receiver.clone())
            .chain(assumption.capability.args.iter().cloned())
            .collect::<Vec<_>>();
        if determinants_match(full_args, &candidate, determined_start)
            && let Some(solution) = hidden_solution_from_determined_args(
                full_args,
                &candidate,
                determined_start,
                hidden_names,
            )
        {
            candidates.push(solution);
        }
    }
    candidates
}

fn hidden_candidates_from_impls(
    ctx: &TyCtx,
    interface_def: DefId,
    determined_start: usize,
    full_args: &[Ty],
    hidden_names: &HashSet<String>,
) -> Vec<Vec<(String, Ty)>> {
    let mut candidates = Vec::new();
    for implementation in &ctx.impls {
        if implementation.interface_def != interface_def {
            continue;
        }
        if determinants_match(full_args, &implementation.interface_args, determined_start)
            && let Some(solution) = hidden_solution_from_determined_args(
                &full_args,
                &implementation.interface_args,
                determined_start,
                hidden_names,
            )
        {
            candidates.push(solution);
        }
    }
    candidates
}

fn hidden_candidates_from_generic_impls(
    ctx: &TyCtx,
    interface_def: DefId,
    determined_start: usize,
    full_args: &[Ty],
    hidden_names: &HashSet<String>,
) -> Vec<Vec<(String, Ty)>> {
    let mut candidates = Vec::new();
    for template in &ctx.generic_impls {
        if template.interface_def != interface_def
            || template.interface_args.len() != full_args.len()
        {
            continue;
        }
        let mut subst = template
            .generics
            .iter()
            .map(|generic| (generic.name.clone(), Ty::Generic(generic.name.clone())))
            .collect::<HashMap<_, _>>();
        if !template
            .interface_args
            .iter()
            .zip(full_args.iter())
            .take(determined_start)
            .all(|(pattern, actual)| unify_ty(pattern, actual, &mut subst))
        {
            continue;
        }
        if template
            .generics
            .iter()
            .any(|generic| subst.get(&generic.name).is_none_or(contains_generic))
        {
            continue;
        }
        if !generic_impl_constraints_satisfied(ctx, template, &subst) {
            continue;
        }
        let candidate = template
            .interface_args
            .iter()
            .map(|ty| substitute_ty(ty, &subst))
            .collect::<Vec<_>>();
        if let Some(solution) = hidden_solution_from_determined_args(
            full_args,
            &candidate,
            determined_start,
            hidden_names,
        ) {
            candidates.push(solution);
        }
    }
    candidates
}

fn generic_impl_has_unfixed_determined_generics(
    ctx: &TyCtx,
    template: &super::GenericImplTemplate,
    determined_start: usize,
) -> bool {
    let template_generics = template
        .generics
        .iter()
        .map(|generic| generic.name.clone())
        .collect::<HashSet<_>>();
    let mut required = HashSet::new();
    for ty in template.interface_args.iter().skip(determined_start) {
        required.extend(
            ty_generic_names(ty)
                .into_iter()
                .filter(|name| template_generics.contains(name)),
        );
    }
    if required.is_empty() {
        return false;
    }

    let mut known = HashSet::new();
    for ty in template.interface_args.iter().take(determined_start) {
        known.extend(
            ty_generic_names(ty)
                .into_iter()
                .filter(|name| template_generics.contains(name)),
        );
    }

    for _ in 0..=template.generics.len() {
        let before = known.clone();
        for generic_constraint in &template.generic_constraints {
            if !known.contains(&generic_constraint.name) {
                continue;
            }
            for capability in &generic_constraint.bounds.positive {
                let Some(interface) = ctx.interfaces.get(&capability.def_id) else {
                    continue;
                };
                let Some(inner_determined_start) = interface.determined_start else {
                    continue;
                };
                let full_args = std::iter::once(Ty::Generic(generic_constraint.name.clone()))
                    .chain(capability.args.iter().cloned())
                    .collect::<Vec<_>>();
                if full_args.len() != interface.generics.len() {
                    continue;
                }
                if !full_args
                    .iter()
                    .take(inner_determined_start)
                    .all(|ty| ty_generic_names(ty).is_subset(&known))
                {
                    continue;
                }
                for ty in full_args.iter().skip(inner_determined_start) {
                    known.extend(
                        ty_generic_names(ty)
                            .into_iter()
                            .filter(|name| template_generics.contains(name)),
                    );
                }
            }
        }
        if known == before {
            break;
        }
    }

    !required.is_subset(&known)
}

fn generic_impl_constraints_satisfied(
    ctx: &TyCtx,
    template: &super::GenericImplTemplate,
    subst: &HashMap<String, Ty>,
) -> bool {
    let mut stack = HashSet::new();
    generic_impl_constraints_satisfied_inner(ctx, template, subst, &mut stack)
}

fn generic_impl_constraints_satisfied_inner(
    ctx: &TyCtx,
    template: &super::GenericImplTemplate,
    subst: &HashMap<String, Ty>,
    stack: &mut HashSet<CapabilityCheckKey>,
) -> bool {
    template
        .generic_constraints
        .iter()
        .all(|generic_constraint| {
            let Some(concrete) = subst.get(&generic_constraint.name) else {
                return false;
            };
            if generic_constraint.is_resource && !ty_is_affine_readonly(ctx, concrete) {
                return false;
            }
            let bounds = substitute_constraint_bounds(&generic_constraint.bounds, subst);
            constraint_bounds_satisfied(ctx, concrete, &bounds, stack)
        })
}

fn constraint_bounds_satisfied(
    ctx: &TyCtx,
    receiver_ty: &Ty,
    bounds: &ConstraintBounds,
    stack: &mut HashSet<CapabilityCheckKey>,
) -> bool {
    bounds.positive.iter().all(|capability| {
        capability_satisfied(
            ctx,
            capability.def_id,
            &capability.name,
            &capability.args,
            receiver_ty,
            stack,
        )
    }) && bounds.negative.iter().all(|capability| {
        !capability_satisfied(
            ctx,
            capability.def_id,
            &capability.name,
            &capability.args,
            receiver_ty,
            stack,
        )
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CapabilityCheckKey {
    interface_def: DefId,
    interface_name: String,
    args: Vec<Ty>,
    receiver_ty: Ty,
}

fn capability_satisfied(
    ctx: &TyCtx,
    interface_def: DefId,
    interface_name: &str,
    args: &[Ty],
    receiver_ty: &Ty,
    stack: &mut HashSet<CapabilityCheckKey>,
) -> bool {
    let key = CapabilityCheckKey {
        interface_def,
        interface_name: interface_name.to_string(),
        args: args.to_vec(),
        receiver_ty: receiver_ty.clone(),
    };
    if !stack.insert(key.clone()) {
        return false;
    }
    let satisfied =
        capability_satisfied_inner(ctx, interface_def, interface_name, args, receiver_ty, stack);
    stack.remove(&key);
    satisfied
}

fn capability_satisfied_inner(
    ctx: &TyCtx,
    interface_def: DefId,
    interface_name: &str,
    args: &[Ty],
    receiver_ty: &Ty,
    stack: &mut HashSet<CapabilityCheckKey>,
) -> bool {
    let _ = interface_name;
    if ctx.impls.iter().any(|implementation| {
        implementation.interface_def == interface_def
            && interface_non_receiver_args(&implementation.interface_args) == args
            && implementation
                .receiver_ty
                .as_ref()
                .is_some_and(|candidate| candidate == receiver_ty)
    }) {
        return true;
    }

    let full_args = std::iter::once(receiver_ty.clone())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>();
    ctx.generic_impls.iter().any(|template| {
        if template.interface_def != interface_def
            || template.interface_args.len() != full_args.len()
        {
            return false;
        }
        let mut subst = template
            .generics
            .iter()
            .map(|generic| (generic.name.clone(), Ty::Generic(generic.name.clone())))
            .collect::<HashMap<_, _>>();
        if !template
            .interface_args
            .iter()
            .zip(full_args.iter())
            .all(|(pattern, actual)| unify_ty(pattern, actual, &mut subst))
        {
            return false;
        }
        if template
            .generics
            .iter()
            .any(|generic| subst.get(&generic.name).is_none_or(contains_generic))
        {
            return false;
        }
        generic_impl_constraints_satisfied_inner(ctx, template, &subst, stack)
    })
}

fn ty_is_affine_readonly(ctx: &TyCtx, ty: &Ty) -> bool {
    ty_is_affine_readonly_inner(ctx, ty, &mut HashSet::new())
}

fn ty_is_affine_readonly_inner(ctx: &TyCtx, ty: &Ty, visiting: &mut HashSet<Ty>) -> bool {
    match ty {
        Ty::Array { elem, .. } => ty_is_affine_readonly_inner(ctx, elem, visiting),
        Ty::GeneratedFuture { .. } => true,
        Ty::ClosureInstance { captures, .. } => captures
            .iter()
            .any(|capture| ty_is_affine_readonly_inner(ctx, capture, visiting)),
        Ty::Named { name, args } => {
            let named_ty = Ty::Named {
                name: name.clone(),
                args: args.clone(),
            };
            if std_id::std_async_future_output_arg(&ctx.resolved, &named_ty).is_some() {
                return true;
            }
            let instance_name = super::enum_instance_name(name, args);
            if ctx.resource_structs.contains(&instance_name) {
                return true;
            }
            if !visiting.insert(ty.clone()) {
                return false;
            }
            if let Some(fields) = ctx.structs.get(&instance_name)
                && fields
                    .iter()
                    .any(|(_, field_ty)| ty_is_affine_readonly_inner(ctx, field_ty, visiting))
            {
                visiting.remove(ty);
                return true;
            }
            if let Some(enm) = ctx.checked_enums.get(&instance_name)
                && enm.variants.iter().any(|variant| {
                    variant
                        .payload
                        .iter()
                        .any(|payload_ty| ty_is_affine_readonly_inner(ctx, payload_ty, visiting))
                })
            {
                visiting.remove(ty);
                return true;
            }
            visiting.remove(ty);
            false
        }
        _ => false,
    }
}

fn unique_candidate(mut candidates: Vec<Vec<(String, Ty)>>) -> HiddenSolveResult {
    candidates.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
    candidates.dedup();
    match candidates.len() {
        0 => HiddenSolveResult::NoSolution,
        1 => HiddenSolveResult::Unique(candidates.remove(0)),
        _ => HiddenSolveResult::Ambiguous,
    }
}

fn determinants_match(required: &[Ty], candidate: &[Ty], determined_start: usize) -> bool {
    required.len() == candidate.len()
        && required
            .iter()
            .zip(candidate.iter())
            .take(determined_start)
            .all(|(required, candidate)| known_ty_matches(required, candidate))
}

fn hidden_solution_from_determined_args(
    required: &[Ty],
    candidate: &[Ty],
    determined_start: usize,
    hidden_names: &HashSet<String>,
) -> Option<Vec<(String, Ty)>> {
    if required.len() != candidate.len() {
        return None;
    }
    let mut solved = HashMap::new();
    for (required, candidate) in required.iter().zip(candidate.iter()).skip(determined_start) {
        bind_hidden_ty(required, candidate, hidden_names, &mut solved)?;
    }
    let mut solved = solved.into_iter().collect::<Vec<_>>();
    solved.sort_by(|left, right| left.0.cmp(&right.0));
    Some(solved)
}

fn bind_hidden_ty(
    required: &Ty,
    candidate: &Ty,
    hidden_names: &HashSet<String>,
    solved: &mut HashMap<String, Ty>,
) -> Option<()> {
    match required {
        Ty::Generic(name) if hidden_names.contains(name) => match solved.get(name) {
            Some(existing) if known_ty_matches(existing, candidate) => Some(()),
            Some(_) => None,
            None => {
                solved.insert(name.clone(), candidate.clone());
                Some(())
            }
        },
        Ty::Pointer {
            nullable,
            mutability,
            inner,
        } => match candidate {
            Ty::Pointer {
                nullable: candidate_nullable,
                mutability: candidate_mutability,
                inner: candidate_inner,
            } if nullable == candidate_nullable && mutability == candidate_mutability => {
                bind_hidden_ty(inner, candidate_inner, hidden_names, solved)
            }
            _ => None,
        },
        Ty::Array { len, elem } => match candidate {
            Ty::Array {
                len: candidate_len,
                elem: candidate_elem,
            } if len == candidate_len => bind_hidden_ty(elem, candidate_elem, hidden_names, solved),
            _ => None,
        },
        Ty::Slice { mutability, elem } => match candidate {
            Ty::Slice {
                mutability: candidate_mutability,
                elem: candidate_elem,
            } if mutability == candidate_mutability => {
                bind_hidden_ty(elem, candidate_elem, hidden_names, solved)
            }
            _ => None,
        },
        Ty::Named { name, args } => match candidate {
            Ty::Named {
                name: candidate_name,
                args: candidate_args,
            } if name == candidate_name && args.len() == candidate_args.len() => args
                .iter()
                .zip(candidate_args.iter())
                .try_for_each(|(required, candidate)| {
                    bind_hidden_ty(required, candidate, hidden_names, solved)
                }),
            _ => None,
        },
        Ty::DynamicInterface { def_id, args, .. } => match candidate {
            Ty::DynamicInterface {
                def_id: candidate_def_id,
                args: candidate_args,
                ..
            } if def_id == candidate_def_id && args.len() == candidate_args.len() => args
                .iter()
                .zip(candidate_args.iter())
                .try_for_each(|(required, candidate)| {
                    bind_hidden_ty(required, candidate, hidden_names, solved)
                }),
            _ => None,
        },
        Ty::Function {
            is_unsafe,
            abi,
            ret,
            params,
        } => match candidate {
            Ty::Function {
                is_unsafe: candidate_is_unsafe,
                abi: candidate_abi,
                ret: candidate_ret,
                params: candidate_params,
            } if is_unsafe == candidate_is_unsafe
                && abi == candidate_abi
                && params.len() == candidate_params.len() =>
            {
                bind_hidden_ty(ret, candidate_ret, hidden_names, solved)?;
                params
                    .iter()
                    .zip(candidate_params.iter())
                    .try_for_each(|(required, candidate)| {
                        bind_hidden_ty(required, candidate, hidden_names, solved)
                    })
            }
            _ => None,
        },
        Ty::Closure {
            ret,
            params,
            constraints,
        } => match candidate {
            Ty::Closure {
                ret: candidate_ret,
                params: candidate_params,
                constraints: candidate_constraints,
            } if constraints == candidate_constraints && params.len() == candidate_params.len() => {
                bind_hidden_ty(ret, candidate_ret, hidden_names, solved)?;
                params
                    .iter()
                    .zip(candidate_params.iter())
                    .try_for_each(|(required, candidate)| {
                        bind_hidden_ty(required, candidate, hidden_names, solved)
                    })
            }
            _ => None,
        },
        _ if known_ty_matches(required, candidate) => Some(()),
        _ => None,
    }
}

fn overlapping_determined_patterns_equivalent(
    left: &[Ty],
    right: &[Ty],
    determined_start: usize,
) -> Option<bool> {
    let left = left
        .iter()
        .map(|ty| coherence_scope_ty(ty, "coherence:left:"))
        .collect::<Vec<_>>();
    let right = right
        .iter()
        .map(|ty| coherence_scope_ty(ty, "coherence:right:"))
        .collect::<Vec<_>>();
    let mut subst = HashMap::new();
    for (left, right) in left.iter().zip(right.iter()).take(determined_start) {
        if !coherence_unify(left, right, &mut subst) {
            return None;
        }
    }
    Some(
        left.iter()
            .zip(right.iter())
            .skip(determined_start)
            .all(|(left, right)| {
                let left = coherence_normalize_ty(left, &subst);
                let right = coherence_normalize_ty(right, &subst);
                known_ty_matches(&left, &right)
            }),
    )
}

fn coherence_scope_ty(ty: &Ty, prefix: &str) -> Ty {
    let subst = ty_generic_names(ty)
        .into_iter()
        .map(|name| {
            let scoped_name = format!("{prefix}{name}");
            (name, Ty::Generic(scoped_name))
        })
        .collect::<HashMap<_, _>>();
    substitute_ty(ty, &subst)
}

fn coherence_unify(left: &Ty, right: &Ty, subst: &mut HashMap<String, Ty>) -> bool {
    let left = coherence_normalize_ty(left, subst);
    let right = coherence_normalize_ty(right, subst);
    match (&left, &right) {
        (Ty::Generic(left), Ty::Generic(right)) if left == right => true,
        (Ty::Generic(name), ty) | (ty, Ty::Generic(name)) => {
            coherence_bind_generic(name, ty, subst)
        }
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
                && coherence_unify(inner, right_inner, subst)
        }
        (
            Ty::Array { len, elem },
            Ty::Array {
                len: right_len,
                elem: right_elem,
            },
        ) => len == right_len && coherence_unify(elem, right_elem, subst),
        (
            Ty::Slice { mutability, elem },
            Ty::Slice {
                mutability: right_mutability,
                elem: right_elem,
            },
        ) => mutability == right_mutability && coherence_unify(elem, right_elem, subst),
        (
            Ty::Named { name, args },
            Ty::Named {
                name: right_name,
                args: right_args,
            },
        ) => {
            name == right_name
                && args.len() == right_args.len()
                && args
                    .iter()
                    .zip(right_args.iter())
                    .all(|(left, right)| coherence_unify(left, right, subst))
        }
        (
            Ty::DynamicInterface { def_id, args, .. },
            Ty::DynamicInterface {
                def_id: right_def_id,
                args: right_args,
                ..
            },
        ) => {
            def_id == right_def_id
                && args.len() == right_args.len()
                && args
                    .iter()
                    .zip(right_args.iter())
                    .all(|(left, right)| coherence_unify(left, right, subst))
        }
        (
            Ty::GeneratedFuture { name, output, .. },
            Ty::GeneratedFuture {
                name: right_name,
                output: right_output,
                ..
            },
        ) => name == right_name && coherence_unify(output, right_output, subst),
        (
            Ty::OpaqueReturn { key, bounds },
            Ty::OpaqueReturn {
                key: right_key,
                bounds: right_bounds,
            },
        ) => {
            key.def_id == right_key.def_id
                && key.args.len() == right_key.args.len()
                && key
                    .args
                    .iter()
                    .zip(right_key.args.iter())
                    .all(|(left, right)| coherence_unify(left, right, subst))
                && coherence_constraint_bounds_match(bounds, right_bounds, subst)
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
                && coherence_unify(ret, right_ret, subst)
                && params
                    .iter()
                    .zip(right_params.iter())
                    .all(|(left, right)| coherence_unify(left, right, subst))
        }
        (
            Ty::Closure {
                ret,
                params,
                constraints,
            },
            Ty::Closure {
                ret: right_ret,
                params: right_params,
                constraints: right_constraints,
            },
        ) => {
            params.len() == right_params.len()
                && coherence_constraint_bounds_match(constraints, right_constraints, subst)
                && coherence_unify(ret, right_ret, subst)
                && params
                    .iter()
                    .zip(right_params.iter())
                    .all(|(left, right)| coherence_unify(left, right, subst))
        }
        (
            Ty::ClosureInstance {
                id,
                ret,
                params,
                captures,
            },
            Ty::ClosureInstance {
                id: right_id,
                ret: right_ret,
                params: right_params,
                captures: right_captures,
            },
        ) => {
            id == right_id
                && params.len() == right_params.len()
                && captures.len() == right_captures.len()
                && coherence_unify(ret, right_ret, subst)
                && params
                    .iter()
                    .zip(right_params.iter())
                    .all(|(left, right)| coherence_unify(left, right, subst))
                && captures
                    .iter()
                    .zip(right_captures.iter())
                    .all(|(left, right)| coherence_unify(left, right, subst))
        }
        _ => left == right,
    }
}

fn coherence_bind_generic(name: &str, ty: &Ty, subst: &mut HashMap<String, Ty>) -> bool {
    let ty = coherence_normalize_ty(ty, subst);
    if matches!(&ty, Ty::Generic(other) if other == name) {
        return true;
    }
    if ty_generic_names(&ty).contains(name) {
        return false;
    }
    subst.insert(name.to_string(), ty);
    true
}

fn coherence_constraint_bounds_match(
    left: &ConstraintBounds,
    right: &ConstraintBounds,
    subst: &mut HashMap<String, Ty>,
) -> bool {
    left.positive.len() == right.positive.len()
        && left.negative.len() == right.negative.len()
        && left
            .positive
            .iter()
            .zip(right.positive.iter())
            .all(|(left, right)| coherence_constraint_ref_match(left, right, subst))
        && left
            .negative
            .iter()
            .zip(right.negative.iter())
            .all(|(left, right)| coherence_constraint_ref_match(left, right, subst))
}

fn coherence_constraint_ref_match(
    left: &ConstraintRef,
    right: &ConstraintRef,
    subst: &mut HashMap<String, Ty>,
) -> bool {
    left.def_id == right.def_id
        && left.args.len() == right.args.len()
        && left
            .args
            .iter()
            .zip(right.args.iter())
            .all(|(left, right)| coherence_unify(left, right, subst))
}

fn coherence_normalize_ty(ty: &Ty, subst: &HashMap<String, Ty>) -> Ty {
    coherence_normalize_ty_inner(ty, subst, &mut HashSet::new())
}

fn coherence_normalize_ty_inner(
    ty: &Ty,
    subst: &HashMap<String, Ty>,
    visiting: &mut HashSet<String>,
) -> Ty {
    match ty {
        Ty::Generic(name) => {
            let Some(replacement) = subst.get(name) else {
                return Ty::Generic(name.clone());
            };
            if !visiting.insert(name.clone()) {
                return Ty::Generic(name.clone());
            }
            let normalized = coherence_normalize_ty_inner(replacement, subst, visiting);
            visiting.remove(name);
            normalized
        }
        Ty::Closure {
            ret,
            params,
            constraints,
        } => Ty::Closure {
            ret: Box::new(coherence_normalize_ty_inner(ret, subst, visiting)),
            params: params
                .iter()
                .map(|param| coherence_normalize_ty_inner(param, subst, visiting))
                .collect(),
            constraints: coherence_normalize_constraint_bounds(constraints, subst, visiting),
        },
        other => map_ty_children(other, |child| {
            coherence_normalize_ty_inner(child, subst, visiting)
        }),
    }
}

fn coherence_normalize_constraint_bounds(
    bounds: &ConstraintBounds,
    subst: &HashMap<String, Ty>,
    visiting: &mut HashSet<String>,
) -> ConstraintBounds {
    ConstraintBounds {
        positive: bounds
            .positive
            .iter()
            .map(|entry| coherence_normalize_constraint_ref(entry, subst, visiting))
            .collect(),
        negative: bounds
            .negative
            .iter()
            .map(|entry| coherence_normalize_constraint_ref(entry, subst, visiting))
            .collect(),
    }
}

fn coherence_normalize_constraint_ref(
    entry: &ConstraintRef,
    subst: &HashMap<String, Ty>,
    visiting: &mut HashSet<String>,
) -> ConstraintRef {
    ConstraintRef {
        def_id: entry.def_id,
        name: entry.name.clone(),
        args: entry
            .args
            .iter()
            .map(|arg| coherence_normalize_ty_inner(arg, subst, visiting))
            .collect(),
    }
}
