//! A vault workflow that names vault libraries, against a real `contextos-mcp` session and a real temporary vault:
//! validation, the version, the lifecycle, and a run are all made from the workflow's files and the libraries it reaches.

mod support;

use contextos_web::cli_run::CliRunOptions;
use contextos_web::job_validation::{JobLocation, JobValidation, validate_now, version_at};
use contextos_workflows::{RunOutcome, WorkflowFailureKind};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use support::jobs::{JobVault, STATE_PATH, Spec, VAULT_NAME, summary_of};
use time::OffsetDateTime;
use time::macros::datetime;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const FIRST: OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);
const LATER: OffsetDateTime = datetime!(2026-10-02 03:00:00 UTC);
const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const ENTRY: &str = "import \"crm:client\" as crm;\ncrm::info()\n";
const PACKAGE: &str = "workflows = [\"sync\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \"*\"\n";
const LIBRARY: &str = "modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\nmcp_tools = [{ server = \"contextos\", tool = \"vault_info\" }]\n";
const CLIENT: &str = "fn info() { call_mcp(\"contextos\", \"vault_info\", #{}) }\n";

fn spec() -> Spec<'static> {
    Spec {
        package: PACKAGE,
        workflow: WORKFLOW,
        script: ENTRY,
        tasks: false,
        ..Spec::default()
    }
}

/// A vault whose workflow names `crm`, with that library written.
async fn vault_with_library() -> Result<JobVault, BoxError> {
    let vault = JobVault::open(&spec()).await?;
    write_library(&vault, "crm", LIBRARY, &[("client.rhai", CLIENT)])?;
    vault.install_releases()?;
    Ok(vault)
}

fn write_library(vault: &JobVault, folder: &str, manifest: &str, modules: &[(&str, &str)]) -> std::io::Result<()> {
    vault.write(&format!("registry/libraries/{folder}/library.toml"), manifest)?;
    for (path, text) in modules {
        vault.write(&format!("registry/libraries/{folder}/{path}"), text)?;
    }
    vault
        .install_releases()
        .map_err(|error| std::io::Error::other(error.to_string()))
}

/// Installs what the vault holds as a new release of the workflow, `number`.0.0, which the job now names. A change to a
/// library or to the workflow reaches a job only through a release like this one.
fn release(vault: &JobVault, number: u32) -> Result<(), BoxError> {
    vault.publish_source_as(&format!("{number}.0.0"))
}

/// Removes the installed releases of the library `crm`, as an operator uninstalling it would.
fn uninstall_library(vault: &JobVault) -> std::io::Result<()> {
    std::fs::remove_dir_all(vault.root().join(".contextos/packages/crm"))
}

async fn validate(vault: &JobVault) -> Result<JobValidation, BoxError> {
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    Ok(validate_now(
        &client,
        VAULT_NAME,
        &["contextos".to_owned()],
        contextos_web::jobs::WorkflowFolder {
            package: "ops",
            directory: "sync",
        },
        "nightly.toml",
        FIRST,
    )
    .await?)
}

fn codes(validation: &JobValidation) -> Vec<(&'static str, String)> {
    validation
        .report
        .failures
        .iter()
        .map(|failure| (failure.code.as_str(), failure.artefact.clone()))
        .collect()
}

fn has(validation: &JobValidation, code: &str, artefact: &str) -> bool {
    codes(validation)
        .iter()
        .any(|(found_code, found_artefact)| *found_code == code && found_artefact == artefact)
}

#[tokio::test]
async fn a_workflow_importing_a_named_librarys_export_passes_validation() -> Result<(), BoxError> {
    let vault = vault_with_library().await?;

    let validation = validate(&vault).await?;

    assert!(validation.report.passed(), "{:?}", validation.report);
    assert!(validation.fingerprint.is_some());
    Ok(())
}

#[tokio::test]
async fn editing_a_library_module_or_its_manifest_is_a_new_version_once_released_with_the_workflow()
-> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    let base = validate(&vault).await?.fingerprint;

    write_library(
        &vault,
        "crm",
        LIBRARY,
        &[(
            "client.rhai",
            "fn info() { call_mcp(\"contextos\", \"vault_info\", #{ }) }\n",
        )],
    )?;
    release(&vault, 2)?;
    let edited_module = validate(&vault).await?.fingerprint;
    write_library(
        &vault,
        "crm",
        &LIBRARY.replace("[\"contextos\"]", "[\"contextos\", \"other\"]"),
        &[],
    )?;
    release(&vault, 3)?;
    let edited_manifest = validate(&vault).await?.fingerprint;

    assert!(base.is_some());
    assert!(edited_module.is_some());
    assert!(edited_manifest.is_some());
    assert_ne!(base, edited_module);
    assert_ne!(base, edited_manifest);
    assert_ne!(edited_module, edited_manifest);
    Ok(())
}

#[tokio::test]
async fn a_library_edited_under_a_release_whose_lock_recorded_it_fails_validation_as_changed() -> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    assert!(validate(&vault).await?.report.passed());

    write_library(
        &vault,
        "crm",
        LIBRARY,
        &[(
            "client.rhai",
            "fn info() { call_mcp(\"contextos\", \"vault_info\", #{ }) }\n",
        )],
    )?;
    vault.install_releases()?;
    let validation = validate(&vault).await?;

    assert!(
        has(&validation, "validation/artefact-invalid", "crm"),
        "{:?}",
        codes(&validation)
    );
    assert_eq!(validation.fingerprint, None);
    Ok(())
}

#[tokio::test]
async fn a_named_library_with_no_folder_fails_validation_naming_it_and_leaves_the_job_without_a_version()
-> Result<(), BoxError> {
    let vault = JobVault::open(&spec()).await?;

    let validation = validate(&vault).await?;

    assert!(
        has(&validation, "validation/artefact-missing", "libraries"),
        "{:?}",
        codes(&validation)
    );
    assert!(
        validation
            .report
            .failures
            .iter()
            .any(|failure| failure.message.contains("crm"))
    );
    assert_eq!(validation.fingerprint, None);
    Ok(())
}

#[tokio::test]
async fn libraries_that_name_each_other_fail_validation_as_a_cycle() -> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    write_library(
        &vault,
        "crm",
        &format!("{LIBRARY}[[libraries]]\nname = \"loop\"\nrequirement = \"*\"\n"),
        &[],
    )?;
    write_library(
        &vault,
        "loop",
        "modules = [\"a.rhai\"]\nexports = [\"a.rhai\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \"*\"\n",
        &[("a.rhai", "fn a() { 1 }\n")],
    )?;

    let validation = validate(&vault).await?;

    assert!(
        codes(&validation)
            .iter()
            .any(|(code, _)| *code == "validation/artefact-invalid"),
        "{:?}",
        codes(&validation)
    );
    Ok(())
}

#[tokio::test]
async fn a_library_whose_manifest_breaks_its_schema_is_reported_against_the_library() -> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    write_library(&vault, "crm", "modules = [\"client.rhai\"]\nexports = []\n", &[])?;

    let validation = validate(&vault).await?;

    assert!(
        has(&validation, "validation/artefact-invalid", "crm:library.toml"),
        "{:?}",
        codes(&validation)
    );
    assert_eq!(validation.fingerprint, None);
    Ok(())
}

#[tokio::test]
async fn a_problem_in_a_library_module_is_reported_against_the_library_and_file_with_its_line() -> Result<(), BoxError>
{
    let vault = vault_with_library().await?;
    write_library(
        &vault,
        "crm",
        LIBRARY,
        &[("client.rhai", "fn info() { 1 }\n\nlet ran = true;\n")],
    )?;

    let validation = validate(&vault).await?;

    let failure = validation
        .report
        .failures
        .iter()
        .find(|failure| failure.code.as_str() == "validation/script-syntax")
        .ok_or("no script failure")?;
    assert_eq!(failure.artefact, "crm:client.rhai");
    assert!(failure.message.contains("line 3"), "{}", failure.message);
    Ok(())
}

#[tokio::test]
async fn a_tool_the_imported_library_declares_and_the_workflow_does_not_permit_is_reported_but_an_unused_library_adds_nothing()
-> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    write_library(
        &vault,
        "crm",
        &LIBRARY.replace(
            "tool = \"vault_info\" }]",
            "tool = \"vault_info\" }, { server = \"billing\", tool = \"charge\" }]",
        ),
        &[],
    )?;

    let imported = validate(&vault).await?;

    let failure = imported
        .report
        .failures
        .iter()
        .find(|failure| failure.code.as_str() == "validation/artefact-invalid")
        .ok_or("no server failure")?;
    assert!(
        failure.message.contains("crm") && failure.message.contains("billing"),
        "{}",
        failure.message
    );

    vault.write("registry/workflows/ops/sync/sync.rhai", "()\n")?;
    release(&vault, 2)?;
    let unused = validate(&vault).await?;
    assert!(
        unused.report.passed(),
        "a library never imported from adds nothing: {:?}",
        unused.report
    );
    Ok(())
}

#[tokio::test]
async fn a_library_name_that_is_not_valid_fails_the_package_file_of_the_release() -> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    let release = vault.root().join(".contextos/packages/ops/1.0.0");
    vault.write(
        ".contextos/packages/ops/1.0.0/workflows.toml",
        &PACKAGE.replace("name = \"crm\"", "name = \"../crm\""),
    )?;
    support::release_fixture::seal_io(&release)?;

    let validation = validate(&vault).await?;

    assert!(
        has(&validation, "validation/artefact-invalid", "workflows.toml"),
        "{:?}",
        codes(&validation)
    );
    Ok(())
}

#[tokio::test]
async fn a_release_named_with_different_case_is_not_the_library_and_the_exact_name_is_unaffected_by_it()
-> Result<(), BoxError> {
    let vault = JobVault::open(&spec()).await?;
    write_library(&vault, "CRM", LIBRARY, &[("client.rhai", CLIENT)])?;

    let only_upper = validate(&vault).await?;
    assert!(
        has(&only_upper, "validation/artefact-missing", "libraries"),
        "{:?}",
        codes(&only_upper)
    );

    // Where the filesystem ignores case, `crm` is the folder just written and there can be no second release.
    if vault.root().join("registry/libraries/crm").exists() {
        return Ok(());
    }
    write_library(&vault, "crm", LIBRARY, &[("client.rhai", CLIENT)])?;
    let both = validate(&vault).await?;
    assert!(
        both.report.passed(),
        "the release named exactly is the library, whatever a release with a differently cased name holds: {:?}",
        codes(&both)
    );
    Ok(())
}

#[tokio::test]
async fn a_validated_job_whose_library_disappears_returns_to_draft_naming_the_library() -> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    vault.pass(FIRST).await;
    assert_eq!(vault.lifecycle()?, "validated");

    uninstall_library(&vault)?;
    vault.pass(LATER).await;

    assert_eq!(vault.lifecycle()?, "draft");
    let state = vault.state()?;
    let validation = state
        .job("ops", "sync", "nightly")
        .and_then(|job| job.validation())
        .ok_or("no validation stored")?;
    assert!(!validation.passed);
    assert_eq!(validation.fingerprint, None);
    assert!(
        validation
            .failures
            .iter()
            .any(|failure| failure.message.contains("crm")),
        "{:?}",
        validation.failures
    );
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_whose_library_disappears_is_suspended_for_failed_validation_naming_the_library()
-> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    vault.pass(FIRST).await;
    let version = vault
        .state()?
        .job("ops", "sync", "nightly")
        .and_then(|job| job.fingerprint())
        .cloned()
        .ok_or("no version stored")?;
    vault.write(
        STATE_PATH,
        &format!(
            "[ops.sync.nightly]\nlifecycle = \"enabled\"\nschedule = \"* * * * *\"\nfingerprint = \"{}\"\n\
             lifecycle_changed = \"2026-09-01T00:00:00Z\"\nlifecycle_actor = \"operator\"\n",
            String::from(version)
        ),
    )?;

    uninstall_library(&vault)?;
    vault.pass(LATER).await;

    assert_eq!(vault.lifecycle()?, "suspended");
    let state = vault.state()?;
    let suspension = state
        .job("ops", "sync", "nightly")
        .and_then(|job| job.suspension())
        .ok_or("no suspension stored")?;
    assert_eq!(suspension.code, "workflow/validation-failed");
    assert!(suspension.reason.contains("crm"), "{}", suspension.reason);
    Ok(())
}

/// A job `registered` for a run, with no version on record.
fn register(vault: &JobVault) -> std::io::Result<()> {
    vault.write(STATE_PATH, "[ops.sync.nightly]\nlifecycle = \"registered\"\n")
}

async fn run(vault: &JobVault) -> Result<RunOutcome, BoxError> {
    Ok(support::run_job(
        std::sync::Arc::clone(&vault.clients),
        &vault.host,
        VAULT_NAME,
        "ops",
        "sync",
        "nightly",
        CliRunOptions {
            dry_run_scenario: None,
            record: true,
        },
    )
    .await?)
}

#[tokio::test]
async fn a_run_imports_the_library_and_its_call_is_made() -> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    register(&vault)?;

    let outcome = run(&vault).await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    Ok(())
}

#[tokio::test]
async fn a_run_whose_library_is_gone_ends_before_its_script_starts_naming_the_library() -> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    register(&vault)?;
    uninstall_library(&vault)?;

    let outcome = run(&vault).await?;

    let RunOutcome::Failed(failure) = outcome else {
        return Err(format!("expected a failed run, got {outcome:?}").into());
    };
    assert_eq!(failure.kind, Some(WorkflowFailureKind::PackageUnavailable));
    assert!(failure.message.contains("crm"), "{}", failure.message);
    Ok(())
}

#[tokio::test]
async fn a_run_whose_locked_library_release_was_changed_or_left_incomplete_ends_before_its_script_starts()
-> Result<(), BoxError> {
    for damage in ["changed", "incomplete"] {
        let vault = vault_with_library().await?;
        register(&vault)?;
        let (library_version, _) = installed_release(&vault, "crm")?;
        let file = format!(".contextos/packages/crm/{library_version}/client.rhai");
        if damage == "changed" {
            vault.write(&file, "fn info() { 1 }\n")?;
        } else {
            std::fs::remove_file(vault.root().join(&file))?;
        }

        let outcome = run(&vault).await?;

        let RunOutcome::Failed(failure) = outcome else {
            return Err(format!("{damage}: expected a failed run, got {outcome:?}").into());
        };
        assert_eq!(failure.kind, Some(WorkflowFailureKind::PackageUnavailable), "{damage}");
        assert!(failure.message.contains("crm"), "{damage}: {}", failure.message);
    }
    Ok(())
}

#[tokio::test]
async fn a_run_by_hand_of_an_enabled_job_whose_code_changed_since_it_was_reviewed_starts_on_the_code_as_it_stands()
-> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    vault.pass(FIRST).await;
    let version = vault
        .state()?
        .job("ops", "sync", "nightly")
        .and_then(|job| job.fingerprint())
        .cloned()
        .ok_or("no version stored")?;
    vault.write(
        STATE_PATH,
        &format!(
            "[ops.sync.nightly]\nlifecycle = \"enabled\"\nschedule = \"0 3 * * *\"\nfingerprint = \"{}\"\n\
             lifecycle_changed = \"2026-09-01T00:00:00Z\"\nlifecycle_actor = \"operator\"\n",
            String::from(version)
        ),
    )?;
    write_library(
        &vault,
        "crm",
        LIBRARY,
        &[(
            "client.rhai",
            "fn info() { call_mcp(\"contextos\", \"vault_info\", #{ }) }\n",
        )],
    )?;

    let outcome = run(&vault).await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    let records = vault.run_records();
    let [(_, record)] = records.as_slice() else {
        return Err(format!("expected one run record, found {}", records.len()).into());
    };
    assert!(record.contains("script_started: true"), "{record}");
    Ok(())
}

/// Marks the job `enabled` with the version it has now on record, as a review leaves it, and returns that version.
async fn enable_at_reviewed_version(vault: &JobVault) -> Result<String, BoxError> {
    vault.pass(FIRST).await;
    let version = vault
        .state()?
        .job("ops", "sync", "nightly")
        .and_then(|job| job.fingerprint())
        .cloned()
        .ok_or("no version stored")?;
    let version = String::from(version);
    vault.write(
        STATE_PATH,
        &format!(
            "[ops.sync.nightly]\nlifecycle = \"enabled\"\nschedule = \"0 3 * * *\"\nfingerprint = \"{version}\"\n\
             lifecycle_changed = \"2026-09-01T00:00:00Z\"\nlifecycle_actor = \"operator\"\n"
        ),
    )?;
    Ok(version)
}

#[tokio::test]
async fn an_enabled_job_whose_code_and_library_are_unchanged_since_the_review_runs_and_succeeds() -> Result<(), BoxError>
{
    let vault = vault_with_library().await?;
    enable_at_reviewed_version(&vault).await?;

    let outcome = run(&vault).await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_with_no_version_on_record_is_not_blocked_by_the_review_gate() -> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    vault.write(
        STATE_PATH,
        "[ops.sync.nightly]\nlifecycle = \"enabled\"\nschedule = \"0 3 * * *\"\n\
         lifecycle_changed = \"2026-09-01T00:00:00Z\"\nlifecycle_actor = \"operator\"\n",
    )?;

    let outcome = run(&vault).await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    Ok(())
}

#[tokio::test]
async fn a_live_run_reports_the_version_of_the_code_it_read_so_a_trial_is_settled_against_that_version()
-> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    register(&vault)?;
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let location = JobLocation {
        package: "ops".to_owned(),
        directory: "sync".to_owned(),
        file: "nightly.toml".to_owned(),
    };
    let expected = version_at(&client, VAULT_NAME, &location).await?;

    let report = contextos_web::cli_run::run_job_reported(
        std::sync::Arc::clone(&vault.clients),
        &vault.host,
        VAULT_NAME,
        &contextos_workflows::JobKey {
            package: "ops".to_owned(),
            workflow: "sync".to_owned(),
            job: "nightly".to_owned(),
        },
        CliRunOptions {
            dry_run_scenario: None,
            record: true,
        },
        contextos_web::run_report::RunControls::default(),
    )
    .await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded);
    assert!(expected.is_some(), "the job has no version to compare with");
    assert_eq!(report.version, expected);
    Ok(())
}

#[tokio::test]
async fn a_dry_run_reports_the_version_of_the_code_it_read_so_it_registers_the_job_at_that_version()
-> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    register(&vault)?;
    vault.write(
        "registry/workflows/ops/sync/tests/happy-path.toml",
        "[[call]]\nserver = \"contextos\"\ntool = \"vault_info\"\nkind = \"valid\"\nresponse = { name = \"mock\" }\n",
    )?;
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let location = JobLocation {
        package: "ops".to_owned(),
        directory: "sync".to_owned(),
        file: "nightly.toml".to_owned(),
    };
    let expected = version_at(&client, VAULT_NAME, &location).await?;

    let report = contextos_web::cli_run::run_job_reported(
        std::sync::Arc::clone(&vault.clients),
        &vault.host,
        VAULT_NAME,
        &contextos_workflows::JobKey {
            package: "ops".to_owned(),
            workflow: "sync".to_owned(),
            job: "nightly".to_owned(),
        },
        CliRunOptions {
            dry_run_scenario: Some(Some("happy-path".to_owned())),
            record: false,
        },
        contextos_web::run_report::RunControls::default(),
    )
    .await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded);
    assert!(expected.is_some(), "the job has no version to compare with");
    assert_eq!(report.version, expected);
    Ok(())
}

fn hash_of(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// The version and digest of the one installed release of `package`, as its manifest states them.
fn installed_release(vault: &JobVault, package: &str) -> Result<(String, String), BoxError> {
    let folder = vault.root().join(".contextos/packages").join(package);
    let mut versions: Vec<std::path::PathBuf> = std::fs::read_dir(&folder)?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    versions.sort();
    let [release] = versions.as_slice() else {
        return Err(format!("expected one release of {package}, found {versions:?}").into());
    };
    let manifest: toml::Table = toml::from_str(&std::fs::read_to_string(release.join("manifest.toml"))?)?;
    let text = |key: &str| {
        manifest
            .get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("the manifest of {package} has no {key}"))
    };
    Ok((text("version")?, text("digest")?))
}

#[tokio::test]
async fn a_completed_run_records_its_release_and_each_locked_library_release_by_version_and_digest_and_logs_each_library_without_content()
-> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    register(&vault)?;
    let (writer, _guard) = support::logs::capture();

    let outcome = run(&vault).await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    let records = vault.run_records();
    let [(_, record)] = records.as_slice() else {
        return Err(format!("expected one run record, found {}", records.len()).into());
    };
    let (version, digest) = installed_release(&vault, "ops")?;
    let (library_version, library_digest) = installed_release(&vault, "crm")?;
    // The run's own release is a top-level key; each library release is a nested entry under `libraries:`. The two are
    // asserted in their own place, since a release and a library can share a version number.
    let (own, nested) = record
        .split_once("\nlibraries:")
        .ok_or_else(|| format!("the record lists no libraries: {record}"))?;
    let own_lines: Vec<&str> = own.lines().collect();
    assert!(own_lines.contains(&"package: ops"), "{record}");
    assert!(own_lines.contains(&format!("version: {version}").as_str()), "{record}");
    assert!(own_lines.contains(&format!("digest: {digest}").as_str()), "{record}");
    let nested_lines: Vec<&str> = nested.lines().map(str::trim).collect();
    assert!(
        nested_lines.contains(&"- name: crm") || nested_lines.contains(&"name: crm"),
        "{record}"
    );
    assert!(
        nested_lines.contains(&format!("version: {library_version}").as_str()),
        "{record}"
    );
    assert!(
        nested_lines.contains(&format!("digest: {library_digest}").as_str()),
        "{record}"
    );
    assert!(!record.contains("client.rhai"), "per-file entries remain: {record}");
    assert!(!record.contains(&hash_of(CLIENT)), "per-file hashes remain: {record}");
    let logged = writer.at("INFO", "library");
    assert!(logged.iter().any(|line| line.contains("crm")), "{logged:?}");
    assert!(
        logged.iter().all(|line| !line.contains("vault_info")),
        "library content was logged: {logged:?}"
    );
    Ok(())
}

fn one_off(code: contextos_web::cli_run::OneOffCode) -> contextos_web::cli_run::OneOffVaultRun {
    contextos_web::cli_run::OneOffVaultRun {
        code,
        vault: VAULT_NAME.to_owned(),
        package: "ops".to_owned(),
        workflow_slug: "sync".to_owned(),
        arguments: Vec::new(),
        dry_run_scenario: None,
        log_level: None,
    }
}

#[tokio::test]
async fn a_one_off_run_of_a_release_resolves_the_installed_libraries_and_the_summary_names_the_release_and_them()
-> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    let (version, digest) = installed_release(&vault, "ops")?;
    let (library_version, library_digest) = installed_release(&vault, "crm")?;

    let report = contextos_web::cli_run::run_workflow_once_reported(
        std::sync::Arc::clone(&vault.clients),
        "contextos",
        one_off(contextos_web::cli_run::OneOffCode::Release(
            contextos_workflows::ReleaseChoice::Version(semver::Version::parse(&version)?),
        )),
        tokio_util::sync::CancellationToken::new(),
    )
    .await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded);
    let summary = summary_of(report);
    let document = serde_json::to_value(&summary)?;
    assert_eq!(document["scope"]["package_version"], version);
    assert_eq!(document["scope"]["digest"], digest);
    assert_eq!(
        document["libraries"],
        serde_json::json!([{ "name": "crm", "origin": "release", "version": library_version, "digest": library_digest }])
    );
    assert!(
        vault
            .state()?
            .library_lock("ops", "sync", &semver::Version::parse(&version)?)
            .is_none(),
        "a one-off run fixes no lock"
    );
    Ok(())
}

async fn run_source(vault: &JobVault) -> Result<contextos_web::run_report::RunReport, BoxError> {
    Ok(contextos_web::cli_run::run_workflow_once_reported(
        std::sync::Arc::clone(&vault.clients),
        "contextos",
        one_off(contextos_web::cli_run::OneOffCode::Source),
        tokio_util::sync::CancellationToken::new(),
    )
    .await?)
}

fn refused_as_unavailable(report: &contextos_web::run_report::RunReport) -> Option<String> {
    match &report.outcome {
        RunOutcome::Failed(failure)
            if !report.started && failure.kind == Some(WorkflowFailureKind::PackageUnavailable) =>
        {
            Some(failure.message.clone())
        }
        _ => None,
    }
}

#[tokio::test]
async fn a_one_off_run_of_source_whose_stated_requirement_no_installed_release_satisfies_is_refused_before_it_starts()
-> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    vault.write(
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \"^2\"\n",
    )?;

    let report = run_source(&vault).await?;

    let message = refused_as_unavailable(&report).ok_or_else(|| format!("{:?}", report.outcome))?;
    assert!(message.contains("crm"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_of_source_is_refused_when_an_installed_library_requires_what_no_release_satisfies()
-> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    write_library(
        &vault,
        "util",
        "modules = [\"util.rhai\"]\nexports = [\"util.rhai\"]\n",
        &[("util.rhai", "fn help() { 1 }\n")],
    )?;
    write_library(
        &vault,
        "crm",
        &format!("{LIBRARY}[[libraries]]\nname = \"util\"\nrequirement = \"^3\"\n"),
        &[("client.rhai", CLIENT)],
    )?;

    let report = run_source(&vault).await?;

    let message = refused_as_unavailable(&report).ok_or_else(|| format!("{:?}", report.outcome))?;
    assert!(message.contains("util"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_of_a_release_whose_library_release_is_gone_ends_before_its_script_starts_as_unavailable()
-> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    let (version, _digest) = installed_release(&vault, "ops")?;
    uninstall_library(&vault)?;

    let report = contextos_web::cli_run::run_workflow_once_reported(
        std::sync::Arc::clone(&vault.clients),
        "contextos",
        one_off(contextos_web::cli_run::OneOffCode::Release(
            contextos_workflows::ReleaseChoice::Version(semver::Version::parse(&version)?),
        )),
        tokio_util::sync::CancellationToken::new(),
    )
    .await?;

    let message = refused_as_unavailable(&report).ok_or_else(|| format!("{:?}", report.outcome))?;
    assert!(message.starts_with("workflow/package-unavailable"), "{message}");
    assert!(message.contains("crm"), "{message}");
    Ok(())
}
