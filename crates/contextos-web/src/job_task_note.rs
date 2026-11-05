//! The words of a task raised for a vault job: its title, and its body of context, evidence, and what to do.
//!
//! The note cites evidence by vault path and stable code only. A failure message, a call's arguments or result, an input
//! value, script text, and a host path never appear in it: those stay in the run record and the state file the evidence
//! links to, where the vault's own access rules apply.

use std::fmt::Write as _;

use contextos_workflows::{AttentionPurpose, Episode};
use time::format_description::well_known::Rfc3339;

use crate::job_attention::TaskSource;

/// The title of the task for `source`.
pub(crate) fn title_of(source: &TaskSource<'_>) -> String {
    let key = &source.facts.key;
    let job = format!("{}/{}/{}", key.package, key.workflow, key.job);
    let code = source.event.codes.first();
    match source.event.purpose {
        AttentionPurpose::ValidationFailed => format!("Job {job} failed validation"),
        AttentionPurpose::Suspended => match code {
            Some(code) => format!("Job {job} was suspended: {code}"),
            None => format!("Job {job} was suspended"),
        },
        AttentionPurpose::RunFailed => {
            let run = match &source.event.episode {
                Episode::Run(number) => format!("R-{number}"),
                other => other.to_string(),
            };
            match code {
                Some(code) => format!("Scheduled run {run} of {job} failed: {code}"),
                None => format!("Scheduled run {run} of {job} failed"),
            }
        }
    }
}

/// The body of the task for `source`: `## Context`, `## Evidence`, and `## What to do`.
pub(crate) fn body_of(source: &TaskSource<'_>) -> String {
    let mut body = String::new();
    body.push_str("## Context\n\n");
    body.push_str(&context_of(source));
    body.push_str("\n\n## Evidence\n\n");
    body.push_str(&evidence_of(source));
    body.push_str("\n## What to do\n\n");
    body.push_str(&what_to_do(source));
    body.push('\n');
    body
}

/// When the episode happened, which is not when the scheduler noticed it: the suspension's own time, the time of the
/// validation that failed, and for a failed run the time it started, which the caller gives as `source.at`.
fn episode_time(source: &TaskSource<'_>) -> time::OffsetDateTime {
    match source.event.purpose {
        AttentionPurpose::Suspended => source.facts.suspension.as_ref().and_then(|record| record.at),
        AttentionPurpose::ValidationFailed => source.facts.validation.as_ref().map(|stored| stored.at),
        AttentionPurpose::RunFailed => None,
    }
    .unwrap_or(source.at)
}

fn context_of(source: &TaskSource<'_>) -> String {
    let key = &source.facts.key;
    let job = format!("{}/{}/{}", key.package, key.workflow, key.job);
    let when = episode_time(source).format(&Rfc3339).unwrap_or_default();
    let state = source.facts.lifecycle;
    match source.event.purpose {
        AttentionPurpose::ValidationFailed => {
            let failures = source.facts.validation.as_ref().map_or_else(String::new, |stored| {
                let mut listed: Vec<String> = Vec::new();
                for failure in &stored.failures {
                    let entry = format!("`{}` (concerning `{}`)", failure.code, failure.artefact);
                    if !listed.contains(&entry) {
                        listed.push(entry);
                    }
                }
                listed.join(", ")
            });
            format!(
                "The job {job} is a ready draft (state: {state}), and its automatic validation, reported at {when}, \
                 failed with {failures}. Until it passes, the job cannot be registered or enabled."
            )
        }
        AttentionPurpose::Suspended => {
            let mut codes = source.event.codes.clone();
            if let Some(failure) = source
                .facts
                .suspension
                .as_ref()
                .and_then(|record| record.failure.clone())
                && !codes.contains(&failure)
            {
                codes.push(failure);
            }
            let listed = codes
                .iter()
                .map(|code| format!("`{code}`"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "The system suspended the job {job} (state: {state}), as recorded at {when}, with {listed}. A \
                 suspended job does not run on its schedule."
            )
        }
        AttentionPurpose::RunFailed => {
            let run = source.event.episode.to_string();
            let listed = source
                .event
                .codes
                .iter()
                .map(|code| format!("`{code}`"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "A scheduled run of the job {job} (state: {state}), {run}, ended without succeeding, as seen at \
                 {when}: {listed}. The job stays enabled, and its next scheduled run goes ahead."
            )
        }
    }
}

fn evidence_of(source: &TaskSource<'_>) -> String {
    let key = &source.facts.key;
    let files = source.files;
    let directory = &files.workflow_directory;
    let mut evidence = String::new();
    let mut line = |text: String| {
        let _ = writeln!(evidence, "- {text}");
    };
    line(format!("The workflow: `registry/workflows/{directory}/workflow.toml`"));
    line(format!(
        "The entry script: `registry/workflows/{directory}/{}`",
        files.entry
    ));
    line(format!(
        "The job file: `registry/workflows/{directory}/jobs/{}`",
        files.job_file
    ));
    line("The job's state, with its stored validation and suspension: `.contextos/jobs.toml`".to_owned());
    if let Some(record) = source.run_record {
        line(format!("The run record: [[{record}]]"));
    }
    line(format!(
        "The run history: `memory/jobs/{}/{}/{}/`",
        key.package, key.workflow, key.job
    ));
    evidence
}

fn what_to_do(source: &TaskSource<'_>) -> String {
    let key = &source.facts.key;
    let (package, workflow, job) = (&key.package, &key.workflow, &key.job);
    match source.event.purpose {
        AttentionPurpose::ValidationFailed => format!(
            "Correct the job's files so they pass, then run `contextos-web jobs validate <vault> {package} {workflow} {job}`, or \
             use Validate on the job's page. The job moves on once validation passes."
        ),
        AttentionPurpose::Suspended => {
            let code = source.event.codes.first().map_or("", String::as_str);
            match code {
                "workflow/validation-failed" => format!(
                    "Correct the job's files so the new version passes validation, then run `contextos-web jobs resume \
                     <vault> {package} {workflow} {job}`, which validates the job first."
                ),
                "workflow/probation-failed" => format!(
                    "Read the run record for the failure, and correct its cause, then resume the job with \
                     `contextos-web jobs resume <vault> {package} {workflow} {job}`, which validates it first. A manual run that \
                     succeeds ends the trial but leaves the job suspended."
                ),
                "workflow/owner-unresolved" => format!(
                    "Set `owner` in the job file to a member of `.contextos/team.toml`, then resume the job with \
                     `contextos-web jobs resume <vault> {package} {workflow} {job}`, or retire the job if it is no longer wanted."
                ),
                _ => format!(
                    "Read the suspension on the job's page and in `.contextos/jobs.toml`, correct its cause, then \
                     resume the job with `contextos-web jobs resume <vault> {package} {workflow} {job}`, which validates it first."
                ),
            }
        }
        AttentionPurpose::RunFailed => "Read the run record. Runs are not retried, so the next scheduled run is the \
                                        next attempt. Suspend the job if the failure needs a change before it runs \
                                        again."
            .to_owned(),
    }
}

#[cfg(test)]
#[path = "job_task_note_test.rs"]
mod tests;
