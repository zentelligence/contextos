use super::*;

#[test]
fn a_lowercase_hyphenated_slug_is_url_safe() {
    assert!(is_url_safe_slug("task-register"));
    assert!(is_url_safe_slug("a1-b2"));
}

#[test]
fn an_empty_slug_is_not_url_safe() {
    assert!(!is_url_safe_slug(""));
}

#[test]
fn a_slug_with_uppercase_or_symbols_is_not_url_safe() {
    assert!(!is_url_safe_slug("Task-Register"));
    assert!(!is_url_safe_slug("task_register"));
    assert!(!is_url_safe_slug("task register"));
    assert!(!is_url_safe_slug("task/register"));
}

#[test]
fn manifest_kind_js_parses_from_its_documented_toml_value() -> Result<(), Box<dyn std::error::Error>> {
    let raw: ManifestRaw = toml::from_str(
        r#"
            name = "Task Register Dashboard"
            slug = "task-register"
            kind = "js"
            entry = "index.html"
            mcp_servers = ["contextos"]
        "#,
    )?;
    assert_eq!(raw.name, "Task Register Dashboard");
    assert_eq!(raw.slug.as_deref(), Some("task-register"));
    assert_eq!(raw.kind, AppKind::Js);
    assert_eq!(raw.entry, "index.html");
    assert_eq!(raw.mcp_servers, vec!["contextos".to_owned()]);
    Ok(())
}

#[test]
fn manifest_kind_spa_and_htmx_both_parse() -> Result<(), Box<dyn std::error::Error>> {
    let spa: ManifestRaw = toml::from_str(
        r#"
            name = "Mock Website"
            kind = "spa"
            entry = "index.html"
        "#,
    )?;
    assert_eq!(spa.kind, AppKind::Spa);
    assert_eq!(spa.slug, None);
    assert!(
        spa.mcp_servers.is_empty(),
        "`spa.mcp_servers` should be empty, got {:?}",
        spa.mcp_servers
    );

    let htmx: ManifestRaw = toml::from_str(
        r#"
            name = "Live Widget"
            kind = "htmx"
            entry = "widget.html"
        "#,
    )?;
    assert_eq!(htmx.kind, AppKind::Htmx);
    Ok(())
}

#[test]
fn an_unknown_manifest_field_is_a_schema_violation() {
    let result: Result<ManifestRaw, _> = toml::from_str(
        r#"
            name = "App"
            kind = "js"
            entry = "index.html"
            surprise = "field"
        "#,
    );
    assert!(result.is_err());
}

/// `target` is removed from the schema; a manifest still declaring it (a
/// field from an earlier schema revision) is rejected as an unknown field,
/// not silently ignored.
#[test]
fn a_target_field_is_a_schema_violation() {
    let result: Result<ManifestRaw, _> = toml::from_str(
        r#"
            name = "App"
            kind = "js"
            entry = "index.html"
            target = "_blank"
        "#,
    );
    assert!(result.is_err());
}

#[test]
fn an_invalid_kind_value_is_a_schema_violation() {
    let result: Result<ManifestRaw, _> = toml::from_str(
        r#"
            name = "App"
            kind = "electron"
            entry = "index.html"
        "#,
    );
    assert!(result.is_err());
}

#[test]
fn a_missing_required_field_is_a_schema_violation() {
    let result: Result<ManifestRaw, _> = toml::from_str(
        r#"
            name = "App"
            kind = "spa"
        "#,
    );
    assert!(result.is_err());
}
