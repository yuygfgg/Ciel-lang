use std::collections::{HashMap, HashSet};

use crate::{
    ast::BindingMutability,
    checked::CheckedProgram,
    ciel_display::{format_function_signature, format_typed_binding},
    hir::{self, ItemKind, LocalId, StmtKind},
    resolve::{DefId, DefKind, ModuleId, ResolvedProgram},
    source::SourceMap,
    span::{FileId, Span},
    thir::{self, TExpr, TExprKind, TForInit, TPattern, TStmt, TStmtKind, ThirVisitor},
    types::{Ty, aggregate_instance_name},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CompletionKind {
    Function,
    Variable,
    Parameter,
    Field,
    EnumVariant,
    Type,
    Struct,
    Enum,
    Interface,
    Module,
    Keyword,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionCandidate {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CompletionFacts {
    resolved: ResolvedProgram,
    modules: Vec<hir::Module>,
    local_tys: HashMap<LocalId, Ty>,
    local_mutabilities: HashMap<LocalId, BindingMutability>,
    parameter_locals: HashSet<LocalId>,
    local_scopes: Vec<LocalScopeCandidate>,
    functions: HashMap<DefId, FunctionCompletionInfo>,
    structs: HashMap<String, Vec<(String, Ty)>>,
    enums: HashMap<String, Vec<String>>,
    selectors: Vec<SelectorCompletionInfo>,
    exprs: Vec<ExprTypeFact>,
    struct_literals: Vec<StructLiteralFact>,
    switches: Vec<SwitchFact>,
}

#[derive(Clone, Debug)]
struct LocalScopeCandidate {
    local_id: LocalId,
    name: String,
    declaration_span: Span,
    scope_span: Span,
}

#[derive(Clone, Debug)]
struct FunctionCompletionInfo {
    label: String,
}

#[derive(Clone, Debug)]
struct SelectorCompletionInfo {
    selector: String,
    module: ModuleId,
    exported: bool,
    receiver_ty: Ty,
    detail: String,
}

#[derive(Clone, Debug)]
struct ExprTypeFact {
    span: Span,
    ty: Ty,
}

#[derive(Clone, Debug)]
struct StructLiteralFact {
    span: Span,
    type_name: String,
}

#[derive(Clone, Debug)]
struct SwitchFact {
    span: Span,
    enum_type_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CompletionContext {
    Bare {
        prefix: String,
    },
    Member {
        prefix: String,
        base_end: usize,
        arrow: bool,
    },
    Qualified {
        prefix: String,
        qualifier: Vec<String>,
    },
}

impl CompletionFacts {
    pub fn from_checked(checked: &CheckedProgram) -> Self {
        let mut builder = CompletionFactsBuilder::new(checked);
        builder.collect();
        builder.finish()
    }

    pub fn complete(
        &self,
        source_map: &SourceMap,
        file_id: FileId,
        offset: usize,
    ) -> Vec<CompletionCandidate> {
        let file = source_map.get(file_id);
        let Some(context) = completion_context(&file.text, offset) else {
            return Vec::new();
        };
        let Some(module) = self.module_for_position(file_id, offset) else {
            return Vec::new();
        };
        let mut out = match context {
            CompletionContext::Member {
                prefix,
                base_end,
                arrow,
            } => self.member_candidates(
                &file.text, file_id, offset, base_end, arrow, &prefix, module,
            ),
            CompletionContext::Qualified { prefix, qualifier } => {
                self.qualified_candidates(module, &qualifier, &prefix)
            }
            CompletionContext::Bare { prefix } => {
                if self.looks_like_case_position(&file.text, offset) {
                    self.case_candidates(file_id, offset, &prefix)
                } else if self.looks_like_struct_field_position(&file.text, offset) {
                    let fields = self.struct_literal_field_candidates(file_id, offset, &prefix);
                    if fields.is_empty() {
                        self.bare_candidates(module, file_id, offset, &prefix)
                    } else {
                        fields
                    }
                } else {
                    self.bare_candidates(module, file_id, offset, &prefix)
                }
            }
        };
        dedup_candidates(&mut out);
        out.sort_by(|left, right| {
            kind_rank(&left.kind)
                .cmp(&kind_rank(&right.kind))
                .then_with(|| left.label.cmp(&right.label))
        });
        out
    }

    fn module_for_position(&self, file_id: FileId, offset: usize) -> Option<ModuleId> {
        let mut fallback = None;
        for module in &self.modules {
            for item in &module.items {
                if item.span.file != file_id {
                    continue;
                }
                fallback.get_or_insert(module.id);
                if item.span.start <= offset && offset <= item.span.end {
                    return Some(module.id);
                }
            }
        }
        fallback
    }

    fn bare_candidates(
        &self,
        module: ModuleId,
        file_id: FileId,
        offset: usize,
        prefix: &str,
    ) -> Vec<CompletionCandidate> {
        let mut candidates = Vec::new();
        candidates.extend(self.local_candidates(file_id, offset, prefix));
        candidates.extend(self.visible_def_candidates(module, file_id, offset, prefix));
        candidates.extend(self.visible_variant_candidates(module, file_id, offset, prefix));
        candidates.extend(self.import_alias_candidates(module, prefix));
        candidates.extend(keyword_candidates(prefix));
        candidates
    }

    fn local_candidates(
        &self,
        file_id: FileId,
        offset: usize,
        prefix: &str,
    ) -> Vec<CompletionCandidate> {
        let mut candidates = self
            .local_scopes
            .iter()
            .filter(|local| local.declaration_span.file == file_id)
            .filter(|local| local.declaration_span.start <= offset)
            .filter(|local| local.scope_span.start <= offset && offset <= local.scope_span.end)
            .filter(|local| label_matches_prefix(&local.name, prefix))
            .map(|local| {
                let ty = self
                    .local_tys
                    .get(&local.local_id)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unknown".to_string());
                let mutability = self
                    .local_mutabilities
                    .get(&local.local_id)
                    .copied()
                    .unwrap_or(BindingMutability::Immutable);
                CompletionCandidate {
                    label: local.name.clone(),
                    kind: if self.parameter_locals.contains(&local.local_id) {
                        CompletionKind::Parameter
                    } else {
                        CompletionKind::Variable
                    },
                    detail: Some(format_typed_binding(&ty, &local.name, mutability)),
                }
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.label.cmp(&right.label));
        candidates
    }

    fn visible_def_candidates(
        &self,
        module: ModuleId,
        file_id: FileId,
        offset: usize,
        prefix: &str,
    ) -> Vec<CompletionCandidate> {
        let mut defs = self.resolved.modules[module.0]
            .defs
            .iter()
            .copied()
            .filter(|def_id| {
                let def = self.resolved.def(*def_id);
                def.kind != DefKind::EnumVariant
                    && (def.span.file != file_id || def.span.start <= offset)
            })
            .collect::<Vec<_>>();
        defs.extend(
            self.resolved
                .visible_imported_bare_defs(module, &non_variant_def_kinds()),
        );
        dedup_def_ids(&mut defs);
        defs.into_iter()
            .filter_map(|def_id| self.def_candidate(def_id, prefix))
            .collect()
    }

    fn visible_variant_candidates(
        &self,
        module: ModuleId,
        file_id: FileId,
        offset: usize,
        prefix: &str,
    ) -> Vec<CompletionCandidate> {
        let visible_enums = self.resolved.modules[module.0]
            .defs
            .iter()
            .copied()
            .filter(|def_id| {
                let def = self.resolved.def(*def_id);
                def.kind == DefKind::Enum && (def.span.file != file_id || def.span.start <= offset)
            })
            .collect::<HashSet<_>>();
        let mut variants = self.resolved.modules[module.0]
            .defs
            .iter()
            .copied()
            .filter(|def_id| {
                let def = self.resolved.def(*def_id);
                def.kind == DefKind::EnumVariant
                    && def
                        .parent
                        .is_some_and(|enum_def| visible_enums.contains(&enum_def))
            })
            .collect::<Vec<_>>();
        variants.extend(self.resolved.visible_imported_bare_variants(module));
        dedup_def_ids(&mut variants);
        variants
            .into_iter()
            .filter_map(|def_id| self.def_candidate(def_id, prefix))
            .collect()
    }

    fn import_alias_candidates(&self, module: ModuleId, prefix: &str) -> Vec<CompletionCandidate> {
        self.resolved
            .visible_import_aliases(module)
            .into_iter()
            .filter(|alias| label_matches_prefix(alias, prefix))
            .map(|alias| CompletionCandidate {
                label: alias,
                kind: CompletionKind::Module,
                detail: Some("import alias".to_string()),
            })
            .collect()
    }

    fn qualified_candidates(
        &self,
        module: ModuleId,
        qualifier: &[String],
        prefix: &str,
    ) -> Vec<CompletionCandidate> {
        match qualifier {
            [head] => {
                if let Some(enum_def) = self.visible_enum_by_name(module, head) {
                    return self.enum_variant_candidates(enum_def, prefix, true);
                }
                let mut candidates = self
                    .resolved
                    .visible_qualified_defs(module, head, &non_variant_def_kinds())
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|def_id| self.qualified_def_candidate(head, def_id, prefix))
                    .collect::<Vec<_>>();
                candidates.extend(
                    self.resolved
                        .visible_qualified_variants(module, head)
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|def_id| self.qualified_def_candidate(head, def_id, prefix)),
                );
                candidates
            }
            [alias, enum_name] => {
                if let Some(enum_def) =
                    self.visible_qualified_enum_by_name(module, alias, enum_name)
                {
                    self.enum_variant_candidates(enum_def, prefix, true)
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn visible_enum_by_name(&self, module: ModuleId, name: &str) -> Option<DefId> {
        self.resolved
            .local_def(module, name, &[DefKind::Enum])
            .or_else(|| {
                self.resolved
                    .lookup_imported_bare(module, name, &[DefKind::Enum])
                    .ok()
                    .flatten()
            })
    }

    fn visible_qualified_enum_by_name(
        &self,
        module: ModuleId,
        alias: &str,
        name: &str,
    ) -> Option<DefId> {
        self.resolved
            .lookup_qualified(module, alias, name, &[DefKind::Enum])
            .ok()
            .flatten()
    }

    fn enum_variant_candidates(
        &self,
        enum_def: DefId,
        prefix: &str,
        qualified_detail: bool,
    ) -> Vec<CompletionCandidate> {
        let enum_name = self.resolved.def(enum_def).name.clone();
        self.resolved
            .enum_variant_defs(enum_def)
            .into_iter()
            .filter_map(|def_id| {
                let def = self.resolved.def(def_id);
                label_matches_prefix(&def.name, prefix).then(|| CompletionCandidate {
                    label: def.name.clone(),
                    kind: CompletionKind::EnumVariant,
                    detail: qualified_detail.then(|| format!("{enum_name}::{}", def.name)),
                })
            })
            .collect()
    }

    fn def_candidate(&self, def_id: DefId, prefix: &str) -> Option<CompletionCandidate> {
        let def = self.resolved.def(def_id);
        if !label_matches_prefix(&def.name, prefix) {
            return None;
        }
        Some(CompletionCandidate {
            label: def.name.clone(),
            kind: completion_kind_for_def(&def.kind),
            detail: self.detail_for_def(def_id),
        })
    }

    fn qualified_def_candidate(
        &self,
        alias: &str,
        def_id: DefId,
        prefix: &str,
    ) -> Option<CompletionCandidate> {
        let def = self.resolved.def(def_id);
        if !label_matches_prefix(&def.name, prefix) {
            return None;
        }
        Some(CompletionCandidate {
            label: def.name.clone(),
            kind: completion_kind_for_def(&def.kind),
            detail: Some(format!("{alias}::{}", def.name)),
        })
    }

    fn detail_for_def(&self, def_id: DefId) -> Option<String> {
        if let Some(function) = self.functions.get(&def_id) {
            return Some(function.label.clone());
        }
        let def = self.resolved.def(def_id);
        Some(match def.kind {
            DefKind::TypeAlias => "type alias".to_string(),
            DefKind::Struct => "struct".to_string(),
            DefKind::Enum => "enum".to_string(),
            DefKind::EnumVariant => {
                if let Some(parent) = def.parent {
                    format!("{}::{}", self.resolved.def(parent).name, def.name)
                } else {
                    "enum variant".to_string()
                }
            }
            DefKind::Interface => "interface".to_string(),
            DefKind::InterfaceAlias => "interface alias".to_string(),
            DefKind::Function => "function".to_string(),
            DefKind::ExternFunction => "extern function".to_string(),
            DefKind::OpaqueStruct => "opaque struct".to_string(),
        })
    }

    fn member_candidates(
        &self,
        text: &str,
        file_id: FileId,
        offset: usize,
        base_end: usize,
        arrow: bool,
        prefix: &str,
        module: ModuleId,
    ) -> Vec<CompletionCandidate> {
        let Some(base_ty) = self
            .expr_ty_ending_at(file_id, base_end)
            .or_else(|| self.simple_base_local_ty(text, file_id, offset, base_end))
        else {
            return Vec::new();
        };
        let mut candidates = self.field_candidates_for_ty(&base_ty, arrow, prefix);
        candidates.extend(self.selector_candidates(module, &base_ty, prefix));
        candidates
    }

    fn expr_ty_ending_at(&self, file_id: FileId, base_end: usize) -> Option<Ty> {
        self.exprs
            .iter()
            .filter(|expr| expr.span.file == file_id && expr.span.end == base_end)
            .min_by_key(|expr| expr.span.end.saturating_sub(expr.span.start))
            .map(|expr| expr.ty.clone())
    }

    fn simple_base_local_ty(
        &self,
        text: &str,
        file_id: FileId,
        offset: usize,
        base_end: usize,
    ) -> Option<Ty> {
        let name = identifier_before(text, base_end)?;
        self.local_scopes
            .iter()
            .filter(|local| local.name == name)
            .filter(|local| local.declaration_span.file == file_id)
            .filter(|local| local.declaration_span.start <= offset)
            .filter(|local| local.scope_span.start <= offset && offset <= local.scope_span.end)
            .max_by_key(|local| local.declaration_span.start)
            .and_then(|local| self.local_tys.get(&local.local_id).cloned())
    }

    fn field_candidates_for_ty(
        &self,
        ty: &Ty,
        arrow: bool,
        prefix: &str,
    ) -> Vec<CompletionCandidate> {
        let view = if arrow {
            match ty {
                Ty::Pointer { inner, .. } => inner.as_ref(),
                _ => ty,
            }
        } else {
            ty
        };
        match view {
            Ty::OpaqueState { base, .. } => self.field_candidates_for_ty(base, arrow, prefix),
            Ty::Slice { .. } => ["ptr", "len"]
                .into_iter()
                .filter(|field| label_matches_prefix(field, prefix))
                .map(|field| CompletionCandidate {
                    label: field.to_string(),
                    kind: CompletionKind::Field,
                    detail: Some("slice field".to_string()),
                })
                .collect(),
            Ty::Named { name, args } => {
                let instance_name = aggregate_instance_name(name, args);
                self.structs
                    .get(&instance_name)
                    .or_else(|| self.structs.get(name))
                    .into_iter()
                    .flat_map(|fields| fields.iter())
                    .filter(|(name, _)| label_matches_prefix(name, prefix))
                    .map(|(name, ty)| CompletionCandidate {
                        label: name.clone(),
                        kind: CompletionKind::Field,
                        detail: Some(ty.to_string()),
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn selector_candidates(
        &self,
        module: ModuleId,
        receiver_ty: &Ty,
        prefix: &str,
    ) -> Vec<CompletionCandidate> {
        self.selectors
            .iter()
            .filter(|selector| label_matches_prefix(&selector.selector, prefix))
            .filter(|selector| {
                selector.module == module
                    || (selector.exported && self.selector_import_visible(module, selector.module))
            })
            .filter(|selector| receiver_matches(&selector.receiver_ty, receiver_ty))
            .map(|selector| CompletionCandidate {
                label: selector.selector.clone(),
                kind: CompletionKind::Function,
                detail: Some(selector.detail.clone()),
            })
            .collect()
    }

    fn selector_import_visible(&self, module: ModuleId, target: ModuleId) -> bool {
        let mut visited = HashSet::new();
        self.selector_import_visible_inner(module, target, &mut visited)
    }

    fn selector_import_visible_inner(
        &self,
        module: ModuleId,
        target: ModuleId,
        visited: &mut HashSet<ModuleId>,
    ) -> bool {
        if !visited.insert(module) {
            return false;
        }
        self.resolved.modules[module.0]
            .imports
            .iter()
            .any(|import| {
                import.alias.is_none()
                    && import.target.is_some_and(|import_target| {
                        import_target == target
                            || (import.exported
                                && self.selector_import_visible_inner(
                                    import_target,
                                    target,
                                    visited,
                                ))
                    })
            })
    }

    fn struct_literal_field_candidates(
        &self,
        file_id: FileId,
        offset: usize,
        prefix: &str,
    ) -> Vec<CompletionCandidate> {
        let Some(literal) = self
            .struct_literals
            .iter()
            .filter(|literal| literal.span.file == file_id)
            .filter(|literal| literal.span.start <= offset && offset <= literal.span.end)
            .min_by_key(|literal| literal.span.end.saturating_sub(literal.span.start))
        else {
            return Vec::new();
        };
        self.structs
            .get(&literal.type_name)
            .into_iter()
            .flat_map(|fields| fields.iter())
            .filter(|(name, _)| label_matches_prefix(name, prefix))
            .map(|(name, ty)| CompletionCandidate {
                label: name.clone(),
                kind: CompletionKind::Field,
                detail: Some(ty.to_string()),
            })
            .collect()
    }

    fn case_candidates(
        &self,
        file_id: FileId,
        offset: usize,
        prefix: &str,
    ) -> Vec<CompletionCandidate> {
        let Some(switch) = self
            .switches
            .iter()
            .filter(|switch| switch.span.file == file_id)
            .filter(|switch| switch.span.start <= offset && offset <= switch.span.end)
            .min_by_key(|switch| switch.span.end.saturating_sub(switch.span.start))
        else {
            return Vec::new();
        };
        self.enums
            .get(&switch.enum_type_name)
            .into_iter()
            .flat_map(|variants| variants.iter())
            .filter(|variant| label_matches_prefix(variant, prefix))
            .map(|variant| CompletionCandidate {
                label: variant.clone(),
                kind: CompletionKind::EnumVariant,
                detail: Some(format!("{}::{}", switch.enum_type_name, variant)),
            })
            .collect()
    }

    fn looks_like_case_position(&self, text: &str, offset: usize) -> bool {
        let line = current_line_prefix(text, offset);
        line.trim_start().starts_with("case ")
    }

    fn looks_like_struct_field_position(&self, text: &str, offset: usize) -> bool {
        let Some(prefix_start) = identifier_prefix_start(text, offset) else {
            return false;
        };
        let Some((idx, ch)) = previous_non_ws(text, prefix_start) else {
            return false;
        };
        matches!(ch, '{' | ',') && !text[idx..prefix_start].contains(';')
    }
}

struct CompletionFactsBuilder<'a> {
    checked: &'a CheckedProgram,
    local_tys: HashMap<LocalId, Ty>,
    local_mutabilities: HashMap<LocalId, BindingMutability>,
    parameter_locals: HashSet<LocalId>,
    local_scopes: Vec<LocalScopeCandidate>,
    functions: HashMap<DefId, FunctionCompletionInfo>,
    selectors: Vec<SelectorCompletionInfo>,
    exprs: Vec<ExprTypeFact>,
    struct_literals: Vec<StructLiteralFact>,
    switches: Vec<SwitchFact>,
}

impl<'a> CompletionFactsBuilder<'a> {
    fn new(checked: &'a CheckedProgram) -> Self {
        Self {
            checked,
            local_tys: HashMap::new(),
            local_mutabilities: HashMap::new(),
            parameter_locals: HashSet::new(),
            local_scopes: Vec::new(),
            functions: function_completion_infos(checked),
            selectors: Vec::new(),
            exprs: Vec::new(),
            struct_literals: Vec::new(),
            switches: Vec::new(),
        }
    }

    fn collect(&mut self) {
        self.collect_checked();
        for module in &self.checked.hir_modules {
            self.collect_module_locals(module);
            self.collect_module_selectors(module);
        }
    }

    fn finish(self) -> CompletionFacts {
        CompletionFacts {
            resolved: self.checked.resolved.clone(),
            modules: self.checked.hir_modules.clone(),
            local_tys: self.local_tys,
            local_mutabilities: self.local_mutabilities,
            parameter_locals: self.parameter_locals,
            local_scopes: self.local_scopes,
            functions: self.functions,
            structs: self
                .checked
                .structs
                .iter()
                .map(|strukt| (strukt.name.clone(), strukt.fields.clone()))
                .collect(),
            enums: self
                .checked
                .enums
                .iter()
                .map(|enm| {
                    (
                        enm.name.clone(),
                        enm.variants
                            .iter()
                            .map(|variant| variant.name.clone())
                            .collect(),
                    )
                })
                .collect(),
            selectors: self.selectors,
            exprs: self.exprs,
            struct_literals: self.struct_literals,
            switches: self.switches,
        }
    }

    fn collect_checked(&mut self) {
        for function in &self.checked.functions {
            let scope_span = function
                .body
                .as_ref()
                .map(|body| body.span)
                .unwrap_or_else(|| self.checked.resolved.def(function.def_id).span);
            for (local_id, name, ty, mutability) in &function.params {
                if let Some(local_id) = local_id {
                    self.local_tys.insert(*local_id, ty.clone());
                    self.local_mutabilities.insert(*local_id, *mutability);
                    self.parameter_locals.insert(*local_id);
                    let declaration_span = self
                        .checked
                        .hir_locals
                        .iter()
                        .find(|local| local.id == *local_id)
                        .map(|local| local.span)
                        .unwrap_or(scope_span);
                    self.local_scopes.push(LocalScopeCandidate {
                        local_id: *local_id,
                        name: name.clone(),
                        declaration_span,
                        scope_span,
                    });
                }
            }
            if let Some(body) = &function.body {
                let mut collector = CompletionThirCollector { builder: self };
                collector.visit_block(body);
            }
        }
    }

    fn collect_module_locals(&mut self, module: &hir::Module) {
        for item in &module.items {
            match &item.kind {
                ItemKind::Function(function) => {
                    if let Some(body) = &function.body {
                        self.collect_block_locals(body);
                    }
                }
                ItemKind::Impl(decl) => {
                    self.collect_block_locals(&decl.body);
                }
                ItemKind::DerivableImpl(decl) => {
                    self.collect_block_locals(&decl.impl_decl.body);
                }
                _ => {}
            }
        }
    }

    fn collect_block_locals(&mut self, block: &hir::Block) {
        for stmt in &block.statements {
            self.collect_stmt_locals(stmt, block.span);
        }
    }

    fn collect_stmt_locals(&mut self, stmt: &hir::Stmt, scope_span: Span) {
        match &stmt.kind {
            StmtKind::Block(block) => self.collect_block_locals(block),
            StmtKind::VarDecl { name, local_id, .. } => {
                self.local_scopes.push(LocalScopeCandidate {
                    local_id: *local_id,
                    name: name.name.clone(),
                    declaration_span: name.span,
                    scope_span,
                });
            }
            StmtKind::For {
                init, step, body, ..
            } => {
                if let Some(init) = init {
                    self.collect_for_init_locals(init, scope_span);
                }
                if let Some(step) = step {
                    self.collect_for_init_locals(step, scope_span);
                }
                self.collect_block_locals(body);
            }
            StmtKind::If {
                then_block,
                else_branch,
                ..
            } => {
                self.collect_block_locals(then_block);
                if let Some(else_branch) = else_branch {
                    self.collect_stmt_locals(else_branch, scope_span);
                }
            }
            StmtKind::While { body, .. } => self.collect_block_locals(body),
            StmtKind::Switch { cases, default, .. } => {
                for (index, case) in cases.iter().enumerate() {
                    let fallback_end = cases
                        .get(index + 1)
                        .map(|next| pattern_span(&next.pattern).start)
                        .or_else(|| default.first().map(|stmt| stmt.span.start))
                        .unwrap_or(scope_span.end);
                    let case_scope_span = case_scope_span(case, fallback_end);
                    self.collect_pattern_binding_locals(&case.pattern, case_scope_span);
                    for stmt in &case.statements {
                        self.collect_stmt_locals(stmt, case_scope_span);
                    }
                }
                let default_scope_span = statements_scope_span(default, scope_span);
                for stmt in default {
                    self.collect_stmt_locals(stmt, default_scope_span);
                }
            }
            StmtKind::Assign { .. }
            | StmtKind::Defer(_)
            | StmtKind::Return(_)
            | StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Expr(_) => {}
        }
    }

    fn collect_for_init_locals(&mut self, init: &hir::ForInit, scope_span: Span) {
        if let hir::ForInit::VarDecl { name, local_id, .. } = init {
            self.local_scopes.push(LocalScopeCandidate {
                local_id: *local_id,
                name: name.name.clone(),
                declaration_span: name.span,
                scope_span,
            });
        }
    }

    fn collect_pattern_binding_locals(&mut self, pattern: &hir::Pattern, scope_span: Span) {
        match pattern {
            hir::Pattern::Variant(name, payload) => {
                if let hir::PatternNameKind::Binding {
                    local_id,
                    mutability,
                } = &name.kind
                {
                    self.local_mutabilities.insert(*local_id, *mutability);
                    self.local_scopes.push(LocalScopeCandidate {
                        local_id: *local_id,
                        name: name.display.clone(),
                        declaration_span: name.name_span,
                        scope_span,
                    });
                }
                for pattern in payload {
                    self.collect_pattern_binding_locals(pattern, scope_span);
                }
            }
            hir::Pattern::Wildcard(_) => {}
        }
    }

    fn collect_module_selectors(&mut self, module: &hir::Module) {
        for item in &module.items {
            match &item.kind {
                ItemKind::Function(function) => {
                    let Some(selector) = function.signature.receiver_selector.as_ref() else {
                        continue;
                    };
                    let Some(def_id) = item.def_ids.iter().copied().find(|def_id| {
                        let def = self.checked.resolved.def(*def_id);
                        matches!(def.kind, DefKind::Function | DefKind::ExternFunction)
                    }) else {
                        continue;
                    };
                    let Some(info) = self.functions.get(&def_id) else {
                        continue;
                    };
                    let Some(receiver_index) =
                        receiver_index_for_selector(&function.signature, selector)
                    else {
                        continue;
                    };
                    let Some(function_info) = self
                        .checked
                        .functions
                        .iter()
                        .find(|function| function.def_id == def_id)
                    else {
                        continue;
                    };
                    let Some((_, _, receiver_ty, _)) = function_info.params.get(receiver_index)
                    else {
                        continue;
                    };
                    self.selectors.push(SelectorCompletionInfo {
                        selector: selector.name.name.clone(),
                        module: module.id,
                        exported: item.export,
                        receiver_ty: receiver_ty.clone(),
                        detail: info.label.clone(),
                    });
                }
                ItemKind::Interface(decl) => {
                    let Some(selector) = decl.signature.receiver_selector.as_ref() else {
                        continue;
                    };
                    let Some(interface) = self
                        .checked
                        .interfaces
                        .iter()
                        .find(|interface| interface.name == decl.signature.name.name)
                    else {
                        continue;
                    };
                    let Some(receiver_index) =
                        receiver_index_for_selector(&decl.signature, selector)
                    else {
                        continue;
                    };
                    let Some(receiver_ty) = interface.params.get(receiver_index) else {
                        continue;
                    };
                    self.selectors.push(SelectorCompletionInfo {
                        selector: selector.name.name.clone(),
                        module: module.id,
                        exported: item.export,
                        receiver_ty: receiver_ty.clone(),
                        detail: format!("interface {}", interface.name),
                    });
                }
                _ => {}
            }
        }
    }
}

struct CompletionThirCollector<'a, 'b> {
    builder: &'a mut CompletionFactsBuilder<'b>,
}

impl ThirVisitor for CompletionThirCollector<'_, '_> {
    fn visit_stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::VarDecl { ty, local_id, .. } => {
                self.builder.local_tys.insert(*local_id, ty.clone());
            }
            TStmtKind::For { init, .. } => {
                if let Some(init) = init {
                    collect_for_init_type(self.builder, init);
                }
            }
            TStmtKind::Switch { enum_type_name, .. } => {
                self.builder.switches.push(SwitchFact {
                    span: stmt.span,
                    enum_type_name: enum_type_name.clone(),
                });
            }
            _ => {}
        }
        thir::walk_stmt(self, stmt);
    }

    fn visit_pattern(&mut self, pattern: &TPattern) {
        collect_pattern_type(self.builder, pattern);
        thir::walk_pattern(self, pattern);
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        self.builder.exprs.push(ExprTypeFact {
            span: expr.span,
            ty: expr.ty.clone(),
        });
        if let TExprKind::StructLiteral { type_name, .. } = &expr.kind {
            self.builder.struct_literals.push(StructLiteralFact {
                span: expr.span,
                type_name: type_name.clone(),
            });
        }
        if let TExprKind::Closure { params, .. } = &expr.kind {
            for (local_id, _, ty) in params {
                self.builder.local_tys.insert(*local_id, ty.clone());
                self.builder.parameter_locals.insert(*local_id);
            }
        }
        thir::walk_expr(self, expr);
    }
}

fn collect_for_init_type(builder: &mut CompletionFactsBuilder<'_>, init: &TForInit) {
    if let TForInit::VarDecl { ty, local_id, .. } = init {
        builder.local_tys.insert(*local_id, ty.clone());
    }
}

fn collect_pattern_type(builder: &mut CompletionFactsBuilder<'_>, pattern: &TPattern) {
    match pattern {
        TPattern::Binding { local_id, ty, .. } => {
            builder.local_tys.insert(*local_id, ty.clone());
        }
        TPattern::Variant { payload, .. } => {
            for pattern in payload {
                collect_pattern_type(builder, pattern);
            }
        }
        TPattern::Wildcard { .. } => {}
    }
}

fn case_scope_span(case: &hir::CaseClause, fallback_end: usize) -> Span {
    let pattern_span = pattern_span(&case.pattern);
    let end = case
        .statements
        .last()
        .map(|stmt| stmt.span.end)
        .unwrap_or(fallback_end)
        .max(pattern_span.end);
    Span::new(pattern_span.file, pattern_span.start, end)
}

fn statements_scope_span(statements: &[hir::Stmt], fallback: Span) -> Span {
    match (statements.first(), statements.last()) {
        (Some(first), Some(last)) => first.span.merge(last.span),
        _ => fallback,
    }
}

fn pattern_span(pattern: &hir::Pattern) -> Span {
    match pattern {
        hir::Pattern::Variant(name, _) => name.span,
        hir::Pattern::Wildcard(span) => *span,
    }
}

fn function_completion_infos(checked: &CheckedProgram) -> HashMap<DefId, FunctionCompletionInfo> {
    let mut infos = HashMap::new();
    for function in &checked.functions {
        let parameters = function
            .params
            .iter()
            .map(|(_, name, ty, mutability)| format_typed_binding(ty, name, *mutability))
            .collect::<Vec<_>>();
        infos.insert(
            function.def_id,
            FunctionCompletionInfo {
                label: format_function_signature(
                    function.is_async,
                    &function.ret,
                    &function.name,
                    parameters,
                ),
            },
        );
    }
    for function in &checked.generic_functions {
        let parameters = function
            .function
            .signature
            .params
            .iter()
            .zip(function.params.iter())
            .map(|(param, ty)| format_typed_binding(ty, &param.name.name, param.mutability))
            .collect::<Vec<_>>();
        infos.insert(
            function.def_id,
            FunctionCompletionInfo {
                label: format_function_signature(
                    function.is_async,
                    &function.ret,
                    &function.name,
                    parameters,
                ),
            },
        );
    }
    infos
}

fn receiver_index_for_selector(
    signature: &hir::FunctionSignature,
    selector: &hir::ReceiverSelector,
) -> Option<usize> {
    if signature.params.is_empty() {
        return None;
    }
    if let Some(receiver_param) = &selector.receiver_param {
        signature
            .params
            .iter()
            .position(|param| param.name.name == receiver_param.name)
    } else {
        Some(0)
    }
}

fn completion_context(text: &str, offset: usize) -> Option<CompletionContext> {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return None;
    }
    let prefix_start = identifier_prefix_start(text, offset)?;
    let prefix = text.get(prefix_start..offset)?.to_string();
    if let Some(colons_start) = text.get(..prefix_start)?.strip_suffix("::") {
        let qualifier = qualified_segments_before(colons_start)?;
        return Some(CompletionContext::Qualified { prefix, qualifier });
    }
    if text.get(..prefix_start)?.ends_with('.') {
        return Some(CompletionContext::Member {
            prefix,
            base_end: prefix_start.saturating_sub(1),
            arrow: false,
        });
    }
    if text.get(..prefix_start)?.ends_with("->") {
        return Some(CompletionContext::Member {
            prefix,
            base_end: prefix_start.saturating_sub(2),
            arrow: true,
        });
    }
    Some(CompletionContext::Bare { prefix })
}

fn identifier_prefix_start(text: &str, offset: usize) -> Option<usize> {
    let mut start = offset;
    for (idx, ch) in text.get(..offset)?.char_indices().rev() {
        if is_ident_continue(ch) {
            start = idx;
        } else {
            break;
        }
    }
    Some(start)
}

fn qualified_segments_before(text: &str) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    let mut end = text.len();
    loop {
        while end > 0 {
            let (idx, ch) = previous_char(text, end)?;
            if ch.is_whitespace() {
                end = idx;
            } else {
                break;
            }
        }
        let ident = identifier_before(text, end)?;
        let start = end - ident.len();
        segments.push(ident);
        if text.get(..start)?.ends_with("::") {
            end = start.saturating_sub(2);
            continue;
        }
        break;
    }
    segments.reverse();
    Some(segments)
}

fn identifier_before(text: &str, end: usize) -> Option<String> {
    if end > text.len() || !text.is_char_boundary(end) {
        return None;
    }
    let mut start = end;
    for (idx, ch) in text.get(..end)?.char_indices().rev() {
        if is_ident_continue(ch) {
            start = idx;
        } else {
            break;
        }
    }
    (start < end).then(|| text[start..end].to_string())
}

fn previous_char(text: &str, end: usize) -> Option<(usize, char)> {
    text.get(..end)?.char_indices().next_back()
}

fn previous_non_ws(text: &str, end: usize) -> Option<(usize, char)> {
    let mut cursor = end;
    while cursor > 0 {
        let (idx, ch) = previous_char(text, cursor)?;
        if !ch.is_whitespace() {
            return Some((idx, ch));
        }
        cursor = idx;
    }
    None
}

fn current_line_prefix(text: &str, offset: usize) -> &str {
    let start = text[..offset].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    &text[start..offset]
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn label_matches_prefix(label: &str, prefix: &str) -> bool {
    prefix.is_empty() || label.starts_with(prefix)
}

fn completion_kind_for_def(kind: &DefKind) -> CompletionKind {
    match kind {
        DefKind::TypeAlias | DefKind::OpaqueStruct => CompletionKind::Type,
        DefKind::Struct => CompletionKind::Struct,
        DefKind::Enum => CompletionKind::Enum,
        DefKind::EnumVariant => CompletionKind::EnumVariant,
        DefKind::Interface | DefKind::InterfaceAlias => CompletionKind::Interface,
        DefKind::Function | DefKind::ExternFunction => CompletionKind::Function,
    }
}

fn keyword_candidates(prefix: &str) -> Vec<CompletionCandidate> {
    [
        "return", "if", "else", "while", "for", "switch", "case", "default", "break", "continue",
        "defer", "unsafe", "async", "await", "select", "biased",
    ]
    .into_iter()
    .filter(|keyword| label_matches_prefix(keyword, prefix))
    .map(|keyword| CompletionCandidate {
        label: keyword.to_string(),
        kind: CompletionKind::Keyword,
        detail: None,
    })
    .collect()
}

fn receiver_matches(param_ty: &Ty, receiver_ty: &Ty) -> bool {
    ty_pattern_matches(param_ty, receiver_ty)
        || matches!(param_ty, Ty::Pointer { inner, .. } if ty_pattern_matches(inner, receiver_ty))
}

fn ty_pattern_matches(pattern: &Ty, actual: &Ty) -> bool {
    match (pattern, actual) {
        (Ty::Generic(_), _) | (_, Ty::Unknown) => true,
        (
            Ty::Named {
                name: left,
                args: left_args,
            },
            Ty::Named {
                name: right,
                args: right_args,
            },
        ) => {
            left == right
                && left_args.len() == right_args.len()
                && left_args
                    .iter()
                    .zip(right_args)
                    .all(|(left, right)| ty_pattern_matches(left, right))
        }
        (Ty::Pointer { inner: left, .. }, Ty::Pointer { inner: right, .. }) => {
            ty_pattern_matches(left, right)
        }
        (Ty::Array { elem: left, .. }, Ty::Array { elem: right, .. }) => {
            ty_pattern_matches(left, right)
        }
        (Ty::Slice { elem: left, .. }, Ty::Slice { elem: right, .. }) => {
            ty_pattern_matches(left, right)
        }
        (Ty::OpaqueState { base, .. }, actual) => ty_pattern_matches(base, actual),
        (pattern, Ty::OpaqueState { base, .. }) => ty_pattern_matches(pattern, base),
        _ => pattern == actual,
    }
}

fn kind_rank(kind: &CompletionKind) -> usize {
    match kind {
        CompletionKind::Variable | CompletionKind::Parameter => 0,
        CompletionKind::Field => 1,
        CompletionKind::Function => 2,
        CompletionKind::EnumVariant => 3,
        CompletionKind::Struct | CompletionKind::Enum | CompletionKind::Type => 4,
        CompletionKind::Interface => 5,
        CompletionKind::Module => 6,
        CompletionKind::Keyword => 7,
    }
}

fn dedup_candidates(candidates: &mut Vec<CompletionCandidate>) {
    let mut seen = HashSet::new();
    candidates.retain(|candidate| seen.insert((candidate.label.clone(), candidate.kind.clone())));
}

fn dedup_def_ids(defs: &mut Vec<DefId>) {
    let mut seen = HashSet::new();
    defs.retain(|def_id| seen.insert(*def_id));
}

fn non_variant_def_kinds() -> [DefKind; 8] {
    [
        DefKind::TypeAlias,
        DefKind::Struct,
        DefKind::Enum,
        DefKind::Interface,
        DefKind::InterfaceAlias,
        DefKind::Function,
        DefKind::ExternFunction,
        DefKind::OpaqueStruct,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{driver::CompileOptions, driver::analyze_frontend_lossy};
    use std::path::PathBuf;

    fn complete_at(source: &str, marker: &str) -> Vec<CompletionCandidate> {
        let path = PathBuf::from("/tmp/ciel_completion.ciel");
        let offset = source.find(marker).expect("marker");
        let source = source.replace(marker, "");
        let options = CompileOptions::new(&path).with_source_override(&path, source.clone());
        let analysis = analyze_frontend_lossy(options).expect("frontend analysis should run");
        let file_id = analysis
            .source_map
            .files()
            .iter()
            .find(|file| file.path == path)
            .expect("test file")
            .id;
        let facts = CompletionFacts::from_checked(&analysis.checked);
        facts.complete(&analysis.source_map, file_id, offset)
    }

    #[test]
    fn completes_visible_locals_and_functions() {
        let items = complete_at(
            r#"
                i64 add(i64 lhs, i64 rhs) { return lhs + rhs; }
                i64 main() {
                    i64 actor = 1;
                    @
                    return actor;
                }
            "#,
            "@",
        );
        assert!(items.iter().any(|item| item.label == "actor"));
        assert!(items.iter().any(|item| item.label == "add"));
    }

    #[test]
    fn completes_case_pattern_bindings() {
        let items = complete_at(
            r#"
                enum Outcome {
                    Ok,
                    Err(i64),
                }

                i64 main() {
                    Outcome outcome = Err(1);
                    switch (outcome) {
                    case Err(error):
                        return er@;
                    case Ok:
                        return 0;
                    }
                }
            "#,
            "@",
        );
        assert!(items.iter().any(|item| item.label == "error"));
    }

    #[test]
    fn completes_fields_and_selectors() {
        let items = complete_at(
            r#"
                struct Packet { i64 value; }
                i64 packet_load(*const Packet packet) = .load { return packet->value; }
                i64 main() {
                    Packet packet = { value: 1 };
                    return packet.@
                }
            "#,
            "@",
        );
        assert!(items.iter().any(|item| item.label == "value"));
        assert!(items.iter().any(|item| item.label == "load"));
    }

    #[test]
    fn completes_qualified_enum_variants() {
        let items = complete_at(
            r#"
                enum Status { Success, Failure }
                i64 main() {
                    Status status = Status::@
                    return 0;
                }
            "#,
            "@",
        );
        assert!(items.iter().any(|item| item.label == "Success"));
        assert!(items.iter().any(|item| item.label == "Failure"));
    }
}
