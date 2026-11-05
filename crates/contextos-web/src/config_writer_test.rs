use serde_json::json;

use super::*;

type BoxError = Box<dyn std::error::Error>;

const BASE: &str = r#"
[server]
bind = "127.0.0.1:7373"
static_dir = "./static"

[[mcp_server]]
name = "contextos"
transport = "stdio"
command = "contextos-mcp"
args = ["--config", "config.toml"]
"#;

fn entry(name: &str) -> Result<serde_json::Map<String, serde_json::Value>, BoxError> {
    json!({
        "transport": "stdio",
        "name": name,
        "command": "some-other-server",
        "args": ["--flag"],
    })
    .as_object()
    .cloned()
    .ok_or_else(|| "expected a JSON object".into())
}

#[test]
fn add_mcp_server_appends_a_new_entry_and_preserves_existing_comments() -> Result<(), BoxError> {
    let source = "# a hand-written comment\n".to_owned() + BASE;
    let mut document = WebConfigDocument::parse(&source)?;

    document.add_mcp_server(&entry("second")?)?;

    let rendered = document.render();
    assert!(rendered.contains("# a hand-written comment"));
    assert!(rendered.contains("name = \"second\""));
    assert_eq!(
        document.mcp_server_names(),
        vec!["contextos".to_owned(), "second".to_owned()]
    );
    Ok(())
}

#[test]
fn add_mcp_server_rejects_a_duplicate_name_and_leaves_the_document_unchanged() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    let before = document.render();

    let result = document.add_mcp_server(&entry("contextos")?);

    assert!(matches!(result, Err(WebConfigWriterError::Invalid { .. })));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn add_mcp_server_rejects_a_missing_required_field() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    let before = document.render();
    let mut incomplete = entry("second")?;
    incomplete.remove("command");

    let result = document.add_mcp_server(&incomplete);

    assert!(matches!(result, Err(WebConfigWriterError::Invalid { .. })));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn add_mcp_server_rejects_a_null_value() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    let before = document.render();
    let mut malformed = entry("second")?;
    malformed.insert("command".to_owned(), serde_json::Value::Null);

    let result = document.add_mcp_server(&malformed);

    assert!(matches!(result, Err(WebConfigWriterError::UnsupportedValue)));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn patch_mcp_server_merges_fields_and_leaves_others_untouched() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    let mut patch = serde_json::Map::new();
    patch.insert(
        "command".to_owned(),
        serde_json::Value::String("patched-command".to_owned()),
    );

    document.patch_mcp_server("contextos", &patch)?;

    let rendered = document.render();
    assert!(rendered.contains("command = \"patched-command\""));
    assert!(rendered.contains("name = \"contextos\""));
    Ok(())
}

#[test]
fn patch_mcp_server_on_an_unknown_name_is_rejected() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    let before = document.render();
    let patch = serde_json::Map::new();

    let result = document.patch_mcp_server("does-not-exist", &patch);

    assert!(matches!(
        result,
        Err(WebConfigWriterError::UnknownMcpServerName { name }) if name == "does-not-exist"
    ));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn replace_mcp_server_swaps_the_full_entry_including_its_name() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;

    document.replace_mcp_server("contextos", &entry("renamed")?)?;

    assert_eq!(document.mcp_server_names(), vec!["renamed".to_owned()]);
    let rendered = document.render();
    assert!(rendered.contains("command = \"some-other-server\""));
    Ok(())
}

#[test]
fn replace_mcp_server_rejects_a_result_with_a_duplicate_name() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    document.add_mcp_server(&entry("second")?)?;
    let before = document.render();

    let result = document.replace_mcp_server("second", &entry("contextos")?);

    assert!(matches!(result, Err(WebConfigWriterError::Invalid { .. })));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn remove_mcp_server_drops_the_named_entry() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    document.add_mcp_server(&entry("second")?)?;

    document.remove_mcp_server("second")?;

    assert_eq!(document.mcp_server_names(), vec!["contextos".to_owned()]);
    Ok(())
}

#[test]
fn remove_mcp_server_on_an_unknown_name_is_rejected_and_leaves_the_document_unchanged() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    let before = document.render();

    let result = document.remove_mcp_server("does-not-exist");

    assert!(matches!(
        result,
        Err(WebConfigWriterError::UnknownMcpServerName { name }) if name == "does-not-exist"
    ));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn patch_ui_creates_and_merges_the_server_ui_table() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    let mut patch = serde_json::Map::new();
    patch.insert("theme".to_owned(), serde_json::Value::String("dark".to_owned()));

    document.patch_ui(&patch)?;

    let rendered = document.render();
    assert!(rendered.contains("[server.ui]"));
    assert!(rendered.contains("theme = \"dark\""));
    // Existing `[server]` keys survive the nested-table insertion.
    assert!(rendered.contains("bind = \"127.0.0.1:7373\""));
    Ok(())
}

#[test]
fn an_edit_is_rejected_when_the_document_already_carries_an_invalid_bind_address() -> Result<(), BoxError> {
    let corrupt = BASE.replace(r#"bind = "127.0.0.1:7373""#, r#"bind = "not-a-socket-address""#);
    let mut document = WebConfigDocument::parse(&corrupt)?;
    let before = document.render();

    let result = document.add_mcp_server(&entry("second")?);

    assert!(matches!(result, Err(WebConfigWriterError::Invalid { .. })));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn parse_rejects_invalid_toml() {
    let result = WebConfigDocument::parse("not = [valid");
    assert!(matches!(result, Err(WebConfigWriterError::Toml { .. })));
}

fn server_input(bind: &str) -> WebServerSettingsInput {
    WebServerSettingsInput {
        bind: bind.to_owned(),
        static_dir: None,
        log_level: "info".to_owned(),
        log_file: String::new(),
        log_max_size_mb: None,
        log_rotate_daily: false,
        log_retention_days: None,
        log_retention_files: None,
    }
}

#[test]
fn server_settings_reads_back_the_current_scalar_values() -> Result<(), BoxError> {
    let source = r#"
[server]
bind = "127.0.0.1:9001"
log_level = "debug"
log_file = "/var/log/contextos-web.log"
log_max_size_mb = 20
log_rotate_daily = true
log_retention_files = 5
"#;
    let document = WebConfigDocument::parse(source)?;

    let view = document.server_settings();

    assert_eq!(view.bind.as_deref(), Some("127.0.0.1:9001"));
    assert_eq!(view.log_level.as_deref(), Some("debug"));
    assert_eq!(view.log_file.as_deref(), Some("/var/log/contextos-web.log"));
    assert_eq!(view.log_max_size_mb, Some(20));
    assert_eq!(view.log_rotate_daily, Some(true));
    assert_eq!(view.log_retention_files, Some(5));
    assert_eq!(view.log_retention_days, None);
    assert_eq!(view.static_dir, None);
    Ok(())
}

#[test]
fn set_server_settings_writes_managed_keys_and_prunes_unset_optionals() -> Result<(), BoxError> {
    let source = r#"
[server]
bind = "127.0.0.1:7373"
static_dir = "/old/static"
log_file = "/old.log"
log_rotate_daily = true
log_retention_days = 30
"#;
    let mut document = WebConfigDocument::parse(source)?;

    document.set_server_settings(&WebServerSettingsInput {
        bind: "127.0.0.1:8000".to_owned(),
        static_dir: None,
        log_level: "warn".to_owned(),
        log_file: String::new(),
        log_max_size_mb: None,
        log_rotate_daily: false,
        log_retention_days: None,
        log_retention_files: None,
    })?;

    let rendered = document.render();
    assert!(rendered.contains(r#"bind = "127.0.0.1:8000""#));
    assert!(rendered.contains(r#"log_level = "warn""#));
    assert!(!rendered.contains("static_dir"));
    assert!(!rendered.contains("log_rotate_daily"));
    assert!(!rendered.contains("log_retention_days"));
    Ok(())
}

#[test]
fn set_server_settings_rejects_a_non_loopback_bind_and_rolls_back() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    let before = document.render();

    let result = document.set_server_settings(&server_input("0.0.0.0:7373"));

    assert!(matches!(result, Err(WebConfigWriterError::Invalid { .. })));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn set_server_settings_keeps_a_preceding_server_ui_subtable_valid() -> Result<(), BoxError> {
    let source = r#"
[server]
[server.ui]
theme = "dark"
"#;
    let mut document = WebConfigDocument::parse(source)?;

    document.set_server_settings(&server_input("127.0.0.1:7373"))?;

    // The scalar keys must render above the `[server.ui]` header, so the
    // rendered document still parses as `bind` living under `[server]`.
    let reloaded: toml::Value = toml::from_str(&document.render())?;
    assert_eq!(reloaded["server"]["bind"].as_str(), Some("127.0.0.1:7373"));
    assert_eq!(reloaded["server"]["ui"]["theme"].as_str(), Some("dark"));
    Ok(())
}

#[test]
fn set_ui_strings_sets_new_keys_and_clears_the_ones_passed_none() -> Result<(), BoxError> {
    let source = r#"
[server]
bind = "127.0.0.1:7373"

[server.ui]
theme = "dark"
font = "serif"
"#;
    let mut document = WebConfigDocument::parse(source)?;

    document.set_ui_strings(&[("theme", Some("light")), ("font", None), ("size", Some("compact"))])?;

    assert_eq!(document.ui_string("theme").as_deref(), Some("light"));
    assert_eq!(document.ui_string("font"), None);
    assert_eq!(document.ui_string("size").as_deref(), Some("compact"));
    Ok(())
}

#[test]
fn mcp_server_entries_returns_the_configured_entries_as_typed_values() -> Result<(), BoxError> {
    let document = WebConfigDocument::parse(BASE)?;

    let entries = document.mcp_server_entries();

    assert_eq!(entries.len(), 1);
    assert!(matches!(
        &entries[0],
        McpServerConfig::Stdio { name, command, .. } if name == "contextos" && command == "contextos-mcp"
    ));
    Ok(())
}

const LEGACY_HTTP_ENTRY: &str = "\n[[mcp_server]]\nname = \"remote\"\ntransport = \"http\"\nendpoint = \"http://127.0.0.1:9000\"\ntoken_env = \"OLD_TOKEN\"\n";

#[test]
fn patching_an_auth_table_onto_a_legacy_entry_removes_its_top_level_token_env() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(&(BASE.to_owned() + LEGACY_HTTP_ENTRY))?;
    let patch = json!({ "auth": { "mode": "bearer", "credential_ref": "remote-token" } });

    document.patch_mcp_server("remote", patch.as_object().ok_or("expected a JSON object")?)?;

    let rendered = document.render();
    assert!(
        !rendered.contains("OLD_TOKEN"),
        "the superseded variable would come back if the table were removed: {rendered}"
    );
    assert!(rendered.contains("credential_ref = \"remote-token\""), "{rendered}");
    Ok(())
}

#[test]
fn patching_a_legacy_entry_without_an_auth_table_keeps_its_top_level_token_env() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(&(BASE.to_owned() + LEGACY_HTTP_ENTRY))?;
    let patch = json!({ "endpoint": "http://127.0.0.1:9001" });

    document.patch_mcp_server("remote", patch.as_object().ok_or("expected a JSON object")?)?;

    assert!(document.render().contains("token_env = \"OLD_TOKEN\""));
    Ok(())
}
