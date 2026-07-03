use std::{fs, path::PathBuf};

use cielc::{
    CompileOptions, ast, compile_to_c,
    driver::compile_to_c_with_sources,
    hir::lower_to_hir_lossy,
    parser::parse_file,
    resolve::{ModuleId, ParsedModule, resolve_modules_lossy},
    span::FileId,
    typeck::type_check_lossy,
};
use cielc::{
    DiagnosticPhase,
    lexer::{TokenKind, lex_lossy},
    parser::parse_file_lossy,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ciel-error-recovery-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn lex_lossy_keeps_valid_tokens_after_lex_error() {
    let result = lex_lossy(FileId(0), "$ void ok() {}");
    assert!(!result.diagnostics.is_empty());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.phase == Some(DiagnosticPhase::Lex))
    );
    assert!(
        result
            .value
            .iter()
            .any(|token| token.kind == TokenKind::Error)
    );
    assert!(result.value.iter().any(|token| token.lexeme == "ok"));
}

#[test]
fn top_level_recovery_preserves_following_item() {
    let source = "type Broken = i64\ntype Kept = bool;\n";
    let tokens = lex_lossy(FileId(0), source).value;
    let parsed = parse_file_lossy(tokens);

    assert!(!parsed.diagnostics.is_empty());
    assert!(matches!(parsed.value.items[0].kind, ast::ItemKind::Error));
    assert!(parsed.value.items.iter().any(|item| matches!(
        &item.kind,
        ast::ItemKind::TypeAlias(alias) if alias.name.name == "Kept"
    )));
}

#[test]
fn statement_recovery_keeps_later_statements() {
    let source = r#"
        void main() {
            i64 value = ;
            return;
            i64 later = 1;
        }
    "#;
    let parsed = parse_file_lossy(lex_lossy(FileId(0), source).value);
    let function = parsed
        .value
        .items
        .iter()
        .find_map(|item| match &item.kind {
            ast::ItemKind::Function(function) => Some(function),
            _ => None,
        })
        .unwrap();
    let body = function.body.as_ref().unwrap();

    assert!(body.statements.len() >= 3);
    assert!(matches!(
        body.statements[0].kind,
        ast::StmtKind::VarDecl { .. }
    ));
    assert!(matches!(body.statements[1].kind, ast::StmtKind::Return(_)));
    assert!(body.statements.iter().any(|stmt| matches!(
        &stmt.kind,
        ast::StmtKind::VarDecl { name, .. } if name.name == "later"
    )));
}

#[test]
fn call_argument_recovery_inserts_error_argument() {
    let source = r#"
        void main() {
            foo(1, , 3);
        }
    "#;
    let parsed = parse_file_lossy(lex_lossy(FileId(0), source).value);
    let function = parsed
        .value
        .items
        .iter()
        .find_map(|item| match &item.kind {
            ast::ItemKind::Function(function) => Some(function),
            _ => None,
        })
        .unwrap();
    let body = function.body.as_ref().unwrap();
    let ast::StmtKind::Expr(expr) = &body.statements[0].kind else {
        panic!("expected expression statement");
    };
    let ast::ExprKind::Call { args, .. } = &expr.kind else {
        panic!("expected call expression");
    };

    assert_eq!(args.len(), 3);
    assert!(matches!(args[1].kind, ast::ExprKind::Error));
}

#[test]
fn array_literal_missing_comma_consumes_following_element() {
    let source = r#"
        void main() {
            [1 2];
        }
    "#;
    let parsed = parse_file_lossy(lex_lossy(FileId(0), source).value);
    let function = parsed
        .value
        .items
        .iter()
        .find_map(|item| match &item.kind {
            ast::ItemKind::Function(function) => Some(function),
            _ => None,
        })
        .unwrap();
    let body = function.body.as_ref().unwrap();
    let ast::StmtKind::Expr(expr) = &body.statements[0].kind else {
        panic!("expected expression statement");
    };
    let ast::ExprKind::ArrayLiteral(elements) = &expr.kind else {
        panic!("expected array literal");
    };

    assert!(parsed.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("expected `,` between array literal elements")
    }));
    assert_eq!(elements.len(), 2);
}

#[test]
fn struct_missing_opening_brace_preserves_following_item() {
    let source = "struct Broken\ntype Kept = bool;\n";
    let parsed = parse_file_lossy(lex_lossy(FileId(0), source).value);

    assert!(parsed.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("expected `{` in struct declaration")
    }));
    assert!(parsed.value.items.iter().any(|item| matches!(
        &item.kind,
        ast::ItemKind::Struct(decl) if decl.name.name == "Broken"
    )));
    assert!(parsed.value.items.iter().any(|item| matches!(
        &item.kind,
        ast::ItemKind::TypeAlias(alias) if alias.name.name == "Kept"
    )));
}

#[test]
fn postfix_type_arg_speculation_rewinds_for_relational_expression() {
    let source = r#"
        void main() {
            bool value = (x + 1) < y > z;
        }
    "#;
    let parsed = parse_file_lossy(lex_lossy(FileId(0), source).value);
    assert!(
        parsed.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        parsed.diagnostics
    );
    let function = parsed
        .value
        .items
        .iter()
        .find_map(|item| match &item.kind {
            ast::ItemKind::Function(function) => Some(function),
            _ => None,
        })
        .unwrap();
    let body = function.body.as_ref().unwrap();
    let ast::StmtKind::VarDecl {
        init: Some(expr), ..
    } = &body.statements[0].kind
    else {
        panic!("expected initialized variable declaration");
    };
    let ast::ExprKind::Binary {
        op: ast::BinaryOp::Gt,
        left,
        ..
    } = &expr.kind
    else {
        panic!("expected top-level `>` binary expression");
    };

    assert!(matches!(
        left.kind,
        ast::ExprKind::Binary {
            op: ast::BinaryOp::Lt,
            ..
        }
    ));
}

#[test]
fn strict_parse_still_rejects_recovery_diagnostics() {
    let source = "type Broken = i64\ntype Kept = bool;\n";
    let tokens = lex_lossy(FileId(0), source).value;
    assert!(parse_file(tokens).is_err());
}

#[test]
fn lossy_semantic_pipeline_keeps_later_function() {
    let source = r#"
        void broken() {
            return +;
        }

        i64 good() {
            return 1;
        }
    "#;
    let ast = parse_file_lossy(lex_lossy(FileId(0), source).value).value;
    let module = ParsedModule {
        id: ModuleId(0),
        path: PathBuf::from("memory.ciel"),
        std_export: None,
        import_paths: Vec::new(),
        ast,
    };
    let resolved = resolve_modules_lossy(vec![module]);
    let hir = lower_to_hir_lossy(resolved.value);
    let checked = type_check_lossy(hir.value);

    assert!(
        checked
            .value
            .functions
            .iter()
            .any(|function| function.name == "good")
    );
}

#[test]
fn strict_compile_path_rejects_recovery_diagnostics() {
    let dir = temp_dir("strict-compile");
    let path = dir.join("main.ciel");
    fs::write(&path, "type Broken = i64\ntype Kept = bool;\n").unwrap();

    let options = CompileOptions::new(&path).with_std_path(repo_root());
    assert!(compile_to_c(options).is_err());
}

#[test]
fn default_compile_path_reports_frontend_and_type_errors_together() {
    let dir = temp_dir("lossy-compile");
    let path = dir.join("main.ciel");
    fs::write(
        &path,
        r#"
            i64 main() {
                i64 value = ;
                return missing;
            }
        "#,
    )
    .unwrap();

    let options = CompileOptions::new(&path).with_std_path(repo_root());
    let Err((diagnostics, _source_map)) = compile_to_c_with_sources(options) else {
        panic!("expected compilation to fail");
    };

    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("expected initializer expression")
    }));
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("unresolved name `missing`") })
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.phase == Some(DiagnosticPhase::Parse))
    );
    assert!(
        diagnostics.iter().any(|diagnostic| matches!(
            diagnostic.phase,
            Some(DiagnosticPhase::Resolve | DiagnosticPhase::TypeCheck)
        )),
        "diagnostics: {:?}",
        diagnostics,
    );
}
