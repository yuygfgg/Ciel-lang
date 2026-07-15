use super::*;

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn emit_closure_thunks_and_wrappers(&mut self) -> DiagResult<()> {
        let closures = self.plan.closure_defs.clone();
        for closure in closures.values() {
            self.emit_closure_thunk(closure)?;
            self.line("");
        }
        let wrappers = self.plan.function_closure_wrappers.clone();
        for wrapper in wrappers.values() {
            self.emit_function_closure_wrapper(wrapper);
            self.line("");
        }
        let wrappers = self.plan.retained_closure_wrappers.clone();
        for wrapper in wrappers.values() {
            self.emit_retained_closure_wrapper(wrapper)?;
            self.line("");
        }
        Ok(())
    }

    fn emit_closure_thunk(&mut self, closure: &ClosureDef) -> DiagResult<()> {
        if closure.is_async {
            return self.emit_async_closure_thunk(closure);
        }
        let (ret, _) = self.callable_ret_params(&closure.ty)?;
        self.line(&format!("{} {{", self.closure_thunk_decl(closure)));

        let previous_return_ty = std::mem::replace(&mut self.current_return_ty, ret.clone());
        let closure_heap_locals = self
            .escapes
            .functions
            .get(&closure.function_def)
            .map(|escape| escape.heap_locals.clone())
            .unwrap_or_default();
        let previous_heap_locals =
            std::mem::replace(&mut self.current_heap_locals, closure_heap_locals.clone());
        let previous_param_locals = std::mem::replace(
            &mut self.current_param_locals,
            closure
                .params
                .iter()
                .filter(|(_, _, ty)| !ty.is_erased_value())
                .enumerate()
                .map(|(idx, (local_id, name, _))| {
                    let cname = if closure_heap_locals.contains(local_id) {
                        format!("{name}__{}", local_id.0)
                    } else {
                        format!("arg{idx}")
                    };
                    (*local_id, cname)
                })
                .collect(),
        );
        let closure_resource_roots = closure
            .captures
            .iter()
            .enumerate()
            .filter_map(|(idx, capture)| {
                self.type_is_affine(&capture.ty)
                    .then(|| (capture.ty.clone(), format!("env->cap{idx}")))
            })
            .chain(
                closure
                    .params
                    .iter()
                    .filter(|(_, _, ty)| !ty.is_erased_value())
                    .filter_map(|(local_id, name, ty)| {
                        self.type_is_affine(ty)
                            .then(|| (ty.clone(), self.local_value_expr(*local_id, name)))
                    }),
            )
            .collect();
        let previous_owned_resource_roots = std::mem::replace(
            &mut self.current_owned_resource_roots,
            closure_resource_roots,
        );
        let previous_capture_locals = std::mem::take(&mut self.current_capture_locals);
        self.defer_stack.clear();
        self.temporary_resource_cleanup_depth = 0;
        self.temporary_resource_cleanup_frames.clear();
        self.loop_defer_starts.clear();
        self.break_defer_starts.clear();

        if matches!(closure.ty, Ty::Closure { .. } | Ty::ClosureInstance { .. })
            && !closure.captures.is_empty()
        {
            let env_name = self.closure_env_name(closure.id);
            self.line_indent(1, &format!("{env_name} *env = ({env_name} *)env_raw;"));
            self.current_capture_locals = closure
                .captures
                .iter()
                .enumerate()
                .map(|(idx, capture)| (capture.local_id, format!("env->cap{idx}")))
                .collect();
        }

        for (idx, (local_id, name, ty)) in closure
            .params
            .iter()
            .filter(|(_, _, ty)| !ty.is_erased_value())
            .enumerate()
        {
            if !self.local_is_heap(*local_id) {
                continue;
            }
            let cname = self.local_c_name(*local_id, name);
            self.line_indent(
                1,
                &format!(
                    "{} = ({}){};",
                    self.c_pointer_decl(ty, &cname),
                    self.c_pointer_type(ty),
                    self.c_object_alloc_expr(ty)
                ),
            );
            if self.type_is_affine(ty) {
                self.line_indent(1, &format!("memset({cname}, 0, sizeof(*{cname}));"));
            }
            self.emit_value_copy(&format!("(*{cname})"), &format!("arg{idx}"), ty, 1);
            if self.type_is_affine(ty) {
                self.line_indent(1, &format!("memset(&arg{idx}, 0, sizeof(arg{idx}));"));
            }
        }

        match &closure.body {
            TClosureBody::Expr(expr) => {
                self.push_owned_resource_root_scope();
                let result = self.emit_sync_closure_expr_return(expr, &ret, 1);
                self.defer_stack.pop();
                result?;
            }
            TClosureBody::Block(block) => {
                let falls_through = self.gen_block_inner(block, 1)?;
                if falls_through && ret.is_never() {
                    self.line_indent(1, "ciel_panic(NULL, 0);");
                } else if falls_through && !ret.is_erased_value() {
                    self.line_indent(1, "ciel_panic(NULL, 0);");
                    self.line_indent(1, &format!("return {};", self.zero_return_value(&ret)));
                }
            }
        }

        self.current_return_ty = previous_return_ty;
        self.current_heap_locals = previous_heap_locals;
        self.current_param_locals = previous_param_locals;
        self.current_owned_resource_roots = previous_owned_resource_roots;
        self.current_capture_locals = previous_capture_locals;
        self.defer_stack.clear();
        self.temporary_resource_cleanup_depth = 0;
        self.temporary_resource_cleanup_frames.clear();
        self.loop_defer_starts.clear();
        self.break_defer_starts.clear();
        self.line("}");
        Ok(())
    }

    fn emit_sync_closure_expr_return(
        &mut self,
        expr: &TExpr,
        ret: &Ty,
        indent: usize,
    ) -> DiagResult<()> {
        let value = self.gen_expr_in_stmt(expr, indent)?;
        if ret.is_erased_value() {
            self.line_indent(indent, &format!("(void)({value});"));
            self.emit_current_defers(indent);
            self.line_indent(indent, "return;");
        } else {
            let value = self.emit_return_value(ret, &value, indent);
            self.emit_current_defers(indent);
            self.line_indent(indent, &format!("return {value};"));
        }
        Ok(())
    }

    fn emit_async_closure_thunk(&mut self, closure: &ClosureDef) -> DiagResult<()> {
        let (ret, _) = self.callable_ret_params(&closure.ty)?;
        let output_ty = self.future_output_ty_for_codegen(&ret).ok_or_else(|| {
            vec![Diagnostic::new(
                crate::span::Span::new(crate::span::FileId(0), 0, 0),
                "internal error: async closure thunk must return Future<T>",
            )]
        })?;
        self.line(&format!("{} {{", self.closure_thunk_decl(closure)));
        let raw = self.emit_async_closure_future_from_parts(
            closure,
            Some("env_raw"),
            AsyncClosureCaptureInit::Copy,
            None,
            None,
            None,
            &output_ty,
            1,
        )?;
        self.line_indent(
            1,
            &format!(
                "return ({}){{ .handle = (void *){raw} }};",
                self.c_type(&ret)
            ),
        );
        self.line("}");
        self.emit_async_closure_run_and_cleanup(closure, &output_ty)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::codegen) fn emit_async_closure_future_from_parts(
        &mut self,
        closure: &ClosureDef,
        env_raw: Option<&str>,
        capture_init: AsyncClosureCaptureInit,
        result_temp: Option<&str>,
        result_layout: Option<&ResultLayout>,
        done_label: Option<&str>,
        output_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let names = self.async_closure_names(closure);
        let ctx = self.next_temp("async_closure_ctx");
        self.line_indent(
            indent,
            &format!(
                "{} *{ctx} = ({} *)ciel_alloc(sizeof({}));",
                names.context, names.context, names.context
            ),
        );
        self.line_indent(indent, &format!("memset({ctx}, 0, sizeof(*{ctx}));"));
        self.line_indent(indent, &format!("{ctx}->pc = 0;"));
        self.line_indent(indent, &format!("{ctx}->cleanup_state = 0;"));
        self.line_indent(indent, &format!("{ctx}->active_future = NULL;"));

        let env_name = if matches!(capture_init, AsyncClosureCaptureInit::Copy)
            && closure
                .captures
                .iter()
                .any(|capture| !capture.ty.is_erased_value())
        {
            let env_raw = env_raw.ok_or_else(|| {
                vec![Diagnostic::new(
                    crate::span::Span::new(crate::span::FileId(0), 0, 0),
                    "internal error: async closure copy init requires an environment",
                )]
            })?;
            let env_name = self.closure_env_name(closure.id);
            self.line_indent(
                indent,
                &format!("{env_name} *env = ({env_name} *){env_raw};"),
            );
            Some(env_name)
        } else {
            None
        };

        for (idx, capture) in closure.captures.iter().enumerate() {
            if capture.ty.is_erased_value() {
                continue;
            }
            match capture_init {
                AsyncClosureCaptureInit::Copy => {
                    let _ = env_name.as_ref().expect("env exists for capture copy");
                    self.emit_value_copy(
                        &format!("{ctx}->cap{idx}"),
                        &format!("env->cap{idx}"),
                        &capture.ty,
                        indent,
                    );
                }
                AsyncClosureCaptureInit::CloneForTask => {
                    let Some(result_temp) = result_temp else {
                        return Err(vec![Diagnostic::new(
                            crate::span::Span::new(crate::span::FileId(0), 0, 0),
                            "internal error: task capture clone needs a result temp",
                        )]);
                    };
                    let Some(result_layout) = result_layout else {
                        return Err(vec![Diagnostic::new(
                            crate::span::Span::new(crate::span::FileId(0), 0, 0),
                            "internal error: task capture clone needs a result layout",
                        )]);
                    };
                    let Some(done_label) = done_label else {
                        return Err(vec![Diagnostic::new(
                            crate::span::Span::new(crate::span::FileId(0), 0, 0),
                            "internal error: task capture clone needs a done label",
                        )]);
                    };
                    let capture_expr = TExpr {
                        span: crate::span::Span::new(crate::span::FileId(0), 0, 0),
                        ty: capture.ty.clone(),
                        kind: TExprKind::Local(capture.local_id, capture.name.clone()),
                    };
                    let capture_src =
                        self.emit_temp_value("task_capture_src", &capture_expr, indent)?;
                    let cloned = self.emit_task_boundary_clone_result_from_ptr(
                        &capture.ty,
                        &format!("&{capture_src}"),
                        indent,
                        capture_expr.span,
                    )?;
                    self.emit_async_clone_error_jump(
                        result_temp,
                        result_layout,
                        &cloned,
                        &capture.ty,
                        done_label,
                        indent,
                        capture_expr.span,
                    )?;
                    let clone_layout = self.result_layout(
                        &std_result_ty(
                            &self.program.checked.resolved,
                            capture.ty.clone(),
                            std_error_ty(&self.program.checked.resolved),
                        ),
                        capture_expr.span,
                    )?;
                    self.emit_value_copy(
                        &format!("{ctx}->cap{idx}"),
                        &format!("{cloned}.as.{}._0", clone_layout.ok_name),
                        &capture.ty,
                        indent,
                    );
                }
            }
        }

        for (idx, (_, _, ty)) in closure
            .params
            .iter()
            .filter(|(_, _, ty)| !ty.is_erased_value())
            .enumerate()
        {
            self.emit_value_copy(
                &format!("{ctx}->arg{idx}"),
                &format!("arg{idx}"),
                ty,
                indent,
            );
        }

        let raw = self.next_temp("async_closure_future");
        let (size_expr, align_expr) = self.future_result_layout_args(output_ty);
        self.line_indent(
            indent,
            &format!(
                "CielFuture *{raw} = ciel_future_new({size_expr}, {align_expr}, {}, {ctx}, {});",
                names.run, names.cleanup
            ),
        );
        if let (Some(result_temp), Some(result_layout), Some(done_label)) =
            (result_temp, result_layout, done_label)
        {
            let span = crate::span::Span::new(crate::span::FileId(0), 0, 0);
            self.line_indent(indent, &format!("if ({raw} == NULL) {{"));
            self.line_indent(
                indent + 1,
                &format!(
                    "{result_temp} = {};",
                    self.result_err_from_runtime_literal(
                        result_layout,
                        "errno == 0 ? EIO : errno",
                        span
                    )?
                ),
            );
            self.line_indent(indent + 1, &format!("goto {done_label};"));
            self.line_indent(indent, "}");
        } else {
            self.line_indent(indent, &format!("if ({raw} == NULL) {{"));
            self.line_indent(indent + 1, "ciel_panic(NULL, 0);");
            self.line_indent(indent, "}");
        }
        Ok(raw)
    }

    fn emit_async_closure_run_and_cleanup(
        &mut self,
        closure: &ClosureDef,
        output_ty: &Ty,
    ) -> DiagResult<()> {
        let names = self.async_closure_names(closure);
        self.line(&format!(
            "static int32_t {}(CielFuture *future, void *ctx_raw, void *out_raw) {{",
            names.run
        ));
        self.defer_stack.clear();
        self.temporary_resource_cleanup_depth = 0;
        self.temporary_resource_cleanup_frames.clear();
        self.loop_defer_starts.clear();
        self.break_defer_starts.clear();
        self.current_return_ty = output_ty.clone();
        self.current_async_output = Some("out_raw".to_string());
        self.current_async_context = Some("ctx".to_string());
        self.current_async_await_index = 0;
        self.current_async_frame_locals = self
            .async_frame_locals_with_escape_info_for_closure(closure)
            .into_iter()
            .map(|local| (local.id, format!("ctx->{}", local.field)))
            .collect();
        self.current_async_await_outputs = self
            .async_facts_for_closure(closure)
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
            .get(&closure.function_def)
            .map(|escape| escape.heap_locals.clone())
            .unwrap_or_default();
        self.current_param_locals = closure
            .params
            .iter()
            .filter(|(_, _, ty)| !ty.is_erased_value())
            .enumerate()
            .map(|(idx, (local_id, _, _))| (*local_id, format!("ctx->arg{idx}")))
            .collect();
        self.current_capture_locals = closure
            .captures
            .iter()
            .enumerate()
            .map(|(idx, capture)| (capture.local_id, format!("ctx->cap{idx}")))
            .collect();
        self.current_owned_resource_roots = closure
            .captures
            .iter()
            .enumerate()
            .filter_map(|(idx, capture)| {
                self.type_is_affine(&capture.ty)
                    .then(|| (capture.ty.clone(), format!("ctx->cap{idx}")))
            })
            .chain(
                closure
                    .params
                    .iter()
                    .filter(|(_, _, ty)| !ty.is_erased_value())
                    .enumerate()
                    .filter_map(|(idx, (_, _, ty))| {
                        self.type_is_affine(ty)
                            .then(|| (ty.clone(), format!("ctx->arg{idx}")))
                    }),
            )
            .collect();
        self.line_indent(
            1,
            &format!("{} *ctx = ({} *)ctx_raw;", names.context, names.context),
        );
        self.line_indent(1, "(void)future;");
        if !self.current_async_await_outputs.is_empty() {
            self.line_indent(1, "switch (ctx->pc) {");
            self.line_indent(2, "case 0: break;");
            for idx in 1..=self.current_async_await_outputs.len() {
                self.line_indent(2, &format!("case {idx}: goto ciel_async_resume_{idx};"));
            }
            self.line_indent(2, "default: return EINVAL;");
            self.line_indent(1, "}");
        }
        let falls_through = match &closure.body {
            TClosureBody::Expr(expr) => {
                self.push_owned_resource_root_scope();
                let result = self.emit_async_closure_expr_return(expr, output_ty, 1);
                self.defer_stack.pop();
                result?;
                false
            }
            TClosureBody::Block(block) => self.gen_block_inner(block, 1)?,
        };
        if falls_through && output_ty.is_never() {
            self.line_indent(1, "ciel_panic(NULL, 0);");
            self.line_indent(1, "return EIO;");
        } else if falls_through && !output_ty.is_erased_value() {
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
        self.current_capture_locals.clear();
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

    fn emit_async_closure_expr_return(
        &mut self,
        expr: &TExpr,
        output_ty: &Ty,
        indent: usize,
    ) -> DiagResult<()> {
        if output_ty.is_erased_value() {
            let value = self.gen_expr_in_stmt(expr, indent)?;
            self.line_indent(indent, &format!("(void)({value});"));
        } else {
            let temp = self.emit_temp_value("async_closure_return", expr, indent)?;
            self.emit_async_output_store(output_ty, "out_raw", &temp, indent);
        }
        self.emit_current_defers(indent);
        self.line_indent(indent, "return 0;");
        Ok(())
    }

    fn emit_function_closure_wrapper(&mut self, wrapper: &FunctionClosureWrapper) {
        let (ret, params) = self
            .callable_ret_params(&wrapper.closure_ty)
            .expect("wrapper closure type is callable");
        self.line(&format!(
            "{} {{",
            self.function_closure_thunk_decl(&wrapper.closure_ty, &wrapper.function_ty)
        ));
        let env_name = self.function_closure_env_name(&wrapper.closure_ty, &wrapper.function_ty);
        self.line_indent(1, &format!("{env_name} *env = ({env_name} *)env_raw;"));
        let args = (0..params.iter().filter(|ty| !ty.is_erased_value()).count())
            .map(|idx| format!("arg{idx}"))
            .collect::<Vec<_>>()
            .join(", ");
        if ret.is_erased_value() {
            self.line_indent(1, &format!("env->func({args});"));
            self.line_indent(1, "return;");
        } else {
            self.line_indent(1, &format!("return env->func({args});"));
        }
        self.line("}");
    }

    fn emit_retained_closure_wrapper(
        &mut self,
        wrapper: &RetainedClosureWrapper,
    ) -> DiagResult<()> {
        let (ret, params) = self.callable_ret_params(&wrapper.target_ty)?;
        self.line(&format!(
            "{} {{",
            self.retained_closure_thunk_decl(&wrapper.target_ty, &wrapper.source_ty)
        ));
        let env_name = self.retained_closure_env_name(&wrapper.target_ty, &wrapper.source_ty);
        self.line_indent(1, &format!("{env_name} *env = ({env_name} *)env_raw;"));
        let args = (0..params.iter().filter(|ty| !ty.is_erased_value()).count())
            .map(|idx| format!("arg{idx}"))
            .collect::<Vec<_>>()
            .join(", ");
        let call_args = if args.is_empty() {
            "env->source.env".to_string()
        } else {
            format!("env->source.env, {args}")
        };
        if ret.is_erased_value() {
            self.line_indent(1, &format!("env->source.call({call_args});"));
            self.line_indent(1, "return;");
        } else {
            self.line_indent(1, &format!("return env->source.call({call_args});"));
        }
        self.line("}");
        Ok(())
    }
}
