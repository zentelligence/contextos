use std::collections::{BTreeMap, HashMap};

use serde_json::json;

use super::*;

fn oauth2_token_record(expires_at: &str) -> CredentialRecord {
    CredentialRecord::Oauth2Token {
        access_token: "access-value".to_owned(),
        refresh_token: Some("refresh-value".to_owned()),
        expires_at: expires_at.to_owned(),
        obtained_at: "2026-09-20T00:00:00Z".to_owned(),
    }
}

#[test]
fn a_stdio_entry_with_no_legacy_token_env_views_as_mode_none() {
    let view = mcp_server_auth_view("some-server", None, None, &BTreeMap::new(), &HashMap::new(), None);

    assert_eq!(view.mode, "none");
}

#[test]
fn an_absent_auth_table_with_a_legacy_token_env_views_as_mode_bearer_for_continuity() {
    let view = mcp_server_auth_view(
        "some-server",
        None,
        Some("SOME_VAR"),
        &BTreeMap::new(),
        &HashMap::new(),
        None,
    );

    assert_eq!(view.mode, "bearer");
    assert_eq!(view.bearer_token_env, "SOME_VAR");
}

#[test]
fn a_bearer_credential_ref_shows_configured_only_once_a_record_actually_exists() {
    let auth = McpServerAuth::Bearer {
        token_env: None,
        credential_ref: Some("some-server-token".to_owned()),
    };
    let mut credentials = HashMap::new();
    credentials.insert(
        "some-server-token".to_owned(),
        CredentialRecord::Bearer {
            value: "pasted-value".to_owned(),
            updated_at: "2026-09-20T00:00:00Z".to_owned(),
        },
    );

    let unconfigured = mcp_server_auth_view(
        "some-server",
        Some(&auth),
        None,
        &BTreeMap::new(),
        &HashMap::new(),
        None,
    );
    let configured = mcp_server_auth_view("some-server", Some(&auth), None, &BTreeMap::new(), &credentials, None);

    assert!(!unconfigured.bearer_credential_configured);
    assert!(configured.bearer_credential_configured);
    assert_eq!(configured.bearer_credential_ref, "some-server-token");
}

#[test]
fn an_oauth2_entry_shows_connected_only_once_a_token_record_exists() {
    let auth = McpServerAuth::Oauth2 {
        client_id: None,
        client_secret_ref: None,
        client_secret_env: None,
        authorization_endpoint: "https://example-provider.invalid/authorize".to_owned(),
        token_endpoint: "https://example-provider.invalid/token".to_owned(),
        scopes: vec!["mcp.tools.call".to_owned()],
        credential_ref: "some-server-oauth-token".to_owned(),
    };
    let mut credentials = HashMap::new();
    credentials.insert(
        "some-server-oauth-token".to_owned(),
        oauth2_token_record("2026-09-20T01:00:00Z"),
    );

    let not_connected = mcp_server_auth_view(
        "some-server",
        Some(&auth),
        None,
        &BTreeMap::new(),
        &HashMap::new(),
        None,
    );
    let connected = mcp_server_auth_view("some-server", Some(&auth), None, &BTreeMap::new(), &credentials, None);

    assert!(!not_connected.oauth_connected);
    assert!(connected.oauth_connected);
    assert_eq!(connected.oauth_expires_at, "2026-09-20T01:00:00Z");
}

#[test]
fn no_query_parameters_render_no_banner() {
    let (notice, error) = settings_banner(&SettingsQuery::default());

    assert_eq!(notice, None);
    assert_eq!(error, None);
}

#[test]
fn oauth_connected_renders_a_success_sentence_naming_the_server() {
    let query = SettingsQuery {
        oauth_connected: Some("hosted-oauth".to_owned()),
        ..SettingsQuery::default()
    };

    let (notice, error) = settings_banner(&query);

    assert_eq!(notice.as_deref(), Some("Connected to hosted-oauth."));
    assert_eq!(error, None);
}

#[test]
fn each_known_oauth_error_reason_renders_its_own_sentence_and_an_unknown_one_falls_back() {
    let known_reasons = [
        "invalid_state",
        "missing_parameters",
        "server_removed",
        "client_secret_missing",
        "token_exchange_failed",
    ];
    for reason in known_reasons {
        let query = SettingsQuery {
            oauth_error: Some(reason.to_owned()),
            server: Some("hosted-oauth".to_owned()),
            ..SettingsQuery::default()
        };
        let (notice, error) = settings_banner(&query);
        assert_eq!(notice, None);
        assert!(
            error
                .as_deref()
                .is_some_and(|text| text.starts_with("Could not connect hosted-oauth: ")),
            "reason {reason:?} produced {error:?}"
        );
    }

    let query = SettingsQuery {
        oauth_error: Some("some_reason_this_page_has_never_seen".to_owned()),
        server: Some("hosted-oauth".to_owned()),
        ..SettingsQuery::default()
    };
    let (_notice, error) = settings_banner(&query);
    assert_eq!(
        error.as_deref(),
        Some("Could not connect hosted-oauth: the connection failed.")
    );
}

#[test]
fn extracting_pasted_secrets_strips_the_value_but_keeps_the_reference_name() -> Result<(), Box<dyn std::error::Error>> {
    let mut patch = json!({
        "auth": {
            "mode": "bearer",
            "credential_ref": "some-server-token",
            "credential_value": "pasted-secret",
        }
    })
    .as_object()
    .ok_or("expected a JSON object")?
    .clone();

    let writes = extract_pasted_secrets_from_patch(&mut patch, "2026-09-20T00:00:00Z")?;

    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, "some-server-token");
    assert!(matches!(&writes[0].1, CredentialRecord::Bearer { value, .. } if value == "pasted-secret"));
    let auth = patch
        .get("auth")
        .and_then(Value::as_object)
        .ok_or("expected an auth object")?;
    assert!(
        !auth.contains_key("credential_value"),
        "the pasted value must never remain in the patch that reaches web.toml"
    );
    assert_eq!(
        auth.get("credential_ref").and_then(Value::as_str),
        Some("some-server-token")
    );
    Ok(())
}

#[test]
fn extracting_pasted_secrets_is_a_no_op_when_the_patch_carries_no_auth_object() -> Result<(), Box<dyn std::error::Error>>
{
    let mut patch = json!({ "endpoint": "https://example-provider.invalid/mcp" })
        .as_object()
        .ok_or("expected a JSON object")?
        .clone();

    let writes = extract_pasted_secrets_from_patch(&mut patch, "2026-09-20T00:00:00Z")?;

    assert!(writes.is_empty(), "`writes` should be empty, got {writes:?}");
    Ok(())
}

#[test]
fn a_pasted_value_with_no_reference_to_store_it_under_is_refused_naming_the_reference()
-> Result<(), Box<dyn std::error::Error>> {
    for (value_key, reference_key) in [
        ("credential_value", "credential_ref"),
        ("client_secret_value", "client_secret_ref"),
    ] {
        let mut patch = json!({ "auth": { "mode": "bearer", value_key: "pasted-secret" } })
            .as_object()
            .ok_or("expected a JSON object")?
            .clone();

        let refused = extract_pasted_secrets_from_patch(&mut patch, "2026-09-20T00:00:00Z").err();

        assert_eq!(
            refused,
            Some(PastedSecretRefusal::WithoutReference {
                reference: reference_key
            })
        );
    }
    Ok(())
}

fn refusal_for(auth: &Value) -> Result<Option<PastedSecretRefusal>, Box<dyn std::error::Error>> {
    let mut patch = json!({ "auth": auth })
        .as_object()
        .ok_or("expected a JSON object")?
        .clone();
    Ok(extract_pasted_secrets_from_patch(&mut patch, "2026-09-20T00:00:00Z").err())
}

#[test]
fn a_pasted_token_beside_a_token_variable_is_refused_rather_than_one_being_dropped()
-> Result<(), Box<dyn std::error::Error>> {
    let refused = refusal_for(&json!({
        "mode": "bearer",
        "token_env": "REMOTE_TOKEN",
        "credential_ref": "remote-token",
        "credential_value": "pasted-secret",
    }))?;

    assert_eq!(
        refused,
        Some(PastedSecretRefusal::BesideVariable {
            value: "a pasted bearer token",
            variable: "token_env",
        })
    );
    Ok(())
}

#[test]
fn an_empty_pasted_value_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let refused = refusal_for(&json!({ "mode": "bearer", "credential_ref": "remote-token", "credential_value": "" }))?;

    assert_eq!(
        refused,
        Some(PastedSecretRefusal::Empty {
            field: "credential_value"
        })
    );
    Ok(())
}

#[test]
fn a_pasted_value_whose_reference_the_store_cannot_hold_is_refused_naming_it() -> Result<(), Box<dyn std::error::Error>>
{
    let refused = refusal_for(&json!({
        "mode": "bearer",
        "credential_ref": "team/remote-token",
        "credential_value": "pasted-secret",
    }))?;

    assert_eq!(
        refused,
        Some(PastedSecretRefusal::UnstorableReference {
            reference: "credential_ref",
            name: "team/remote-token".to_owned(),
        })
    );
    assert!(
        refused.is_some_and(|refusal| !refusal.to_string().contains("pasted-secret")),
        "the refusal never echoes the value"
    );
    Ok(())
}

#[test]
fn credential_references_in_deduplicates_across_the_whole_entry_array() {
    let entries = vec![
        McpServerConfig::Http {
            name: "a".to_owned(),
            endpoint: "https://a.invalid/mcp".to_owned(),
            token_env: None,
            auth: Some(McpServerAuth::Bearer {
                token_env: None,
                credential_ref: Some("shared-ref".to_owned()),
            }),
            headers: BTreeMap::new(),
        },
        McpServerConfig::Http {
            name: "b".to_owned(),
            endpoint: "https://b.invalid/mcp".to_owned(),
            token_env: None,
            auth: Some(McpServerAuth::Bearer {
                token_env: None,
                credential_ref: Some("shared-ref".to_owned()),
            }),
            headers: BTreeMap::new(),
        },
    ];

    let references = credential_references_in(&entries);

    assert_eq!(references.len(), 1);
    assert!(references.contains("shared-ref"));
}

#[test]
fn a_record_left_under_the_default_name_offers_no_show_control_for_an_entry_that_does_not_use_it() {
    let auth = McpServerAuth::Bearer {
        token_env: Some("REMOTE_TOKEN".to_owned()),
        credential_ref: None,
    };
    let mut credentials = HashMap::new();
    credentials.insert(
        "some-server-token".to_owned(),
        CredentialRecord::Bearer {
            value: "leftover".to_owned(),
            updated_at: "2026-09-20T00:00:00Z".to_owned(),
        },
    );

    let view = mcp_server_auth_view("some-server", Some(&auth), None, &BTreeMap::new(), &credentials, None);

    assert!(view.bearer_token_secret.is_none());
    assert!(view.bearer_variable_secret.is_some());
}

#[test]
fn a_pasted_client_secret_beside_a_client_secret_variable_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let refused = refusal_for(&json!({
        "mode": "oauth2",
        "client_secret_env": "SOME_CLIENT_SECRET",
        "client_secret_ref": "remote-client-secret",
        "client_secret_value": "pasted-secret",
    }))?;

    assert_eq!(
        refused,
        Some(PastedSecretRefusal::BesideVariable {
            value: "a pasted client secret",
            variable: "client_secret_env",
        })
    );
    Ok(())
}
