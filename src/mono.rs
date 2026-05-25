use std::collections::{HashMap, HashSet, VecDeque};

use crate::{
    diagnostic::{DiagResult, Diagnostic},
    hir::{
        ConstraintExpr, FieldDecl, ItemKind, NameRef, NameRefKind, PrimitiveType, Type,
        TypeAliasTarget, TypeKind, TypeNameKind, VariantDecl,
    },
    resolve::{DefId, DefKind, ResolvedProgram},
    thir::{
        CheckedEnum, CheckedFunction, CheckedGenericFunction, CheckedImpl, CheckedInterface,
        CheckedInterfaceRef, CheckedProgram, CheckedStruct, CheckedVariant, TBlock, TCase, TExpr,
        TExprKind, TForInit, TPattern, TStmt, TStmtKind,
    },
    typeck::{CheckedGenericInstance, type_check_generic_instance},
    types::{ConstraintBounds, ConstraintRef, Ty},
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
                self.mark_retained_closure_witness_impls(&expr.ty, &inner.ty);
                TExprKind::FunctionToClosure(Box::new(self.rewrite_expr(*inner)?))
            }
            TExprKind::RetainClosure {
                expr: inner,
                source_ty,
            } => {
                self.mark_retained_closure_witness_impls(&expr.ty, &source_ty);
                TExprKind::RetainClosure {
                    expr: Box::new(self.rewrite_expr(*inner)?),
                    source_ty,
                }
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
            TExprKind::RetainedClosureInterfaceCall {
                interface_name,
                interface_args,
                receiver,
                args,
            } => TExprKind::RetainedClosureInterfaceCall {
                interface_name,
                interface_args,
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
            TExprKind::MetaAsRefRepr { value, source_ty } => TExprKind::MetaAsRefRepr {
                value: Box::new(self.rewrite_expr(*value)?),
                source_ty,
            },
            TExprKind::MetaIntoRepr { value, source_ty } => TExprKind::MetaIntoRepr {
                value: Box::new(self.rewrite_expr(*value)?),
                source_ty,
            },
            TExprKind::MetaFromRepr { value, target_ty } => TExprKind::MetaFromRepr {
                value: Box::new(self.rewrite_expr(*value)?),
                target_ty,
            },
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

    fn mark_retained_closure_witness_impls(&mut self, target_ty: &Ty, source_ty: &Ty) {
        for capability in retained_closure_capabilities(target_ty) {
            if retained_closure_has_capability(source_ty, &capability) {
                continue;
            }
            if is_clone_message_capability(&capability) {
                self.mark_message_clone_impls(source_ty);
                continue;
            }
            if let Some(function_def) = self
                .checked
                .impls
                .iter()
                .find(|implementation| {
                    implementation.interface_name == capability.name
                        && implementation
                            .receiver_ty
                            .as_ref()
                            .is_some_and(|receiver| receiver == source_ty.unqualified())
                        && implementation.interface_args.get(1..)
                            == Some(capability.args.as_slice())
                })
                .map(|implementation| implementation.function_def)
            {
                self.mark_function(function_def);
            }
        }
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
            Ty::Hole(_) => {}
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
    retained_closure_interface_tys: HashSet<(Ty, ConstraintRef)>,
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
            retained_closure_interface_tys: HashSet::new(),
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
            TExprKind::FunctionToClosure(inner) => {
                self.collect_expr(inner);
                self.collect_retained_closure_witness_tys(&expr.ty, &inner.ty);
                if retained_closure_capabilities(&expr.ty)
                    .iter()
                    .any(is_clone_message_capability)
                {
                    self.collect_message_clone_result_tys(&expr.ty);
                }
            }
            TExprKind::RetainClosure {
                expr: inner,
                source_ty,
            } => {
                self.collect_expr(inner);
                self.collect_ty(source_ty);
                self.collect_retained_closure_witness_tys(&expr.ty, source_ty);
                if retained_closure_capabilities(&expr.ty)
                    .iter()
                    .any(is_clone_message_capability)
                {
                    self.collect_message_clone_result_tys(&expr.ty);
                    self.collect_message_clone_result_tys(source_ty);
                }
            }
            TExprKind::ArrayToSlice(inner) => self.collect_expr(inner),
            TExprKind::MakeDynamicInterface { expr, concrete_ty } => {
                self.collect_expr(expr);
                self.collect_ty(concrete_ty);
            }
            TExprKind::DynamicInterfaceCall { receiver, args, .. }
            | TExprKind::RetainedClosureInterfaceCall { receiver, args, .. } => {
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
                self.collect_message_clone_result_tys(message_ty);
            }
            TExprKind::MetaAsRefRepr { value, source_ty }
            | TExprKind::MetaIntoRepr { value, source_ty } => {
                self.collect_expr(value);
                self.collect_ty(source_ty);
            }
            TExprKind::MetaFromRepr { value, target_ty } => {
                self.collect_expr(value);
                self.collect_ty(target_ty);
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
                self.collect_message_clone_result_tys(state_ty);
                self.collect_message_clone_result_tys(handler_ty);
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
                self.collect_message_clone_result_tys(message_ty);
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

    fn collect_message_clone_result_tys(&mut self, ty: &Ty) {
        self.collect_message_clone_result_tys_inner(ty, &mut HashSet::new());
    }

    fn collect_message_clone_result_tys_inner(&mut self, ty: &Ty, seen: &mut HashSet<Ty>) {
        let ty = ty.unqualified();
        if !seen.insert(ty.clone()) {
            return;
        }
        self.collect_ty(&message_result_ty(ty.clone()));
        match ty {
            Ty::Const(inner) => self.collect_message_clone_result_tys_inner(inner, seen),
            Ty::Array { elem, .. } => self.collect_message_clone_result_tys_inner(elem, seen),
            Ty::ClosureInstance { captures, .. } => {
                for capture in captures {
                    self.collect_message_clone_result_tys_inner(capture, seen);
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
                        self.collect_message_clone_result_tys_inner(&field_ty, seen);
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
                            self.collect_message_clone_result_tys_inner(&payload, seen);
                        }
                    }
                }
            }
            _ => {}
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
            Ty::Closure {
                ret,
                params,
                constraints,
            } => {
                self.collect_ty(ret);
                for param in params {
                    self.collect_ty(param);
                }
                self.collect_constraint_bounds_tys(constraints);
                for capability in &constraints.positive {
                    self.collect_retained_closure_interface_tys(ty, capability);
                }
            }
            Ty::ClosureInstance { ret, params, .. } => {
                self.collect_ty(ret);
                for param in params {
                    self.collect_ty(param);
                }
            }
            Ty::Hole(_)
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

    fn collect_constraint_bounds_tys(&mut self, bounds: &ConstraintBounds) {
        for entry in bounds.positive.iter().chain(bounds.negative.iter()) {
            for arg in &entry.args {
                self.collect_ty(arg);
            }
        }
    }

    fn collect_retained_closure_witness_tys(&mut self, target_ty: &Ty, source_ty: &Ty) {
        for capability in retained_closure_capabilities(target_ty) {
            self.collect_retained_closure_interface_tys(target_ty, &capability);
            self.collect_retained_closure_interface_tys(source_ty, &capability);
        }
    }

    fn collect_retained_closure_interface_tys(
        &mut self,
        receiver_ty: &Ty,
        capability: &ConstraintRef,
    ) {
        let key = (receiver_ty.unqualified().clone(), capability.clone());
        if !self.retained_closure_interface_tys.insert(key) {
            return;
        }
        for arg in &capability.args {
            self.collect_ty(arg);
        }
        let Some(interface) = self
            .checked
            .interfaces
            .iter()
            .find(|interface| interface.name == capability.name)
        else {
            return;
        };
        let subst = retained_closure_interface_subst(interface, receiver_ty, &capability.args);
        let ret_ty = interface.ret.clone();
        let params = interface.params.iter().skip(1).cloned().collect::<Vec<_>>();
        let ret = substitute_ty(&ret_ty, &subst);
        self.collect_ty(&ret);
        for param in params {
            let ty = substitute_ty(&param, &subst);
            self.collect_ty(&ty);
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

    fn normalize_meta_repr_markers(&mut self, ty: &Ty) -> Ty {
        match ty {
            Ty::Const(inner) => Ty::Const(Box::new(self.normalize_meta_repr_markers(inner))),
            Ty::Pointer { nullable, inner } => Ty::Pointer {
                nullable: *nullable,
                inner: Box::new(self.normalize_meta_repr_markers(inner)),
            },
            Ty::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(self.normalize_meta_repr_markers(elem)),
            },
            Ty::Slice(elem) => Ty::Slice(Box::new(self.normalize_meta_repr_markers(elem))),
            Ty::Named { name, args } => {
                let args = args
                    .iter()
                    .map(|arg| self.normalize_meta_repr_markers(arg))
                    .collect::<Vec<_>>();
                if let Some(borrowed) = meta_repr_marker_name(name)
                    && args.len() == 1
                    && !contains_generic(&args[0])
                {
                    return self.meta_repr_ty(None, &args[0], borrowed);
                }
                Ty::Named {
                    name: name.clone(),
                    args,
                }
            }
            Ty::DynamicInterface { name, args } => Ty::DynamicInterface {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.normalize_meta_repr_markers(arg))
                    .collect(),
            },
            Ty::Function { abi, ret, params } => Ty::Function {
                abi: abi.clone(),
                ret: Box::new(self.normalize_meta_repr_markers(ret)),
                params: params
                    .iter()
                    .map(|param| self.normalize_meta_repr_markers(param))
                    .collect(),
            },
            Ty::Closure {
                ret,
                params,
                constraints,
            } => Ty::Closure {
                ret: Box::new(self.normalize_meta_repr_markers(ret)),
                params: params
                    .iter()
                    .map(|param| self.normalize_meta_repr_markers(param))
                    .collect(),
                constraints: self.normalize_constraint_bounds(constraints),
            },
            Ty::ClosureInstance {
                id,
                ret,
                params,
                captures,
            } => Ty::ClosureInstance {
                id: *id,
                ret: Box::new(self.normalize_meta_repr_markers(ret)),
                params: params
                    .iter()
                    .map(|param| self.normalize_meta_repr_markers(param))
                    .collect(),
                captures: captures
                    .iter()
                    .map(|capture| self.normalize_meta_repr_markers(capture))
                    .collect(),
            },
            other => other.clone(),
        }
    }

    fn meta_repr_ty(
        &mut self,
        span: impl Into<Option<crate::span::Span>>,
        source_ty: &Ty,
        borrowed: bool,
    ) -> Ty {
        if contains_generic(source_ty) {
            return std_meta_repr_marker_ty(borrowed, source_ty.clone());
        }
        match source_ty.unqualified() {
            Ty::Named { name, args } => {
                let instance_name = aggregate_instance_name(name, args);
                self.instantiate_struct(name, args);
                if let Some(fields) = self
                    .checked_structs
                    .iter()
                    .find(|strukt| strukt.name == instance_name)
                    .map(|strukt| strukt.fields.clone())
                    .or_else(|| {
                        self.checked
                            .structs
                            .iter()
                            .find(|strukt| strukt.name == instance_name)
                            .map(|strukt| strukt.fields.clone())
                    })
                {
                    return meta_product_ty(
                        fields.into_iter().map(|(_, ty)| ty),
                        if borrowed { "FieldRef" } else { "Field" },
                    );
                }
                self.instantiate_enum(name, args);
                if let Some(enm) = self
                    .checked_enums
                    .iter()
                    .find(|enm| enm.name == instance_name)
                    .cloned()
                    .or_else(|| {
                        self.checked
                            .enums
                            .iter()
                            .find(|enm| enm.name == instance_name)
                            .cloned()
                    })
                {
                    return meta_sum_ty(&enm.variants, borrowed);
                }
                self.push_meta_unsupported_repr(span, source_ty);
                Ty::Unknown
            }
            Ty::ClosureInstance { captures, .. } => meta_product_ty(
                captures.iter().filter(|ty| !ty.is_erased_value()).cloned(),
                if borrowed { "FieldRef" } else { "Field" },
            ),
            Ty::Closure { .. } => {
                self.diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "meta structural representation requires a concrete closure value, got erased closure `{source_ty}`"
                    ),
                ));
                Ty::Unknown
            }
            _ => {
                self.push_meta_unsupported_repr(span, source_ty);
                Ty::Unknown
            }
        }
    }

    fn push_meta_unsupported_repr(
        &mut self,
        span: impl Into<Option<crate::span::Span>>,
        source_ty: &Ty,
    ) {
        self.diagnostics.push(Diagnostic::new(
            span,
            format!(
                "meta structural representation supports visible structs, enums, and concrete closure values, got `{source_ty}`"
            ),
        ));
    }

    fn std_meta_repr_marker(&self, def_id: DefId, name: &str) -> Option<bool> {
        let borrowed = std_meta_repr_source_name(name)?;
        let def = self.checked.resolved.def(def_id);
        if self.checked.resolved.modules[def.module.0]
            .path
            .ends_with(std::path::Path::new("std/meta.ciel"))
        {
            Some(borrowed)
        } else {
            None
        }
    }

    fn lower_ast_type(&mut self, ty: &Type, subst: &HashMap<String, Ty>) -> Ty {
        match &ty.kind {
            TypeKind::Hole => Ty::Unknown,
            TypeKind::Never => Ty::Never,
            TypeKind::Void => Ty::Void,
            TypeKind::Primitive(primitive) => ty_from_primitive(primitive),
            TypeKind::Named(type_name, args) => {
                let (name, def_kind, def_id) = match &type_name.kind {
                    TypeNameKind::Def(def_id) => {
                        let def = self.checked.resolved.def(*def_id);
                        (def.name.clone(), Some(def.kind.clone()), Some(*def_id))
                    }
                    TypeNameKind::Generic(generic) => (generic.clone(), None, None),
                    TypeNameKind::Error => return Ty::Unknown,
                };
                if args.is_empty()
                    && let Some(replacement) = subst.get(&name)
                {
                    return self.normalize_meta_repr_markers(replacement);
                }
                let args = args
                    .iter()
                    .map(|arg| self.lower_ast_type(arg, subst))
                    .collect::<Vec<_>>();
                if let Some(def_id) = def_id
                    && let Some(borrowed) = self.std_meta_repr_marker(def_id, &name)
                {
                    if args.len() != 1 {
                        self.diagnostics.push(Diagnostic::new(
                            type_name.span,
                            format!("meta::{name} requires exactly one type argument"),
                        ));
                        return Ty::Unknown;
                    }
                    if contains_generic(&args[0]) {
                        return std_meta_repr_marker_ty(borrowed, args[0].clone());
                    }
                    return self.meta_repr_ty(type_name.span, &args[0], borrowed);
                }
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
                    return self.normalize_meta_repr_markers(&lowered);
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
                    self.normalize_meta_repr_markers(&Ty::Named { name, args })
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
            TypeKind::Closure {
                ret,
                params,
                constraint,
            } => Ty::Closure {
                ret: Box::new(self.lower_ast_type(ret, subst)),
                params: params
                    .iter()
                    .map(|param| self.lower_ast_type(param, subst))
                    .collect(),
                constraints: constraint
                    .as_ref()
                    .map(|constraint| self.constraint_bounds(constraint, subst))
                    .unwrap_or_default(),
            },
        }
    }

    fn normalize_constraint_bounds(&mut self, bounds: &ConstraintBounds) -> ConstraintBounds {
        ConstraintBounds {
            positive: bounds
                .positive
                .iter()
                .map(|entry| ConstraintRef {
                    name: entry.name.clone(),
                    args: entry
                        .args
                        .iter()
                        .map(|arg| self.normalize_meta_repr_markers(arg))
                        .collect(),
                })
                .collect(),
            negative: bounds
                .negative
                .iter()
                .map(|entry| ConstraintRef {
                    name: entry.name.clone(),
                    args: entry
                        .args
                        .iter()
                        .map(|arg| self.normalize_meta_repr_markers(arg))
                        .collect(),
                })
                .collect(),
        }
    }

    fn constraint_bounds(
        &mut self,
        expr: &ConstraintExpr,
        subst: &HashMap<String, Ty>,
    ) -> ConstraintBounds {
        let mut bounds = ConstraintBounds::default();
        for term in &expr.terms {
            let args = term
                .args
                .iter()
                .map(|arg| self.lower_ast_type(arg, subst))
                .collect::<Vec<_>>();
            let view = self.interface_view(
                &name_ref_canonical(&self.checked.resolved, &term.name),
                &args,
            );
            if term.removed {
                bounds
                    .positive
                    .retain(|entry| !view.iter().any(|removed| removed == entry));
            } else if term.negated {
                for entry in view {
                    if !bounds.negative.contains(&entry) {
                        bounds.negative.push(entry);
                    }
                }
            } else {
                for entry in view {
                    if !bounds.positive.contains(&entry) {
                        bounds.positive.push(entry);
                    }
                }
            }
        }
        bounds
    }

    fn interface_view(&self, name: &str, args: &[Ty]) -> Vec<ConstraintRef> {
        self.checked
            .interface_aliases
            .iter()
            .find(|alias| alias.name == name)
            .map(|alias| {
                alias
                    .positive
                    .iter()
                    .map(|entry| ConstraintRef {
                        name: entry.name.clone(),
                        args: entry.args.clone(),
                    })
                    .collect()
            })
            .unwrap_or_else(|| {
                vec![ConstraintRef {
                    name: name.to_string(),
                    args: args.to_vec(),
                }]
            })
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

fn meta_product_ty<I>(fields: I, head_name: &str) -> Ty
where
    I: IntoIterator<Item = Ty>,
    I::IntoIter: DoubleEndedIterator,
{
    fields
        .into_iter()
        .rev()
        .fold(meta_named("HNil", Vec::new()), |tail, field_ty| {
            let head = meta_named(head_name, vec![field_ty]);
            meta_named("HCons", vec![head, tail])
        })
}

fn meta_sum_ty(variants: &[CheckedVariant], borrowed: bool) -> Ty {
    variants
        .iter()
        .rev()
        .fold(meta_named("CoNil", Vec::new()), |tail, variant| {
            let payload_head = if borrowed { "PayloadRef" } else { "Payload" };
            let payload = meta_product_ty(variant.payload.iter().cloned(), payload_head);
            let variant_head = if borrowed { "VariantRef" } else { "Variant" };
            let head = meta_named(variant_head, vec![payload]);
            meta_named("Coproduct", vec![head, tail])
        })
}

fn meta_named(name: &str, args: Vec<Ty>) -> Ty {
    Ty::Named {
        name: name.to_string(),
        args,
    }
}

const STD_META_REF_REPR_MARKER: &str = "__ciel_std_meta_RefRepr";
const STD_META_REPR_MARKER: &str = "__ciel_std_meta_Repr";

fn std_meta_repr_marker_ty(borrowed: bool, source_ty: Ty) -> Ty {
    Ty::Named {
        name: if borrowed {
            STD_META_REF_REPR_MARKER
        } else {
            STD_META_REPR_MARKER
        }
        .to_string(),
        args: vec![source_ty],
    }
}

fn std_meta_repr_source_name(name: &str) -> Option<bool> {
    match name {
        "RefRepr" => Some(true),
        "Repr" => Some(false),
        _ => None,
    }
}

fn meta_repr_marker_name(name: &str) -> Option<bool> {
    match name {
        STD_META_REF_REPR_MARKER => Some(true),
        STD_META_REPR_MARKER => Some(false),
        _ => None,
    }
}

fn contains_generic(ty: &Ty) -> bool {
    match ty {
        Ty::Const(inner) => contains_generic(inner),
        Ty::Generic(_) => true,
        Ty::Pointer { inner, .. } => contains_generic(inner),
        Ty::Array { elem, .. } | Ty::Slice(elem) => contains_generic(elem),
        Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
            args.iter().any(contains_generic)
        }
        Ty::Function { ret, params, .. } => {
            contains_generic(ret) || params.iter().any(contains_generic)
        }
        Ty::Closure {
            ret,
            params,
            constraints,
        } => {
            contains_generic(ret)
                || params.iter().any(contains_generic)
                || constraint_bounds_contains_generic(constraints)
        }
        Ty::ClosureInstance {
            ret,
            params,
            captures,
            ..
        } => {
            contains_generic(ret)
                || params.iter().any(contains_generic)
                || captures.iter().any(contains_generic)
        }
        _ => false,
    }
}

fn constraint_bounds_contains_generic(bounds: &ConstraintBounds) -> bool {
    bounds
        .positive
        .iter()
        .chain(bounds.negative.iter())
        .any(|entry| entry.args.iter().any(contains_generic))
}

fn constraint_bounds_complexity(bounds: &ConstraintBounds) -> usize {
    bounds
        .positive
        .iter()
        .chain(bounds.negative.iter())
        .map(|entry| 1 + entry.args.iter().map(type_complexity).sum::<usize>())
        .sum()
}

fn constraint_bounds_contains_ty(bounds: &ConstraintBounds, needle: &Ty) -> bool {
    bounds
        .positive
        .iter()
        .chain(bounds.negative.iter())
        .any(|entry| entry.args.iter().any(|arg| ty_contains(arg, needle)))
}

fn retained_closure_capabilities(ty: &Ty) -> Vec<ConstraintRef> {
    match ty.unqualified() {
        Ty::Closure { constraints, .. } => constraints.positive.clone(),
        _ => Vec::new(),
    }
}

fn retained_closure_has_capability(ty: &Ty, capability: &ConstraintRef) -> bool {
    let Ty::Closure { constraints, .. } = ty.unqualified() else {
        return false;
    };
    constraints.positive.iter().any(|entry| entry == capability)
}

fn retained_closure_interface_subst(
    interface: &CheckedInterface,
    receiver_ty: &Ty,
    args: &[Ty],
) -> HashMap<String, Ty> {
    let mut subst = HashMap::new();
    if let Some(receiver) = interface.generics.first() {
        subst.insert(receiver.clone(), receiver_ty.clone());
    }
    for (generic, arg) in interface.generics.iter().skip(1).zip(args.iter()) {
        subst.insert(generic.clone(), arg.clone());
    }
    subst
}

fn substitute_ty(ty: &Ty, subst: &HashMap<String, Ty>) -> Ty {
    match ty {
        Ty::Hole(id) => Ty::Hole(*id),
        Ty::Const(inner) => Ty::Const(Box::new(substitute_ty(inner, subst))),
        Ty::Generic(name) => subst
            .get(name)
            .cloned()
            .unwrap_or_else(|| Ty::Generic(name.clone())),
        Ty::Pointer { nullable, inner } => Ty::Pointer {
            nullable: *nullable,
            inner: Box::new(substitute_ty(inner, subst)),
        },
        Ty::Array { len, elem } => Ty::Array {
            len: *len,
            elem: Box::new(substitute_ty(elem, subst)),
        },
        Ty::Slice(elem) => Ty::Slice(Box::new(substitute_ty(elem, subst))),
        Ty::Named { name, args } => Ty::Named {
            name: name.clone(),
            args: args.iter().map(|arg| substitute_ty(arg, subst)).collect(),
        },
        Ty::DynamicInterface { name, args } => Ty::DynamicInterface {
            name: name.clone(),
            args: args.iter().map(|arg| substitute_ty(arg, subst)).collect(),
        },
        Ty::Function { abi, ret, params } => Ty::Function {
            abi: abi.clone(),
            ret: Box::new(substitute_ty(ret, subst)),
            params: params
                .iter()
                .map(|param| substitute_ty(param, subst))
                .collect(),
        },
        Ty::Closure {
            ret,
            params,
            constraints,
        } => Ty::Closure {
            ret: Box::new(substitute_ty(ret, subst)),
            params: params
                .iter()
                .map(|param| substitute_ty(param, subst))
                .collect(),
            constraints: substitute_constraint_bounds(constraints, subst),
        },
        Ty::ClosureInstance {
            id,
            ret,
            params,
            captures,
        } => Ty::ClosureInstance {
            id: *id,
            ret: Box::new(substitute_ty(ret, subst)),
            params: params
                .iter()
                .map(|param| substitute_ty(param, subst))
                .collect(),
            captures: captures
                .iter()
                .map(|capture| substitute_ty(capture, subst))
                .collect(),
        },
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
        | Ty::Unknown => ty.clone(),
    }
}

fn substitute_constraint_bounds(
    bounds: &ConstraintBounds,
    subst: &HashMap<String, Ty>,
) -> ConstraintBounds {
    ConstraintBounds {
        positive: bounds
            .positive
            .iter()
            .map(|entry| substitute_constraint_ref(entry, subst))
            .collect(),
        negative: bounds
            .negative
            .iter()
            .map(|entry| substitute_constraint_ref(entry, subst))
            .collect(),
    }
}

fn substitute_constraint_ref(entry: &ConstraintRef, subst: &HashMap<String, Ty>) -> ConstraintRef {
    ConstraintRef {
        name: entry.name.clone(),
        args: entry
            .args
            .iter()
            .map(|arg| substitute_ty(arg, subst))
            .collect(),
    }
}

fn is_clone_message_capability(capability: &ConstraintRef) -> bool {
    capability.name == "clone_message" && capability.args.is_empty()
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
        Ty::Closure {
            ret,
            params,
            constraints,
        } => {
            1 + type_complexity(ret)
                + params.iter().map(type_complexity).sum::<usize>()
                + constraint_bounds_complexity(constraints)
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
        Ty::Hole(_)
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
        Ty::Closure {
            ret,
            params,
            constraints,
        } => {
            ty_contains(ret, needle)
                || params.iter().any(|param| ty_contains(param, needle))
                || constraint_bounds_contains_ty(constraints, needle)
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
        Ty::Hole(_)
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

fn name_ref_canonical(resolved: &ResolvedProgram, name: &NameRef) -> String {
    match name.kind {
        NameRefKind::Def(def_id) => resolved.def(def_id).name.clone(),
        _ => name.display.clone(),
    }
}

fn mangle_ty_fragment(ty: &Ty) -> String {
    match ty {
        Ty::Hole(_) => "hole".to_string(),
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
        Ty::Closure {
            ret,
            params,
            constraints,
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
            format!(
                "closure_ret_{}_args_{}_caps_{}",
                mangle_ty_fragment(ret),
                params,
                mangle_constraint_bounds(constraints)
            )
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

fn mangle_constraint_bounds(bounds: &ConstraintBounds) -> String {
    if bounds.is_empty() {
        return "none".to_string();
    }
    let mut parts = bounds
        .positive
        .iter()
        .map(|entry| format!("pos_{}", mangle_constraint_ref(entry)))
        .collect::<Vec<_>>();
    parts.extend(
        bounds
            .negative
            .iter()
            .map(|entry| format!("neg_{}", mangle_constraint_ref(entry))),
    );
    parts.join("_")
}

fn mangle_constraint_ref(entry: &ConstraintRef) -> String {
    if entry.args.is_empty() {
        sanitize_mangle_fragment(&entry.name)
    } else {
        format!(
            "{}_{}",
            sanitize_mangle_fragment(&entry.name),
            entry
                .args
                .iter()
                .map(mangle_ty_fragment)
                .collect::<Vec<_>>()
                .join("_")
        )
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
