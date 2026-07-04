use tree_sitter_language::LanguageFn;

pub const HIGHLIGHTS_QUERY: &str = include_str!("../tree-sitter-ciel/highlights.scm");

pub const HIGHLIGHT_NAMES: &[&str] = &[
    "keyword",
    "type.builtin",
    "boolean",
    "constant.builtin",
    "number",
    "number.float",
    "string",
    "string.special",
    "comment",
    "function",
    "function.call",
    "type",
    "type.definition",
    "type.parameter",
    "property.definition",
    "property",
    "constant",
    "variable.parameter",
    "variable",
    "namespace",
    "operator",
];

unsafe extern "C" {
    fn tree_sitter_ciel() -> *const ();
}

pub fn language() -> tree_sitter::Language {
    let language_fn = unsafe { LanguageFn::from_raw(tree_sitter_ciel) };
    tree_sitter::Language::new(language_fn)
}
