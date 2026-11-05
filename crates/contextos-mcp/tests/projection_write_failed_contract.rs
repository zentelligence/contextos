//! A projection that cannot be written arrives as a `projection/write-failed` warning on a vault write that still
//! succeeded. The failure is injected into the host folder swap through the server's test seam, so it happens the same
//! way for every user, including one who can write through any folder permission.

mod support;
use std::io;
use std::sync::Arc;

use contextos_fs::host_file::{FolderReplaceObserver, ReplaceStep};
use serde_json::json;
use support::git_harness::call;
use support::projection_world::{BoxError, ProjectionWorld, skill};

const WRITING: &str = "registry/skills/writing/SKILL.md";

/// Fails the skill folder swap as soon as the new folder is built, before anything in the host folder is touched.
struct FailsBeforeInstalling;

impl FolderReplaceObserver for FailsBeforeInstalling {
    fn reached(&self, step: ReplaceStep) -> io::Result<()> {
        match step {
            ReplaceStep::NewFolderBuilt => Err(io::Error::other("injected failure")),
            ReplaceStep::PreviousMovedAside | ReplaceStep::NewFolderInstalled | ReplaceStep::PreviousRemoved => Ok(()),
        }
    }
}

#[tokio::test]
async fn a_failed_skill_folder_swap_gives_a_write_failed_warning_and_the_vault_write_stands() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let server = world
        .projecting_server()?
        .with_projection_folder_observer(Arc::new(FailsBeforeInstalling));

    let result = call(
        &server,
        "fs_write_file",
        json!({"path": WRITING, "content": skill("writing")}),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let structured = result.structured_content.ok_or("no structured result")?;
    let warnings: Vec<&str> = structured["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|warning| warning.as_str())
        .collect();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.starts_with("projection/write-failed:")),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .all(|warning| !warning.contains(&world.home().to_string_lossy().into_owned())),
        "a warning showed the home path: {warnings:?}"
    );
    assert!(
        world.vault(0).join(WRITING).is_file(),
        "the vault write was rolled back"
    );
    assert!(
        !world.claude_skill("writing").exists(),
        "a skill was installed despite the failed swap"
    );
    Ok(())
}
