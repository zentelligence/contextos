use std::fs;

use tempfile::tempdir;

use super::*;

/// A web.toml holding one `oauth2` entry whose `[mcp_server.auth]` table also carries `extra`.
fn load_oauth_entry(extra: &str) -> Result<Result<WebConfig, WebConfigError>, Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("web.toml");
    fs::write(
        &path,
        format!(
            "[[mcp_server]]\ntransport = \"http\"\nname = \"some-oauth-server\"\nendpoint = \"https://mcp.example.invalid/mcp\"\n\
             [mcp_server.auth]\nmode = \"oauth2\"\nauthorization_endpoint = \"https://example.invalid/authorize\"\n\
             token_endpoint = \"https://example.invalid/token\"\nscopes = [\"mcp\"]\n\
             credential_ref = \"some-oauth-server-token\"\n{extra}"
        ),
    )?;
    Ok(load_web_config(&path))
}

#[test]
fn a_client_secret_can_come_from_an_environment_variable() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_oauth_entry("client_id = \"abc\"\nclient_secret_env = \"SOME_CLIENT_SECRET\"\n")??;

    let auth = match config.mcp_servers.first() {
        Some(McpServerConfig::Http { auth: Some(auth), .. }) => auth,
        other => return Err(format!("expected an oauth2 entry, got {other:?}").into()),
    };
    assert_eq!(
        auth.client_secret(),
        Some(ClientSecretSource::Environment("SOME_CLIENT_SECRET".to_owned()))
    );
    Ok(())
}

#[test]
fn a_stored_client_secret_is_still_named_by_its_reference() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_oauth_entry("client_id = \"abc\"\nclient_secret_ref = \"some-oauth-server-client-secret\"\n")??;

    let auth = match config.mcp_servers.first() {
        Some(McpServerConfig::Http { auth: Some(auth), .. }) => auth,
        other => return Err(format!("expected an oauth2 entry, got {other:?}").into()),
    };
    assert_eq!(
        auth.client_secret(),
        Some(ClientSecretSource::Stored("some-oauth-server-client-secret".to_owned()))
    );
    Ok(())
}

#[test]
fn a_client_secret_and_its_environment_variable_together_are_refused_naming_both()
-> Result<(), Box<dyn std::error::Error>> {
    let result = load_oauth_entry(
        "client_id = \"abc\"\nclient_secret_ref = \"some-oauth-server-client-secret\"\n\
         client_secret_env = \"SOME_CLIENT_SECRET\"\n",
    )?;

    let Err(error) = result else {
        return Err("expected both sources to be refused".into());
    };
    assert!(
        matches!(&error, WebConfigError::Oauth2ClientSecretSourceAmbiguous { name } if name == "some-oauth-server"),
        "{error:?}"
    );
    assert!(
        error.to_string().contains("client_secret_ref or client_secret_env"),
        "{error}"
    );
    Ok(())
}

#[test]
fn a_client_secret_variable_without_a_client_id_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let result = load_oauth_entry("client_secret_env = \"SOME_CLIENT_SECRET\"\n")?;

    assert!(
        matches!(&result, Err(WebConfigError::Oauth2ClientSecretRefWithoutClientId { name }) if name == "some-oauth-server"),
        "{result:?}"
    );
    Ok(())
}
