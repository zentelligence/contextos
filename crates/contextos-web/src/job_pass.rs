//! The scheduler's validation pass over one vault's jobs: validates what the rules say is due, suspends an enabled job
//! whose owner is gone or whose result no longer passes the rules, and says what it saw of each job.
//!
//! The checks themselves, the version of a job, and the recording of an operator's validation are in
//! [`crate::job_validation`]. This module decides which job is looked at, when, and what happens to it.

use contextos_core::TeamRoster;
use contextos_workflows::{
    Actor, JobFingerprint, JobLifecycle, JobScope, NewVersionVerdict, StoreKind, Suspension, SuspensionCause,
    Transition,
};
use time::OffsetDateTime;

use crate::job_attention::JobAttentionMemory;
use crate::job_checks::OwnerCheck;
use crate::job_discovery::read_package_folders;
use crate::job_team_gate::{PassTeam, team_for_pass};
use crate::job_validation::{
    CurrentVersion, current_version, entry_named, names_ready, read_text, record_result_kept, tracked_slug,
    validate_job_with_state,
};
use crate::jobs::{ResolvedEntity, WorkflowFolder, declared_entity_of, declared_owner_of, list_job_files};
use crate::mcp_client::{McpCallError, McpClient};
use crate::scheduler::{read_state, write_state};
use crate::store_retention::{RetentionWarning, retain_vault, retain_vault_held};
use contextos_packages::PackageStore as _;

/// What one pass over a vault's jobs did.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct PassReport {
    /// Jobs validated and found to pass, and so now `validated`.
    pub validated: usize,
    /// Jobs validated and found to fail, which stay as they were with the failures recorded.
    pub failed: usize,
    /// Validated or registered jobs whose version changed, which went back to `draft`.
    pub returned_to_draft: usize,
    /// Enabled jobs whose new version passed validation, so they stay enabled with their first live run on trial.
    pub on_probation: usize,
    /// Enabled jobs whose new version failed validation, which were suspended with the failures as the reason.
    pub suspended: usize,
    /// Retention could not finish after the pass. What the pass did stands.
    pub warnings: Vec<RetentionWarning>,
}

/// One pass over every job in the vault, as the validator and as the system:
///
/// - a `draft` job whose job file says `ready = true` is validated once for each version it has, and becomes
///   `validated` if it passes, with the result recorded either way;
/// - a `validated` or `registered` job whose version has changed goes back to `draft`, and is validated afresh in the
///   same pass if it is ready;
/// - an `enabled` job whose owner is no member of the team is suspended by the system, before anything else is looked
///   at, because a job nobody answers for does not run;
/// - an `enabled` job whose version has changed is validated at once: a pass keeps it enabled with its first live run
///   on trial, without repeating the dry run, and a failure suspends it with the failures as the reason;
/// - a `validated`, `registered`, or `enabled` job whose stored result predates the current validation rules is
///   validated again, once: the result is kept against the version it is for, and an enabled job that no longer passes
///   is suspended, because tightening the rules would otherwise never reach a job whose files nobody edits.
///
/// A `suspended` job stays suspended whatever its files do, and a `retired` job does nothing. A state file that cannot
/// be read, or that another writer changed during the pass, ends the pass without writing, and the next one starts
/// again. A team that cannot be used is the caller's to deal with: this pass is given a valid one.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself fails after the team has been read. A team that
/// cannot be read or used leaves the pass undone, as the scheduler's own pass leaves it (see
/// [`crate::job_team_gate::team_for_pass`], which decides it), since a pass cannot say a job has no owner when it does
/// not know who the owners can be.
pub async fn validation_pass(
    client: &McpClient,
    vault_name: &str,
    known_mcp_servers: &[String],
    now: OffsetDateTime,
) -> Result<PassReport, McpCallError> {
    match team_for_pass(client, &JobAttentionMemory::default(), vault_name).await {
        PassTeam::Roster(roster) => Ok(validation_pass_for(client, vault_name, known_mcp_servers, &roster, now)
            .await?
            .report),
        PassTeam::Waits => Ok(PassReport::default()),
    }
}

/// What a pass saw of one job, for what happens after it: the tasks the scheduler raises. Read from the files the pass
/// read anyway, so reconciling needs no second read of them.
#[derive(Clone, Debug)]
pub struct ObservedJob {
    pub workflow_slug: String,
    pub job_slug: String,
    /// The package folder under `registry/workflows/` that holds the job's workflow.
    pub package: String,
    /// The job's workflow folder within its package.
    pub directory: String,
    /// The job file's name within that folder's `jobs/` folder.
    pub file: String,
    /// The workflow's entry script, when its manifest names one.
    pub entry: Option<String>,
    /// Whether the job file says authoring is finished.
    pub ready: bool,
    /// The job's version, when the pass worked it out: for a job it considered. `None` otherwise, and when its files
    /// could not all be read.
    pub version: Option<JobFingerprint>,
    /// The owner the job file declares, whether or not the job loads.
    pub declared_owner: Option<contextos_core::DeclaredOwner>,
    pub entity: ResolvedEntity,
}

/// What a pass found and saw.
#[derive(Debug, Default)]
pub struct PassOutcome {
    pub report: PassReport,
    pub observed: Vec<ObservedJob>,
}

/// [`validation_pass`] over a team the caller has already read and found valid, which also says what it saw of each job.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself fails.
pub async fn validation_pass_for(
    client: &McpClient,
    vault_name: &str,
    known_mcp_servers: &[String],
    roster: &TeamRoster,
    now: OffsetDateTime,
) -> Result<PassOutcome, McpCallError> {
    // The first validation of a workflow release chooses its library releases and records them in the pass's one state
    // write, so the vault's store lock is held from before the choice to after the write: retention cannot remove a
    // chosen release in between. A store another operation holds is turned away, and the next pass looks again.
    let Ok(engine) = client.vault_packages(vault_name).await else {
        let mut outcome = pass_over_jobs(client, vault_name, known_mcp_servers, roster, now).await?;
        if outcome.report.validated > 0
            && let Err(error) = retain_vault(client, vault_name).await
        {
            outcome
                .report
                .warnings
                .push(RetentionWarning::raised(StoreKind::Vault, &error));
        }
        return Ok(outcome);
    };
    let lock = match engine.lock().await {
        Ok(lock) => lock,
        Err(error) => {
            tracing::warn!(
                vault = vault_name,
                code = error.code(),
                "the vault's package store is in use, so jobs were not validated this pass"
            );
            return Ok(PassOutcome::default());
        }
    };
    let mut outcome = pass_over_jobs(client, vault_name, known_mcp_servers, roster, now).await?;
    // A job validated for the first time on its workflow release fixes that release's library lock, which can leave a
    // library release nothing keeps. A retention that cannot finish is a warning: what the pass did stands.
    if outcome.report.validated > 0
        && let Err(error) = retain_vault_held(&engine, &lock, client, vault_name).await
    {
        outcome
            .report
            .warnings
            .push(RetentionWarning::raised(StoreKind::Vault, &error));
    }
    Ok(outcome)
}

/// The pass itself, over the jobs of every package folder, writing the job state once.
async fn pass_over_jobs(
    client: &McpClient,
    vault_name: &str,
    known_mcp_servers: &[String],
    roster: &TeamRoster,
    now: OffsetDateTime,
) -> Result<PassOutcome, McpCallError> {
    let mut outcome = PassOutcome::default();
    let packages = read_package_folders(client, vault_name).await?;
    if packages.is_empty() {
        return Ok(outcome);
    }
    let Ok(mut snapshot) = read_state(client, vault_name).await else {
        tracing::warn!(
            vault = vault_name,
            "the job state could not be read, so jobs were not validated this pass"
        );
        return Ok(outcome);
    };
    let context = PassContext {
        client,
        vault_name,
        known_mcp_servers,
        roster,
        now,
    };
    let mut changed = false;
    for package in &packages {
        for folder in package.folders() {
            // A job is found by listing the job files where they live. Its workflow is the folder they are in, and the
            // workflow's own manifest is read only for the details a task about the job quotes.
            let workflow_text = read_text(
                client,
                &format!("{vault_name}://registry/workflows/{folder}/workflow.toml"),
            )
            .await?;
            for file in list_job_files(client, vault_name, folder).await? {
                let Some(job_text) = read_text(
                    client,
                    &format!("{vault_name}://registry/workflows/{folder}/jobs/{file}"),
                )
                .await?
                else {
                    continue;
                };
                let files = JobFiles {
                    folder,
                    file: &file,
                    workflow_text: workflow_text.as_deref(),
                    job_text: &job_text,
                };
                changed |= visit_job(&context, &mut snapshot.value, &files, &mut outcome).await?;
            }
        }
    }
    if changed && write_state(client, vault_name, &snapshot).await.is_err() {
        tracing::warn!(
            vault = vault_name,
            "the job state changed or could not be written, so this pass's validation results were dropped"
        );
        return Ok(PassOutcome {
            report: PassReport::default(),
            observed: outcome.observed,
        });
    }
    Ok(outcome)
}

/// What a pass needs to look at jobs.
struct PassContext<'a> {
    client: &'a McpClient,
    vault_name: &'a str,
    known_mcp_servers: &'a [String],
    roster: &'a TeamRoster,
    now: OffsetDateTime,
}

/// The files of one job as a pass has read them.
struct JobFiles<'a> {
    folder: WorkflowFolder<'a>,
    file: &'a str,
    /// The workflow's own manifest in source, when it can be read: only the details a task quotes come from it.
    workflow_text: Option<&'a str>,
    job_text: &'a str,
}

/// Looks at one job in a pass and applies what the rules say: suspends an enabled job whose owner is gone, returns a job
/// to draft when its version changed, validates a ready draft whose version has not been validated, and validates again
/// a job whose result predates the rules. Records what it saw. Returns whether the state changed.
async fn visit_job(
    context: &PassContext<'_>,
    state: &mut contextos_workflows::JobsState,
    files: &JobFiles<'_>,
    outcome: &mut PassOutcome,
) -> Result<bool, McpCallError> {
    let stem = files.file.strip_suffix(".toml").unwrap_or(files.file);
    let Some(job_slug) = tracked_slug(files.job_text, stem) else {
        return Ok(false);
    };
    let package = files.folder.package;
    let workflow_slug = files.folder.directory;
    let lifecycle = state.lifecycle_of(package, workflow_slug, &job_slug);
    let ready = names_ready(files.job_text);
    let mut observed = ObservedJob {
        workflow_slug: workflow_slug.to_owned(),
        job_slug: job_slug.clone(),
        package: files.folder.package.to_owned(),
        directory: files.folder.directory.to_owned(),
        file: files.file.to_owned(),
        entry: files.workflow_text.and_then(entry_named),
        ready,
        version: None,
        declared_owner: declared_owner_of(files.job_text),
        entity: ResolvedEntity::from((
            declared_entity_of(files.job_text).as_deref(),
            files.workflow_text.and_then(declared_entity_of).as_deref(),
        )),
    };
    let result = look_at_job(
        context,
        state,
        files,
        &job_slug,
        lifecycle,
        &mut observed,
        &mut outcome.report,
    )
    .await;
    outcome.observed.push(observed);
    result
}

async fn look_at_job(
    context: &PassContext<'_>,
    state: &mut contextos_workflows::JobsState,
    files: &JobFiles<'_>,
    job_slug: &str,
    lifecycle: JobLifecycle,
    observed: &mut ObservedJob,
    report: &mut PassReport,
) -> Result<bool, McpCallError> {
    let package = files.folder.package;
    let workflow_slug = files.folder.directory;
    if lifecycle == JobLifecycle::Enabled
        && crate::job_owner_check::suspend_for_unresolved_owner(
            state,
            JobScope {
                package,
                workflow: workflow_slug,
                job: job_slug,
            },
            observed.declared_owner.as_ref(),
            context.roster,
            context.now,
        )
    {
        report.suspended += 1;
        return Ok(true);
    }
    let considered = match lifecycle {
        JobLifecycle::Draft => observed.ready,
        JobLifecycle::Validated | JobLifecycle::Registered | JobLifecycle::Enabled => true,
        JobLifecycle::Suspended | JobLifecycle::Retired => false,
    };
    if !considered {
        return Ok(false);
    }
    let current = match current_version(
        context.client,
        context.vault_name,
        (files.folder, files.job_text),
        Some(&*state),
    )
    .await?
    {
        CurrentVersion::Known(version) => Some(version),
        CurrentVersion::Gone => None,
        // A release that is incomplete or cannot be verified says nothing about the job, so its stage and recorded
        // version stay as they are and the next pass looks again.
        CurrentVersion::Unverifiable => return Ok(false),
    };
    observed.version.clone_from(&current);
    if state.new_version_under_enabled_job(package, workflow_slug, job_slug, current.as_ref()) {
        return review_new_version(context, state, files, job_slug, report).await;
    }
    let mut changed = false;
    if state.version_drift(package, workflow_slug, job_slug, current.as_ref())
        && state
            .transition(
                package,
                workflow_slug,
                job_slug,
                Actor::System,
                &Transition::VersionChanged,
                context.now,
            )
            .is_ok()
    {
        report.returned_to_draft += 1;
        changed = true;
    }
    if state.validation_due(package, workflow_slug, job_slug, observed.ready, current.as_ref()) {
        let validation = validate_job_with_state(
            context.client,
            context.vault_name,
            (files.folder, files.file),
            context.known_mcp_servers,
            OwnerCheck::Roster(context.roster),
            Some(&*state),
        )
        .await?;
        if validation.release_changed_while_read {
            // The release became unverifiable between the version read and the check, which says nothing about the job.
            return Ok(changed);
        }
        // The result belongs to the version that was read for the check, which can be newer than `current` when a file
        // changed in between, so it is recorded against that one.
        let kept = record_result_kept(
            state,
            package,
            workflow_slug,
            job_slug,
            &validation,
            Actor::Validator,
            context.now,
        );
        if kept.lock == contextos_workflows::LockOutcome::Conflict {
            // Another job's validation fixed the release's library lock first: nothing was kept, and the next pass
            // validates this job against the recorded lock.
            return Ok(false);
        }
        if kept.validated {
            report.validated += 1;
        } else {
            report.failed += 1;
        }
        return Ok(true);
    }
    if state.rules_outdated(package, workflow_slug, job_slug) {
        changed |= revalidate_under_current_rules(context, state, files, job_slug, report).await?;
    }
    Ok(changed)
}

/// Validates a `validated`, `registered`, or `enabled` job whose stored result was made under older rules, and keeps the
/// result against the version it is for as [`contextos_workflows::JobsState::keep_validation`] does: a failure the job
/// owns returns a validated or registered job to draft and suspends an enabled one. A check that could not be made
/// changes nothing, so the job is looked at again on a later pass. Returns whether the state changed.
async fn revalidate_under_current_rules(
    context: &PassContext<'_>,
    state: &mut contextos_workflows::JobsState,
    files: &JobFiles<'_>,
    job_slug: &str,
    report: &mut PassReport,
) -> Result<bool, McpCallError> {
    let package = files.folder.package;
    let workflow_slug = files.folder.directory;
    if !matches!(
        state.lifecycle_of(package, workflow_slug, job_slug),
        JobLifecycle::Validated | JobLifecycle::Registered | JobLifecycle::Enabled
    ) {
        return Ok(false);
    }
    let validation = validate_job_with_state(
        context.client,
        context.vault_name,
        (files.folder, files.file),
        context.known_mcp_servers,
        OwnerCheck::Roster(context.roster),
        Some(&*state),
    )
    .await?;
    if validation.report.failures.is_empty() && !validation.report.unverifiable.is_empty() {
        return Ok(false);
    }
    let kept = state.keep_validation(
        package,
        workflow_slug,
        job_slug,
        validation.stored(context.now),
        context.now,
    );
    if kept.returned_to_draft {
        report.returned_to_draft += 1;
    }
    if kept.suspended {
        report.suspended += 1;
    }
    Ok(true)
}

/// Validates the new version of an `enabled` job at once and applies the verdict: a pass keeps the job enabled with its
/// first live run on trial, and a failure, or a version that cannot be determined, suspends it with the failures as the
/// reason. A check that could not be made leaves the job's state alone to be looked at again on a later pass. Whatever
/// validation found is kept against the version it was made for. Returns whether the state changed.
///
/// A version that turns out to be the one on record after all, because a file changed back or a read failed once,
/// changes nothing.
async fn review_new_version(
    context: &PassContext<'_>,
    state: &mut contextos_workflows::JobsState,
    files: &JobFiles<'_>,
    job_slug: &str,
    report: &mut PassReport,
) -> Result<bool, McpCallError> {
    let package = files.folder.package;
    let workflow_slug = files.folder.directory;
    let mut validation = validate_job_with_state(
        context.client,
        context.vault_name,
        (files.folder, files.file),
        context.known_mcp_servers,
        OwnerCheck::Roster(context.roster),
        Some(&*state),
    )
    .await?;
    if validation.release_changed_while_read {
        return Ok(false);
    }
    // A file edited away from the release or predecessor the job was enabled with is another job, not a new version of
    // this one: it fails on that key and is suspended, and never put on trial.
    validation.apply_release_rules(state, package, (workflow_slug, job_slug));
    let recorded = state
        .job(package, workflow_slug, job_slug)
        .and_then(contextos_workflows::JobState::fingerprint)
        .cloned();
    if validation.fingerprint.is_some() && validation.fingerprint == recorded {
        return Ok(false);
    }
    state.record_validation(package, workflow_slug, job_slug, validation.stored(context.now));
    let package_unavailable = validation.package_unavailable;
    let (actor, transition) = match NewVersionVerdict::from((&validation.report, validation.fingerprint)) {
        NewVersionVerdict::OnProbation(version) => (Actor::Validator, Transition::OnProbation(version)),
        NewVersionVerdict::Suspend(reason) => (
            Actor::System,
            Transition::Suspend(Suspension {
                cause: if package_unavailable {
                    SuspensionCause::PackageUnavailable
                } else {
                    SuspensionCause::ValidationFailed
                },
                reason,
            }),
        ),
        NewVersionVerdict::Wait => return Ok(true),
    };
    let on_probation = matches!(transition, Transition::OnProbation(_));
    if on_probation && let Some(basis) = &validation.made_from {
        // The first passing validation on a workflow release fixes its library lock, whichever job it is for, and no
        // later read of the release chooses libraries again. A lock another validation fixed first is the one this was
        // made against, so it agrees; one that differs, or a release that takes no new lock, puts nothing on trial.
        if let Err(refusal) = state.fix_release_lock(&basis.package, workflow_slug, &basis.release, basis.lock.clone())
        {
            tracing::warn!(
                vault = context.vault_name,
                workflow = workflow_slug,
                job = job_slug,
                %refusal,
                "an enabled job's new version was not put on trial because its release's library lock could not be fixed"
            );
            return Ok(true);
        }
    }
    if state
        .transition(package, workflow_slug, job_slug, actor, &transition, context.now)
        .is_ok()
    {
        if on_probation {
            report.on_probation += 1;
        } else {
            report.suspended += 1;
        }
    }
    Ok(true)
}

#[cfg(test)]
#[path = "job_pass_libraries_test.rs"]
mod library_tests;
