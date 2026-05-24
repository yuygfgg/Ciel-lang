use std::collections::{HashMap, HashSet, VecDeque};

use crate::{
    diagnostic::{DiagResult, Diagnostic},
    hir::{
        FieldDecl, ItemKind, PrimitiveType, Type, TypeAliasTarget, TypeKind, TypeNameKind,
        VariantDecl,
    },
    resolve::{DefId, DefKind},
    thir::{
        CheckedEnum, CheckedFunction, CheckedGenericFunction, CheckedImpl, CheckedInterfaceRef,
        CheckedProgram, CheckedStruct, CheckedVariant, TBlock, TCase, TExpr, TExprKind, TForInit,
        TPattern, TStmt, TStmtKind,
    },
    typeck::{CheckedGenericInstance, type_check_generic_instance},
    types::Ty,
};

#[derive(Clone, Debug)]
pub struct MonoProgram {
    pub checked: CheckedProgram,
}

pub fn monomorphize(checked: CheckedProgram) -> DiagResult<MonoProgram> {
    MonoContext::new(checked).run()
}

struct MonoContext {
    checked: CheckedProgram,
    functions_by_def: HashMap<DefId, CheckedFunction>,
    generic_by_def: HashMap<DefId, CheckedGenericFunction>,
    generic_chains: HashMap<DefId, Vec<GenericFrame>>,
    processed: HashMap<DefId, CheckedFunction>,
    queued: HashSet<DefId>,
    worklist: VecDeque<DefId>,
    generic_instances: HashMap<(DefId, Vec<Ty>), DefId>,
    current_stack: Vec<GenericFrame>,
    next_def: usize,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug)]
struct GenericFrame {
    template_def: DefId,
    template_name: String,
    type_args: Vec<Ty>,
}

impl MonoContext {
    fn new(checked: CheckedProgram) -> Self {
        let functions_by_def = checked
            .functions
            .iter()
            .cloned()
            .map(|function| (function.def_id, function))
            .collect::<HashMap<_, _>>();
        let generic_by_def = checked
            .generic_functions
            .iter()
            .cloned()
            .map(|function| (function.def_id, function))
            .collect::<HashMap<_, _>>();
        let next_def = checked
            .functions
            .iter()
            .map(|function| function.def_id.0)
            .chain(
                checked
                    .impls
                    .iter()
                    .map(|implementation| implementation.function_def.0),
            )
            .chain(checked.resolved.defs.iter().map(|def| def.id.0))
            .max()
            .map(|id| id + 1)
            .unwrap_or(0);
        Self {
            checked,
            functions_by_def,
            generic_by_def,
            generic_chains: HashMap::new(),
            processed: HashMap::new(),
            queued: HashSet::new(),
            worklist: VecDeque::new(),
            generic_instances: HashMap::new(),
            current_stack: Vec::new(),
            next_def,
            diagnostics: Vec::new(),
        }
    }

    fn run(mut self) -> DiagResult<MonoProgram> {
        for root in self.root_defs() {
            self.mark_function(root);
        }
        while let Some(def_id) = self.worklist.pop_front() {
            self.process_function(def_id);
        }
        if !self.diagnostics.is_empty() {
            return Err(self.diagnostics);
        }

        let mut functions = self.processed.into_values().collect::<Vec<_>>();
        functions.sort_by_key(|function| function.def_id.0);

        let mut aggregates = AggregateCollector::new(&self.checked);
        aggregates.collect_from_functions(&functions);
        let reachable_defs = functions
            .iter()
            .map(|function| function.def_id)
            .collect::<HashSet<_>>();
        aggregates.collect_from_impls(&self.checked.impls, &reachable_defs);
        let (structs, enums) = aggregates.finish()?;

        let mut checked = self.checked;
        checked.functions = functions;
        checked.structs = structs;
        checked.enums = enums;
        checked.generic_functions.clear();
        Ok(MonoProgram { checked })
    }

    fn root_defs(&self) -> Vec<DefId> {
        let main = self
            .checked
            .functions
            .iter()
            .find(|function| function.name == "main" && function.body.is_some())
            .map(|function| function.def_id);
        let mut roots = Vec::new();
        if let Some(main) = main {
            roots.push(main);
        }
        roots.extend(
            self.checked
                .functions
                .iter()
                .filter(|function| function.abi.as_deref() == Some("C"))
                .map(|function| function.def_id),
        );
        roots.sort_by_key(|def_id| def_id.0);
        roots.dedup();
        roots
    }

    fn mark_function(&mut self, def_id: DefId) {
        if self.processed.contains_key(&def_id) || !self.functions_by_def.contains_key(&def_id) {
            return;
        }
        if self.queued.insert(def_id) {
            self.worklist.push_back(def_id);
        }
    }

    fn process_function(&mut self, def_id: DefId) {
        if self.processed.contains_key(&def_id) {
            return;
        }
        let Some(mut function) = self.functions_by_def.get(&def_id).cloned() else {
            return;
        };
        let previous_stack = std::mem::replace(
            &mut self.current_stack,
            self.generic_chains
                .get(&def_id)
                .cloned()
                .unwrap_or_default(),
        );
        if let Some(body) = function.body.take() {
            match self.rewrite_block(body) {
                Ok(body) => function.body = Some(body),
                Err(mut diagnostics) => self.diagnostics.append(&mut diagnostics),
            }
        }
        self.current_stack = previous_stack;
        self.processed.insert(def_id, function);
    }

    fn instantiate_generic(&mut self, def_id: DefId, type_args: &[Ty]) -> DiagResult<DefId> {
        let key = (def_id, type_args.to_vec());
        if let Some(existing) = self.generic_instances.get(&key) {
            return Ok(*existing);
        }
        let Some(template) = self.generic_by_def.get(&def_id).cloned() else {
            return Err(vec![Diagnostic::new(
                None,
                format!(
                    "internal error: missing generic function template `{}`",
                    def_id.0
                ),
            )]);
        };
        if let Some(diagnostic) = self.infinite_growth_diagnostic(&template, type_args) {
            return Err(vec![diagnostic]);
        }
        let instance_def = DefId(self.next_def);
        self.next_def += 1;
        let instance_name = generic_instance_name(&template.name, type_args);
        let CheckedGenericInstance {
            function,
            generated_functions,
            impls,
        } = type_check_generic_instance(
            &self.checked,
            &template,
            type_args,
            instance_def,
            instance_name,
            self.next_def,
        )?;
        for implementation in impls {
            if !self.checked.impls.iter().any(|existing| {
                existing.interface_name == implementation.interface_name
                    && existing.interface_args == implementation.interface_args
                    && existing.receiver_ty == implementation.receiver_ty
            }) {
                self.next_def = self.next_def.max(implementation.function_def.0 + 1);
                self.checked.impls.push(implementation);
            }
        }
        for generated in generated_functions {
            self.next_def = self.next_def.max(generated.def_id.0 + 1);
            self.functions_by_def.insert(generated.def_id, generated);
        }
        self.generic_instances.insert(key, instance_def);
        let mut chain = self.current_stack.clone();
        chain.push(GenericFrame {
            template_def: def_id,
            template_name: template.name.clone(),
            type_args: type_args.to_vec(),
        });
        self.generic_chains.insert(instance_def, chain);
        self.functions_by_def.insert(instance_def, function);
        self.mark_function(instance_def);
        Ok(instance_def)
    }

    fn infinite_growth_diagnostic(
        &self,
        template: &CheckedGenericFunction,
        type_args: &[Ty],
    ) -> Option<Diagnostic> {
        let same_template = self
            .current_stack
            .iter()
            .filter(|frame| frame.template_def == template.def_id)
            .collect::<Vec<_>>();
        if same_template.len() < 2 {
            return None;
        }
        let previous = same_template.last()?;
        if !is_strict_generic_growth(&previous.type_args, type_args) {
            return None;
        }
        let chain = self
            .current_stack
            .iter()
            .map(|frame| {
                format!(
                    "{}<{}>",
                    frame.template_name,
                    frame
                        .type_args
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .chain(std::iter::once(format!(
                "{}<{}>",
                template.name,
                type_args
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )))
            .collect::<Vec<_>>()
            .join(" -> ");
        Some(
            Diagnostic::new(
                template.function.signature.name.span,
                format!(
                    "infinite generic instantiation cycle involving `{}`",
                    template.name
                ),
            )
            .note(format!("instantiation chain: {chain}")),
        )
    }

    fn rewrite_block(&mut self, block: TBlock) -> DiagResult<TBlock> {
        let statements = block
            .statements
            .into_iter()
            .map(|stmt| self.rewrite_stmt(stmt))
            .collect::<DiagResult<Vec<_>>>()?;
        Ok(TBlock {
            statements,
            ..block
        })
    }

    fn rewrite_stmt(&mut self, stmt: TStmt) -> DiagResult<TStmt> {
        let kind = match stmt.kind {
            TStmtKind::Block(block) => TStmtKind::Block(self.rewrite_block(block)?),
            TStmtKind::VarDecl {
                ty,
                name,
                local_id,
                init,
            } => TStmtKind::VarDecl {
                ty,
                name,
                local_id,
                init: init.map(|expr| self.rewrite_expr(expr)).transpose()?,
            },
            TStmtKind::Assign { target, value } => TStmtKind::Assign {
                target: self.rewrite_expr(target)?,
                value: self.rewrite_expr(value)?,
            },
            TStmtKind::If {
                cond,
                then_block,
                else_branch,
            } => TStmtKind::If {
                cond: self.rewrite_expr(cond)?,
                then_block: self.rewrite_block(then_block)?,
                else_branch: else_branch
                    .map(|stmt| self.rewrite_stmt(*stmt).map(Box::new))
                    .transpose()?,
            },
            TStmtKind::While { cond, body } => TStmtKind::While {
                cond: self.rewrite_expr(cond)?,
                body: self.rewrite_block(body)?,
            },
            TStmtKind::For {
                init,
                cond,
                step,
                body,
            } => TStmtKind::For {
                init: init.map(|init| self.rewrite_for_init(init)).transpose()?,
                cond: cond.map(|expr| self.rewrite_expr(expr)).transpose()?,
                step: step.map(|step| self.rewrite_for_init(step)).transpose()?,
                body: self.rewrite_block(body)?,
            },
            TStmtKind::Switch {
                expr,
                enum_type_name,
                cases,
                has_default,
                default,
                can_fallthrough,
            } => {
                let expr = self.rewrite_expr(expr)?;
                let enum_type_name = enum_c_name_from_ty(&expr.ty).unwrap_or(enum_type_name);
                TStmtKind::Switch {
                    expr,
                    enum_type_name,
                    cases: cases
                        .into_iter()
                        .map(|case| self.rewrite_case(case))
                        .collect::<DiagResult<Vec<_>>>()?,
                    default: default
                        .into_iter()
                        .map(|stmt| self.rewrite_stmt(stmt))
                        .collect::<DiagResult<Vec<_>>>()?,
                    has_default,
                    can_fallthrough,
                }
            }
            TStmtKind::Defer(expr) => TStmtKind::Defer(self.rewrite_expr(expr)?),
            TStmtKind::Return(expr) => {
                TStmtKind::Return(expr.map(|expr| self.rewrite_expr(expr)).transpose()?)
            }
            TStmtKind::Expr(expr) => TStmtKind::Expr(self.rewrite_expr(expr)?),
            TStmtKind::Break => TStmtKind::Break,
            TStmtKind::Continue => TStmtKind::Continue,
            TStmtKind::Unsupported => TStmtKind::Unsupported,
        };
        Ok(TStmt { kind, ..stmt })
    }

    fn rewrite_for_init(&mut self, init: TForInit) -> DiagResult<TForInit> {
        match init {
            TForInit::VarDecl {
                ty,
                name,
                local_id,
                init,
            } => Ok(TForInit::VarDecl {
                ty,
                name,
                local_id,
                init: init.map(|expr| self.rewrite_expr(expr)).transpose()?,
            }),
            TForInit::Assign { target, value } => Ok(TForInit::Assign {
                target: self.rewrite_expr(target)?,
                value: self.rewrite_expr(value)?,
            }),
            TForInit::Expr(expr) => Ok(TForInit::Expr(self.rewrite_expr(expr)?)),
        }
    }

    fn rewrite_case(&mut self, case: TCase) -> DiagResult<TCase> {
        let pattern = self.rewrite_pattern(case.pattern)?;
        let statements = case
            .statements
            .into_iter()
            .map(|stmt| self.rewrite_stmt(stmt))
            .collect::<DiagResult<Vec<_>>>()?;
        Ok(TCase {
            pattern,
            statements,
            ..case
        })
    }

    fn rewrite_pattern(&mut self, pattern: TPattern) -> DiagResult<TPattern> {
        Ok(match pattern {
            TPattern::Wildcard { ty } => TPattern::Wildcard { ty },
            TPattern::Binding { local_id, name, ty } => TPattern::Binding { local_id, name, ty },
            TPattern::Variant {
                ty,
                enum_type_name,
                variant_name,
                variant_index,
                payload,
            } => TPattern::Variant {
                enum_type_name: enum_c_name_from_ty(&ty).unwrap_or(enum_type_name),
                ty,
                variant_name,
                variant_index,
                payload: payload
                    .into_iter()
                    .map(|pattern| self.rewrite_pattern(pattern))
                    .collect::<DiagResult<Vec<_>>>()?,
            },
        })
    }

    fn rewrite_expr(&mut self, expr: TExpr) -> DiagResult<TExpr> {
        let kind = match expr.kind {
            TExprKind::Function(def_id, name) => {
                self.mark_function(def_id);
                TExprKind::Function(def_id, name)
            }
            TExprKind::GenericFunction {
                def_id,
                name,
                type_args,
            } => {
                let instance_def = self.instantiate_generic(def_id, &type_args)?;
                let instance_name = self
                    .functions_by_def
                    .get(&instance_def)
                    .map(|function| function.name.clone())
                    .unwrap_or_else(|| generic_instance_name(&name, &type_args));
                TExprKind::Function(instance_def, instance_name)
            }
            TExprKind::Unary { op, expr: inner } => TExprKind::Unary {
                op,
                expr: Box::new(self.rewrite_expr(*inner)?),
            },
            TExprKind::Binary { op, left, right } => TExprKind::Binary {
                op,
                left: Box::new(self.rewrite_expr(*left)?),
                right: Box::new(self.rewrite_expr(*right)?),
            },
            TExprKind::Cast { expr: inner, ty } => TExprKind::Cast {
                expr: Box::new(self.rewrite_expr(*inner)?),
                ty,
            },
            TExprKind::Call { callee, args } => TExprKind::Call {
                callee: Box::new(self.rewrite_expr(*callee)?),
                args: args
                    .into_iter()
                    .map(|arg| self.rewrite_expr(arg))
                    .collect::<DiagResult<Vec<_>>>()?,
            },
            TExprKind::Closure {
                id,
                params,
                captures,
                body,
            } => TExprKind::Closure {
                id,
                params,
                captures,
                body: self.rewrite_closure_body(body)?,
            },
            TExprKind::FunctionToClosure(inner) => {
                TExprKind::FunctionToClosure(Box::new(self.rewrite_expr(*inner)?))
            }
            TExprKind::ArrayToSlice(inner) => {
                TExprKind::ArrayToSlice(Box::new(self.rewrite_expr(*inner)?))
            }
            TExprKind::MakeDynamicInterface {
                expr: inner,
                concrete_ty,
            } => {
                self.mark_dynamic_impls(&expr.ty, &concrete_ty);
                TExprKind::MakeDynamicInterface {
                    expr: Box::new(self.rewrite_expr(*inner)?),
                    concrete_ty,
                }
            }
            TExprKind::DynamicInterfaceCall {
                interface_name,
                receiver,
                args,
            } => TExprKind::DynamicInterfaceCall {
                interface_name,
                receiver: Box::new(self.rewrite_expr(*receiver)?),
                args: args
                    .into_iter()
                    .map(|arg| self.rewrite_expr(arg))
                    .collect::<DiagResult<Vec<_>>>()?,
            },
            TExprKind::Field { base, field } => TExprKind::Field {
                base: Box::new(self.rewrite_expr(*base)?),
                field,
            },
            TExprKind::Arrow { base, field } => TExprKind::Arrow {
                base: Box::new(self.rewrite_expr(*base)?),
                field,
            },
            TExprKind::Index { base, index } => TExprKind::Index {
                base: Box::new(self.rewrite_expr(*base)?),
                index: Box::new(self.rewrite_expr(*index)?),
            },
            TExprKind::Slice { base, start, end } => TExprKind::Slice {
                base: Box::new(self.rewrite_expr(*base)?),
                start: start
                    .map(|expr| self.rewrite_expr(*expr).map(Box::new))
                    .transpose()?,
                end: end
                    .map(|expr| self.rewrite_expr(*expr).map(Box::new))
                    .transpose()?,
            },
            TExprKind::Try(inner) => TExprKind::Try(Box::new(self.rewrite_expr(*inner)?)),
            TExprKind::BuiltinCloneMessage { value, message_ty } => {
                self.mark_message_clone_impls(&message_ty);
                TExprKind::BuiltinCloneMessage {
                    value: Box::new(self.rewrite_expr(*value)?),
                    message_ty,
                }
            }
            TExprKind::ActorSpawn {
                initial_state,
                handler,
                state_ty,
                message_ty,
                handler_ty,
            } => {
                self.mark_message_clone_impls(&state_ty);
                self.mark_message_clone_impls(&handler_ty);
                TExprKind::ActorSpawn {
                    initial_state: Box::new(self.rewrite_expr(*initial_state)?),
                    handler: Box::new(self.rewrite_expr(*handler)?),
                    state_ty,
                    message_ty,
                    handler_ty,
                }
            }
            TExprKind::ActorSend {
                actor,
                value,
                message_ty,
            } => {
                self.mark_message_clone_impls(&message_ty);
                TExprKind::ActorSend {
                    actor: Box::new(self.rewrite_expr(*actor)?),
                    value: Box::new(self.rewrite_expr(*value)?),
                    message_ty,
                }
            }
            TExprKind::ActorStop { actor, message_ty } => TExprKind::ActorStop {
                actor: Box::new(self.rewrite_expr(*actor)?),
                message_ty,
            },
            TExprKind::ActorJoin { actor, message_ty } => TExprKind::ActorJoin {
                actor: Box::new(self.rewrite_expr(*actor)?),
                message_ty,
            },
            TExprKind::TypeSize { ty } => TExprKind::TypeSize { ty },
            TExprKind::TypeAlign { ty } => TExprKind::TypeAlign { ty },
            TExprKind::StructLiteral { type_name, fields } => TExprKind::StructLiteral {
                type_name,
                fields: fields
                    .into_iter()
                    .map(|(name, value)| self.rewrite_expr(value).map(|value| (name, value)))
                    .collect::<DiagResult<Vec<_>>>()?,
            },
            TExprKind::EnumLiteral {
                type_name,
                variant_name,
                variant_index,
                payload,
            } => TExprKind::EnumLiteral {
                type_name,
                variant_name,
                variant_index,
                payload: payload
                    .into_iter()
                    .map(|value| self.rewrite_expr(value))
                    .collect::<DiagResult<Vec<_>>>()?,
            },
            TExprKind::ArrayLiteral(elements) => TExprKind::ArrayLiteral(
                elements
                    .into_iter()
                    .map(|element| self.rewrite_expr(element))
                    .collect::<DiagResult<Vec<_>>>()?,
            ),
            TExprKind::ArrayRepeat { element, len } => TExprKind::ArrayRepeat {
                element: Box::new(self.rewrite_expr(*element)?),
                len,
            },
            TExprKind::Local(local_id, name) => TExprKind::Local(local_id, name),
            TExprKind::Literal(literal) => TExprKind::Literal(literal),
        };
        Ok(TExpr { kind, ..expr })
    }

    fn rewrite_closure_body(
        &mut self,
        body: crate::thir::TClosureBody,
    ) -> DiagResult<crate::thir::TClosureBody> {
        Ok(match body {
            crate::thir::TClosureBody::Expr(expr) => {
                crate::thir::TClosureBody::Expr(Box::new(self.rewrite_expr(*expr)?))
            }
            crate::thir::TClosureBody::Block(block) => {
                crate::thir::TClosureBody::Block(self.rewrite_block(block)?)
            }
        })
    }

    fn mark_dynamic_impls(&mut self, dyn_ty: &Ty, concrete_ty: &Ty) {
        let Ty::DynamicInterface { name, args } = dyn_ty else {
            return;
        };
        let receiver_ty = receiver_ty_from_value_ty(concrete_ty);
        for interface in self.dynamic_view_interfaces(name, args) {
            let function_def = self
                .checked
                .impls
                .iter()
                .find(|implementation| {
                    implementation.interface_name == interface.name
                        && implementation
                            .receiver_ty
                            .as_ref()
                            .is_some_and(|receiver| receiver == &receiver_ty)
                        && implementation.interface_args.get(1..) == Some(interface.args.as_slice())
                })
                .map(|implementation| implementation.function_def);
            if let Some(function_def) = function_def {
                self.mark_function(function_def);
            }
        }
    }

    fn dynamic_view_interfaces(&self, name: &str, args: &[Ty]) -> Vec<CheckedInterfaceRef> {
        if self
            .checked
            .interfaces
            .iter()
            .any(|interface| interface.name == name)
        {
            return vec![CheckedInterfaceRef {
                name: name.to_string(),
                args: args.to_vec(),
            }];
        }
        self.checked
            .interface_aliases
            .iter()
            .find(|alias| alias.name == name)
            .map(|alias| alias.positive.clone())
            .unwrap_or_default()
    }

    fn mark_message_clone_impls(&mut self, ty: &Ty) {
        self.mark_message_clone_impls_inner(ty, &mut HashSet::new());
    }

    fn mark_message_clone_impls_inner(&mut self, ty: &Ty, seen: &mut HashSet<Ty>) {
        let ty = ty.unqualified();
        if !seen.insert(ty.clone()) {
            return;
        }
        if let Some(function_def) = self
            .checked
            .impls
            .iter()
            .find(|implementation| {
                implementation.interface_name == "clone_message"
                    && implementation
                        .receiver_ty
                        .as_ref()
                        .is_some_and(|receiver| receiver == ty)
                    && implementation.interface_args.get(1..) == Some(&[][..])
            })
            .map(|implementation| implementation.function_def)
        {
            self.mark_function(function_def);
            return;
        }
        match ty {
            Ty::Const(inner) => self.mark_message_clone_impls_inner(inner, seen),
            Ty::Array { elem, .. } => self.mark_message_clone_impls_inner(elem, seen),
            Ty::ClosureInstance { captures, .. } => {
                for capture in captures {
                    self.mark_message_clone_impls_inner(capture, seen);
                }
            }
            Ty::Named { name, args } => {
                let instance_name = aggregate_instance_name(name, args);
                if let Some(fields) = self
                    .checked
                    .structs
                    .iter()
                    .find(|strukt| strukt.name == instance_name)
                    .map(|strukt| strukt.fields.clone())
                {
                    for (_, field_ty) in fields {
                        self.mark_message_clone_impls_inner(&field_ty, seen);
                    }
                    return;
                }
                if let Some(variants) = self
                    .checked
                    .enums
                    .iter()
                    .find(|enm| enm.name == instance_name)
                    .map(|enm| enm.variants.clone())
                {
                    for variant in variants {
                        for payload in variant.payload {
                            self.mark_message_clone_impls_inner(&payload, seen);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

struct StructTemplate {
    generics: Vec<String>,
    fields: Vec<FieldDecl>,
}

struct EnumTemplate {
    generics: Vec<String>,
    variants: Vec<VariantDecl>,
}

struct AliasTemplate {
    generics: Vec<String>,
    target: TypeAliasTarget,
}

struct AggregateCollector<'a> {
    checked: &'a CheckedProgram,
    structs: HashMap<String, StructTemplate>,
    enums: HashMap<String, EnumTemplate>,
    aliases: HashMap<String, AliasTemplate>,
    alias_stack: Vec<String>,
    emitted_structs: HashSet<String>,
    visiting_structs: HashSet<String>,
    emitted_enums: HashSet<String>,
    visiting_enums: HashSet<String>,
    checked_structs: Vec<CheckedStruct>,
    checked_enums: Vec<CheckedEnum>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> AggregateCollector<'a> {
    fn new(checked: &'a CheckedProgram) -> Self {
        let mut structs = HashMap::new();
        let mut enums = HashMap::new();
        let mut aliases = HashMap::new();
        for module in &checked.hir_modules {
            for item in &module.items {
                match &item.kind {
                    ItemKind::TypeAlias(decl) => {
                        aliases.insert(
                            decl.name.name.clone(),
                            AliasTemplate {
                                generics: decl
                                    .generics
                                    .iter()
                                    .map(|param| param.name.name.clone())
                                    .collect(),
                                target: decl.target.clone(),
                            },
                        );
                    }
                    ItemKind::Struct(decl) => {
                        structs.insert(
                            decl.name.name.clone(),
                            StructTemplate {
                                generics: decl
                                    .generics
                                    .iter()
                                    .map(|param| param.name.name.clone())
                                    .collect(),
                                fields: decl.fields.clone(),
                            },
                        );
                    }
                    ItemKind::Enum(decl) => {
                        enums.insert(
                            decl.name.name.clone(),
                            EnumTemplate {
                                generics: decl
                                    .generics
                                    .iter()
                                    .map(|param| param.name.name.clone())
                                    .collect(),
                                variants: decl.variants.clone(),
                            },
                        );
                    }
                    ItemKind::ExternBlock(block) => {
                        for extern_item in &block.items {
                            if let crate::hir::ExternItem::TypeAlias(decl) = extern_item {
                                aliases.insert(
                                    decl.name.name.clone(),
                                    AliasTemplate {
                                        generics: decl
                                            .generics
                                            .iter()
                                            .map(|param| param.name.name.clone())
                                            .collect(),
                                        target: decl.target.clone(),
                                    },
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Self {
            checked,
            structs,
            enums,
            aliases,
            alias_stack: Vec::new(),
            emitted_structs: HashSet::new(),
            visiting_structs: HashSet::new(),
            emitted_enums: HashSet::new(),
            visiting_enums: HashSet::new(),
            checked_structs: Vec::new(),
            checked_enums: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn collect_from_functions(&mut self, functions: &[CheckedFunction]) {
        for function in functions {
            self.collect_ty(&function.ret);
            for (_, _, ty) in &function.params {
                self.collect_ty(ty);
            }
            if let Some(body) = &function.body {
                self.collect_block(body);
            }
        }
    }

    fn collect_from_impls(&mut self, impls: &[CheckedImpl], reachable_defs: &HashSet<DefId>) {
        for implementation in impls {
            if !reachable_defs.contains(&implementation.function_def) {
                continue;
            }
            self.collect_ty(&implementation.ret);
            for ty in &implementation.params {
                self.collect_ty(ty);
            }
            for ty in &implementation.interface_args {
                self.collect_ty(ty);
            }
        }
    }

    fn finish(mut self) -> DiagResult<(Vec<CheckedStruct>, Vec<CheckedEnum>)> {
        if self.diagnostics.is_empty() {
            Ok((self.checked_structs, self.checked_enums))
        } else {
            Err(std::mem::take(&mut self.diagnostics))
        }
    }

    fn collect_block(&mut self, block: &TBlock) {
        for stmt in &block.statements {
            self.collect_stmt(stmt);
        }
    }

    fn collect_stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::Block(block) => self.collect_block(block),
            TStmtKind::VarDecl { ty, init, .. } => {
                self.collect_ty(ty);
                if let Some(init) = init {
                    self.collect_expr(init);
                }
            }
            TStmtKind::Assign { target, value } => {
                self.collect_expr(target);
                self.collect_expr(value);
            }
            TStmtKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                self.collect_expr(cond);
                self.collect_block(then_block);
                if let Some(else_branch) = else_branch {
                    self.collect_stmt(else_branch);
                }
            }
            TStmtKind::While { cond, body } => {
                self.collect_expr(cond);
                self.collect_block(body);
            }
            TStmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(init) = init {
                    self.collect_for_init(init);
                }
                if let Some(cond) = cond {
                    self.collect_expr(cond);
                }
                if let Some(step) = step {
                    self.collect_for_init(step);
                }
                self.collect_block(body);
            }
            TStmtKind::Switch {
                expr,
                cases,
                default,
                ..
            } => {
                self.collect_expr(expr);
                for case in cases {
                    self.collect_pattern(&case.pattern);
                    for stmt in &case.statements {
                        self.collect_stmt(stmt);
                    }
                }
                for stmt in default {
                    self.collect_stmt(stmt);
                }
            }
            TStmtKind::Defer(expr) | TStmtKind::Return(Some(expr)) | TStmtKind::Expr(expr) => {
                self.collect_expr(expr);
            }
            TStmtKind::Return(None)
            | TStmtKind::Break
            | TStmtKind::Continue
            | TStmtKind::Unsupported => {}
        }
    }

    fn collect_pattern(&mut self, pattern: &TPattern) {
        self.collect_ty(pattern.ty());
        if let TPattern::Variant { payload, .. } = pattern {
            for pattern in payload {
                self.collect_pattern(pattern);
            }
        }
    }

    fn collect_for_init(&mut self, init: &TForInit) {
        match init {
            TForInit::VarDecl { ty, init, .. } => {
                self.collect_ty(ty);
                if let Some(init) = init {
                    self.collect_expr(init);
                }
            }
            TForInit::Assign { target, value } => {
                self.collect_expr(target);
                self.collect_expr(value);
            }
            TForInit::Expr(expr) => self.collect_expr(expr),
        }
    }

    fn collect_expr(&mut self, expr: &TExpr) {
        self.collect_ty(&expr.ty);
        match &expr.kind {
            TExprKind::Unary { expr, .. } | TExprKind::Cast { expr, .. } | TExprKind::Try(expr) => {
                self.collect_expr(expr);
            }
            TExprKind::Binary { left, right, .. } => {
                self.collect_expr(left);
                self.collect_expr(right);
            }
            TExprKind::Call { callee, args, .. } => {
                self.collect_expr(callee);
                for arg in args {
                    self.collect_expr(arg);
                }
            }
            TExprKind::Closure { body, .. } => self.collect_closure_body(body),
            TExprKind::FunctionToClosure(inner) => self.collect_expr(inner),
            TExprKind::ArrayToSlice(inner) => self.collect_expr(inner),
            TExprKind::MakeDynamicInterface { expr, concrete_ty } => {
                self.collect_expr(expr);
                self.collect_ty(concrete_ty);
            }
            TExprKind::DynamicInterfaceCall { receiver, args, .. } => {
                self.collect_expr(receiver);
                for arg in args {
                    self.collect_expr(arg);
                }
            }
            TExprKind::Field { base, .. } | TExprKind::Arrow { base, .. } => {
                self.collect_expr(base);
            }
            TExprKind::Index { base, index } => {
                self.collect_expr(base);
                self.collect_expr(index);
            }
            TExprKind::Slice { base, start, end } => {
                self.collect_expr(base);
                if let Some(start) = start {
                    self.collect_expr(start);
                }
                if let Some(end) = end {
                    self.collect_expr(end);
                }
            }
            TExprKind::BuiltinCloneMessage { value, message_ty } => {
                self.collect_expr(value);
                self.collect_ty(message_ty);
                self.collect_ty(&message_result_ty(message_ty.clone()));
            }
            TExprKind::ActorSpawn {
                initial_state,
                handler,
                state_ty,
                message_ty,
                handler_ty,
            } => {
                self.collect_expr(initial_state);
                self.collect_expr(handler);
                self.collect_ty(state_ty);
                self.collect_ty(message_ty);
                self.collect_ty(handler_ty);
                self.collect_ty(&message_result_ty(state_ty.clone()));
                self.collect_ty(&message_result_ty(handler_ty.clone()));
            }
            TExprKind::ActorSend {
                actor,
                value,
                message_ty,
            } => {
                self.collect_expr(actor);
                self.collect_expr(value);
                self.collect_ty(message_ty);
                self.collect_ty(&message_result_ty(message_ty.clone()));
            }
            TExprKind::ActorStop { actor, message_ty }
            | TExprKind::ActorJoin { actor, message_ty } => {
                self.collect_expr(actor);
                self.collect_ty(message_ty);
            }
            TExprKind::TypeSize { ty } | TExprKind::TypeAlign { ty } => self.collect_ty(ty),
            TExprKind::StructLiteral { fields, .. } => {
                for (_, value) in fields {
                    self.collect_expr(value);
                }
            }
            TExprKind::EnumLiteral { payload, .. } => {
                for value in payload {
                    self.collect_expr(value);
                }
            }
            TExprKind::ArrayLiteral(elements) => {
                for element in elements {
                    self.collect_expr(element);
                }
            }
            TExprKind::ArrayRepeat { element, .. } => self.collect_expr(element),
            TExprKind::Local(..)
            | TExprKind::Function(_, _)
            | TExprKind::GenericFunction { .. }
            | TExprKind::Literal(_) => {}
        }
    }

    fn collect_closure_body(&mut self, body: &crate::thir::TClosureBody) {
        match body {
            crate::thir::TClosureBody::Expr(expr) => self.collect_expr(expr),
            crate::thir::TClosureBody::Block(block) => self.collect_block(block),
        }
    }

    fn collect_ty(&mut self, ty: &Ty) {
        match ty {
            Ty::Const(inner) => self.collect_ty(inner),
            Ty::Named { name, args } => {
                if self.structs.contains_key(name) {
                    self.instantiate_struct(name, args);
                }
                if self.enums.contains_key(name) {
                    self.instantiate_enum(name, args);
                }
                for arg in args {
                    self.collect_ty(arg);
                }
            }
            Ty::Pointer { inner, .. } => self.collect_ty(inner),
            Ty::Array { elem, .. } | Ty::Slice(elem) => self.collect_ty(elem),
            Ty::DynamicInterface { args, .. } => {
                for arg in args {
                    self.collect_ty(arg);
                }
            }
            Ty::Function { ret, params, .. } => {
                self.collect_ty(ret);
                for param in params {
                    self.collect_ty(param);
                }
            }
            Ty::Closure { ret, params } | Ty::ClosureInstance { ret, params, .. } => {
                self.collect_ty(ret);
                for param in params {
                    self.collect_ty(param);
                }
            }
            Ty::Never
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

    fn instantiate_struct(&mut self, name: &str, args: &[Ty]) {
        let instance_name = aggregate_instance_name(name, args);
        if self.emitted_structs.contains(&instance_name)
            || self.visiting_structs.contains(&instance_name)
        {
            return;
        }
        let Some(template) = self.structs.get(name) else {
            return;
        };
        if template.generics.len() != args.len() {
            self.diagnostics.push(Diagnostic::new(
                None,
                format!(
                    "struct `{name}` expects {} type arguments, got {}",
                    template.generics.len(),
                    args.len()
                ),
            ));
            return;
        }
        let generics = template.generics.clone();
        let fields = template.fields.clone();
        let subst = generics
            .into_iter()
            .zip(args.iter().cloned())
            .collect::<HashMap<_, _>>();
        self.visiting_structs.insert(instance_name.clone());
        let fields = fields
            .iter()
            .map(|field| {
                let ty = self.lower_ast_type(&field.ty, &subst);
                self.collect_ty(&ty);
                (field.name.name.clone(), ty)
            })
            .collect::<Vec<_>>();
        self.visiting_structs.remove(&instance_name);
        self.emitted_structs.insert(instance_name.clone());
        self.checked_structs.push(CheckedStruct {
            name: instance_name,
            fields,
        });
    }

    fn instantiate_enum(&mut self, name: &str, args: &[Ty]) {
        let instance_name = aggregate_instance_name(name, args);
        if self.emitted_enums.contains(&instance_name)
            || self.visiting_enums.contains(&instance_name)
        {
            return;
        }
        let Some(template) = self.enums.get(name) else {
            return;
        };
        if template.generics.len() != args.len() {
            self.diagnostics.push(Diagnostic::new(
                None,
                format!(
                    "enum `{name}` expects {} type arguments, got {}",
                    template.generics.len(),
                    args.len()
                ),
            ));
            return;
        }
        let generics = template.generics.clone();
        let variants = template.variants.clone();
        let subst = generics
            .into_iter()
            .zip(args.iter().cloned())
            .collect::<HashMap<_, _>>();
        self.visiting_enums.insert(instance_name.clone());
        let variants = variants
            .iter()
            .map(|variant| {
                let payload = variant
                    .payload
                    .iter()
                    .filter_map(|payload| {
                        let ty = self.lower_ast_type(payload, &subst);
                        if ty.is_erased_value() {
                            None
                        } else {
                            self.collect_ty(&ty);
                            Some(ty)
                        }
                    })
                    .collect::<Vec<_>>();
                CheckedVariant {
                    name: variant.name.name.clone(),
                    payload,
                }
            })
            .collect::<Vec<_>>();
        self.visiting_enums.remove(&instance_name);
        self.emitted_enums.insert(instance_name.clone());
        self.checked_enums.push(CheckedEnum {
            name: instance_name,
            variants,
        });
    }

    fn lower_ast_type(&mut self, ty: &Type, subst: &HashMap<String, Ty>) -> Ty {
        match &ty.kind {
            TypeKind::Never => Ty::Never,
            TypeKind::Void => Ty::Void,
            TypeKind::Primitive(primitive) => ty_from_primitive(primitive),
            TypeKind::Named(type_name, args) => {
                let (name, def_kind) = match &type_name.kind {
                    TypeNameKind::Def(def_id) => {
                        let def = self.checked.resolved.def(*def_id);
                        (def.name.clone(), Some(def.kind.clone()))
                    }
                    TypeNameKind::Generic(generic) => (generic.clone(), None),
                    TypeNameKind::Error => return Ty::Unknown,
                };
                if args.is_empty()
                    && let Some(replacement) = subst.get(&name)
                {
                    return replacement.clone();
                }
                let args = args
                    .iter()
                    .map(|arg| self.lower_ast_type(arg, subst))
                    .collect::<Vec<_>>();
                if matches!(def_kind, Some(DefKind::TypeAlias))
                    && let Some(alias) = self.aliases.get(&name)
                {
                    if self.alias_stack.contains(&name) {
                        self.diagnostics.push(Diagnostic::new(
                            type_name.span,
                            format!("recursive type alias `{name}`"),
                        ));
                        return Ty::Unknown;
                    }
                    if alias.generics.len() != args.len() {
                        self.diagnostics.push(Diagnostic::new(
                            type_name.span,
                            format!(
                                "type alias `{name}` expects {} type arguments, got {}",
                                alias.generics.len(),
                                args.len()
                            ),
                        ));
                        return Ty::Unknown;
                    }
                    let mut alias_subst = subst.clone();
                    for (generic, arg) in alias.generics.iter().zip(args.iter()) {
                        alias_subst.insert(generic.clone(), arg.clone());
                    }
                    let alias_target = alias.target.clone();
                    self.alias_stack.push(name.clone());
                    let lowered = match &alias_target {
                        TypeAliasTarget::Type(alias_ty) => {
                            self.lower_ast_type(alias_ty, &alias_subst)
                        }
                        TypeAliasTarget::CSpelling { abi, spelling } => Ty::CSpelling {
                            abi: abi.clone(),
                            spelling: spelling.clone(),
                        },
                    };
                    self.alias_stack.pop();
                    return lowered;
                }
                if self
                    .checked
                    .interfaces
                    .iter()
                    .any(|interface| interface.name == name)
                    || self
                        .checked
                        .interface_aliases
                        .iter()
                        .any(|alias| alias.name == name)
                {
                    Ty::DynamicInterface { name, args }
                } else {
                    Ty::Named { name, args }
                }
            }
            TypeKind::Pointer { nullable, inner } => Ty::Pointer {
                nullable: *nullable,
                inner: Box::new(self.lower_ast_type(inner, subst)),
            },
            TypeKind::Const(inner) => Ty::Const(Box::new(self.lower_ast_type(inner, subst))),
            TypeKind::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(self.lower_ast_type(elem, subst)),
            },
            TypeKind::Slice(elem) => Ty::Slice(Box::new(self.lower_ast_type(elem, subst))),
            TypeKind::Function { abi, ret, params } => Ty::Function {
                abi: abi.clone(),
                ret: Box::new(self.lower_ast_type(ret, subst)),
                params: params
                    .iter()
                    .map(|param| self.lower_ast_type(param, subst))
                    .collect(),
            },
            TypeKind::Closure { ret, params } => Ty::Closure {
                ret: Box::new(self.lower_ast_type(ret, subst)),
                params: params
                    .iter()
                    .map(|param| self.lower_ast_type(param, subst))
                    .collect(),
            },
        }
    }
}

fn generic_instance_name(name: &str, args: &[Ty]) -> String {
    if args.is_empty() {
        name.to_string()
    } else {
        format!(
            "{}__{}",
            name,
            args.iter()
                .map(mangle_ty_fragment)
                .collect::<Vec<_>>()
                .join("_")
        )
    }
}

fn aggregate_instance_name(name: &str, args: &[Ty]) -> String {
    if args.is_empty() {
        name.to_string()
    } else {
        format!(
            "{}_{}",
            name,
            args.iter()
                .map(mangle_ty_fragment)
                .collect::<Vec<_>>()
                .join("_")
        )
    }
}

fn enum_c_name_from_ty(ty: &Ty) -> Option<String> {
    match ty.unqualified() {
        Ty::Named { name, args } => Some(aggregate_instance_name(name, args)),
        _ => None,
    }
}

fn message_result_ty(ok_ty: Ty) -> Ty {
    Ty::Named {
        name: "Result".to_string(),
        args: vec![
            ok_ty,
            Ty::Named {
                name: "Error".to_string(),
                args: Vec::new(),
            },
        ],
    }
}

fn receiver_ty_from_value_ty(ty: &Ty) -> Ty {
    match ty {
        Ty::Const(inner) => receiver_ty_from_value_ty(inner),
        Ty::Pointer { inner, .. } => (**inner).clone(),
        other => other.clone(),
    }
}

fn is_strict_generic_growth(previous: &[Ty], next: &[Ty]) -> bool {
    let previous_complexity = previous.iter().map(type_complexity).sum::<usize>();
    let next_complexity = next.iter().map(type_complexity).sum::<usize>();
    next_complexity > previous_complexity
        && previous
            .iter()
            .any(|old| next.iter().any(|new| ty_contains(new, old)))
}

fn type_complexity(ty: &Ty) -> usize {
    match ty {
        Ty::Const(inner) => type_complexity(inner),
        Ty::Pointer { inner, .. } => 1 + type_complexity(inner),
        Ty::Array { elem, .. } | Ty::Slice(elem) => 1 + type_complexity(elem),
        Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
            1 + args.iter().map(type_complexity).sum::<usize>()
        }
        Ty::Function { ret, params, .. } => {
            1 + type_complexity(ret) + params.iter().map(type_complexity).sum::<usize>()
        }
        Ty::Closure { ret, params } => {
            1 + type_complexity(ret) + params.iter().map(type_complexity).sum::<usize>()
        }
        Ty::ClosureInstance {
            ret,
            params,
            captures,
            ..
        } => {
            1 + type_complexity(ret)
                + params.iter().map(type_complexity).sum::<usize>()
                + captures.iter().map(type_complexity).sum::<usize>()
        }
        Ty::Never
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
        | Ty::Unknown => 0,
    }
}

fn ty_contains(container: &Ty, needle: &Ty) -> bool {
    if container == needle {
        return true;
    }
    match container {
        Ty::Const(inner) => ty_contains(inner, needle),
        Ty::Pointer { inner, .. } => ty_contains(inner, needle),
        Ty::Array { elem, .. } | Ty::Slice(elem) => ty_contains(elem, needle),
        Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
            args.iter().any(|arg| ty_contains(arg, needle))
        }
        Ty::Function { ret, params, .. } => {
            ty_contains(ret, needle) || params.iter().any(|param| ty_contains(param, needle))
        }
        Ty::Closure { ret, params } => {
            ty_contains(ret, needle) || params.iter().any(|param| ty_contains(param, needle))
        }
        Ty::ClosureInstance {
            ret,
            params,
            captures,
            ..
        } => {
            ty_contains(ret, needle)
                || params.iter().any(|param| ty_contains(param, needle))
                || captures.iter().any(|capture| ty_contains(capture, needle))
        }
        Ty::Never
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
        | Ty::Unknown => false,
    }
}

fn ty_from_primitive(primitive: &PrimitiveType) -> Ty {
    match primitive {
        PrimitiveType::Bool => Ty::Bool,
        PrimitiveType::Char => Ty::Char,
        PrimitiveType::I8 => Ty::I8,
        PrimitiveType::I16 => Ty::I16,
        PrimitiveType::I32 => Ty::I32,
        PrimitiveType::I64 => Ty::I64,
        PrimitiveType::U8 => Ty::U8,
        PrimitiveType::U16 => Ty::U16,
        PrimitiveType::U32 => Ty::U32,
        PrimitiveType::U64 => Ty::U64,
        PrimitiveType::Usize => Ty::Usize,
        PrimitiveType::F32 => Ty::F32,
        PrimitiveType::F64 => Ty::F64,
    }
}

fn mangle_ty_fragment(ty: &Ty) -> String {
    match ty {
        Ty::Const(inner) => format!("const_{}", mangle_ty_fragment(inner)),
        Ty::Never => "never".to_string(),
        Ty::Void => "void".to_string(),
        Ty::Bool => "bool".to_string(),
        Ty::Char => "char".to_string(),
        Ty::I8 => "i8".to_string(),
        Ty::I16 => "i16".to_string(),
        Ty::I32 => "i32".to_string(),
        Ty::I64 => "i64".to_string(),
        Ty::U8 => "u8".to_string(),
        Ty::U16 => "u16".to_string(),
        Ty::U32 => "u32".to_string(),
        Ty::U64 => "u64".to_string(),
        Ty::Usize => "usize".to_string(),
        Ty::F32 => "f32".to_string(),
        Ty::F64 => "f64".to_string(),
        Ty::CSpelling { abi, spelling } => {
            format!(
                "c_{}_{}",
                mangle_abi_fragment(Some(abi)),
                sanitize_mangle_fragment(spelling)
            )
        }
        Ty::Pointer { inner, nullable } => {
            if *nullable {
                format!("qptr_{}", mangle_ty_fragment(inner))
            } else {
                format!("ptr_{}", mangle_ty_fragment(inner))
            }
        }
        Ty::Array { len, elem } => format!("arr{len}_{}", mangle_ty_fragment(elem)),
        Ty::Slice(elem) => format!("slice_{}", mangle_ty_fragment(elem)),
        Ty::Named { name, args } => aggregate_instance_name(name, args),
        Ty::Generic(name) => format!("gen_{name}"),
        Ty::DynamicInterface { name, args } => {
            if args.is_empty() {
                format!("dyn_{name}")
            } else {
                format!(
                    "dyn_{}_{}",
                    name,
                    args.iter()
                        .map(mangle_ty_fragment)
                        .collect::<Vec<_>>()
                        .join("_")
                )
            }
        }
        Ty::Function { abi, ret, params } => {
            let params = if params.is_empty() {
                "void".to_string()
            } else {
                params
                    .iter()
                    .map(mangle_ty_fragment)
                    .collect::<Vec<_>>()
                    .join("_")
            };
            format!(
                "fn_{}_ret_{}_args_{}",
                mangle_abi_fragment(abi.as_deref()),
                mangle_ty_fragment(ret),
                params
            )
        }
        Ty::Closure { ret, params } => {
            let params = if params.is_empty() {
                "void".to_string()
            } else {
                params
                    .iter()
                    .map(mangle_ty_fragment)
                    .collect::<Vec<_>>()
                    .join("_")
            };
            format!("closure_ret_{}_args_{}", mangle_ty_fragment(ret), params)
        }
        Ty::ClosureInstance {
            id,
            ret,
            params,
            captures,
        } => {
            let params = if params.is_empty() {
                "void".to_string()
            } else {
                params
                    .iter()
                    .map(mangle_ty_fragment)
                    .collect::<Vec<_>>()
                    .join("_")
            };
            let captures = if captures.is_empty() {
                "empty".to_string()
            } else {
                captures
                    .iter()
                    .map(mangle_ty_fragment)
                    .collect::<Vec<_>>()
                    .join("_")
            };
            format!(
                "closure_inst{id}_ret_{}_args_{}_caps_{}",
                mangle_ty_fragment(ret),
                params,
                captures
            )
        }
        Ty::Unknown => "unknown".to_string(),
    }
}

fn mangle_abi_fragment(abi: Option<&str>) -> String {
    let Some(abi) = abi else {
        return "ciel".to_string();
    };
    let out = abi
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    if out.is_empty() {
        "abi".to_string()
    } else {
        out
    }
}

fn sanitize_mangle_fragment(value: &str) -> String {
    let out = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    if out.is_empty() {
        "type".to_string()
    } else {
        out
    }
}
