use super::*;

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn emit_dynamic_vtable_layouts(&mut self) {
        let dynamic_types = self.plan.dynamic_types.clone();
        for (_, ty) in dynamic_types {
            let Ty::DynamicInterface { name, args } = &ty else {
                continue;
            };
            let vtable = self.dynamic_vtable_name(&ty);
            self.line(&format!("struct {vtable} {{"));
            for interface in self.dynamic_view_interfaces(name, args) {
                let field_ret = self.dynamic_interface_ret(&interface);
                let field_params = self.dynamic_interface_params(&interface);
                let params = field_params
                    .iter()
                    .filter(|ty| !ty.is_erased_value())
                    .enumerate()
                    .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}")))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.line(&format!(
                    "    {};",
                    self.c_return_decl(&field_ret, &format!("(*{})({})", interface.name, params)),
                ));
            }
            self.line("};");
            self.line("");
        }
    }

    pub(in crate::codegen) fn emit_dynamic_shim_prototypes(&mut self) {
        let uses = self.plan.dynamic_impls.clone();
        for (_, dynamic_use) in uses {
            for interface in self.dynamic_use_interfaces(&dynamic_use) {
                if self
                    .impl_for_dynamic(&interface, &dynamic_use.concrete_ty)
                    .is_some()
                {
                    let ret = self.dynamic_interface_ret(&interface);
                    let params = self.dynamic_interface_params(&interface);
                    let params = params
                        .iter()
                        .filter(|ty| !ty.is_erased_value())
                        .enumerate()
                        .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}")))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let name = self.dynamic_shim_name(
                        &dynamic_use.dyn_ty,
                        &dynamic_use.concrete_ty,
                        &interface.name,
                    );
                    self.line(&format!(
                        "{};",
                        self.c_static_return_decl(&ret, &format!("{name}({params})"))
                    ));
                }
            }
        }
    }

    pub(in crate::codegen) fn emit_dynamic_shims_and_tables(&mut self) {
        let uses = self.plan.dynamic_impls.clone();
        for (_, dynamic_use) in uses {
            for interface in self.dynamic_use_interfaces(&dynamic_use) {
                let Some(implementation) = self
                    .impl_for_dynamic(&interface, &dynamic_use.concrete_ty)
                    .cloned()
                else {
                    continue;
                };
                let ret = self.dynamic_interface_ret(&interface);
                let params = self.dynamic_interface_params(&interface);
                let params_decl = params
                    .iter()
                    .filter(|ty| !ty.is_erased_value())
                    .enumerate()
                    .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}")))
                    .collect::<Vec<_>>()
                    .join(", ");
                let shim_name = self.dynamic_shim_name(
                    &dynamic_use.dyn_ty,
                    &dynamic_use.concrete_ty,
                    &interface.name,
                );
                self.line(&format!(
                    "{} {{",
                    self.c_static_return_decl(&ret, &format!("{shim_name}({params_decl})"))
                ));
                let mut args = Vec::new();
                let first_param = implementation
                    .params
                    .first()
                    .cloned()
                    .unwrap_or(Ty::Unknown);
                if matches!(first_param, Ty::Pointer { .. }) {
                    args.push(format!("({})arg0", self.c_type(&first_param)));
                } else {
                    args.push(format!("*({} *)arg0", self.c_type(&first_param)));
                }
                let mut physical_idx = 1;
                for param in implementation.params.iter().skip(1) {
                    if param.is_erased_value() {
                        continue;
                    }
                    args.push(format!("arg{physical_idx}"));
                    physical_idx += 1;
                }
                let call = format!(
                    "{}({})",
                    self.c_name(implementation.function_def),
                    args.join(", ")
                );
                if ret.is_erased_value() {
                    self.line_indent(1, &format!("{call};"));
                } else {
                    self.line_indent(1, &format!("return {call};"));
                }
                self.line("}");
            }
            let vtable = self.dynamic_vtable_name(&dynamic_use.dyn_ty);
            let table = self.dynamic_table_name(&dynamic_use.dyn_ty, &dynamic_use.concrete_ty);
            self.line(&format!("static const {vtable} {table} = {{"));
            for interface in self.dynamic_use_interfaces(&dynamic_use) {
                self.line(&format!(
                    "    .{} = {},",
                    interface.name,
                    self.dynamic_shim_name(
                        &dynamic_use.dyn_ty,
                        &dynamic_use.concrete_ty,
                        &interface.name
                    )
                ));
            }
            self.line("};");
        }
    }
}
