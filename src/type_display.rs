use crate::{resolve::ResolvedProgram, std_id, types::Ty};

pub fn result_args<'a>(resolved: &ResolvedProgram, ty: &'a Ty) -> Option<(&'a Ty, &'a Ty)> {
    if let Ty::OpaqueState { base, .. } = ty {
        return result_args(resolved, base);
    }
    let Ty::Named { def_id, name, args } = ty else {
        return None;
    };
    let is_std_result = def_id.is_some_and(|def_id| std_id::is_std_result_enum(resolved, def_id))
        || (def_id.is_none() && std_id::is_std_result_type_name(resolved, name));
    if args.len() == 2 && is_std_result {
        Some((&args[0], &args[1]))
    } else {
        None
    }
}
