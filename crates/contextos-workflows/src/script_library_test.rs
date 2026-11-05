use super::*;
use crate::script_engine::script_engine;

/// Evaluates `body` after importing `module` as `m`, in the engine every script runs in.
fn with(module: &str, body: &str) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
    script_engine().eval::<Dynamic>(&format!("import \"{module}\" as m;\n{body}"))
}

#[test]
fn every_listed_built_in_module_can_be_imported_and_nothing_else_under_contextos() {
    for name in BUILT_IN_MODULES {
        assert!(built_in_module(name).is_some(), "{name}");
    }
    assert!(built_in_module("contextos:missing").is_none());
    assert!(built_in_module("text").is_none());
}

#[test]
fn text_functions_return_new_strings_and_count_characters() -> Result<(), Box<rhai::EvalAltResult>> {
    assert_eq!(
        with(
            "contextos:text",
            r#"let t = "a-b-c"; let r = m::replaced(t, "-", "+"); [t, r]"#
        )?
        .into_typed_array::<String>()?,
        ["a-b-c", "a+b+c"]
    );
    assert_eq!(
        with("contextos:text", r#"m::replaced("abc", "", "x")"#)?.into_string()?,
        "abc"
    );
    assert_eq!(
        with("contextos:text", r#"m::last_index_of("é/a/b", "/")"#)?.as_int()?,
        3
    );
    assert_eq!(with("contextos:text", r#"m::last_index_of("abc", "/")"#)?.as_int()?, -1);
    assert_eq!(
        with("contextos:text", r#"m::trimmed("  a b \n")"#)?.into_string()?,
        "a b"
    );
    assert_eq!(
        with("contextos:text", r#"m::trimmed_start("  a ")"#)?.into_string()?,
        "a "
    );
    assert_eq!(
        with("contextos:text", r#"m::trimmed_end("  a \r\n")"#)?.into_string()?,
        "  a"
    );
    assert!(with("contextos:text", r#"m::is_blank(" \t\n")"#)?.as_bool()?);
    assert!(!with("contextos:text", r#"m::is_blank(" x ")"#)?.as_bool()?);
    assert_eq!(
        with("contextos:text", r#"m::lines("one\r\ntwo\n\nfour\n")"#)?.into_typed_array::<String>()?,
        ["one", "two", "", "four"]
    );
    Ok(())
}

#[test]
fn path_functions_split_slash_separated_paths_as_text() {
    assert_eq!(parent("a/b/c.md"), "a/b");
    assert_eq!(parent("c.md"), "");
    assert_eq!(name("a/b/c.md"), "c.md");
    assert_eq!(name("c.md"), "c.md");
    assert_eq!(stem("a/b/c.tar.gz"), "c.tar");
    assert_eq!(stem("a/.gitignore"), ".gitignore");
    assert_eq!(extension("a/b/c.md"), "md");
    assert_eq!(extension("a/.gitignore"), "");
    assert_eq!(extension("a/b/readme"), "");
    assert_eq!(join("a/b/", "/c.md"), "a/b/c.md");
    assert_eq!(join("", "c.md"), "c.md");
}

#[test]
fn path_functions_are_reached_through_the_module() -> Result<(), Box<rhai::EvalAltResult>> {
    assert_eq!(
        with(
            "contextos:path",
            r#"[m::parent("a/b.md"), m::name("a/b.md"), m::stem("a/b.md"), m::extension("a/b.md"), m::join("a", "b.md")]"#
        )?
        .into_typed_array::<String>()?,
        ["a", "b.md", "b", "md", "a/b.md"]
    );
    Ok(())
}

#[test]
fn a_quoted_value_reads_back_as_the_same_toml_string() -> Result<(), Box<dyn std::error::Error>> {
    for value in [
        "plain",
        "a \"quote\" and a \\ backslash",
        "tab\tline\nreturn\r",
        "bell\u{7} del\u{7f} é",
    ] {
        let parsed: toml::Table = toml::from_str(&format!("value = {}", quoted(value)))?;

        assert_eq!(
            parsed.get("value").and_then(toml::Value::as_str),
            Some(value),
            "{value:?}"
        );
    }
    assert_eq!(
        with("contextos:toml", r#"m::quoted("say \"hi\"")"#)?.into_string()?,
        "\"say \\\"hi\\\"\""
    );
    Ok(())
}
