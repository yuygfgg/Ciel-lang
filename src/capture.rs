use std::collections::HashSet;

use crate::{
    hir::LocalId,
    thir::{
        TClosureBody, TExpr, TExprKind, TForInit, TPattern, ThirVisitor, walk_expr, walk_for_init,
        walk_pattern,
    },
    types::Ty,
};

pub fn collect_closure_capture_ids(
    params: &[(LocalId, String, Ty)],
    body: &TClosureBody,
) -> Vec<LocalId> {
    let mut declared = params
        .iter()
        .map(|(local_id, _, _)| *local_id)
        .collect::<HashSet<_>>();
    DeclaredLocalsCollector {
        declared: &mut declared,
    }
    .visit_closure_body(body);

    let mut captures = Vec::new();
    LocalRefCollector {
        declared: &declared,
        captures: &mut captures,
    }
    .visit_closure_body(body);
    captures
}

struct DeclaredLocalsCollector<'a> {
    declared: &'a mut HashSet<LocalId>,
}

impl ThirVisitor for DeclaredLocalsCollector<'_> {
    fn visit_for_init(&mut self, init: &TForInit) {
        if let TForInit::VarDecl { local_id, .. } = init {
            self.declared.insert(*local_id);
        }
        walk_for_init(self, init);
    }

    fn visit_pattern(&mut self, pattern: &TPattern) {
        if let TPattern::Binding { local_id, .. } = pattern {
            self.declared.insert(*local_id);
        }
        walk_pattern(self, pattern);
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        if let TExprKind::Closure { params, .. } = &expr.kind {
            for (local_id, _, _) in params {
                self.declared.insert(*local_id);
            }
        }
        walk_expr(self, expr);
    }
}

struct LocalRefCollector<'a> {
    declared: &'a HashSet<LocalId>,
    captures: &'a mut Vec<LocalId>,
}

impl ThirVisitor for LocalRefCollector<'_> {
    fn visit_expr(&mut self, expr: &TExpr) {
        if let TExprKind::Local(local_id, _) = &expr.kind
            && !self.declared.contains(local_id)
            && !self.captures.contains(local_id)
        {
            self.captures.push(*local_id);
        }
        walk_expr(self, expr);
    }
}
