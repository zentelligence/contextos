use clap::Parser;
use contextos_core::ContentHash;
use contextos_web::shared_job_authoring::{
    Authored, AuthoredOutcome, AuthoringError, AuthoringWarning, JobListing, ListingProblem, ShownJob,
};
use contextos_workflows::{JobLifecycle, JobSchedule};

use crate::job_authoring_commands::{AuthoringReport, OutputFormat, Store, render};

use super::*;

/// Parses `arguments` as the `jobs` subcommand the way the real command line does.
#[derive(Debug, Parser)]
struct Jobs {
    #[command(subcommand)]
    action: JobsAction,
}

fn parse(arguments: &[&str]) -> Result<JobsAction, clap::Error> {
    Jobs::try_parse_from(std::iter::once("jobs").chain(arguments.iter().copied())).map(|jobs| jobs.action)
}

fn invocation(arguments: &[&str]) -> Result<Invocation, Box<dyn Error>> {
    match parse(arguments)? {
        JobsAction::Operate(action) => Ok(Invocation::try_from(action)?),
        other => Err(format!("not an operator command: {other:?}").into()),
    }
}

#[test]
fn every_lifecycle_command_names_the_job_by_vault_package_workflow_and_job() -> Result<(), Box<dyn Error>> {
    for command in ["validate", "resume", "retire", "reactivate", "delete"] {
        let parsed = invocation(&[command, "harbour", "ops", "sync", "nightly"])?;
        assert_eq!(parsed.job.target, ["harbour", "ops", "sync", "nightly"], "{command}");
        assert!(!parsed.job.shared, "{command}");
    }
    Ok(())
}

#[test]
fn shared_marks_the_job_as_a_shared_job_wherever_it_is_given() -> Result<(), Box<dyn Error>> {
    let before = invocation(&["retire", "--shared", "acme", "sync", "nightly"])?;
    let after = invocation(&["retire", "acme", "sync", "nightly", "--shared"])?;

    assert!(before.job.shared && after.job.shared);
    assert_eq!(before.job.target, ["acme", "sync", "nightly"]);
    Ok(())
}

/// The arguments of one command, and what the request it makes must be.
type Case<'a> = (&'a [&'a str], fn(&OperatorRequest) -> bool);

#[test]
fn each_command_makes_its_own_request() -> Result<(), Box<dyn Error>> {
    let cases: [Case; 7] = [
        (&["validate", "v", "p", "w", "j"], |r| {
            matches!(r, OperatorRequest::Validate)
        }),
        (&["resume", "v", "p", "w", "j"], |r| {
            matches!(r, OperatorRequest::Resume)
        }),
        (&["retire", "v", "p", "w", "j"], |r| {
            matches!(r, OperatorRequest::Retire)
        }),
        (&["reactivate", "v", "p", "w", "j"], |r| {
            matches!(r, OperatorRequest::Reactivate)
        }),
        (&["delete", "v", "p", "w", "j"], |r| {
            matches!(r, OperatorRequest::Delete)
        }),
        (&["schedule", "v", "p", "w", "j", "--clear"], |r| {
            matches!(r, OperatorRequest::ClearSchedule)
        }),
        (&["suspend", "v", "p", "w", "j", "--reason", "held"], |r| {
            matches!(r, OperatorRequest::Suspend(_))
        }),
    ];
    for (arguments, expected) in cases {
        let parsed = invocation(arguments)?;
        assert!(expected(&parsed.request), "{arguments:?}: {:?}", parsed.request);
    }
    Ok(())
}

#[test]
fn a_shared_schedule_command_takes_the_expression_after_the_three_words_that_name_the_job() -> Result<(), Box<dyn Error>>
{
    let parsed = invocation(&["schedule", "--shared", "acme", "sync", "nightly", "30 4 * * 1"])?;

    assert_eq!(parsed.job.target, ["acme", "sync", "nightly"]);
    assert!(parsed.job.shared);
    assert!(matches!(parsed.request, OperatorRequest::Schedule(_)));
    Ok(())
}

#[test]
fn a_schedule_command_carries_the_expression_it_was_given() -> Result<(), Box<dyn Error>> {
    let parsed = invocation(&["schedule", "v", "p", "w", "j", "30 4 * * 1"])?;

    let OperatorRequest::Schedule(schedule) = parsed.request else {
        return Err("expected a schedule".into());
    };
    assert_eq!(schedule, JobSchedule::try_from("30 4 * * 1")?);
    Ok(())
}

#[test]
fn a_schedule_that_is_not_a_cron_expression_is_refused_before_anything_runs() {
    let refused = invocation(&["schedule", "v", "p", "w", "j", "every tuesday"]);

    let message = refused.err().map(|error| error.to_string()).unwrap_or_default();
    assert!(
        message.contains("every tuesday") && message.contains("cron"),
        "{message}"
    );
}

#[test]
fn a_schedule_command_takes_an_expression_or_clear_and_never_both_or_neither() {
    assert!(invocation(&["schedule", "v", "p", "w", "j"]).is_err());
    assert!(invocation(&["schedule", "v", "p", "w", "j", "0 3 * * *", "--clear"]).is_err());
}

#[test]
fn a_suspension_needs_a_reason_and_an_empty_one_is_refused() {
    assert!(parse(&["suspend", "v", "p", "w", "j"]).is_err(), "--reason is required");

    let empty = invocation(&["suspend", "v", "p", "w", "j", "--reason", "  "]);

    let message = empty.err().map(|error| error.to_string()).unwrap_or_default();
    assert!(message.contains("reason"), "{message}");
}

#[test]
fn a_dry_run_command_names_the_job_and_an_optional_scenario() -> Result<(), Box<dyn Error>> {
    let JobsAction::DryRun { job, scenario, .. } = parse(&[
        "dry-run",
        "--shared",
        "acme",
        "sync",
        "nightly",
        "--scenario",
        "offline",
    ])?
    else {
        return Err("expected a dry run".into());
    };

    assert!(job.shared);
    assert_eq!(job.target, ["acme", "sync", "nightly"]);
    assert_eq!(scenario.as_deref(), Some("offline"));
    Ok(())
}

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn format_accepts_text_and_json_and_refuses_anything_else() {
    for format in ["text", "json"] {
        for arguments in [
            vec!["list", "--shared", "--format", format],
            vec!["show", "--shared", "acme", "sync", "nightly", "--format", format],
            vec![
                "create", "--shared", "acme", "sync", "nightly", "--file", "-", "--format", format,
            ],
            vec![
                "replace",
                "--shared",
                "acme",
                "sync",
                "nightly",
                "--file",
                "-",
                "--expected-hash",
                HASH,
                "--format",
                format,
            ],
        ] {
            assert!(parse(&arguments).is_ok(), "{arguments:?}");
        }
    }

    for refused in ["yaml", "JSON", "", "text,json"] {
        for arguments in [
            vec!["list", "--shared", "--format", refused],
            vec!["show", "--shared", "acme", "sync", "nightly", "--format", refused],
        ] {
            assert!(
                parse(&arguments).is_err(),
                "--format {refused:?} must be refused: {arguments:?}"
            );
        }
    }
}

#[test]
fn an_expected_hash_that_is_not_sha256_hex_is_a_usage_error() {
    let with = |hash: &str| {
        parse(&[
            "replace",
            "--shared",
            "acme",
            "sync",
            "nightly",
            "--file",
            "-",
            "--expected-hash",
            hash,
        ])
    };

    assert!(with(HASH).is_ok());
    for malformed in [
        "",
        "not-a-hash",
        "0123",
        &HASH[..63],
        &format!("{HASH}0"),
        &format!("g{}", &HASH[1..]),
    ] {
        let refused = with(malformed);
        assert!(refused.is_err(), "{malformed:?} must be a usage error");
        assert_eq!(
            refused.err().map(|error| error.kind()),
            Some(clap::error::ErrorKind::ValueValidation),
            "{malformed:?}"
        );
    }
    assert!(
        parse(&["replace", "--shared", "acme", "sync", "nightly", "--file", "-"]).is_err(),
        "--expected-hash is required"
    );
}

#[test]
fn the_authoring_commands_apply_to_a_vault_job_or_with_the_shared_flag_to_a_shared_one() {
    assert!(parse(&["create", "va", "sync", "nightly", "--file", "-"]).is_ok());
    assert!(parse(&["show", "va", "sync", "nightly"]).is_ok());
    assert!(
        parse(&[
            "replace",
            "va",
            "sync",
            "nightly",
            "--file",
            "-",
            "--expected-hash",
            HASH
        ])
        .is_ok()
    );
    assert!(parse(&["show", "--shared", "acme", "sync", "nightly"]).is_ok());
    assert!(parse(&["list", "va"]).is_ok());
    assert!(parse(&["list", "--shared"]).is_ok());
}

#[test]
fn a_list_names_a_vault_or_the_host_and_never_both_or_neither() {
    assert!(parse(&["list"]).is_err());
    assert!(parse(&["list", "va", "--shared"]).is_err());
}

fn json(report: &AuthoringReport) -> Result<serde_json::Value, Box<dyn Error>> {
    let printed = render(report, OutputFormat::Json);
    Ok(serde_json::from_str(&printed).map_err(|error| format!("{error}: {printed:?}"))?)
}

fn key() -> contextos_web::shared_claim::SharedJobKey {
    contextos_web::shared_claim::SharedJobKey {
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    }
}

#[test]
fn a_listing_renders_one_json_document_with_a_row_per_job() -> Result<(), Box<dyn Error>> {
    let hash = ContentHash::try_from(HASH)?;
    let listed = json(&AuthoringReport::Listed(
        Store::Shared,
        vec![
            JobListing {
                key: key(),
                file: None,
                name: Some("Nightly".to_owned()),
                lifecycle: JobLifecycle::Draft,
                schedule: None,
                ready: Some(true),
                version: None,
                release_kind: None,
                predecessor: None,
                successor: None,
                replaced_by: None,
                hash: Some(hash.clone()),
                problems: Vec::new(),
                suspension: None,
            },
            JobListing {
                key: contextos_web::shared_claim::SharedJobKey {
                    job: "twin".to_owned(),
                    ..key()
                },
                file: Some("twin-a".to_owned()),
                name: None,
                lifecycle: JobLifecycle::Draft,
                schedule: Some("0 3 * * *".to_owned()),
                ready: None,
                version: Some(semver::Version::parse("1.2.0")?),
                release_kind: Some(contextos_workflows::ReleaseKind::Future),
                predecessor: None,
                successor: None,
                replaced_by: None,
                hash: None,
                problems: vec![ListingProblem {
                    code: "workflow/job-key-taken".to_owned(),
                    message: "shared".to_owned(),
                }],
                suspension: None,
            },
        ],
    ))?;
    let rows = listed["jobs"].as_array().ok_or("jobs is not an array")?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["job"], "nightly");
    assert_eq!(rows[0]["file"], serde_json::Value::Null);
    assert_eq!(rows[0]["name"], "Nightly");
    assert_eq!(rows[0]["state"], "draft");
    assert_eq!(rows[0]["ready"], true);
    assert_eq!(rows[0]["hash"], HASH);
    assert_eq!(rows[0]["problems"], serde_json::json!([]));
    assert_eq!(rows[1]["file"], "twin-a");
    assert_eq!(rows[1]["schedule"], "0 3 * * *");
    assert_eq!(rows[1]["version"], "1.2.0");
    assert_eq!(rows[1]["release_kind"], "future");
    assert_eq!(rows[1]["hash"], serde_json::Value::Null);
    assert_eq!(rows[1]["problems"][0]["code"], "workflow/job-key-taken");
    Ok(())
}

#[test]
fn each_authoring_outcome_renders_one_json_document_with_its_stable_fields() -> Result<(), Box<dyn Error>> {
    let hash = ContentHash::try_from(HASH)?;
    for (outcome, name) in [
        (AuthoredOutcome::Created, "created"),
        (AuthoredOutcome::Replaced, "replaced"),
        (AuthoredOutcome::Unchanged, "unchanged"),
    ] {
        let report = AuthoringReport::Authored(
            Store::Shared,
            Authored {
                outcome,
                key: key(),
                hash: hash.clone(),
                warnings: vec![AuthoringWarning::PackageUnavailable {
                    package: "acme".to_owned(),
                    workflow: "sync".to_owned(),
                }],
            },
        );
        let document = json(&report)?;
        assert_eq!(document["outcome"], name);
        assert_eq!(document["package"], "acme");
        assert_eq!(document["workflow"], "sync");
        assert_eq!(document["job"], "nightly");
        assert_eq!(document["hash"], HASH);
        assert_eq!(document["warnings"][0]["code"], "workflow/package-unavailable");
        assert!(document["warnings"][0]["message"].is_string());
    }

    let shown = json(&AuthoringReport::Shown(
        Store::Shared,
        ShownJob {
            key: key(),
            content: "name = \"Nightly\"\r\n".to_owned(),
            hash: hash.clone(),
        },
    ))?;
    assert_eq!(shown["job"], "nightly");
    assert_eq!(shown["hash"], HASH);
    assert_eq!(shown["content"], "name = \"Nightly\"\r\n");

    let empty = json(&AuthoringReport::Listed(Store::Shared, Vec::new()))?;
    assert_eq!(empty["jobs"], serde_json::json!([]));

    let refused = json(&AuthoringReport::Refused(AuthoringError::JobFileInvalid {
        cause: "validation/artefact-invalid",
        message: "a job file cannot carry a schedule".to_owned(),
    }))?;
    assert_eq!(refused["error"]["code"], "workflow/job-file-invalid");
    assert_eq!(refused["error"]["cause"], "validation/artefact-invalid");
    assert!(refused["error"]["message"].is_string());
    let plain = json(&AuthoringReport::Refused(AuthoringError::JobExists {
        key: "acme sync/nightly".to_owned(),
    }))?;
    assert_eq!(plain["error"]["code"], "workflow/job-exists");
    assert!(plain["error"].get("cause").is_none_or(serde_json::Value::is_null));
    Ok(())
}

#[test]
fn run_and_dry_run_accept_format_and_repeated_input_for_vault_and_shared_jobs() -> Result<(), Box<dyn Error>> {
    for shared in [false, true] {
        let mut run = vec![
            "run", "acme", "sync", "nightly", "--format", "json", "--input", "a=1", "--input", "b=two",
        ];
        let mut dry_run = vec![
            "dry-run", "acme", "sync", "nightly", "--format", "json", "--input", "a=1", "--input", "b=two",
        ];
        if shared {
            run.push("--shared");
            dry_run.push("--shared");
        }

        let JobsAction::Run { inputs, format, .. } = parse(&run)? else {
            return Err("`run` did not parse as a run".into());
        };
        assert_eq!(
            inputs,
            ["a=1", "b=two"],
            "shared={shared}: each --input is one override, in order"
        );
        assert_eq!(format, OutputFormat::Json, "shared={shared}");
        let JobsAction::DryRun {
            inputs, format, job, ..
        } = parse(&dry_run)?
        else {
            return Err("`dry-run` did not parse as a dry run".into());
        };
        assert_eq!(inputs, ["a=1", "b=two"], "shared={shared}");
        assert_eq!(format, OutputFormat::Json, "shared={shared}");
        assert_eq!(job.shared, shared);
    }
    Ok(())
}

#[test]
fn a_scenario_without_dry_run_is_a_usage_error_for_jobs_run() {
    let parsed = parse(&["run", "acme", "sync", "nightly", "--scenario", "offline"]);

    assert!(
        parsed.as_ref().is_err_and(|error| error.exit_code() == 2),
        "`--scenario` needs `--dry-run`, and the refusal is clap's own usage error: {parsed:?}"
    );
    assert!(parse(&["run", "acme", "sync", "nightly", "--dry-run", "--scenario", "offline"]).is_ok());
}

#[test]
fn a_schedule_command_prints_text_unless_json_is_asked_for() -> Result<(), Box<dyn Error>> {
    let plain = invocation(&["schedule", "harbour", "ops", "sync", "nightly", "0 3 * * *"])?;
    let json = invocation(&[
        "schedule",
        "harbour",
        "ops",
        "sync",
        "nightly",
        "0 3 * * *",
        "--format",
        "json",
    ])?;

    assert_eq!(plain.format, OutputFormat::Text);
    assert_eq!(json.format, OutputFormat::Json);
    assert_eq!(json.job.target, ["harbour", "ops", "sync", "nightly"]);
    Ok(())
}
