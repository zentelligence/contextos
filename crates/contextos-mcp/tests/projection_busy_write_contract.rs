//! A write whose projection cannot take the host-level lock in time still succeeds, and says so with the warning
//! `projection/busy`.

mod support;
use std::fs::OpenOptions;
use std::time::Duration;

use serde_json::json;
use support::git_harness::call;
use support::projection_world::{BoxError, ProjectionWorld, agent};

#[tokio::test]
async fn a_write_while_another_process_holds_the_projection_lock_succeeds_with_a_busy_warning() -> Result<(), BoxError>
{
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let server = world
        .projecting_server()?
        .with_projection_lock_wait(Duration::from_millis(200));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(world.home().join(".contextos/projection.lock"))?;
    assert!(contextos_fs::try_lock_exclusive(&file)?, "the lock was already held");

    let result = call(
        &server,
        "fs_write_file",
        json!({"path": "registry/agents/reviewer.md", "content": agent("Review it.")}),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let structured = result.structured_content.ok_or("no structured result")?;
    let warnings = structured["warnings"].to_string();
    assert!(warnings.contains("projection/busy"), "{warnings}");
    assert!(world.vault(0).join("registry/agents/reviewer.md").is_file());
    assert!(!world.claude_agent("reviewer").exists());
    Ok(())
}
