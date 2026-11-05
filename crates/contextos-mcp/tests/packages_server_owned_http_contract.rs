//! The server-owned package store is refused over streamable HTTP exactly as it is over stdio (see
//! `packages_server_owned_contract.rs`), a write through an in-vault symbolic link to it is refused too, and what a
//! workflow run legitimately writes (`registry/` and `.contextos/jobs.toml`) is not refused.

mod support;

use contextos_mcp::{ContextOsServer, HttpConfig};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const TOKEN: &str = "server-owned-http-token";
const RELEASE_MANIFEST: &str = ".contextos/packages/demo/1.0.0/package.toml";
const MANIFEST_TEXT: &str = "name = \"demo\"\nversion = \"1.0.0\"\n";

struct Served {
    vault: tempfile::TempDir,
    url: String,
    shutdown: CancellationToken,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

async fn serve() -> Result<Served, BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join(".contextos/packages/demo/1.0.0"))?;
    std::fs::write(vault.path().join(RELEASE_MANIFEST), MANIFEST_TEXT)?;
    std::fs::write(vault.path().join(".contextos/jobs.toml"), "# jobs\n")?;
    std::fs::create_dir_all(vault.path().join("registry/jobs"))?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(".contextos/packages", vault.path().join("alias"))?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].git.destructive_delete = true;
    let server = ContextOsServer::try_from(config)?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let router = contextos_mcp::build_router(
        server,
        &HttpConfig {
            bind: "127.0.0.1:0".to_owned(),
            token: TOKEN.to_owned(),
            token_env: String::new(),
            max_body_kb: 2048,
        },
    )?;
    let shutdown = CancellationToken::new();
    let serving = shutdown.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { serving.cancelled().await })
            .await;
    });
    Ok(Served {
        vault,
        url: format!("http://{address}{}", contextos_mcp::HTTP_MOUNT_PATH),
        shutdown,
    })
}

async fn call_over_http(url: &str, tool: &'static str, arguments: Value) -> Result<CallToolResult, BoxError> {
    let arguments: Map<String, Value> = serde_json::from_value(arguments)?;
    let config = StreamableHttpClientTransportConfig::with_uri(url.to_owned()).auth_header(TOKEN.to_owned());
    let client = ().serve(StreamableHttpClientTransport::from_config(config)).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
        .await;
    client.cancel().await?;
    Ok(result?)
}

fn code_of(result: &CallToolResult) -> Option<&str> {
    result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("code"))
        .and_then(Value::as_str)
}

#[tokio::test]
async fn every_kind_of_write_under_the_package_store_is_refused_over_http_and_changes_nothing() -> Result<(), BoxError>
{
    let served = serve().await?;
    let attempts: Vec<(&'static str, Value)> = vec![
        (
            "fs_write_file",
            json!({"path": ".contextos/packages/demo/1.0.0/new.toml", "content": "x"}),
        ),
        (
            "fs_write_file",
            json!({"path": RELEASE_MANIFEST, "content": "x", "force": true}),
        ),
        (
            "fs_edit_file",
            json!({"path": RELEASE_MANIFEST, "edits": [{"old_text": "demo", "new_text": "forged"}]}),
        ),
        (
            "fs_move_file",
            json!({"source": RELEASE_MANIFEST, "destination": "elsewhere.toml"}),
        ),
        ("fs_delete_file", json!({"path": RELEASE_MANIFEST})),
        ("fs_delete_file", json!({"path": ".contextos", "hard": true})),
        ("fs_create_directory", json!({"path": ".contextos/packages/other"})),
    ];
    for (tool, arguments) in attempts {
        let result = call_over_http(&served.url, tool, arguments.clone()).await?;

        assert_eq!(result.is_error, Some(true), "{tool} {arguments}: {result:?}");
        assert_eq!(code_of(&result), Some("packages/server-owned"), "{tool} {arguments}");
    }
    assert_eq!(
        std::fs::read_to_string(served.vault.path().join(RELEASE_MANIFEST))?,
        MANIFEST_TEXT
    );
    assert!(
        !served
            .vault
            .path()
            .join(".contextos/packages/demo/1.0.0/new.toml")
            .exists()
    );
    assert!(!served.vault.path().join(".contextos/packages/other").exists());
    assert!(!served.vault.path().join("elsewhere.toml").exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_write_through_an_in_vault_symbolic_link_to_the_package_store_is_refused() -> Result<(), BoxError> {
    let served = serve().await?;
    for (tool, arguments) in [
        (
            "fs_write_file",
            json!({"path": "alias/demo/1.0.0/new.toml", "content": "x"}),
        ),
        (
            "fs_edit_file",
            json!({"path": "alias/demo/1.0.0/package.toml", "edits": [{"old_text": "demo", "new_text": "forged"}]}),
        ),
        ("fs_delete_file", json!({"path": "alias/demo/1.0.0/package.toml"})),
        (
            "fs_move_file",
            json!({"source": "alias/demo/1.0.0/package.toml", "destination": "elsewhere.toml"}),
        ),
    ] {
        let result = call_over_http(&served.url, tool, arguments.clone()).await?;

        assert_eq!(result.is_error, Some(true), "{tool} {arguments}: {result:?}");
        assert_eq!(code_of(&result), Some("packages/server-owned"), "{tool} {arguments}");
    }
    assert_eq!(
        std::fs::read_to_string(served.vault.path().join(RELEASE_MANIFEST))?,
        MANIFEST_TEXT
    );
    assert!(
        !served
            .vault
            .path()
            .join(".contextos/packages/demo/1.0.0/new.toml")
            .exists()
    );
    assert!(!served.vault.path().join("elsewhere.toml").exists());
    Ok(())
}

#[tokio::test]
async fn what_a_run_writes_to_the_registry_and_the_jobs_state_is_not_refused_over_http() -> Result<(), BoxError> {
    let served = serve().await?;
    let info = call_over_http(&served.url, "fs_get_file_info", json!({"path": ".contextos/jobs.toml"})).await?;
    let hash = info
        .structured_content
        .as_ref()
        .and_then(|content| content.get("content_hash"))
        .and_then(Value::as_str)
        .ok_or("no content_hash")?
        .to_owned();

    for (tool, arguments) in [
        (
            "fs_write_file",
            json!({"path": ".contextos/jobs.toml", "content": "# jobs v2\n", "expected_hash": hash}),
        ),
        (
            "fs_write_file",
            json!({"path": "registry/jobs/new.md", "content": "# New\n"}),
        ),
    ] {
        let result = call_over_http(&served.url, tool, arguments.clone()).await?;

        assert_eq!(result.is_error, Some(false), "{tool} {arguments}: {result:?}");
    }
    assert_eq!(
        std::fs::read_to_string(served.vault.path().join(".contextos/jobs.toml"))?,
        "# jobs v2\n"
    );
    assert!(served.vault.path().join("registry/jobs/new.md").is_file());
    Ok(())
}
