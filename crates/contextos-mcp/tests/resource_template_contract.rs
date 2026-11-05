//! `resources/templates/list`: one `{name}://{+path}` template per
//! configured vault, so a client can construct a valid `resources/read`
//! URI without first calling `resources/list`, and that constructed URI
//! matches what `resources/list` itself emits. Exercised through a real MCP
//! transport (`.claude/rules/mcp-contracts.md`: tests invoke the adapter,
//! not the underlying service), including parity over streamable HTTP.

mod resource_support;

mod support;
use contextos_mcp::{Config, ContextOsServer};
use rmcp::ServiceExt;
use rmcp::model::{ReadResourceRequestParams, ResourceContents};

use resource_support::{BoxError, connect, named_vault_config};

#[tokio::test]
async fn resources_templates_list_advertises_one_template_per_configured_vault() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let templates = client.list_resource_templates(None).await?;
    assert_eq!(templates.resource_templates.len(), 1);
    let template = &templates.resource_templates[0];
    assert_eq!(template.uri_template, "vault://{+path}");
    assert_eq!(template.name, "vault");

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn a_multi_vault_configuration_advertises_a_distinct_template_per_vault() -> Result<(), BoxError> {
    let mine = tempfile::Builder::new().prefix("mine").tempdir()?;
    let family = tempfile::Builder::new().prefix("family").tempdir()?;
    let source = format!(
        "[[vault]]\npath = {:?}\nname = \"mine\"\nstate_directory = \".contextos\"\n\
         [[vault]]\npath = {:?}\nname = \"family\"\nstate_directory = \".contextos\"\n",
        mine.path(),
        family.path()
    );
    let server = ContextOsServer::try_from(Config::try_from(source.as_str())?)?;
    let (mut client, server_handle) = connect(server).await?;

    let templates = client.list_resource_templates(None).await?;
    let mut uri_templates: Vec<String> = templates
        .resource_templates
        .iter()
        .map(|template| template.uri_template.clone())
        .collect();
    uri_templates.sort_unstable();
    assert_eq!(
        uri_templates,
        vec!["family://{+path}".to_owned(), "mine://{+path}".to_owned()],
        "each configured vault must be named by its own template, never the other's"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn substituting_the_template_produces_a_uri_that_reads_back_identical_content() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("nested"))?;
    let content = "# Nested note\n";
    std::fs::write(vault.path().join("nested/note.md"), content)?;

    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let templates = client.list_resource_templates(None).await?;
    let template = &templates.resource_templates[0];
    // RFC 6570 reserved expansion (`{+path}`): substituting a relative
    // path with no characters needing extra percent-encoding is exact
    // string replacement, mirroring how `resource_uri` already builds a
    // concrete URI for a real file.
    let constructed_uri = template.uri_template.replace("{+path}", "nested/note.md");

    let listed = client.list_resources(None).await?;
    let listed_uri = listed
        .resources
        .iter()
        .find(|resource| resource.name == "nested/note.md")
        .map(|resource| resource.uri.clone())
        .ok_or_else(|| std::io::Error::other("nested/note.md missing from resources/list"))?;
    assert_eq!(
        constructed_uri, listed_uri,
        "a URI built from the advertised template must match the one resources/list emits"
    );

    let read = client
        .read_resource(ReadResourceRequestParams::new(constructed_uri))
        .await?;
    match &read.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => {
            assert_eq!(text, content);
        }
        other => {
            return Err(std::io::Error::other(format!("expected text resource contents, got {other:?}")).into());
        }
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// `mcp-contracts.md` checklist item 7: parity across transports.
#[tokio::test]
async fn resource_templates_are_reachable_over_the_streamable_http_transport_too() -> Result<(), BoxError> {
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;

    let token = "resource-templates-parity-token";
    let http = contextos_mcp::HttpConfig {
        bind: "127.0.0.1:0".to_owned(),
        token: token.to_owned(),
        token_env: String::new(),
        max_body_kb: 2048,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = contextos_mcp::build_router(server, &http)?;
    let shutdown = CancellationToken::new();
    let shutdown_for_serve = shutdown.clone();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown_for_serve.cancelled().await })
            .await;
    });
    let url = format!("http://{addr}{}", contextos_mcp::HTTP_MOUNT_PATH);

    let client_config = StreamableHttpClientTransportConfig::with_uri(url).auth_header(token.to_owned());
    let transport = StreamableHttpClientTransport::from_config(client_config);
    let client = ().serve(transport).await?;

    let templates = client.list_resource_templates(None).await?;
    assert_eq!(templates.resource_templates.len(), 1);

    client.cancel().await?;
    shutdown.cancel();
    handle.await?;
    Ok(())
}
