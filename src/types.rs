use std::fmt;

use crate::ast::{PrimitiveType, Type, TypeKind};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    Hole(usize),
    Never,
    Void,
    Bool,
    Char,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Usize,
    F32,
    F64,
    CSpelling {
        abi: String,
        spelling: String,
    },
    Pointer {
        nullable: bool,
        inner: Box<Ty>,
    },
    Const(Box<Ty>),
    Array {
        len: usize,
        elem: Box<Ty>,
    },
    Slice(Box<Ty>),
    Named {
        name: String,
        args: Vec<Ty>,
    },
    Generic(String),
    DynamicInterface {
        name: String,
        args: Vec<Ty>,
    },
    Function {
        abi: Option<String>,
        ret: Box<Ty>,
        params: Vec<Ty>,
    },
    Closure {
        ret: Box<Ty>,
        params: Vec<Ty>,
    },
    ClosureInstance {
        id: usize,
        ret: Box<Ty>,
        params: Vec<Ty>,
        captures: Vec<Ty>,
    },
    Unknown,
}

impl Ty {
    pub fn from_ast(ty: &Type) -> Self {
        match &ty.kind {
            TypeKind::Never => Ty::Never,
            TypeKind::Hole => Ty::Unknown,
            TypeKind::Void => Ty::Void,
            TypeKind::Primitive(primitive) => match primitive {
                PrimitiveType::Bool => Ty::Bool,
                PrimitiveType::Char => Ty::Char,
                PrimitiveType::I8 => Ty::I8,
                PrimitiveType::I16 => Ty::I16,
                PrimitiveType::I32 => Ty::I32,
                PrimitiveType::I64 => Ty::I64,
                PrimitiveType::U8 => Ty::U8,
                PrimitiveType::U16 => Ty::U16,
                PrimitiveType::U32 => Ty::U32,
                PrimitiveType::U64 => Ty::U64,
                PrimitiveType::Usize => Ty::Usize,
                PrimitiveType::F32 => Ty::F32,
                PrimitiveType::F64 => Ty::F64,
            },
            TypeKind::Named(path, args) => Ty::Named {
                name: path
                    .last()
                    .map(|name| name.name.clone())
                    .unwrap_or_default(),
                args: args.iter().map(Ty::from_ast).collect(),
            },
            TypeKind::Pointer { nullable, inner } => Ty::Pointer {
                nullable: *nullable,
                inner: Box::new(Ty::from_ast(inner)),
            },
            TypeKind::Const(inner) => Ty::Const(Box::new(Ty::from_ast(inner))),
            TypeKind::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(Ty::from_ast(elem)),
            },
            TypeKind::Slice(elem) => Ty::Slice(Box::new(Ty::from_ast(elem))),
            TypeKind::Function { abi, ret, params } => Ty::Function {
                abi: abi.clone(),
                ret: Box::new(Ty::from_ast(ret)),
                params: params.iter().map(Ty::from_ast).collect(),
            },
            TypeKind::Closure { ret, params } => Ty::Closure {
                ret: Box::new(Ty::from_ast(ret)),
                params: params.iter().map(Ty::from_ast).collect(),
            },
        }
    }

    pub fn is_integer(&self) -> bool {
        matches!(
            self.unqualified(),
            Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize
        )
    }

    pub fn is_signed_integer(&self) -> bool {
        matches!(self.unqualified(), Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64)
    }

    pub fn is_numeric(&self) -> bool {
        self.is_integer() || matches!(self.unqualified(), Ty::F32 | Ty::F64)
    }

    pub fn is_void(&self) -> bool {
        matches!(self.unqualified(), Ty::Void)
    }

    pub fn is_erased_value(&self) -> bool {
        match self.unqualified() {
            Ty::Void => true,
            Ty::Array { elem, .. } => elem.is_erased_value(),
            _ => false,
        }
    }

    pub fn is_never(&self) -> bool {
        matches!(self.unqualified(), Ty::Never)
    }

    pub fn unqualified(&self) -> &Ty {
        match self {
            Ty::Const(inner) => inner.unqualified(),
            other => other,
        }
    }

    pub fn pointer_to(inner: Ty) -> Self {
        Ty::Pointer {
            nullable: false,
            inner: Box::new(inner),
        }
    }

    pub fn nullable_pointer_to(inner: Ty) -> Self {
        Ty::Pointer {
            nullable: true,
            inner: Box::new(inner),
        }
    }

    pub fn can_assign_from(&self, source: &Ty) -> bool {
        if matches!(self.unqualified(), Ty::Hole(_)) || matches!(source.unqualified(), Ty::Hole(_))
        {
            return true;
        }
        if source.unqualified().is_never() {
            return true;
        }
        if self.unqualified() == source.unqualified() {
            return true;
        }
        matches!(
            (self.unqualified(), source.unqualified()),
            (
                Ty::Pointer {
                    nullable: true,
                    inner: expected,
                },
                Ty::Pointer {
                    nullable: false,
                    inner: actual,
                },
            ) if expected.can_assign_from(actual)
        ) || matches!(
            (self.unqualified(), source.unqualified()),
            (Ty::Slice(expected), Ty::Array { elem: actual, .. }) if expected.can_assign_from(actual)
        ) || matches!(
            (self.unqualified(), source.unqualified()),
            (
                Ty::Pointer {
                    nullable: expected_nullable,
                    inner: expected,
                },
                Ty::Pointer {
                    nullable: actual_nullable,
                    inner: actual,
                },
        ) if expected_nullable == actual_nullable && expected.can_assign_from(actual)
        ) || matches!(
            (self.unqualified(), source.unqualified()),
            (Ty::Slice(expected), Ty::Slice(actual)) if expected.can_assign_from(actual)
        ) || matches!(
            (self.unqualified(), source.unqualified()),
            (
                Ty::Closure {
                    ret: expected_ret,
                    params: expected_params,
                },
                Ty::Closure {
                    ret: actual_ret,
                    params: actual_params,
                },
            ) if expected_params == actual_params && expected_ret.can_assign_from(actual_ret)
        ) || matches!(
            (self.unqualified(), source.unqualified()),
            (
                Ty::Closure {
                    ret: expected_ret,
                    params: expected_params,
                },
                Ty::ClosureInstance {
                    ret: actual_ret,
                    params: actual_params,
                    ..
                },
            ) if expected_params == actual_params && expected_ret.can_assign_from(actual_ret)
        )
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Never => write!(f, "never"),
            Ty::Hole(_) => write!(f, "_"),
            Ty::Void => write!(f, "void"),
            Ty::Bool => write!(f, "bool"),
            Ty::Char => write!(f, "char"),
            Ty::I8 => write!(f, "i8"),
            Ty::I16 => write!(f, "i16"),
            Ty::I32 => write!(f, "i32"),
            Ty::I64 => write!(f, "i64"),
            Ty::U8 => write!(f, "u8"),
            Ty::U16 => write!(f, "u16"),
            Ty::U32 => write!(f, "u32"),
            Ty::U64 => write!(f, "u64"),
            Ty::Usize => write!(f, "usize"),
            Ty::F32 => write!(f, "f32"),
            Ty::F64 => write!(f, "f64"),
            Ty::CSpelling { spelling, .. } => write!(f, "{spelling}"),
            Ty::Pointer { nullable, inner } => {
                if *nullable {
                    write!(f, "?*{inner}")
                } else {
                    write!(f, "*{inner}")
                }
            }
            Ty::Const(inner) => write!(f, "const {inner}"),
            Ty::Array { len, elem } => write!(f, "[{len}]{elem}"),
            Ty::Slice(elem) => write!(f, "[]{elem}"),
            Ty::Named { name, args } => {
                if args.is_empty() {
                    write!(f, "{name}")
                } else {
                    write!(
                        f,
                        "{}<{}>",
                        name,
                        args.iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Ty::Generic(name) => write!(f, "{name}"),
            Ty::DynamicInterface { name, args } => {
                if args.is_empty() {
                    write!(f, "{name}")
                } else {
                    write!(
                        f,
                        "{}<{}>",
                        name,
                        args.iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Ty::Function { abi, ret, params } => {
                if let Some(abi) = abi {
                    write!(f, "extern \"{abi}\" ")?;
                }
                write!(
                    f,
                    "{} fn({})",
                    ret,
                    params
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            Ty::Closure { ret, params } => {
                write!(
                    f,
                    "{} |({})|",
                    ret,
                    params
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            Ty::ClosureInstance {
                id, ret, params, ..
            } => {
                write!(
                    f,
                    "closure#{id}<{} |({})|>",
                    ret,
                    params
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            Ty::Unknown => write!(f, "<unknown>"),
        }
    }
}
