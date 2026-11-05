//! The rules revision, who asked for a validation, and a job's entity, as the automatic validation pass and an operator's
//! validation record them, against a real `contextos-mcp` session and a real temporary vault. These use the shared job
//! vault, which carries a valid team and job files with an owner and a timeout.

mod support;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const STATE_PATH: &str = ".contextos/jobs.toml";
const FIRST: time::OffsetDateTime = time::macros::datetime!(2026-10-01 03:00:00 UTC);
use contextos_web::job_validation::{JobLocation, validate_and_record, validate_now};
use contextos_workflows::{Actor, JobState, RULES_REVISION};
use support::jobs::{
    JobVault, READY_JOB, Spec, VAULT_NAME, WORKFLOW, current_enabled_state, frontmatter_field, legacy_enabled_state,
    legacy_validation, lifecycle_state, suspended_state, version_of,
};

/// A job file that states an owner and no timeout.
const JOB_WITHOUT_TIMEOUT: &str = "ready = true\nname = \"Nightly\"\nowner = \"vela\"\nversion = \"1.0.0\"\n";

fn stored_validation(vault: &JobVault) -> Result<contextos_workflows::StoredValidation, BoxError> {
    let state = vault.state()?;
    Ok(state
        .job("ops", "sync", "nightly")
        .and_then(JobState::validation)
        .cloned()
        .ok_or("no validation was stored")?)
}

#[tokio::test]
async fn an_enabled_job_whose_file_lacks_timeout_secs_is_suspended_with_validation_failed_on_the_first_pass_under_the_new_rules()
-> Result<(), BoxError> {
    let mut spec = Spec {
        job: JOB_WITHOUT_TIMEOUT,
        ..Spec::default()
    };
    let stored = legacy_enabled_state(&spec);
    spec.state = Some(&stored);
    let vault = JobVault::open(&spec).await?;

    vault.pass(FIRST).await;

    assert_eq!(vault.lifecycle()?, "suspended");
    let state = vault.state()?;
    let record = state
        .job("ops", "sync", "nightly")
        .and_then(JobState::suspension)
        .ok_or("the job has no suspension record")?;
    assert_eq!(record.code, "workflow/validation-failed");
    assert_eq!(record.by, Some(Actor::System));
    let validation = stored_validation(&vault)?;
    assert!(!validation.passed);
    assert!(
        validation
            .failures
            .iter()
            .any(|failure| failure.code == "validation/artefact-invalid" && failure.message.contains("timeout_secs")),
        "{:?}",
        validation.failures
    );
    assert_eq!(validation.rules, RULES_REVISION);
    let tasks = vault.raised_tasks_for("suspended");
    let [(_, task)] = tasks.as_slice() else {
        return Err(format!("expected exactly one suspended task, found {}", tasks.len()).into());
    };
    assert_eq!(
        frontmatter_field(task, "assignee").as_deref(),
        Some("vela"),
        "the owner, who is a member, is asked to fix the file: {task}"
    );
    Ok(())
}

#[tokio::test]
async fn a_suspended_job_whose_result_predates_the_rules_is_left_untouched_beside_an_enabled_one_that_is_suspended()
-> Result<(), BoxError> {
    let mut spec = Spec {
        job: JOB_WITHOUT_TIMEOUT,
        ..Spec::default()
    };
    // `nightly` is suspended and `second` is enabled; both have the same file, which lacks `timeout_secs`, and a
    // validation that predates the rules revision.
    let suspended = format!("{}{}", suspended_state(&spec), legacy_validation(&spec));
    let enabled_beside_it = legacy_enabled_state(&spec).replace("nightly", "second");
    let stored = format!("{suspended}\n{enabled_beside_it}");
    spec.state = Some(&stored);
    let vault = JobVault::open(&spec).await?;
    vault.write("registry/workflows/ops/sync/jobs/second.toml", JOB_WITHOUT_TIMEOUT)?;
    let before = vault.state()?;

    vault.pass(FIRST).await;

    let after = vault.state()?;
    assert_eq!(
        after.lifecycle_of("ops", "sync", "second").as_str(),
        "suspended",
        "the enabled job fails the new rules, so the pass did look at the vault"
    );
    assert_eq!(
        after.job("ops", "sync", "nightly"),
        before.job("ops", "sync", "nightly"),
        "the suspended job is left as it was: no stored result, no suspension record, no change of actor"
    );
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_that_passes_the_new_rules_keeps_its_state_and_starts_no_new_trial() -> Result<(), BoxError> {
    let mut spec = Spec::default();
    let stored = legacy_enabled_state(&spec);
    spec.state = Some(&stored);
    let vault = JobVault::open(&spec).await?;

    vault.pass(FIRST).await;

    assert_eq!(vault.lifecycle()?, "enabled");
    let state = vault.state()?;
    let job = state.job("ops", "sync", "nightly").ok_or("the job has no state")?;
    assert!(
        !job.first_run_pending(),
        "the version did not change, so there is no new trial"
    );
    assert_eq!(job.fingerprint(), Some(&version_of(&spec)));
    let validation = job.validation().ok_or("no validation was stored")?;
    assert!(validation.passed);
    assert_eq!(
        validation.rules, RULES_REVISION,
        "the result now stands under the current rules"
    );
    assert_eq!(validation.by, Actor::Validator);
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_whose_result_already_stands_under_the_current_rules_is_not_validated_again()
-> Result<(), BoxError> {
    let mut spec = Spec::default();
    let stored = current_enabled_state(&spec);
    spec.state = Some(&stored);
    let vault = JobVault::open(&spec).await?;
    let before = vault.read(STATE_PATH)?;

    vault.pass(FIRST).await;

    assert_eq!(
        vault.read(STATE_PATH)?,
        before,
        "a result under the current rules is not repeated"
    );
    Ok(())
}

#[tokio::test]
async fn a_registered_job_whose_script_imports_an_undeclared_module_is_validated_again_and_returns_to_draft()
-> Result<(), BoxError> {
    let mut spec = Spec {
        script: "import \"helpers\" as h;\nh::refresh();\n",
        ..Spec::default()
    };
    // Validated and dry-run under the rules before undeclared imports were refused, when the script compiled.
    let stored = format!(
        "{}\n[ops.sync.nightly.validation]\nby = \"validator\"\nrules = 1\nat = \"2026-09-01T00:00:00Z\"\n\
         fingerprint = \"{}\"\npassed = true\nfailures = []\nunverifiable = []\n",
        lifecycle_state(&spec, "registered"),
        String::from(version_of(&spec))
    );
    spec.state = Some(&stored);
    let vault = JobVault::open(&spec).await?;

    vault.pass(FIRST).await;

    assert_eq!(vault.lifecycle()?, "draft");
    let validation = stored_validation(&vault)?;
    assert_eq!(validation.rules, RULES_REVISION);
    assert!(
        validation
            .failures
            .iter()
            .any(|failure| failure.code == "validation/script-syntax" && failure.message.contains("names no module")),
        "{:?}",
        validation.failures
    );
    Ok(())
}

#[tokio::test]
async fn a_registered_job_that_fails_the_new_rules_returns_to_draft_with_the_failure_stored() -> Result<(), BoxError> {
    let mut spec = Spec {
        job: JOB_WITHOUT_TIMEOUT,
        ..Spec::default()
    };
    let stored = format!("{}{}", lifecycle_state(&spec, "registered"), legacy_validation(&spec));
    spec.state = Some(&stored);
    let vault = JobVault::open(&spec).await?;

    vault.pass(FIRST).await;

    assert_eq!(
        vault.lifecycle()?,
        "draft",
        "a failure the job owns withdraws its validation and dry run"
    );
    let state = vault.state()?;
    let job = state.job("ops", "sync", "nightly").ok_or("the job has no state")?;
    assert_eq!(
        job.fingerprint(),
        None,
        "no version is held once the validation is withdrawn"
    );
    assert_eq!(job.dry_run_at(), None);
    let validation = job.validation().ok_or("no validation was stored")?;
    assert!(!validation.passed);
    assert!(
        validation
            .failures
            .iter()
            .any(|failure| failure.code == "validation/artefact-invalid" && failure.message.contains("timeout_secs")),
        "{:?}",
        validation.failures
    );
    assert_eq!(validation.rules, RULES_REVISION);
    Ok(())
}

#[tokio::test]
async fn a_validation_asked_for_by_hand_is_stored_as_the_operators() -> Result<(), BoxError> {
    let vault = JobVault::open(&Spec::default()).await?;
    vault.pass(FIRST).await;
    assert_eq!(
        stored_validation(&vault)?.by,
        Actor::Validator,
        "the pass stores the validator's"
    );
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let servers = vec!["contextos".to_owned()];

    validate_now(
        &client,
        VAULT_NAME,
        &servers,
        contextos_web::jobs::WorkflowFolder {
            package: "ops",
            directory: "sync",
        },
        "nightly.toml",
        FIRST,
    )
    .await?;
    assert_eq!(stored_validation(&vault)?.by, Actor::Operator);

    let other = JobVault::open(&Spec::default()).await?;
    let client = other.clients.get("contextos").ok_or("no primary server")?;
    let location = JobLocation {
        package: "ops".to_owned(),
        directory: "sync".to_owned(),
        file: "nightly.toml".to_owned(),
    };
    validate_and_record(&client, VAULT_NAME, &servers, &location, FIRST).await?;
    let stored = stored_validation(&other)?;
    assert_eq!(stored.by, Actor::Operator);
    assert_eq!(stored.rules, RULES_REVISION);
    Ok(())
}

const ENTITIES: &str = "[[entities]]\nslug = \"acme\"\nname = \"Acme\"\naccent = 1\n";

fn job_in(entity: &str) -> String {
    format!("{READY_JOB}entity = \"{entity}\"\n")
}

fn workflow_in(entity: &str) -> String {
    format!("{WORKFLOW}entity = \"{entity}\"\n")
}

async fn validated_after_a_pass(spec: &Spec<'_>, entities: Option<&str>) -> Result<JobVault, BoxError> {
    validated_after_a_pass_logging(spec, entities, false).await
}

/// As [`validated_after_a_pass`], with the server's log kept for the test to read.
async fn validated_after_a_logged_pass(spec: &Spec<'_>, entities: Option<&str>) -> Result<JobVault, BoxError> {
    validated_after_a_pass_logging(spec, entities, true).await
}

async fn validated_after_a_pass_logging(
    spec: &Spec<'_>,
    entities: Option<&str>,
    logged: bool,
) -> Result<JobVault, BoxError> {
    let vault = if logged {
        JobVault::open_logged(spec).await?
    } else {
        JobVault::open(spec).await?
    };
    if let Some(entities) = entities {
        vault.write(".contextos/entities.toml", entities)?;
    }
    vault.pass(FIRST).await;
    Ok(vault)
}

fn failure_codes(vault: &JobVault) -> Result<Vec<String>, BoxError> {
    Ok(stored_validation(vault)?
        .failures
        .into_iter()
        .map(|failure| failure.code)
        .collect())
}

#[tokio::test]
async fn an_unknown_entity_fails_validation_and_unreadable_entities_make_the_check_unverifiable() -> Result<(), BoxError>
{
    // A job or a workflow naming an entity that is not configured fails, and the job stays a draft.
    let job = job_in("nosuch");
    let vault = validated_after_a_pass(
        &Spec {
            job: &job,
            ..Spec::default()
        },
        Some(ENTITIES),
    )
    .await?;
    assert_eq!(vault.lifecycle()?, "draft");
    assert_eq!(failure_codes(&vault)?, vec!["validation/entity-unresolved".to_owned()]);

    let workflow = workflow_in("nosuch");
    let vault = validated_after_a_pass(
        &Spec {
            workflow: &workflow,
            ..Spec::default()
        },
        Some(ENTITIES),
    )
    .await?;
    assert_eq!(failure_codes(&vault)?, vec!["validation/entity-unresolved".to_owned()]);

    // Positive controls: `vault`, and a configured slug, pass.
    let job = job_in("vault");
    let vault = validated_after_a_pass(
        &Spec {
            job: &job,
            ..Spec::default()
        },
        None,
    )
    .await?;
    assert_eq!(vault.lifecycle()?, "validated", "{:?}", stored_validation(&vault)?);
    let job = job_in("acme");
    let vault = validated_after_a_pass(
        &Spec {
            job: &job,
            ..Spec::default()
        },
        Some(ENTITIES),
    )
    .await?;
    assert_eq!(vault.lifecycle()?, "validated", "{:?}", stored_validation(&vault)?);

    // Entities that cannot be read make the check unverifiable: no defect is recorded, but a job whose check could
    // not be made does not pass either, so it stays a draft until the entities can be read.
    let job = job_in("acme");
    let vault = validated_after_a_logged_pass(
        &Spec {
            job: &job,
            ..Spec::default()
        },
        Some("[[entities]\nslug = \n"),
    )
    .await?;
    // The index says why it went without entities, which is what the operator reads in the server's log.
    let log = std::fs::read_to_string(vault.server_log()).unwrap_or_default();
    assert!(
        log.lines().any(|line| line.contains("WARN")
            && line.contains("entity list unusable")
            && line.contains("index/entities")),
        "the server warns that the entity list is unusable: {log}"
    );
    let stored = stored_validation(&vault)?;
    assert!(stored.failures.is_empty(), "{:?}", stored.failures);
    assert!(!stored.unverifiable.is_empty(), "the entity check could not be made");
    assert!(!stored.passed, "a check that could not be made is not a pass");
    assert_eq!(
        vault.lifecycle()?,
        "draft",
        "an unverifiable check keeps the job from passing"
    );
    Ok(())
}
