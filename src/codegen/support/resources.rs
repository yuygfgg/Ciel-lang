use super::*;
use crate::affine::{self, AffineStructInfo, AffineTypeEnv};

struct CodegenAffineEnv<'g, 'a> {
    generator: &'g CGenerator<'a>,
}

impl AffineTypeEnv for CodegenAffineEnv<'_, '_> {
    fn is_resource_handle_leaf(&self, ty: &Ty) -> bool {
        self.generator.type_is_resource_handle_leaf(ty)
    }

    fn named_type_is_async_future(&self, ty: &Ty) -> bool {
        std_id::std_async_future_output_arg(&self.generator.program.checked.resolved, ty).is_some()
    }

    fn named_struct_info(&mut self, ty: &Ty) -> Option<AffineStructInfo> {
        let Ty::Named { name, args, .. } = ty else {
            return None;
        };
        let instance_name = aggregate_instance_name(name, args);
        self.generator
            .program
            .checked
            .structs
            .iter()
            .find(|strukt| strukt.name == instance_name)
            .map(|strukt| AffineStructInfo {
                is_resource: strukt.is_resource,
                fields: strukt
                    .fields
                    .iter()
                    .map(|(_, field_ty)| field_ty.clone())
                    .collect(),
            })
    }

    fn named_enum_payloads(&mut self, ty: &Ty) -> Option<Vec<Ty>> {
        let Ty::Named { name, args, .. } = ty else {
            return None;
        };
        let instance_name = aggregate_instance_name(name, args);
        self.generator
            .program
            .checked
            .enums
            .iter()
            .find(|enm| enm.name == instance_name)
            .map(|enm| {
                enm.variants
                    .iter()
                    .flat_map(|variant| variant.payload.iter().cloned())
                    .collect()
            })
    }
}

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn type_is_resource_handle_leaf(&self, ty: &Ty) -> bool {
        std_id::is_std_resource_handle_ty(&self.program.checked.resolved, ty)
    }

    pub(in crate::codegen) fn type_is_affine(&self, ty: &Ty) -> bool {
        let mut env = CodegenAffineEnv { generator: self };
        affine::type_is_affine(&mut env, ty)
    }

    pub(in crate::codegen) fn resource_cleanup_name(&self, ty: &Ty) -> String {
        format!("CielResourceCleanup_{}", mangle_ty_fragment(ty))
    }

    pub(in crate::codegen) fn resource_transfer_to_parent_name(&self, ty: &Ty) -> String {
        format!("CielResourceTransferToParent_{}", mangle_ty_fragment(ty))
    }

    pub(in crate::codegen) fn resource_cleanup_call(&self, ty: &Ty, value: &str) -> String {
        format!("{}(&{value})", self.resource_cleanup_name(ty))
    }

    pub(in crate::codegen) fn push_resource_cleanup_defer(&mut self, ty: &Ty, value: &str) {
        if !self.type_is_affine(ty) {
            return;
        }
        let call = self.resource_cleanup_call(ty, value);
        self.defer_stack
            .last_mut()
            .expect("defer stack is not empty")
            .push(call);
    }

    pub(in crate::codegen) fn push_temporary_resource_cleanup_defer(
        &mut self,
        ty: &Ty,
        value: &str,
    ) {
        if self.temporary_resource_cleanup_depth == 0 {
            return;
        }
        self.push_resource_cleanup_defer(ty, value);
    }

    pub(in crate::codegen) fn with_temporary_resource_cleanup_scope<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> DiagResult<T>,
    ) -> DiagResult<T> {
        let frame_index = self.defer_stack.len();
        self.defer_stack.push(Vec::new());
        self.temporary_resource_cleanup_depth += 1;
        self.temporary_resource_cleanup_frames.push(frame_index);
        let result = f(self);
        self.temporary_resource_cleanup_frames.pop();
        self.temporary_resource_cleanup_depth -= 1;
        self.defer_stack.pop();
        result
    }

    pub(in crate::codegen) fn async_cleanup_defer_stack(&self) -> Vec<Vec<String>> {
        if self.temporary_resource_cleanup_frames.is_empty() {
            return self.defer_stack.clone();
        }
        self.defer_stack
            .iter()
            .enumerate()
            .filter(|(idx, _)| !self.temporary_resource_cleanup_frames.contains(idx))
            .map(|(_, frame)| frame.clone())
            .collect()
    }

    pub(in crate::codegen) fn resource_cleanup_arg_decl(&self, ty: &Ty, name: &str) -> String {
        match ty {
            Ty::Array { len, elem } => format!("{} (*{name})[{len}]", self.c_type(elem)),
            _ => self.c_decl(&Ty::pointer_to(ty.clone()), name),
        }
    }

    pub(in crate::codegen) fn emit_resource_cleanup_helpers(&mut self) {
        let helpers = self
            .plan
            .resource_cleanup_tys
            .values()
            .cloned()
            .collect::<Vec<_>>();
        if helpers.is_empty() {
            return;
        }
        for ty in &helpers {
            let name = self.resource_cleanup_name(ty);
            let arg = self.resource_cleanup_arg_decl(ty, "value");
            self.line(&format!("static void {name}({arg});"));
        }
        self.line("");
        for ty in helpers {
            let name = self.resource_cleanup_name(&ty);
            let arg = self.resource_cleanup_arg_decl(&ty, "value");
            self.line(&format!("static void {name}({arg}) {{"));
            self.emit_resource_cleanup_body(&ty, "value", 1);
            self.line("}");
            self.line("");
        }
    }

    pub(in crate::codegen) fn emit_resource_transfer_helpers(&mut self) {
        let helpers = self
            .plan
            .resource_cleanup_tys
            .values()
            .filter(|ty| self.type_is_affine(ty))
            .cloned()
            .collect::<Vec<_>>();
        if helpers.is_empty() {
            return;
        }
        for ty in &helpers {
            let name = self.resource_transfer_to_parent_name(ty);
            let arg = self.resource_cleanup_arg_decl(ty, "value");
            self.line(&format!("static int32_t {name}({arg});"));
        }
        self.line("");
        for ty in helpers {
            let name = self.resource_transfer_to_parent_name(&ty);
            let arg = self.resource_cleanup_arg_decl(&ty, "value");
            self.line(&format!("static int32_t {name}({arg}) {{"));
            self.emit_resource_transfer_to_parent_body(&ty, "value", 1);
            self.line("}");
            self.line("");
        }
    }

    pub(in crate::codegen) fn emit_resource_cleanup_body(
        &mut self,
        ty: &Ty,
        value: &str,
        indent: usize,
    ) {
        if self.type_is_resource_handle_leaf(ty) {
            self.line_indent(
                indent,
                &format!("if ({value} != NULL && {value}->owner_id != 0) {{"),
            );
            self.line_indent(
                indent + 1,
                &format!(
                    "(void)ciel_resource_close_handle({value}->owner_id, {value}->resource_id, {value}->generation);"
                ),
            );
            self.line_indent(indent + 1, &format!("{value}->owner_id = 0;"));
            self.line_indent(indent + 1, &format!("{value}->resource_id = 0;"));
            self.line_indent(indent + 1, &format!("{value}->generation = 0;"));
            self.line_indent(indent, "}");
            return;
        }
        match ty {
            Ty::OpaqueState { base, .. } => {
                self.emit_resource_cleanup_body(base, value, indent);
            }
            Ty::GeneratedFuture { .. } => {
                self.line_indent(
                    indent,
                    &format!("if ({value} != NULL && {value}->handle != NULL) {{"),
                );
                self.line_indent(
                    indent + 1,
                    &format!("(void)ciel_future_abort(ciel_future_from_handle({value}->handle));"),
                );
                self.line_indent(indent + 1, &format!("{value}->handle = NULL;"));
                self.line_indent(indent, "}");
            }
            Ty::Array { len, elem } if self.type_is_affine(elem) => {
                let index = self.next_temp("resource_cleanup_i");
                self.line_indent(
                    indent,
                    &format!("for (size_t {index} = {len}; {index} > 0; {index}--) {{"),
                );
                let helper = self.resource_cleanup_name(elem);
                self.line_indent(indent + 1, &format!("{helper}(&(*{value})[{index} - 1]);"));
                self.line_indent(indent, "}");
            }
            Ty::Named { def_id, name, args } => {
                let named_ty = named_ty(*def_id, name.clone(), args.clone());
                if std_id::std_async_future_output_arg(&self.program.checked.resolved, &named_ty)
                    .is_some()
                {
                    self.line_indent(
                        indent,
                        &format!("if ({value} != NULL && {value}->handle != NULL) {{"),
                    );
                    self.line_indent(
                        indent + 1,
                        &format!(
                            "(void)ciel_future_abort(ciel_future_from_handle({value}->handle));"
                        ),
                    );
                    self.line_indent(indent + 1, &format!("{value}->handle = NULL;"));
                    self.line_indent(indent, "}");
                    return;
                }
                let instance_name = aggregate_instance_name(name, args);
                if let Some(strukt) = self
                    .program
                    .checked
                    .structs
                    .iter()
                    .find(|strukt| strukt.name == instance_name)
                    .cloned()
                {
                    let mut emitted_cleanup = false;
                    for (field_name, field_ty) in strukt.fields.iter().rev() {
                        if self.type_is_affine(field_ty) {
                            emitted_cleanup = true;
                            let helper = self.resource_cleanup_name(field_ty);
                            self.line_indent(indent, &format!("{helper}(&{value}->{field_name});"));
                        }
                    }
                    if !emitted_cleanup {
                        self.line_indent(indent, &format!("(void){value};"));
                    }
                } else if let Some(enm) = self
                    .program
                    .checked
                    .enums
                    .iter()
                    .find(|enm| enm.name == instance_name)
                    .cloned()
                {
                    self.line_indent(indent, &format!("if ({value} != NULL) {{"));
                    self.line_indent(indent + 1, &format!("switch ({value}->tag) {{"));
                    for (variant_index, variant) in enm.variants.iter().enumerate() {
                        let affine_payload = variant
                            .payload
                            .iter()
                            .enumerate()
                            .filter(|(_, ty)| self.type_is_affine(ty))
                            .collect::<Vec<_>>();
                        if affine_payload.is_empty() {
                            continue;
                        }
                        self.line_indent(indent + 2, &format!("case {variant_index}:"));
                        for (payload_index, payload_ty) in affine_payload.into_iter().rev() {
                            let helper = self.resource_cleanup_name(payload_ty);
                            self.line_indent(
                                indent + 3,
                                &format!(
                                    "{helper}(&{value}->as.{}._{payload_index});",
                                    variant.name
                                ),
                            );
                        }
                        self.line_indent(indent + 3, "break;");
                    }
                    self.line_indent(indent + 2, "default:");
                    self.line_indent(indent + 3, "break;");
                    self.line_indent(indent + 1, "}");
                    self.line_indent(indent, "}");
                }
            }
            _ => {}
        }
    }

    pub(in crate::codegen) fn emit_resource_transfer_to_parent_body(
        &mut self,
        ty: &Ty,
        value: &str,
        indent: usize,
    ) {
        if self.type_is_resource_handle_leaf(ty) {
            self.line_indent(indent, &format!("if ({value} == NULL) return EINVAL;"));
            self.line_indent(indent, "uint64_t owner_id = 0;");
            self.line_indent(indent, "uint64_t resource_id = 0;");
            self.line_indent(indent, "uint64_t generation = 0;");
            self.line_indent(
                indent,
                &format!(
                    "int32_t rc = ciel_resource_reattach_to_parent_handle({value}->owner_id, {value}->resource_id, {value}->generation, &owner_id, &resource_id, &generation);"
                ),
            );
            self.line_indent(indent, "if (rc != 0) return rc;");
            self.line_indent(indent, &format!("{value}->owner_id = owner_id;"));
            self.line_indent(indent, &format!("{value}->resource_id = resource_id;"));
            self.line_indent(indent, &format!("{value}->generation = generation;"));
            self.line_indent(indent, "return 0;");
            return;
        }
        match ty {
            Ty::OpaqueState { base, .. } => {
                self.emit_resource_transfer_to_parent_body(base, value, indent);
            }
            Ty::GeneratedFuture { .. } => {
                self.line_indent(indent, &format!("if ({value} == NULL) return EINVAL;"));
                self.line_indent(indent, "return ENOTSUP;");
            }
            Ty::Array { len, elem } if self.type_is_affine(elem) => {
                let index = self.next_temp("resource_transfer_i");
                let helper = self.resource_transfer_to_parent_name(elem);
                self.line_indent(indent, "int32_t rc = 0;");
                self.line_indent(
                    indent,
                    &format!("for (size_t {index} = 0; {index} < {len}; {index}++) {{"),
                );
                self.line_indent(indent + 1, &format!("rc = {helper}(&(*{value})[{index}]);"));
                self.line_indent(indent + 1, "if (rc != 0) return rc;");
                self.line_indent(indent, "}");
                self.line_indent(indent, "return 0;");
            }
            Ty::Named { def_id, name, args } => {
                let named_ty = named_ty(*def_id, name.clone(), args.clone());
                if std_id::std_async_future_output_arg(&self.program.checked.resolved, &named_ty)
                    .is_some()
                {
                    self.line_indent(indent, &format!("if ({value} == NULL) return EINVAL;"));
                    self.line_indent(indent, "return ENOTSUP;");
                    return;
                }
                let instance_name = aggregate_instance_name(name, args);
                if let Some(strukt) = self
                    .program
                    .checked
                    .structs
                    .iter()
                    .find(|strukt| strukt.name == instance_name)
                    .cloned()
                {
                    self.line_indent(indent, &format!("if ({value} == NULL) return EINVAL;"));
                    self.line_indent(indent, "int32_t rc = 0;");
                    for (field_name, field_ty) in &strukt.fields {
                        if self.type_is_affine(field_ty) {
                            let helper = self.resource_transfer_to_parent_name(field_ty);
                            self.line_indent(
                                indent,
                                &format!("rc = {helper}(&{value}->{field_name});"),
                            );
                            self.line_indent(indent, "if (rc != 0) return rc;");
                        }
                    }
                    self.line_indent(indent, "return 0;");
                } else if let Some(enm) = self
                    .program
                    .checked
                    .enums
                    .iter()
                    .find(|enm| enm.name == instance_name)
                    .cloned()
                {
                    self.line_indent(indent, &format!("if ({value} == NULL) return EINVAL;"));
                    self.line_indent(indent, "int32_t rc = 0;");
                    self.line_indent(indent, &format!("switch ({value}->tag) {{"));
                    for (variant_index, variant) in enm.variants.iter().enumerate() {
                        let affine_payload = variant
                            .payload
                            .iter()
                            .enumerate()
                            .filter(|(_, ty)| self.type_is_affine(ty))
                            .collect::<Vec<_>>();
                        if affine_payload.is_empty() {
                            continue;
                        }
                        self.line_indent(indent + 1, &format!("case {variant_index}:"));
                        for (payload_index, payload_ty) in affine_payload {
                            let helper = self.resource_transfer_to_parent_name(payload_ty);
                            self.line_indent(
                                indent + 2,
                                &format!(
                                    "rc = {helper}(&{value}->as.{}._{payload_index});",
                                    variant.name
                                ),
                            );
                            self.line_indent(indent + 2, "if (rc != 0) return rc;");
                        }
                        self.line_indent(indent + 2, "break;");
                    }
                    self.line_indent(indent + 1, "default:");
                    self.line_indent(indent + 2, "break;");
                    self.line_indent(indent, "}");
                    self.line_indent(indent, "return 0;");
                } else {
                    self.line_indent(indent, "(void)value;");
                    self.line_indent(indent, "return 0;");
                }
            }
            _ => {
                self.line_indent(indent, "(void)value;");
                self.line_indent(indent, "return 0;");
            }
        }
    }

    pub(in crate::codegen) fn emit_resource_zero_expr(
        &mut self,
        ty: &Ty,
        value: &str,
        indent: usize,
    ) {
        if self.type_is_resource_handle_leaf(ty) {
            self.line_indent(indent, &format!("({value}).owner_id = 0;"));
            self.line_indent(indent, &format!("({value}).resource_id = 0;"));
            self.line_indent(indent, &format!("({value}).generation = 0;"));
            return;
        }
        match ty {
            Ty::OpaqueState { base, .. } => {
                self.emit_resource_zero_expr(base, value, indent);
            }
            Ty::GeneratedFuture { .. } => {
                self.line_indent(indent, &format!("({value}).handle = NULL;"));
            }
            Ty::Array { len, elem } if self.type_is_affine(elem) => {
                let index = self.next_temp("resource_zero_i");
                self.line_indent(
                    indent,
                    &format!("for (size_t {index} = 0; {index} < {len}; {index}++) {{"),
                );
                self.emit_resource_zero_expr(elem, &format!("({value})[{index}]"), indent + 1);
                self.line_indent(indent, "}");
            }
            Ty::Named { def_id, name, args } => {
                let named_ty = named_ty(*def_id, name.clone(), args.clone());
                if std_id::std_async_future_output_arg(&self.program.checked.resolved, &named_ty)
                    .is_some()
                {
                    self.line_indent(indent, &format!("({value}).handle = NULL;"));
                    return;
                }
                let instance_name = aggregate_instance_name(name, args);
                if let Some(strukt) = self
                    .program
                    .checked
                    .structs
                    .iter()
                    .find(|strukt| strukt.name == instance_name)
                    .cloned()
                {
                    for (field_name, field_ty) in &strukt.fields {
                        if self.type_is_affine(field_ty) {
                            self.emit_resource_zero_expr(
                                field_ty,
                                &format!("({value}).{field_name}"),
                                indent,
                            );
                        }
                    }
                } else if self
                    .program
                    .checked
                    .enums
                    .iter()
                    .any(|enm| enm.name == instance_name)
                {
                    self.line_indent(indent, &format!("memset(&{value}, 0, sizeof({value}));"));
                }
            }
            _ => {}
        }
    }
}
