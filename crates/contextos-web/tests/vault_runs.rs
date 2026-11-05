//! What a vault job's run does on the host rather than in the vault: its claim, its run number, the tick it fires on, and
//! the cancel request that stops it. Real `contextos-mcp` sessions against real temporary vaults and a real claims
//! directory, so the claim is a real operating-system lock.

mod support;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use contextos_web::McpClientSet;
use contextos_web::cli_run::{CliRunError, CliRunOptions};
use contextos_web::run_claim::{ActiveRun, ClaimSlot, ClaimState, RunClaim};
use contextos_web::run_settings::HostContext;
use contextos_web::scheduler::poll_once;
use contextos_web::vault_runs::claim_slot;
use contextos_workflows::{Clock, RunOutcome};
use time::OffsetDateTime;
use time::macros::datetime;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

/// 03:00:30 on a day when a job scheduled every minute fell due at 03:00:00, inside the scheduler's look-back window.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        datetime!(2026-10-01 03:00:30 UTC)
    }
}

const WORKFLOW: &str = "name = \"Sync\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const JOB: &str = "name = \"Job\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

struct Fixture {
    vault: tempfile::TempDir,
    config: tempfile::TempDir,
    clients: Arc<McpClientSet>,
}

impl Fixture {
    /// A vault with a workflow whose script is `script` and one job per name in `jobs`, each in `lifecycle` with a schedule
    /// that fires every minute.
    async fn new(script: &str, jobs: &[&str], lifecycle: &str) -> Result<Self, BoxError> {
        let vault = tempfile::tempdir()?;
        let config = tempfile::tempdir()?;
        write(vault.path(), "index.md", "# Root\n")?;
        write(vault.path(), ".contextos/team.toml", support::jobs::TEAM)?;
        write(
            vault.path(),
            "registry/workflows/ops/workflows.toml",
            "workflows = [\"sync\"]\n",
        )?;
        write(vault.path(), "registry/workflows/ops/sync/workflow.toml", WORKFLOW)?;
        write(vault.path(), "registry/workflows/ops/sync/workflow.rhai", script)?;
        let mut state = String::new();
        for job in jobs {
            write(
                vault.path(),
                &format!("registry/workflows/ops/sync/jobs/{job}.toml"),
                JOB,
            )?;
            writeln!(
                state,
                "[ops.sync.{job}]\nlifecycle = \"{lifecycle}\"\nschedule = \"* * * * *\"\n"
            )?;
        }
        write(vault.path(), ".contextos/jobs.toml", &state)?;
        support::release_fixture::install_vault_releases(vault.path()).map_err(|error| error.to_string())?;
        let config_path = support::write_vault_config(config.path(), vault.path())?;
        let entry = support::real_contextos_entry("contextos", &config_path)?;
        let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
        Ok(Self { vault, config, clients })
    }

    fn host(&self) -> HostContext {
        support::host(self.config.path())
    }

    fn slot(&self, job: &str) -> Result<ClaimSlot, BoxError> {
        Ok(claim_slot(&self.host(), VAULT_NAME, "ops", "sync", job)?)
    }

    fn state_text(&self) -> Result<String, std::io::Error> {
        std::fs::read_to_string(self.vault.path().join(".contextos/jobs.toml"))
    }

    /// Every run record of `job`, as (file name, text).
    fn records(&self, job: &str) -> Vec<(String, String)> {
        let mut found = Vec::new();
        let mut pending = vec![self.vault.path().join(format!("memory/jobs/ops/sync/{job}"))];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if let (Some(name), Ok(text)) = (
                    path.file_name().and_then(|name| name.to_str()),
                    std::fs::read_to_string(&path),
                ) && name.starts_with("R-")
                {
                    found.push((name.to_owned(), text));
                }
            }
        }
        found.sort();
        found
    }

    fn record_path(&self, job: &str, number: u64) -> PathBuf {
        self.vault
            .path()
            .join(format!("memory/jobs/ops/sync/{job}/2026/10/{}", record_name(number)))
    }

    fn write_record(&self, job: &str, number: u64, text: &str) -> std::io::Result<()> {
        write(
            self.vault.path(),
            &format!("memory/jobs/ops/sync/{job}/2026/10/{}", record_name(number)),
            text,
        )
    }

    async fn live_run(&self, job: &str) -> Result<RunOutcome, CliRunError> {
        support::run_job(
            Arc::clone(&self.clients),
            &self.host(),
            VAULT_NAME,
            "ops",
            "sync",
            job,
            CliRunOptions {
                dry_run_scenario: None,
                record: true,
            },
        )
        .await
    }

    async fn poll(&self) {
        poll_once(&self.clients, &self.host(), &FixedClock).await;
    }
}

/// Waits, against a real deadline and never a fixed pause, until `ready` holds.
async fn eventually(what: &str, mut ready: impl FnMut() -> bool) -> Result<(), BoxError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !ready() {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for {what}").into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Ok(())
}

/// The file name of run `number`'s record.
fn record_name(number: u64) -> String {
    format!("R-{number}.md")
}

/// A run record as the scheduler writes one, for a run that is `status`.
fn record_text(number: u64, status: &str, dry_run: bool) -> String {
    let completed = if status == "running" {
        ""
    } else {
        " 2026-10-01T03:00:01Z"
    };
    format!(
        "---\ntype: job-run\nid: R-{number}\nworkflow: sync\njob: nightly\ntrigger: manual\nstatus: {status}\ndry_run: {dry_run}\nstarted: 2026-10-01T03:00:00Z\ncompleted:{completed}\nerror:\n---\n"
    )
}

fn status_of(record: &str) -> &str {
    record
        .lines()
        .find_map(|line| line.strip_prefix("status:"))
        .map_or("", str::trim)
}

#[tokio::test]
async fn a_tick_for_a_job_that_is_still_running_is_recorded_skipped_while_another_job_in_the_vault_runs()
-> Result<(), BoxError> {
    let fixture = Fixture::new("()", &["nightly", "weekly"], "enabled").await?;
    let _running = RunClaim::acquire(&fixture.slot("nightly")?)?;

    fixture.poll().await;

    let skipped = fixture.records("nightly");
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!(status_of(&skipped[0].1), "skipped");
    eventually("the other job's run to end", || {
        fixture
            .records("weekly")
            .first()
            .is_some_and(|(_, text)| status_of(text) == "succeeded")
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn a_tick_fires_once_however_many_passes_see_it() -> Result<(), BoxError> {
    let fixture = Fixture::new("()", &["nightly"], "enabled").await?;

    fixture.poll().await;
    eventually("the run to end", || {
        fixture
            .records("nightly")
            .first()
            .is_some_and(|(_, text)| status_of(text) == "succeeded")
    })
    .await?;
    fixture.poll().await;
    fixture.poll().await;

    assert_eq!(fixture.records("nightly").len(), 1, "the tick did not fire again");
    Ok(())
}

#[tokio::test]
async fn a_scheduled_run_writes_no_run_bookkeeping_to_the_vaults_job_state() -> Result<(), BoxError> {
    let fixture = Fixture::new("()", &["nightly"], "enabled").await?;

    fixture.poll().await;
    eventually("the run to end", || {
        fixture
            .records("nightly")
            .first()
            .is_some_and(|(_, text)| status_of(text) == "succeeded")
    })
    .await?;

    let state = fixture.state_text()?;
    for host_owned in ["current_run", "next_run_id", "last_scheduled_fire"] {
        assert!(
            !state.contains(host_owned),
            "which run is active, the run number, and the tick fired are the host's, not the vault's: {state}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_run_that_died_leaves_a_record_the_next_run_marks_interrupted() -> Result<(), BoxError> {
    let fixture = Fixture::new("()", &["nightly"], "registered").await?;
    fixture.write_record("nightly", 3, &record_text(3, "running", false))?;

    let outcome = fixture.live_run("nightly").await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    let records = fixture.records("nightly");
    let marked = records
        .iter()
        .find(|(name, _)| *name == record_name(3))
        .ok_or("the abandoned record vanished")?;
    assert_eq!(status_of(&marked.1), "interrupted", "{}", marked.1);
    assert!(
        marked.1.contains("last call may or may not") && !marked.1.contains("completed:\n"),
        "it says when and why it ended: {}",
        marked.1
    );
    assert!(
        records.iter().any(|(name, _)| *name == record_name(4)),
        "the new run follows the highest record: {records:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_dry_run_that_is_still_writing_its_record_is_never_marked_interrupted() -> Result<(), BoxError> {
    let fixture = Fixture::new("()", &["nightly"], "registered").await?;
    fixture.write_record("nightly", 3, &record_text(3, "running", true))?;

    fixture.live_run("nightly").await?;

    let text = std::fs::read_to_string(fixture.record_path("nightly", 3))?;
    assert_eq!(
        status_of(&text),
        "running",
        "a dry run takes no claim, so it is never abandoned"
    );
    Ok(())
}

#[tokio::test]
async fn run_numbers_are_per_job_and_start_after_the_highest_record_the_vault_holds() -> Result<(), BoxError> {
    let fixture = Fixture::new("()", &["nightly", "weekly"], "registered").await?;
    fixture.write_record("nightly", 2, &record_text(2, "succeeded", false))?;
    fixture.write_record("nightly", 9, &record_text(9, "succeeded", false))?;

    fixture.live_run("nightly").await?;
    fixture.live_run("weekly").await?;
    fixture.live_run("nightly").await?;

    let names = |job| -> Vec<String> { fixture.records(job).into_iter().map(|(name, _)| name).collect() };
    assert_eq!(
        names("nightly"),
        [record_name(10), record_name(11), record_name(2), record_name(9)],
        "the first run follows the highest record the vault holds"
    );
    assert_eq!(names("weekly"), [record_name(1)], "another job's numbers are its own");
    Ok(())
}

#[tokio::test]
async fn a_second_live_run_of_a_running_job_is_refused_as_busy_and_a_different_job_is_not() -> Result<(), BoxError> {
    let fixture = Fixture::new("()", &["nightly", "weekly"], "registered").await?;
    let _running = RunClaim::acquire(&fixture.slot("nightly")?)?;

    let busy = fixture.live_run("nightly").await;
    let other = fixture.live_run("weekly").await;

    assert!(matches!(busy, Err(CliRunError::Busy)), "{busy:?}");
    assert_eq!(other?, RunOutcome::Succeeded);
    Ok(())
}

#[tokio::test]
async fn a_claim_a_dead_run_left_behind_does_not_stop_the_next_run() -> Result<(), BoxError> {
    let fixture = Fixture::new("()", &["nightly"], "registered").await?;
    let dead = RunClaim::acquire(&fixture.slot("nightly")?)?;
    dead.publish(&ActiveRun {
        run_id: 1,
        started: datetime!(2026-10-01 03:00:00 UTC),
    })?;
    drop(dead);

    assert_eq!(fixture.live_run("nightly").await?, RunOutcome::Succeeded);
    Ok(())
}

#[tokio::test]
async fn a_dry_run_takes_no_claim_so_it_runs_while_the_job_is_running() -> Result<(), BoxError> {
    let fixture = Fixture::new(
        "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"missing.md\" });",
        &["nightly"],
        "registered",
    )
    .await?;
    write(
        fixture.vault.path(),
        "registry/workflows/ops/sync/tests/happy.toml",
        "[[call]]\nserver = \"contextos\"\ntool = \"fs_read_text_file\"\nkind = \"valid\"\nresponse = { content = \"mocked\" }\n",
    )?;
    let running = RunClaim::acquire(&fixture.slot("nightly")?)?;

    let outcome = support::run_job(
        Arc::clone(&fixture.clients),
        &fixture.host(),
        VAULT_NAME,
        "ops",
        "sync",
        "nightly",
        CliRunOptions {
            dry_run_scenario: Some(Some("happy".to_owned())),
            record: true,
        },
    )
    .await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    assert_eq!(
        fixture.slot("nightly")?.state()?,
        ClaimState::Held(None),
        "the dry run neither took nor released the live run's claim"
    );
    drop(running);
    Ok(())
}

#[tokio::test]
async fn a_dry_run_is_recorded_as_one_when_it_ends() -> Result<(), BoxError> {
    let fixture = Fixture::new(
        "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"missing.md\" });",
        &["nightly"],
        "registered",
    )
    .await?;
    write(
        fixture.vault.path(),
        "registry/workflows/ops/sync/tests/happy.toml",
        "[[call]]\nserver = \"contextos\"\ntool = \"fs_read_text_file\"\nkind = \"valid\"\nresponse = { content = \"mocked\" }\n",
    )?;

    support::run_job(
        Arc::clone(&fixture.clients),
        &fixture.host(),
        VAULT_NAME,
        "ops",
        "sync",
        "nightly",
        CliRunOptions {
            dry_run_scenario: Some(Some("happy".to_owned())),
            record: true,
        },
    )
    .await?;

    let records = fixture.records("nightly");
    assert_eq!(records.len(), 1, "{records:?}");
    assert!(records[0].1.contains("dry_run: true"), "{}", records[0].1);
    assert_eq!(status_of(&records[0].1), "succeeded");
    Ok(())
}

#[tokio::test]
async fn a_live_run_asked_to_stop_beside_its_claim_ends_cancelled_and_releases_the_job() -> Result<(), BoxError> {
    let fixture = Fixture::new("loop { }", &["nightly"], "registered").await?;
    let slot = fixture.slot("nightly")?;
    let run = tokio::spawn({
        let clients = Arc::clone(&fixture.clients);
        let host = fixture.host();
        async move {
            support::run_job(
                clients,
                &host,
                VAULT_NAME,
                "ops",
                "sync",
                "nightly",
                CliRunOptions {
                    dry_run_scenario: None,
                    record: true,
                },
            )
            .await
        }
    });
    let mut active = None;
    eventually("the run to say it is active", || {
        if let Ok(ClaimState::Held(Some(run))) = slot.state() {
            active = Some(run);
            true
        } else {
            false
        }
    })
    .await?;
    let active = active.ok_or("the run never said it was active")?;

    slot.request_cancel(active.run_id)?;

    let outcome = tokio::time::timeout(Duration::from_secs(30), run).await???;
    assert_eq!(outcome, RunOutcome::Cancelled);
    assert_eq!(slot.state()?, ClaimState::Idle, "the claim is released");
    let records = fixture.records("nightly");
    assert_eq!(status_of(&records[0].1), "cancelled", "{}", records[0].1);
    Ok(())
}

#[tokio::test]
async fn a_request_to_stop_a_run_that_has_ended_is_refused() -> Result<(), BoxError> {
    let fixture = Fixture::new("()", &["nightly"], "registered").await?;
    fixture.live_run("nightly").await?;

    let refused = fixture.slot("nightly")?.request_cancel(1);

    assert!(refused.is_err());
    Ok(())
}
