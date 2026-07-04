pub mod manifest;
pub mod native;
pub(crate) mod package;
pub(crate) mod planner;
mod requirements;

use std::env;

pub use requirements::{BuildPlan, BuildProfile, CmakeTarget};

const DEFAULT_C_COMPILER: &str = "clang";

pub fn default_c_compiler() -> String {
    default_c_compiler_from(env::var("CC").ok())
}

pub fn default_c_compiler_from(cc_env: Option<String>) -> String {
    cc_env
        .filter(|compiler| !compiler.is_empty())
        .unwrap_or_else(|| DEFAULT_C_COMPILER.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_c_compiler_is_clang_without_cc_env() {
        assert_eq!(default_c_compiler_from(None), "clang");
        assert_eq!(default_c_compiler_from(Some(String::new())), "clang");
    }

    #[test]
    fn cc_env_overrides_default_c_compiler() {
        assert_eq!(
            default_c_compiler_from(Some("custom-clang".to_string())),
            "custom-clang"
        );
    }
}
