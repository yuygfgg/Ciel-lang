use std::fmt::Display;

use crate::ast::BindingMutability;

pub fn format_binding_name(name: &str, mutability: BindingMutability) -> String {
    match mutability {
        BindingMutability::Immutable => name.to_string(),
        BindingMutability::Mutable => format!("@{name}"),
    }
}

pub fn format_typed_binding<T: Display>(
    ty: &T,
    name: &str,
    mutability: BindingMutability,
) -> String {
    format!("{} {}", ty, format_binding_name(name, mutability))
}

pub fn format_function_signature<T, I, S>(is_async: bool, ret: &T, name: &str, params: I) -> String
where
    T: Display,
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let params = params
        .into_iter()
        .map(|param| param.as_ref().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let async_prefix = if is_async { "async " } else { "" };
    format!("{async_prefix}{ret} {name}({params})")
}
