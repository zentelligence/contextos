use super::*;

use contextos_packages::{
    PackageStore, ReadCheck, ReleaseKey, ReleaseName, ReleaseVersion, StoreEntry, StoreEntryKind, StoreIo, StorePath,
};
use std::path::PathBuf;

use rmcp::ServiceExt;
use serde_json::json;

use crate::config::McpServerConfig;
use crate::fake_vault::FakeVault;
use crate::release_fixture::write_whole_release;

type BoxError = Box<dyn std::error::Error>;

const WORKFLOW: &str = "[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n";

struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Result<Self, BoxError> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("vault"))?;
        Ok(Self { dir })
    }

    fn vault(&self) -> PathBuf {
        self.dir.path().join("vault")
    }

    fn server(&self) -> Result<contextos_mcp::ContextOsServer, BoxError> {
        let mut config = contextos_mcp::Config::try_from(vec![self.vault()])?;
        for vault in &mut config.vaults {
            vault.state_directory = Some(PathBuf::from("state"));
        }
        Ok(contextos_mcp::ContextOsServer::try_from(config)?)
    }

    fn put_release(&self) -> Result<(), BoxError> {
        write_whole_release(
            &self.vault().join(".contextos/packages/sync/1.0.0"),
            ("sync", "1.0.0"),
            WORKFLOW,
            &[("sync.rhai", b"let x = 1;")],
        )?;
        Ok(())
    }

    /// A session to the real server, served in process.
    async fn session(&self) -> Result<McpClient, BoxError> {
        let server = self.server()?;
        let (client_io, server_io) = tokio::io::duplex(1 << 20);
        tokio::spawn(async move {
            if let Ok(running) = server.serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        Ok(McpClient::over("contextos", ().serve(client_io).await?))
    }

    /// A session to the real server, reached over HTTP on an ephemeral loopback port.
    async fn http_session(&self) -> Result<McpClient, BoxError> {
        let http = contextos_mcp::HttpConfig {
            bind: "127.0.0.1:0".to_owned(),
            token: String::new(),
            token_env: String::new(),
            max_body_kb: 2048,
        };
        let router = contextos_mcp::build_router(self.server()?, &http)?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let entry = McpServerConfig::Http {
            name: "contextos".to_owned(),
            endpoint: format!("http://{address}/mcp"),
            token_env: None,
            auth: None,
            headers: std::collections::BTreeMap::new(),
        };
        Ok(McpClient::connect(&entry).await?)
    }
}

async fn listing_of_the_store(client: &McpClient) -> Result<Vec<StoreEntry>, BoxError> {
    let io = client.vault_store("vault").await?;
    Ok(io.list(&StorePath::root()).await.map_err(|failure| failure.detail)?)
}

fn sync_folder() -> Vec<StoreEntry> {
    vec![StoreEntry {
        name: "sync".to_owned(),
        kind: StoreEntryKind::Directory,
    }]
}

#[tokio::test]
async fn a_session_finds_the_vaults_package_store_from_what_vault_info_reports() -> Result<(), BoxError> {
    let world = World::new()?;
    world.put_release()?;
    let client = world.session().await?;

    assert_eq!(listing_of_the_store(&client).await?, sync_folder());
    Ok(())
}

#[tokio::test]
async fn a_primary_server_reached_over_http_has_its_vaults_package_store_found_the_same_way() -> Result<(), BoxError> {
    let world = World::new()?;
    world.put_release()?;
    let client = world.http_session().await?;

    assert_eq!(listing_of_the_store(&client).await?, sync_folder());
    Ok(())
}

#[tokio::test]
async fn a_release_in_the_vaults_store_is_read_and_verified_through_the_handle() -> Result<(), BoxError> {
    let world = World::new()?;
    world.put_release()?;
    let client = world.session().await?;

    let engine = client.vault_packages("vault").await?;
    let key = ReleaseKey {
        name: ReleaseName::try_from("sync")?,
        version: ReleaseVersion::try_from("1.0.0")?,
    };

    let check = engine.read_release(&key).await;
    assert!(
        matches!(check, ReadCheck::Verified(_)),
        "{}",
        match &check {
            ReadCheck::Verified(_) => String::new(),
            other => format!("{:?}", std::mem::discriminant(other)),
        }
    );
    Ok(())
}

#[tokio::test]
async fn the_vaults_location_is_asked_of_the_server_once_per_session() -> Result<(), BoxError> {
    let world = World::new()?;
    let fake = FakeVault::new().with_vault_info(json!({"vaults": [{"name": "vault", "path": world.vault()}]}));
    let client = fake.session().await?;

    client.vault_store("vault").await?;
    client.vault_store("vault").await?;

    assert_eq!(fake.calls_to("vault_info", ""), 1);
    Ok(())
}

/// What locating `vault` in a session served by `fake` fails with, as its code and message.
async fn unlocated(fake: &FakeVault) -> Result<(&'static str, String), BoxError> {
    let client = fake.session().await?;
    let error = client
        .vault_store("vault")
        .await
        .err()
        .ok_or("the store was located, which was not expected")?;
    Ok((error.code(), error.to_string()))
}

#[tokio::test]
async fn a_vault_the_server_does_not_report_has_no_package_store_and_the_message_names_it() -> Result<(), BoxError> {
    let world = World::new()?;
    let fake = FakeVault::new().with_vault_info(json!({"vaults": [{"name": "elsewhere", "path": world.vault()}]}));

    let (code, message) = unlocated(&fake).await?;

    assert_eq!(code, "packages/unknown-vault");
    assert!(message.contains("vault"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_vault_reported_without_a_path_has_no_package_store_and_the_message_says_so() -> Result<(), BoxError> {
    let fake = FakeVault::new().with_vault_info(json!({"vaults": [{"name": "vault"}]}));

    let (code, message) = unlocated(&fake).await?;

    assert_eq!(code, "packages/vault-info");
    assert!(message.contains("path"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_vault_info_the_server_refuses_leaves_the_package_store_unlocatable() -> Result<(), BoxError> {
    let (code, message) = unlocated(&FakeVault::new()).await?;

    assert_eq!(code, "packages/vault-info");
    assert!(message.contains("vault_info"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_vault_info_that_is_not_a_report_leaves_the_package_store_unlocatable() -> Result<(), BoxError> {
    let fake = FakeVault::new().with_vault_info(json!({"unexpected": true}));

    let (code, message) = unlocated(&fake).await?;

    assert_eq!(code, "packages/vault-info");
    assert!(message.contains("vault_info"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_reported_path_that_is_not_a_folder_on_this_host_leaves_the_package_store_unlocatable() -> Result<(), BoxError>
{
    let world = World::new()?;
    let missing = world.dir.path().join("not-here");
    let fake = FakeVault::new().with_vault_info(json!({"vaults": [{"name": "vault", "path": missing}]}));

    let (code, _) = unlocated(&fake).await?;

    assert_eq!(code, "packages/vault-root");
    Ok(())
}
