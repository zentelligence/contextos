//! Every `contextos` process on the host projects into the same agent host folders, so each change to them waits for
//! the host-level projection lock rather than racing another process. Each test plays the other process by holding the
//! lock file through its own handle, as another instance would, and shows the change waits for it and then completes.

mod support;
use std::fs::{self, File, OpenOptions};
use std::time::Duration;

use serde_json::json;
use support::git_harness::call;
use support::projection_doctor::REVIEWER;
use support::projection_world::{BoxError, ProjectionWorld, agent};

/// How long the other process keeps the lock before the test looks; the default wait is far longer.
const HELD_FOR: Duration = Duration::from_millis(300);

/// Holds the projection lock the way another `contextos` process on the host would, until unlocked or dropped.
fn hold_the_projection_lock(world: &ProjectionWorld) -> Result<File, BoxError> {
    let state_root = world.home().join(".contextos");
    fs::create_dir_all(&state_root)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state_root.join("projection.lock"))?;
    assert!(contextos_fs::try_lock_exclusive(&file)?, "the lock was already held");
    Ok(file)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_projects_only_once_another_process_releases_the_projection_lock() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let server = world.projecting_server()?;
    let other_process = hold_the_projection_lock(&world)?;

    let writer = tokio::spawn({
        let server = server.clone();
        async move {
            call(
                &server,
                "fs_write_file",
                json!({"path": REVIEWER, "content": agent("Review it.")}),
            )
            .await
        }
    });
    tokio::time::sleep(HELD_FOR).await;
    assert!(
        !world.claude_agent("reviewer").exists(),
        "the agent was projected while another process held the lock"
    );
    assert!(
        !writer.is_finished(),
        "the write finished while another process held the lock"
    );

    other_process.unlock()?;
    let result = writer.await??;
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let warnings = result
        .structured_content
        .as_ref()
        .and_then(|body| body.get("warnings"))
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    assert_eq!(warnings, 0, "{result:?}");
    assert!(
        world.claude_agent("reviewer").is_file(),
        "the agent was not projected once the lock was free"
    );
    assert!(
        world.codex_agent("reviewer").is_file(),
        "the agent was not projected once the lock was free"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_starting_server_refreshes_only_once_another_process_releases_the_projection_lock() -> Result<(), BoxError> {
    let world = std::sync::Arc::new(ProjectionWorld::new(1)?);
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    let other_process = hold_the_projection_lock(&world)?;

    let starting = tokio::task::spawn_blocking({
        let world = std::sync::Arc::clone(&world);
        move || world.projecting_server().map(drop).map_err(|error| error.to_string())
    });
    tokio::time::sleep(HELD_FOR).await;
    assert!(
        !world.claude_agent("reviewer").exists(),
        "the start-up refresh projected while another process held the lock"
    );
    assert!(
        !starting.is_finished(),
        "the server started while another process held the lock"
    );

    other_process.unlock()?;
    starting.await??;
    assert!(
        world.claude_agent("reviewer").is_file(),
        "the refresh did not project once the lock was free"
    );
    Ok(())
}
