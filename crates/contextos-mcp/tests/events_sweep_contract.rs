//! The retention sweep deletes `info` events past their window, keeps what drew attention, reports what it cannot
//! read, removes the month folders it empties, and runs once per UTC day however many processes serve the vault.
mod support;

use std::path::Path;

use contextos_mcp::{ContextOsServer, ModuleRegistry, ServerBuildConfig, ServerClock};
use support::events_vault::{events_harness_with_clock, seed_events_scaffold};
use support::git_harness::{BoxError, Harness};
use time::macros::datetime;

fn write_event(root: &Path, relative: &str, kind: &str, severity: &str, at: &str) -> std::io::Result<()> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        path,
        format!("---\ntype: event\nkind: {kind}\nseverity: {severity}\nat: {at}\nentity: vault\n---\n\n# Event\n"),
    )
}

/// The instant every sweep in this file runs at, so that what counts as old never depends on the real date.
const NOW: time::OffsetDateTime = datetime!(2026-06-15 12:00:00 UTC);

fn clock() -> ServerClock {
    ServerClock::from(|| NOW)
}

const OLD_INFO: &str = "memory/events/2020/01/2020-01-05-000000-old.md";
const OLD_WARNING: &str = "memory/events/2020/01/2020-01-06-000000-warn.md";
const OLD_ERROR: &str = "memory/events/2020/02/2020-02-06-000000-err.md";
const RECENT_INFO: &str = "memory/events/2099/01/2099-01-05-000000-recent.md";

fn seed_old_events(root: &Path) -> std::io::Result<()> {
    seed_events_scaffold(root)?;
    write_event(root, OLD_INFO, "task.created", "info", "2020-01-05T00:00:00Z")?;
    write_event(
        root,
        OLD_WARNING,
        "task.unknown-state",
        "warning",
        "2020-01-06T00:00:00Z",
    )?;
    write_event(root, OLD_ERROR, "job.failed", "error", "2020-02-06T00:00:00Z")?;
    write_event(root, RECENT_INFO, "task.created", "info", "2099-01-05T00:00:00Z")?;
    std::fs::write(root.join("memory/events/index.md"), "# Events\n")
}

async fn harness() -> Result<Harness, BoxError> {
    events_harness_with_clock(|_| {}, seed_old_events, clock()).await
}

#[tokio::test]
async fn info_events_past_the_window_are_deleted_and_everything_else_is_kept() -> Result<(), BoxError> {
    let harness = harness().await?;
    let reports = harness.server.sweep_events().await?;

    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].deleted, 1, "{reports:?}");
    let root = harness.vault.path();
    assert!(!root.join(OLD_INFO).exists());
    assert!(root.join(OLD_WARNING).exists(), "warnings are kept");
    assert!(root.join(OLD_ERROR).exists(), "errors are kept");
    assert!(root.join(RECENT_INFO).exists());
    assert!(root.join("memory/events/index.md").exists());
    Ok(())
}

#[tokio::test]
async fn a_month_folder_the_sweep_empties_is_removed_and_one_that_still_holds_events_is_not() -> Result<(), BoxError> {
    let harness = harness().await?;
    write_event(
        harness.vault.path(),
        "memory/events/2020/03/2020-03-01-000000-a.md",
        "task.created",
        "info",
        "2020-03-01T00:00:00Z",
    )?;
    harness.server.sweep_events().await?;

    let root = harness.vault.path();
    assert!(
        !root.join("memory/events/2020/03").exists(),
        "an emptied month folder goes"
    );
    assert!(
        root.join("memory/events/2020/01").exists(),
        "a month folder that still holds a warning stays"
    );
    assert!(root.join("memory/events/2099/01").exists());
    Ok(())
}

#[tokio::test]
async fn an_event_whose_time_cannot_be_read_is_kept_and_reported() -> Result<(), BoxError> {
    let harness = harness().await?;
    let path = "memory/events/2020/04/2020-04-01-000000-odd.md";
    write_event(harness.vault.path(), path, "task.created", "info", "not-a-time")?;

    let reports = harness.server.sweep_events().await?;

    assert!(harness.vault.path().join(path).exists());
    assert_eq!(reports[0].unreadable, vec![path.to_owned()]);
    Ok(())
}

#[tokio::test]
async fn a_kinds_own_window_applies_to_its_info_events() -> Result<(), BoxError> {
    let harness = harness().await?;
    // import.completed declares 400 days; this one is well inside that but far older than the default 90.
    let recorded = (NOW - time::Duration::days(200)).format(&time::format_description::well_known::Rfc3339)?;
    let path = "memory/events/2025/01/2025-01-01-000000-import-completed.md";
    write_event(harness.vault.path(), path, "import.completed", "info", &recorded)?;
    let plain = "memory/events/2025/01/2025-01-01-000000-task-created.md";
    write_event(harness.vault.path(), plain, "task.created", "info", &recorded)?;

    harness.server.sweep_events().await?;

    assert!(
        harness.vault.path().join(path).exists(),
        "inside its own 400 day window"
    );
    assert!(
        !harness.vault.path().join(plain).exists(),
        "outside the vault's 90 day window"
    );
    Ok(())
}

#[tokio::test]
async fn the_vault_window_and_keeping_attention_are_configurable() -> Result<(), BoxError> {
    let harness = events_harness_with_clock(
        |config| {
            for vault in &mut config.vaults {
                vault.events.keep_attention = false;
            }
        },
        seed_old_events,
        clock(),
    )
    .await?;
    harness.server.sweep_events().await?;

    let root = harness.vault.path();
    assert!(!root.join(OLD_WARNING).exists());
    assert!(!root.join(OLD_ERROR).exists());
    assert!(root.join(RECENT_INFO).exists());
    Ok(())
}

#[tokio::test]
async fn the_vault_sweeps_once_a_day_even_with_two_processes() -> Result<(), BoxError> {
    let harness = harness().await?;
    let other = ContextOsServer::with_clock(
        ServerBuildConfig {
            config: support::scoped_config(vec![harness.vault.path().to_path_buf()])?,
            modules: ModuleRegistry::new(),
        },
        clock(),
    )?;
    harness.server.sweep_events().await?;
    write_event(
        harness.vault.path(),
        "memory/events/2020/05/2020-05-01-000000-late.md",
        "task.created",
        "info",
        "2020-05-01T00:00:00Z",
    )?;

    let again = other.sweep_events().await?;

    assert_eq!(again[0].deleted, 0);
    assert!(!again[0].swept, "{again:?}");
    assert!(
        harness
            .vault
            .path()
            .join("memory/events/2020/05/2020-05-01-000000-late.md")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn deletions_are_staged_in_git_and_the_sweep_raises_no_events() -> Result<(), BoxError> {
    let harness = harness().await?;
    harness.settle()?;
    let before = harness.commit_count().await?;
    harness.server.sweep_events().await?;

    let pending = harness.pending_paths().await?;
    assert!(pending.contains(&OLD_INFO.to_owned()), "{pending:?}");
    harness.settle()?;
    assert_eq!(harness.commit_count().await?, before + 1);
    assert_eq!(
        support::events_vault::events_of_kind(harness.vault.path(), "task.created").len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn the_sweep_judges_age_by_the_servers_clock() -> Result<(), BoxError> {
    let early = ServerClock::from(|| datetime!(2026-02-01 00:00:00 UTC));
    let harness = events_harness_with_clock(
        |_| {},
        |root| {
            seed_old_events(root)?;
            write_event(
                root,
                "memory/events/2026/01/2026-01-01-000000-a.md",
                "task.created",
                "info",
                "2026-01-01T00:00:00Z",
            )
        },
        early,
    )
    .await?;
    harness.server.sweep_events().await?;
    assert!(
        harness
            .vault
            .path()
            .join("memory/events/2026/01/2026-01-01-000000-a.md")
            .exists(),
        "31 days old at the server's time, so inside the 90 day window"
    );
    Ok(())
}

#[tokio::test]
async fn a_note_in_the_events_folder_that_is_not_an_event_is_never_deleted() -> Result<(), BoxError> {
    let harness = harness().await?;
    let stray = "memory/events/2020/06/2020-06-01-000000-stray.md";
    let path = harness.vault.path().join(stray);
    std::fs::create_dir_all(path.parent().ok_or("no parent")?)?;
    std::fs::write(
        &path,
        "---\ntype: note\nkind: task.created\nseverity: info\nat: 2020-06-01T00:00:00Z\n---\n\n# Mine\n",
    )?;

    let reports = harness.server.sweep_events().await?;

    assert!(path.exists(), "{reports:?}");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_symbolic_link_in_the_events_folder_is_never_followed_or_deleted() -> Result<(), BoxError> {
    let harness = harness().await?;
    let root = harness.vault.path();
    // An old `info` event-shaped note outside the events folder, which a followed link would delete.
    write_event(
        root,
        "projects/precious.md",
        "task.created",
        "info",
        "2020-01-05T00:00:00Z",
    )?;
    let month = root.join("memory/events/2020/07");
    std::fs::create_dir_all(&month)?;
    std::os::unix::fs::symlink(
        root.join("projects/precious.md"),
        month.join("2020-07-01-000000-link.md"),
    )?;
    std::os::unix::fs::symlink(root.join("projects"), month.join("linked-folder"))?;

    let reports = harness.server.sweep_events().await?;

    assert!(root.join("projects/precious.md").exists(), "{reports:?}");
    assert_eq!(reports[0].deleted, 1, "only the genuine old event goes: {reports:?}");
    assert!(month.join("2020-07-01-000000-link.md").symlink_metadata().is_ok());
    Ok(())
}

fn write_about(root: &Path, relative: &str, kind: &str, severity: &str, at: &str) -> std::io::Result<()> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        path,
        format!(
            "---\ntype: event\nkind: {kind}\nseverity: {severity}\nat: {at}\nentity: vault\nsubject: \"[[memory/tasks/t|t]]\"\n---\n\n# Event\n"
        ),
    )
}

#[tokio::test]
async fn a_cleared_warning_is_swept_with_its_clearing_event_while_an_open_one_is_kept() -> Result<(), BoxError> {
    let harness = harness().await?;
    let root = harness.vault.path();
    let cleared = "memory/events/2020/04/2020-04-01-000000-stuck.md";
    let clearing = "memory/events/2020/04/2020-04-02-000000-done.md";
    write_about(root, cleared, "task.stuck", "warning", "2020-04-01T00:00:00Z")?;
    write_about(root, clearing, "task.done", "info", "2020-04-02T00:00:00Z")?;

    harness.server.sweep_events().await?;

    assert!(
        !root.join(cleared).exists(),
        "a cleared warning ages out on its own time"
    );
    assert!(!root.join(clearing).exists());
    assert!(root.join(OLD_WARNING).exists(), "an open warning is kept");
    assert!(root.join(OLD_ERROR).exists(), "an open error is kept");
    Ok(())
}

#[tokio::test]
async fn an_event_that_clears_a_surviving_warning_is_kept_so_the_warning_never_reappears_as_open()
-> Result<(), BoxError> {
    let harness = harness().await?;
    let root = harness.vault.path();
    let cleared = "memory/events/2026/06/2026-06-10-000000-stuck.md";
    let clearing = "memory/events/2026/06/2026-06-11-000000-done.md";
    write_about(root, cleared, "task.stuck", "warning", "2026-06-10T00:00:00Z")?;
    write_about(root, clearing, "task.done", "info", "2026-06-11T00:00:00Z")?;

    harness.server.sweep_events().await?;

    assert!(root.join(cleared).exists());
    assert!(root.join(clearing).exists());
    Ok(())
}

/// A `tracing` destination the test can read back.
#[cfg(unix)]
#[derive(Clone, Default)]
struct LogCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

#[cfg(unix)]
impl std::io::Write for LogCapture {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut held) = self.0.lock() {
            held.extend_from_slice(buffer);
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(unix)]
#[tokio::test]
async fn an_expired_event_that_cannot_be_deleted_is_logged_by_path_and_deleted_at_the_next_sweep()
-> Result<(), BoxError> {
    let capture = LogCapture::default();
    let installed = tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_ansi(false)
            .finish(),
    );
    assert!(
        installed.is_ok(),
        "this is the only test in the file that installs a subscriber"
    );
    let now = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(NOW.unix_timestamp()));
    let clock_now = std::sync::Arc::clone(&now);
    let harness = events_harness_with_clock(
        |_| {},
        seed_old_events,
        ServerClock::from(move || {
            time::OffsetDateTime::from_unix_timestamp(clock_now.load(std::sync::atomic::Ordering::SeqCst))
                .unwrap_or(NOW)
        }),
    )
    .await?;
    let folder = harness.vault.path().join("memory/events/2020/01");
    // A folder that cannot be written to refuses the removal of the notes in it.
    set_mode(&folder, 0o555)?;

    let first = harness.server.sweep_events().await?;
    set_mode(&folder, 0o755)?;

    assert_eq!(first[0].failed, 1, "{first:?}");
    assert!(
        harness.vault.path().join(OLD_INFO).exists(),
        "a failed deletion leaves the note"
    );
    let logged = String::from_utf8_lossy(&capture.0.lock().map_err(|_| "log lock")?).into_owned();
    assert!(
        logged.contains(OLD_INFO) && logged.contains("retried at the next sweep"),
        "the vault-relative path is logged: {logged}"
    );

    now.fetch_add(86_400, std::sync::atomic::Ordering::SeqCst);
    let next_day = harness.server.sweep_events().await?;

    assert!(next_day[0].swept && next_day[0].deleted >= 1, "{next_day:?}");
    assert!(
        !harness.vault.path().join(OLD_INFO).exists(),
        "the next sweep retries it"
    );
    Ok(())
}

#[tokio::test]
async fn a_server_that_starts_serving_sweeps_before_it_waits_for_the_next_day() -> Result<(), BoxError> {
    let harness = harness().await?;
    let shutdown = tokio_util::sync::CancellationToken::new();
    let maintenance = harness.server.spawn_events_maintenance(&shutdown);
    // The task sweeps first and only then waits, so a token already cancelled lets it finish having swept.
    shutdown.cancel();
    maintenance.await?;

    assert!(
        !harness.vault.path().join(OLD_INFO).exists(),
        "the start-up sweep deleted the expired event"
    );
    assert!(harness.vault.path().join(RECENT_INFO).exists());
    Ok(())
}
