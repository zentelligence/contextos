use serde_json::Value;

use super::*;

type TestResult = Result<(), Box<dyn Error>>;

/// A task that does not complete, as when its thread panics or the runtime cancels it, so awaiting it fails.
async fn failed_task() -> Result<Result<AuthoringReport, AuthoringError>, tokio::task::JoinError> {
    let handle = tokio::spawn(std::future::pending::<Result<AuthoringReport, AuthoringError>>());
    handle.abort();
    handle.await
}

#[tokio::test]
async fn a_blocking_task_that_fails_to_complete_is_reported_as_a_job_store_refusal_in_one_json_document() -> TestResult
{
    let report = AuthoringReport::from(failed_task().await);

    let AuthoringReport::Refused(ref error) = report else {
        return Err(format!("expected a refusal, got {report:?}").into());
    };
    assert_eq!(error.code(), "workflow/job-store-unavailable");
    let document: Value = serde_json::from_str(&render(&report, OutputFormat::Json))?;
    assert_eq!(document["error"]["code"], "workflow/job-store-unavailable");
    Ok(())
}

#[tokio::test]
async fn a_blocking_task_that_completes_is_reported_as_it_came_out() -> TestResult {
    let refused = AuthoringReport::from(Ok(Err(AuthoringError::JobNotFound {
        key: "a/b/c".to_owned(),
    })));
    let listed = AuthoringReport::from(Ok(Ok(AuthoringReport::Listed(Store::Shared, Vec::new()))));

    assert!(matches!(refused, AuthoringReport::Refused(ref error) if error.code() == "workflow/job-not-found"));
    assert!(matches!(listed, AuthoringReport::Listed(_, ref rows) if rows.is_empty()));
    Ok(())
}

#[tokio::test]
async fn a_command_whose_blocking_thread_panics_still_reports_one_refusal_document() -> TestResult {
    let report = report_of(|| -> Result<AuthoringReport, AuthoringError> {
        std::panic::resume_unwind(Box::new("the blocking thread failed"))
    })
    .await;

    let AuthoringReport::Refused(ref error) = report else {
        return Err(format!("expected a refusal, got {report:?}").into());
    };
    assert_eq!(error.code(), "workflow/job-store-unavailable");
    let document: Value = serde_json::from_str(&render(&report, OutputFormat::Json))?;
    assert_eq!(document["error"]["code"], "workflow/job-store-unavailable");
    Ok(())
}

#[tokio::test]
async fn a_command_whose_task_returns_a_refusal_reports_that_refusal() -> TestResult {
    let report = report_of(|| {
        Err(AuthoringError::JobNotFound {
            key: "a/b/c".to_owned(),
        })
    })
    .await;

    assert!(matches!(report, AuthoringReport::Refused(ref error) if error.code() == "workflow/job-not-found"));
    Ok(())
}

fn listing(key: contextos_web::shared_claim::SharedJobKey) -> JobListing {
    JobListing {
        key,
        file: None,
        name: Some("Weekly".to_owned()),
        lifecycle: contextos_workflows::JobLifecycle::Draft,
        schedule: None,
        ready: Some(true),
        version: semver::Version::parse("1.2.0").ok(),
        release_kind: Some(contextos_workflows::ReleaseKind::Future),
        predecessor: Some("weekly-old".to_owned()),
        successor: Some("weekly-next".to_owned()),
        replaced_by: None,
        hash: None,
        problems: Vec::new(),
        suspension: None,
    }
}

fn key(package: &str) -> contextos_web::shared_claim::SharedJobKey {
    contextos_web::shared_claim::SharedJobKey {
        package: package.to_owned(),
        workflow: "sync".to_owned(),
        job: "weekly".to_owned(),
    }
}

#[test]
fn a_listed_job_shows_its_release_its_role_and_the_jobs_it_is_linked_to_in_json() -> TestResult {
    let report = AuthoringReport::Listed(Store::Vault("va".to_owned()), vec![listing(key("sync"))]);

    let document: Value = serde_json::from_str(&render(&report, OutputFormat::Json))?;

    let job = &document["jobs"][0];
    assert_eq!(job["version"], "1.2.0");
    assert_eq!(job["release_kind"], "future");
    assert_eq!(job["predecessor"], "weekly-old");
    assert_eq!(job["successor"], "weekly-next");
    assert_eq!(document["store"], "va");
    Ok(())
}

#[test]
fn a_listed_job_shows_its_release_and_role_in_text() {
    let report = AuthoringReport::Listed(Store::Shared, vec![listing(key("acme"))]);

    let text = render(&report, OutputFormat::Text);

    assert!(text.contains("1.2.0"), "{text}");
    assert!(text.contains("future"), "{text}");
    assert!(text.contains("weekly-old") && text.contains("weekly-next"), "{text}");
}

fn authored(store: Store, package: &str) -> Result<AuthoringReport, Box<dyn Error>> {
    Ok(AuthoringReport::Authored(
        store,
        Authored {
            outcome: contextos_web::shared_job_authoring::AuthoredOutcome::Created,
            key: key(package),
            hash: ContentHash::try_from("0".repeat(64).as_str())?,
            warnings: Vec::new(),
        },
    ))
}

#[test]
fn a_shared_result_names_its_package_and_a_vault_result_names_its_vault_and_package() -> TestResult {
    let shared: Value = serde_json::from_str(&render(&authored(Store::Shared, "acme")?, OutputFormat::Json))?;
    let vault_report = authored(Store::Vault("va".to_owned()), "ops")?;
    let vault: Value = serde_json::from_str(&render(&vault_report, OutputFormat::Json))?;

    assert_eq!(shared["store"], "shared");
    assert_eq!(shared["package"], "acme");
    assert_eq!(vault["store"], "va");
    assert_eq!(vault["package"], "ops");
    assert!(render(&vault_report, OutputFormat::Text).starts_with("created job va/ops/sync/weekly"));
    Ok(())
}
