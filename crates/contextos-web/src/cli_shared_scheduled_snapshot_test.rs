//! A due tick reads the store once: the scheduler reads the job's code as a snapshot before it claims the tick, and the
//! run it starts executes that same snapshot instead of reading its own. Shares its fixture with `cli_shared_test.rs`
//! and uses the snapshot's test seam to change the store right after the scheduler's read.

use std::sync::atomic::{AtomicUsize, Ordering};

use time::macros::datetime;

use super::*;
use crate::run_settings::HostContext;
use crate::shared_scheduler::{SharedSchedulerHost, start_due_shared_jobs};
use crate::snapshot_hooks::{AFTER_READ, set_hook};

const IMPORTING_SCRIPT: &str = "import \"helper\" as helper;\nhelper::step();";
const HELPER_WITHOUT_CALL: &str = "fn step() { }";
const HELPER_WITH_CALL: &str = "fn step() { call_mcp(\"contextos\", \"vault_info\", #{}); }";
const READY_JOB: &str = "name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\nready = true\n";

/// An enabled and proven job whose package ships one module, `helper.rhai`, that makes no call.
fn host_with_helper() -> Result<Host, Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, IMPORTING_SCRIPT)?;
    let dir = host.store.path().join("acme").join("1.0.0");
    std::fs::write(
        dir.join("manifest.toml"),
        format!(
            "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"sha256:{}\"\nmodules = [\"helper.rhai\"]\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n{EFFECT}",
            "a".repeat(64)
        ),
    )?;
    std::fs::write(dir.join("helper.rhai"), HELPER_WITHOUT_CALL)?;
    crate::release_fixture::seal(&dir)?;
    host.register_jobs = false;
    host.add_job(READY_JOB)?;
    host.bring_to(Stage::EnabledProven)?;
    Ok(host)
}

/// After every snapshot read completes, replaces the release with one whose helper module makes a call, so a read after
/// that sees the call in it, and counts the reads. The replacement is a whole release, as a real upgrade in place would be.
fn change_helper_after_each_read(host: &Host) -> Arc<AtomicUsize> {
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&reads);
    let helper = host.store.path().join("acme/1.0.0/helper.rhai");
    set_hook(
        &AFTER_READ,
        host.store.path(),
        Box::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = std::fs::write(&helper, HELPER_WITH_CALL);
            if let Some(release) = helper.parent() {
                let _ = crate::release_fixture::seal(release);
            }
        }),
    );
    reads
}

/// The first record of the job's runs, wherever the run filed it: the folder holds the year and month the run happened
/// in, which is the real date and so is not fixed.
fn first_record(host: &Host) -> Option<std::path::PathBuf> {
    let mut pending = vec![host.runs.path().join("acme/sync/nightly")];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|name| name == "R-1.md") {
                return Some(path);
            }
        }
    }
    None
}

/// Waits, by yielding, for the run the pass spawned to leave a finished record, and returns it.
async fn finished_record(host: &Host) -> Result<String, Box<dyn std::error::Error>> {
    for _ in 0..200_000 {
        if let Some(record) = first_record(host)
            && let Ok(text) = std::fs::read_to_string(&record)
            && (text.contains("status: succeeded") || text.contains("status: failed"))
        {
            return Ok(text);
        }
        tokio::task::yield_now().await;
    }
    Err("the scheduled run never finished".into())
}

async fn due_pass(host: &Host) -> Result<crate::shared_scheduler::SharedPollReport, Box<dyn std::error::Error>> {
    let context = HostContext {
        attention: crate::job_attention::JobAttentionMemory::default(),
        log_level: None,
        claims_root: Some(host.claims.path().to_path_buf()),
        clock: Arc::new(crate::vault_time::VaultTime::from(crate::vault_time::VaultTimeInput {
            clock: Arc::new(contextos_workflows::SystemClock),
            clients: Arc::new(McpClientSet::default()),
        })),
    };
    let shared = SharedSchedulerHost {
        store_root: host.store.path().to_path_buf(),
        jobs_root: host.jobs.path().to_path_buf(),
        claims_root: host.claims.path().to_path_buf(),
        runs_root: host.runs.path().to_path_buf(),
        state: host.state.clone(),
        run_retention: time::Duration::days(30),
        owner: None,
        log_level: None,
    };
    let clients = Arc::new(McpClientSet::default());
    let due = FixedClock(datetime!(2026-10-02 03:00:30 UTC));
    Ok(start_due_shared_jobs(&clients, &context, &shared, &due).await)
}

#[tokio::test]
async fn a_scheduled_run_executes_the_module_text_the_scheduler_read_even_when_the_release_changes_after_that_read()
-> Result<(), Box<dyn std::error::Error>> {
    let host = host_with_helper()?;
    change_helper_after_each_read(&host);

    let report = due_pass(&host).await?;
    let record = finished_record(&host).await?;

    assert_eq!(report.spawned, 1, "{report:?}");
    assert!(
        record.contains("status: succeeded"),
        "the run executed the module as it was read, which makes no call:\n{record}"
    );
    assert!(
        !record.contains("vault_info"),
        "the run executed text the scheduler did not read:\n{record}"
    );
    Ok(())
}

#[tokio::test]
async fn a_due_tick_reads_the_snapshot_once() -> Result<(), Box<dyn std::error::Error>> {
    let host = host_with_helper()?;
    let reads = change_helper_after_each_read(&host);

    let report = due_pass(&host).await?;
    finished_record(&host).await?;

    assert_eq!(report.spawned, 1, "{report:?}");
    assert_eq!(reads.load(Ordering::SeqCst), 1, "the run read the store a second time");
    Ok(())
}
