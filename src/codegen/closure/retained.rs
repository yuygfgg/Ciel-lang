use super::*;

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn emit_retained_closure_witness_prototypes(&mut self) {
        let witnesses = self.plan.retained_closure_witnesses.clone();
        for witness in witnesses.values() {
            self.line(&format!("{};", self.retained_closure_witness_decl(witness)));
        }
    }

    pub(in crate::codegen) fn emit_retained_closure_witnesses(&mut self) -> DiagResult<()> {
        let witnesses = self.plan.retained_closure_witnesses.clone();
        for witness in witnesses.values() {
            self.emit_retained_closure_witness(witness)?;
            self.line("");
        }
        Ok(())
    }

    fn retained_closure_witness_decl(&self, witness: &RetainedClosureWitness) -> String {
        let ret = self.retained_closure_interface_ret(&witness.target_ty, &witness.capability);
        let params =
            self.retained_closure_interface_params(&witness.target_ty, &witness.capability);
        let params = params
            .iter()
            .filter(|ty| !ty.is_erased_value())
            .enumerate()
            .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}")))
            .collect::<Vec<_>>()
            .join(", ");
        self.c_static_return_decl(
            &ret,
            &format!(
                "{}({})",
                self.retained_closure_witness_name(
                    &witness.target_ty,
                    &witness.source_ty,
                    &witness.capability
                ),
                params
            ),
        )
    }

    fn emit_retained_closure_witness(
        &mut self,
        witness: &RetainedClosureWitness,
    ) -> DiagResult<()> {
        if self.is_std_clone_message_capability(&witness.capability) {
            return self.emit_retained_closure_clone_witness(witness);
        }
        if retained_closure_can_forward_source_witness(&witness.source_ty, &witness.capability) {
            return self.emit_retained_closure_forwarding_witness(witness);
        }
        let Some(implementation) = self
            .impl_for_retained_closure_witness(&witness.capability, &witness.source_ty)
            .cloned()
        else {
            return Err(vec![Diagnostic::new(
                None,
                format!(
                    "internal error: missing retained closure witness implementation for `{}` on `{}`",
                    witness.capability.name, witness.source_ty
                ),
            )]);
        };
        let ret = self.retained_closure_interface_ret(&witness.target_ty, &witness.capability);
        let params =
            self.retained_closure_interface_params(&witness.target_ty, &witness.capability);
        self.line(&format!(
            "{} {{",
            self.retained_closure_witness_decl(witness)
        ));
        let first_param = implementation
            .params
            .first()
            .cloned()
            .unwrap_or(Ty::Unknown);
        let mut args = Vec::new();
        if matches!(first_param, Ty::Pointer { .. }) {
            args.push(self.retained_closure_source_pointer_expr(witness));
        } else {
            let source_ptr = self.retained_closure_source_pointer_expr(witness);
            args.push(format!("*({source_ptr})"));
        }
        let mut physical_idx = 1;
        for (target_param, source_param) in params
            .iter()
            .skip(1)
            .zip(implementation.params.iter().skip(1))
        {
            if target_param.is_erased_value() {
                continue;
            }
            let arg = format!("arg{physical_idx}");
            physical_idx += 1;
            if source_param.is_erased_value() {
                continue;
            }
            let adapted = self.emit_retained_closure_adapt_value(
                witness,
                target_param,
                source_param,
                &arg,
                1,
            )?;
            args.push(adapted);
        }
        let call = format!(
            "{}({})",
            self.c_name(implementation.function_def),
            args.join(", ")
        );
        let source_ret = implementation.ret.clone();
        self.emit_retained_closure_adapted_return(witness, &source_ret, &ret, &call, 1)?;
        self.line("}");
        Ok(())
    }

    fn emit_retained_closure_forwarding_witness(
        &mut self,
        witness: &RetainedClosureWitness,
    ) -> DiagResult<()> {
        let target_ret =
            self.retained_closure_interface_ret(&witness.target_ty, &witness.capability);
        let source_ret =
            self.retained_closure_interface_ret(&witness.source_ty, &witness.capability);
        let params =
            self.retained_closure_interface_params(&witness.target_ty, &witness.capability);
        self.line(&format!(
            "{} {{",
            self.retained_closure_witness_decl(witness)
        ));
        let source_ptr = self.retained_closure_source_pointer_expr(witness);
        let mut args = vec![format!("(void *)({source_ptr})")];
        let source_params =
            self.retained_closure_interface_params(&witness.source_ty, &witness.capability);
        let mut physical_idx = 1;
        for (target_param, source_param) in params.iter().skip(1).zip(source_params.iter().skip(1))
        {
            if target_param.is_erased_value() {
                continue;
            }
            let arg = format!("arg{physical_idx}");
            physical_idx += 1;
            if source_param.is_erased_value() {
                continue;
            }
            let adapted = self.emit_retained_closure_adapt_value(
                witness,
                target_param,
                source_param,
                &arg,
                1,
            )?;
            args.push(adapted);
        }
        let field = self.retained_closure_witness_field_name(&witness.capability);
        let call = format!("(*({source_ptr})).{field}({})", args.join(", "));
        self.emit_retained_closure_adapted_return(witness, &source_ret, &target_ret, &call, 1)?;
        self.line("}");
        Ok(())
    }

    fn emit_retained_closure_adapted_return(
        &mut self,
        witness: &RetainedClosureWitness,
        source_ret: &Ty,
        target_ret: &Ty,
        call: &str,
        indent: usize,
    ) -> DiagResult<()> {
        if target_ret.is_erased_value() {
            self.line_indent(indent, &format!("{call};"));
            self.line_indent(indent, "return;");
            return Ok(());
        }
        if source_ret == target_ret {
            self.line_indent(indent, &format!("return {call};"));
            return Ok(());
        }
        let source_temp = self.next_temp("retained_source_ret");
        self.line_indent(
            indent,
            &format!("{} = {call};", self.c_decl(source_ret, &source_temp)),
        );
        let adapted = self.emit_retained_closure_adapt_value(
            witness,
            source_ret,
            target_ret,
            &source_temp,
            indent,
        )?;
        self.line_indent(indent, &format!("return {adapted};"));
        Ok(())
    }

    fn emit_retained_closure_adapt_value(
        &mut self,
        witness: &RetainedClosureWitness,
        source_ty: &Ty,
        target_ty: &Ty,
        source_value: &str,
        indent: usize,
    ) -> DiagResult<String> {
        if source_ty == target_ty {
            return Ok(source_value.to_string());
        }
        if source_ty == &witness.source_ty && target_ty == &witness.target_ty {
            return self.emit_closure_value_from_source(
                &witness.target_ty,
                &witness.source_ty,
                source_value,
                indent,
            );
        }
        if source_ty == &witness.target_ty && target_ty == &witness.source_ty {
            return self.emit_retained_closure_source_value_from_target(
                witness,
                source_value,
                indent,
            );
        }
        if let (Some((source_ok, source_err)), Some((target_ok, target_err))) = (
            result_args(&self.program.checked.resolved, source_ty),
            result_args(&self.program.checked.resolved, target_ty),
        ) && source_err == target_err
        {
            let source_layout = self.result_layout(source_ty, witness.span)?;
            let target_layout = self.result_layout(target_ty, witness.span)?;
            let target_temp = self.next_temp("retained_target_ret");
            self.line_indent(
                indent,
                &format!("{};", self.c_decl(target_ty, &target_temp)),
            );
            self.line_indent(
                indent,
                &format!("if ({source_value}.tag == {}) {{", source_layout.err_index),
            );
            self.line_indent(
                indent + 1,
                &format!(
                    "{target_temp} = {};",
                    self.result_err_literal(&target_layout, &source_layout, source_value)
                ),
            );
            self.line_indent(indent, "} else {");
            if target_layout.ok_has_payload {
                let source_ok_value = format!("{source_value}.as.{}._0", source_layout.ok_name);
                let adapted = self.emit_retained_closure_adapt_value(
                    witness,
                    source_ok,
                    target_ok,
                    &source_ok_value,
                    indent + 1,
                )?;
                self.line_indent(
                    indent + 1,
                    &format!(
                        "{target_temp} = {};",
                        self.result_ok_literal(&target_layout, Some(&adapted))
                    ),
                );
            } else {
                self.line_indent(
                    indent + 1,
                    &format!(
                        "{target_temp} = {};",
                        self.result_ok_literal(&target_layout, None)
                    ),
                );
            }
            self.line_indent(indent, "}");
            return Ok(target_temp);
        }
        if let Some(adapted) = self.emit_retained_closure_adapt_struct_value(
            witness,
            source_ty,
            target_ty,
            source_value,
            indent,
        )? {
            return Ok(adapted);
        }
        if let Some(adapted) = self.emit_retained_closure_adapt_enum_value(
            witness,
            source_ty,
            target_ty,
            source_value,
            indent,
        )? {
            return Ok(adapted);
        }
        if let (
            Ty::Array {
                len: source_len,
                elem: source_elem,
            },
            Ty::Array {
                len: target_len,
                elem: target_elem,
            },
        ) = (source_ty, target_ty)
            && source_len == target_len
        {
            let target_temp = self.next_temp("retained_target_array");
            self.line_indent(
                indent,
                &format!("{};", self.c_decl(target_ty, &target_temp)),
            );
            let idx = self.next_temp("retained_i");
            self.line_indent(
                indent,
                &format!("for (size_t {idx} = 0; {idx} < {target_len}; {idx}++) {{"),
            );
            let source_item = format!("({source_value})[{idx}]");
            let adapted_item = self.emit_retained_closure_adapt_value(
                witness,
                source_elem,
                target_elem,
                &source_item,
                indent + 1,
            )?;
            self.line_indent(
                indent + 1,
                &format!("({target_temp})[{idx}] = {adapted_item};"),
            );
            self.line_indent(indent, "}");
            return Ok(target_temp);
        }
        if let (
            Ty::Slice {
                elem: source_elem, ..
            },
            Ty::Slice {
                elem: target_elem, ..
            },
        ) = (source_ty, target_ty)
        {
            let target_temp = self.next_temp("retained_target_slice");
            self.line_indent(
                indent,
                &format!("{};", self.c_decl(target_ty, &target_temp)),
            );
            self.line_indent(
                indent,
                &format!("{target_temp}.len = ({source_value}).len;"),
            );
            if target_elem.is_erased_value() {
                self.line_indent(indent, &format!("{target_temp}.ptr = NULL;"));
                return Ok(target_temp);
            }
            self.line_indent(
                indent,
                &format!(
                    "{target_temp}.ptr = ({}){};",
                    self.c_pointer_type(target_elem),
                    self.c_array_alloc_expr(target_elem, &format!("({source_value}).len"))
                ),
            );
            let idx = self.next_temp("retained_i");
            self.line_indent(
                indent,
                &format!("for (size_t {idx} = 0; {idx} < ({source_value}).len; {idx}++) {{"),
            );
            let source_item = format!("({source_value}).ptr[{idx}]");
            let adapted_item = self.emit_retained_closure_adapt_value(
                witness,
                source_elem,
                target_elem,
                &source_item,
                indent + 1,
            )?;
            self.line_indent(
                indent + 1,
                &format!("{target_temp}.ptr[{idx}] = {adapted_item};"),
            );
            self.line_indent(indent, "}");
            return Ok(target_temp);
        }
        Err(vec![Diagnostic::new(
            witness.span,
            format!(
                "internal error: cannot adapt retained closure witness return `{source_ty}` to `{target_ty}`"
            ),
        )])
    }

    fn emit_retained_closure_source_value_from_target(
        &mut self,
        witness: &RetainedClosureWitness,
        target_value: &str,
        indent: usize,
    ) -> DiagResult<String> {
        let target_temp = self.next_temp("retained_target_value");
        self.line_indent(
            indent,
            &format!(
                "{} = {target_value};",
                self.c_decl(&witness.target_ty, &target_temp)
            ),
        );
        let target_ptr = format!("&{target_temp}");
        let source_ptr =
            self.retained_closure_source_pointer_expr_from_target_ptr(witness, &target_ptr);
        let source_temp = self.next_temp("retained_source_value");
        self.line_indent(
            indent,
            &format!(
                "{} = *({source_ptr});",
                self.c_decl(&witness.source_ty, &source_temp)
            ),
        );
        Ok(source_temp)
    }

    fn emit_retained_closure_adapt_struct_value(
        &mut self,
        witness: &RetainedClosureWitness,
        source_ty: &Ty,
        target_ty: &Ty,
        source_value: &str,
        indent: usize,
    ) -> DiagResult<Option<String>> {
        let (
            Ty::Named {
                name: source_name,
                args: source_args,
            },
            Ty::Named {
                name: target_name,
                args: target_args,
            },
        ) = (source_ty, target_ty)
        else {
            return Ok(None);
        };
        if source_name != target_name || source_args.len() != target_args.len() {
            return Ok(None);
        }
        let source_instance = self.c_named_type(source_name, source_args);
        let target_instance = self.c_named_type(target_name, target_args);
        let Some(source_fields) = self
            .program
            .checked
            .structs
            .iter()
            .find(|strukt| strukt.name == source_instance)
            .map(|strukt| strukt.fields.clone())
        else {
            return Ok(None);
        };
        let Some(target_fields) = self
            .program
            .checked
            .structs
            .iter()
            .find(|strukt| strukt.name == target_instance)
            .map(|strukt| strukt.fields.clone())
        else {
            return Ok(None);
        };
        if source_fields.len() != target_fields.len() {
            return Ok(None);
        }
        let target_temp = self.next_temp("retained_target_struct");
        self.line_indent(
            indent,
            &format!(
                "{} = {};",
                self.c_decl(target_ty, &target_temp),
                self.zero_value(target_ty)
            ),
        );
        for ((source_field, source_field_ty), (target_field, target_field_ty)) in
            source_fields.iter().zip(target_fields.iter())
        {
            if source_field != target_field {
                return Ok(None);
            }
            if target_field_ty.is_erased_value() {
                continue;
            }
            if source_field_ty.is_erased_value() {
                return Ok(None);
            }
            let source_field_value = format!("({source_value}).{source_field}");
            let adapted = self.emit_retained_closure_adapt_value(
                witness,
                source_field_ty,
                target_field_ty,
                &source_field_value,
                indent,
            )?;
            self.emit_value_copy(
                &format!("{target_temp}.{target_field}"),
                &adapted,
                target_field_ty,
                indent,
            );
        }
        Ok(Some(target_temp))
    }

    fn emit_retained_closure_adapt_enum_value(
        &mut self,
        witness: &RetainedClosureWitness,
        source_ty: &Ty,
        target_ty: &Ty,
        source_value: &str,
        indent: usize,
    ) -> DiagResult<Option<String>> {
        let (
            Ty::Named {
                name: source_name,
                args: source_args,
            },
            Ty::Named {
                name: target_name,
                args: target_args,
            },
        ) = (source_ty, target_ty)
        else {
            return Ok(None);
        };
        if source_name != target_name || source_args.len() != target_args.len() {
            return Ok(None);
        }
        let source_instance = self.c_named_type(source_name, source_args);
        let target_instance = self.c_named_type(target_name, target_args);
        let Some(source_variants) = self
            .program
            .checked
            .enums
            .iter()
            .find(|enm| enm.name == source_instance)
            .map(|enm| enm.variants.clone())
        else {
            return Ok(None);
        };
        let Some(target_variants) = self
            .program
            .checked
            .enums
            .iter()
            .find(|enm| enm.name == target_instance)
            .map(|enm| enm.variants.clone())
        else {
            return Ok(None);
        };
        if source_variants.len() != target_variants.len() {
            return Ok(None);
        }
        let target_temp = self.next_temp("retained_target_enum");
        self.line_indent(
            indent,
            &format!(
                "{} = {};",
                self.c_decl(target_ty, &target_temp),
                self.zero_value(target_ty)
            ),
        );
        self.line_indent(indent, &format!("switch (({source_value}).tag) {{"));
        for (idx, (source_variant, target_variant)) in source_variants
            .iter()
            .zip(target_variants.iter())
            .enumerate()
        {
            if source_variant.name != target_variant.name {
                return Ok(None);
            }
            if source_variant.payload.len() != target_variant.payload.len() {
                return Ok(None);
            }
            self.line_indent(indent, &format!("case {idx}:"));
            self.line_indent(indent + 1, &format!("{target_temp}.tag = {idx};"));
            for (payload_idx, (source_payload, target_payload)) in source_variant
                .payload
                .iter()
                .zip(target_variant.payload.iter())
                .enumerate()
            {
                let source_payload_value =
                    format!("({source_value}).as.{}._{payload_idx}", source_variant.name);
                let adapted = self.emit_retained_closure_adapt_value(
                    witness,
                    source_payload,
                    target_payload,
                    &source_payload_value,
                    indent + 1,
                )?;
                self.emit_value_copy(
                    &format!("{target_temp}.as.{}._{payload_idx}", target_variant.name),
                    &adapted,
                    target_payload,
                    indent + 1,
                );
            }
            self.line_indent(indent + 1, "break;");
        }
        self.line_indent(indent, "}");
        Ok(Some(target_temp))
    }

    fn emit_retained_closure_clone_witness(
        &mut self,
        witness: &RetainedClosureWitness,
    ) -> DiagResult<()> {
        let result_ty =
            self.retained_closure_interface_ret(&witness.target_ty, &witness.capability);
        let result_layout = self.result_layout(&result_ty, witness.span)?;
        let result_temp = self.next_temp("retained_clone_result");
        let done_label = self.next_temp("retained_clone_done");
        self.line(&format!(
            "{} {{",
            self.retained_closure_witness_decl(witness)
        ));
        self.line_indent(1, &format!("{};", self.c_decl(&result_ty, &result_temp)));
        if result_layout.ok_has_payload {
            let target = format!("{result_temp}.as.{}._0", result_layout.ok_name);
            let source_clone = self.emit_retained_closure_clone_source_value(
                witness,
                &result_temp,
                &result_layout,
                &done_label,
                1,
            )?;
            let target_value = self.emit_closure_value_from_source(
                &witness.target_ty,
                &witness.source_ty,
                &source_clone,
                1,
            )?;
            self.line_indent(1, &format!("{target} = {target_value};"));
            self.line_indent(
                1,
                &format!("{result_temp}.tag = {};", result_layout.ok_index),
            );
        } else {
            self.line_indent(
                1,
                &format!(
                    "{result_temp} = {};",
                    self.result_ok_literal(&result_layout, None)
                ),
            );
        }
        self.line_indent(1, &format!("{done_label}:;"));
        self.line_indent(1, &format!("return {result_temp};"));
        self.line("}");
        Ok(())
    }

    fn emit_retained_closure_clone_source_value(
        &mut self,
        witness: &RetainedClosureWitness,
        result_temp: &str,
        result_layout: &ResultLayout,
        done_label: &str,
        indent: usize,
    ) -> DiagResult<String> {
        let source_ptr = self.retained_closure_source_pointer_expr(witness);
        let source_temp = self.next_temp("retained_clone_source");
        self.line_indent(
            indent,
            &format!("{};", self.c_decl(&witness.source_ty, &source_temp)),
        );
        match &witness.source_ty {
            Ty::Function {
                is_unsafe: false,
                abi: None,
                ..
            } => {
                self.line_indent(indent, &format!("{source_temp} = *({source_ptr});"));
            }
            Ty::Closure { constraints, .. }
                if constraints
                    .positive
                    .iter()
                    .any(|capability| self.is_std_clone_message_capability(capability)) =>
            {
                let capability = clone_message_capability(
                    self.std_message_interface_def(STD_MESSAGE_CLONE_INTERFACE),
                );
                let field = self.retained_closure_witness_field_name(&capability);
                let clone_result_ty = std_result_ty(witness.source_ty.clone(), std_error_ty());
                let clone_layout = self.result_layout(&clone_result_ty, witness.span)?;
                let clone_temp = self.next_temp("retained_source_clone");
                self.line_indent(
                    indent,
                    &format!(
                        "{} {clone_temp} = (*({source_ptr})).{field}((void *)({source_ptr}));",
                        clone_layout.c_type
                    ),
                );
                self.line_indent(
                    indent,
                    &format!("if ({clone_temp}.tag == {}) {{", clone_layout.err_index),
                );
                self.line_indent(
                    indent + 1,
                    &format!(
                        "{result_temp} = {};",
                        self.result_err_literal(result_layout, &clone_layout, &clone_temp)
                    ),
                );
                self.line_indent(indent + 1, &format!("goto {done_label};"));
                self.line_indent(indent, "}");
                self.line_indent(
                    indent,
                    &format!(
                        "{source_temp} = {clone_temp}.as.{}._0;",
                        clone_layout.ok_name
                    ),
                );
            }
            Ty::ClosureInstance { .. } => {
                let Some(function_def) = self
                    .clone_message_impl(&witness.source_ty)
                    .map(|implementation| implementation.function_def)
                else {
                    return Err(vec![Diagnostic::new(
                        witness.span,
                        format!(
                            "internal error: missing clone_message implementation for `{}`",
                            witness.source_ty
                        ),
                    )]);
                };
                let clone_result_ty = std_result_ty(witness.source_ty.clone(), std_error_ty());
                let clone_layout = self.result_layout(&clone_result_ty, witness.span)?;
                let clone_temp = self.next_temp("retained_source_clone");
                self.line_indent(
                    indent,
                    &format!(
                        "{} {clone_temp} = {}({source_ptr});",
                        clone_layout.c_type,
                        self.c_name(function_def)
                    ),
                );
                self.line_indent(
                    indent,
                    &format!("if ({clone_temp}.tag == {}) {{", clone_layout.err_index),
                );
                self.line_indent(
                    indent + 1,
                    &format!(
                        "{result_temp} = {};",
                        self.result_err_literal(result_layout, &clone_layout, &clone_temp)
                    ),
                );
                self.line_indent(indent + 1, &format!("goto {done_label};"));
                self.line_indent(indent, "}");
                self.line_indent(
                    indent,
                    &format!(
                        "{source_temp} = {clone_temp}.as.{}._0;",
                        clone_layout.ok_name
                    ),
                );
            }
            other => {
                return Err(vec![Diagnostic::new(
                    witness.span,
                    format!("internal error: cannot clone retained closure source type `{other}`"),
                )]);
            }
        }
        Ok(source_temp)
    }
}
