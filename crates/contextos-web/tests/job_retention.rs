//! Retention-sweep contract tests: real
//! `contextos-mcp` sessions against real temporary vault fixtures, per
//! `testing.md`. `now` and the retention
//! window are both passed explicitly to
//! [`contextos_web::retention::sweep_vault`] rather than read from the
//! wall clock, so these fixtures stay deterministic regardless of when
//! the suite runs.

mod support;

use std::path::Path;
use std::sync::Arc;

use contextos_web::{McpClientSet, RegisteredJob};
use time::Duration as TimeDuration;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";
const RETENTION_WINDOW_DAYS: i64 = 90;

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

fn rfc3339(instant: OffsetDateTime) -> String {
    instant.format(&Rfc3339).unwrap_or_else(|_| instant.to_string())
}

fn run_record(
    workflow: &str,
    job: &str,
    run_id: u64,
    status: &str,
    started: OffsetDateTime,
    completed: OffsetDateTime,
) -> String {
    format!(
        "---\n\
         type: job-run\n\
         title: \"Run: {workflow}/{job} R-{run_id}\"\n\
         entity: zenith\n\
         id: R-{run_id}\n\
         workflow: {workflow}\n\
         job: {job}\n\
         trigger: scheduled\n\
         status: {status}\n\
         started: {}\n\
         completed: {}\n\
         error:\n\
         ---\n",
        rfc3339(started),
        rfc3339(completed)
    )
}

async fn connect(vault_dir: &Path, config_dir: &Path) -> Result<Arc<McpClientSet>, BoxError> {
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    Ok(Arc::new(McpClientSet::connect(&[entry]).await?))
}

fn job(workflow_slug: &str, slug: &str) -> RegisteredJob {
    RegisteredJob {
        templates: contextos_workflows::DeclaredFiles::default(),
        modules: contextos_workflows::DeclaredModules::default(),
        libraries: Vec::new(),
        package: workflow_slug.to_owned(),
        workflow_slug: workflow_slug.to_owned(),
        owner: None,
        declared_owner: None,
        entity: contextos_web::jobs::ResolvedEntity::from((None, None)),
        release: contextos_workflows::JobFileRelease {
            version: contextos_workflows::JobVersion::from(contextos_workflows::JobVersionNumbers {
                major: 1,
                minor: 0,
                patch: 0,
            }),
            predecessor: None,
        },
        slug: slug.to_owned(),
        name: slug.to_owned(),
        entry: "workflow.rhai".to_owned(),
        timeout_secs: 300,
        calls: contextos_workflows::call_rules::CallRules::default(),
        inputs: contextos_workflows::ValidatedInputs::default(),
        declared_inputs: contextos_workflows::DeclaredInputs::default(),
        outputs: contextos_workflows::DeclaredOutputs::default(),
        workflow_log_level: None,
        log_level: None,
        ready: false,
        file_name: "nightly.toml".to_owned(),
    }
}

#[tokio::test]
async fn an_aged_succeeded_run_is_archived_and_deleted_while_recent_and_failed_runs_are_kept() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;

    let now = OffsetDateTime::now_utc();
    let old = now - TimeDuration::days(RETENTION_WINDOW_DAYS + 1);
    let recent = now - TimeDuration::days(1);

    write(
        vault_dir.path(),
        "memory/jobs/sync-crm/sync-crm/nightly/2020/01/R-1.md",
        &run_record(
            "sync-crm",
            "nightly",
            1,
            "succeeded",
            old,
            old + TimeDuration::seconds(5),
        ),
    )?;
    write(
        vault_dir.path(),
        "memory/jobs/sync-crm/sync-crm/nightly/2026/09/R-2.md",
        &run_record(
            "sync-crm",
            "nightly",
            2,
            "succeeded",
            recent,
            recent + TimeDuration::seconds(5),
        ),
    )?;
    write(
        vault_dir.path(),
        "memory/jobs/sync-crm/sync-crm/nightly/2020/01/R-3.md",
        &run_record("sync-crm", "nightly", 3, "failed", old, old + TimeDuration::seconds(5)),
    )?;

    let clients = connect(vault_dir.path(), config_dir.path()).await?;
    let client = clients.get("contextos").ok_or("client not configured")?;
    let jobs = [job("sync-crm", "nightly")];
    let window = TimeDuration::days(RETENTION_WINDOW_DAYS);
    contextos_web::retention::sweep_vault(&client, VAULT_NAME, &jobs, now, window).await;

    assert!(
        !vault_dir
            .path()
            .join("memory/jobs/sync-crm/sync-crm/nightly/2020/01/R-1.md")
            .exists(),
        "an aged succeeded run should have been deleted"
    );
    assert!(
        vault_dir
            .path()
            .join("memory/jobs/sync-crm/sync-crm/nightly/2026/09/R-2.md")
            .exists(),
        "a recent succeeded run must not be swept"
    );
    assert!(
        vault_dir
            .path()
            .join("memory/jobs/sync-crm/sync-crm/nightly/2020/01/R-3.md")
            .exists(),
        "a failed run is exempt from the sweep regardless of age"
    );

    let ledger = std::fs::read_to_string(
        vault_dir
            .path()
            .join("memory/jobs/sync-crm/sync-crm/nightly/archive.md"),
    )?;
    assert!(ledger.contains("R-1 |"));
    assert!(ledger.contains("succeeded"));
    assert!(ledger.contains("5s"));
    assert!(!ledger.contains("R-2 |"), "only the aged run is archived");
    assert!(!ledger.contains("R-3 |"), "an exempt run is never archived");
    Ok(())
}

#[tokio::test]
async fn a_run_whose_ledger_line_already_exists_is_deleted_without_duplicating_it() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;

    let now = OffsetDateTime::now_utc();
    let old = now - TimeDuration::days(RETENTION_WINDOW_DAYS + 1);

    // Simulates a crash between the ledger append and the delete step:
    // the run record still exists, and its ledger line has already been
    // written.
    write(
        vault_dir.path(),
        "memory/jobs/always-succeeds/always-succeeds/run/2020/01/R-9.md",
        &run_record(
            "always-succeeds",
            "run",
            9,
            "succeeded",
            old,
            old + TimeDuration::seconds(2),
        ),
    )?;
    write(
        vault_dir.path(),
        "memory/jobs/always-succeeds/always-succeeds/run/archive.md",
        &format!("- R-9 | {} | succeeded | 2s\n", rfc3339(old + TimeDuration::seconds(2))),
    )?;

    let clients = connect(vault_dir.path(), config_dir.path()).await?;
    let client = clients.get("contextos").ok_or("client not configured")?;
    let jobs = [job("always-succeeds", "run")];
    let window = TimeDuration::days(RETENTION_WINDOW_DAYS);
    contextos_web::retention::sweep_vault(&client, VAULT_NAME, &jobs, now, window).await;

    assert!(
        !vault_dir
            .path()
            .join("memory/jobs/always-succeeds/always-succeeds/run/2020/01/R-9.md")
            .exists(),
        "an already-archived record is still deleted on re-sweep"
    );
    let ledger = std::fs::read_to_string(
        vault_dir
            .path()
            .join("memory/jobs/always-succeeds/always-succeeds/run/archive.md"),
    )?;
    assert_eq!(
        ledger.matches("R-9 |").count(),
        1,
        "re-sweeping an already-archived run must not duplicate its ledger line, got:\n{ledger}"
    );
    Ok(())
}

#[tokio::test]
async fn an_aged_degraded_run_is_kept_like_a_failed_one_and_never_archived() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let now = OffsetDateTime::now_utc();
    let old = now - TimeDuration::days(RETENTION_WINDOW_DAYS + 1);
    let record = "memory/jobs/sync-crm/sync-crm/nightly/2020/01/R-1.md";
    write(
        vault_dir.path(),
        record,
        &run_record(
            "sync-crm",
            "nightly",
            1,
            "degraded",
            old,
            old + TimeDuration::seconds(5),
        ),
    )?;

    let clients = connect(vault_dir.path(), config_dir.path()).await?;
    let client = clients.get("contextos").ok_or("client not configured")?;
    let jobs = [job("sync-crm", "nightly")];
    contextos_web::retention::sweep_vault(
        &client,
        VAULT_NAME,
        &jobs,
        now,
        TimeDuration::days(RETENTION_WINDOW_DAYS),
    )
    .await;

    assert!(vault_dir.path().join(record).exists(), "a degraded run must be kept");
    assert!(
        !vault_dir
            .path()
            .join("memory/jobs/sync-crm/sync-crm/nightly/archive.md")
            .exists(),
        "a degraded run must not be rolled into the archive"
    );
    Ok(())
}
