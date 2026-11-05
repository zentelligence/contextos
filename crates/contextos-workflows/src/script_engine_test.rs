use rhai::ParseErrorType;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A `.rhai` file outside any package, whose functions and constants would be usable if it could be imported.
fn outside_module() -> Result<(tempfile::TempDir, String), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    std::fs::write(
        dir.path().join("outside.rhai"),
        "fn secret() { \"read from outside the package\" }\nexport const VALUE = 42;\n",
    )?;
    let stem = dir.path().join("outside").to_string_lossy().replace('\\', "/");
    Ok((dir, stem))
}

#[test]
fn the_engine_alone_imports_only_the_built_in_library_and_never_a_file() -> TestResult {
    let (_dir, outside) = outside_module()?;

    for form in [
        format!("import \"{outside}\" as m; m::secret()"),
        "import \"outside\" as m; m::VALUE".to_owned(),
    ] {
        let refused = script_engine().eval::<rhai::Dynamic>(&form);

        assert!(
            matches!(refused, Err(ref error) if error.to_string().contains("names no module")),
            "{form}: {refused:?}"
        );
    }
    assert!(script_engine().eval::<bool>("import \"contextos:text\" as text; text::is_blank(\" \")")?);
    Ok(())
}

#[test]
fn every_form_of_eval_is_refused() {
    let forms = [
        "eval(\"40 + 2\")",
        "let code = \"40 + 2\"; eval(code)",
        "fn f(code) { eval(code) } f(\"40 + 2\")",
    ];

    for form in forms {
        // Each form evaluates its text in an engine left as Rhai builds it, so each is a real way of reaching `eval`.
        assert_eq!(
            Engine::new().eval::<i64>(form).ok(),
            Some(42),
            "{form} is not a form of eval"
        );

        let refused = match script_engine().compile(form) {
            Err(error) => refusal(&error.0) == Some(EVAL_REFUSED),
            Ok(ast) => script_engine().eval_ast::<rhai::Dynamic>(&ast).is_err(),
        };

        assert!(refused, "{form} was evaluated");
    }
}

/// `pairs` pairs of parentheses around a literal.
fn nested(pairs: usize) -> String {
    format!("let x = {}1{};", "(".repeat(pairs), ")".repeat(pairs))
}

/// Whether compiling `script` is refused for nesting too deep.
fn too_deep(script: &str) -> bool {
    matches!(script_engine().compile(script), Err(ref error) if matches!(*error.0, ParseErrorType::ExprTooDeep))
}

#[test]
fn every_build_of_the_server_has_the_same_limits() {
    let engine = script_engine();

    assert_eq!(engine.max_expr_depth(), MAX_EXPR_DEPTH);
    assert_eq!(engine.max_function_expr_depth(), MAX_FUNCTION_EXPR_DEPTH);
    assert_eq!(engine.max_call_levels(), MAX_CALL_LEVELS);
}

#[test]
fn nesting_is_allowed_exactly_up_to_the_limit_at_the_top_level_and_in_a_function() {
    // Each pair of parentheses is two levels of nesting, and the statement around them takes the rest.
    assert!(!too_deep(&nested(30)), "30 pairs at the top level");
    assert!(too_deep(&nested(31)), "31 pairs at the top level");
    assert!(
        !too_deep(&format!("fn f() {{ {} }}", nested(13))),
        "13 pairs in a function"
    );
    assert!(
        too_deep(&format!("fn f() {{ {} }}", nested(14))),
        "14 pairs in a function"
    );
}
