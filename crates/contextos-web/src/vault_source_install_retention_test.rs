//! Retention after an install from source into a vault's store.

use contextos_workflows::PackageName;

use super::*;
use crate::source_install_world::World;

type BoxError = Box<dyn std::error::Error>;

const SYNC: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\n";

fn lay_out(world: &World, script: &str) -> Result<(), BoxError> {
    world.write("registry/workflows/ops/workflows.toml", "workflows = [\"sync\"]\n")?;
    world.write("registry/workflows/ops/sync/workflow.toml", SYNC)?;
    world.write("registry/workflows/ops/sync/sync.rhai", script)?;
    Ok(())
}

async fn install(world: &World) -> Result<SourceInstalled, BoxError> {
    let client = world.session().await?;
    Ok(SourceInstall {
        client: &client,
        vault: "vault",
    }
    .install(
        &SourceTarget {
            kind: SourceKind::Workflows,
            name: PackageName::try_from("ops")?,
        },
        None,
    )
    .await?)
}

fn versions(world: &World) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(world.vault().join(".contextos/packages/ops")) else {
        return Vec::new();
    };
    let mut found: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found
}

#[tokio::test]
async fn restoring_an_older_release_nothing_keeps_is_removed_at_once_and_says_so() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out(&world, "let x = 1;")?;
    let first = install(&world).await?;
    lay_out(&world, "let x = 2;")?;
    let second = install(&world).await?;
    world.write(
        ".contextos/jobs.toml",
        &format!("[ops.sync]\ncurrent = \"{}\"\n", second.release.version),
    )?;
    lay_out(&world, "let x = 1;")?;
    let client = world.session().await?;
    let at = semver::Version::parse(&first.release.version)?;

    let restored = SourceInstall {
        client: &client,
        vault: "vault",
    }
    .install(
        &SourceTarget {
            kind: SourceKind::Workflows,
            name: PackageName::try_from("ops")?,
        },
        Some(&at),
    )
    .await?;

    assert_eq!(versions(&world), std::slice::from_ref(&second.release.version));
    assert!(restored.removed_by_retention, "{restored:?}");
    Ok(())
}

#[tokio::test]
async fn an_install_that_retention_keeps_does_not_say_it_was_removed() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out(&world, "let x = 1;")?;

    let installed = install(&world).await?;

    assert!(!installed.removed_by_retention);
    Ok(())
}

#[tokio::test]
async fn installing_a_newer_release_removes_the_older_one_nothing_keeps() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out(&world, "let x = 1;")?;
    let first = install(&world).await?;
    lay_out(&world, "let x = 2;")?;

    let second = install(&world).await?;

    assert_eq!(versions(&world), std::slice::from_ref(&second.release.version));
    assert_ne!(first.release.version, second.release.version);
    assert!(second.warnings.is_empty(), "{:?}", second.warnings);
    assert_eq!(second.standing.get("sync").map(String::as_str), Some("future"));
    Ok(())
}

#[tokio::test]
async fn installing_a_newer_release_keeps_the_one_a_job_file_names() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out(&world, "let x = 1;")?;
    let first = install(&world).await?;
    world.write(
        "registry/workflows/ops/sync/jobs/nightly.toml",
        &format!("version = \"{}\"\n", first.release.version),
    )?;
    lay_out(&world, "let x = 2;")?;

    let second = install(&world).await?;

    assert_eq!(versions(&world), [first.release.version, second.release.version]);
    Ok(())
}

#[tokio::test]
async fn an_install_whose_retention_cannot_finish_still_succeeds_and_carries_the_warning() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out(&world, "let x = 1;")?;
    install(&world).await?;
    lay_out(&world, "let x = 2;")?;
    // A file where the workflow's jobs folder belongs: what names the installed releases cannot be read.
    world.write("registry/workflows/ops/sync/jobs", "not a folder")?;

    let second = install(&world).await?;

    assert_eq!(versions(&world).len(), 2, "{:?}", versions(&world));
    assert_eq!(second.warnings.len(), 1, "{:?}", second.warnings);
    assert!(
        second.warnings[0]
            .to_string()
            .starts_with("packages/retention-failed: ")
    );
    Ok(())
}
