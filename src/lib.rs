pub mod ast;
pub mod codegen;
pub mod diagnostic;
pub mod driver;
pub mod escape;
pub mod hir;
pub mod lexer;
pub mod mono;
pub mod parser;
pub mod resolve;
pub mod source;
pub mod span;
pub mod thir;
pub mod typeck;
pub mod types;

pub use diagnostic::{DiagResult, Diagnostic};
pub use driver::{CompileOptions, compile_to_c};
