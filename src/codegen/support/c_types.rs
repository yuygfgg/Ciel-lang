use super::*;

const C_META_TYPE_TAG: &str = "CielMetaTypeTag";

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn c_return_decl(&self, ty: &Ty, name: &str) -> String {
        if ty.is_erased_value() {
            c_base_decl("void", name)
        } else if self.ty_needs_array_return_wrapper(ty) {
            c_base_decl(&self.array_return_type_name(ty), name)
        } else {
            self.c_decl(ty, name)
        }
    }

    pub(in crate::codegen) fn c_static_return_decl(&self, ty: &Ty, name: &str) -> String {
        format!("static {}", self.c_return_decl(ty, name))
    }

    pub(in crate::codegen) fn c_decl(&self, ty: &Ty, name: &str) -> String {
        self.c_decl_qualified(ty, name, false)
    }

    pub(in crate::codegen) fn lower_opaque_returns_in_ty(&self, ty: &Ty) -> Ty {
        match ty {
            Ty::OpaqueReturn { key, .. } => {
                let Some(concrete) = self.program.checked.opaque_returns.get(key) else {
                    return ty.clone();
                };
                self.lower_opaque_returns_in_ty(concrete)
            }
            _ => map_ty_children(ty, |child| self.lower_opaque_returns_in_ty(child)),
        }
    }

    pub(in crate::codegen) fn c_decl_qualified(
        &self,
        ty: &Ty,
        name: &str,
        top_const: bool,
    ) -> String {
        if matches!(ty, Ty::OpaqueReturn { .. }) {
            let concrete = self.lower_opaque_returns_in_ty(ty);
            if &concrete != ty {
                return self.c_decl_qualified(&concrete, name, top_const);
            }
        }
        match ty {
            Ty::Never => c_base_decl(&c_qualified_base("void", top_const), name),
            Ty::Void => c_base_decl(&c_qualified_base("void", top_const), name),
            Ty::Bool => c_base_decl(&c_qualified_base("bool", top_const), name),
            Ty::Char => c_base_decl(&c_qualified_base("char", top_const), name),
            Ty::I8 => c_base_decl(&c_qualified_base("int8_t", top_const), name),
            Ty::I16 => c_base_decl(&c_qualified_base("int16_t", top_const), name),
            Ty::I32 => c_base_decl(&c_qualified_base("int32_t", top_const), name),
            Ty::I64 => c_base_decl(&c_qualified_base("int64_t", top_const), name),
            Ty::U8 => c_base_decl(&c_qualified_base("uint8_t", top_const), name),
            Ty::U16 => c_base_decl(&c_qualified_base("uint16_t", top_const), name),
            Ty::U32 => c_base_decl(&c_qualified_base("uint32_t", top_const), name),
            Ty::U64 => c_base_decl(&c_qualified_base("uint64_t", top_const), name),
            Ty::Usize => c_base_decl(&c_qualified_base("size_t", top_const), name),
            Ty::F32 => c_base_decl(&c_qualified_base("float", top_const), name),
            Ty::F64 => c_base_decl(&c_qualified_base("double", top_const), name),
            Ty::CSpelling { spelling, .. } => {
                c_base_decl(&c_qualified_base(spelling, top_const), name)
            }
            Ty::Pointer {
                mutability, inner, ..
            } => {
                let ptr_name = c_pointer_name(name, top_const, matches!(**inner, Ty::Array { .. }));
                self.c_decl_qualified(inner, &ptr_name, mutability.is_read_only())
            }
            Ty::Array { len, elem } => {
                self.c_decl_qualified(elem, &format!("{name}[{len}]"), top_const)
            }
            Ty::Slice { mutability, elem } => c_base_decl(
                &c_qualified_base(&self.slice_name(*mutability, elem), top_const),
                name,
            ),
            Ty::Named {
                name: ty_name,
                args,
            } => {
                if self.is_std_meta_type_witness(ty_name, args) {
                    return c_base_decl(&c_qualified_base(C_META_TYPE_TAG, top_const), name);
                }
                if let Some(repr_ty) = self.meta_repr_marker_storage_ty(ty_name, args) {
                    return self.c_decl_qualified(&repr_ty, name, top_const);
                }
                if let Some(schema_ty) = self.meta_schema_marker_storage_ty(ty_name, args) {
                    return self.c_decl_qualified(&schema_ty, name, top_const);
                }
                c_base_decl(
                    &c_qualified_base(&self.c_named_type(ty_name, args), top_const),
                    name,
                )
            }
            Ty::GeneratedFuture { output, .. } => {
                self.c_decl_qualified(&std_future_ty((**output).clone()), name, top_const)
            }
            Ty::OpaqueState { base, .. } => self.c_decl_qualified(base, name, top_const),
            Ty::OpaqueReturn { .. } => c_base_decl(&c_qualified_base("void", top_const), name),
            Ty::DynamicInterface { .. } => c_base_decl(
                &c_qualified_base(&self.dynamic_type_name(ty), top_const),
                name,
            ),
            Ty::Closure { .. } | Ty::ClosureInstance { .. } => c_base_decl(
                &c_qualified_base(&self.closure_type_name(ty), top_const),
                name,
            ),
            Ty::Function { ret, params, .. } => {
                let params = params
                    .iter()
                    .filter(|ty| !ty.is_erased_value())
                    .enumerate()
                    .map(|(idx, ty)| self.c_decl(ty, &format!("arg{idx}")))
                    .collect::<Vec<_>>();
                let params = if params.is_empty() {
                    "void".to_string()
                } else {
                    params.join(", ")
                };
                self.c_return_decl(
                    ret,
                    &format!("{}({params})", c_function_pointer_name(name, top_const)),
                )
            }
            Ty::Hole(_) | Ty::Generic(_) | Ty::Unknown => {
                c_base_decl(&c_qualified_base("void", top_const), name)
            }
        }
    }

    pub(in crate::codegen) fn c_pointer_decl(&self, ty: &Ty, name: &str) -> String {
        self.c_decl(
            &Ty::Pointer {
                nullable: false,
                mutability: ViewMutability::Writable,
                inner: Box::new(ty.clone()),
            },
            name,
        )
    }

    pub(in crate::codegen) fn c_pointer_type(&self, ty: &Ty) -> String {
        self.c_type(&Ty::Pointer {
            nullable: false,
            mutability: ViewMutability::Writable,
            inner: Box::new(ty.clone()),
        })
    }

    pub(in crate::codegen) fn c_sizeof_type(&self, ty: &Ty) -> String {
        match ty {
            Ty::Array { len, elem } => format!("{}[{}]", self.c_type(elem), len),
            _ => self.c_type(ty),
        }
    }

    pub(in crate::codegen) fn c_array_alloc_expr(&self, elem: &Ty, len: &str) -> String {
        let allocator = if self.ty_can_carry_gc_pointer(elem) {
            "ciel_alloc_array"
        } else {
            "ciel_alloc_atomic_array"
        };
        format!("{allocator}(sizeof({}), {len})", self.c_sizeof_type(elem))
    }

    pub(in crate::codegen) fn c_object_alloc_expr(&self, ty: &Ty) -> String {
        let allocator = if self.ty_can_carry_gc_pointer(ty) {
            "ciel_alloc"
        } else {
            "ciel_alloc_atomic"
        };
        format!("{allocator}(sizeof({}))", self.c_sizeof_type(ty))
    }

    pub(in crate::codegen) fn ty_can_carry_gc_pointer(&self, ty: &Ty) -> bool {
        self.ty_can_carry_gc_pointer_inner(ty, &mut HashSet::new())
    }

    fn ty_can_carry_gc_pointer_inner(&self, ty: &Ty, visiting: &mut HashSet<String>) -> bool {
        if matches!(ty, Ty::OpaqueReturn { .. }) {
            let concrete = self.lower_opaque_returns_in_ty(ty);
            if &concrete != ty {
                return self.ty_can_carry_gc_pointer_inner(&concrete, visiting);
            }
        }
        match ty {
            Ty::Pointer { .. }
            | Ty::Slice { .. }
            | Ty::DynamicInterface { .. }
            | Ty::Function { .. }
            | Ty::Closure { .. }
            | Ty::ClosureInstance { .. }
            | Ty::GeneratedFuture { .. } => true,
            Ty::OpaqueState { base, .. } => self.ty_can_carry_gc_pointer_inner(base, visiting),
            Ty::Array { elem, .. } => self.ty_can_carry_gc_pointer_inner(elem, visiting),
            Ty::Named { name, args } => {
                if self.is_std_meta_type_witness(name, args) {
                    return false;
                }
                if let Some(repr_ty) = self.meta_repr_marker_storage_ty(name, args) {
                    return self.ty_can_carry_gc_pointer_inner(&repr_ty, visiting);
                }
                let instance_name = aggregate_instance_name(name, args);
                if !visiting.insert(instance_name.clone()) {
                    return true;
                }
                let can_carry = if let Some(strukt) = self
                    .program
                    .checked
                    .structs
                    .iter()
                    .find(|strukt| strukt.name == instance_name)
                {
                    strukt
                        .fields
                        .iter()
                        .any(|(_, ty)| self.ty_can_carry_gc_pointer_inner(ty, visiting))
                } else if let Some(enm) = self
                    .program
                    .checked
                    .enums
                    .iter()
                    .find(|enm| enm.name == instance_name)
                {
                    enm.variants.iter().any(|variant| {
                        variant
                            .payload
                            .iter()
                            .any(|ty| self.ty_can_carry_gc_pointer_inner(ty, visiting))
                    })
                } else {
                    true
                };
                visiting.remove(&instance_name);
                can_carry
            }
            Ty::OpaqueReturn { .. }
            | Ty::CSpelling { .. }
            | Ty::Generic(_)
            | Ty::Hole(_)
            | Ty::Unknown => true,
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
            | Ty::F64 => false,
        }
    }

    pub(in crate::codegen) fn meta_repr_marker_storage_ty(
        &self,
        name: &str,
        args: &[Ty],
    ) -> Option<Ty> {
        let borrowed = meta_repr_marker_name(name)?;
        let source = args.first()?;
        if args.len() != 1 {
            return Some(Ty::Unknown);
        }
        let span = crate::span::Span::new(crate::span::FileId(0), 0, 0);
        if borrowed {
            return Some(
                self.meta_borrowed_repr_ty(span, source)
                    .unwrap_or(Ty::Unknown),
            );
        }
        Some(
            self.meta_owned_leaf_repr_ty(span, source, source)
                .unwrap_or(Ty::Unknown),
        )
    }

    pub(in crate::codegen) fn meta_schema_marker_storage_ty(
        &self,
        name: &str,
        args: &[Ty],
    ) -> Option<Ty> {
        if !meta_schema_marker_name(name) {
            return None;
        }
        let source = args.first()?;
        if args.len() != 1 {
            return Some(Ty::Unknown);
        }
        let span = crate::span::Span::new(crate::span::FileId(0), 0, 0);
        Some(self.meta_schema_ty(span, source).unwrap_or(Ty::Unknown))
    }

    pub(in crate::codegen) fn c_type(&self, ty: &Ty) -> String {
        if let Ty::Named { name, args } = ty
            && self.is_std_meta_type_witness(name, args)
        {
            return C_META_TYPE_TAG.to_string();
        }
        if let Ty::Named { name, args } = ty
            && let Some(repr_ty) = self.meta_repr_marker_storage_ty(name, args)
        {
            return self.c_decl(&repr_ty, "").trim().to_string();
        }
        if let Ty::Named { name, args } = ty
            && let Some(schema_ty) = self.meta_schema_marker_storage_ty(name, args)
        {
            return self.c_decl(&schema_ty, "").trim().to_string();
        }
        self.c_decl(ty, "").trim().to_string()
    }

    pub(in crate::codegen) fn is_std_meta_type_witness(&self, name: &str, args: &[Ty]) -> bool {
        args.len() == 1
            && self.program.checked.resolved.defs.iter().any(|def| {
                nominal_type_name(&self.program.checked.resolved, def.id) == name
                    && std_id::is_std_meta_type(&self.program.checked.resolved, def.id, "Type")
            })
    }

    pub(in crate::codegen) fn c_named_type(&self, name: &str, args: &[Ty]) -> String {
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

    pub(in crate::codegen) fn array_return_type_name(&self, ty: &Ty) -> String {
        format!("CielArrayReturn_{}", mangle_ty_fragment(ty))
    }

    pub(in crate::codegen) fn ty_needs_array_return_wrapper(&self, ty: &Ty) -> bool {
        matches!(ty, Ty::Array { .. }) && !ty.is_erased_value()
    }

    pub(in crate::codegen) fn zero_value(&self, ty: &Ty) -> String {
        if matches!(ty, Ty::OpaqueReturn { .. }) {
            let concrete = self.lower_opaque_returns_in_ty(ty);
            if &concrete != ty {
                return self.zero_value(&concrete);
            }
        }
        if ty.is_erased_value() {
            return String::new();
        }
        match ty {
            Ty::Never => String::new(),
            Ty::Void => String::new(),
            Ty::Bool => "false".to_string(),
            Ty::Pointer { .. } | Ty::Function { .. } => "NULL".to_string(),
            Ty::I8
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
            | Ty::Char => "0".to_string(),
            Ty::Array { .. }
            | Ty::Slice { .. }
            | Ty::Named { .. }
            | Ty::GeneratedFuture { .. }
            | Ty::OpaqueState { .. }
            | Ty::DynamicInterface { .. }
            | Ty::OpaqueReturn { .. }
            | Ty::Closure { .. }
            | Ty::ClosureInstance { .. }
            | Ty::Hole(_)
            | Ty::Generic(_)
            | Ty::Unknown => {
                format!("({}){{0}}", self.c_type(ty))
            }
        }
    }
}
