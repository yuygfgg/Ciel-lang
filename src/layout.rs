use std::collections::{HashMap, HashSet};

use crate::{
    diagnostic::Diagnostic,
    thir::{CheckedEnum, CheckedStruct},
    types::{Ty, aggregate_instance_name},
};

#[derive(Default)]
struct LayoutGraph {
    edges: HashMap<String, HashSet<String>>,
    display_names: HashMap<String, String>,
}

pub fn check_checked_aggregate_layouts(
    structs: &[CheckedStruct],
    enums: &[CheckedEnum],
) -> Vec<Diagnostic> {
    let aggregate_names = structs
        .iter()
        .map(|structure| structure.name.clone())
        .chain(enums.iter().map(|enm| enm.name.clone()))
        .collect::<HashSet<_>>();
    let mut graph = LayoutGraph::default();

    for structure in structs {
        graph.ensure_node(&structure.name);
        let mut edges = HashSet::new();
        for (_, field_ty) in &structure.fields {
            collect_layout_edges_from_ty(field_ty, &aggregate_names, &mut graph, &mut edges);
        }
        graph.edges.insert(structure.name.clone(), edges);
    }

    for enm in enums {
        graph.ensure_node(&enm.name);
        let mut edges = HashSet::new();
        for variant in &enm.variants {
            for payload_ty in &variant.payload {
                collect_layout_edges_from_ty(payload_ty, &aggregate_names, &mut graph, &mut edges);
            }
        }
        graph.edges.insert(enm.name.clone(), edges);
    }

    graph.detect_cycles()
}

impl LayoutGraph {
    fn ensure_node(&mut self, name: &str) {
        self.edges.entry(name.to_string()).or_default();
        self.display_names
            .entry(name.to_string())
            .or_insert_with(|| name.to_string());
    }

    fn set_display_name(&mut self, name: &str, display: String) {
        self.display_names.insert(name.to_string(), display);
    }

    fn display_name(&self, name: &str) -> String {
        self.display_names
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    fn detect_cycles(&self) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        let mut visiting = Vec::<String>::new();
        let mut visited = HashSet::<String>::new();
        let mut names = self.edges.keys().cloned().collect::<Vec<_>>();
        names.sort();
        for name in names {
            self.detect_cycle_from(&name, &mut visiting, &mut visited, &mut diagnostics);
        }
        diagnostics
    }

    fn detect_cycle_from(
        &self,
        name: &str,
        visiting: &mut Vec<String>,
        visited: &mut HashSet<String>,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        if visited.contains(name) {
            return;
        }
        if let Some(pos) = visiting.iter().position(|entry| entry == name) {
            let mut cycle = visiting[pos..].to_vec();
            cycle.push(name.to_string());
            let display = self.display_name(name);
            let cycle_display = cycle
                .iter()
                .map(|entry| self.display_name(entry))
                .collect::<Vec<_>>()
                .join(" -> ");
            diagnostics.push(
                Diagnostic::new(
                    None,
                    format!(
                        "recursive by-value type is not supported: `{display}` (layout cycle: {cycle_display})"
                    ),
                ),
            );
            return;
        }

        visiting.push(name.to_string());
        if let Some(edges) = self.edges.get(name) {
            let mut edges = edges.iter().cloned().collect::<Vec<_>>();
            edges.sort();
            for next in edges {
                self.detect_cycle_from(&next, visiting, visited, diagnostics);
            }
        }
        visiting.pop();
        visited.insert(name.to_string());
    }
}

fn collect_layout_edges_from_ty(
    ty: &Ty,
    aggregate_names: &HashSet<String>,
    graph: &mut LayoutGraph,
    edges: &mut HashSet<String>,
) {
    match ty {
        Ty::Named { name, args } => {
            let instance_name = aggregate_instance_name(name, args);
            if aggregate_names.contains(&instance_name) {
                graph.ensure_node(&instance_name);
                graph.set_display_name(&instance_name, ty.to_string());
                edges.insert(instance_name);
            }
        }
        Ty::Array { elem, .. } => {
            collect_layout_edges_from_ty(elem, aggregate_names, graph, edges);
        }
        Ty::Pointer { .. }
        | Ty::Slice { .. }
        | Ty::Function { .. }
        | Ty::Closure { .. }
        | Ty::ClosureInstance { .. }
        | Ty::DynamicInterface { .. }
        | Ty::Hole(_)
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
        | Ty::Unknown => {}
    }
}
