use super::*;

use contextos_packages::{ReleaseFilePath, StoreFailureKind};

use crate::fake_vault::FakeVault;

type BoxError = Box<dyn std::error::Error>;

fn path(text: &str) -> Result<StorePath, BoxError> {
    if text.is_empty() {
        return Ok(StorePath::root());
    }
    Ok(StorePath::root().within(&ReleaseFilePath::try_from(text)?))
}

fn kind_of<T: std::fmt::Debug>(result: Result<T, StoreFailure>) -> Result<StoreFailureKind, BoxError> {
    match result {
        Ok(value) => Err(format!("expected a failure, got {value:?}").into()),
        Err(failure) => Ok(failure.kind),
    }
}

/// A temporary vault named `vault`, with a session to a stand-in server that answers as a test arranges. The store's
/// I/O against a real server is covered by the store parity suite.
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

    /// The location a primary server's `vault_info` gives this vault.
    fn location(&self) -> Result<VaultLocation, BoxError> {
        let report = serde_json::from_value(serde_json::json!({
            "vaults": [{"name": "vault", "path": self.vault()}]
        }))?;
        Ok(VaultLocation::try_from(ReportedLocation { report, vault: "vault" })?)
    }

    fn io_with(&self, client: McpClient) -> Result<VaultStoreIo, BoxError> {
        Ok(VaultStoreIo::try_from(VaultStoreSettings {
            client: Arc::new(client),
            location: self.location()?,
            lock_wait: Duration::ZERO,
        })?)
    }
}

// Reads when the server cannot answer.

#[tokio::test]
async fn a_connection_that_drops_part_way_through_a_tree_listing_is_an_outage() -> Result<(), BoxError> {
    let world = World::new()?;
    let client = FakeVault::new().dropping_after(0).session().await?;
    let io = world.io_with(client)?;

    let failure = io
        .list_tree(&path("demo")?, 10)
        .await
        .err()
        .ok_or("expected a failure")?;

    assert_eq!(failure.kind, StoreFailureKind::Unavailable);
    assert!(!failure.detail.contains(&world.vault().to_string_lossy().into_owned()));
    Ok(())
}

#[tokio::test]
async fn an_answer_that_is_not_a_listing_is_a_failure_not_an_empty_folder() -> Result<(), BoxError> {
    let world = World::new()?;
    let client = FakeVault::new()
        .with_file(".contextos/packages/demo/file.txt", "x")
        .listing(crate::fake_vault::Listing::Garbled)
        .session()
        .await?;
    let io = world.io_with(client)?;

    assert_eq!(kind_of(io.list(&path("demo")?).await)?, StoreFailureKind::Failed);
    Ok(())
}
