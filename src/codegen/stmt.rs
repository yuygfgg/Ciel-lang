use super::*;

impl<'a> CGenerator<'a> {
    pub(super) fn gen_function(&mut self, function: &CheckedFunction) -> DiagResult<()> {
        let Some(body) = &function.body else {
            return Ok(());
        };
        if function.is_async {
            return self.gen_async_function(function, body);
        }
        self.emit_line_directive(body.span);
        self.line(&format!("{} {{", self.function_decl(function, false)));
        self.defer_stack.clear();
        self.temporary_resource_cleanup_depth = 0;
        self.temporary_resource_cleanup_frames.clear();
        self.loop_defer_starts.clear();
        self.break_defer_starts.clear();
        self.current_return_ty = function.ret.clone();
        self.current_heap_locals = self
            .escapes
            .functions
            .get(&function.def_id)
            .map(|escape| escape.heap_locals.clone())
            .unwrap_or_default();
        self.current_param_locals = function
            .params
            .iter()
            .filter_map(|(local_id, name, _)| local_id.map(|id| (id, name.clone())))
            .collect();
        self.current_owned_resource_roots = function
            .params
            .iter()
            .filter_map(|(local_id, name, ty)| {
                if self.type_is_affine(ty) {
                    local_id.map(|id| (ty.clone(), self.local_value_expr(id, name)))
                } else {
                    None
                }
            })
            .collect();
        self.current_closure_owner = Some(function.def_id);
        let falls_through = self.gen_block_inner(body, 1)?;
        if falls_through && function.ret.is_never() {
            self.line_indent(1, "ciel_panic(NULL, 0);");
        } else if falls_through && !function.ret.is_erased_value() {
            self.line_indent(1, "ciel_panic(NULL, 0);");
            self.line_indent(
                1,
                &format!("return {};", self.zero_return_value(&function.ret)),
            );
        }
        self.current_heap_locals.clear();
        self.current_param_locals.clear();
        self.current_owned_resource_roots.clear();
        self.current_closure_owner = None;
        self.current_return_ty = Ty::Void;
        self.line("}");
        Ok(())
    }

    pub(super) fn gen_async_function(
        &mut self,
        function: &CheckedFunction,
        body: &TBlock,
    ) -> DiagResult<()> {
        let names = self.async_function_names(function.def_id);
        let future_ty = self.function_call_return_ty(function);
        self.emit_line_directive(body.span);
        self.line(&format!("{} {{", self.function_decl(function, false)));
        let ctx = self.next_temp("async_ctx");
        self.line_indent(
            1,
            &format!(
                "{} *{ctx} = ({} *)ciel_alloc(sizeof({}));",
                names.context, names.context, names.context
            ),
        );
        self.line_indent(1, &format!("memset({ctx}, 0, sizeof(*{ctx}));"));
        self.line_indent(1, &format!("{ctx}->pc = 0;"));
        self.line_indent(1, &format!("{ctx}->cleanup_state = 0;"));
        self.line_indent(1, &format!("{ctx}->future = NULL;"));
        self.line_indent(1, &format!("{ctx}->active_future = NULL;"));
        for (idx, (_, name, ty)) in function
            .params
            .iter()
            .filter(|(_, _, ty)| !ty.is_erased_value())
            .enumerate()
        {
            self.emit_value_copy(&format!("{ctx}->arg{idx}"), name, ty, 1);
        }
        let raw = self.next_temp("async_future");
        let (size_expr, align_expr) = self.future_result_layout_args(&function.ret);
        self.line_indent(
            1,
            &format!(
                "CielFuture *{raw} = ciel_future_new({size_expr}, {align_expr}, {}, {ctx}, {});",
                names.run, names.cleanup
            ),
        );
        let (file, line) = self.location_args(body.span);
        self.line_indent(1, &format!("if ({raw} == NULL) {{"));
        self.line_indent(
            2,
            &format!(
                "ciel_panic_at(\"future allocation failed\", sizeof(\"future allocation failed\") - 1, {file}, {line});"
            ),
        );
        self.line_indent(1, "}");
        self.line_indent(1, &format!("{ctx}->future = {raw};"));
        self.line_indent(
            1,
            &format!(
                "return ({}){{ .handle = (void *){raw} }};",
                self.c_type(&future_ty)
            ),
        );
        self.line("}");

        self.line(&format!(
            "static int32_t {}(void *ctx_raw, void *out_raw) {{",
            names.run
        ));
        self.defer_stack.clear();
        self.temporary_resource_cleanup_depth = 0;
        self.temporary_resource_cleanup_frames.clear();
        self.loop_defer_starts.clear();
        self.break_defer_starts.clear();
        self.current_return_ty = function.ret.clone();
        self.current_async_output = Some("out_raw".to_string());
        self.current_async_context = Some("ctx".to_string());
        self.current_async_await_index = 0;
        self.current_async_frame_locals = self
            .async_frame_locals_with_escape_info_for_function(function)
            .into_iter()
            .map(|local| (local.id, format!("ctx->{}", local.field)))
            .collect();
        self.current_async_await_outputs = self
            .async_facts_for_function(function)
            .await_output_tys
            .iter()
            .cloned()
            .into_iter()
            .enumerate()
            .map(|(idx, ty)| {
                if ty.is_erased_value() {
                    None
                } else {
                    Some((format!("ctx->await_out{}", idx + 1), ty))
                }
            })
            .collect();
        self.current_async_defer_arg_index = 0;
        self.current_async_cleanup_cases = vec![Vec::new(); self.current_async_await_outputs.len()];
        self.current_heap_locals = self
            .escapes
            .functions
            .get(&function.def_id)
            .map(|escape| escape.heap_locals.clone())
            .unwrap_or_default();
        self.current_param_locals = function
            .params
            .iter()
            .filter(|(_, _, ty)| !ty.is_erased_value())
            .enumerate()
            .filter_map(|(idx, (local_id, _, _))| local_id.map(|id| (id, format!("ctx->arg{idx}"))))
            .collect();
        self.current_owned_resource_roots = function
            .params
            .iter()
            .filter(|(_, _, ty)| !ty.is_erased_value())
            .enumerate()
            .filter_map(|(idx, (local_id, _, ty))| {
                if self.type_is_affine(ty) {
                    local_id.map(|_| (ty.clone(), format!("ctx->arg{idx}")))
                } else {
                    None
                }
            })
            .collect();
        self.current_closure_owner = Some(function.def_id);
        self.line_indent(
            1,
            &format!("{} *ctx = ({} *)ctx_raw;", names.context, names.context),
        );
        if !self.current_async_await_outputs.is_empty() {
            self.line_indent(1, "switch (ctx->pc) {");
            self.line_indent(2, "case 0: break;");
            for idx in 1..=self.current_async_await_outputs.len() {
                self.line_indent(2, &format!("case {idx}: goto ciel_async_resume_{idx};"));
            }
            self.line_indent(2, "default: return EINVAL;");
            self.line_indent(1, "}");
        }
        let falls_through = self.gen_block_inner(body, 1)?;
        if falls_through && function.ret.is_never() {
            self.line_indent(1, "ciel_panic(NULL, 0);");
            self.line_indent(1, "return EIO;");
        } else if falls_through && !function.ret.is_erased_value() {
            self.line_indent(1, "ciel_panic(NULL, 0);");
            self.line_indent(1, "return EIO;");
        } else if falls_through {
            self.line_indent(1, "return 0;");
        }
        let cleanup_cases = self.current_async_cleanup_cases.clone();
        let owned_resource_roots = self.current_owned_resource_roots.clone();
        self.current_heap_locals.clear();
        self.current_param_locals.clear();
        self.current_owned_resource_roots.clear();
        self.current_closure_owner = None;
        self.current_return_ty = Ty::Void;
        self.current_async_output = None;
        self.current_async_context = None;
        self.current_async_await_index = 0;
        self.current_async_frame_locals.clear();
        self.current_async_await_outputs.clear();
        self.current_async_defer_arg_index = 0;
        self.current_async_cleanup_cases.clear();
        self.line("}");
        self.emit_async_cleanup_function(&names, &cleanup_cases, &owned_resource_roots);
        Ok(())
    }

    pub(super) fn emit_async_cleanup_function(
        &mut self,
        names: &AsyncFunctionNames,
        cleanup_cases: &[Vec<Vec<String>>],
        owned_resource_roots: &[(Ty, String)],
    ) {
        self.line(&format!(
            "static void {}(void *ctx_raw, int32_t reason) {{",
            names.cleanup
        ));
        self.line_indent(
            1,
            &format!("{} *ctx = ({} *)ctx_raw;", names.context, names.context),
        );
        self.line_indent(1, "(void)reason;");
        self.line_indent(1, "if (ctx == NULL) return;");
        self.line_indent(1, "if (ctx->cleanup_state == 0) {");
        for (ty, value) in owned_resource_roots.iter().rev() {
            self.line_indent(2, &format!("{};", self.resource_cleanup_call(ty, value)));
        }
        self.line_indent(2, "ciel_future_clear_pending_operation(ctx->future);");
        self.line_indent(2, "return;");
        self.line_indent(1, "}");
        self.line_indent(1, "if (ctx->active_future != NULL) {");
        self.line_indent(2, "(void)ciel_future_abort(ctx->active_future);");
        self.line_indent(2, "ctx->active_future = NULL;");
        self.line_indent(1, "}");
        self.line_indent(1, "switch (ctx->cleanup_state) {");
        for (idx, frames) in cleanup_cases.iter().enumerate() {
            if frames.iter().all(|frame| frame.is_empty()) {
                continue;
            }
            self.line_indent(2, &format!("case {}:", idx + 1));
            self.emit_defer_frames(frames, 3);
            self.line_indent(3, "break;");
        }
        self.line_indent(2, "default:");
        self.line_indent(3, "break;");
        self.line_indent(1, "}");
        self.line_indent(1, "ctx->pc = 0;");
        self.line_indent(1, "ctx->cleanup_state = 0;");
        self.line_indent(1, "ciel_future_clear_pending_operation(ctx->future);");
        self.line("}");
    }

    pub(super) fn gen_block(&mut self, block: &TBlock, indent: usize) -> DiagResult<bool> {
        self.line_indent(indent, "{");
        let falls_through = self.gen_block_inner(block, indent + 1)?;
        self.line_indent(indent, "}");
        Ok(falls_through)
    }

    pub(super) fn gen_block_inner(&mut self, block: &TBlock, indent: usize) -> DiagResult<bool> {
        self.push_owned_resource_root_scope();
        let mut falls_through = true;
        for stmt in &block.statements {
            if !self.gen_stmt(stmt, indent)? {
                falls_through = false;
                break;
            }
        }
        if falls_through {
            self.emit_current_defers(indent);
        }
        self.defer_stack.pop();
        Ok(falls_through)
    }

    pub(super) fn push_owned_resource_root_scope(&mut self) {
        self.defer_stack.push(Vec::new());
        if self.defer_stack.len() == 1 {
            for (ty, value) in self.current_owned_resource_roots.clone() {
                self.push_resource_cleanup_defer(&ty, &value);
            }
        }
    }

    pub(super) fn gen_stmt(&mut self, stmt: &TStmt, indent: usize) -> DiagResult<bool> {
        self.emit_line_directive(stmt.span);
        match &stmt.kind {
            TStmtKind::Block(block) => self.gen_block(block, indent),
            TStmtKind::VarDecl {
                ty,
                name,
                local_id,
                init,
            } => {
                let cname = self.local_c_name(*local_id, name);
                if ty.is_erased_value() {
                    if let Some(init) = init {
                        let value = self.gen_expr_in_stmt(init, indent)?;
                        self.line_indent(indent, &format!("(void)({value});"));
                    }
                    return Ok(true);
                }
                if self.local_is_async_frame(*local_id) || self.local_is_heap(*local_id) {
                    self.gen_frame_or_heap_local_decl(ty, name, *local_id, init.as_ref(), indent)?;
                    return Ok(true);
                }
                if let Some(init) = init {
                    self.with_temporary_resource_cleanup_scope(|this| {
                        this.emit_local_decl_with_init(ty, &cname, init, indent)
                    })?;
                } else if self.type_is_affine(ty) {
                    self.line_indent(indent, &format!("{} = {{0}};", self.c_decl(ty, &cname)));
                } else {
                    self.line_indent(indent, &format!("{};", self.c_decl(ty, &cname)));
                }
                let local_value = self.local_value_expr(*local_id, name);
                self.push_resource_cleanup_defer(ty, &local_value);
                Ok(true)
            }
            TStmtKind::Assign { target, value } => {
                if target.ty.is_erased_value() {
                    let target = self.gen_expr_in_stmt(target, indent)?;
                    let value = self.gen_expr_in_stmt(value, indent)?;
                    self.line_indent(indent, &format!("(void)({target});"));
                    self.line_indent(indent, &format!("(void)({value});"));
                    return Ok(true);
                }
                self.emit_assignment(target, value, indent)?;
                Ok(true)
            }
            TStmtKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                let cond = self.gen_expr_in_stmt(cond, indent)?;
                self.line_indent(indent, &format!("if ({cond})"));
                let then_falls_through = self.gen_block(then_block, indent)?;
                let else_falls_through = if let Some(else_branch) = else_branch {
                    self.line_indent(indent, "else");
                    self.gen_stmt(else_branch, indent)?
                } else {
                    true
                };
                Ok(then_falls_through || else_falls_through)
            }
            TStmtKind::While { cond, body } => {
                if expr_needs_stmt_lowering(cond) {
                    self.line_indent(indent, "while (true)");
                    self.line_indent(indent, "{");
                    let cond = self.gen_expr_in_stmt(cond, indent + 1)?;
                    self.line_indent(indent + 1, &format!("if (!({cond})) break;"));
                    self.loop_defer_starts.push(self.defer_stack.len());
                    self.break_defer_starts.push(self.defer_stack.len());
                    self.continue_targets.push(None);
                    self.gen_block(body, indent + 1)?;
                    self.continue_targets.pop();
                    self.break_defer_starts.pop();
                    self.loop_defer_starts.pop();
                    self.line_indent(indent, "}");
                } else {
                    let cond = self.gen_expr(cond)?;
                    self.line_indent(indent, &format!("while ({cond})"));
                    self.loop_defer_starts.push(self.defer_stack.len());
                    self.break_defer_starts.push(self.defer_stack.len());
                    self.continue_targets.push(None);
                    self.gen_block(body, indent)?;
                    self.continue_targets.pop();
                    self.break_defer_starts.pop();
                    self.loop_defer_starts.pop();
                }
                Ok(true)
            }
            TStmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if for_stmt_needs_stmt_lowering(init.as_ref(), cond.as_ref(), step.as_ref())
                    || self.for_stmt_needs_resource_lowering(
                        init.as_ref(),
                        cond.as_ref(),
                        step.as_ref(),
                    )
                {
                    return self.gen_lowered_for_stmt(
                        init.as_ref(),
                        cond.as_ref(),
                        step.as_ref(),
                        body,
                        indent,
                    );
                }
                let init = if let Some(TForInit::VarDecl {
                    ty,
                    name,
                    local_id,
                    init,
                }) = init
                    && (self.local_is_heap(*local_id) || self.local_is_async_frame(*local_id))
                {
                    self.gen_frame_or_heap_local_decl(ty, name, *local_id, init.as_ref(), indent)?;
                    String::new()
                } else {
                    init.as_ref()
                        .map(|init| self.gen_for_init(init))
                        .transpose()?
                        .unwrap_or_default()
                };
                let cond = cond
                    .as_ref()
                    .map(|expr| self.gen_expr(expr))
                    .transpose()?
                    .unwrap_or_default();
                let step = step
                    .as_ref()
                    .map(|step| self.gen_for_init(step))
                    .transpose()?
                    .unwrap_or_default();
                self.line_indent(indent, &format!("for ({init}; {cond}; {step})"));
                self.loop_defer_starts.push(self.defer_stack.len());
                self.break_defer_starts.push(self.defer_stack.len());
                self.continue_targets.push(None);
                self.gen_block(body, indent)?;
                self.continue_targets.pop();
                self.break_defer_starts.pop();
                self.loop_defer_starts.pop();
                Ok(true)
            }
            TStmtKind::Switch {
                expr,
                enum_type_name,
                cases,
                has_default,
                default,
                can_fallthrough,
            } => {
                let switch_is_affine = self.type_is_affine(&expr.ty);
                let temp = if switch_is_affine {
                    self.emit_temp_value("switch", expr, indent)?
                } else {
                    let temp = self.next_temp("switch");
                    let expr_code = self.gen_expr_in_stmt(expr, indent)?;
                    self.line_indent(indent, &format!("{enum_type_name} {temp} = {expr_code};"));
                    temp
                };
                let matched = has_default.then(|| self.next_temp("matched"));
                if let Some(matched) = &matched {
                    self.line_indent(indent, &format!("bool {matched} = false;"));
                }
                self.break_defer_starts.push(self.defer_stack.len());
                self.line_indent(indent, &format!("switch ({temp}.tag) {{"));
                let mut grouped = BTreeMap::<usize, Vec<&crate::thir::TCase>>::new();
                for case in cases {
                    grouped.entry(case.variant_index).or_default().push(case);
                }
                for (variant_index, cases) in grouped {
                    self.line_indent(indent + 1, &format!("case {variant_index}: {{"));
                    for case in cases {
                        let mut conditions = Vec::new();
                        self.collect_pattern_conditions(
                            &case.pattern,
                            &temp,
                            true,
                            &mut conditions,
                        );
                        let condition = if conditions.is_empty() {
                            "true".to_string()
                        } else {
                            conditions.join(" && ")
                        };
                        self.line_indent(indent + 2, &format!("if ({condition}) {{"));
                        if let Some(matched) = &matched {
                            self.line_indent(indent + 3, &format!("{matched} = true;"));
                        }
                        self.defer_stack.push(Vec::new());
                        self.emit_pattern_bindings(&case.pattern, &temp, indent + 3)?;
                        if switch_is_affine {
                            let cleanup = self.resource_cleanup_call(&expr.ty, &temp);
                            self.line_indent(indent + 3, &format!("{cleanup};"));
                        }
                        let mut branch_falls_through = true;
                        for stmt in &case.statements {
                            if !self.gen_stmt(stmt, indent + 3)? {
                                branch_falls_through = false;
                                break;
                            }
                        }
                        if branch_falls_through {
                            self.emit_current_defers(indent + 3);
                            self.line_indent(indent + 3, "break;");
                        }
                        self.defer_stack.pop();
                        self.line_indent(indent + 2, "}");
                    }
                    self.line_indent(indent + 2, "break;");
                    self.line_indent(indent + 1, "}");
                }
                self.line_indent(indent, "}");
                if let Some(matched) = &matched {
                    self.line_indent(indent, &format!("if (!{matched}) {{"));
                    self.defer_stack.push(Vec::new());
                    if switch_is_affine {
                        let cleanup = self.resource_cleanup_call(&expr.ty, &temp);
                        self.line_indent(indent + 1, &format!("{cleanup};"));
                    }
                    let mut default_falls_through = true;
                    for stmt in default {
                        if !self.gen_stmt(stmt, indent + 1)? {
                            default_falls_through = false;
                            break;
                        }
                    }
                    if default_falls_through {
                        self.emit_current_defers(indent + 1);
                    }
                    self.defer_stack.pop();
                    self.line_indent(indent, "}");
                }
                self.break_defer_starts.pop();
                Ok(*can_fallthrough)
            }
            TStmtKind::Defer(expr) => {
                let call = self.gen_defer_call(expr, indent)?;
                self.defer_stack
                    .last_mut()
                    .expect("defer stack is not empty")
                    .push(call);
                Ok(true)
            }
            TStmtKind::ResourceCleanup(expr) => {
                if self.type_is_affine(&expr.ty) {
                    let value = self.gen_expr_in_stmt(expr, indent)?;
                    let helper = self.resource_cleanup_name(&expr.ty);
                    self.line_indent(indent, &format!("{helper}(&{value});"));
                } else {
                    let value = self.gen_expr_in_stmt(expr, indent)?;
                    self.line_indent(indent, &format!("(void)({value});"));
                }
                Ok(true)
            }
            TStmtKind::Return(expr) => {
                if let Some(out_raw) = self.current_async_output.clone() {
                    if let Some(expr) = expr {
                        if self.current_return_ty.is_erased_value() {
                            let value = self.gen_expr_in_stmt(expr, indent)?;
                            self.line_indent(indent, &format!("(void)({value});"));
                            self.emit_all_defers(indent);
                            self.line_indent(indent, "return 0;");
                            return Ok(false);
                        }
                        let temp = self.emit_temp_value("return", expr, indent)?;
                        let return_ty = self.current_return_ty.clone();
                        self.emit_all_defers(indent);
                        self.emit_async_output_store(&return_ty, &out_raw, &temp, indent);
                        self.line_indent(indent, "return 0;");
                    } else {
                        self.emit_all_defers(indent);
                        self.line_indent(indent, "return 0;");
                    }
                    return Ok(false);
                }
                if let Some(expr) = expr {
                    if self.current_return_ty.is_erased_value() {
                        let value = self.gen_expr_in_stmt(expr, indent)?;
                        self.line_indent(indent, &format!("(void)({value});"));
                        self.emit_all_defers(indent);
                        self.line_indent(indent, "return;");
                        return Ok(false);
                    }
                    let temp = self.emit_temp_value("return", expr, indent)?;
                    let return_ty = self.current_return_ty.clone();
                    let value = self.emit_return_value(&return_ty, &temp, indent);
                    self.emit_all_defers(indent);
                    self.line_indent(indent, &format!("return {value};"));
                } else {
                    self.emit_all_defers(indent);
                    self.line_indent(indent, "return;");
                }
                Ok(false)
            }
            TStmtKind::Break => {
                self.emit_break_defers(indent);
                self.line_indent(indent, "break;");
                Ok(false)
            }
            TStmtKind::Continue => {
                self.emit_loop_defers(indent);
                if let Some(label) = self.continue_targets.last().and_then(|label| label.clone()) {
                    self.line_indent(indent, &format!("goto {label};"));
                } else {
                    self.line_indent(indent, "continue;");
                }
                Ok(false)
            }
            TStmtKind::Expr(expr) => {
                let terminates = expr.is_never();
                self.emit_expr_statement(expr, indent)?;
                Ok(!terminates)
            }
            TStmtKind::Unsupported => Err(vec![Diagnostic::new(
                stmt.span,
                "cannot generate C for unsupported statement",
            )]),
        }
    }

    pub(super) fn collect_pattern_conditions(
        &self,
        pattern: &TPattern,
        value_expr: &str,
        skip_current: bool,
        out: &mut Vec<String>,
    ) {
        match pattern {
            TPattern::Wildcard { .. } | TPattern::Binding { .. } => {}
            TPattern::Variant {
                variant_name,
                variant_index,
                payload,
                ..
            } => {
                if !skip_current {
                    out.push(format!("{value_expr}.tag == {variant_index}"));
                }
                let mut physical_idx = 0;
                for pattern in payload {
                    if pattern.ty().is_erased_value() {
                        continue;
                    }
                    let idx = physical_idx;
                    physical_idx += 1;
                    let child = format!("{value_expr}.as.{variant_name}._{idx}");
                    self.collect_pattern_conditions(pattern, &child, false, out);
                }
            }
        }
    }

    pub(super) fn emit_pattern_bindings(
        &mut self,
        pattern: &TPattern,
        value_expr: &str,
        indent: usize,
    ) -> DiagResult<()> {
        match pattern {
            TPattern::Wildcard { .. } => {}
            TPattern::Binding {
                local_id, name, ty, ..
            } => {
                if ty.is_erased_value() {
                    return Ok(());
                }
                let cname = self.local_c_name(*local_id, name);
                if self.local_is_async_frame(*local_id) {
                    if self.local_is_heap(*local_id) {
                        self.line_indent(
                            indent,
                            &format!(
                                "{cname} = ({}){};",
                                self.c_pointer_type(ty),
                                self.c_object_alloc_expr(ty)
                            ),
                        );
                        self.emit_value_copy(&format!("*{cname}"), value_expr, ty, indent);
                    } else {
                        self.emit_value_copy(&cname, value_expr, ty, indent);
                    }
                    if self.type_is_affine(ty) {
                        self.emit_resource_zero_expr(ty, value_expr, indent);
                        let local_value = self.local_value_expr(*local_id, name);
                        self.push_resource_cleanup_defer(ty, &local_value);
                    }
                    return Ok(());
                }
                if self.local_is_heap(*local_id) {
                    self.line_indent(
                        indent,
                        &format!(
                            "{} = ({}){};",
                            self.c_pointer_decl(ty, &cname),
                            self.c_pointer_type(ty),
                            self.c_object_alloc_expr(ty)
                        ),
                    );
                    self.emit_value_copy(&format!("*{cname}"), value_expr, ty, indent);
                } else {
                    self.line_indent(indent, &format!("{};", self.c_decl(ty, &cname)));
                    self.emit_value_copy(&cname, value_expr, ty, indent);
                }
                if self.type_is_affine(ty) {
                    self.emit_resource_zero_expr(ty, value_expr, indent);
                    let local_value = self.local_value_expr(*local_id, name);
                    self.push_resource_cleanup_defer(ty, &local_value);
                }
            }
            TPattern::Variant {
                enum_type_name,
                variant_name,
                variant_index,
                payload,
                ..
            } => {
                let physical_payload =
                    self.checked_enum_variant_payload(enum_type_name, *variant_index)?;
                let mut physical_idx = 0;
                for payload_pattern in payload {
                    if payload_pattern.ty().is_erased_value() {
                        continue;
                    }
                    let idx = physical_idx;
                    physical_idx += 1;
                    let Some(source_ty) = physical_payload.get(idx) else {
                        return Err(vec![Diagnostic::new(
                            None,
                            format!(
                                "internal error: enum `{enum_type_name}` payload layout is missing field {idx}"
                            ),
                        )]);
                    };
                    let child = format!("{value_expr}.as.{variant_name}._{idx}");
                    let child = if source_ty == payload_pattern.ty() {
                        child
                    } else {
                        let adapted = self.value_initializer_for_type(
                            source_ty,
                            payload_pattern.ty(),
                            &child,
                            None,
                        )?;
                        let temp = self.next_temp("pattern_payload");
                        self.line_indent(
                            indent,
                            &format!("{} = {adapted};", self.c_decl(payload_pattern.ty(), &temp)),
                        );
                        temp
                    };
                    self.emit_pattern_bindings(payload_pattern, &child, indent)?;
                    if source_ty != payload_pattern.ty() && self.type_is_affine(source_ty) {
                        let source_child = format!("{value_expr}.as.{variant_name}._{idx}");
                        self.emit_resource_zero_expr(source_ty, &source_child, indent);
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn checked_enum_variant_payload(
        &self,
        enum_type_name: &str,
        variant_index: usize,
    ) -> DiagResult<Vec<Ty>> {
        let Some(enm) = self
            .program
            .checked
            .enums
            .iter()
            .find(|enm| enm.name == enum_type_name)
        else {
            return Err(vec![Diagnostic::new(
                None,
                format!("internal error: missing enum layout `{enum_type_name}`"),
            )]);
        };
        let Some(variant) = enm.variants.get(variant_index) else {
            return Err(vec![Diagnostic::new(
                None,
                format!(
                    "internal error: enum `{enum_type_name}` has no variant at index {variant_index}"
                ),
            )]);
        };
        Ok(variant.payload.clone())
    }

    pub(super) fn gen_for_init(&mut self, init: &TForInit) -> DiagResult<String> {
        match init {
            TForInit::VarDecl {
                ty,
                name,
                local_id,
                init,
            } => {
                let cname = self.local_c_name(*local_id, name);
                if ty.is_erased_value() {
                    return if let Some(init) = init {
                        Ok(format!("(void)({})", self.gen_expr(init)?))
                    } else {
                        Ok(String::new())
                    };
                }
                if self.local_is_async_frame(*local_id) {
                    return if let Some(init) = init {
                        Ok(format!("{cname} = {}", self.gen_expr(init)?))
                    } else {
                        Ok(String::new())
                    };
                }
                if let Some(init) = init {
                    Ok(format!(
                        "{} = {}",
                        self.c_decl(ty, &cname),
                        self.gen_expr(init)?
                    ))
                } else {
                    Ok(self.c_decl(ty, &cname))
                }
            }
            TForInit::Assign { target, value } => {
                if target.ty.is_erased_value() {
                    Ok(format!(
                        "(void)({}), (void)({})",
                        self.gen_expr(target)?,
                        self.gen_expr(value)?
                    ))
                } else {
                    Ok(format!(
                        "{} = {}",
                        self.gen_expr(target)?,
                        self.gen_expr(value)?
                    ))
                }
            }
            TForInit::Expr(expr) => self.gen_expr(expr),
        }
    }

    pub(super) fn for_stmt_needs_resource_lowering(
        &self,
        init: Option<&TForInit>,
        cond: Option<&TExpr>,
        step: Option<&TForInit>,
    ) -> bool {
        init.is_some_and(|clause| self.for_clause_needs_resource_lowering(clause))
            || cond.is_some_and(|expr| self.type_is_affine(&expr.ty))
            || step.is_some_and(|clause| self.for_clause_needs_resource_lowering(clause))
    }

    pub(super) fn for_clause_needs_resource_lowering(&self, clause: &TForInit) -> bool {
        match clause {
            TForInit::VarDecl { ty, init, .. } => {
                self.type_is_affine(ty)
                    || init
                        .as_ref()
                        .is_some_and(|expr| self.type_is_affine(&expr.ty))
            }
            TForInit::Assign { target, value } => {
                self.type_is_affine(&target.ty) || self.type_is_affine(&value.ty)
            }
            TForInit::Expr(expr) => self.type_is_affine(&expr.ty),
        }
    }

    pub(super) fn gen_lowered_for_stmt(
        &mut self,
        init: Option<&TForInit>,
        cond: Option<&TExpr>,
        step: Option<&TForInit>,
        body: &TBlock,
        indent: usize,
    ) -> DiagResult<bool> {
        self.line_indent(indent, "{");
        self.defer_stack.push(Vec::new());
        if let Some(init) = init {
            self.gen_for_init_stmt(init, indent + 1)?;
        }
        self.line_indent(indent + 1, "while (true)");
        self.line_indent(indent + 1, "{");
        if let Some(cond) = cond {
            let cond = self.gen_expr_in_stmt(cond, indent + 2)?;
            self.line_indent(indent + 2, &format!("if (!({cond})) break;"));
        }
        let step_label = self.next_temp("for_step");
        self.loop_defer_starts.push(self.defer_stack.len());
        self.break_defer_starts.push(self.defer_stack.len());
        self.continue_targets.push(Some(step_label.clone()));
        self.gen_block(body, indent + 2)?;
        self.continue_targets.pop();
        self.break_defer_starts.pop();
        self.loop_defer_starts.pop();
        self.line_indent(indent + 2, &format!("{step_label}:;"));
        if let Some(step) = step {
            self.gen_for_init_stmt(step, indent + 2)?;
        }
        self.line_indent(indent + 1, "}");
        self.emit_current_defers(indent + 1);
        self.defer_stack.pop();
        self.line_indent(indent, "}");
        Ok(true)
    }

    pub(super) fn gen_for_init_stmt(&mut self, init: &TForInit, indent: usize) -> DiagResult<()> {
        match init {
            TForInit::VarDecl {
                ty,
                name,
                local_id,
                init,
            } => {
                let cname = self.local_c_name(*local_id, name);
                if ty.is_erased_value() {
                    if let Some(init) = init {
                        let value = self.gen_expr_in_stmt(init, indent)?;
                        self.line_indent(indent, &format!("(void)({value});"));
                    }
                    return Ok(());
                }
                if self.local_is_heap(*local_id) || self.local_is_async_frame(*local_id) {
                    return self.gen_frame_or_heap_local_decl(
                        ty,
                        name,
                        *local_id,
                        init.as_ref(),
                        indent,
                    );
                }
                if let Some(init) = init {
                    self.with_temporary_resource_cleanup_scope(|this| {
                        this.emit_local_decl_with_init(ty, &cname, init, indent)
                    })?;
                } else if self.type_is_affine(ty) {
                    self.line_indent(indent, &format!("{} = {{0}};", self.c_decl(ty, &cname)));
                } else {
                    self.line_indent(indent, &format!("{};", self.c_decl(ty, &cname)));
                }
                let local_value = self.local_value_expr(*local_id, name);
                self.push_resource_cleanup_defer(ty, &local_value);
                Ok(())
            }
            TForInit::Assign { target, value } => {
                if target.ty.is_erased_value() {
                    let target = self.gen_expr_in_stmt(target, indent)?;
                    let value = self.gen_expr_in_stmt(value, indent)?;
                    self.line_indent(indent, &format!("(void)({target});"));
                    self.line_indent(indent, &format!("(void)({value});"));
                    return Ok(());
                }
                self.emit_assignment(target, value, indent)?;
                Ok(())
            }
            TForInit::Expr(expr) => {
                self.emit_expr_statement(expr, indent)?;
                Ok(())
            }
        }
    }

    pub(super) fn gen_frame_or_heap_local_decl(
        &mut self,
        ty: &Ty,
        name: &str,
        local_id: LocalId,
        init: Option<&TExpr>,
        indent: usize,
    ) -> DiagResult<()> {
        let cname = self.local_c_name(local_id, name);
        let local_value = self.local_value_expr(local_id, name);
        if self.local_is_async_frame(local_id) {
            if self.local_is_heap(local_id) {
                self.line_indent(
                    indent,
                    &format!(
                        "{cname} = ({}){};",
                        self.c_pointer_type(ty),
                        self.c_object_alloc_expr(ty)
                    ),
                );
                if self.type_is_affine(ty) {
                    self.line_indent(indent, &format!("memset({cname}, 0, sizeof(*{cname}));"));
                }
                if let Some(init) = init {
                    let target = format!("(*{cname})");
                    self.with_temporary_resource_cleanup_scope(|this| {
                        this.push_temporary_resource_cleanup_defer(ty, &local_value);
                        this.emit_expr_store(&target, init, indent)
                    })?;
                }
            } else if let Some(init) = init {
                self.with_temporary_resource_cleanup_scope(|this| {
                    this.push_temporary_resource_cleanup_defer(ty, &local_value);
                    this.emit_expr_store(&cname, init, indent)
                })?;
            }
            self.push_resource_cleanup_defer(ty, &local_value);
            return Ok(());
        }
        self.line_indent(
            indent,
            &format!(
                "{} = ({}){};",
                self.c_pointer_decl(ty, &cname),
                self.c_pointer_type(ty),
                self.c_object_alloc_expr(ty)
            ),
        );
        if self.type_is_affine(ty) {
            self.line_indent(indent, &format!("memset({cname}, 0, sizeof(*{cname}));"));
        }
        if let Some(init) = init {
            let target = format!("(*{cname})");
            self.with_temporary_resource_cleanup_scope(|this| {
                this.push_temporary_resource_cleanup_defer(ty, &local_value);
                this.emit_expr_store(&target, init, indent)
            })?;
        }
        self.push_resource_cleanup_defer(ty, &local_value);
        Ok(())
    }
}
