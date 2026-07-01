use crate::{resolve::ResolvedProgram, std_id, types::Ty};

pub fn result_args<'a>(resolved: &ResolvedProgram, ty: &'a Ty) -> Option<(&'a Ty, &'a Ty)> {
    let Ty::Named { name, args } = ty else {
        return None;
    };
    if args.len() == 2 && std_id::is_std_result_type_name(resolved, name) {
        Some((&args[0], &args[1]))
    } else {
        None
    }
}
