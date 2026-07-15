use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use crate::{
    ast::*,
    diagnostic::{DiagResult, Diagnostic, DiagnosticPhase, WithDiagnostics},
    span::Span,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModuleId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DefId(pub usize);

#[derive(Clone, Debug)]
pub struct ParsedModule {
    pub id: ModuleId,
    pub path: PathBuf,
    pub std_export: Option<String>,
    pub import_paths: Vec<PathBuf>,
    pub ast: AstFile,
}

#[derive(Clone, Debug)]
pub struct ResolvedProgram {
    pub modules: Vec<ResolvedModule>,
    pub defs: Vec<Def>,
    pub impls: Vec<ImplRecord>,
}

#[derive(Clone, Debug)]
pub struct ResolvedModule {
    pub id: ModuleId,
    pub path: PathBuf,
    pub std_export: Option<String>,
    pub ast: AstFile,
    pub defs: Vec<DefId>,
    pub imports: Vec<ResolvedImport>,
}

#[derive(Clone, Debug)]
pub struct ResolvedImport {
    pub path: String,
    pub resolved_path: PathBuf,
    pub alias: Option<String>,
    pub exported: bool,
    pub target: Option<ModuleId>,
}

#[derive(Clone, Debug)]
pub struct Def {
    pub id: DefId,
    pub module: ModuleId,
    pub name: String,
    pub kind: DefKind,
    pub parent: Option<DefId>,
    pub exported: bool,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefKind {
    TypeAlias,
    Struct,
    Enum,
    EnumVariant,
    Interface,
    InterfaceAlias,
    Function,
    ExternFunction,
    OpaqueStruct,
}

#[derive(Clone, Debug)]
pub struct ImplRecord {
    pub module: ModuleId,
    pub interface_name: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum LookupError {
    Ambiguous {
        name: String,
        candidates: Vec<DefId>,
    },
    UnknownAlias {
        alias: String,
    },
    NotExported {
        name: String,
    },
    UnresolvedImport {
        path: String,
    },
    TooManySegments {
        len: usize,
    },
}

impl ResolvedProgram {
    pub fn def(&self, id: DefId) -> &Def {
        &self.defs[id.0]
    }

    pub fn item_for_def(&self, id: DefId) -> Option<&Item> {
        let def = self.def(id);
        let module = &self.modules[def.module.0];
        module.ast.items.iter().find(|item| match &item.kind {
            ItemKind::TypeAlias(decl) => decl.name.name == def.name,
            ItemKind::Struct(decl) => decl.name.name == def.name,
            ItemKind::Enum(decl) => decl.name.name == def.name,
            ItemKind::Interface(decl) => decl.signature.name.name == def.name,
            ItemKind::InterfaceAlias(decl) => decl.name.name == def.name,
            ItemKind::Function(decl) => decl.signature.name.name == def.name,
            ItemKind::ExternBlock(block) => block.items.iter().any(|item| match item {
                ExternItem::OpaqueStruct(name) => name.name == def.name,
                ExternItem::Function { signature, .. } => signature.name.name == def.name,
                ExternItem::TypeAlias(alias) => alias.name.name == def.name,
            }),
            ItemKind::Error => false,
            _ => false,
        })
    }

    pub fn local_def(&self, module: ModuleId, name: &str, kinds: &[DefKind]) -> Option<DefId> {
        self.modules[module.0].defs.iter().copied().find(|id| {
            let def = self.def(*id);
            def.name == name && kind_matches(&def.kind, kinds)
        })
    }

    pub fn local_enum_variant_def(
        &self,
        module: ModuleId,
        enum_def: DefId,
        name: &str,
    ) -> Option<DefId> {
        self.modules[module.0].defs.iter().copied().find(|id| {
            let def = self.def(*id);
            def.kind == DefKind::EnumVariant && def.parent == Some(enum_def) && def.name == name
        })
    }

    pub fn enum_variant_def(&self, enum_def: DefId, name: &str) -> Option<DefId> {
        let enum_module = self.def(enum_def).module;
        self.local_enum_variant_def(enum_module, enum_def, name)
    }

    pub fn lookup_bare(
        &self,
        module: ModuleId,
        name: &str,
        kinds: &[DefKind],
    ) -> Result<Option<DefId>, LookupError> {
        if let Some(local) = self.local_def(module, name, kinds) {
            return Ok(Some(local));
        }

        let mut candidates = Vec::new();
        let mut visited = HashSet::new();
        for import in &self.modules[module.0].imports {
            if import.alias.is_some() {
                continue;
            }
            let Some(target) = import.target else {
                return Err(LookupError::UnresolvedImport {
                    path: import.path.clone(),
                });
            };
            self.exported_bare_defs(target, name, kinds, &mut visited, &mut candidates);
        }
        dedup_defs(&mut candidates);
        match candidates.len() {
            0 => Ok(None),
            1 => Ok(Some(candidates[0])),
            _ => Err(LookupError::Ambiguous {
                name: name.to_string(),
                candidates,
            }),
        }
    }

    pub fn lookup_imported_bare(
        &self,
        module: ModuleId,
        name: &str,
        kinds: &[DefKind],
    ) -> Result<Option<DefId>, LookupError> {
        let mut candidates = Vec::new();
        let mut visited = HashSet::new();
        for import in &self.modules[module.0].imports {
            if import.alias.is_some() {
                continue;
            }
            let Some(target) = import.target else {
                return Err(LookupError::UnresolvedImport {
                    path: import.path.clone(),
                });
            };
            self.exported_bare_defs(target, name, kinds, &mut visited, &mut candidates);
        }
        dedup_defs(&mut candidates);
        match candidates.len() {
            0 => Ok(None),
            1 => Ok(Some(candidates[0])),
            _ => Err(LookupError::Ambiguous {
                name: name.to_string(),
                candidates,
            }),
        }
    }

    pub fn visible_imported_bare_defs(&self, module: ModuleId, kinds: &[DefKind]) -> Vec<DefId> {
        let mut candidates = Vec::new();
        let mut visited = HashSet::new();
        for import in &self.modules[module.0].imports {
            if import.alias.is_some() {
                continue;
            }
            if let Some(target) = import.target {
                self.exported_defs(target, kinds, &mut visited, &mut candidates);
            }
        }
        dedup_defs(&mut candidates);
        candidates
    }

    pub fn lookup_bare_variants(
        &self,
        module: ModuleId,
        name: &str,
    ) -> Result<Vec<DefId>, LookupError> {
        let local = self
            .modules
            .get(module.0)
            .into_iter()
            .flat_map(|module| module.defs.iter().copied())
            .filter(|id| {
                let def = self.def(*id);
                def.kind == DefKind::EnumVariant && def.name == name
            })
            .collect::<Vec<_>>();
        if !local.is_empty() {
            return Ok(local);
        }
        self.lookup_imported_bare_variants(module, name)
    }

    pub fn lookup_imported_bare_variants(
        &self,
        module: ModuleId,
        name: &str,
    ) -> Result<Vec<DefId>, LookupError> {
        let mut candidates = Vec::new();
        let mut visited = HashSet::new();
        for import in &self.modules[module.0].imports {
            if import.alias.is_some() {
                continue;
            }
            let Some(target) = import.target else {
                return Err(LookupError::UnresolvedImport {
                    path: import.path.clone(),
                });
            };
            self.exported_bare_variant_defs(target, name, &mut visited, &mut candidates);
        }
        dedup_defs(&mut candidates);
        Ok(candidates)
    }

    pub fn visible_imported_bare_variants(&self, module: ModuleId) -> Vec<DefId> {
        let mut candidates = Vec::new();
        let mut visited = HashSet::new();
        for import in &self.modules[module.0].imports {
            if import.alias.is_some() {
                continue;
            }
            if let Some(target) = import.target {
                self.exported_variant_defs(target, &mut visited, &mut candidates);
            }
        }
        dedup_defs(&mut candidates);
        candidates
    }

    pub fn lexical_def_before(
        &self,
        module: ModuleId,
        name: &str,
        before_index: usize,
    ) -> Option<DefId> {
        let module = &self.modules[module.0];
        module
            .ast
            .items
            .iter()
            .take(before_index)
            .flat_map(item_declared_names)
            .find_map(|def_name| {
                (def_name == name).then(|| {
                    let kinds = all_def_kinds();
                    self.local_def(module.id, name, &kinds)
                })?
            })
    }

    pub fn visible_import_aliases(&self, module: ModuleId) -> Vec<String> {
        let mut aliases = Vec::new();
        for import in &self.modules[module.0].imports {
            if let Some(alias) = &import.alias {
                aliases.push(alias.clone());
            }
        }
        let mut visited = HashSet::new();
        for import in &self.modules[module.0].imports {
            if import.alias.is_some() {
                continue;
            }
            if let Some(target) = import.target {
                self.exported_aliases(target, &mut visited, &mut aliases);
            }
        }
        aliases.sort();
        aliases.dedup();
        aliases
    }

    pub fn lookup_qualified(
        &self,
        module: ModuleId,
        alias: &str,
        name: &str,
        kinds: &[DefKind],
    ) -> Result<Option<DefId>, LookupError> {
        let mut targets = Vec::new();
        for import in &self.modules[module.0].imports {
            if import.alias.as_deref() != Some(alias) {
                continue;
            }
            let Some(target) = import.target else {
                return Err(LookupError::UnresolvedImport {
                    path: import.path.clone(),
                });
            };
            targets.push(target);
        }

        let mut visited_aliases = HashSet::new();
        for import in &self.modules[module.0].imports {
            if import.alias.is_some() {
                continue;
            }
            let Some(target) = import.target else {
                return Err(LookupError::UnresolvedImport {
                    path: import.path.clone(),
                });
            };
            self.exported_alias_targets(target, alias, &mut visited_aliases, &mut targets);
        }

        dedup_modules(&mut targets);
        if targets.is_empty() {
            return Err(LookupError::UnknownAlias {
                alias: alias.to_string(),
            });
        }

        let mut candidates = Vec::new();
        for target in targets {
            let mut visited = HashSet::new();
            self.exported_bare_defs(target, name, kinds, &mut visited, &mut candidates);
        }
        dedup_defs(&mut candidates);
        if candidates.is_empty() {
            for import in &self.modules[module.0].imports {
                if import.alias.as_deref() == Some(alias)
                    && let Some(target) = import.target
                    && self.local_def(target, name, kinds).is_some()
                {
                    return Err(LookupError::NotExported {
                        name: format!("{alias}::{name}"),
                    });
                }
            }
        }
        match candidates.len() {
            0 => Ok(None),
            1 => Ok(Some(candidates[0])),
            _ => Err(LookupError::Ambiguous {
                name: format!("{alias}::{name}"),
                candidates,
            }),
        }
    }

    pub fn lookup_qualified_variants(
        &self,
        module: ModuleId,
        alias: &str,
        name: &str,
    ) -> Result<Vec<DefId>, LookupError> {
        let mut targets = Vec::new();
        for import in &self.modules[module.0].imports {
            if import.alias.as_deref() != Some(alias) {
                continue;
            }
            let Some(target) = import.target else {
                return Err(LookupError::UnresolvedImport {
                    path: import.path.clone(),
                });
            };
            targets.push(target);
        }

        let mut visited_aliases = HashSet::new();
        for import in &self.modules[module.0].imports {
            if import.alias.is_some() {
                continue;
            }
            let Some(target) = import.target else {
                return Err(LookupError::UnresolvedImport {
                    path: import.path.clone(),
                });
            };
            self.exported_alias_targets(target, alias, &mut visited_aliases, &mut targets);
        }

        dedup_modules(&mut targets);
        if targets.is_empty() {
            return Err(LookupError::UnknownAlias {
                alias: alias.to_string(),
            });
        }

        let mut candidates = Vec::new();
        for target in targets {
            let mut visited = HashSet::new();
            self.exported_bare_variant_defs(target, name, &mut visited, &mut candidates);
        }
        dedup_defs(&mut candidates);
        if candidates.is_empty() {
            for import in &self.modules[module.0].imports {
                if import.alias.as_deref() == Some(alias)
                    && let Some(target) = import.target
                    && self
                        .modules
                        .get(target.0)
                        .into_iter()
                        .flat_map(|module| module.defs.iter().copied())
                        .any(|id| {
                            let def = self.def(id);
                            def.kind == DefKind::EnumVariant && def.name == name
                        })
                {
                    return Err(LookupError::NotExported {
                        name: format!("{alias}::{name}"),
                    });
                }
            }
        }
        Ok(candidates)
    }

    pub fn visible_qualified_defs(
        &self,
        module: ModuleId,
        alias: &str,
        kinds: &[DefKind],
    ) -> Result<Vec<DefId>, LookupError> {
        let targets = self.visible_alias_targets(module, alias)?;
        let mut candidates = Vec::new();
        for target in targets {
            let mut visited = HashSet::new();
            self.exported_defs(target, kinds, &mut visited, &mut candidates);
        }
        dedup_defs(&mut candidates);
        Ok(candidates)
    }

    pub fn visible_qualified_variants(
        &self,
        module: ModuleId,
        alias: &str,
    ) -> Result<Vec<DefId>, LookupError> {
        let targets = self.visible_alias_targets(module, alias)?;
        let mut candidates = Vec::new();
        for target in targets {
            let mut visited = HashSet::new();
            self.exported_variant_defs(target, &mut visited, &mut candidates);
        }
        dedup_defs(&mut candidates);
        Ok(candidates)
    }

    pub fn enum_variant_defs(&self, enum_def: DefId) -> Vec<DefId> {
        let enum_module = self.def(enum_def).module;
        self.modules[enum_module.0]
            .defs
            .iter()
            .copied()
            .filter(|id| {
                let def = self.def(*id);
                def.kind == DefKind::EnumVariant && def.parent == Some(enum_def)
            })
            .collect()
    }

    pub fn lookup_path(
        &self,
        module: ModuleId,
        path: &[Ident],
        kinds: &[DefKind],
    ) -> Result<Option<DefId>, LookupError> {
        match path {
            [name] => self.lookup_bare(module, &name.name, kinds),
            [alias, name] => self.lookup_qualified(module, &alias.name, &name.name, kinds),
            _ => Err(LookupError::TooManySegments { len: path.len() }),
        }
    }

    pub fn struct_fields(&self, name: &str) -> Option<&[FieldDecl]> {
        self.modules.iter().find_map(|module| {
            module.ast.items.iter().find_map(|item| {
                if let ItemKind::Struct(decl) = &item.kind
                    && decl.name.name == name
                {
                    Some(decl.fields.as_slice())
                } else {
                    None
                }
            })
        })
    }

    fn visible_alias_targets(
        &self,
        module: ModuleId,
        alias: &str,
    ) -> Result<Vec<ModuleId>, LookupError> {
        let mut targets = Vec::new();
        for import in &self.modules[module.0].imports {
            if import.alias.as_deref() != Some(alias) {
                continue;
            }
            let Some(target) = import.target else {
                return Err(LookupError::UnresolvedImport {
                    path: import.path.clone(),
                });
            };
            targets.push(target);
        }

        let mut visited_aliases = HashSet::new();
        for import in &self.modules[module.0].imports {
            if import.alias.is_some() {
                continue;
            }
            let Some(target) = import.target else {
                return Err(LookupError::UnresolvedImport {
                    path: import.path.clone(),
                });
            };
            self.exported_alias_targets(target, alias, &mut visited_aliases, &mut targets);
        }

        dedup_modules(&mut targets);
        if targets.is_empty() {
            Err(LookupError::UnknownAlias {
                alias: alias.to_string(),
            })
        } else {
            Ok(targets)
        }
    }

    fn exported_defs(
        &self,
        module: ModuleId,
        kinds: &[DefKind],
        visited: &mut HashSet<ModuleId>,
        out: &mut Vec<DefId>,
    ) {
        if !visited.insert(module) {
            return;
        }
        for id in &self.modules[module.0].defs {
            let def = self.def(*id);
            if def.exported && kind_matches(&def.kind, kinds) {
                out.push(*id);
            }
        }
        for import in &self.modules[module.0].imports {
            if !import.exported || import.alias.is_some() {
                continue;
            }
            if let Some(target) = import.target {
                self.exported_defs(target, kinds, visited, out);
            }
        }
    }

    fn exported_variant_defs(
        &self,
        module: ModuleId,
        visited: &mut HashSet<ModuleId>,
        out: &mut Vec<DefId>,
    ) {
        if !visited.insert(module) {
            return;
        }
        for id in &self.modules[module.0].defs {
            let def = self.def(*id);
            if def.exported && def.kind == DefKind::EnumVariant {
                out.push(*id);
            }
        }
        for import in &self.modules[module.0].imports {
            if !import.exported || import.alias.is_some() {
                continue;
            }
            if let Some(target) = import.target {
                self.exported_variant_defs(target, visited, out);
            }
        }
    }

    fn exported_aliases(
        &self,
        module: ModuleId,
        visited: &mut HashSet<ModuleId>,
        out: &mut Vec<String>,
    ) {
        if !visited.insert(module) {
            return;
        }
        for import in &self.modules[module.0].imports {
            if !import.exported {
                continue;
            }
            if let Some(alias) = &import.alias {
                out.push(alias.clone());
            } else if let Some(target) = import.target {
                self.exported_aliases(target, visited, out);
            }
        }
    }

    fn exported_bare_defs(
        &self,
        module: ModuleId,
        name: &str,
        kinds: &[DefKind],
        visited: &mut HashSet<ModuleId>,
        out: &mut Vec<DefId>,
    ) {
        if !visited.insert(module) {
            return;
        }
        let start_len = out.len();
        for id in &self.modules[module.0].defs {
            let def = self.def(*id);
            if def.exported && def.name == name && kind_matches(&def.kind, kinds) {
                out.push(*id);
            }
        }
        if out.len() != start_len {
            return;
        }
        for import in &self.modules[module.0].imports {
            if !import.exported || import.alias.is_some() {
                continue;
            }
            if let Some(target) = import.target {
                self.exported_bare_defs(target, name, kinds, visited, out);
            }
        }
    }

    fn exported_bare_variant_defs(
        &self,
        module: ModuleId,
        name: &str,
        visited: &mut HashSet<ModuleId>,
        out: &mut Vec<DefId>,
    ) {
        if !visited.insert(module) {
            return;
        }
        let start_len = out.len();
        for id in &self.modules[module.0].defs {
            let def = self.def(*id);
            if def.exported && def.kind == DefKind::EnumVariant && def.name == name {
                out.push(*id);
            }
        }
        if out.len() != start_len {
            return;
        }
        for import in &self.modules[module.0].imports {
            if !import.exported || import.alias.is_some() {
                continue;
            }
            if let Some(target) = import.target {
                self.exported_bare_variant_defs(target, name, visited, out);
            }
        }
    }

    fn exported_alias_targets(
        &self,
        module: ModuleId,
        alias: &str,
        visited: &mut HashSet<ModuleId>,
        out: &mut Vec<ModuleId>,
    ) {
        if !visited.insert(module) {
            return;
        }
        for import in &self.modules[module.0].imports {
            if !import.exported {
                continue;
            }
            if import.alias.as_deref() == Some(alias) {
                if let Some(target) = import.target {
                    out.push(target);
                }
            } else if import.alias.is_none()
                && let Some(target) = import.target
            {
                self.exported_alias_targets(target, alias, visited, out);
            }
        }
    }
}

pub fn resolve_modules(modules: Vec<ParsedModule>) -> DiagResult<ResolvedProgram> {
    let result = resolve_modules_lossy(modules);
    if result.diagnostics.is_empty() {
        Ok(result.value)
    } else {
        Err(result.diagnostics)
    }
}

pub fn resolve_modules_lossy(modules: Vec<ParsedModule>) -> WithDiagnostics<ResolvedProgram> {
    let mut diagnostics = Vec::new();
    let mut defs = Vec::new();
    let mut resolved_modules = Vec::new();
    let mut impls = Vec::new();

    for module in modules {
        let mut module_defs = Vec::new();
        let mut imports = Vec::new();
        let mut local_names = HashSet::<String>::new();

        let mut import_paths = module.import_paths.into_iter();
        for item in &module.ast.items {
            match &item.kind {
                ItemKind::Import(import) => {
                    let resolved_path = import_paths.next().unwrap_or_default();
                    imports.push(ResolvedImport {
                        path: import.path.raw.clone(),
                        resolved_path,
                        alias: import.alias.as_ref().map(|alias| alias.name.clone()),
                        exported: item.export,
                        target: None,
                    });
                }
                ItemKind::TypeAlias(decl) => {
                    add_def(
                        &mut diagnostics,
                        &mut defs,
                        &mut module_defs,
                        &mut local_names,
                        module.id,
                        decl.name.name.clone(),
                        DefKind::TypeAlias,
                        item.export,
                        decl.name.span,
                    );
                }
                ItemKind::Struct(decl) => {
                    add_def(
                        &mut diagnostics,
                        &mut defs,
                        &mut module_defs,
                        &mut local_names,
                        module.id,
                        decl.name.name.clone(),
                        DefKind::Struct,
                        item.export,
                        decl.name.span,
                    );
                }
                ItemKind::Enum(decl) => {
                    let Some(enum_def_id) = add_def(
                        &mut diagnostics,
                        &mut defs,
                        &mut module_defs,
                        &mut local_names,
                        module.id,
                        decl.name.name.clone(),
                        DefKind::Enum,
                        item.export,
                        decl.name.span,
                    ) else {
                        continue;
                    };
                    let mut variant_names = HashSet::<String>::new();
                    for variant in &decl.variants {
                        if !variant_names.insert(variant.name.name.clone()) {
                            // TODO(diagnostics): keep the first variant span so this note can point to it.
                            diagnostics.push(
                                Diagnostic::new(
                                    variant.name.span,
                                    format!(
                                        "duplicate variant `{}` in enum `{}`",
                                        variant.name.name, decl.name.name
                                    ),
                                )
                                .note(format!(
                                    "enum `{}` already has a variant named `{}`",
                                    decl.name.name, variant.name.name
                                )),
                            );
                            continue;
                        }
                        add_variant_def(
                            &mut diagnostics,
                            &mut defs,
                            &mut module_defs,
                            module.id,
                            enum_def_id,
                            variant.name.name.clone(),
                            item.export,
                            variant.name.span,
                        );
                    }
                }
                ItemKind::Interface(decl) => {
                    add_def(
                        &mut diagnostics,
                        &mut defs,
                        &mut module_defs,
                        &mut local_names,
                        module.id,
                        decl.signature.name.name.clone(),
                        DefKind::Interface,
                        item.export,
                        decl.signature.name.span,
                    );
                }
                ItemKind::InterfaceAlias(decl) => {
                    add_def(
                        &mut diagnostics,
                        &mut defs,
                        &mut module_defs,
                        &mut local_names,
                        module.id,
                        decl.name.name.clone(),
                        DefKind::InterfaceAlias,
                        item.export,
                        decl.name.span,
                    );
                }
                ItemKind::Impl(decl) => {
                    if let Some(name) = decl.name.last() {
                        impls.push(ImplRecord {
                            module: module.id,
                            interface_name: name.name.clone(),
                            span: item.span,
                        });
                    }
                }
                ItemKind::DerivableImpl(decl) => {
                    if let Some(name) = decl.impl_decl.name.last() {
                        impls.push(ImplRecord {
                            module: module.id,
                            interface_name: name.name.clone(),
                            span: item.span,
                        });
                    }
                }
                ItemKind::Derive(_) => {}
                ItemKind::Function(decl) => {
                    let kind = if decl.abi.as_deref() == Some("C") && decl.body.is_none() {
                        DefKind::ExternFunction
                    } else {
                        DefKind::Function
                    };
                    add_def(
                        &mut diagnostics,
                        &mut defs,
                        &mut module_defs,
                        &mut local_names,
                        module.id,
                        decl.signature.name.name.clone(),
                        kind,
                        item.export,
                        decl.signature.name.span,
                    );
                }
                ItemKind::ExternBlock(block) => {
                    for extern_item in &block.items {
                        match extern_item {
                            ExternItem::OpaqueStruct(name) => add_def(
                                &mut diagnostics,
                                &mut defs,
                                &mut module_defs,
                                &mut local_names,
                                module.id,
                                name.name.clone(),
                                DefKind::OpaqueStruct,
                                item.export,
                                name.span,
                            ),
                            ExternItem::Function { signature, .. } => add_def(
                                &mut diagnostics,
                                &mut defs,
                                &mut module_defs,
                                &mut local_names,
                                module.id,
                                signature.name.name.clone(),
                                DefKind::ExternFunction,
                                item.export,
                                signature.name.span,
                            ),
                            ExternItem::TypeAlias(alias) => add_def(
                                &mut diagnostics,
                                &mut defs,
                                &mut module_defs,
                                &mut local_names,
                                module.id,
                                alias.name.name.clone(),
                                DefKind::TypeAlias,
                                item.export,
                                alias.name.span,
                            ),
                        };
                    }
                }
                ItemKind::CInclude(_) => {}
                ItemKind::Error => {}
            }
        }

        resolved_modules.push(ResolvedModule {
            id: module.id,
            path: module.path,
            std_export: module.std_export,
            ast: module.ast,
            defs: module_defs,
            imports,
        });
    }

    resolve_import_targets(&mut diagnostics, &mut resolved_modules);

    for diagnostic in &mut diagnostics {
        if diagnostic.phase.is_none() {
            diagnostic.phase = Some(DiagnosticPhase::Resolve);
        }
    }

    WithDiagnostics {
        value: ResolvedProgram {
            modules: resolved_modules,
            defs,
            impls,
        },
        diagnostics,
    }
}

fn resolve_import_targets(diagnostics: &mut Vec<Diagnostic>, modules: &mut [ResolvedModule]) {
    let path_to_id = modules
        .iter()
        .map(|module| (module.path.clone(), module.id))
        .collect::<HashMap<_, _>>();
    for idx in 0..modules.len() {
        for import in &mut modules[idx].imports {
            import.target = path_to_id.get(&import.resolved_path).copied();
            if import.target.is_none() {
                diagnostics.push(
                    Diagnostic::new(None, format!("unresolved import `{}`", import.path)).note(
                        format!(
                            "resolved filesystem path `{}` was not loaded",
                            import.resolved_path.display()
                        ),
                    ),
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn add_def(
    diagnostics: &mut Vec<Diagnostic>,
    defs: &mut Vec<Def>,
    module_defs: &mut Vec<DefId>,
    local_names: &mut HashSet<String>,
    module: ModuleId,
    name: String,
    kind: DefKind,
    exported: bool,
    span: Span,
) -> Option<DefId> {
    if !local_names.insert(name.clone()) {
        // TODO(diagnostics): keep the first declaration span so this note can point to it.
        diagnostics.push(
            Diagnostic::new(span, format!("duplicate declaration `{name}` in module")).note(
                format!("this module already contains a declaration named `{name}`"),
            ),
        );
        return None;
    }
    Some(push_def(
        defs,
        module_defs,
        module,
        name,
        kind,
        None,
        exported,
        span,
    ))
}

#[allow(clippy::too_many_arguments)]
fn add_variant_def(
    _diagnostics: &mut Vec<Diagnostic>,
    defs: &mut Vec<Def>,
    module_defs: &mut Vec<DefId>,
    module: ModuleId,
    enum_def_id: DefId,
    name: String,
    exported: bool,
    span: Span,
) -> DefId {
    push_def(
        defs,
        module_defs,
        module,
        name,
        DefKind::EnumVariant,
        Some(enum_def_id),
        exported,
        span,
    )
}

#[allow(clippy::too_many_arguments)]
fn push_def(
    defs: &mut Vec<Def>,
    module_defs: &mut Vec<DefId>,
    module: ModuleId,
    name: String,
    kind: DefKind,
    parent: Option<DefId>,
    exported: bool,
    span: Span,
) -> DefId {
    let id = DefId(defs.len());
    defs.push(Def {
        id,
        module,
        name,
        kind,
        parent,
        exported,
        span,
    });
    module_defs.push(id);
    id
}

fn kind_matches(kind: &DefKind, kinds: &[DefKind]) -> bool {
    kinds.iter().any(|candidate| candidate == kind)
}

fn all_def_kinds() -> [DefKind; 9] {
    [
        DefKind::TypeAlias,
        DefKind::Struct,
        DefKind::Enum,
        DefKind::EnumVariant,
        DefKind::Interface,
        DefKind::InterfaceAlias,
        DefKind::Function,
        DefKind::ExternFunction,
        DefKind::OpaqueStruct,
    ]
}

fn item_declared_names(item: &Item) -> Vec<&str> {
    match &item.kind {
        ItemKind::TypeAlias(decl) => vec![decl.name.name.as_str()],
        ItemKind::Struct(decl) => vec![decl.name.name.as_str()],
        ItemKind::Enum(decl) => vec![decl.name.name.as_str()],
        ItemKind::Interface(decl) => vec![decl.signature.name.name.as_str()],
        ItemKind::InterfaceAlias(decl) => vec![decl.name.name.as_str()],
        ItemKind::Function(decl) => vec![decl.signature.name.name.as_str()],
        ItemKind::ExternBlock(block) => block
            .items
            .iter()
            .map(|item| match item {
                ExternItem::OpaqueStruct(name) => name.name.as_str(),
                ExternItem::Function { signature, .. } => signature.name.name.as_str(),
                ExternItem::TypeAlias(alias) => alias.name.name.as_str(),
            })
            .collect(),
        ItemKind::Import(_)
        | ItemKind::Impl(_)
        | ItemKind::DerivableImpl(_)
        | ItemKind::Derive(_)
        | ItemKind::CInclude(_)
        | ItemKind::Error => Vec::new(),
    }
}

fn dedup_defs(defs: &mut Vec<DefId>) {
    let mut seen = HashSet::new();
    defs.retain(|id| seen.insert(*id));
}

fn dedup_modules(modules: &mut Vec<ModuleId>) {
    let mut seen = HashSet::new();
    modules.retain(|id| seen.insert(*id));
}
