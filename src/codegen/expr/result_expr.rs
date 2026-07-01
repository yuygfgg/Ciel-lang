use super::*;

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn result_layout(
        &self,
        ty: &Ty,
        span: crate::span::Span,
    ) -> DiagResult<ResultLayout> {
        let Ty::Named { name, args } = ty else {
            return Err(vec![Diagnostic::new(
                span,
                format!("internal error: expected Result type, got `{ty}`"),
            )]);
        };
        let c_type = self.c_named_type(name, args);
        let Some(enm) = self
            .program
            .checked
            .enums
            .iter()
            .find(|enm| enm.name == c_type)
        else {
            return Err(vec![Diagnostic::new(
                span,
                format!("internal error: missing enum layout for `{ty}`"),
            )]);
        };
        let Some((ok_index, ok_variant)) = enm
            .variants
            .iter()
            .enumerate()
            .find(|(_, variant)| variant.name == "Ok")
        else {
            return Err(vec![Diagnostic::new(
                span,
                format!(
                    "internal error: Result layout `{}` has no Ok variant",
                    enm.name
                ),
            )]);
        };
        let Some((err_index, err_variant)) = enm
            .variants
            .iter()
            .enumerate()
            .find(|(_, variant)| variant.name == "Err")
        else {
            return Err(vec![Diagnostic::new(
                span,
                format!(
                    "internal error: Result layout `{}` has no Err variant",
                    enm.name
                ),
            )]);
        };
        Ok(ResultLayout {
            c_type,
            ok_index,
            ok_name: ok_variant.name.clone(),
            ok_has_payload: !ok_variant.payload.is_empty(),
            ok_payload_ty: ok_variant.payload.first().cloned(),
            err_name: err_variant.name.clone(),
            err_index,
            err_has_payload: !err_variant.payload.is_empty(),
            err_payload_ty: err_variant.payload.first().cloned(),
        })
    }

    pub(in crate::codegen) fn result_err_literal(
        &self,
        return_layout: &ResultLayout,
        inner_layout: &ResultLayout,
        temp: &str,
    ) -> String {
        if return_layout.err_has_payload {
            let payload = self.value_or_initializer_from_expr(
                return_layout
                    .err_payload_ty
                    .as_ref()
                    .expect("result err payload type is present"),
                &format!("{temp}.as.{}._0", inner_layout.err_name),
            );
            format!(
                "({}){{ .tag = {}, .as.{} = {{ ._0 = {} }} }}",
                return_layout.c_type, return_layout.err_index, return_layout.err_name, payload
            )
        } else {
            format!(
                "({}){{ .tag = {} }}",
                return_layout.c_type, return_layout.err_index
            )
        }
    }

    pub(in crate::codegen) fn result_ok_literal(
        &self,
        layout: &ResultLayout,
        value: Option<&str>,
    ) -> String {
        if layout.ok_has_payload {
            let payload = self.value_or_initializer_from_expr(
                layout
                    .ok_payload_ty
                    .as_ref()
                    .expect("result ok payload type is present"),
                value.unwrap_or("0"),
            );
            format!(
                "({}){{ .tag = {}, .as.{} = {{ ._0 = {} }} }}",
                layout.c_type, layout.ok_index, layout.ok_name, payload
            )
        } else {
            format!("({}){{ .tag = {} }}", layout.c_type, layout.ok_index)
        }
    }

    pub(in crate::codegen) fn result_err_from_error_literal(
        &self,
        layout: &ResultLayout,
        error: &str,
    ) -> String {
        if layout.err_has_payload {
            format!(
                "({}){{ .tag = {}, .as.{} = {{ ._0 = {error} }} }}",
                layout.c_type, layout.err_index, layout.err_name
            )
        } else {
            format!("({}){{ .tag = {} }}", layout.c_type, layout.err_index)
        }
    }

    pub(super) fn enum_variant_literal(
        &self,
        enum_ty: &Ty,
        variant_name: &str,
        payloads: &[String],
        span: crate::span::Span,
    ) -> DiagResult<String> {
        let c_type = self.c_type(enum_ty);
        let Some(enm) = self
            .program
            .checked
            .enums
            .iter()
            .find(|enm| enm.name == c_type)
        else {
            return Err(vec![Diagnostic::new(
                span,
                format!("internal error: missing enum layout for `{enum_ty}`"),
            )]);
        };
        let Some((variant_index, variant)) = enm
            .variants
            .iter()
            .enumerate()
            .find(|(_, variant)| variant.name == variant_name)
        else {
            return Err(vec![Diagnostic::new(
                span,
                format!("internal error: enum `{enum_ty}` has no variant `{variant_name}`"),
            )]);
        };
        if payloads.len() != variant.payload.len() {
            return Err(vec![Diagnostic::new(
                span,
                format!(
                    "internal error: enum variant `{variant_name}` expects {} payloads, got {}",
                    variant.payload.len(),
                    payloads.len()
                ),
            )]);
        }
        if payloads.is_empty() {
            return Ok(format!("({c_type}){{ .tag = {variant_index} }}"));
        }
        let payload = payloads
            .iter()
            .zip(variant.payload.iter())
            .enumerate()
            .map(|(idx, (payload, payload_ty))| {
                let payload = self.value_or_initializer_from_expr(payload_ty, payload);
                format!("._{idx} = {payload}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!(
            "({c_type}){{ .tag = {variant_index}, .as.{variant_name} = {{ {payload} }} }}"
        ))
    }

    pub(super) fn enum_has_variant_with_payload(
        &self,
        enum_ty: &Ty,
        variant_name: &str,
        payload: &[Ty],
    ) -> bool {
        let c_type = self.c_type(enum_ty);
        self.program
            .checked
            .enums
            .iter()
            .find(|enm| enm.name == c_type)
            .and_then(|enm| {
                enm.variants
                    .iter()
                    .find(|variant| variant.name == variant_name)
            })
            .is_some_and(|variant| variant.payload == payload)
    }

    pub(in crate::codegen) fn async_error_runtime_literal(
        &self,
        code: &str,
        span: crate::span::Span,
    ) -> DiagResult<String> {
        self.enum_variant_literal(
            &std_async_error_ty(),
            "Runtime",
            &[format!("(int64_t)({code})")],
            span,
        )
    }

    pub(in crate::codegen) fn async_error_message_clone_literal(
        &self,
        error: &str,
        span: crate::span::Span,
    ) -> DiagResult<String> {
        self.enum_variant_literal(
            &std_async_error_ty(),
            "MessageClone",
            &[error.to_string()],
            span,
        )
    }

    pub(in crate::codegen) fn async_error_channel_or_runtime_literal(
        &self,
        code: &str,
        span: crate::span::Span,
    ) -> DiagResult<String> {
        let closed =
            self.enum_variant_literal(&std_async_error_ty(), "ChannelClosed", &[], span)?;
        let runtime = self.async_error_runtime_literal(code, span)?;
        Ok(format!(
            "(({code}) == ciel_async_channel_closed_errno() ? {closed} : {runtime})"
        ))
    }

    pub(super) fn resource_error_runtime_literal(
        &self,
        code: &str,
        span: crate::span::Span,
    ) -> DiagResult<String> {
        self.enum_variant_literal(
            &std_resource_error_ty(),
            "Runtime",
            &[format!("(int64_t)({code})")],
            span,
        )
    }

    pub(super) fn resource_scoped_resource_literal(
        &self,
        scoped_error_ty: &Ty,
        code: &str,
        span: crate::span::Span,
    ) -> DiagResult<String> {
        let inner = self.resource_error_runtime_literal(code, span)?;
        self.enum_variant_literal(scoped_error_ty, "Resource", &[inner], span)
    }

    pub(super) fn resource_scoped_body_literal(
        &self,
        scoped_error_ty: &Ty,
        body_error: Option<&str>,
        span: crate::span::Span,
    ) -> DiagResult<String> {
        let payloads = body_error
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        self.enum_variant_literal(scoped_error_ty, "Body", &payloads, span)
    }

    pub(super) fn runtime_error_payload_for_result(
        &self,
        layout: &ResultLayout,
        code: &str,
        span: crate::span::Span,
    ) -> DiagResult<String> {
        let Some(err_ty) = layout.err_payload_ty.as_ref() else {
            return Ok("0".to_string());
        };
        if err_ty == &std_error_ty() {
            return Ok(self.error_code_literal(code));
        }
        if err_ty == &std_async_error_ty() {
            return self.async_error_runtime_literal(code, span);
        }
        if self.enum_has_variant_with_payload(err_ty, "Async", &[std_async_error_ty()]) {
            let inner = self.async_error_runtime_literal(code, span)?;
            return self.enum_variant_literal(err_ty, "Async", &[inner], span);
        }
        if self.enum_has_variant_with_payload(err_ty, "TaskGroupAsync", &[std_async_error_ty()]) {
            let inner = self.async_error_runtime_literal(code, span)?;
            return self.enum_variant_literal(err_ty, "TaskGroupAsync", &[inner], span);
        }
        if self.enum_has_variant_with_payload(err_ty, "Runtime", &[Ty::I64]) {
            return self.enum_variant_literal(
                err_ty,
                "Runtime",
                &[format!("(int64_t)({code})")],
                span,
            );
        }
        if self.enum_has_variant_with_payload(err_ty, "Resource", &[std_resource_error_ty()]) {
            let inner = self.resource_error_runtime_literal(code, span)?;
            return self.enum_variant_literal(err_ty, "Resource", &[inner], span);
        }
        Err(vec![Diagnostic::new(
            span,
            format!(
                "internal error: cannot synthesize async runtime error for Result error type `{err_ty}`"
            ),
        )])
    }

    pub(super) fn message_clone_payload_for_result(
        &self,
        layout: &ResultLayout,
        error: &str,
        span: crate::span::Span,
    ) -> DiagResult<String> {
        let Some(err_ty) = layout.err_payload_ty.as_ref() else {
            return Ok("0".to_string());
        };
        if err_ty == &std_error_ty() {
            return Ok(error.to_string());
        }
        if err_ty == &std_async_error_ty() {
            return self.async_error_message_clone_literal(error, span);
        }
        if self.enum_has_variant_with_payload(err_ty, "MessageClone", &[std_error_ty()]) {
            return self.enum_variant_literal(err_ty, "MessageClone", &[error.to_string()], span);
        }
        if self.enum_has_variant_with_payload(err_ty, "Async", &[std_async_error_ty()]) {
            let inner = self.async_error_message_clone_literal(error, span)?;
            return self.enum_variant_literal(err_ty, "Async", &[inner], span);
        }
        if self.enum_has_variant_with_payload(err_ty, "TaskGroupAsync", &[std_async_error_ty()]) {
            let inner = self.async_error_message_clone_literal(error, span)?;
            return self.enum_variant_literal(err_ty, "TaskGroupAsync", &[inner], span);
        }
        Err(vec![Diagnostic::new(
            span,
            format!(
                "internal error: cannot synthesize async message-clone error for Result error type `{err_ty}`"
            ),
        )])
    }

    pub(in crate::codegen) fn result_err_from_runtime_literal(
        &self,
        layout: &ResultLayout,
        code: &str,
        span: crate::span::Span,
    ) -> DiagResult<String> {
        let payload = self.runtime_error_payload_for_result(layout, code, span)?;
        Ok(self.result_err_from_error_literal(layout, &payload))
    }

    pub(super) fn result_err_from_message_clone_literal(
        &self,
        layout: &ResultLayout,
        error: &str,
        span: crate::span::Span,
    ) -> DiagResult<String> {
        let payload = self.message_clone_payload_for_result(layout, error, span)?;
        Ok(self.result_err_from_error_literal(layout, &payload))
    }

    pub(super) fn emit_error_boxed_value(
        &mut self,
        value: &str,
        concrete_ty: &Ty,
        indent: usize,
        span: crate::span::Span,
    ) -> DiagResult<String> {
        let error_ty = std_error_ty();
        let trait_ty = std_error_trait_ty();
        let trait_value = self.emit_dynamic_interface_value_from_code(
            &trait_ty,
            concrete_ty,
            value,
            indent,
            span,
        )?;
        Ok(self.error_value_literal(&error_ty, &trait_value, "\"\"", "NULL"))
    }

    pub(super) fn error_code_literal(&self, code: &str) -> String {
        let code_ty = std_error_code_ty();
        let code_ptr = format!(
            "(({})ciel_box_value(&({}){{ .code = (int64_t)({code}) }}, sizeof({})))",
            self.c_pointer_type(&code_ty),
            self.c_type(&code_ty),
            self.c_sizeof_type(&code_ty)
        );
        let trait_value =
            self.dynamic_interface_from_ptr_literal(&std_error_trait_ty(), &code_ty, &code_ptr);
        self.error_value_literal(&std_error_ty(), &trait_value, "\"\"", "NULL")
    }

    pub(super) fn error_value_literal(
        &self,
        error_ty: &Ty,
        value: &str,
        context: &str,
        source: &str,
    ) -> String {
        let c_type = self.c_type(error_ty);
        format!(
            "({c_type}){{ .value = {value}, .context = CIEL_CONST_STR({context}), .source = {source} }}"
        )
    }

    pub(super) fn emit_dynamic_interface_value_from_code(
        &mut self,
        dyn_ty: &Ty,
        concrete_ty: &Ty,
        value: &str,
        indent: usize,
        span: crate::span::Span,
    ) -> DiagResult<String> {
        let data_ptr = if matches!(concrete_ty, Ty::Pointer { .. }) {
            format!("(void *)({value})")
        } else {
            let temp = self.next_temp("dyn_data");
            self.line_indent(
                indent,
                &format!(
                    "{} *{temp} = ({})ciel_alloc(sizeof({}));",
                    self.c_type(concrete_ty),
                    self.c_pointer_type(concrete_ty),
                    self.c_sizeof_type(concrete_ty)
                ),
            );
            self.line_indent(indent, &format!("*{temp} = {value};"));
            format!("(void *){temp}")
        };
        let Ty::DynamicInterface { .. } = dyn_ty else {
            return Err(vec![Diagnostic::new(
                span,
                "internal error: error-box target is not dynamic",
            )]);
        };
        let dyn_c = self.c_type(dyn_ty);
        let vtable = self.dynamic_table_name(dyn_ty, concrete_ty);
        Ok(format!(
            "({dyn_c}){{ .data = {data_ptr}, .vtable = &{vtable} }}"
        ))
    }

    pub(super) fn dynamic_interface_from_ptr_literal(
        &self,
        dyn_ty: &Ty,
        concrete_ty: &Ty,
        ptr: &str,
    ) -> String {
        let dyn_c = self.c_type(dyn_ty);
        let vtable = self.dynamic_table_name(dyn_ty, concrete_ty);
        format!("({dyn_c}){{ .data = (void *)({ptr}), .vtable = &{vtable} }}")
    }

    pub(super) fn emit_clone_message_result_from_ptr(
        &mut self,
        message_ty: &Ty,
        source_ptr: &str,
        indent: usize,
        span: crate::span::Span,
    ) -> DiagResult<String> {
        let result_ty = std_result_ty(message_ty.clone(), std_error_ty());
        let result_temp = self.next_temp("clone_result");
        self.line_indent(
            indent,
            &format!("{};", self.c_decl(&result_ty, &result_temp)),
        );
        if let Some(function_def) = self
            .clone_message_impl(message_ty)
            .map(|implementation| implementation.function_def)
        {
            self.line_indent(
                indent,
                &format!(
                    "{result_temp} = {}({source_ptr});",
                    self.c_name(function_def)
                ),
            );
            return Ok(result_temp);
        }
        if let Ty::Closure { constraints, .. } = message_ty
            && constraints.positive.iter().any(is_clone_message_capability)
        {
            let capability = clone_message_capability();
            let field = self.retained_closure_witness_field_name(&capability);
            self.line_indent(
                indent,
                &format!("{result_temp} = (*({source_ptr})).{field}((void *)({source_ptr}));"),
            );
            return Ok(result_temp);
        }
        Err(vec![Diagnostic::new(
            span,
            format!("internal error: missing clone_message implementation for `{message_ty}`"),
        )])
    }

    pub(in crate::codegen) fn emit_task_boundary_clone_result_from_ptr(
        &mut self,
        message_ty: &Ty,
        source_ptr: &str,
        indent: usize,
        span: crate::span::Span,
    ) -> DiagResult<String> {
        if self.clone_message_impl(message_ty).is_some()
            || matches!(
                message_ty,
                Ty::Closure { constraints, .. }
                    if constraints.positive.iter().any(is_clone_message_capability)
            )
        {
            return self.emit_clone_message_result_from_ptr(message_ty, source_ptr, indent, span);
        }
        if let Ty::Named { name, args } = message_ty
            && matches!(meta_repr_marker_name(name), Some(false))
            && let Some(repr_ty) = self.meta_repr_marker_storage_ty(name, args)
        {
            let result_ty = std_result_ty(message_ty.clone(), std_error_ty());
            let result_layout = self.result_layout(&result_ty, span)?;
            let result_temp = self.next_temp("task_boundary_repr_clone");
            self.line_indent(
                indent,
                &format!("{};", self.c_decl(&result_ty, &result_temp)),
            );

            let repr_source_ptr = format!("(const {} *)({source_ptr})", self.c_type(&repr_ty));
            let repr_clone =
                self.emit_clone_message_result_from_ptr(&repr_ty, &repr_source_ptr, indent, span)?;
            let repr_result_ty = std_result_ty(repr_ty.clone(), std_error_ty());
            let repr_layout = self.result_layout(&repr_result_ty, span)?;
            self.line_indent(
                indent,
                &format!("if ({repr_clone}.tag == {}) {{", repr_layout.err_index),
            );
            self.line_indent(
                indent + 1,
                &format!(
                    "{result_temp} = {};",
                    self.result_err_literal(&result_layout, &repr_layout, &repr_clone)
                ),
            );
            self.line_indent(indent, "} else {");
            if result_layout.ok_has_payload {
                let ok_payload = format!("{repr_clone}.as.{}._0", repr_layout.ok_name);
                self.line_indent(
                    indent + 1,
                    &format!(
                        "{result_temp} = {};",
                        self.result_ok_literal(&result_layout, Some(&ok_payload))
                    ),
                );
            } else {
                self.line_indent(
                    indent + 1,
                    &format!(
                        "{result_temp} = {};",
                        self.result_ok_literal(&result_layout, None)
                    ),
                );
            }
            self.line_indent(indent, "}");
            return Ok(result_temp);
        }
        if !matches!(
            message_ty,
            Ty::Array { .. } | Ty::Named { .. } | Ty::ClosureInstance { .. }
        ) {
            return self.emit_clone_message_result_from_ptr(message_ty, source_ptr, indent, span);
        }

        let result_ty = std_result_ty(message_ty.clone(), std_error_ty());
        let result_layout = self.result_layout(&result_ty, span)?;
        let result_temp = self.next_temp("task_boundary_clone");
        let done_label = self.next_temp("task_boundary_clone_done");
        self.line_indent(
            indent,
            &format!("{};", self.c_decl(&result_ty, &result_temp)),
        );

        let source_value = format!("(*({source_ptr}))");
        let (repr_ty, repr_value) = self.emit_meta_owned_leaf_repr_expr(
            span,
            message_ty,
            &source_value,
            message_ty,
            indent,
        )?;
        let repr_temp = self.next_temp("task_boundary_repr");
        self.line_indent(
            indent,
            &format!("{} {repr_temp} = {repr_value};", self.c_type(&repr_ty)),
        );
        let repr_clone = self.emit_clone_message_result_from_ptr(
            &repr_ty,
            &format!("&{repr_temp}"),
            indent,
            span,
        )?;
        let repr_result_ty = std_result_ty(repr_ty.clone(), std_error_ty());
        let repr_layout = self.result_layout(&repr_result_ty, span)?;
        self.line_indent(
            indent,
            &format!("if ({repr_clone}.tag == {}) {{", repr_layout.err_index),
        );
        self.line_indent(
            indent + 1,
            &format!(
                "{result_temp} = {};",
                self.result_err_literal(&result_layout, &repr_layout, &repr_clone)
            ),
        );
        self.line_indent(indent + 1, &format!("goto {done_label};"));
        self.line_indent(indent, "}");

        if message_ty.is_erased_value() || !result_layout.ok_has_payload {
            self.line_indent(
                indent,
                &format!(
                    "{result_temp} = {};",
                    self.result_ok_literal(&result_layout, None)
                ),
            );
        } else {
            let value_temp = self.next_temp("task_boundary_value");
            self.line_indent(
                indent,
                &format!("{};", self.c_decl(message_ty, &value_temp)),
            );
            self.emit_meta_value_from_repr_into(
                span,
                &value_temp,
                message_ty,
                &format!("{repr_clone}.as.{}._0", repr_layout.ok_name),
                message_ty,
                indent,
            )?;
            self.line_indent(
                indent,
                &format!(
                    "{result_temp} = {};",
                    self.result_ok_literal(&result_layout, Some(&value_temp))
                ),
            );
        }
        self.line_indent(indent, &format!("{done_label}:;"));
        Ok(result_temp)
    }

    pub(in crate::codegen) fn clone_message_impl(&self, ty: &Ty) -> Option<&CheckedImpl> {
        self.program.checked.impls.iter().find(|implementation| {
            implementation.interface_name == STD_MESSAGE_CLONE_INTERFACE
                && implementation
                    .receiver_ty
                    .as_ref()
                    .is_some_and(|receiver| receiver == ty)
                && implementation.interface_args.get(1..) == Some(&[][..])
        })
    }

    pub(super) fn share_handle_impl(&self, ty: &Ty) -> Option<&CheckedImpl> {
        self.program.checked.impls.iter().find(|implementation| {
            implementation.interface_name == STD_MESSAGE_SHARE_HANDLE_INTERFACE
                && implementation
                    .receiver_ty
                    .as_ref()
                    .is_some_and(|receiver| receiver == ty)
                && implementation.interface_args.get(1..) == Some(&[][..])
        })
    }

    pub(super) fn thread_local_impl(&self, ty: &Ty) -> Option<&CheckedImpl> {
        self.program.checked.impls.iter().find(|implementation| {
            implementation.interface_name == STD_MESSAGE_THREAD_LOCAL_INTERFACE
                && implementation
                    .receiver_ty
                    .as_ref()
                    .is_some_and(|receiver| receiver == ty)
                && implementation.interface_args.get(1..) == Some(&[][..])
        })
    }
}
