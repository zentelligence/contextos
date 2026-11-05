use super::*;
use crate::host_store::HostStore;

#[tokio::test]
async fn a_store_with_no_ledger_and_no_releases_has_recorded_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let store = HostStore::new(root.path());
    let package = PackageName::try_from("acme")?;
    let version = Version::new(1, 0, 0);

    assert!(!ever_recorded(store.engine(), &package, &version).await?);
    Ok(())
}

#[tokio::test]
async fn a_ledger_that_cannot_be_used_is_taken_as_recording_the_release() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    std::fs::write(root.path().join("ledger.toml"), "this is not a ledger")?;
    let store = HostStore::new(root.path());
    let package = PackageName::try_from("acme")?;
    let version = Version::new(1, 0, 0);

    assert!(ever_recorded(store.engine(), &package, &version).await?);
    Ok(())
}
