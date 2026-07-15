use super::*;

impl<'a> CGenerator<'a> {
    pub(super) fn prepare_plan_data(&mut self) {
        self.plan = CodegenPlanBuilder::build(self);
    }
}

struct CodegenPlanBuilder<'a, 'b> {
    generator: &'a CGenerator<'b>,
    plan: CodegenPlanData,
}

impl<'a, 'b> CodegenPlanBuilder<'a, 'b> {
    fn build(generator: &'a CGenerator<'b>) -> CodegenPlanData {
        let mut builder = Self {
            generator,
            plan: CodegenPlanData::default(),
        };
        builder.collect_names();
        builder.collect_slice_types();
        builder.collect_dynamic_interfaces();
        builder.collect_closures();
        builder.collect_array_return_types();
        builder.collect_string_literals();
        builder.collect_source_locations();
        builder.collect_resource_cleanup_types();
        builder.plan
    }

    fn collect_resource_cleanup_types(&mut self) {
        let functions = self.generator.program.checked.functions.clone();
        for function in &functions {
            self.collect_resource_cleanup_ty(&function.ret);
            for (_, _, ty, _) in &function.params {
                self.collect_resource_cleanup_ty(ty);
            }
            if let Some(body) = &function.body {
                let mut visitor = ResourceCleanupVisitor { builder: self };
                visitor.visit_block(body);
            }
        }
        for strukt in self.generator.program.checked.structs.clone() {
            for (_, ty) in strukt.fields {
                self.collect_resource_cleanup_ty(&ty);
            }
        }
        for enm in self.generator.program.checked.enums.clone() {
            for variant in enm.variants {
                for ty in variant.payload {
                    self.collect_resource_cleanup_ty(&ty);
                }
            }
        }
    }

    fn collect_resource_cleanup_ty(&mut self, ty: &Ty) {
        if !self.generator.type_is_affine(ty) {
            return;
        }
        let key = mangle_ty_fragment(ty);
        self.plan
            .resource_cleanup_tys
            .entry(key)
            .or_insert_with(|| ty.clone());
        match ty {
            Ty::Array { elem, .. } => self.collect_resource_cleanup_ty(elem),
            Ty::Named { def_id, name, args } => {
                let named_ty = named_ty(*def_id, name.clone(), args.clone());
                if std_id::std_async_future_output_arg(
                    &self.generator.program.checked.resolved,
                    &named_ty,
                )
                .is_some()
                {
                    return;
                }
                let instance_name = aggregate_instance_name(name, args);
                if let Some(strukt) = self
                    .generator
                    .program
                    .checked
                    .structs
                    .iter()
                    .find(|strukt| strukt.name == instance_name)
                    .cloned()
                {
                    for (_, field_ty) in strukt.fields {
                        self.collect_resource_cleanup_ty(&field_ty);
                    }
                }
                if let Some(enm) = self
                    .generator
                    .program
                    .checked
                    .enums
                    .iter()
                    .find(|enm| enm.name == instance_name)
                    .cloned()
                {
                    for variant in enm.variants {
                        for payload_ty in variant.payload {
                            self.collect_resource_cleanup_ty(&payload_ty);
                        }
                    }
                }
            }
            Ty::ClosureInstance { captures, .. } => {
                for capture in captures {
                    self.collect_resource_cleanup_ty(capture);
                }
            }
            Ty::GeneratedFuture { output, .. } => self.collect_resource_cleanup_ty(output),
            _ => {}
        }
    }
}

impl<'a> CGenerator<'a> {
    pub(super) fn aggregate_layout_order(&self) -> Vec<AggregateLayoutRef> {
        let mut by_name = HashMap::new();
        for (idx, strukt) in self.program.checked.structs.iter().enumerate() {
            by_name.insert(strukt.name.clone(), AggregateLayoutRef::Struct(idx));
        }
        for (idx, enm) in self.program.checked.enums.iter().enumerate() {
            by_name.insert(enm.name.clone(), AggregateLayoutRef::Enum(idx));
        }

        let aggregate_names = by_name.keys().cloned().collect::<HashSet<_>>();
        let mut visited = HashSet::new();
        let mut visiting = HashSet::new();
        let mut out = Vec::new();
        for strukt in &self.program.checked.structs {
            self.visit_aggregate_layout(
                &strukt.name,
                &by_name,
                &aggregate_names,
                &mut visiting,
                &mut visited,
                &mut out,
            );
        }
        for enm in &self.program.checked.enums {
            self.visit_aggregate_layout(
                &enm.name,
                &by_name,
                &aggregate_names,
                &mut visiting,
                &mut visited,
                &mut out,
            );
        }
        out
    }

    fn visit_aggregate_layout(
        &self,
        name: &str,
        by_name: &HashMap<String, AggregateLayoutRef>,
        aggregate_names: &HashSet<String>,
        visiting: &mut HashSet<String>,
        visited: &mut HashSet<String>,
        out: &mut Vec<AggregateLayoutRef>,
    ) {
        if visited.contains(name) || !visiting.insert(name.to_string()) {
            return;
        }
        let Some(item) = by_name.get(name).copied() else {
            visiting.remove(name);
            return;
        };
        for dep in self.aggregate_layout_deps(item, aggregate_names) {
            self.visit_aggregate_layout(&dep, by_name, aggregate_names, visiting, visited, out);
        }
        visiting.remove(name);
        visited.insert(name.to_string());
        out.push(item);
    }

    fn aggregate_layout_deps(
        &self,
        item: AggregateLayoutRef,
        aggregate_names: &HashSet<String>,
    ) -> Vec<String> {
        let mut deps = Vec::new();
        match item {
            AggregateLayoutRef::Struct(idx) => {
                for (_, ty) in &self.program.checked.structs[idx].fields {
                    self.collect_aggregate_value_deps(ty, aggregate_names, &mut deps);
                }
            }
            AggregateLayoutRef::Enum(idx) => {
                for variant in &self.program.checked.enums[idx].variants {
                    for ty in &variant.payload {
                        self.collect_aggregate_value_deps(ty, aggregate_names, &mut deps);
                    }
                }
            }
        }
        deps.sort();
        deps.dedup();
        deps
    }

    fn collect_aggregate_value_deps(
        &self,
        ty: &Ty,
        aggregate_names: &HashSet<String>,
        out: &mut Vec<String>,
    ) {
        match ty {
            Ty::Array { elem, .. } => self.collect_aggregate_value_deps(elem, aggregate_names, out),
            Ty::Named { name, args, .. } => {
                if let Some(storage_ty) = self.meta_repr_marker_storage_ty(name, args) {
                    self.collect_aggregate_value_deps(&storage_ty, aggregate_names, out);
                    return;
                }
                if let Some(storage_ty) = self.meta_schema_marker_storage_ty(name, args) {
                    self.collect_aggregate_value_deps(&storage_ty, aggregate_names, out);
                    return;
                }
                let c_name = self.c_named_type(name, args);
                if aggregate_names.contains(&c_name) {
                    out.push(c_name);
                }
            }
            _ => {}
        }
    }

    pub(super) fn emit_struct_layout(&mut self, idx: usize) {
        let strukt = self.program.checked.structs[idx].clone();
        self.line(&format!("struct {} {{", strukt.name));
        let mut emitted_field = false;
        for (field, ty) in &strukt.fields {
            if ty.is_erased_value() {
                continue;
            }
            emitted_field = true;
            self.line(&format!("    {};", self.c_decl(ty, field)));
        }
        if !emitted_field {
            self.line("    char _ciel_empty;");
        }
        self.line("};");
        self.line("");
    }

    pub(super) fn emit_enum_layout(&mut self, idx: usize) {
        let enm = self.program.checked.enums[idx].clone();
        self.line(&format!("struct {} {{", enm.name));
        self.line("    int tag;");
        if enm
            .variants
            .iter()
            .any(|variant| !variant.payload.is_empty())
        {
            self.line("    union {");
            for variant in &enm.variants {
                if variant.payload.is_empty() {
                    continue;
                }
                self.line("        struct {");
                for (idx, ty) in variant.payload.iter().enumerate() {
                    self.line(&format!(
                        "            {};",
                        self.c_decl(ty, &format!("_{idx}"))
                    ));
                }
                self.line(&format!("        }} {};", variant.name));
            }
            self.line("    } as;");
        }
        self.line("};");
        self.line("");
    }

    pub(super) fn emit_array_return_type_layouts(&mut self) {
        let array_return_types = self.plan.array_return_types.clone();
        for (name, ty) in array_return_types {
            self.line(&format!("struct {name} {{"));
            self.line(&format!("    {};", self.c_decl(&ty, "value")));
            self.line("};");
            self.line("");
        }
    }

    pub(super) fn emit_c_includes(&mut self) {
        let mut includes = Vec::new();
        for module in &self.program.checked.hir_modules {
            for item in &module.items {
                if let crate::hir::ItemKind::CInclude(include) = &item.kind {
                    includes.push(include.clone());
                }
            }
        }
        includes.sort();
        includes.dedup();
        for include in includes {
            self.line(&format!("#include \"{}\"", escape_c_include(&include)));
        }
    }

    pub(super) fn emit_runtime(&mut self) {
        self.line("#include \"ciel_runtime.h\"");
    }

    pub(super) fn emit_source_location_table(&mut self) {
        if self.plan.source_locations.is_empty() {
            return;
        }
        self.line(
            "typedef struct CielSourceLocation { char *file; size_t line; } CielSourceLocation;",
        );
        let locations = self
            .plan
            .source_locations
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for location in locations {
            self.line(&format!(
                "static CIEL_MAYBE_UNUSED const CielSourceLocation {} = {{ \"{}\", {} }};",
                location.name,
                escape_c_string(&location.file),
                location.line
            ));
        }
        self.line("");
    }
}

impl<'a, 'b> CodegenPlanBuilder<'a, 'b> {
    fn collect_names(&mut self) {
        for function in &self.generator.program.checked.functions {
            let c_name = if function.abi.as_deref() == Some("C")
                && (function.exported || function.body.is_none())
            {
                function.name.clone()
            } else {
                format!("ciel_{}_{}", function.def_id.0, function.name)
            };
            self.plan.name_map.insert(function.def_id, c_name);
        }
    }

    fn collect_slice_types(&mut self) {
        self.collect_program_types_and_bodies(
            |this, ty| this.collect_ty_slice(ty),
            |this, body, _| {
                let mut visitor = SliceVisitor { builder: this };
                visitor.visit_block(body);
            },
        );
    }

    fn collect_ty_slice(&mut self, ty: &Ty) {
        match ty {
            Ty::Slice { mutability, elem } => {
                self.plan
                    .slice_types
                    .insert(self.generator.slice_name(*mutability, elem), ty.clone());
                self.collect_ty_slice(elem);
            }
            Ty::Pointer { inner, .. } | Ty::Array { elem: inner, .. } => {
                self.collect_ty_slice(inner)
            }
            Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
                for arg in args {
                    self.collect_ty_slice(arg);
                }
            }
            Ty::Function { ret, params, .. } => {
                self.collect_ty_slice(ret);
                for param in params {
                    self.collect_ty_slice(param);
                }
            }
            Ty::Closure { ret, params, .. } | Ty::ClosureInstance { ret, params, .. } => {
                self.collect_ty_slice(ret);
                for param in params {
                    self.collect_ty_slice(param);
                }
            }
            _ => {}
        }
    }

    fn collect_dynamic_interfaces(&mut self) {
        self.collect_program_types_and_bodies(
            |this, ty| this.collect_ty_dynamic(ty),
            |this, body, _| {
                let mut visitor = DynamicVisitor { builder: this };
                visitor.visit_block(body);
            },
        );
    }

    fn collect_closures(&mut self) {
        self.collect_program_types_and_bodies(
            |this, ty| this.collect_ty_closure(ty),
            |this, body, function_def| {
                let mut visitor = ClosureVisitor {
                    builder: this,
                    function_def,
                };
                visitor.visit_block(body);
            },
        );
    }

    fn collect_program_types_and_bodies(
        &mut self,
        mut collect_ty: impl FnMut(&mut Self, &Ty),
        mut collect_body: impl FnMut(&mut Self, &TBlock, DefId),
    ) {
        for strukt in &self.generator.program.checked.structs {
            for (_, ty) in &strukt.fields {
                collect_ty(self, ty);
            }
        }
        for enm in &self.generator.program.checked.enums {
            for variant in &enm.variants {
                for ty in &variant.payload {
                    collect_ty(self, ty);
                }
            }
        }
        let functions = self.generator.program.checked.functions.clone();
        for function in &functions {
            collect_ty(self, &function.ret);
            for (_, _, ty, _) in &function.params {
                collect_ty(self, ty);
            }
            if let Some(body) = &function.body {
                collect_body(self, body, function.def_id);
            }
        }
    }

    fn collect_array_return_types(&mut self) {
        for strukt in &self.generator.program.checked.structs {
            for (_, ty) in &strukt.fields {
                self.collect_ty_array_returns(ty);
            }
        }
        for enm in &self.generator.program.checked.enums {
            for variant in &enm.variants {
                for ty in &variant.payload {
                    self.collect_ty_array_returns(ty);
                }
            }
        }
        for interface in &self.generator.program.checked.interfaces {
            self.collect_return_ty_array_return(&interface.ret);
            for param in &interface.params {
                self.collect_ty_array_returns(param);
            }
        }
        for implementation in &self.generator.program.checked.impls {
            self.collect_return_ty_array_return(&implementation.ret);
            for param in &implementation.params {
                self.collect_ty_array_returns(param);
            }
        }
        let functions = self.generator.program.checked.functions.clone();
        for function in &functions {
            self.collect_return_ty_array_return(&self.generator.function_call_return_ty(function));
            if function.is_async {
                self.collect_return_ty_array_return(&function.ret);
            }
            for (_, _, ty, _) in &function.params {
                self.collect_ty_array_returns(ty);
            }
            if let Some(body) = &function.body {
                let mut visitor = ArrayReturnVisitor { builder: self };
                visitor.visit_block(body);
            }
        }
        let closure_defs = self.plan.closure_defs.clone();
        for closure in closure_defs.values() {
            self.collect_ty_array_returns(&closure.ty);
            for (_, _, ty) in &closure.params {
                self.collect_ty_array_returns(ty);
            }
        }
        let dynamic_types = self.plan.dynamic_types.clone();
        for (_, ty) in dynamic_types {
            let Ty::DynamicInterface { def_id, args, .. } = &ty else {
                continue;
            };
            for interface_ref in self.generator.dynamic_view_interfaces(*def_id, args) {
                let ret = self.generator.dynamic_interface_ret(&interface_ref);
                self.collect_return_ty_array_return(&ret);
                for param in self.generator.dynamic_interface_params(&interface_ref) {
                    self.collect_ty_array_returns(&param);
                }
            }
        }
    }

    fn collect_return_ty_array_return(&mut self, ty: &Ty) {
        if self.generator.ty_needs_array_return_wrapper(ty) {
            self.plan
                .array_return_types
                .insert(self.generator.array_return_type_name(ty), ty.clone());
        }
        self.collect_ty_array_returns(ty);
    }

    fn collect_ty_array_returns(&mut self, ty: &Ty) {
        match ty {
            Ty::Pointer { inner, .. }
            | Ty::Array { elem: inner, .. }
            | Ty::Slice { elem: inner, .. } => {
                self.collect_ty_array_returns(inner);
            }
            Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
                for arg in args {
                    self.collect_ty_array_returns(arg);
                }
            }
            Ty::Function { ret, params, .. }
            | Ty::Closure { ret, params, .. }
            | Ty::ClosureInstance { ret, params, .. } => {
                self.collect_return_ty_array_return(ret);
                for param in params {
                    self.collect_ty_array_returns(param);
                }
            }
            _ => {}
        }
    }

    fn collect_ty_closure(&mut self, ty: &Ty) {
        match ty {
            Ty::Closure {
                ret,
                params,
                constraints,
            } => {
                self.plan
                    .closure_types
                    .insert(self.generator.closure_type_name(ty), ty.clone());
                self.collect_ty_closure(ret);
                for param in params {
                    self.collect_ty_closure(param);
                }
                self.collect_constraint_bounds_closures(constraints);
            }
            Ty::ClosureInstance { ret, params, .. } => {
                self.plan
                    .closure_types
                    .insert(self.generator.closure_type_name(ty), ty.clone());
                self.collect_ty_closure(ret);
                for param in params {
                    self.collect_ty_closure(param);
                }
            }
            Ty::Pointer { inner, .. }
            | Ty::Array { elem: inner, .. }
            | Ty::Slice { elem: inner, .. } => self.collect_ty_closure(inner),
            Ty::Named { args, .. } | Ty::DynamicInterface { args, .. } => {
                for arg in args {
                    self.collect_ty_closure(arg);
                }
            }
            Ty::Function { ret, params, .. } => {
                self.collect_ty_closure(ret);
                for param in params {
                    self.collect_ty_closure(param);
                }
            }
            _ => {}
        }
    }

    fn collect_constraint_bounds_closures(&mut self, bounds: &ConstraintBounds) {
        for entry in bounds.positive.iter().chain(bounds.negative.iter()) {
            for arg in &entry.args {
                self.collect_ty_closure(arg);
            }
        }
    }

    fn collect_retained_closure_witnesses(
        &mut self,
        target_ty: &Ty,
        source_ty: &Ty,
        span: crate::span::Span,
    ) {
        for capability in retained_closure_required_witnesses(target_ty, source_ty) {
            let key =
                self.generator
                    .retained_closure_witness_name(target_ty, source_ty, &capability);
            self.plan
                .retained_closure_witnesses
                .entry(key)
                .or_insert_with(|| RetainedClosureWitness {
                    target_ty: target_ty.clone(),
                    source_ty: source_ty.clone(),
                    capability,
                    span,
                });
        }
    }

    fn collect_retained_closure_wrapper(&mut self, target_ty: &Ty, source_ty: &Ty) {
        if !retained_closure_needs_wrapper(target_ty, source_ty) {
            return;
        }
        let key = self
            .generator
            .retained_closure_wrapper_key(target_ty, source_ty);
        self.plan
            .retained_closure_wrappers
            .entry(key)
            .or_insert_with(|| RetainedClosureWrapper {
                target_ty: target_ty.clone(),
                source_ty: source_ty.clone(),
            });
    }

    fn collect_ty_dynamic(&mut self, ty: &Ty) {
        match ty {
            Ty::DynamicInterface { .. } => {
                let name = self.generator.dynamic_type_name(ty);
                self.plan.dynamic_types.insert(name, ty.clone());
                if let Ty::DynamicInterface { args, .. } = ty {
                    for arg in args {
                        self.collect_ty_dynamic(arg);
                    }
                }
            }
            Ty::Pointer { inner, .. }
            | Ty::Array { elem: inner, .. }
            | Ty::Slice { elem: inner, .. } => self.collect_ty_dynamic(inner),
            Ty::Named { args, .. } => {
                for arg in args {
                    self.collect_ty_dynamic(arg);
                }
            }
            Ty::Function { ret, params, .. } => {
                self.collect_ty_dynamic(ret);
                for param in params {
                    self.collect_ty_dynamic(param);
                }
            }
            Ty::Closure { ret, params, .. } | Ty::ClosureInstance { ret, params, .. } => {
                self.collect_ty_dynamic(ret);
                for param in params {
                    self.collect_ty_dynamic(param);
                }
            }
            _ => {}
        }
    }

    fn collect_type_id(&mut self, ty: &Ty) {
        let ty = canonical_type_identity_ty(ty);
        if !self.plan.type_ids.contains(&ty) {
            self.plan.type_ids.push(ty);
        }
    }

    fn collect_dynamic_error_witness_type_id(&mut self, dyn_ty: &Ty, concrete_ty: &Ty) {
        let Ty::DynamicInterface { def_id, args, .. } = dyn_ty else {
            return;
        };
        let has_witness = self
            .generator
            .dynamic_view_interfaces(*def_id, args)
            .iter()
            .any(|interface| {
                std_id::is_std_error_interface(
                    &self.generator.program.checked.resolved,
                    interface.def_id,
                    "erased_error_ref",
                )
            });
        if has_witness {
            self.collect_type_id(&receiver_ty_from_value_ty(concrete_ty));
        }
    }

    fn collect_dynamic_impl_use(&mut self, dyn_ty: &Ty, concrete_ty: &Ty) {
        self.collect_ty_dynamic(dyn_ty);
        self.collect_ty_dynamic(concrete_ty);
        if matches!(concrete_ty, Ty::DynamicInterface { .. }) {
            return;
        }
        self.collect_dynamic_error_witness_type_id(dyn_ty, concrete_ty);
        self.plan.dynamic_impls.insert(
            self.generator.dynamic_impl_key(dyn_ty, concrete_ty),
            DynamicImplUse {
                dyn_ty: dyn_ty.clone(),
                concrete_ty: concrete_ty.clone(),
            },
        );
    }

    fn collect_standard_error_code_dynamic(&mut self) {
        let dyn_ty = self.generator.std_error_trait_ty();
        let code_ty = std_error_code_ty(&self.generator.program.checked.resolved);
        self.collect_dynamic_impl_use(&dyn_ty, &code_ty);
    }

    fn erased_box_try_err_ty<'t>(
        &self,
        inner: &'t TExpr,
        propagation: &TryPropagation,
    ) -> Option<&'t Ty> {
        if matches!(propagation, TryPropagation::Exact) {
            return None;
        }
        result_args(&self.generator.program.checked.resolved, &inner.ty).map(|(_, err_ty)| err_ty)
    }

    fn collect_string_literals(&mut self) {
        let functions = self.generator.program.checked.functions.clone();
        for function in &functions {
            if let Some(body) = &function.body {
                let mut visitor = StringLiteralVisitor { builder: self };
                visitor.visit_block(body);
            }
        }
        let keys = self
            .plan
            .string_literals
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for (idx, key) in keys.into_iter().enumerate() {
            self.plan
                .string_literal_names
                .insert(key, format!("ciel_str_{idx}"));
        }
    }

    fn collect_source_locations(&mut self) {
        let functions = self.generator.program.checked.functions.clone();
        for function in &functions {
            if let Some(body) = &function.body {
                let mut visitor = SourceLocationVisitor { builder: self };
                visitor.visit_block(body);
            }
        }
        let keys = self
            .plan
            .source_locations
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for (idx, key) in keys.into_iter().enumerate() {
            if let Some(location) = self.plan.source_locations.get_mut(&key) {
                location.name = format!("ciel_loc_{idx}");
            }
        }
    }

    fn register_source_location(&mut self, span: crate::span::Span) {
        let (line, _) = self.generator.source_map.line_col(span.file, span.start);
        let key = (span.file.0, line);
        self.plan.source_locations.entry(key).or_insert_with(|| {
            let file = self
                .generator
                .source_map
                .file_path(span.file)
                .display()
                .to_string();
            SourceLocation {
                name: String::new(),
                file,
                line,
            }
        });
    }
}

struct SourceLocationVisitor<'a, 'b, 'c> {
    builder: &'a mut CodegenPlanBuilder<'b, 'c>,
}

impl ThirVisitor for SourceLocationVisitor<'_, '_, '_> {
    fn visit_block(&mut self, block: &TBlock) {
        self.builder.register_source_location(block.span);
        walk_block(self, block);
    }

    fn visit_stmt(&mut self, stmt: &TStmt) {
        self.builder.register_source_location(stmt.span);
        walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        self.builder.register_source_location(expr.span);
        walk_expr(self, expr);
    }
}

struct StringLiteralVisitor<'a, 'b, 'c> {
    builder: &'a mut CodegenPlanBuilder<'b, 'c>,
}

impl ThirVisitor for StringLiteralVisitor<'_, '_, '_> {
    fn visit_expr(&mut self, expr: &TExpr) {
        if let TExprKind::Literal(Literal::String(raw)) = &expr.kind {
            self.builder
                .plan
                .string_literals
                .insert(span_key(expr.span), raw.clone());
        }
        walk_expr(self, expr);
    }
}

struct ResourceCleanupVisitor<'a, 'b, 'c> {
    builder: &'a mut CodegenPlanBuilder<'b, 'c>,
}

impl ThirVisitor for ResourceCleanupVisitor<'_, '_, '_> {
    fn visit_stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::VarDecl { ty, init, .. } => {
                self.builder.collect_resource_cleanup_ty(ty);
                if let Some(init) = init {
                    self.visit_expr(init);
                }
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_for_init(&mut self, init: &TForInit) {
        match init {
            TForInit::VarDecl { ty, init, .. } => {
                self.builder.collect_resource_cleanup_ty(ty);
                if let Some(init) = init {
                    self.visit_expr(init);
                }
            }
            _ => walk_for_init(self, init),
        }
    }

    fn visit_pattern(&mut self, pattern: &TPattern) {
        self.builder.collect_resource_cleanup_ty(pattern.ty());
        walk_pattern(self, pattern);
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        self.builder.collect_resource_cleanup_ty(&expr.ty);
        match &expr.kind {
            TExprKind::Call { callee, args } => {
                if self
                    .builder
                    .generator
                    .std_resource_transfer_before_owner_close_call(callee)
                    && let Some(Ty::Pointer { inner, .. }) = args.first().map(|arg| &arg.ty)
                {
                    self.builder.collect_resource_cleanup_ty(inner);
                }
                if let Some(scoped) = self.builder.generator.std_resource_scoped_call(callee) {
                    let body_arg = match scoped {
                        ResourceScopedCall::Default => args.first(),
                        ResourceScopedCall::WithLimits => args.get(1),
                    };
                    if let Some(body_arg) = body_arg
                        && let Some((body_ret_ty, _)) = callable_ret_params_ty(&body_arg.ty)
                    {
                        self.builder.collect_resource_cleanup_ty(&body_ret_ty);
                    }
                }
                walk_expr(self, expr);
            }
            TExprKind::Closure { captures, .. } => {
                for capture in captures {
                    self.builder.collect_resource_cleanup_ty(&capture.ty);
                }
                walk_expr(self, expr);
            }
            _ => walk_expr(self, expr),
        }
    }
}

struct ArrayReturnVisitor<'a, 'b, 'c> {
    builder: &'a mut CodegenPlanBuilder<'b, 'c>,
}

impl ThirVisitor for ArrayReturnVisitor<'_, '_, '_> {
    fn visit_stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::VarDecl { ty, init, .. } => {
                self.builder.collect_ty_array_returns(ty);
                if let Some(init) = init {
                    self.visit_expr(init);
                }
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_for_init(&mut self, init: &TForInit) {
        match init {
            TForInit::VarDecl { ty, init, .. } => {
                self.builder.collect_ty_array_returns(ty);
                if let Some(init) = init {
                    self.visit_expr(init);
                }
            }
            _ => walk_for_init(self, init),
        }
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        self.builder.collect_ty_array_returns(&expr.ty);
        match &expr.kind {
            TExprKind::Closure { captures, .. } => {
                for capture in captures {
                    self.builder.collect_ty_array_returns(&capture.ty);
                }
            }
            TExprKind::RetainClosure { source_ty, .. } => {
                self.builder.collect_ty_array_returns(source_ty);
                walk_expr(self, expr);
            }
            TExprKind::MakeDynamicInterface { concrete_ty, .. }
            | TExprKind::ErrorBox { concrete_ty, .. }
            | TExprKind::ReportBox { concrete_ty, .. } => {
                self.builder.collect_ty_array_returns(concrete_ty);
                walk_expr(self, expr);
            }
            TExprKind::AsyncSelect { arms, .. } => {
                for arm in arms {
                    self.visit_expr(&arm.future);
                    self.builder.collect_ty_array_returns(&arm.future_output_ty);
                    self.visit_expr(&arm.body);
                }
            }
            TExprKind::AsyncSleep { output_ty, .. } => {
                self.builder.collect_ty_array_returns(&std_future_ty(
                    &self.builder.generator.program.checked.resolved,
                    output_ty.clone(),
                ));
                self.builder.collect_ty_array_returns(output_ty);
                walk_expr(self, expr);
            }
            TExprKind::AsyncSpawn {
                task_output_ty,
                task_error_ty,
                ..
            } => {
                self.builder.collect_ty_array_returns(&std_task_ty(
                    &self.builder.generator.program.checked.resolved,
                    task_output_ty.clone(),
                    task_error_ty.clone(),
                ));
                self.builder.collect_ty_array_returns(&std_result_ty(
                    &self.builder.generator.program.checked.resolved,
                    std_task_ty(
                        &self.builder.generator.program.checked.resolved,
                        task_output_ty.clone(),
                        task_error_ty.clone(),
                    ),
                    std_async_error_ty(&self.builder.generator.program.checked.resolved),
                ));
                walk_expr(self, expr);
            }
            TExprKind::AsyncTaskCancel {
                task_output_ty,
                task_error_ty,
                ..
            }
            | TExprKind::AsyncTaskIsFinished {
                task_output_ty,
                task_error_ty,
                ..
            } => {
                self.builder.collect_ty_array_returns(&std_task_ty(
                    &self.builder.generator.program.checked.resolved,
                    task_output_ty.clone(),
                    task_error_ty.clone(),
                ));
                walk_expr(self, expr);
            }
            TExprKind::ActorSpawn {
                state_ty,
                handle_message_ty,
                message_ty,
                handler_ty,
                ..
            } => {
                self.builder.collect_ty_array_returns(state_ty);
                self.builder.collect_ty_array_returns(handle_message_ty);
                self.builder.collect_ty_array_returns(message_ty);
                self.builder.collect_ty_array_returns(handler_ty);
                walk_expr(self, expr);
            }
            TExprKind::ActorSend { message_ty, .. }
            | TExprKind::ActorStop { message_ty, .. }
            | TExprKind::ActorJoin { message_ty, .. } => {
                self.builder.collect_ty_array_returns(message_ty);
                walk_expr(self, expr);
            }
            TExprKind::TypeSize { ty }
            | TExprKind::TypeAlign { ty }
            | TExprKind::TypeNeedsGcScan { ty }
            | TExprKind::TypeId { ty } => {
                self.builder.collect_ty_array_returns(ty);
            }
            TExprKind::MetaSchema { source_ty } => {
                self.builder.collect_ty_array_returns(source_ty);
            }
            _ => walk_expr(self, expr),
        }
    }
}

struct ClosureVisitor<'a, 'b, 'c> {
    builder: &'a mut CodegenPlanBuilder<'b, 'c>,
    function_def: DefId,
}

impl ThirVisitor for ClosureVisitor<'_, '_, '_> {
    fn visit_stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::VarDecl { ty, init, .. } => {
                self.builder.collect_ty_closure(ty);
                if let Some(init) = init {
                    self.visit_expr(init);
                }
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_for_init(&mut self, init: &TForInit) {
        match init {
            TForInit::VarDecl { ty, init, .. } => {
                self.builder.collect_ty_closure(ty);
                if let Some(init) = init {
                    self.visit_expr(init);
                }
            }
            _ => walk_for_init(self, init),
        }
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        self.builder.collect_ty_closure(&expr.ty);
        match &expr.kind {
            TExprKind::Closure {
                is_async,
                id,
                params,
                captures,
                body,
                async_facts,
                ..
            } => {
                self.builder
                    .plan
                    .closure_defs
                    .entry(*id)
                    .or_insert_with(|| ClosureDef {
                        id: *id,
                        function_def: self.function_def,
                        ty: expr.ty.clone(),
                        is_async: *is_async,
                        async_facts: async_facts.clone(),
                        params: params.clone(),
                        captures: captures.clone(),
                        body: body.clone(),
                    });
                for (_, _, ty) in params {
                    self.builder.collect_ty_closure(ty);
                }
                for capture in captures {
                    self.builder.collect_ty_closure(&capture.ty);
                }
                self.visit_closure_body(body);
            }
            TExprKind::FunctionToClosure(inner) => {
                self.visit_expr(inner);
                self.builder
                    .collect_retained_closure_witnesses(&expr.ty, &inner.ty, expr.span);
                let key = self
                    .builder
                    .generator
                    .function_closure_wrapper_key(&expr.ty, &inner.ty);
                self.builder
                    .plan
                    .function_closure_wrappers
                    .entry(key)
                    .or_insert_with(|| FunctionClosureWrapper {
                        closure_ty: expr.ty.clone(),
                        function_ty: inner.ty.clone(),
                    });
            }
            TExprKind::RetainClosure { source_ty, .. } => {
                self.builder.collect_ty_closure(source_ty);
                self.builder
                    .collect_retained_closure_wrapper(&expr.ty, source_ty);
                self.builder
                    .collect_retained_closure_witnesses(&expr.ty, source_ty, expr.span);
                walk_expr(self, expr);
            }
            TExprKind::AsyncSelect { arms, .. } => {
                for arm in arms {
                    self.visit_expr(&arm.future);
                    self.builder.collect_ty_closure(&arm.future_output_ty);
                    self.visit_expr(&arm.body);
                }
            }
            TExprKind::AsyncSleep { output_ty, .. } => {
                self.builder
                    .plan
                    .async_sleep_output_tys
                    .insert(mangle_ty_fragment(output_ty), output_ty.clone());
                walk_expr(self, expr);
            }
            TExprKind::AsyncSpawn {
                task_output_ty,
                task_error_ty,
                ..
            } => {
                self.builder.collect_ty_closure(&std_task_ty(
                    &self.builder.generator.program.checked.resolved,
                    task_output_ty.clone(),
                    task_error_ty.clone(),
                ));
                self.builder.collect_ty_closure(&std_result_ty(
                    &self.builder.generator.program.checked.resolved,
                    task_output_ty.clone(),
                    task_error_ty.clone(),
                ));
                walk_expr(self, expr);
            }
            TExprKind::AsyncTaskCancel {
                task_output_ty,
                task_error_ty,
                ..
            }
            | TExprKind::AsyncTaskIsFinished {
                task_output_ty,
                task_error_ty,
                ..
            } => {
                self.builder.collect_ty_closure(&std_task_ty(
                    &self.builder.generator.program.checked.resolved,
                    task_output_ty.clone(),
                    task_error_ty.clone(),
                ));
                walk_expr(self, expr);
            }
            TExprKind::MakeDynamicInterface { concrete_ty, .. }
            | TExprKind::ErrorBox { concrete_ty, .. }
            | TExprKind::ReportBox { concrete_ty, .. } => {
                self.builder.collect_ty_closure(concrete_ty);
                walk_expr(self, expr);
            }
            TExprKind::MetaAsRefRepr { source_ty, .. }
            | TExprKind::MetaIntoRepr { source_ty, .. } => {
                self.builder.collect_ty_closure(source_ty);
                walk_expr(self, expr);
            }
            TExprKind::MetaFromRepr { target_ty, .. } => {
                self.builder.collect_ty_closure(target_ty);
                walk_expr(self, expr);
            }
            TExprKind::MetaSchema { source_ty } => {
                self.builder.collect_ty_closure(source_ty);
            }
            TExprKind::ActorSpawn {
                mode,
                state_ty,
                handle_message_ty,
                message_ty,
                handler_ty,
                ..
            } => {
                self.builder.collect_ty_closure(state_ty);
                self.builder.collect_ty_closure(handle_message_ty);
                self.builder.collect_ty_closure(message_ty);
                self.builder.collect_ty_closure(handler_ty);
                let name = self
                    .builder
                    .generator
                    .actor_dispatch_name(mode, state_ty, message_ty, handler_ty);
                self.builder
                    .plan
                    .actor_dispatches
                    .entry(name.clone())
                    .or_insert_with(|| ActorDispatch {
                        name,
                        mode: mode.clone(),
                        state_ty: state_ty.clone(),
                        handle_message_ty: handle_message_ty.clone(),
                        message_ty: message_ty.clone(),
                        handler_ty: handler_ty.clone(),
                    });
                walk_expr(self, expr);
            }
            TExprKind::ActorSend { message_ty, .. }
            | TExprKind::ActorStop { message_ty, .. }
            | TExprKind::ActorJoin { message_ty, .. } => {
                self.builder.collect_ty_closure(message_ty);
                walk_expr(self, expr);
            }
            TExprKind::TypeSize { ty }
            | TExprKind::TypeAlign { ty }
            | TExprKind::TypeNeedsGcScan { ty }
            | TExprKind::TypeId { ty } => {
                self.builder.collect_ty_closure(ty);
            }
            _ => walk_expr(self, expr),
        }
    }
}

struct DynamicVisitor<'a, 'b, 'c> {
    builder: &'a mut CodegenPlanBuilder<'b, 'c>,
}

impl ThirVisitor for DynamicVisitor<'_, '_, '_> {
    fn visit_stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::VarDecl { ty, init, .. } => {
                self.builder.collect_ty_dynamic(ty);
                if let Some(init) = init {
                    self.visit_expr(init);
                }
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_for_init(&mut self, init: &TForInit) {
        match init {
            TForInit::VarDecl { ty, init, .. } => {
                self.builder.collect_ty_dynamic(ty);
                if let Some(init) = init {
                    self.visit_expr(init);
                }
            }
            _ => walk_for_init(self, init),
        }
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        self.builder.collect_ty_dynamic(&expr.ty);
        match &expr.kind {
            TExprKind::MakeDynamicInterface { concrete_ty, .. } => {
                self.builder.collect_dynamic_impl_use(&expr.ty, concrete_ty);
                walk_expr(self, expr);
            }
            TExprKind::ErrorBox { concrete_ty, .. } => {
                let dyn_ty = self.builder.generator.std_error_trait_ty();
                self.builder.collect_dynamic_impl_use(&dyn_ty, concrete_ty);
                walk_expr(self, expr);
            }
            TExprKind::ReportBox { concrete_ty, .. } => {
                let dyn_ty = self.builder.generator.std_error_trait_ty();
                self.builder.collect_dynamic_impl_use(&dyn_ty, concrete_ty);
                walk_expr(self, expr);
            }
            TExprKind::Try {
                expr: inner,
                propagation,
            } => {
                if let Some(err_ty) = self.builder.erased_box_try_err_ty(inner, propagation) {
                    let dyn_ty = self.builder.generator.std_error_trait_ty();
                    self.builder.collect_dynamic_impl_use(&dyn_ty, err_ty);
                }
                walk_expr(self, expr);
            }
            TExprKind::TypeId { ty } => {
                self.builder.collect_type_id(ty);
            }
            TExprKind::Await { .. } | TExprKind::AsyncBlockOn { .. } => {
                self.builder.collect_standard_error_code_dynamic();
                walk_expr(self, expr);
            }
            TExprKind::AsyncSelect { arms, .. } => {
                self.builder.collect_standard_error_code_dynamic();
                for arm in arms {
                    self.visit_expr(&arm.future);
                    self.builder.collect_ty_dynamic(&arm.future_output_ty);
                    self.visit_expr(&arm.body);
                }
            }
            TExprKind::AsyncSleep { output_ty, .. } => {
                self.builder.collect_standard_error_code_dynamic();
                self.builder.collect_ty_dynamic(output_ty);
                walk_expr(self, expr);
            }
            TExprKind::AsyncSpawn {
                task_output_ty,
                task_error_ty,
                ..
            } => {
                self.builder.collect_standard_error_code_dynamic();
                self.builder.collect_ty_dynamic(task_output_ty);
                self.builder.collect_ty_dynamic(task_error_ty);
                self.builder.collect_ty_dynamic(&std_task_ty(
                    &self.builder.generator.program.checked.resolved,
                    task_output_ty.clone(),
                    task_error_ty.clone(),
                ));
                self.builder.collect_ty_dynamic(&std_result_ty(
                    &self.builder.generator.program.checked.resolved,
                    task_output_ty.clone(),
                    task_error_ty.clone(),
                ));
                walk_expr(self, expr);
            }
            TExprKind::AsyncTaskCancel {
                task_output_ty,
                task_error_ty,
                ..
            }
            | TExprKind::AsyncTaskIsFinished {
                task_output_ty,
                task_error_ty,
                ..
            } => {
                self.builder.collect_standard_error_code_dynamic();
                self.builder.collect_ty_dynamic(task_output_ty);
                self.builder.collect_ty_dynamic(task_error_ty);
                self.builder.collect_ty_dynamic(&std_task_ty(
                    &self.builder.generator.program.checked.resolved,
                    task_output_ty.clone(),
                    task_error_ty.clone(),
                ));
                walk_expr(self, expr);
            }
            TExprKind::RetainClosure { source_ty, .. } => {
                self.builder.collect_ty_dynamic(source_ty);
                walk_expr(self, expr);
            }
            TExprKind::RawSliceFromPtr { elem_ty, .. } => {
                self.builder.collect_ty_dynamic(elem_ty);
                walk_expr(self, expr);
            }
            TExprKind::ActorSpawn { .. }
            | TExprKind::ActorSend { .. }
            | TExprKind::ActorStop { .. }
            | TExprKind::ActorJoin { .. } => {
                self.builder.collect_standard_error_code_dynamic();
                walk_expr(self, expr);
            }
            _ => walk_expr(self, expr),
        }
    }
}

struct SliceVisitor<'a, 'b, 'c> {
    builder: &'a mut CodegenPlanBuilder<'b, 'c>,
}

impl ThirVisitor for SliceVisitor<'_, '_, '_> {
    fn visit_stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::VarDecl { ty, init, .. } => {
                self.builder.collect_ty_slice(ty);
                if let Some(init) = init {
                    self.visit_expr(init);
                }
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_for_init(&mut self, init: &TForInit) {
        match init {
            TForInit::VarDecl { ty, init, .. } => {
                self.builder.collect_ty_slice(ty);
                if let Some(init) = init {
                    self.visit_expr(init);
                }
            }
            _ => walk_for_init(self, init),
        }
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        self.builder.collect_ty_slice(&expr.ty);
        match &expr.kind {
            TExprKind::Try {
                expr: inner,
                propagation,
            } => {
                if let Some(err_ty) = self.builder.erased_box_try_err_ty(inner, propagation) {
                    let dyn_ty = self.builder.generator.std_error_trait_ty();
                    self.builder.collect_ty_slice(&dyn_ty);
                    self.builder.collect_ty_slice(err_ty);
                }
                walk_expr(self, expr);
            }
            TExprKind::AsyncSelect { arms, .. } => {
                for arm in arms {
                    self.visit_expr(&arm.future);
                    self.builder.collect_ty_slice(&arm.future_output_ty);
                    self.visit_expr(&arm.body);
                }
            }
            TExprKind::AsyncSleep { output_ty, .. } => {
                self.builder.collect_ty_slice(output_ty);
                self.builder.collect_ty_slice(&std_error_code_ty(
                    &self.builder.generator.program.checked.resolved,
                ));
                let dyn_ty = self.builder.generator.std_error_trait_ty();
                self.builder.collect_ty_slice(&dyn_ty);
                walk_expr(self, expr);
            }
            TExprKind::AsyncSpawn {
                task_output_ty,
                task_error_ty,
                ..
            } => {
                self.builder.collect_ty_slice(task_output_ty);
                self.builder.collect_ty_slice(task_error_ty);
                self.builder.collect_ty_slice(&std_task_ty(
                    &self.builder.generator.program.checked.resolved,
                    task_output_ty.clone(),
                    task_error_ty.clone(),
                ));
                self.builder.collect_ty_slice(&std_error_code_ty(
                    &self.builder.generator.program.checked.resolved,
                ));
                let dyn_ty = self.builder.generator.std_error_trait_ty();
                self.builder.collect_ty_slice(&dyn_ty);
                walk_expr(self, expr);
            }
            TExprKind::AsyncTaskCancel {
                task_output_ty,
                task_error_ty,
                ..
            }
            | TExprKind::AsyncTaskIsFinished {
                task_output_ty,
                task_error_ty,
                ..
            } => {
                self.builder.collect_ty_slice(task_output_ty);
                self.builder.collect_ty_slice(task_error_ty);
                self.builder.collect_ty_slice(&std_task_ty(
                    &self.builder.generator.program.checked.resolved,
                    task_output_ty.clone(),
                    task_error_ty.clone(),
                ));
                self.builder.collect_ty_slice(&std_error_code_ty(
                    &self.builder.generator.program.checked.resolved,
                ));
                let dyn_ty = self.builder.generator.std_error_trait_ty();
                self.builder.collect_ty_slice(&dyn_ty);
                walk_expr(self, expr);
            }
            TExprKind::RetainClosure { source_ty, .. } => {
                self.builder.collect_ty_slice(source_ty);
                walk_expr(self, expr);
            }
            TExprKind::RawSliceFromPtr { elem_ty, .. } => {
                self.builder.collect_ty_slice(elem_ty);
                walk_expr(self, expr);
            }
            TExprKind::MakeDynamicInterface { concrete_ty, .. } => {
                self.builder.collect_ty_slice(concrete_ty);
                walk_expr(self, expr);
            }
            TExprKind::ErrorBox { concrete_ty, .. } => {
                let dyn_ty = self.builder.generator.std_error_trait_ty();
                self.builder.collect_ty_slice(&dyn_ty);
                self.builder.collect_ty_slice(concrete_ty);
                walk_expr(self, expr);
            }
            TExprKind::ReportBox { concrete_ty, .. } => {
                let dyn_ty = self.builder.generator.std_error_trait_ty();
                self.builder.collect_ty_slice(&dyn_ty);
                self.builder.collect_ty_slice(concrete_ty);
                walk_expr(self, expr);
            }
            TExprKind::MetaAsRefRepr { source_ty, .. }
            | TExprKind::MetaIntoRepr { source_ty, .. } => {
                self.builder.collect_ty_slice(source_ty);
                walk_expr(self, expr);
            }
            TExprKind::MetaFromRepr { target_ty, .. } => {
                self.builder.collect_ty_slice(target_ty);
                walk_expr(self, expr);
            }
            TExprKind::MetaSchema { source_ty } => {
                self.builder.collect_ty_slice(source_ty);
            }
            TExprKind::ActorSpawn {
                state_ty,
                handle_message_ty,
                message_ty,
                handler_ty,
                ..
            } => {
                self.builder.collect_ty_slice(state_ty);
                self.builder.collect_ty_slice(handle_message_ty);
                self.builder.collect_ty_slice(message_ty);
                self.builder.collect_ty_slice(handler_ty);
                walk_expr(self, expr);
            }
            TExprKind::ActorSend { message_ty, .. }
            | TExprKind::ActorStop { message_ty, .. }
            | TExprKind::ActorJoin { message_ty, .. } => {
                self.builder.collect_ty_slice(message_ty);
                walk_expr(self, expr);
            }
            TExprKind::TypeSize { ty }
            | TExprKind::TypeAlign { ty }
            | TExprKind::TypeNeedsGcScan { ty }
            | TExprKind::TypeId { ty } => {
                self.builder.collect_ty_slice(ty);
            }
            _ => walk_expr(self, expr),
        }
    }
}
