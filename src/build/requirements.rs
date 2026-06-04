use std::{collections::HashSet, hash::Hash, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuildProfile {
    Debug,
    Release,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CmakeTarget {
    pub package_root: PathBuf,
    pub cmake_file: PathBuf,
    pub target: String,
    pub requires_allow_native_build: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildPlan {
    pub generated_c: String,
    pub profile: BuildProfile,
    pub allow_native_build: bool,
    pub cmake_targets: Vec<CmakeTarget>,
    pub package_inputs: Vec<PathBuf>,
}

impl BuildPlan {
    pub fn new(generated_c: String, profile: BuildProfile, allow_native_build: bool) -> Self {
        Self {
            generated_c,
            profile,
            allow_native_build,
            cmake_targets: Vec::new(),
            package_inputs: Vec::new(),
        }
    }

    pub fn deduplicate(&mut self) {
        dedupe_vec(&mut self.cmake_targets);
        dedupe_vec(&mut self.package_inputs);
    }
}

fn dedupe_vec<T>(items: &mut Vec<T>)
where
    T: Eq + Hash + Clone,
{
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(item.clone()));
}
