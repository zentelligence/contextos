use contextos_packages::host_io::HostStoreIo;

use super::*;

fn path(text: &str) -> Result<StorePath, Box<dyn std::error::Error>> {
    Ok(StorePath::root().within(&contextos_packages::ReleaseFilePath::try_from(text)?))
}

#[tokio::test]
async fn the_wrapper_lets_the_calls_before_the_chosen_one_through_and_fails_it_and_every_later_one()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let plan = FaultPlan::failing_from(2);
    let io = FaultyIo {
        inner: HostStoreIo::new(dir.path().to_path_buf(), std::time::Duration::ZERO),
        plan: plan.clone(),
    };

    io.create_dir_all(&path("a")?).await.map_err(|f| f.detail)?;
    io.write_new(&path("a/file")?, b"x").await.map_err(|f| f.detail)?;
    let third = io.write_new(&path("a/other")?, b"x").await;
    let fourth = io.remove_tree(&path("a")?).await;

    assert_eq!(third.err().map(|f| f.kind), Some(StoreFailureKind::Failed));
    assert_eq!(fourth.err().map(|f| f.kind), Some(StoreFailureKind::Failed));
    assert_eq!(plan.attempted(), 4);
    assert!(dir.path().join("a/file").is_file());
    assert!(!dir.path().join("a/other").exists());
    assert_eq!(
        io.list(&path("a")?).await.map_err(|f| f.detail)?.len(),
        1,
        "reads are never gated"
    );
    Ok(())
}

#[tokio::test]
async fn a_plan_that_never_fails_only_counts() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let plan = FaultPlan::never_failing();
    let io = FaultyIo {
        inner: HostStoreIo::new(dir.path().to_path_buf(), std::time::Duration::ZERO),
        plan: plan.clone(),
    };

    io.create_dir_all(&path("a")?).await.map_err(|f| f.detail)?;
    io.replace(&path("a/file")?, b"x").await.map_err(|f| f.detail)?;

    assert_eq!(plan.attempted(), 2);
    Ok(())
}
