use std::{
    fs,
    path::{Path, PathBuf},
};

use cielc::tree_sitter_ciel;
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

const REQUIRED_PARSE_KINDS: &[&str] = &[
    "compile",
    "run",
    "host",
    "dependency",
    "manual",
    "known-fail-compile",
    "known-fail-cc",
    "known-fail-run",
];

const SKIPPED_KINDS: &[&str] = &["error", "known-fail-accepts"];

#[test]
fn positive_sources_parse_without_tree_sitter_errors() {
    let repo_root = repo_root();
    let cases_root = repo_root.join("tests/cases");
    let mut fixture_paths = collect_ciel_files(&cases_root);
    fixture_paths.sort();

    let mut cases = Vec::new();
    let mut metadata_errors = Vec::new();
    for path in fixture_paths {
        match parse_case(&repo_root, path) {
            Ok(test_case) => cases.push(test_case),
            Err(error) => metadata_errors.push(error),
        }
    }
    if !metadata_errors.is_empty() {
        panic!("{}", metadata_errors.join("\n"));
    }

    let unknown_kinds: Vec<_> = cases
        .iter()
        .filter(|test_case| {
            !REQUIRED_PARSE_KINDS.contains(&test_case.kind.as_str())
                && !SKIPPED_KINDS.contains(&test_case.kind.as_str())
        })
        .map(|test_case| {
            format!(
                "{}: unknown ciel-test kind `{}`",
                test_case.relative_path, test_case.kind
            )
        })
        .collect();
    if !unknown_kinds.is_empty() {
        panic!("{}", unknown_kinds.join("\n"));
    }

    let required_fixture_count = cases
        .iter()
        .filter(|test_case| REQUIRED_PARSE_KINDS.contains(&test_case.kind.as_str()))
        .count();
    let skipped_fixture_count = cases.len() - required_fixture_count;

    let mut required_cases: Vec<_> = cases
        .into_iter()
        .filter(|test_case| REQUIRED_PARSE_KINDS.contains(&test_case.kind.as_str()))
        .collect();
    for root_name in ["std", "examples"] {
        let root = repo_root.join(root_name);
        let mut paths = collect_ciel_files(&root);
        paths.sort();
        required_cases.extend(
            paths
                .into_iter()
                .map(|path| parse_positive_source(&repo_root, path)),
        );
    }
    required_cases.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));

    let mut parser = parser();
    let mut failures = Vec::new();
    for test_case in &required_cases {
        let tree = parser
            .parse(test_case.source.as_bytes(), None)
            .expect("Tree-sitter parser should return a tree");
        let mut problems = Vec::new();
        collect_parse_problems(tree.root_node(), &test_case.source, &mut problems);
        if !problems.is_empty() {
            failures.push(format!(
                "{} ({})\n{}",
                test_case.relative_path,
                test_case.kind,
                problems.join("\n")
            ));
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} Ciel file(s) produced Tree-sitter parse errors:\n\n{}",
            failures.len(),
            failures.join("\n\n")
        );
    }

    eprintln!(
        "Tree-sitter parsed {} positive Ciel file(s); skipped {} negative fixture file(s).",
        required_cases.len(),
        skipped_fixture_count
    );
}

#[test]
fn highlight_query_captures_contextual_tokens() {
    let source = [
        "import /std/async as async;",
        "import /std/resource as resource;",
        "import /std/message as message;",
        "import /std/derive as derive;",
        "import /std/derivable as derivable;",
        "async void f() {",
        "    plain_call();",
        "    await async::block_on(task);",
        "    biased select { case value = async::recv(rx): value; };",
        "}",
        "type T = async::Task<i64>;",
        "type F = i64 fn(i64);",
        "impl async::poll(i64 x) { return x; }",
        "interface I = async::Readable + PlainReadable;",
        "struct Box { i64 value; }",
        "resource struct FileBox<resource T> { T value; }",
        "export i64 box_len(*const Box box) = .len { return box->value; }",
        "export bool box_contains(i64 key, *const Box box) = box.contains { return true; }",
        "void selectors(Box @box) { box.len(); box.symbols::len(); box.contains(1); }",
        "void scoped_alias() { resource::scoped(fn_value); }",
        "enum Event { Data(i64), Timeout, }",
        "void match_one(Result<Event, Error> x) { switch (x) { case Result::Ok(Event::Data(value)): break; case Result::Ok(Event::Timeout): break; case Result::Err(_): break; } }",
        "void make_event() { Event::Data(1); Event::Timeout; }",
        "derive message::Message<Box>;",
        "unsafe derive message::share_handle_marker<Box>;",
        "derivable unsafe impl<T> clone_message(*const T value) { return Ok(*value); }",
        "unsafe derivable unsafe impl<T> share_handle_marker(*const T value) { return true; }",
        "",
    ]
    .join("\n");

    let mut parser = parser();
    let tree = parser
        .parse(source.as_bytes(), None)
        .expect("Tree-sitter parser should return a tree");
    let mut problems = Vec::new();
    collect_parse_problems(tree.root_node(), &source, &mut problems);
    if !problems.is_empty() {
        panic!(
            "highlighting test source did not parse cleanly:\n{}",
            problems.join("\n")
        );
    }

    let language = tree_sitter_ciel::language();
    let query = Query::new(&language, tree_sitter_ciel::HIGHLIGHTS_QUERY)
        .expect("highlight query should compile");
    let captures = query_captures(&query, tree.root_node(), &source);

    assert_eq!(
        capture_positions(&captures, "keyword", "async"),
        ["5:0"],
        "`async` should only be a query keyword capture in the async function modifier"
    );
    assert_eq!(
        capture_positions(&captures, "keyword", "resource"),
        ["15:0", "15:24"],
        "`resource` should only be a query keyword capture in resource modifier positions"
    );
    assert_eq!(
        capture_positions(&captures, "keyword", "derive"),
        ["23:0", "24:7"],
        "`derive` should only be a query keyword capture in derive declarations"
    );
    assert_eq!(
        capture_positions(&captures, "keyword", "derivable"),
        ["25:0", "26:7"],
        "`derivable` should only be a query keyword capture in derivable impl declarations"
    );

    for text in ["await", "biased", "select", "fn"] {
        assert_capture(&captures, "keyword", text);
    }
    for text in ["plain_call", "block_on", "recv", "len", "contains"] {
        assert_capture(&captures, "function.call", text);
    }
    for text in ["poll", "len", "contains"] {
        assert_capture(&captures, "function", text);
    }
    for text in [
        "Task",
        "Box",
        "Readable",
        "PlainReadable",
        "Message",
        "share_handle_marker",
    ] {
        assert_capture(&captures, "type", text);
    }
    assert_capture(&captures, "type.parameter", "T");
    for text in ["Ok", "Err", "Data", "Timeout"] {
        assert_capture(&captures, "constant", text);
    }
}

#[test]
fn pointer_declarations_and_deref_assignments_are_structured() {
    let source = [
        "struct Box { i64 slot; }",
        "struct Refs { *i64 item0; *i64 item1; }",
        "void probe(*Box box, Refs refs, *i64 ptr, *[4]i64 out) {",
        "    *i64 local = ptr;",
        "    *Box named = box;",
        "    *const Box view = box;",
        "    ?*_ maybe = null;",
        "    *refs.item1 = 11;",
        "    *box->slot = 12;",
        "    *(ptr) = 13;",
        "    (*out)[0] = 14;",
        "}",
        "",
    ]
    .join("\n");

    let mut parser = parser();
    let tree = parser
        .parse(source.as_bytes(), None)
        .expect("Tree-sitter parser should return a tree");
    let mut problems = Vec::new();
    collect_parse_problems(tree.root_node(), &source, &mut problems);
    if !problems.is_empty() {
        panic!(
            "pointer structure test source did not parse cleanly:\n{}\n\n{}",
            problems.join("\n"),
            tree.root_node().to_sexp()
        );
    }

    let sexp = tree.root_node().to_sexp();
    assert!(
        sexp.contains("(pointer_var_declaration_clause type: (pointer_declaration_type"),
        "pointer local declarations should expose structured type nodes:\n{sexp}"
    );
    assert!(
        !sexp.contains("pointer_declaration_head"),
        "pointer declarations should not use the legacy tokenized head:\n{sexp}"
    );

    let mut targets = Vec::new();
    collect_node_texts(
        tree.root_node(),
        &source,
        "deref_assignment_target",
        &mut targets,
    );
    assert_eq!(targets, ["*refs.item1", "*box->slot", "*(ptr)"]);
    assert!(
        targets.iter().all(|target| !target.contains('=')),
        "deref assignment targets should not consume the assignment operator"
    );
}

#[derive(Debug)]
struct TestCase {
    relative_path: String,
    kind: String,
    source: String,
}

#[derive(Debug)]
struct Capture {
    name: String,
    text: String,
    row: usize,
    column: usize,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn parser() -> Parser {
    let mut parser = Parser::new();
    let language = tree_sitter_ciel::language();
    parser
        .set_language(&language)
        .expect("Ciel Tree-sitter language should be compatible");
    parser
}

fn collect_ciel_files(dir: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    collect_ciel_files_inner(dir, &mut result);
    result
}

fn collect_ciel_files_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("failed to read `{}`: {error}", dir.display()))
    {
        let entry = entry
            .unwrap_or_else(|error| panic!("failed to read entry in `{}`: {error}", dir.display()));
        let path = entry.path();
        if path.is_dir() {
            collect_ciel_files_inner(&path, out);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("ciel") {
            out.push(path);
        }
    }
}

fn parse_case(repo_root: &Path, path: PathBuf) -> Result<TestCase, String> {
    let source = fs::read_to_string(&path).map_err(|error| {
        format!(
            "{}: failed to read source: {error}",
            relative_to(repo_root, &path)
        )
    })?;
    let mut kind = None;
    for line in source.lines() {
        let Some(comment) = line.trim_start().strip_prefix("//") else {
            continue;
        };
        let Some((key, raw_value)) = comment.trim_start().split_once(':') else {
            continue;
        };
        if key.trim() == "ciel-test" {
            if kind.is_some() {
                return Err(format!(
                    "{}: duplicate ciel-test metadata",
                    relative_to(repo_root, &path)
                ));
            }
            kind = Some(raw_value.trim().to_string());
        }
    }

    let relative_path = relative_to(repo_root, &path);
    let kind =
        kind.ok_or_else(|| format!("{relative_path}: missing // ciel-test: ... metadata"))?;
    Ok(TestCase {
        relative_path,
        kind,
        source,
    })
}

fn parse_positive_source(repo_root: &Path, path: PathBuf) -> TestCase {
    let source = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{}: failed to read source: {error}",
            relative_to(repo_root, &path)
        )
    });
    TestCase {
        relative_path: relative_to(repo_root, &path),
        kind: "source".to_string(),
        source,
    }
}

fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn collect_parse_problems(node: Node<'_>, source: &str, problems: &mut Vec<String>) {
    if node.kind() == "ERROR" || node.is_missing() {
        problems.push(format_problem(node, source));
    }

    for index in 0..node.child_count() {
        if let Some(child) = node.child(index as u32) {
            collect_parse_problems(child, source, problems);
        }
    }
}

fn collect_node_texts(node: Node<'_>, source: &str, kind: &str, out: &mut Vec<String>) {
    if node.kind() == kind {
        out.push(
            node.utf8_text(source.as_bytes())
                .expect("node text should be UTF-8")
                .to_string(),
        );
    }

    for index in 0..node.child_count() {
        if let Some(child) = node.child(index as u32) {
            collect_node_texts(child, source, kind, out);
        }
    }
}

fn format_problem(node: Node<'_>, source: &str) -> String {
    let start = node.start_position();
    let end = node.end_position();
    let location = format!(
        "{}:{}-{}:{}",
        start.row + 1,
        start.column + 1,
        end.row + 1,
        end.column + 1
    );
    let text = node
        .utf8_text(source.as_bytes())
        .ok()
        .filter(|text| !text.is_empty())
        .map(|text| format!(" {:?}", truncate(text, 80)))
        .unwrap_or_default();
    format!("  {} at {location}{text}", node.kind())
}

fn truncate(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        return value.to_string();
    }
    let cutoff = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= max_len - 3)
        .last()
        .unwrap_or(0);
    format!("{}...", &value[..cutoff])
}

fn query_captures(query: &Query, root: Node<'_>, source: &str) -> Vec<Capture> {
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.captures(query, root, source.as_bytes());
    let mut captures = Vec::new();
    while let Some((query_match, capture_index)) = matches.next() {
        let capture = query_match.captures[*capture_index];
        let position = capture.node.start_position();
        captures.push(Capture {
            name: capture_names[capture.index as usize].to_string(),
            text: capture
                .node
                .utf8_text(source.as_bytes())
                .expect("capture text should be UTF-8")
                .to_string(),
            row: position.row,
            column: position.column,
        });
    }
    captures
}

fn capture_positions(captures: &[Capture], name: &str, text: &str) -> Vec<String> {
    captures
        .iter()
        .filter(|capture| capture.name == name && capture.text == text)
        .map(|capture| format!("{}:{}", capture.row, capture.column))
        .collect()
}

fn assert_capture(captures: &[Capture], name: &str, text: &str) {
    assert!(
        captures
            .iter()
            .any(|capture| capture.name == name && capture.text == text),
        "expected query {name} capture for {text}"
    );
}
