use super::*;

impl<'a> CGenerator<'a> {
    pub(super) fn emit_meta_owned_leaf_repr_expr(
        &mut self,
        span: crate::span::Span,
        ty: &Ty,
        value_expr: &str,
        root_ty: &Ty,
        indent: usize,
    ) -> DiagResult<(Ty, String)> {
        if let Ty::Named { name, args } = ty
            && matches!(meta_repr_marker_name(name), Some(false))
            && args.len() == 1
        {
            let repr_ty = self.meta_owned_leaf_repr_ty(span, &args[0], root_ty)?;
            return Ok((
                repr_ty.clone(),
                self.value_or_initializer_from_expr(&repr_ty, value_expr),
            ));
        }
        if self.is_owned_meta_policy_leaf(ty, root_ty) {
            let leaf_ty = self.meta_repr_policy_leaf_ty(ty);
            return Ok((
                leaf_ty.clone(),
                self.value_or_initializer_from_expr(&leaf_ty, value_expr),
            ));
        }
        match ty {
            Ty::Array { len, elem } => {
                self.emit_meta_array_repr_literal(span, *len, elem, value_expr, root_ty, indent)
            }
            Ty::Named { .. } => {
                if let Ok(fields) = self.struct_fields_for_ty(span, ty) {
                    let mut repr_fields = Vec::new();
                    for (name, field_ty) in fields {
                        let (repr_ty, repr_expr) = self.emit_meta_owned_leaf_repr_expr(
                            span,
                            &field_ty,
                            &format!("({value_expr}).{name}"),
                            root_ty,
                            indent,
                        )?;
                        repr_fields.push(MetaProductField {
                            value_expr: repr_expr,
                            name,
                            ty: repr_ty,
                        });
                    }
                    let (repr_ty, literal) =
                        self.meta_named_product_literal(&repr_fields, "Field")?;
                    return Ok((repr_ty, literal));
                }
                if let Ok(variants) = self.enum_variants_for_ty(span, ty) {
                    let repr_ty = self.meta_owned_sum_ty(span, &variants, root_ty)?;
                    let literal = self.emit_meta_enum_owned_repr_value(
                        span, &repr_ty, value_expr, &variants, root_ty, indent,
                    )?;
                    return Ok((repr_ty, literal));
                }
                Err(vec![Diagnostic::new(
                    span,
                    format!("internal error: unsupported owned meta leaf `{ty}`"),
                )])
            }
            Ty::ClosureInstance { .. } => {
                let repr_ty = self.meta_owned_closure_repr_ty(span, ty, root_ty)?;
                let literal =
                    self.emit_meta_closure_owned_repr_value(span, ty, value_expr, root_ty, indent)?;
                Ok((repr_ty, literal))
            }
            other => Ok((
                other.clone(),
                self.value_or_initializer_from_expr(ty, value_expr),
            )),
        }
    }

    pub(super) fn emit_meta_array_repr_literal(
        &mut self,
        span: crate::span::Span,
        len: usize,
        elem: &Ty,
        source_expr: &str,
        root_ty: &Ty,
        indent: usize,
    ) -> DiagResult<(Ty, String)> {
        let repr_ty = self.meta_owned_array_repr_ty(span, len, elem, root_ty)?;
        if len == 0 {
            return Ok((repr_ty.clone(), format!("({}){{0}}", self.c_type(&repr_ty))));
        }
        if len <= crate::types::META_ARRAY_CHUNK_SIZE {
            let mut fields = Vec::new();
            for idx in 0..len {
                if elem.is_erased_value() {
                    continue;
                }
                let (_, item) = self.emit_meta_owned_leaf_repr_expr(
                    span,
                    elem,
                    &format!("({source_expr})[{idx}]"),
                    root_ty,
                    indent,
                )?;
                fields.push(format!(".item{idx} = {item}"));
            }
            let literal = if fields.is_empty() {
                format!("({}){{0}}", self.c_type(&repr_ty))
            } else {
                format!("({}){{ {} }}", self.c_type(&repr_ty), fields.join(", "))
            };
            return Ok((repr_ty, literal));
        }
        let split = meta_array_split_len(len);
        let (_, left) =
            self.emit_meta_array_repr_literal(span, split, elem, source_expr, root_ty, indent)?;
        let (_, right) = self.emit_meta_array_repr_literal(
            span,
            len - split,
            elem,
            &format!("({source_expr}) + {split}"),
            root_ty,
            indent,
        )?;
        Ok((
            repr_ty.clone(),
            format!(
                "({}){{ .left = {left}, .right = {right} }}",
                self.c_type(&repr_ty)
            ),
        ))
    }

    pub(super) fn emit_meta_array_ref_repr_literal(
        &self,
        len: usize,
        elem: &Ty,
        source_ptr: &str,
        base_index: usize,
    ) -> DiagResult<(Ty, String)> {
        let repr_ty = meta_ref_array_repr_ty(len, elem);
        if len == 0 {
            return Ok((repr_ty.clone(), format!("({}){{0}}", self.c_type(&repr_ty))));
        }
        if len <= crate::types::META_ARRAY_CHUNK_SIZE {
            let fields = (0..len)
                .map(|idx| format!(".item{idx} = &((*({source_ptr}))[{}])", base_index + idx))
                .collect::<Vec<_>>();
            return Ok((
                repr_ty.clone(),
                format!("({}){{ {} }}", self.c_type(&repr_ty), fields.join(", ")),
            ));
        }
        let split = meta_array_split_len(len);
        let (_, left) =
            self.emit_meta_array_ref_repr_literal(split, elem, source_ptr, base_index)?;
        let (_, right) = self.emit_meta_array_ref_repr_literal(
            len - split,
            elem,
            source_ptr,
            base_index + split,
        )?;
        Ok((
            repr_ty.clone(),
            format!(
                "({}){{ .left = {left}, .right = {right} }}",
                self.c_type(&repr_ty)
            ),
        ))
    }

    pub(in crate::codegen) fn meta_owned_leaf_repr_ty(
        &self,
        span: crate::span::Span,
        ty: &Ty,
        root_ty: &Ty,
    ) -> DiagResult<Ty> {
        if let Ty::Named { name, args } = ty
            && matches!(meta_repr_marker_name(name), Some(false))
            && args.len() == 1
        {
            return self.meta_owned_leaf_repr_ty(span, &args[0], root_ty);
        }
        if self.is_owned_meta_policy_leaf(ty, root_ty) {
            return Ok(self.meta_repr_policy_leaf_ty(ty));
        }
        match ty {
            Ty::Array { len, elem } => self.meta_owned_array_repr_ty(span, *len, elem, root_ty),
            Ty::Named { .. } => {
                if let Ok(fields) = self.struct_fields_for_ty(span, ty) {
                    return Ok(meta_product_ty(
                        fields
                            .iter()
                            .map(|(_, field_ty)| {
                                self.meta_owned_leaf_repr_ty(span, field_ty, root_ty)
                                    .unwrap_or(Ty::Unknown)
                            })
                            .collect::<Vec<_>>(),
                        "Field",
                    ));
                }
                if let Ok(variants) = self.enum_variants_for_ty(span, ty) {
                    return self.meta_owned_sum_ty(span, &variants, root_ty);
                }
                Err(vec![Diagnostic::new(
                    span,
                    format!("internal error: unsupported owned meta leaf `{ty}`"),
                )])
            }
            Ty::ClosureInstance { .. } => self.meta_owned_closure_repr_ty(span, ty, root_ty),
            other => Ok(other.clone()),
        }
    }

    pub(super) fn meta_owned_array_repr_ty(
        &self,
        span: crate::span::Span,
        len: usize,
        elem: &Ty,
        root_ty: &Ty,
    ) -> DiagResult<Ty> {
        if len == 0 {
            return Ok(meta_named("ArrayNil", Vec::new()));
        }
        if len <= crate::types::META_ARRAY_CHUNK_SIZE {
            return Ok(meta_named(
                &format!("ArrayChunk{len}"),
                vec![self.meta_owned_leaf_repr_ty(span, elem, root_ty)?],
            ));
        }
        let split = meta_array_split_len(len);
        Ok(meta_named(
            "ArrayCat",
            vec![
                self.meta_owned_array_repr_ty(span, split, elem, root_ty)?,
                self.meta_owned_array_repr_ty(span, len - split, elem, root_ty)?,
            ],
        ))
    }

    pub(super) fn meta_owned_sum_ty(
        &self,
        span: crate::span::Span,
        variants: &[CheckedVariant],
        root_ty: &Ty,
    ) -> DiagResult<Ty> {
        if variants.is_empty() {
            return Ok(meta_named("CoNil", Vec::new()));
        }
        let variant_tys = variants
            .iter()
            .map(|variant| {
                variant
                    .payload
                    .iter()
                    .map(|payload| {
                        self.meta_owned_leaf_repr_ty(span, payload, root_ty)
                            .unwrap_or(Ty::Unknown)
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        Ok(meta_sum_ty(variant_tys, false))
    }

    pub(super) fn meta_owned_closure_repr_ty(
        &self,
        span: crate::span::Span,
        ty: &Ty,
        root_ty: &Ty,
    ) -> DiagResult<Ty> {
        let captures = self.meta_capture_fields_for_ty(span, ty)?;
        Ok(meta_product_ty(
            captures
                .iter()
                .map(|capture| {
                    self.meta_owned_leaf_repr_ty(span, &capture.ty, root_ty)
                        .unwrap_or(Ty::Unknown)
                })
                .collect::<Vec<_>>(),
            "Field",
        ))
    }

    pub(in crate::codegen) fn meta_borrowed_repr_ty(
        &self,
        span: crate::span::Span,
        ty: &Ty,
    ) -> DiagResult<Ty> {
        match ty {
            Ty::Array { len, elem } => Ok(meta_ref_array_repr_ty(*len, elem)),
            Ty::Named { .. } => {
                if let Ok(fields) = self.struct_fields_for_ty(span, ty) {
                    return Ok(meta_product_ty(
                        fields
                            .iter()
                            .map(|(_, field_ty)| meta_repr_borrowed_array_leaf_ty(field_ty))
                            .collect::<Vec<_>>(),
                        "FieldRef",
                    ));
                }
                if let Ok(variants) = self.enum_variants_for_ty(span, ty) {
                    let variant_tys = variants
                        .iter()
                        .map(|variant| {
                            variant
                                .payload
                                .iter()
                                .map(meta_repr_borrowed_array_leaf_ty)
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>();
                    return Ok(meta_sum_ty(variant_tys, true));
                }
                Err(vec![Diagnostic::new(
                    span,
                    format!("internal error: unsupported borrowed meta leaf `{ty}`"),
                )])
            }
            Ty::ClosureInstance { .. } => {
                let captures = self.meta_capture_fields_for_ty(span, ty)?;
                Ok(meta_product_ty(
                    captures
                        .iter()
                        .map(|capture| meta_repr_borrowed_array_leaf_ty(&capture.ty))
                        .collect::<Vec<_>>(),
                    "FieldRef",
                ))
            }
            other => Ok(other.clone()),
        }
    }

    pub(in crate::codegen) fn meta_schema_ty(
        &self,
        span: crate::span::Span,
        ty: &Ty,
    ) -> DiagResult<Ty> {
        match ty {
            Ty::Array { len, elem } => self.meta_schema_array_ty(span, *len, elem, ty),
            Ty::Named { name, args } if meta_schema_marker_name(name) && args.len() == 1 => {
                self.meta_schema_ty(span, &args[0])
            }
            Ty::Named { .. } => {
                if let Ok(fields) = self.struct_fields_for_ty(span, ty) {
                    return Ok(meta_schema_product_ty(
                        fields
                            .iter()
                            .map(|(_, field_ty)| {
                                (
                                    field_ty.clone(),
                                    self.meta_owned_leaf_repr_ty(span, field_ty, ty)
                                        .unwrap_or(Ty::Unknown),
                                )
                            })
                            .collect::<Vec<_>>(),
                    ));
                }
                if let Ok(variants) = self.enum_variants_for_ty(span, ty) {
                    return Ok(meta_schema_sum_ty(variants.iter().map(|variant| {
                        variant
                            .payload
                            .iter()
                            .map(|payload| {
                                (
                                    payload.clone(),
                                    self.meta_owned_leaf_repr_ty(span, payload, ty)
                                        .unwrap_or(Ty::Unknown),
                                )
                            })
                            .collect::<Vec<_>>()
                    })));
                }
                Err(vec![Diagnostic::new(
                    span,
                    format!("internal error: unsupported meta schema source `{ty}`"),
                )])
            }
            Ty::ClosureInstance { .. } => {
                let captures = self.meta_capture_fields_for_ty(span, ty)?;
                Ok(meta_schema_product_ty(
                    captures
                        .iter()
                        .map(|capture| {
                            (
                                capture.ty.clone(),
                                self.meta_owned_leaf_repr_ty(span, &capture.ty, ty)
                                    .unwrap_or(Ty::Unknown),
                            )
                        })
                        .collect::<Vec<_>>(),
                ))
            }
            other => Err(vec![Diagnostic::new(
                span,
                format!("internal error: unsupported meta schema source `{other}`"),
            )]),
        }
    }

    pub(super) fn meta_schema_array_ty(
        &self,
        span: crate::span::Span,
        len: usize,
        elem: &Ty,
        root_ty: &Ty,
    ) -> DiagResult<Ty> {
        if len == 0 {
            return Ok(meta_named("ArrayNil", Vec::new()));
        }
        if len <= crate::types::META_ARRAY_CHUNK_SIZE {
            let repr_ty = self.meta_owned_leaf_repr_ty(span, elem, root_ty)?;
            let elem_schema = meta_named("ElementSchema", vec![elem.clone(), repr_ty]);
            return Ok(meta_named(&format!("ArrayChunk{len}"), vec![elem_schema]));
        }
        let split = meta_array_split_len(len);
        Ok(meta_named(
            "ArrayCat",
            vec![
                self.meta_schema_array_ty(span, split, elem, root_ty)?,
                self.meta_schema_array_ty(span, len - split, elem, root_ty)?,
            ],
        ))
    }

    pub(super) fn meta_array_repr_item_expr(
        &self,
        len: usize,
        elem: &Ty,
        repr_expr: &str,
        index: usize,
    ) -> String {
        debug_assert!(index < len);
        if len <= crate::types::META_ARRAY_CHUNK_SIZE {
            return format!("({repr_expr}).item{index}");
        }
        let split = meta_array_split_len(len);
        if index < split {
            self.meta_array_repr_item_expr(split, elem, &format!("({repr_expr}).left"), index)
        } else {
            self.meta_array_repr_item_expr(
                len - split,
                elem,
                &format!("({repr_expr}).right"),
                index - split,
            )
        }
    }

    pub(super) fn emit_meta_value_from_repr_into(
        &mut self,
        span: crate::span::Span,
        target: &str,
        ty: &Ty,
        repr_expr: &str,
        root_ty: &Ty,
        indent: usize,
    ) -> DiagResult<()> {
        if let Ty::Named { name, args } = ty
            && matches!(meta_repr_marker_name(name), Some(false))
            && args.len() == 1
        {
            let repr_ty = self.meta_owned_leaf_repr_ty(span, &args[0], root_ty)?;
            self.emit_value_copy(target, repr_expr, &repr_ty, indent);
            return Ok(());
        }
        if self.is_owned_meta_policy_leaf(ty, root_ty) {
            let leaf_ty = self.meta_repr_policy_leaf_ty(ty);
            self.emit_value_copy(target, repr_expr, &leaf_ty, indent);
            return Ok(());
        }
        match ty {
            Ty::Array { len, elem } => {
                for idx in 0..*len {
                    if elem.is_erased_value() {
                        continue;
                    }
                    let item = self.meta_array_repr_item_expr(*len, elem, repr_expr, idx);
                    self.emit_meta_value_from_repr_into(
                        span,
                        &format!("({target})[{idx}]"),
                        elem,
                        &item,
                        root_ty,
                        indent,
                    )?;
                }
                Ok(())
            }
            Ty::Named { .. } => {
                if let Ok(fields) = self.struct_fields_for_ty(span, ty) {
                    let mut cursor = repr_expr.to_string();
                    for (field, field_ty) in fields {
                        let head = format!("({cursor}).head");
                        if !field_ty.is_erased_value() {
                            self.emit_meta_value_from_repr_into(
                                span,
                                &format!("({target}).{field}"),
                                &field_ty,
                                &format!("{head}.value"),
                                root_ty,
                                indent,
                            )?;
                        }
                        cursor = format!("({cursor}).tail");
                    }
                    return Ok(());
                }
                if let Ok(variants) = self.enum_variants_for_ty(span, ty) {
                    return self.emit_meta_enum_from_repr_into(
                        span, &variants, 0, repr_expr, ty, target, root_ty, indent,
                    );
                }
                Err(vec![Diagnostic::new(
                    span,
                    format!("internal error: unsupported owned meta target `{ty}`"),
                )])
            }
            Ty::ClosureInstance { .. } => {
                let value =
                    self.emit_meta_closure_from_repr_value(span, ty, repr_expr, root_ty, indent)?;
                self.emit_value_copy(target, &value, ty, indent);
                Ok(())
            }
            _ => {
                self.emit_value_copy(target, repr_expr, ty, indent);
                Ok(())
            }
        }
    }

    pub(super) fn is_owned_meta_policy_leaf(&self, ty: &Ty, root_ty: &Ty) -> bool {
        let leaf_ty = self.meta_repr_policy_leaf_ty(ty);
        let is_thread_local = self.type_matches_thread_local_template(&leaf_ty)
            || self.thread_local_impl(&leaf_ty).is_some();
        if ty == root_ty && !is_thread_local {
            return false;
        }
        matches!(ty, Ty::Named { .. })
            && (is_thread_local
                || self.type_matches_share_handle_template(&leaf_ty)
                || self.share_handle_impl(&leaf_ty).is_some()
                || self.clone_message_impl(&leaf_ty).is_some())
    }

    pub(super) fn meta_repr_policy_leaf_ty(&self, ty: &Ty) -> Ty {
        match ty {
            Ty::Named { name, args } => Ty::Named {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.preserve_meta_repr_markers(arg))
                    .collect(),
            },
            _ => ty.clone(),
        }
    }

    pub(super) fn preserve_meta_repr_markers(&self, ty: &Ty) -> Ty {
        match ty {
            Ty::Pointer {
                nullable,
                mutability,
                inner,
            } => Ty::Pointer {
                nullable: *nullable,
                mutability: *mutability,
                inner: Box::new(self.preserve_meta_repr_markers(inner)),
            },
            Ty::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(self.preserve_meta_repr_markers(elem)),
            },
            Ty::Slice { mutability, elem } => Ty::Slice {
                mutability: *mutability,
                elem: Box::new(self.preserve_meta_repr_markers(elem)),
            },
            Ty::Named { name, args } => {
                if let Some(borrowed) = meta_repr_marker_name(name) {
                    if args.len() == 1 {
                        return std_meta_repr_marker_ty(borrowed, args[0].clone());
                    }
                }
                if meta_schema_marker_name(name) && args.len() == 1 {
                    return std_meta_schema_marker_ty(args[0].clone());
                }
                Ty::Named {
                    name: name.clone(),
                    args: args
                        .iter()
                        .map(|arg| self.preserve_meta_repr_markers(arg))
                        .collect(),
                }
            }
            Ty::DynamicInterface { def_id, name, args } => Ty::DynamicInterface {
                def_id: *def_id,
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.preserve_meta_repr_markers(arg))
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
                ret: Box::new(self.preserve_meta_repr_markers(ret)),
                params: params
                    .iter()
                    .map(|param| self.preserve_meta_repr_markers(param))
                    .collect(),
            },
            Ty::Closure {
                ret,
                params,
                constraints,
            } => Ty::Closure {
                ret: Box::new(self.preserve_meta_repr_markers(ret)),
                params: params
                    .iter()
                    .map(|param| self.preserve_meta_repr_markers(param))
                    .collect(),
                constraints: constraints.clone(),
            },
            Ty::ClosureInstance {
                id,
                ret,
                params,
                captures,
            } => Ty::ClosureInstance {
                id: *id,
                ret: Box::new(self.preserve_meta_repr_markers(ret)),
                params: params
                    .iter()
                    .map(|param| self.preserve_meta_repr_markers(param))
                    .collect(),
                captures: captures
                    .iter()
                    .map(|capture| self.preserve_meta_repr_markers(capture))
                    .collect(),
            },
            other => other.clone(),
        }
    }

    pub(super) fn type_matches_share_handle_template(&self, ty: &Ty) -> bool {
        self.share_handle_templates.iter().any(|pattern| {
            let mut subst = HashMap::new();
            unify_ty(pattern, ty, &mut subst)
        })
    }

    pub(super) fn type_matches_thread_local_template(&self, ty: &Ty) -> bool {
        self.thread_local_templates.iter().any(|pattern| {
            let mut subst = HashMap::new();
            unify_ty(pattern, ty, &mut subst)
        })
    }

    pub(super) fn type_matches_meta_policy_marker(&self, ty: &Ty) -> bool {
        let leaf_ty = self.meta_repr_policy_leaf_ty(ty);
        self.type_matches_share_handle_template(&leaf_ty)
            || self.share_handle_impl(&leaf_ty).is_some()
            || self.type_matches_thread_local_template(&leaf_ty)
            || self.thread_local_impl(&leaf_ty).is_some()
    }

    pub(super) fn emit_meta_enum_ref_repr(
        &mut self,
        expr: &TExpr,
        source_ptr: &str,
        variants: &[CheckedVariant],
        indent: usize,
    ) -> DiagResult<String> {
        let result = self.next_temp("meta_ref_repr");
        self.line_indent(indent, &format!("{};", self.c_decl(&expr.ty, &result)));
        self.line_indent(indent, &format!("switch (({source_ptr})->tag) {{"));
        for idx in 0..variants.len() {
            let (_, literal) = self.meta_ref_sum_branch_literal(variants, idx, source_ptr)?;
            self.line_indent(indent + 1, &format!("case {idx}:"));
            self.line_indent(indent + 2, &format!("{result} = {literal};"));
            self.line_indent(indent + 2, "break;");
        }
        self.line_indent(indent + 1, "default:");
        self.line_indent(indent + 2, "ciel_panic(NULL, 0);");
        self.line_indent(
            indent + 2,
            &format!("{result} = {};", self.zero_value(&expr.ty)),
        );
        self.line_indent(indent + 2, "break;");
        self.line_indent(indent, "}");
        Ok(result)
    }

    pub(super) fn emit_meta_enum_owned_repr_value(
        &mut self,
        span: crate::span::Span,
        repr_ty: &Ty,
        source_value: &str,
        variants: &[CheckedVariant],
        root_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let result = self.next_temp("meta_owned_repr");
        self.line_indent(indent, &format!("{};", self.c_decl(repr_ty, &result)));
        self.line_indent(indent, &format!("switch (({source_value}).tag) {{"));
        for idx in 0..variants.len() {
            self.line_indent(indent + 1, &format!("case {idx}: {{"));
            let (_, literal) = self.meta_owned_sum_branch_literal(
                span,
                variants,
                idx,
                source_value,
                root_ty,
                indent + 2,
            )?;
            self.line_indent(indent + 2, &format!("{result} = {literal};"));
            self.line_indent(indent + 2, "break;");
            self.line_indent(indent + 1, "}");
        }
        self.line_indent(indent + 1, "default:");
        self.line_indent(indent + 2, "ciel_panic(NULL, 0);");
        self.line_indent(
            indent + 2,
            &format!("{result} = {};", self.zero_value(repr_ty)),
        );
        self.line_indent(indent + 2, "break;");
        self.line_indent(indent, "}");
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_meta_enum_from_repr_into(
        &mut self,
        span: crate::span::Span,
        variants: &[CheckedVariant],
        variant_offset: usize,
        cursor: &str,
        target_ty: &Ty,
        result: &str,
        root_ty: &Ty,
        indent: usize,
    ) -> DiagResult<()> {
        if variants.is_empty() {
            self.line_indent(indent, "ciel_panic(NULL, 0);");
            self.line_indent(
                indent,
                &format!("{result} = {};", self.zero_value(target_ty)),
            );
            return Ok(());
        }
        self.line_indent(indent, &format!("switch (({cursor}).tag) {{"));
        self.line_indent(indent + 1, "case 0:");
        self.emit_meta_enum_variant_from_repr_into(
            span,
            target_ty,
            &variants[0],
            variant_offset,
            cursor,
            result,
            root_ty,
            indent + 2,
        )?;
        self.line_indent(indent + 2, "break;");
        self.line_indent(indent + 1, "case 1:");
        if variants.len() == 1 {
            self.line_indent(indent + 2, "ciel_panic(NULL, 0);");
            self.line_indent(
                indent + 2,
                &format!("{result} = {};", self.zero_value(target_ty)),
            );
        } else {
            let tail = format!("({cursor}).as.Next._0");
            self.emit_meta_enum_from_repr_into(
                span,
                &variants[1..],
                variant_offset + 1,
                &tail,
                target_ty,
                result,
                root_ty,
                indent + 2,
            )?;
        }
        self.line_indent(indent + 2, "break;");
        self.line_indent(indent + 1, "default:");
        self.line_indent(indent + 2, "ciel_panic(NULL, 0);");
        self.line_indent(
            indent + 2,
            &format!("{result} = {};", self.zero_value(target_ty)),
        );
        self.line_indent(indent + 2, "break;");
        self.line_indent(indent, "}");
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_meta_enum_variant_from_repr_into(
        &mut self,
        span: crate::span::Span,
        _target_ty: &Ty,
        variant: &CheckedVariant,
        variant_index: usize,
        cursor: &str,
        target: &str,
        root_ty: &Ty,
        indent: usize,
    ) -> DiagResult<()> {
        self.line_indent(indent, &format!("({target}).tag = {variant_index};"));
        let mut payload_cursor = format!("(({cursor}).as.This._0).payload");
        for (idx, ty) in variant.payload.iter().enumerate() {
            if !ty.is_erased_value() {
                self.emit_meta_value_from_repr_into(
                    span,
                    &format!("({target}).as.{}._{idx}", variant.name),
                    ty,
                    &format!("({payload_cursor}).head.value"),
                    root_ty,
                    indent,
                )?;
            }
            payload_cursor = format!("({payload_cursor}).tail");
        }
        Ok(())
    }

    pub(super) fn emit_meta_closure_ref_repr(
        &mut self,
        expr: &TExpr,
        source_ptr: &str,
        source_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let captures = self.meta_capture_fields_for_ty(expr.span, source_ty)?;
        if captures.is_empty() {
            let (_, literal) = self.meta_named_product_literal(&[], "FieldRef")?;
            return Ok(literal);
        }
        let (owner, id) = self.closure_instance_owner_id(expr.span, source_ty)?;
        let env_name = self.closure_env_name(owner, id);
        let env_temp = self.next_temp("meta_ref_env");
        self.line_indent(
            indent,
            &format!("{env_name} *{env_temp} = ({env_name} *)({source_ptr})->env;"),
        );
        let fields = captures
            .into_iter()
            .map(|capture| MetaProductField {
                name: format!("capture#{}", capture.index),
                ty: capture.ty,
                value_expr: format!("&({env_temp})->cap{}", capture.index),
            })
            .collect::<Vec<_>>();
        let (_, literal) = self.meta_named_product_literal(&fields, "FieldRef")?;
        Ok(literal)
    }

    pub(super) fn emit_meta_closure_owned_repr_value(
        &mut self,
        span: crate::span::Span,
        source_ty: &Ty,
        source_value: &str,
        root_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let captures = self.meta_capture_fields_for_ty(span, source_ty)?;
        if captures.is_empty() {
            let (_, literal) = self.meta_named_product_literal(&[], "Field")?;
            return Ok(literal);
        }
        let (owner, id) = self.closure_instance_owner_id(span, source_ty)?;
        let env_name = self.closure_env_name(owner, id);
        let env_temp = self.next_temp("meta_owned_env");
        self.line_indent(
            indent,
            &format!("{env_name} *{env_temp} = ({env_name} *)({source_value}).env;"),
        );
        let mut fields = Vec::new();
        for capture in captures {
            let (ty, value_expr) = self.emit_meta_owned_leaf_repr_expr(
                span,
                &capture.ty,
                &format!("({env_temp})->cap{}", capture.index),
                root_ty,
                indent,
            )?;
            fields.push(MetaProductField {
                name: format!("capture#{}", capture.index),
                ty,
                value_expr,
            });
        }
        let (_, literal) = self.meta_named_product_literal(&fields, "Field")?;
        Ok(literal)
    }

    pub(super) fn emit_meta_closure_from_repr_value(
        &mut self,
        span: crate::span::Span,
        target_ty: &Ty,
        value_temp: &str,
        root_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        let captures = self.meta_capture_fields_for_ty(span, target_ty)?;
        let (owner, id) = self.closure_instance_owner_id(span, target_ty)?;
        if captures.is_empty() {
            return Ok(format!(
                "({}){{ .call = {}, .env = NULL }}",
                self.c_type(target_ty),
                self.closure_thunk_name(owner, id)
            ));
        }
        let env_name = self.closure_env_name(owner, id);
        let env_temp = self.next_temp("meta_closure_env");
        self.line_indent(
            indent,
            &format!("{env_name} *{env_temp} = ({env_name} *)ciel_alloc(sizeof({env_name}));"),
        );
        let mut cursor = value_temp.to_string();
        for capture in captures {
            self.emit_meta_value_from_repr_into(
                span,
                &format!("{env_temp}->cap{}", capture.index),
                &capture.ty,
                &format!("({cursor}).head.value"),
                root_ty,
                indent,
            )?;
            cursor = format!("({cursor}).tail");
        }
        Ok(format!(
            "({}){{ .call = {}, .env = (void *){} }}",
            self.c_type(target_ty),
            self.closure_thunk_name(owner, id),
            env_temp
        ))
    }

    pub(super) fn emit_meta_schema_expr(
        &mut self,
        expr: &TExpr,
        source_ty: &Ty,
        indent: usize,
    ) -> DiagResult<String> {
        if let Ty::Array { len, elem } = source_ty {
            let (_, literal) =
                self.emit_meta_array_schema_literal(expr.span, *len, elem, source_ty, indent)?;
            return Ok(literal);
        }
        if let Ok(fields) = self.struct_fields_for_ty(expr.span, source_ty) {
            let mut schema_fields = Vec::new();
            for (name, field_ty) in fields {
                let repr_ty = self.meta_owned_leaf_repr_ty(expr.span, &field_ty, source_ty)?;
                schema_fields.push(MetaSchemaField {
                    name,
                    source_ty: field_ty,
                    repr_ty,
                });
            }
            let (_, literal) = self.meta_schema_product_literal(&schema_fields)?;
            return Ok(literal);
        }
        if let Ok(variants) = self.enum_variants_for_ty(expr.span, source_ty) {
            let (_, literal) = self.meta_schema_sum_literal(expr.span, &variants, source_ty)?;
            return Ok(literal);
        }
        if matches!(source_ty, Ty::ClosureInstance { .. }) {
            return self.emit_meta_closure_schema(expr, source_ty);
        }
        Err(vec![Diagnostic::new(
            expr.span,
            format!("internal error: unsupported schema source `{source_ty}`"),
        )])
    }

    pub(super) fn emit_meta_closure_schema(
        &self,
        expr: &TExpr,
        source_ty: &Ty,
    ) -> DiagResult<String> {
        let captures = self.meta_capture_fields_for_ty(expr.span, source_ty)?;
        let fields = captures
            .into_iter()
            .map(|capture| {
                let repr_ty = self
                    .meta_owned_leaf_repr_ty(expr.span, &capture.ty, source_ty)
                    .unwrap_or(Ty::Unknown);
                MetaSchemaField {
                    name: format!("capture#{}", capture.index),
                    source_ty: capture.ty,
                    repr_ty,
                }
            })
            .collect::<Vec<_>>();
        let (_, literal) = self.meta_schema_product_literal(&fields)?;
        Ok(literal)
    }

    pub(super) fn emit_meta_array_schema_literal(
        &mut self,
        span: crate::span::Span,
        len: usize,
        elem: &Ty,
        root_ty: &Ty,
        indent: usize,
    ) -> DiagResult<(Ty, String)> {
        let schema_ty = self.meta_schema_array_ty(span, len, elem, root_ty)?;
        if len == 0 {
            return Ok((
                schema_ty.clone(),
                format!("({}){{0}}", self.c_type(&schema_ty)),
            ));
        }
        if len <= crate::types::META_ARRAY_CHUNK_SIZE {
            let repr_ty = self.meta_owned_leaf_repr_ty(span, elem, root_ty)?;
            let elem_literal = self.meta_element_schema_literal(elem, &repr_ty);
            let fields = (0..len)
                .map(|idx| format!(".item{idx} = {elem_literal}"))
                .collect::<Vec<_>>();
            return Ok((
                schema_ty.clone(),
                format!("({}){{ {} }}", self.c_type(&schema_ty), fields.join(", ")),
            ));
        }
        let split = meta_array_split_len(len);
        let (_, left) = self.emit_meta_array_schema_literal(span, split, elem, root_ty, indent)?;
        let (_, right) =
            self.emit_meta_array_schema_literal(span, len - split, elem, root_ty, indent)?;
        Ok((
            schema_ty.clone(),
            format!(
                "({}){{ .left = {left}, .right = {right} }}",
                self.c_type(&schema_ty)
            ),
        ))
    }

    pub(super) fn meta_type_witness_literal(&self, ty: &Ty) -> String {
        let witness_ty = meta_named("Type", vec![ty.clone()]);
        format!("({}){{0}}", self.c_type(&witness_ty))
    }

    pub(super) fn meta_element_schema_literal(&self, source_ty: &Ty, repr_ty: &Ty) -> String {
        let elem_schema_ty = meta_named("ElementSchema", vec![source_ty.clone(), repr_ty.clone()]);
        format!(
            "({}){{ .source_ty = {}, .repr_ty = {} }}",
            self.c_type(&elem_schema_ty),
            self.meta_type_witness_literal(source_ty),
            self.meta_type_witness_literal(repr_ty)
        )
    }

    pub(super) fn meta_schema_product_literal(
        &self,
        fields: &[MetaSchemaField],
    ) -> DiagResult<(Ty, String)> {
        let Some((field, rest)) = fields.split_first() else {
            let ty = meta_named("HNil", Vec::new());
            return Ok((ty.clone(), format!("({}){{0}}", self.c_type(&ty))));
        };
        let (tail_ty, tail) = self.meta_schema_product_literal(rest)?;
        let head_ty = meta_named(
            "FieldSchema",
            vec![field.source_ty.clone(), field.repr_ty.clone()],
        );
        let ty = meta_named("HCons", vec![head_ty.clone(), tail_ty]);
        let head = format!(
            "({}){{ .name = {}, .source_ty = {}, .repr_ty = {} }}",
            self.c_type(&head_ty),
            self.meta_name_slice_literal(&field.name),
            self.meta_type_witness_literal(&field.source_ty),
            self.meta_type_witness_literal(&field.repr_ty)
        );
        Ok((
            ty.clone(),
            format!("({}){{ .head = {head}, .tail = {tail} }}", self.c_type(&ty)),
        ))
    }

    pub(super) fn meta_schema_payload_literal(
        &self,
        fields: &[MetaSchemaPayload],
    ) -> DiagResult<(Ty, String)> {
        let Some((field, rest)) = fields.split_first() else {
            let ty = meta_named("HNil", Vec::new());
            return Ok((ty.clone(), format!("({}){{0}}", self.c_type(&ty))));
        };
        let (tail_ty, tail) = self.meta_schema_payload_literal(rest)?;
        let head_ty = meta_named(
            "PayloadSchema",
            vec![field.source_ty.clone(), field.repr_ty.clone()],
        );
        let ty = meta_named("HCons", vec![head_ty.clone(), tail_ty]);
        let head = format!(
            "({}){{ .index = {}, .source_ty = {}, .repr_ty = {} }}",
            self.c_type(&head_ty),
            field.index,
            self.meta_type_witness_literal(&field.source_ty),
            self.meta_type_witness_literal(&field.repr_ty)
        );
        Ok((
            ty.clone(),
            format!("({}){{ .head = {head}, .tail = {tail} }}", self.c_type(&ty)),
        ))
    }

    pub(super) fn meta_schema_sum_literal(
        &self,
        span: crate::span::Span,
        variants: &[CheckedVariant],
        root_ty: &Ty,
    ) -> DiagResult<(Ty, String)> {
        let Some((variant, rest)) = variants.split_first() else {
            let ty = meta_named("HNil", Vec::new());
            return Ok((ty.clone(), format!("({}){{0}}", self.c_type(&ty))));
        };
        let payloads = variant
            .payload
            .iter()
            .enumerate()
            .map(|(index, payload)| {
                let repr_ty = self
                    .meta_owned_leaf_repr_ty(span, payload, root_ty)
                    .unwrap_or(Ty::Unknown);
                MetaSchemaPayload {
                    index,
                    source_ty: payload.clone(),
                    repr_ty,
                }
            })
            .collect::<Vec<_>>();
        let (payload_ty, payload_literal) = self.meta_schema_payload_literal(&payloads)?;
        let head_ty = meta_named("VariantSchema", vec![payload_ty]);
        let (tail_ty, tail) = self.meta_schema_sum_literal(span, rest, root_ty)?;
        let ty = meta_named("HCons", vec![head_ty.clone(), tail_ty]);
        let head = format!(
            "({}){{ .name = {}, .payload = {payload_literal} }}",
            self.c_type(&head_ty),
            self.meta_name_slice_literal(&variant.name)
        );
        Ok((
            ty.clone(),
            format!("({}){{ .head = {head}, .tail = {tail} }}", self.c_type(&ty)),
        ))
    }

    pub(super) fn meta_named_product_literal(
        &self,
        fields: &[MetaProductField],
        head_name: &str,
    ) -> DiagResult<(Ty, String)> {
        let Some((field, rest)) = fields.split_first() else {
            let ty = meta_named("HNil", Vec::new());
            return Ok((ty.clone(), format!("({}){{0}}", self.c_type(&ty))));
        };
        let (tail_ty, tail) = self.meta_named_product_literal(rest, head_name)?;
        let head_ty = meta_named(head_name, vec![field.ty.clone()]);
        let ty = meta_named("HCons", vec![head_ty.clone(), tail_ty]);
        let mut head_fields = vec![format!(
            ".name = {}",
            self.meta_name_slice_literal(&field.name)
        )];
        if field.ty.is_erased_value() {
            if head_name == "FieldRef" {
                return Err(vec![Diagnostic::new(
                    None,
                    format!(
                        "internal error: cannot borrow erased meta field `{}`",
                        field.name
                    ),
                )]);
            }
        } else {
            let value = if head_name == "FieldRef" {
                field.value_expr.clone()
            } else {
                field.value_expr.clone()
            };
            head_fields.push(format!(".value = {}", value));
        }
        let head = format!(
            "({}){{ {} }}",
            self.c_type(&head_ty),
            head_fields.join(", ")
        );
        Ok((
            ty.clone(),
            format!("({}){{ .head = {head}, .tail = {tail} }}", self.c_type(&ty)),
        ))
    }

    pub(super) fn meta_payload_product_literal(
        &self,
        fields: &[MetaPayloadField],
        head_name: &str,
    ) -> DiagResult<(Ty, String)> {
        let Some((field, rest)) = fields.split_first() else {
            let ty = meta_named("HNil", Vec::new());
            return Ok((ty.clone(), format!("({}){{0}}", self.c_type(&ty))));
        };
        let (tail_ty, tail) = self.meta_payload_product_literal(rest, head_name)?;
        let head_ty = meta_named(head_name, vec![field.ty.clone()]);
        let ty = meta_named("HCons", vec![head_ty.clone(), tail_ty]);
        let value = if head_name == "PayloadRef" {
            field.value_expr.clone()
        } else {
            field.value_expr.clone()
        };
        let head = format!(
            "({}){{ .index = {}, .value = {} }}",
            self.c_type(&head_ty),
            field.index,
            value
        );
        Ok((
            ty.clone(),
            format!("({}){{ .head = {head}, .tail = {tail} }}", self.c_type(&ty)),
        ))
    }

    pub(super) fn meta_ref_sum_branch_literal(
        &self,
        variants: &[CheckedVariant],
        active_idx: usize,
        source_ptr: &str,
    ) -> DiagResult<(Ty, String)> {
        self.meta_sum_branch_literal(
            variants,
            active_idx,
            |variant| {
                variant
                    .payload
                    .iter()
                    .enumerate()
                    .map(|(idx, ty)| MetaPayloadField {
                        index: idx,
                        ty: ty.clone(),
                        value_expr: format!("&({source_ptr})->as.{}._{idx}", variant.name),
                    })
                    .collect::<Vec<_>>()
            },
            true,
        )
    }

    pub(super) fn meta_owned_sum_branch_literal(
        &mut self,
        span: crate::span::Span,
        variants: &[CheckedVariant],
        active_idx: usize,
        source_value: &str,
        root_ty: &Ty,
        indent: usize,
    ) -> DiagResult<(Ty, String)> {
        let Some((variant, rest)) = variants.split_first() else {
            return Err(vec![Diagnostic::new(
                None,
                "internal error: cannot construct meta CoNil branch",
            )]);
        };
        let payload_ty = meta_product_ty(
            variant.payload.iter().map(|payload| {
                self.meta_owned_leaf_repr_ty(span, payload, root_ty)
                    .unwrap_or(Ty::Unknown)
            }),
            "Payload",
        );
        let head_ty = meta_named("Variant", vec![payload_ty]);
        let tail_ty = self.meta_owned_sum_ty(span, rest, root_ty)?;
        let ty = meta_named("Coproduct", vec![head_ty.clone(), tail_ty]);
        if active_idx == 0 {
            let mut payloads = Vec::new();
            for (idx, ty) in variant.payload.iter().enumerate() {
                let (payload_ty, value_expr) = self.emit_meta_owned_leaf_repr_expr(
                    span,
                    ty,
                    &format!("({source_value}).as.{}._{idx}", variant.name),
                    root_ty,
                    indent,
                )?;
                payloads.push(MetaPayloadField {
                    index: idx,
                    ty: payload_ty,
                    value_expr,
                });
            }
            let (_, payload_literal) = self.meta_payload_product_literal(&payloads, "Payload")?;
            let head = format!(
                "({}){{ .name = {}, .payload = {payload_literal} }}",
                self.c_type(&head_ty),
                self.meta_name_slice_literal(&variant.name)
            );
            Ok((
                ty.clone(),
                format!(
                    "({}){{ .tag = 0, .as.This = {{ ._0 = {head} }} }}",
                    self.c_type(&ty)
                ),
            ))
        } else {
            let (_, tail_literal) = self.meta_owned_sum_branch_literal(
                span,
                rest,
                active_idx - 1,
                source_value,
                root_ty,
                indent,
            )?;
            Ok((
                ty.clone(),
                format!(
                    "({}){{ .tag = 1, .as.Next = {{ ._0 = {tail_literal} }} }}",
                    self.c_type(&ty)
                ),
            ))
        }
    }

    pub(super) fn meta_sum_branch_literal<F>(
        &self,
        variants: &[CheckedVariant],
        active_idx: usize,
        payloads_for: F,
        borrowed: bool,
    ) -> DiagResult<(Ty, String)>
    where
        F: Fn(&CheckedVariant) -> Vec<MetaPayloadField> + Copy,
    {
        let Some((variant, rest)) = variants.split_first() else {
            return Err(vec![Diagnostic::new(
                None,
                "internal error: cannot construct meta CoNil branch",
            )]);
        };
        let payload_head = if borrowed { "PayloadRef" } else { "Payload" };
        let variant_head = if borrowed { "VariantRef" } else { "Variant" };
        let payloads = payloads_for(variant);
        let (payload_ty, payload_literal) =
            self.meta_payload_product_literal(&payloads, payload_head)?;
        let head_ty = meta_named(variant_head, vec![payload_ty]);
        let tail_ty = meta_sum_ty(
            rest.iter()
                .map(|variant| variant.payload.iter().cloned().collect::<Vec<_>>()),
            borrowed,
        );
        let ty = meta_named("Coproduct", vec![head_ty.clone(), tail_ty]);
        if active_idx == 0 {
            let head = format!(
                "({}){{ .name = {}, .payload = {payload_literal} }}",
                self.c_type(&head_ty),
                self.meta_name_slice_literal(&variant.name)
            );
            Ok((
                ty.clone(),
                format!(
                    "({}){{ .tag = 0, .as.This = {{ ._0 = {head} }} }}",
                    self.c_type(&ty)
                ),
            ))
        } else {
            let (_, tail_literal) =
                self.meta_sum_branch_literal(rest, active_idx - 1, payloads_for, borrowed)?;
            Ok((
                ty.clone(),
                format!(
                    "({}){{ .tag = 1, .as.Next = {{ ._0 = {tail_literal} }} }}",
                    self.c_type(&ty)
                ),
            ))
        }
    }

    pub(super) fn meta_name_slice_literal(&self, name: &str) -> String {
        format!(
            "({}){{ .ptr = \"{}\", .len = {} }}",
            self.slice_name(ViewMutability::ReadOnly, &Ty::Char),
            escape_c_string(name),
            name.len()
        )
    }

    pub(super) fn struct_fields_for_ty(
        &self,
        span: crate::span::Span,
        ty: &Ty,
    ) -> DiagResult<Vec<(String, Ty)>> {
        if let Ty::OpaqueState { base, .. } = ty {
            return self.struct_fields_for_ty(span, base);
        }
        let Ty::Named { name, args } = ty else {
            return Err(vec![Diagnostic::new(
                span,
                format!("internal error: expected struct type for meta representation, got `{ty}`"),
            )]);
        };
        let c_name = self.c_named_type(name, args);
        self.program
            .checked
            .structs
            .iter()
            .find(|strukt| strukt.name == c_name)
            .map(|strukt| strukt.fields.clone())
            .ok_or_else(|| {
                vec![Diagnostic::new(
                    span,
                    format!(
                        "internal error: missing struct layout `{c_name}` for meta representation"
                    ),
                )]
            })
    }

    pub(super) fn enum_variants_for_ty(
        &self,
        span: crate::span::Span,
        ty: &Ty,
    ) -> DiagResult<Vec<CheckedVariant>> {
        if let Ty::OpaqueState { base, .. } = ty {
            return self.enum_variants_for_ty(span, base);
        }
        let Ty::Named { name, args } = ty else {
            return Err(vec![Diagnostic::new(
                span,
                format!("internal error: expected enum type for meta representation, got `{ty}`"),
            )]);
        };
        let c_name = self.c_named_type(name, args);
        self.program
            .checked
            .enums
            .iter()
            .find(|enm| enm.name == c_name)
            .map(|enm| enm.variants.clone())
            .ok_or_else(|| {
                vec![Diagnostic::new(
                    span,
                    format!(
                        "internal error: missing enum layout `{c_name}` for meta representation"
                    ),
                )]
            })
    }

    pub(super) fn meta_capture_fields_for_ty(
        &self,
        span: crate::span::Span,
        ty: &Ty,
    ) -> DiagResult<Vec<MetaCaptureField>> {
        if let Ty::OpaqueState { base, .. } = ty {
            return self.meta_capture_fields_for_ty(span, base);
        }
        let Ty::ClosureInstance { captures, .. } = ty else {
            return Err(vec![Diagnostic::new(
                span,
                format!(
                    "internal error: expected concrete closure type for meta representation, got `{ty}`"
                ),
            )]);
        };
        Ok(captures
            .iter()
            .enumerate()
            .filter(|(_, ty)| !ty.is_erased_value())
            .map(|(index, ty)| MetaCaptureField {
                index,
                ty: ty.clone(),
            })
            .collect())
    }

    pub(super) fn closure_instance_owner_id(
        &self,
        span: crate::span::Span,
        ty: &Ty,
    ) -> DiagResult<(DefId, usize)> {
        let Ty::ClosureInstance { id, .. } = ty else {
            return Err(vec![Diagnostic::new(
                span,
                format!("internal error: expected concrete closure type, got `{ty}`"),
            )]);
        };
        let matches = self
            .plan
            .closure_defs
            .values()
            .filter(|closure| closure.id == *id && &closure.ty == ty)
            .collect::<Vec<_>>();
        if let Some(owner) = self.current_closure_owner
            && let Some(closure) = matches.iter().find(|closure| closure.owner == owner)
        {
            return Ok((closure.owner, closure.id));
        }
        matches
            .first()
            .map(|closure| (closure.owner, closure.id))
            .ok_or_else(|| {
                vec![Diagnostic::new(
                    span,
                    format!("internal error: missing closure metadata for `{ty}`"),
                )]
            })
    }
}
