//! The host store's I/O through the public `StoreIo` interface, over a real temporary folder.

use std::time::Duration;

use contextos_packages::host_io::HostStoreIo;
use contextos_packages::{
    OperationId, ReleaseFilePath, ReleaseKey, ReleaseName, ReleaseVersion, StoreEntryKind, StoreFailure,
    StoreFailureKind, StoreIo, StorePath,
};

#[derive(Debug)]
struct Oops(String);

impl std::fmt::Display for Oops {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<StoreFailure> for Oops {
    fn from(value: StoreFailure) -> Self {
        Self(format!("{value:?}"))
    }
}

impl From<contextos_packages::PackagesError> for Oops {
    fn from(value: contextos_packages::PackagesError) -> Self {
        Self(value.to_string())
    }
}

impl From<std::io::Error> for Oops {
    fn from(value: std::io::Error) -> Self {
        Self(value.to_string())
    }
}

type TestResult = Result<(), Oops>;

fn release(name: &str, version: &str) -> Result<StorePath, Oops> {
    let key = ReleaseKey {
        name: ReleaseName::try_from(name)?,
        version: ReleaseVersion::try_from(version)?,
    };
    Ok(StorePath::from(&key))
}

fn store(root: &std::path::Path) -> HostStoreIo {
    HostStoreIo::new(root.to_path_buf(), Duration::from_millis(100))
}

#[tokio::test]
async fn a_release_is_staged_written_made_visible_listed_read_and_removed_through_the_interface() -> TestResult {
    let root = tempfile::tempdir()?;
    let io = store(root.path());
    let staging = StorePath::staging_folder(&OperationId::try_from("op1")?);
    let live = release("demo", "1.0.0")?;
    let script = ReleaseFilePath::try_from("lib/util.rhai")?;

    io.create_dir_all(&staging.within(&ReleaseFilePath::try_from("lib")?))
        .await?;
    io.write_new(&staging.manifest(), b"manifest").await?;
    io.write_new(&staging.within(&script), b"script").await?;
    io.create_dir_all(&live.parent().ok_or_else(|| Oops("no parent".to_owned()))?)
        .await?;
    io.rename(&staging, &live).await?;

    assert_eq!(io.read(&live.manifest(), 100).await?, b"manifest");
    let mut tree: Vec<_> = io
        .list_tree(&live, 10)
        .await?
        .into_iter()
        .map(|entry| entry.path)
        .collect();
    tree.sort();
    assert_eq!(tree, ["lib", "lib/util.rhai", "manifest.toml"]);
    let top = io.list(&StorePath::root()).await?;
    assert_eq!(top.len(), 1);
    assert_eq!(top[0].name, "demo");
    assert_eq!(top[0].kind, StoreEntryKind::Directory);

    assert!(io.remove_tree(&live).await?);
    assert!(!io.remove_tree(&live).await?);
    Ok(())
}

#[tokio::test]
async fn the_ledger_is_replaced_whole_in_the_stores_root() -> TestResult {
    let root = tempfile::tempdir()?;
    let io = store(&root.path().join("packages"));

    io.replace(&StorePath::ledger_file(), b"one").await?;
    io.replace(&StorePath::ledger_file(), b"two").await?;

    assert_eq!(io.read(&StorePath::ledger_file(), 10).await?, b"two");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_link_inside_a_release_is_listed_as_a_link_and_a_release_that_is_a_link_is_refused() -> TestResult {
    let root = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    std::fs::write(outside.path().join("secret"), b"secret")?;
    let io = store(root.path());
    let live = release("demo", "1.0.0")?;
    io.create_dir_all(&live).await?;
    std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("demo/1.0.0/leak"))?;
    std::os::unix::fs::symlink(outside.path(), root.path().join("demo/2.0.0"))?;

    let tree = io.list_tree(&live, 10).await?;
    let linked = release("demo", "2.0.0")?;

    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].kind, StoreEntryKind::Link);
    assert_eq!(
        io.read(&linked.manifest(), 10).await.map_err(|f| f.kind),
        Err(StoreFailureKind::Refused)
    );
    assert_eq!(
        io.list(&linked).await.map_err(|f| f.kind),
        Err(StoreFailureKind::Refused)
    );
    Ok(())
}

#[tokio::test]
async fn the_lock_is_busy_for_a_second_holder_and_waiting_for_it_leaves_the_executor_free() -> TestResult {
    let root = tempfile::tempdir()?;
    let io = store(root.path());
    let _held = io.lock().await?;

    let ticker = async {
        for _ in 0..1000 {
            tokio::task::yield_now().await;
        }
    };
    let busy = io.lock();
    tokio::pin!(busy);
    tokio::pin!(ticker);

    // The ticker finishes while the second lock is still waiting, so the wait is not on the executor thread.
    tokio::select! {
        biased;
        () = &mut ticker => {}
        _ = &mut busy => return Err(Oops("the second lock was granted".to_owned())),
    }
    assert_eq!(
        busy.await.map(|_| ()).map_err(|f| f.kind),
        Err(StoreFailureKind::LockBusy)
    );
    Ok(())
}
