//! A run of a saved shared job that resolves library packages, against a real package store, real job files, the real
//! host state file, run records on disk, and a real `contextos-mcp` session: the libraries are in the run record and the
//! log, a run whose libraries do not resolve ends before its script starts, and a scheduled run of a version nobody
//! reviewed does not start.

mod support;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use contextos_web::cli_shared::{
    CliSharedError, OneOffRequest, SharedHost, SharedRunOptions, run_shared_job_reported, run_shared_once_reported,
};
use contextos_workflows::{McpCallFailure, McpCaller, RunOutcome, RunTrigger, SystemClock, WorkflowFailureKind};
use serde_json::{Map, Value};
use support::BoxError;
use support::shared::{DIGEST, SharedFixture as Fixture, key};

/// The digest the installed release `name` `version` declares in its manifest, which sealing made the real one.
fn declared_digest(fixture: &Fixture, name: &str, version: &str) -> Result<String, BoxError> {
    let text = std::fs::read_to_string(fixture.store_root().join(name).join(version).join("manifest.toml"))?;
    let table: toml::Table = toml::from_str(&text)?;
    table
        .get("digest")
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "the manifest declares no digest".into())
}
use tokio_util::sync::CancellationToken;

const READY: &str = "name = \"Nightly\"\nready = true\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
const SCRIPT: &str = "import \"acme-text:strings\" as s;\nlog_info(\"shout \" + s::shout());";

/// Answers every call at once; a test that must see no call counts them.
#[derive(Default)]
struct Silent(std::sync::atomic::AtomicUsize);

impl McpCaller for Silent {
    fn call<'a>(
        &'a self,
        _server: &'a str,
        _tool: &'a str,
        _arguments: Map<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpCallFailure>> + Send + 'a>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok(Value::Null) })
    }
}

fn host<'a>(fixture: &'a Fixture, state: &'a contextos_web::shared_state::SharedStateStore) -> SharedHost<'a> {
    // The paths are borrowed for the length of the host, so they are leaked into the test's scope by being owned by it.
    SharedHost {
        clients: Arc::clone(&fixture.clients),
        primary_server: "contextos",
        store_root: Box::leak(fixture.store_root().into_boxed_path()),
        jobs_root: Box::leak(fixture.jobs_root().into_boxed_path()),
        claims_root: Box::leak(fixture.claims_root().into_boxed_path()),
        runs_root: Box::leak(fixture.runs_root().into_boxed_path()),
        state,
        run_retention: time::Duration::days(30),
        clock: Arc::new(SystemClock),
        caller: Arc::new(Silent::default()),
        owner: None,
        log_level: None,
        cancel: CancellationToken::new(),
    }
}

/// `acme` imports from `acme-text`, which needs `acme-core`: two libraries, installed.
async fn installed() -> Result<Fixture, BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.install_library("acme-core", "1.0.0", "core", "fn mark() { \"!\" }")?;
    let text = fixture.store_root().join("acme-text").join("1.2.0");
    support::shared::write(
        &text.join("manifest.toml"),
        &format!(
            "name = \"acme-text\"\nversion = \"1.2.0\"\ndigest = \"{DIGEST}\"\nmodules = [\"strings.rhai\"]\nexports = [\"strings.rhai\"]\n\n[[dependencies]]\nname = \"acme-core\"\nrequirement = \"^1\"\n"
        ),
    )?;
    support::shared::write(
        &text.join("strings.rhai"),
        "import \"acme-core:core\" as core;\nfn shout() { \"hi\" + core::mark() }",
    )?;
    support::release_fixture::seal_io(&text)?;
    fixture.install_depending_on("1.0.0", SCRIPT, &[("acme-text", "^1")])?;
    Ok(fixture)
}

/// As [`installed`], with the job registered, so it may be run by hand.
async fn with_libraries() -> Result<Fixture, BoxError> {
    let fixture = installed().await?;
    fixture.register().await?;
    Ok(fixture)
}

fn the_record(fixture: &Fixture) -> Result<String, BoxError> {
    let dir = fixture.runs_root().join("acme/sync/nightly");
    let mut found = Vec::new();
    let mut pending = vec![dir];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("R-"))
            {
                found.push(std::fs::read_to_string(path)?);
            }
        }
    }
    match found.as_slice() {
        [only] => Ok(only.clone()),
        other => Err(format!("expected one record, found {}", other.len()).into()),
    }
}

#[tokio::test]
async fn a_run_records_its_libraries_in_name_order_and_logs_each_without_host_paths() -> Result<(), BoxError> {
    let fixture = with_libraries().await?;
    let state = fixture.state();
    let (writer, _guard) = support::logs::capture();

    let report =
        run_shared_job_reported(host(&fixture, &state), key(), SharedRunOptions::default(), Vec::new()).await?;

    assert!(matches!(report.outcome, RunOutcome::Succeeded), "{:?}", report.outcome);
    let record = the_record(&fixture)?;
    let core_digest = declared_digest(&fixture, "acme-core", "1.0.0")?;
    let text_digest = declared_digest(&fixture, "acme-text", "1.2.0")?;
    let expected = format!(
        "libraries: [{{\"name\":\"acme-core\",\"version\":\"1.0.0\",\"digest\":\"{core_digest}\"}},{{\"name\":\"acme-text\",\"version\":\"1.2.0\",\"digest\":\"{text_digest}\"}}]\n"
    );
    assert!(record.contains(&expected), "{record}");
    let logged = writer.at("INFO", "library");
    for (name, digest) in [("acme-core", &core_digest), ("acme-text", &text_digest)] {
        assert!(
            logged
                .iter()
                .any(|line| line.contains(name) && line.contains(digest.as_str())),
            "{name}: {logged:?}"
        );
    }
    let root = fixture.root.path().display().to_string();
    assert!(logged.iter().all(|line| !line.contains(&root)), "{logged:?}");
    Ok(())
}

#[tokio::test]
async fn a_run_whose_library_is_gone_ends_before_its_script_starts_naming_the_library() -> Result<(), BoxError> {
    let fixture = with_libraries().await?;
    std::fs::remove_dir_all(fixture.store_root().join("acme-text"))?;
    let state = fixture.state();
    let silent = Arc::new(Silent::default());
    let mut at = host(&fixture, &state);
    at.caller = Arc::clone(&silent) as Arc<dyn McpCaller>;

    let report = run_shared_job_reported(at, key(), SharedRunOptions::default(), Vec::new()).await?;

    let RunOutcome::Failed(failure) = &report.outcome else {
        return Err(format!("expected a failed run, got {:?}", report.outcome).into());
    };
    assert_eq!(failure.kind, Some(WorkflowFailureKind::PackageUnavailable));
    assert!(
        failure.message.starts_with("workflow/package-unavailable"),
        "{}",
        failure.message
    );
    assert!(failure.message.contains("acme-text"), "{}", failure.message);
    assert!(!report.started);
    assert_eq!(silent.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(report.record.is_none(), "a failure before the claim leaves no record");
    Ok(())
}

/// Runs the job with `package`'s release `version` manifest replaced by `text`, and returns the failure the run ended with.
async fn failed_after_damaging(package: &str, version: &str, text: &str) -> Result<(String, bool, bool), BoxError> {
    let fixture = with_libraries().await?;
    support::shared::write(
        &fixture.store_root().join(package).join(version).join("manifest.toml"),
        text,
    )?;
    let state = fixture.state();
    let silent = Arc::new(Silent::default());
    let mut at = host(&fixture, &state);
    at.caller = Arc::clone(&silent) as Arc<dyn McpCaller>;

    let report = run_shared_job_reported(at, key(), SharedRunOptions::default(), Vec::new()).await?;

    let RunOutcome::Failed(failure) = &report.outcome else {
        return Err(format!("expected a failed run, got {:?}", report.outcome).into());
    };
    assert_eq!(failure.kind, Some(WorkflowFailureKind::PackageUnavailable));
    Ok((failure.message.clone(), report.started, report.record.is_some()))
}

#[tokio::test]
async fn a_run_whose_library_manifest_is_damaged_ends_before_its_script_starts_naming_the_library()
-> Result<(), BoxError> {
    let (message, started, recorded) = failed_after_damaging("acme-text", "1.2.0", "name = \"acme-text\n[[").await?;

    assert!(message.starts_with("workflow/package-unavailable"), "{message}");
    assert!(message.contains("acme-text"), "{message}");
    assert!(!started);
    assert!(!recorded, "a failure before the claim leaves no record");
    Ok(())
}

#[tokio::test]
async fn a_run_whose_own_package_manifest_is_damaged_ends_before_its_script_starts() -> Result<(), BoxError> {
    let (message, started, recorded) = failed_after_damaging("acme", "1.0.0", "name = \"acme\n[[").await?;

    assert!(message.starts_with("workflow/package-unavailable"), "{message}");
    assert!(!started);
    assert!(!recorded, "a failure before the claim leaves no record");
    Ok(())
}

#[tokio::test]
async fn a_scheduled_run_of_a_release_nobody_reviewed_does_not_start() -> Result<(), BoxError> {
    let fixture = installed().await?;
    fixture.enable().await?;
    // The job file now names a newer release of the package, a version that has not been reviewed.
    fixture.install_depending_on("1.1.0", SCRIPT, &[("acme-text", "^1")])?;
    fixture.name_release("1.1.0")?;
    let state = fixture.state();

    let held = run_shared_job_reported(
        host(&fixture, &state),
        key(),
        SharedRunOptions {
            trigger: RunTrigger::Scheduled,
            dry_run_scenario: None,
        },
        Vec::new(),
    )
    .await;

    assert!(matches!(held, Err(CliSharedError::NotReviewed)), "{held:?}");
    Ok(())
}

#[tokio::test]
async fn a_newer_library_release_is_not_a_new_version_for_a_scheduled_run_of_a_locked_job() -> Result<(), BoxError> {
    let fixture = installed().await?;
    fixture.enable().await?;
    fixture.install_library("acme-core", "1.1.0", "core", "fn mark() { \"?\" }")?;
    let state = fixture.state();

    let report = run_shared_job_reported(
        host(&fixture, &state),
        key(),
        SharedRunOptions {
            trigger: RunTrigger::Scheduled,
            dry_run_scenario: None,
        },
        Vec::new(),
    )
    .await?;

    assert!(matches!(report.outcome, RunOutcome::Succeeded), "{:?}", report.outcome);
    let record = the_record(&fixture)?;
    let core_digest = declared_digest(&fixture, "acme-core", "1.0.0")?;
    assert!(
        record.contains(&format!(
            "\"name\":\"acme-core\",\"version\":\"1.0.0\",\"digest\":\"{core_digest}\""
        )),
        "{record}"
    );
    Ok(())
}

#[tokio::test]
async fn a_run_reads_the_recorded_lock_and_resolves_nothing_even_when_a_newer_library_release_cannot_resolve()
-> Result<(), BoxError> {
    let fixture = with_libraries().await?;
    // The newest acme-text no longer resolves, because it needs a library that is not installed. The lock names 1.2.0.
    let newer = fixture.store_root().join("acme-text").join("1.3.0");
    support::shared::write(
        &newer.join("manifest.toml"),
        &format!(
            "name = \"acme-text\"\nversion = \"1.3.0\"\ndigest = \"{DIGEST}\"\nmodules = [\"strings.rhai\"]\nexports = [\"strings.rhai\"]\n\n[[dependencies]]\nname = \"acme-gone\"\nrequirement = \"^1\"\n"
        ),
    )?;
    support::shared::write(&newer.join("strings.rhai"), "fn shout() { 3 }")?;
    support::release_fixture::seal_io(&newer)?;
    let state = fixture.state();

    let report =
        run_shared_job_reported(host(&fixture, &state), key(), SharedRunOptions::default(), Vec::new()).await?;

    assert!(matches!(report.outcome, RunOutcome::Succeeded), "{:?}", report.outcome);
    let record = the_record(&fixture)?;
    let text_digest = declared_digest(&fixture, "acme-text", "1.2.0")?;
    assert!(
        record.contains(&format!(
            "\"name\":\"acme-text\",\"version\":\"1.2.0\",\"digest\":\"{text_digest}\""
        )),
        "{record}"
    );
    Ok(())
}

#[tokio::test]
async fn a_run_whose_locked_library_release_was_removed_is_refused_and_no_newer_release_stands_in()
-> Result<(), BoxError> {
    let fixture = with_libraries().await?;
    fixture.install_library("acme-text", "1.3.0", "strings", "fn shout() { 3 }")?;
    std::fs::remove_dir_all(fixture.store_root().join("acme-text").join("1.2.0"))?;
    let state = fixture.state();

    let report =
        run_shared_job_reported(host(&fixture, &state), key(), SharedRunOptions::default(), Vec::new()).await?;

    let RunOutcome::Failed(failure) = &report.outcome else {
        return Err(format!("expected a failed run, got {:?}", report.outcome).into());
    };
    assert_eq!(failure.kind, Some(WorkflowFailureKind::PackageUnavailable));
    assert!(failure.message.contains("acme-text"), "{}", failure.message);
    assert!(!report.started);
    Ok(())
}

/// The summary of a one-off run of `acme sync`, as the command prints it.
fn summary_of(report: contextos_web::run_report::RunReport) -> contextos_web::run_summary::RunSummary {
    contextos_web::run_summary::RunSummary::finished(
        contextos_web::run_summary::RunInvocation {
            command: contextos_web::run_summary::RunCommand::WorkflowsRun,
            scope: contextos_web::run_summary::RunScope::Shared {
                package: "acme".to_owned(),
                workflow: "sync".to_owned(),
                job: None,
                release: None,
            },
            dry_run: false,
            scenario: None,
            overridden_inputs: Vec::new(),
            timings: contextos_web::run_summary::Timings::default(),
        },
        report,
    )
}

fn once(release: contextos_workflows::ReleaseChoice) -> OneOffRequest {
    OneOffRequest {
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        release,
        arguments: Vec::new(),
        dry_run_scenario: None,
    }
}

fn libraries_of(summary: &contextos_web::run_summary::RunSummary) -> Vec<(String, String)> {
    summary
        .libraries
        .iter()
        .filter_map(|library| {
            library
                .release
                .as_ref()
                .map(|release| (library.name.clone(), release.version.clone()))
        })
        .collect()
}

#[tokio::test]
async fn a_one_off_run_of_an_unlocked_release_resolves_the_installed_libraries_and_the_summary_lists_them()
-> Result<(), BoxError> {
    let fixture = installed().await?;
    let state = fixture.state();

    let report = run_shared_once_reported(
        host(&fixture, &state),
        once(contextos_workflows::ReleaseChoice::Version(semver::Version::new(
            1, 0, 0,
        ))),
    )
    .await?;

    assert!(matches!(report.outcome, RunOutcome::Succeeded), "{:?}", report.outcome);
    assert_eq!(
        libraries_of(&summary_of(report)),
        [
            ("acme-core".to_owned(), "1.0.0".to_owned()),
            ("acme-text".to_owned(), "1.2.0".to_owned())
        ]
    );
    assert!(
        state
            .read()?
            .library_lock("acme", "sync", &semver::Version::new(1, 0, 0))
            .is_none(),
        "a one-off run fixes no lock"
    );
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_of_a_locked_release_uses_the_lock_even_when_a_newer_library_release_is_installed()
-> Result<(), BoxError> {
    let fixture = with_libraries().await?;
    fixture.install_library("acme-text", "1.3.0", "strings", "fn shout() { 3 }")?;
    let state = fixture.state();

    let report = run_shared_once_reported(
        host(&fixture, &state),
        once(contextos_workflows::ReleaseChoice::Version(semver::Version::new(
            1, 0, 0,
        ))),
    )
    .await?;

    assert!(matches!(report.outcome, RunOutcome::Succeeded), "{:?}", report.outcome);
    assert_eq!(
        libraries_of(&summary_of(report)),
        [
            ("acme-core".to_owned(), "1.0.0".to_owned()),
            ("acme-text".to_owned(), "1.2.0".to_owned())
        ]
    );
    Ok(())
}
