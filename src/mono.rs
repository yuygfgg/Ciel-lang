use std::collections::{HashMap, HashSet, VecDeque};

use crate::{
    diagnostic::{DiagResult, Diagnostic},
    hir::{
        ConstraintExpr, FieldDecl, ItemKind, NameRef, NameRefKind, Type, TypeAliasTarget, TypeKind,
        TypeNameKind, VariantDecl,
    },
    interfaces::{
        checked_interface_view, constraint_interface_view, impl_matches_dynamic_interface,
        impl_matches_interface_receiver, retained_closure_interface_signature,
    },
    layout::check_checked_aggregate_layouts,
    resolve::{DefId, DefKind, ResolvedProgram},
    retained::{
        retained_closure_has_clone_message_capability, retained_closure_missing_capabilities,
    },
    std_id,
    thir::{
        CheckedEnum, CheckedFunction, CheckedGenericFunction, CheckedImpl, CheckedInterfaceRef,
        CheckedProgram, CheckedStruct, CheckedVariant, TBlock, TCase, TExpr, TExprKind, TForInit,
        TPattern, TStmt, TStmtKind,
    },
    typeck::{CheckedGenericInstance, type_check_generic_instance},
    types::{
        ConstraintBounds, ConstraintRef, STD_ERROR_FORMAT_INTERFACE, STD_MESSAGE_CLONE_INTERFACE,
        STD_MESSAGE_SHARE_HANDLE_INTERFACE, Ty, aggregate_instance_name, contains_generic,
        is_clone_message_capability, mangle_ty_fragment, meta_array_split_len, meta_named,
        meta_product_ty, meta_ref_array_repr_ty, meta_repr_borrowed_array_leaf_ty,
        meta_repr_marker_name, meta_sum_ty, retained_closure_capabilities, std_error_code_ty,
        std_error_trait_ty, std_message_result_ty, std_meta_repr_marker_ty,
        std_meta_repr_source_name, ty_contains, ty_from_primitive, type_complexity, unify_ty,
    },
};

#[derive(Clone, Debug)]
pub struct MonoProgram {
    pub checked: CheckedProgram,
}

pub fn monomorphize(checked: CheckedProgram) -> DiagResult<MonoProgram> {
    MonoContext::new(checked).run()
}

fn is_nominal_type_def_kind(kind: &DefKind) -> bool {
    matches!(kind, DefKind::Struct | DefKind::Enum)
}

fn nominal_type_name(resolved: &ResolvedProgram, def_id: DefId) -> String {
    let def = resolved.def(def_id);
    if !is_nominal_type_def_kind(&def.kind) {
        return def.name.clone();
    }
    let has_same_named_nominal = resolved.defs.iter().any(|other| {
        other.id != def.id && other.name == def.name && is_nominal_type_def_kind(&other.kind)
    });
    if has_same_named_nominal {
        format!("{}__def{}", def.name, def.id.0)
    } else {
        def.name.clone()
    }
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
            TPattern::Binding {
                local_id,
                name,
                mutability,
                ty,
            } => TPattern::Binding {
                local_id,
                name,
                mutability,
                ty,
            },
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
            TExprKind::UnsafeBlock { statements, value } => TExprKind::UnsafeBlock {
                statements: statements
                    .into_iter()
                    .map(|stmt| self.rewrite_stmt(stmt))
                    .collect::<DiagResult<Vec<_>>>()?,
                value: value
                    .map(|expr| self.rewrite_expr(*expr).map(Box::new))
                    .transpose()?,
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
            TExprKind::SliceToConst(inner) => {
                TExprKind::SliceToConst(Box::new(self.rewrite_expr(*inner)?))
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
            TExprKind::Try {
                expr: inner,
                propagation,
            } => {
                let inner = self.rewrite_expr(*inner)?;
                if matches!(propagation, crate::thir::TryPropagation::ErrorBox)
                    && let Some((_, err_ty)) = result_args(&inner.ty)
                {
                    self.mark_dynamic_impls(&std_error_trait_ty(), &err_ty);
                }
                TExprKind::Try {
                    expr: Box::new(inner),
                    propagation,
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
                handle_message_ty,
                message_ty,
                handler_ty,
            } => {
                self.mark_standard_error_code_impl();
                self.mark_message_clone_impls(&state_ty);
                self.mark_message_clone_impls(&handler_ty);
                TExprKind::ActorSpawn {
                    initial_state: Box::new(self.rewrite_expr(*initial_state)?),
                    handler: Box::new(self.rewrite_expr(*handler)?),
                    state_ty,
                    handle_message_ty,
                    message_ty,
                    handler_ty,
                }
            }
            TExprKind::ActorSend {
                actor,
                value,
                message_ty,
            } => {
                self.mark_standard_error_code_impl();
                self.mark_message_clone_impls(&message_ty);
                TExprKind::ActorSend {
                    actor: Box::new(self.rewrite_expr(*actor)?),
                    value: Box::new(self.rewrite_expr(*value)?),
                    message_ty,
                }
            }
            TExprKind::ActorStop { actor, message_ty } => {
                self.mark_standard_error_code_impl();
                TExprKind::ActorStop {
                    actor: Box::new(self.rewrite_expr(*actor)?),
                    message_ty,
                }
            }
            TExprKind::ActorJoin { actor, message_ty } => {
                self.mark_standard_error_code_impl();
                TExprKind::ActorJoin {
                    actor: Box::new(self.rewrite_expr(*actor)?),
                    message_ty,
                }
            }
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
        for interface in self.dynamic_view_interfaces(name, args) {
            let function_def = self
                .checked
                .impls
                .iter()
                .find(|implementation| {
                    impl_matches_dynamic_interface(implementation, &interface, concrete_ty)
                })
                .map(|implementation| implementation.function_def);
            if let Some(function_def) = function_def {
                self.mark_function(function_def);
            }
        }
    }

    fn mark_standard_error_code_impl(&mut self) {
        let code_ty = std_error_code_ty();
        let function_def = self
            .checked
            .impls
            .iter()
            .find(|implementation| {
                impl_matches_interface_receiver(
                    implementation,
                    STD_ERROR_FORMAT_INTERFACE,
                    &[],
                    &code_ty,
                )
            })
            .map(|implementation| implementation.function_def);
        if let Some(function_def) = function_def {
            self.mark_function(function_def);
        }
    }

    fn dynamic_view_interfaces(&self, name: &str, args: &[Ty]) -> Vec<CheckedInterfaceRef> {
        checked_interface_view(
            &self.checked.interfaces,
            &self.checked.interface_aliases,
            name,
            args,
        )
    }

    fn mark_message_clone_impls(&mut self, ty: &Ty) {
        if let Some(function_def) = self.clone_message_impl_def(ty) {
            self.mark_function(function_def);
        }
    }

    fn mark_retained_closure_witness_impls(&mut self, target_ty: &Ty, source_ty: &Ty) {
        for capability in retained_closure_missing_capabilities(target_ty, source_ty) {
            if is_clone_message_capability(&capability) {
                self.mark_message_clone_impls(source_ty);
                continue;
            }
            if let Some(function_def) = self
                .checked
                .impls
                .iter()
                .find(|implementation| {
                    impl_matches_interface_receiver(
                        implementation,
                        &capability.name,
                        &capability.args,
                        source_ty,
                    )
                })
                .map(|implementation| implementation.function_def)
            {
                self.mark_function(function_def);
            }
        }
    }

    fn clone_message_impl_def(&self, ty: &Ty) -> Option<DefId> {
        let ty = ty;
        self.checked
            .impls
            .iter()
            .find(|implementation| {
                implementation.interface_name == STD_MESSAGE_CLONE_INTERFACE
                    && implementation
                        .receiver_ty
                        .as_ref()
                        .is_some_and(|receiver| receiver == ty)
                    && implementation.interface_args.get(1..) == Some(&[][..])
            })
            .map(|implementation| implementation.function_def)
    }
}

fn result_args(ty: &Ty) -> Option<(&Ty, &Ty)> {
    let Ty::Named { name, args } = ty else {
        return None;
    };
    if name == "Result" && args.len() == 2 {
        Some((&args[0], &args[1]))
    } else {
        None
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
    share_handle_templates: Vec<Ty>,
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
                        let Some(def_id) = checked.resolved.local_def(
                            module.id,
                            &decl.name.name,
                            &[DefKind::Struct],
                        ) else {
                            continue;
                        };
                        structs.insert(
                            nominal_type_name(&checked.resolved, def_id),
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
                        let Some(def_id) = checked.resolved.local_def(
                            module.id,
                            &decl.name.name,
                            &[DefKind::Enum],
                        ) else {
                            continue;
                        };
                        enums.insert(
                            nominal_type_name(&checked.resolved, def_id),
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
            share_handle_templates: checked.share_handle_templates.clone(),
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
        self.diagnostics.extend(check_checked_aggregate_layouts(
            &self.checked_structs,
            &self.checked_enums,
        ));
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
            TExprKind::Unary { expr, .. } | TExprKind::Cast { expr, .. } => self.collect_expr(expr),
            TExprKind::Try { expr, propagation } => {
                self.collect_expr(expr);
                if matches!(propagation, crate::thir::TryPropagation::ErrorBox) {
                    self.collect_ty(&std_error_trait_ty());
                    if let Some((_, err_ty)) = result_args(&expr.ty) {
                        self.collect_ty(err_ty);
                    }
                }
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
            TExprKind::UnsafeBlock { statements, value } => {
                for stmt in statements {
                    self.collect_stmt(stmt);
                }
                if let Some(value) = value {
                    self.collect_expr(value);
                }
            }
            TExprKind::Closure { body, .. } => self.collect_closure_body(body),
            TExprKind::FunctionToClosure(inner) => {
                self.collect_expr(inner);
                self.collect_retained_closure_witness_tys(&expr.ty, &inner.ty);
                if retained_closure_has_clone_message_capability(&expr.ty) {
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
                if retained_closure_has_clone_message_capability(&expr.ty) {
                    self.collect_message_clone_result_tys(&expr.ty);
                    self.collect_message_clone_result_tys(source_ty);
                }
            }
            TExprKind::ArrayToSlice(inner) | TExprKind::SliceToConst(inner) => {
                self.collect_expr(inner)
            }
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
                handle_message_ty,
                message_ty,
                handler_ty,
            } => {
                self.collect_expr(initial_state);
                self.collect_expr(handler);
                self.collect_ty(state_ty);
                self.collect_ty(handle_message_ty);
                self.collect_ty(message_ty);
                self.collect_ty(handler_ty);
                self.collect_ty(&std_error_code_ty());
                self.collect_ty(&std_message_result_ty(state_ty.clone()));
                self.collect_ty(&std_message_result_ty(handler_ty.clone()));
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
                self.collect_ty(&std_error_code_ty());
                self.collect_ty(&std_message_result_ty(message_ty.clone()));
                self.collect_message_clone_result_tys(message_ty);
            }
            TExprKind::ActorStop { actor, message_ty }
            | TExprKind::ActorJoin { actor, message_ty } => {
                self.collect_expr(actor);
                self.collect_ty(message_ty);
                self.collect_ty(&std_error_code_ty());
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
        self.collect_ty(&std_message_result_ty(ty.clone()));
    }

    fn collect_ty(&mut self, ty: &Ty) {
        match ty {
            Ty::Named { name, args } => {
                if let Some(borrowed) = meta_repr_marker_name(name)
                    && args.len() == 1
                    && !contains_generic(&args[0])
                {
                    let storage_ty = self.meta_repr_ty(None, &args[0], borrowed);
                    self.collect_ty(&storage_ty);
                    return;
                }
                let ty = Ty::Named {
                    name: name.clone(),
                    args: args.clone(),
                };
                if self.is_owned_meta_policy_leaf(&ty, None) {
                    if self.structs.contains_key(name) {
                        self.instantiate_struct(name, args);
                    }
                    if self.enums.contains_key(name) {
                        self.instantiate_enum(name, args);
                    }
                    return;
                }
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
            Ty::Array { elem, .. } | Ty::Slice { elem, .. } => self.collect_ty(elem),
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
        let key = (receiver_ty.clone(), capability.clone());
        if !self.retained_closure_interface_tys.insert(key) {
            return;
        }
        for arg in &capability.args {
            self.collect_ty(arg);
        }
        let Some(signature) =
            retained_closure_interface_signature(&self.checked.interfaces, receiver_ty, capability)
        else {
            return;
        };
        self.collect_ty(&signature.ret);
        for param in signature.params.iter().skip(1) {
            self.collect_ty(param);
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
                let ty = self.lower_ast_type_preserving_meta_repr_markers(&field.ty, &subst);
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
                        let ty = self.lower_ast_type_preserving_meta_repr_markers(payload, &subst);
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

    fn lower_ast_type_preserving_meta_repr_markers(
        &mut self,
        ty: &Type,
        subst: &HashMap<String, Ty>,
    ) -> Ty {
        self.lower_ast_type_inner(ty, subst, true)
    }

    fn normalize_meta_repr_markers(&mut self, ty: &Ty) -> Ty {
        self.normalize_meta_repr_markers_inner(ty, false)
    }

    fn normalize_meta_repr_markers_preserving_markers(&mut self, ty: &Ty) -> Ty {
        self.normalize_meta_repr_markers_inner(ty, true)
    }

    fn normalize_meta_repr_markers_inner(&mut self, ty: &Ty, preserve_markers: bool) -> Ty {
        match ty {
            Ty::Pointer {
                nullable,
                mutability,
                inner,
            } => Ty::Pointer {
                nullable: *nullable,
                mutability: *mutability,
                inner: Box::new(self.normalize_meta_repr_markers_inner(inner, preserve_markers)),
            },
            Ty::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(self.normalize_meta_repr_markers_inner(elem, preserve_markers)),
            },
            Ty::Slice { mutability, elem } => Ty::Slice {
                mutability: *mutability,
                elem: Box::new(self.normalize_meta_repr_markers_inner(elem, preserve_markers)),
            },
            Ty::Named { name, args } => {
                if let Some(borrowed) = meta_repr_marker_name(name) {
                    if preserve_markers {
                        return Ty::Named {
                            name: name.clone(),
                            args: args.clone(),
                        };
                    }
                    let args = args
                        .iter()
                        .map(|arg| self.normalize_meta_repr_markers_inner(arg, preserve_markers))
                        .collect::<Vec<_>>();
                    if args.len() == 1 && !contains_generic(&args[0]) {
                        return self.meta_repr_ty(None, &args[0], borrowed);
                    }
                    return Ty::Named {
                        name: name.clone(),
                        args,
                    };
                }
                let original = Ty::Named {
                    name: name.clone(),
                    args: args.clone(),
                };
                if self.is_owned_meta_policy_leaf(&original, None) {
                    return original;
                }
                let args = args
                    .iter()
                    .map(|arg| self.normalize_meta_repr_markers_inner(arg, preserve_markers))
                    .collect::<Vec<_>>();
                Ty::Named {
                    name: name.clone(),
                    args,
                }
            }
            Ty::DynamicInterface { name, args } => Ty::DynamicInterface {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.normalize_meta_repr_markers_inner(arg, preserve_markers))
                    .collect(),
            },
            Ty::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => Ty::Function {
                is_unsafe: *is_unsafe,
                abi: abi.clone(),
                ret: Box::new(self.normalize_meta_repr_markers_inner(ret, preserve_markers)),
                params: params
                    .iter()
                    .map(|param| self.normalize_meta_repr_markers_inner(param, preserve_markers))
                    .collect(),
            },
            Ty::Closure {
                ret,
                params,
                constraints,
            } => Ty::Closure {
                ret: Box::new(self.normalize_meta_repr_markers_inner(ret, preserve_markers)),
                params: params
                    .iter()
                    .map(|param| self.normalize_meta_repr_markers_inner(param, preserve_markers))
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
                ret: Box::new(self.normalize_meta_repr_markers_inner(ret, preserve_markers)),
                params: params
                    .iter()
                    .map(|param| self.normalize_meta_repr_markers_inner(param, preserve_markers))
                    .collect(),
                captures: captures
                    .iter()
                    .map(|capture| {
                        self.normalize_meta_repr_markers_inner(capture, preserve_markers)
                    })
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
        let span = span.into();
        let root = (!borrowed).then(|| source_ty.clone());
        let mut expanding = HashSet::new();
        self.meta_repr_ty_rec(span, source_ty, borrowed, root.as_ref(), &mut expanding)
    }

    fn meta_repr_ty_rec(
        &mut self,
        span: Option<crate::span::Span>,
        source_ty: &Ty,
        borrowed: bool,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Ty {
        if contains_generic(source_ty) {
            return std_meta_repr_marker_ty(borrowed, source_ty.clone());
        }
        match source_ty {
            Ty::Array { .. } => {
                if borrowed {
                    let Ty::Array { len, elem } = source_ty else {
                        unreachable!();
                    };
                    meta_ref_array_repr_ty(*len, elem)
                } else {
                    let Ty::Array { len, elem } = source_ty else {
                        unreachable!();
                    };
                    self.meta_array_repr_ty_rec(*len, elem, false, root, expanding)
                }
            }
            Ty::Named { name, args } => {
                let instance_ty = Ty::Named {
                    name: name.clone(),
                    args: args.clone(),
                };
                if !expanding.insert(instance_ty.clone()) {
                    self.diagnostics.push(Diagnostic::new(
                        span,
                        format!(
                            "meta structural representation is recursive through `{source_ty}`"
                        ),
                    ));
                    return Ty::Unknown;
                }
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
                    let fields = fields.into_iter().map(|(_, ty)| {
                        self.meta_repr_field_ty(span, &ty, borrowed, root, expanding)
                    });
                    let ty = meta_product_ty(fields, if borrowed { "FieldRef" } else { "Field" });
                    expanding.remove(&instance_ty);
                    return ty;
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
                    let ty = meta_sum_ty(
                        enm.variants.iter().map(|variant| {
                            variant
                                .payload
                                .iter()
                                .map(|payload| {
                                    self.meta_repr_field_ty(
                                        span, payload, borrowed, root, expanding,
                                    )
                                })
                                .collect::<Vec<_>>()
                        }),
                        borrowed,
                    );
                    expanding.remove(&instance_ty);
                    return ty;
                }
                expanding.remove(&instance_ty);
                self.push_meta_unsupported_repr(span, source_ty);
                Ty::Unknown
            }
            Ty::ClosureInstance { captures, .. } => meta_product_ty(
                captures
                    .iter()
                    .filter(|ty| !ty.is_erased_value())
                    .map(|ty| self.meta_repr_field_ty(span, ty, borrowed, root, expanding)),
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

    fn meta_repr_field_ty(
        &mut self,
        span: Option<crate::span::Span>,
        ty: &Ty,
        borrowed: bool,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Ty {
        if borrowed {
            return meta_repr_borrowed_array_leaf_ty(ty);
        }
        self.meta_repr_owned_leaf_ty_rec(span, ty, root, expanding)
    }

    fn meta_repr_policy_leaf_ty(&mut self, ty: &Ty) -> Ty {
        match ty {
            Ty::Named { name, args } => Ty::Named {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.normalize_meta_repr_markers_preserving_markers(arg))
                    .collect(),
            },
            _ => ty.clone(),
        }
    }

    fn meta_repr_owned_leaf_ty_rec(
        &mut self,
        span: Option<crate::span::Span>,
        ty: &Ty,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Ty {
        if self.is_owned_meta_policy_leaf(ty, root) {
            return self.meta_repr_policy_leaf_ty(ty);
        }
        match ty {
            Ty::Array { len, elem } => {
                self.meta_array_repr_ty_rec(*len, elem, false, root, expanding)
            }
            Ty::Named { .. } | Ty::ClosureInstance { .. } => {
                self.meta_repr_ty_rec(span, ty, false, root, expanding)
            }
            other => other.clone(),
        }
    }

    fn is_owned_meta_policy_leaf(&mut self, ty: &Ty, root: Option<&Ty>) -> bool {
        if root.is_some_and(|root| ty == root) || contains_generic(ty) {
            return false;
        }
        let leaf_ty = self.meta_repr_policy_leaf_ty(ty);
        matches!(ty, Ty::Named { .. })
            && (self.share_handle_templates.iter().any(|pattern| {
                let mut subst = HashMap::new();
                unify_ty(pattern, &leaf_ty, &mut subst)
            }) || self.checked.impls.iter().any(|implementation| {
                implementation.interface_name == STD_MESSAGE_SHARE_HANDLE_INTERFACE
                    && implementation
                        .receiver_ty
                        .as_ref()
                        .is_some_and(|receiver| receiver == &leaf_ty)
                    && implementation.interface_args.get(1..) == Some(&[][..])
            }) || self.checked.impls.iter().any(|implementation| {
                implementation.interface_name == STD_MESSAGE_CLONE_INTERFACE
                    && implementation
                        .receiver_ty
                        .as_ref()
                        .is_some_and(|receiver| receiver == &leaf_ty)
                    && implementation.interface_args.get(1..) == Some(&[][..])
            }))
    }

    fn meta_array_repr_ty_rec(
        &mut self,
        len: usize,
        elem: &Ty,
        borrowed: bool,
        root: Option<&Ty>,
        expanding: &mut HashSet<Ty>,
    ) -> Ty {
        if len == 0 {
            return meta_named("ArrayNil", Vec::new());
        }
        if len <= crate::types::META_ARRAY_CHUNK_SIZE {
            let elem_ty = if borrowed {
                meta_repr_borrowed_array_leaf_ty(elem)
            } else {
                self.meta_repr_owned_leaf_ty_rec(None, elem, root, expanding)
            };
            return meta_named(&format!("ArrayChunk{len}"), vec![elem_ty]);
        }
        let split = meta_array_split_len(len);
        meta_named(
            "ArrayCat",
            vec![
                self.meta_array_repr_ty_rec(split, elem, borrowed, root, expanding),
                self.meta_array_repr_ty_rec(len - split, elem, borrowed, root, expanding),
            ],
        )
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
        if std_id::is_std_meta_type(&self.checked.resolved, def_id, name) {
            Some(borrowed)
        } else {
            None
        }
    }

    fn lower_ast_type(&mut self, ty: &Type, subst: &HashMap<String, Ty>) -> Ty {
        self.lower_ast_type_inner(ty, subst, false)
    }

    fn lower_ast_type_inner(
        &mut self,
        ty: &Type,
        subst: &HashMap<String, Ty>,
        preserve_meta_repr_markers: bool,
    ) -> Ty {
        match &ty.kind {
            TypeKind::Hole => Ty::Unknown,
            TypeKind::Never => Ty::Never,
            TypeKind::Void => Ty::Void,
            TypeKind::Primitive(primitive) => ty_from_primitive(primitive),
            TypeKind::Named(type_name, args) => {
                let (name, def_kind, def_id) = match &type_name.kind {
                    TypeNameKind::Def(def_id) => {
                        let def = self.checked.resolved.def(*def_id);
                        (
                            nominal_type_name(&self.checked.resolved, *def_id),
                            Some(def.kind.clone()),
                            Some(*def_id),
                        )
                    }
                    TypeNameKind::Generic(generic) => (generic.clone(), None, None),
                    TypeNameKind::Error => return Ty::Unknown,
                };
                if args.is_empty()
                    && let Some(replacement) = subst.get(&name)
                {
                    return if preserve_meta_repr_markers {
                        self.normalize_meta_repr_markers_preserving_markers(replacement)
                    } else {
                        self.normalize_meta_repr_markers(replacement)
                    };
                }
                let args = args
                    .iter()
                    .map(|arg| self.lower_ast_type_inner(arg, subst, preserve_meta_repr_markers))
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
                    if preserve_meta_repr_markers || contains_generic(&args[0]) {
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
                        TypeAliasTarget::Type(alias_ty) => self.lower_ast_type_inner(
                            alias_ty,
                            &alias_subst,
                            preserve_meta_repr_markers,
                        ),
                        TypeAliasTarget::CSpelling { abi, spelling } => Ty::CSpelling {
                            abi: abi.clone(),
                            spelling: spelling.clone(),
                        },
                    };
                    self.alias_stack.pop();
                    return if preserve_meta_repr_markers {
                        self.normalize_meta_repr_markers_preserving_markers(&lowered)
                    } else {
                        self.normalize_meta_repr_markers(&lowered)
                    };
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
                    let ty = Ty::Named { name, args };
                    if preserve_meta_repr_markers {
                        self.normalize_meta_repr_markers_preserving_markers(&ty)
                    } else {
                        self.normalize_meta_repr_markers(&ty)
                    }
                }
            }
            TypeKind::Pointer {
                nullable,
                mutability,
                inner,
            } => Ty::Pointer {
                nullable: *nullable,
                mutability: *mutability,
                inner: Box::new(self.lower_ast_type_inner(
                    inner,
                    subst,
                    preserve_meta_repr_markers,
                )),
            },
            TypeKind::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(self.lower_ast_type_inner(elem, subst, preserve_meta_repr_markers)),
            },
            TypeKind::Slice { mutability, elem } => Ty::Slice {
                mutability: *mutability,
                elem: Box::new(self.lower_ast_type_inner(elem, subst, preserve_meta_repr_markers)),
            },
            TypeKind::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => Ty::Function {
                is_unsafe: *is_unsafe,
                abi: abi.clone(),
                ret: Box::new(self.lower_ast_type_inner(ret, subst, preserve_meta_repr_markers)),
                params: params
                    .iter()
                    .map(|param| {
                        self.lower_ast_type_inner(param, subst, preserve_meta_repr_markers)
                    })
                    .collect(),
            },
            TypeKind::Closure {
                ret,
                params,
                constraint,
            } => Ty::Closure {
                ret: Box::new(self.lower_ast_type_inner(ret, subst, preserve_meta_repr_markers)),
                params: params
                    .iter()
                    .map(|param| {
                        self.lower_ast_type_inner(param, subst, preserve_meta_repr_markers)
                    })
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
            let view = constraint_interface_view(
                &self.checked.interface_aliases,
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

fn enum_c_name_from_ty(ty: &Ty) -> Option<String> {
    match ty {
        Ty::Named { name, args } => Some(aggregate_instance_name(name, args)),
        _ => None,
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

fn name_ref_canonical(resolved: &ResolvedProgram, name: &NameRef) -> String {
    match name.kind {
        NameRefKind::Def(def_id) => resolved.def(def_id).name.clone(),
        _ => name.display.clone(),
    }
}
