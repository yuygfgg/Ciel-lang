use std::collections::HashMap;

use crate::{
    resolve::DefId,
    types::{Ty, contains_generic, named_ty_identity_eq, unify_ty},
};

use super::{
    CompilerMarkerDomain, GenericImplTemplate, ImplSig, TyCtx, interface_non_receiver_args,
};

pub struct CapabilityTable<'ctx> {
    ctx: &'ctx TyCtx,
}

impl<'ctx> CapabilityTable<'ctx> {
    pub(super) fn new(ctx: &'ctx TyCtx) -> Self {
        Self { ctx }
    }

    pub(super) fn impls(&self) -> &'ctx [ImplSig] {
        &self.ctx.impls
    }

    pub(super) fn generic_impls(&self) -> &'ctx [GenericImplTemplate] {
        &self.ctx.generic_impls
    }

    pub(super) fn ctx(&self) -> &'ctx TyCtx {
        self.ctx
    }

    pub(super) fn find_impl(
        &self,
        interface_def: DefId,
        args: &[Ty],
        receiver_ty: &Ty,
    ) -> Option<&'ctx ImplSig> {
        self.ctx.impls.iter().find(|implementation| {
            implementation.interface_def == interface_def
                && implementation
                    .receiver_ty
                    .as_ref()
                    .is_some_and(|candidate| candidate == receiver_ty)
                && interface_non_receiver_args(&implementation.interface_args) == args
        })
    }

    pub(super) fn find_impl_by_full_args(
        &self,
        interface_def: DefId,
        interface_args: &[Ty],
        receiver_ty: Option<&Ty>,
    ) -> Option<&'ctx ImplSig> {
        find_impl_in(&self.ctx.impls, interface_def, interface_args, receiver_ty)
    }

    pub(super) fn generic_impl_matches_without_constraints(
        &self,
        interface_def: DefId,
        non_receiver_args: &[Ty],
        receiver_ty: &Ty,
    ) -> bool {
        let interface_args = std::iter::once(receiver_ty.clone())
            .chain(non_receiver_args.iter().cloned())
            .collect::<Vec<_>>();
        self.ctx.generic_impls.iter().any(|template| {
            if template.interface_def != interface_def
                || template.interface_args.len() != interface_args.len()
            {
                return false;
            }
            if template
                .generics
                .iter()
                .any(|generic| generic.constraint.is_some())
            {
                return false;
            }
            let mut subst = template
                .generics
                .iter()
                .map(|generic| (generic.name.clone(), Ty::Generic(generic.name.clone())))
                .collect::<HashMap<_, _>>();
            template
                .interface_args
                .iter()
                .zip(interface_args.iter())
                .all(|(pattern, actual)| unify_ty(pattern, actual, &mut subst))
                && template
                    .receiver_ty
                    .as_ref()
                    .is_some_and(|pattern| unify_ty(pattern, receiver_ty, &mut subst))
                && template.generics.iter().all(|generic| {
                    subst
                        .get(&generic.name)
                        .is_some_and(|ty| !contains_generic(ty))
                })
        })
    }
}

pub(super) fn find_impl_in<'a>(
    impls: &'a [ImplSig],
    interface_def: DefId,
    interface_args: &[Ty],
    receiver_ty: Option<&Ty>,
) -> Option<&'a ImplSig> {
    impls.iter().find(|implementation| {
        implementation.interface_def == interface_def
            && implementation.interface_args == interface_args
            && match (implementation.receiver_ty.as_ref(), receiver_ty) {
                (Some(left), Some(right)) => left == right,
                (None, None) => true,
                (Some(_), None) => true,
                _ => false,
            }
    })
}

pub(super) fn marker_impl_patterns_overlap(
    left_args: &[Ty],
    left_receiver: Option<&Ty>,
    right_args: &[Ty],
    right_receiver: Option<&Ty>,
) -> bool {
    if left_args.len() != right_args.len() {
        return false;
    }
    let receiver_overlaps = match (left_receiver, right_receiver) {
        (Some(left), Some(right)) => marker_ty_patterns_overlap(left, right),
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => true,
    };
    receiver_overlaps
        && left_args
            .iter()
            .zip(right_args.iter())
            .all(|(left, right)| marker_ty_patterns_overlap(left, right))
}

pub(super) fn marker_impl_domains_disjoint(
    left_domain: Option<CompilerMarkerDomain>,
    left_receiver: Option<&Ty>,
    right_domain: Option<CompilerMarkerDomain>,
    right_receiver: Option<&Ty>,
) -> bool {
    match (left_domain, right_domain) {
        (Some(left), Some(right)) if left != right => return true,
        _ => {}
    }

    if let (Some(domain), Some(receiver)) = (left_domain, right_receiver)
        && !ty_can_satisfy_compiler_marker_domain(receiver, domain)
    {
        return true;
    }
    if let (Some(domain), Some(receiver)) = (right_domain, left_receiver)
        && !ty_can_satisfy_compiler_marker_domain(receiver, domain)
    {
        return true;
    }
    false
}

fn ty_can_satisfy_compiler_marker_domain(ty: &Ty, domain: CompilerMarkerDomain) -> bool {
    match (ty, domain) {
        (Ty::Generic(_), _) => true,
        (
            Ty::Function {
                is_unsafe: false,
                abi: None,
                ..
            },
            CompilerMarkerDomain::CielFnValue,
        ) => true,
        (Ty::ClosureInstance { .. }, CompilerMarkerDomain::ClosureValue) => true,
        _ => false,
    }
}

fn marker_ty_patterns_overlap(left: &Ty, right: &Ty) -> bool {
    match (left, right) {
        (Ty::Generic(_), _) | (_, Ty::Generic(_)) => true,
        (
            Ty::Pointer {
                nullable: left_nullable,
                mutability: left_mutability,
                inner: left_inner,
            },
            Ty::Pointer {
                nullable: right_nullable,
                mutability: right_mutability,
                inner: right_inner,
            },
        ) => {
            left_nullable == right_nullable
                && left_mutability == right_mutability
                && marker_ty_patterns_overlap(left_inner, right_inner)
        }
        (
            Ty::Array {
                len: left_len,
                elem: left_elem,
            },
            Ty::Array {
                len: right_len,
                elem: right_elem,
            },
        ) => left_len == right_len && marker_ty_patterns_overlap(left_elem, right_elem),
        (
            Ty::Slice {
                mutability: left_mutability,
                elem: left_elem,
            },
            Ty::Slice {
                mutability: right_mutability,
                elem: right_elem,
            },
        ) => {
            left_mutability == right_mutability && marker_ty_patterns_overlap(left_elem, right_elem)
        }
        (
            Ty::Named {
                def_id: left_def_id,
                name: left_name,
                args: left_args,
            },
            Ty::Named {
                def_id: right_def_id,
                name: right_name,
                args: right_args,
            },
        ) => {
            named_ty_identity_eq(*left_def_id, left_name, *right_def_id, right_name)
                && left_args.len() == right_args.len()
                && left_args
                    .iter()
                    .zip(right_args.iter())
                    .all(|(left, right)| marker_ty_patterns_overlap(left, right))
        }
        (
            Ty::DynamicInterface {
                def_id: left_def_id,
                args: left_args,
                ..
            },
            Ty::DynamicInterface {
                def_id: right_def_id,
                args: right_args,
                ..
            },
        ) => {
            left_def_id == right_def_id
                && left_args.len() == right_args.len()
                && left_args
                    .iter()
                    .zip(right_args.iter())
                    .all(|(left, right)| marker_ty_patterns_overlap(left, right))
        }
        (
            Ty::Function {
                is_unsafe: left_is_unsafe,
                abi: left_abi,
                ret: left_ret,
                params: left_params,
            },
            Ty::Function {
                is_unsafe: right_is_unsafe,
                abi: right_abi,
                ret: right_ret,
                params: right_params,
            },
        ) => {
            left_is_unsafe == right_is_unsafe
                && left_abi == right_abi
                && left_params.len() == right_params.len()
                && marker_ty_patterns_overlap(left_ret, right_ret)
                && left_params
                    .iter()
                    .zip(right_params.iter())
                    .all(|(left, right)| marker_ty_patterns_overlap(left, right))
        }
        (
            Ty::Closure {
                ret: left_ret,
                params: left_params,
                ..
            },
            Ty::Closure {
                ret: right_ret,
                params: right_params,
                ..
            },
        ) => {
            left_params.len() == right_params.len()
                && marker_ty_patterns_overlap(left_ret, right_ret)
                && left_params
                    .iter()
                    .zip(right_params.iter())
                    .all(|(left, right)| marker_ty_patterns_overlap(left, right))
        }
        (
            Ty::ClosureInstance {
                id: left_id,
                ret: left_ret,
                params: left_params,
                captures: left_captures,
            },
            Ty::ClosureInstance {
                id: right_id,
                ret: right_ret,
                params: right_params,
                captures: right_captures,
            },
        ) => {
            left_id == right_id
                && left_params.len() == right_params.len()
                && left_captures.len() == right_captures.len()
                && marker_ty_patterns_overlap(left_ret, right_ret)
                && left_params
                    .iter()
                    .zip(right_params.iter())
                    .all(|(left, right)| marker_ty_patterns_overlap(left, right))
                && left_captures
                    .iter()
                    .zip(right_captures.iter())
                    .all(|(left, right)| marker_ty_patterns_overlap(left, right))
        }
        (left, right) => left == right,
    }
}
