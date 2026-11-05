use super::*;

use std::path::Path;

use contextos_workflows::ReleaseKind;

use crate::host_store::HostStore;
use crate::release_fixture::write_whole_release;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SYNC: &str = "[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n";
const SYNC_AND_REPORT: &str =
    "[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n[[workflow]]\nslug = \"report\"\nentry = \"report.rhai\"\n";

fn put(root: &Path, package: &str, version: &str, body: &str, files: &[(&str, &[u8])]) -> TestResult {
    write_whole_release(&root.join(package).join(version), (package, version), body, files)
}

async fn report_of(
    root: &Path,
    state: &JobsState,
    workflow: &str,
) -> Result<WorkflowReleases, Box<dyn std::error::Error>> {
    let package = PackageName::try_from("ops")?;
    let store = HostStore::new(root);
    Ok(ReleasesQuery {
        store_name: "shared",
        package: &package,
        workflow,
        state,
        jobs: &[],
        templates: TemplateHome::Release,
    }
    .report(store.engine())
    .await?)
}

#[tokio::test]
async fn the_releases_of_the_package_that_hold_the_workflow_are_reported_with_their_kind() -> TestResult {
    let root = tempfile::tempdir()?;
    put(root.path(), "ops", "1.0.0", SYNC, &[("sync.rhai", b"let x = 1;")])?;
    put(root.path(), "ops", "1.1.0", SYNC, &[("sync.rhai", b"let x = 2;")])?;
    let state: JobsState = toml::from_str("[ops.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.1.0\"\n")?;

    let report = report_of(root.path(), &state, "sync").await?;

    let seen: Vec<(String, ReleaseKind)> = report
        .releases
        .iter()
        .map(|line| (line.version.to_string(), line.kind))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("1.1.0".to_owned(), ReleaseKind::Future),
            ("1.0.0".to_owned(), ReleaseKind::Current)
        ]
    );
    assert!(
        report
            .releases
            .iter()
            .all(|line| line.condition == ReleaseCondition::Whole)
    );
    Ok(())
}

#[tokio::test]
async fn a_release_of_the_package_that_lacks_the_workflow_and_a_release_of_another_package_are_left_out() -> TestResult
{
    let root = tempfile::tempdir()?;
    put(
        root.path(),
        "ops",
        "1.0.0",
        SYNC_AND_REPORT,
        &[("sync.rhai", b"1;"), ("report.rhai", b"2;")],
    )?;
    put(root.path(), "ops", "2.0.0", SYNC, &[("sync.rhai", b"3;")])?;
    put(
        root.path(),
        "other",
        "1.0.0",
        SYNC_AND_REPORT,
        &[("sync.rhai", b"1;"), ("report.rhai", b"2;")],
    )?;

    let report = report_of(root.path(), &JobsState::default(), "report").await?;

    let seen: Vec<String> = report.releases.iter().map(|line| line.version.to_string()).collect();
    assert_eq!(seen, vec!["1.0.0".to_owned()]);
    Ok(())
}

#[tokio::test]
async fn a_release_missing_a_listed_file_is_incomplete_and_names_the_file() -> TestResult {
    let root = tempfile::tempdir()?;
    put(root.path(), "ops", "1.0.0", SYNC, &[("sync.rhai", b"let x = 1;")])?;
    std::fs::remove_file(root.path().join("ops/1.0.0/sync/sync.rhai"))?;

    let report = report_of(root.path(), &JobsState::default(), "sync").await?;

    assert_eq!(report.releases.len(), 1);
    let findings = match &report.releases[0].condition {
        ReleaseCondition::Incomplete(findings) => findings.clone(),
        _ => Vec::new(),
    };
    assert!(
        findings.iter().any(|finding| finding.contains("sync/sync.rhai")),
        "{:?}",
        report.releases[0].condition
    );
    Ok(())
}

#[tokio::test]
async fn a_release_with_altered_content_is_damaged() -> TestResult {
    let root = tempfile::tempdir()?;
    put(root.path(), "ops", "1.0.0", SYNC, &[("sync.rhai", b"let x = 1;")])?;
    std::fs::write(root.path().join("ops/1.0.0/sync/sync.rhai"), b"let x = 99;")?;

    let report = report_of(root.path(), &JobsState::default(), "sync").await?;

    assert!(matches!(
        report.releases.first().map(|line| &line.condition),
        Some(ReleaseCondition::Damaged(_))
    ));
    Ok(())
}

#[tokio::test]
async fn a_version_the_state_records_but_the_store_no_longer_holds_is_removed() -> TestResult {
    let root = tempfile::tempdir()?;
    put(root.path(), "ops", "1.1.0", SYNC, &[("sync.rhai", b"let x = 1;")])?;
    let state: JobsState = toml::from_str("[ops.sync]\ncurrent = \"1.1.0\"\nprevious = \"1.0.0\"\n")?;

    let report = report_of(root.path(), &state, "sync").await?;

    let previous = report.releases.iter().find(|line| line.version.to_string() == "1.0.0");
    assert_eq!(previous.map(|line| &line.condition), Some(&ReleaseCondition::Removed));
    assert_eq!(previous.map(|line| line.kind), Some(ReleaseKind::Previous));
    Ok(())
}

#[tokio::test]
async fn a_store_that_holds_nothing_reports_no_releases() -> TestResult {
    let root = tempfile::tempdir()?;

    let report = report_of(&root.path().join("absent"), &JobsState::default(), "sync").await?;

    assert_eq!(report.releases.len(), 0);
    Ok(())
}

#[tokio::test]
async fn an_unusable_ledger_leaves_the_install_date_and_origin_unknown() -> TestResult {
    let root = tempfile::tempdir()?;
    put(root.path(), "ops", "1.0.0", SYNC, &[("sync.rhai", b"let x = 1;")])?;

    let report = report_of(root.path(), &JobsState::default(), "sync").await?;

    assert_eq!(report.releases.len(), 1);
    assert_eq!(report.releases[0].installed_at, None);
    assert_eq!(report.releases[0].origin, None);
    Ok(())
}

const WITH_TEMPLATES: &str =
    "templates = [\"letter.md\", \"notes\"]\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n";

async fn report_against_source(
    root: &Path,
    source: &crate::fake_vault::FakeVault,
    state: &JobsState,
) -> Result<WorkflowReleases, Box<dyn std::error::Error>> {
    let package = PackageName::try_from("ops")?;
    let store = HostStore::new(root);
    let client = source.session().await?;
    Ok(ReleasesQuery {
        store_name: "va",
        package: &package,
        workflow: "sync",
        state,
        jobs: &[],
        templates: TemplateHome::VaultSource {
            client: &client,
            vault: "va",
        },
    }
    .report(store.engine())
    .await?)
}

#[tokio::test]
async fn each_release_reports_the_templates_it_declares_that_source_no_longer_holds() -> TestResult {
    let root = tempfile::tempdir()?;
    put(
        root.path(),
        "ops",
        "1.0.0",
        WITH_TEMPLATES,
        &[("sync.rhai", b"1"), ("letter.md", b"x"), ("notes/a.md", b"x")],
    )?;
    put(
        root.path(),
        "ops",
        "2.0.0",
        "[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n",
        &[("sync.rhai", b"2")],
    )?;
    let state: JobsState = toml::from_str("[ops.sync]\ncurrent = \"2.0.0\"\nprevious = \"1.0.0\"\n")?;
    let source = crate::fake_vault::FakeVault::new().with_file("registry/workflows/ops/notes/a.md", "x");

    let report = report_against_source(root.path(), &source, &state).await?;

    let missing: Vec<(String, Vec<String>)> = report
        .releases
        .iter()
        .map(|line| (line.version.to_string(), line.missing_templates.clone()))
        .collect();
    assert_eq!(
        missing,
        vec![
            ("2.0.0".to_owned(), Vec::new()),
            ("1.0.0".to_owned(), vec!["letter.md".to_owned()])
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_release_whose_templates_are_held_in_the_release_reports_none_missing() -> TestResult {
    let root = tempfile::tempdir()?;
    put(
        root.path(),
        "ops",
        "1.0.0",
        WITH_TEMPLATES,
        &[("sync.rhai", b"1"), ("letter.md", b"x"), ("notes/a.md", b"x")],
    )?;
    let state: JobsState = toml::from_str("[ops.sync]\ncurrent = \"1.0.0\"\n")?;

    let report = report_of(root.path(), &state, "sync").await?;

    assert!(report.releases.iter().all(|line| line.missing_templates.is_empty()));
    Ok(())
}
