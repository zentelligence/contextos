use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;

use crate::app::build_router;
use crate::config::McpServerConfig;
use crate::fake_vault::FakeVault;
use crate::mcp_client::{McpClient, McpConnectError};
use crate::mcp_client_set::{McpClientSet, SessionConnector, Strictness};

type BoxError = Box<dyn std::error::Error>;

const TWO_SERVERS: &str = r#"
[[mcp_server]]
name = "contextos"
transport = "stdio"
command = "contextos-mcp"
args = ["--config", "config.toml"]

[[mcp_server]]
name = "extra"
transport = "stdio"
command = "contextos-mcp"
args = ["--config", "config.toml"]
"#;

/// Serves every entry from the same in-process vault.
struct VaultConnector(FakeVault);

impl SessionConnector for VaultConnector {
    fn open<'a>(
        &'a self,
        _entry: &'a McpServerConfig,
        _shutdown: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<McpClient, McpConnectError>> {
        Box::pin(async move { self.0.session().await.map_err(|_| McpConnectError::Shutdown) })
    }
}

async fn delete_extra_with(vault: FakeVault) -> Result<(StatusCode, Value, String, String), BoxError> {
    let dir = tempfile::tempdir()?;
    let web_toml = dir.path().join("web.toml");
    std::fs::write(&web_toml, TWO_SERVERS)?;
    let entries = vec![
        McpServerConfig::Stdio {
            name: "contextos".to_owned(),
            command: "contextos-mcp".to_owned(),
            args: vec!["--config".to_owned(), "config.toml".to_owned()],
        },
        McpServerConfig::Stdio {
            name: "extra".to_owned(),
            command: "contextos-mcp".to_owned(),
            args: vec!["--config".to_owned(), "config.toml".to_owned()],
        },
    ];
    let connector: Arc<dyn SessionConnector> = Arc::new(VaultConnector(vault));
    let clients =
        Arc::new(McpClientSet::open_all(connector, &entries, &CancellationToken::new(), Strictness::Lenient).await?);
    let router = build_router(clients, Some(dir.path()), &web_toml);
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri("/server/settings/mcp-servers/extra")
                .body(Body::empty())?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Ok((
        status,
        body,
        std::fs::read_to_string(&web_toml)?,
        TWO_SERVERS.to_owned(),
    ))
}

#[tokio::test]
async fn removing_a_server_is_refused_when_the_vault_list_is_refused() -> Result<(), BoxError> {
    // No `vault_info` answer is set, so the fake vault refuses the call.
    let (status, body, after, before) = delete_extra_with(FakeVault::new()).await?;

    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["error"], "mcp/vault-list-unreadable");
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|d| d.contains("vault list could not be read")),
        "{body}"
    );
    assert_eq!(after, before, "web.toml must be left unchanged");
    Ok(())
}

#[tokio::test]
async fn removing_a_server_is_refused_when_the_vault_list_is_unparsable() -> Result<(), BoxError> {
    let vault = FakeVault::new().with_vault_info(json!({ "unexpected": true }));

    let (status, body, after, before) = delete_extra_with(vault).await?;

    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["error"], "mcp/vault-list-unreadable");
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|d| d.contains("dependent apps could not be checked")),
        "{body}"
    );
    assert_eq!(after, before, "web.toml must be left unchanged");
    Ok(())
}
