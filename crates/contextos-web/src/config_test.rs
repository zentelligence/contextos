use std::fs;

use tempfile::tempdir;

use super::*;

fn write(dir: &std::path::Path, name: &str, contents: &str) -> std::io::Result<std::path::PathBuf> {
    let path = dir.join(name);
    fs::write(&path, contents)?;
    Ok(path)
}

#[test]
fn defaults_apply_when_the_server_table_is_absent() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "stdio"
            name = "contextos"
            command = "contextos-mcp"
        "#,
    )?;

    let config = load_web_config(&path)?;

    assert_eq!(config.server.bind, "127.0.0.1:7373");
    assert_eq!(config.server.static_dir, None);
    assert_eq!(config.mcp_servers.len(), 1);
    assert_eq!(config.mcp_servers[0].name(), "contextos");
    Ok(())
}

#[test]
fn a_stdio_mcp_server_entry_parses_command_and_args() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "stdio"
            name = "contextos"
            command = "contextos-mcp"
            args = ["--config", "/tmp/config.toml"]
        "#,
    )?;

    let config = load_web_config(&path)?;

    let McpServerConfig::Stdio { name, command, args } = &config.mcp_servers[0] else {
        return Err("expected a stdio entry".into());
    };
    assert_eq!(name, "contextos");
    assert_eq!(command, "contextos-mcp");
    assert_eq!(args, &["--config", "/tmp/config.toml"]);
    Ok(())
}

#[test]
fn an_http_mcp_server_entry_parses_endpoint_and_token_env() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "stdio"
            name = "contextos"
            command = "contextos-mcp"

            [[mcp_server]]
            transport = "http"
            name = "some-other-server"
            endpoint = "http://127.0.0.1:9000"
            token_env = "SOME_OTHER_SERVER_TOKEN"
        "#,
    )?;

    let config = load_web_config(&path)?;

    let McpServerConfig::Http {
        name,
        endpoint,
        token_env,
        auth,
        headers,
    } = &config.mcp_servers[1]
    else {
        return Err("expected an http entry".into());
    };
    assert_eq!(name, "some-other-server");
    assert_eq!(endpoint, "http://127.0.0.1:9000");
    assert_eq!(token_env.as_deref(), Some("SOME_OTHER_SERVER_TOKEN"));
    // Non-regression: a bare top-level `token_env` with no `[mcp_server.auth]`
    // table continues to mean exactly what it meant before `auth`/`headers`
    // existed on this variant.
    assert_eq!(*auth, None);
    assert!(headers.is_empty());
    Ok(())
}

#[test]
fn bearer_auth_via_token_env_parses_and_leaves_credential_ref_unset() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "http"
            name = "some-bearer-server"
            endpoint = "https://services.example.invalid/mcp/"

            [mcp_server.auth]
            mode = "bearer"
            token_env = "SOME_BEARER_SERVER_TOKEN"
        "#,
    )?;

    let config = load_web_config(&path)?;

    let McpServerConfig::Http { auth, .. } = &config.mcp_servers[0] else {
        return Err("expected an http entry".into());
    };
    assert_eq!(
        *auth,
        Some(McpServerAuth::Bearer {
            token_env: Some("SOME_BEARER_SERVER_TOKEN".to_owned()),
            credential_ref: None,
        })
    );
    Ok(())
}

#[test]
fn bearer_auth_via_credential_ref_parses_and_leaves_token_env_unset() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "http"
            name = "some-bearer-server"
            endpoint = "https://services.example.invalid/mcp/"

            [mcp_server.auth]
            mode = "bearer"
            credential_ref = "some-bearer-server-token"
        "#,
    )?;

    let config = load_web_config(&path)?;

    let McpServerConfig::Http { auth, .. } = &config.mcp_servers[0] else {
        return Err("expected an http entry".into());
    };
    assert_eq!(
        *auth,
        Some(McpServerAuth::Bearer {
            token_env: None,
            credential_ref: Some("some-bearer-server-token".to_owned()),
        })
    );
    // The two credential sources are distinguishable, not merged into one
    // shape: this parse is not equal to the `token_env`-only parse.
    assert_ne!(
        *auth,
        Some(McpServerAuth::Bearer {
            token_env: Some("some-bearer-server-token".to_owned()),
            credential_ref: None,
        })
    );
    Ok(())
}

#[test]
fn bearer_auth_rejects_both_token_env_and_credential_ref_set() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "http"
            name = "some-bearer-server"
            endpoint = "https://services.example.invalid/mcp/"

            [mcp_server.auth]
            mode = "bearer"
            token_env = "SOME_BEARER_SERVER_TOKEN"
            credential_ref = "some-bearer-server-token"
        "#,
    )?;

    let result = load_web_config(&path);

    assert!(matches!(
        result,
        Err(WebConfigError::BearerAuthCredentialSourceAmbiguous { name }) if name == "some-bearer-server"
    ));
    Ok(())
}

#[test]
fn bearer_auth_rejects_neither_token_env_nor_credential_ref_set() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "http"
            name = "some-bearer-server"
            endpoint = "https://services.example.invalid/mcp/"

            [mcp_server.auth]
            mode = "bearer"
        "#,
    )?;

    let result = load_web_config(&path);

    assert!(matches!(
        result,
        Err(WebConfigError::BearerAuthCredentialSourceAmbiguous { name }) if name == "some-bearer-server"
    ));
    Ok(())
}

#[test]
fn oauth2_auth_parses_with_client_id_and_client_secret_ref_set() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "http"
            name = "some-oauth-server"
            endpoint = "https://mcp.example.invalid/mcp"

            [mcp_server.auth]
            mode                    = "oauth2"
            client_id               = "abc123.apps.example.invalid"
            client_secret_ref       = "some-oauth-server-client-secret"
            authorization_endpoint  = "https://example.invalid/oauth2/authorize"
            token_endpoint          = "https://example.invalid/oauth2/token"
            scopes                  = ["mcp.tools.read", "mcp.tools.call"]
            credential_ref          = "some-oauth-server-token"
        "#,
    )?;

    let config = load_web_config(&path)?;

    let McpServerConfig::Http { auth, .. } = &config.mcp_servers[0] else {
        return Err("expected an http entry".into());
    };
    assert_eq!(
        *auth,
        Some(McpServerAuth::Oauth2 {
            client_id: Some("abc123.apps.example.invalid".to_owned()),
            client_secret_ref: Some("some-oauth-server-client-secret".to_owned()),
            client_secret_env: None,
            authorization_endpoint: "https://example.invalid/oauth2/authorize".to_owned(),
            token_endpoint: "https://example.invalid/oauth2/token".to_owned(),
            scopes: vec!["mcp.tools.read".to_owned(), "mcp.tools.call".to_owned()],
            credential_ref: "some-oauth-server-token".to_owned(),
        })
    );
    Ok(())
}

#[test]
fn oauth2_auth_parses_with_client_id_and_client_secret_ref_both_unset() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "http"
            name = "some-hosted-consent-server"
            endpoint = "https://mcp.example.invalid/mcp"

            [mcp_server.auth]
            mode                    = "oauth2"
            authorization_endpoint  = "https://example.invalid/oauth2/authorize"
            token_endpoint          = "https://example.invalid/oauth2/token"
            scopes                  = ["mcp.tools.read"]
            credential_ref          = "some-hosted-consent-server-token"
        "#,
    )?;

    let config = load_web_config(&path)?;

    let McpServerConfig::Http { auth, .. } = &config.mcp_servers[0] else {
        return Err("expected an http entry".into());
    };
    assert_eq!(
        *auth,
        Some(McpServerAuth::Oauth2 {
            client_id: None,
            client_secret_ref: None,
            client_secret_env: None,
            authorization_endpoint: "https://example.invalid/oauth2/authorize".to_owned(),
            token_endpoint: "https://example.invalid/oauth2/token".to_owned(),
            scopes: vec!["mcp.tools.read".to_owned()],
            credential_ref: "some-hosted-consent-server-token".to_owned(),
        })
    );
    Ok(())
}

#[test]
fn oauth2_auth_rejects_client_secret_ref_without_client_id() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "http"
            name = "some-oauth-server"
            endpoint = "https://mcp.example.invalid/mcp"

            [mcp_server.auth]
            mode                    = "oauth2"
            client_secret_ref       = "some-oauth-server-client-secret"
            authorization_endpoint  = "https://example.invalid/oauth2/authorize"
            token_endpoint          = "https://example.invalid/oauth2/token"
            scopes                  = ["mcp.tools.read"]
            credential_ref          = "some-oauth-server-token"
        "#,
    )?;

    let result = load_web_config(&path);

    assert!(matches!(
        result,
        Err(WebConfigError::Oauth2ClientSecretRefWithoutClientId { name }) if name == "some-oauth-server"
    ));
    Ok(())
}

#[test]
fn explicit_auth_mode_none_parses_as_the_none_variant() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "http"
            name = "some-open-server"
            endpoint = "https://mcp.example.invalid/mcp"

            [mcp_server.auth]
            mode = "none"
        "#,
    )?;

    let config = load_web_config(&path)?;

    let McpServerConfig::Http { auth, .. } = &config.mcp_servers[0] else {
        return Err("expected an http entry".into());
    };
    assert_eq!(*auth, Some(McpServerAuth::None));
    Ok(())
}

#[test]
fn mcp_server_headers_table_round_trips_as_a_string_map() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "http"
            name = "some-bearer-server"
            endpoint = "https://services.example.invalid/mcp/"

            [mcp_server.auth]
            mode = "bearer"
            credential_ref = "some-bearer-server-token"

            [mcp_server.headers]
            locationId = "not-a-secret-account-id"
            otherHeader = "some-value"
        "#,
    )?;

    let config = load_web_config(&path)?;

    let McpServerConfig::Http { headers, .. } = &config.mcp_servers[0] else {
        return Err("expected an http entry".into());
    };
    assert_eq!(
        headers.get("locationId").map(String::as_str),
        Some("not-a-secret-account-id")
    );
    assert_eq!(headers.get("otherHeader").map(String::as_str), Some("some-value"));
    assert_eq!(headers.len(), 2);
    Ok(())
}

#[test]
fn a_stdio_entry_with_an_auth_table_fails_to_parse() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "stdio"
            name = "contextos"
            command = "contextos-mcp"

            [mcp_server.auth]
            mode = "none"
        "#,
    )?;

    let result = load_web_config(&path);

    assert!(matches!(result, Err(WebConfigError::Toml { .. })));
    Ok(())
}

#[test]
fn a_stdio_entry_with_a_headers_table_fails_to_parse() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "stdio"
            name = "contextos"
            command = "contextos-mcp"

            [mcp_server.headers]
            locationId = "not-a-secret-account-id"
        "#,
    )?;

    let result = load_web_config(&path);

    assert!(matches!(result, Err(WebConfigError::Toml { .. })));
    Ok(())
}

#[test]
fn an_unknown_top_level_key_is_a_hard_error() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            unknown_key = "surprise"

            [[mcp_server]]
            transport = "stdio"
            name = "contextos"
            command = "contextos-mcp"
        "#,
    )?;

    let result = load_web_config(&path);

    assert!(matches!(result, Err(WebConfigError::Toml { .. })));
    Ok(())
}

#[test]
fn a_non_loopback_bind_is_rejected() {
    let result = validate_loopback_bind("0.0.0.0:7373");
    assert!(matches!(result, Err(WebConfigError::NonLoopbackBind { .. })));
}

#[test]
fn a_loopback_bind_is_accepted() -> Result<(), Box<dyn std::error::Error>> {
    validate_loopback_bind("127.0.0.1:7373")?;
    validate_loopback_bind("[::1]:7373")?;
    Ok(())
}

#[test]
fn an_unparseable_bind_is_rejected() {
    let result = validate_loopback_bind("not-a-socket-address");
    assert!(matches!(result, Err(WebConfigError::InvalidBindAddress { .. })));
}

#[test]
fn a_web_toml_with_a_non_loopback_bind_fails_to_load() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [server]
            bind = "0.0.0.0:7373"

            [[mcp_server]]
            transport = "stdio"
            name = "contextos"
            command = "contextos-mcp"
        "#,
    )?;

    let result = load_web_config(&path);

    assert!(matches!(result, Err(WebConfigError::NonLoopbackBind { .. })));
    Ok(())
}

#[test]
fn duplicate_mcp_server_names_are_rejected_case_insensitively() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "stdio"
            name = "contextos"
            command = "contextos-mcp"

            [[mcp_server]]
            transport = "stdio"
            name = "Contextos"
            command = "contextos-mcp-2"
        "#,
    )?;

    let result = load_web_config(&path);

    assert!(matches!(result, Err(WebConfigError::DuplicateMcpServerName { .. })));
    Ok(())
}

#[test]
fn a_missing_web_toml_file_is_a_clear_read_error() {
    let missing = std::path::Path::new("/does/not/exist/web.toml");
    let result = load_web_config(missing);
    assert!(matches!(result, Err(WebConfigError::Read { .. })));
}

#[test]
fn size_rotation_without_a_log_file_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r"
            [server]
            log_max_size_mb = 10
        ",
    )?;

    let result = load_web_config(&path);

    assert!(matches!(result, Err(WebConfigError::LogRotationWithoutFile)));
    Ok(())
}

#[test]
fn daily_rotation_without_a_log_file_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r"
            [server]
            log_rotate_daily = true
        ",
    )?;

    let result = load_web_config(&path);

    assert!(matches!(result, Err(WebConfigError::LogRotationWithoutFile)));
    Ok(())
}

#[test]
fn retention_without_a_log_file_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r"
            [server]
            log_retention_files = 5
        ",
    )?;

    let result = load_web_config(&path);

    assert!(matches!(result, Err(WebConfigError::LogRotationWithoutFile)));
    Ok(())
}

#[test]
fn a_zero_size_rotation_threshold_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [server]
            log_file = "contextos-web.log"
            log_max_size_mb = 0
        "#,
    )?;

    let result = load_web_config(&path);

    assert!(matches!(result, Err(WebConfigError::InvalidLogRotation { .. })));
    Ok(())
}

#[test]
fn retention_with_no_rotation_trigger_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [server]
            log_file = "contextos-web.log"
            log_retention_days = 7
        "#,
    )?;

    let result = load_web_config(&path);

    assert!(matches!(result, Err(WebConfigError::InvalidLogRotation { .. })));
    Ok(())
}

#[test]
fn size_and_daily_rotation_with_retention_is_accepted() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [server]
            log_file = "contextos-web.log"
            log_max_size_mb = 10
            log_rotate_daily = true
            log_retention_days = 30
            log_retention_files = 10
        "#,
    )?;

    let config = load_web_config(&path)?;

    assert_eq!(config.server.log_max_size_mb, Some(10));
    assert!(config.server.log_rotate_daily);
    assert_eq!(config.server.log_retention_days, Some(30));
    assert_eq!(config.server.log_retention_files, Some(10));
    Ok(())
}

// ---------------------------------------------------------------------
// `McpServerConfig::credential_references`: the garbage-collection diff
// `routes::settings::apply_edit_and_persist` runs after a successful edit.
// ---------------------------------------------------------------------

fn http_entry(auth: Option<McpServerAuth>) -> McpServerConfig {
    McpServerConfig::Http {
        name: "some-server".to_owned(),
        endpoint: "https://mcp.example-provider.invalid/mcp".to_owned(),
        token_env: None,
        auth,
        headers: std::collections::BTreeMap::new(),
    }
}

#[test]
fn serialising_an_http_entry_with_no_token_env_omits_the_key_rather_than_emitting_null()
-> Result<(), Box<dyn std::error::Error>> {
    // Regression: `token_env` lacked `skip_serializing_if` even though its sibling `auth`/
    // `headers` fields both document exactly why they need it (this type round-trips through
    // `routes::settings::json_object`, which rejects a literal TOML `null`). A `bearer`/`oauth2`
    // entry with no `token_env` bearer source at all is the normal case, so submitting one
    // through `/server/settings/` unconditionally failed before this fix.
    let entry = http_entry(Some(McpServerAuth::Bearer {
        token_env: None,
        credential_ref: Some("bearer-server-token".to_owned()),
    }));

    let value = serde_json::to_value(&entry)?;
    assert!(
        !value.as_object().is_some_and(|object| object.contains_key("token_env")),
        "{value}"
    );
    Ok(())
}

#[test]
fn serialising_bearer_auth_omits_whichever_half_is_unset_rather_than_emitting_null()
-> Result<(), Box<dyn std::error::Error>> {
    let entry = http_entry(Some(McpServerAuth::Bearer {
        token_env: None,
        credential_ref: Some("bearer-server-token".to_owned()),
    }));

    let value = serde_json::to_value(&entry)?;
    let auth = value
        .get("auth")
        .and_then(serde_json::Value::as_object)
        .ok_or("expected auth")?;
    assert!(!auth.contains_key("token_env"), "{value}");
    assert_eq!(
        auth.get("credential_ref"),
        Some(&serde_json::Value::String("bearer-server-token".to_owned()))
    );
    Ok(())
}

#[test]
fn serialising_oauth2_auth_omits_client_id_and_client_secret_ref_when_unset() -> Result<(), Box<dyn std::error::Error>>
{
    // The hosted-consent pattern, where the provider issues the client itself, leaves both unset:
    // the common case, not an edge case.
    let entry = http_entry(Some(McpServerAuth::Oauth2 {
        client_id: None,
        client_secret_ref: None,
        client_secret_env: None,
        authorization_endpoint: "https://example-provider.invalid/authorize".to_owned(),
        token_endpoint: "https://example-provider.invalid/token".to_owned(),
        scopes: vec!["mcp.tools.call".to_owned()],
        credential_ref: "hosted-oauth-token".to_owned(),
    }));

    let value = serde_json::to_value(&entry)?;
    let auth = value
        .get("auth")
        .and_then(serde_json::Value::as_object)
        .ok_or("expected auth")?;
    assert!(!auth.contains_key("client_id"), "{value}");
    assert!(!auth.contains_key("client_secret_ref"), "{value}");
    Ok(())
}

#[test]
fn a_stdio_entry_names_no_credential_references() {
    let entry = McpServerConfig::Stdio {
        name: "contextos".to_owned(),
        command: "contextos-mcp".to_owned(),
        args: Vec::new(),
    };

    assert!(
        entry.credential_references().is_empty(),
        "`entry.credential_references()` should be empty, got {:?}",
        entry.credential_references()
    );
}

#[test]
fn an_http_entry_with_no_auth_names_no_credential_references() {
    assert!(
        http_entry(None).credential_references().is_empty(),
        "`http_entry(None).credential_references()` should be empty, got {:?}",
        http_entry(None).credential_references()
    );
}

#[test]
fn mode_none_names_no_credential_references() {
    assert!(
        http_entry(Some(McpServerAuth::None)).credential_references().is_empty(),
        "`http_entry(Some(McpServerAuth::None)).credential_references()` should be empty, got {:?}",
        http_entry(Some(McpServerAuth::None)).credential_references()
    );
}

#[test]
fn a_token_env_sourced_bearer_entry_names_no_credential_references() {
    let entry = http_entry(Some(McpServerAuth::Bearer {
        token_env: Some("SOME_TOKEN".to_owned()),
        credential_ref: None,
    }));

    assert!(
        entry.credential_references().is_empty(),
        "`entry.credential_references()` should be empty, got {:?}",
        entry.credential_references()
    );
}

#[test]
fn a_credential_ref_sourced_bearer_entry_names_its_own_reference() {
    let entry = http_entry(Some(McpServerAuth::Bearer {
        token_env: None,
        credential_ref: Some("bearer-server-token".to_owned()),
    }));

    assert_eq!(entry.credential_references(), vec!["bearer-server-token".to_owned()]);
}

#[test]
fn an_oauth2_entry_with_no_client_secret_ref_names_only_its_own_credential_ref() {
    let entry = http_entry(Some(McpServerAuth::Oauth2 {
        client_id: None,
        client_secret_ref: None,
        client_secret_env: None,
        authorization_endpoint: "https://example-provider.invalid/authorize".to_owned(),
        token_endpoint: "https://example-provider.invalid/token".to_owned(),
        scopes: vec!["mcp.tools.call".to_owned()],
        credential_ref: "hosted-oauth-token".to_owned(),
    }));

    assert_eq!(entry.credential_references(), vec!["hosted-oauth-token".to_owned()]);
}

#[test]
fn an_oauth2_entry_with_a_client_secret_ref_names_both_references() {
    let entry = http_entry(Some(McpServerAuth::Oauth2 {
        client_id: Some("some-client-id".to_owned()),
        client_secret_ref: Some("preregistered-client-secret".to_owned()),
        client_secret_env: None,
        authorization_endpoint: "https://example-provider.invalid/authorize".to_owned(),
        token_endpoint: "https://example-provider.invalid/token".to_owned(),
        scopes: vec!["mcp.tools.call".to_owned()],
        credential_ref: "preregistered-token".to_owned(),
    }));

    let mut references = entry.credential_references();
    references.sort();
    assert_eq!(
        references,
        vec![
            "preregistered-client-secret".to_owned(),
            "preregistered-token".to_owned()
        ]
    );
}
