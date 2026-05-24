use std::fmt;

use crate::{source::SourceMap, span::Span};

pub type DiagResult<T> = Result<T, Vec<Diagnostic>>;

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub span: Option<Span>,
    pub message: String,
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn new(span: impl Into<Option<Span>>, message: impl Into<String>) -> Self {
        Self {
            span: span.into(),
            message: message.into(),
            notes: Vec::new(),
        }
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

pub fn fail<T>(diagnostic: Diagnostic) -> DiagResult<T> {
    Err(vec![diagnostic])
}

pub fn render_diagnostics(source_map: &SourceMap, diagnostics: &[Diagnostic]) -> String {
    let mut out = String::new();
    for diagnostic in diagnostics {
        if let Some(span) = diagnostic.span {
            let path = source_map.file_path(span.file);
            let (line, col) = source_map.line_col(span.file, span.start);
            out.push_str(&format!(
                "{}:{}:{}: error: {}\n",
                path.display(),
                line,
                col,
                diagnostic.message
            ));
        } else {
            out.push_str(&format!("error: {}\n", diagnostic.message));
        }
        for note in &diagnostic.notes {
            out.push_str(&format!("  note: {note}\n"));
        }
    }
    out
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}
