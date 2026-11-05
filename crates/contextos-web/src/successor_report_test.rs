use super::*;

use serde_json::Value;

use crate::shared_job_authoring::AuthoringError;
use crate::successor_interface::{FieldChange, FieldSide};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn v(text: &str) -> Version {
    Version::parse(text).unwrap_or_else(|_| Version::new(0, 0, 0))
}

fn made(predecessor: &str, key: &str, (from, to): (&str, &str), takeup: Takeup) -> MadeSuccessor {
    MadeSuccessor {
        predecessor: predecessor.to_owned(),
        key: key.to_owned(),
        from: v(from),
        to: v(to),
        step: ReleaseStep::from((&v(from), &v(to))),
        hash: ContentHash::from([7_u8; 32]),
        takeup,
    }
}

fn report(made: Vec<MadeSuccessor>, refused: Vec<SuccessorRefused>) -> SuccessorReport {
    SuccessorReport {
        store: "va".to_owned(),
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        to: Some(v("1.1.0")),
        release: Some(ReleaseKind::Future),
        made,
        refused,
    }
}

fn json(report: &SuccessorReport) -> Result<Value, serde_json::Error> {
    serde_json::from_str(&SuccessorJson(report).to_string())
}

fn since_changes() -> Takeup {
    Takeup::Changes(vec![InterfaceChange {
        side: FieldSide::Input,
        name: "since".to_owned(),
        change: FieldChange::Added { required: true },
    }])
}

#[test]
fn text_names_each_successor_and_says_a_small_step_carries_over() {
    let text = SuccessorText(&report(
        vec![made("nightly", "nightly-v1-1-0", ("1.0.0", "1.1.0"), Takeup::Nothing)],
        Vec::new(),
    ))
    .to_string();
    assert!(text.contains("va/acme/sync"), "{text}");
    assert!(text.contains("1.1.0 (future)"), "{text}");
    assert!(
        text.contains("nightly to nightly-v1-1-0, 1.0.0 to 1.1.0, minor step: carries over without parameter changes"),
        "{text}"
    );
}

#[test]
fn text_lists_what_each_job_must_take_up_after_a_major_step() {
    let text = SuccessorText(&report(
        vec![made("nightly", "nightly-v2-0-0", ("1.0.0", "2.0.0"), since_changes())],
        Vec::new(),
    ))
    .to_string();
    assert!(
        text.contains("major step: take up these changes while it is draft"),
        "{text}"
    );
    assert!(text.contains("input since added, and required"), "{text}");
    assert!(!text.contains("carries over"), "{text}");
}

#[test]
fn text_says_when_the_changes_of_a_major_step_cannot_be_listed() {
    let text = SuccessorText(&report(
        vec![made("nightly", "nightly-v2-0-0", ("1.0.0", "2.0.0"), Takeup::Unknown)],
        Vec::new(),
    ))
    .to_string();
    assert!(text.contains("the store's ledger does not say what changed"), "{text}");
}

#[test]
fn text_says_a_major_step_carries_over_when_no_input_or_output_changed() {
    let text = SuccessorText(&report(
        vec![made("nightly", "nightly-v2-0-0", ("1.0.0", "2.0.0"), Takeup::Nothing)],
        Vec::new(),
    ))
    .to_string();
    assert!(
        text.contains("major step: carries over without parameter changes"),
        "{text}"
    );
}

#[test]
fn text_with_nothing_made_says_so() {
    let text = SuccessorText(&report(Vec::new(), Vec::new())).to_string();
    assert!(text.contains("no job to make a successor for"), "{text}");
}

#[test]
fn text_for_refusals_names_each_job_and_says_nothing_was_written() {
    let text = SuccessorText(&report(
        Vec::new(),
        vec![
            SuccessorRefused {
                job: Some("a".to_owned()),
                error: AuthoringError::JobExists {
                    key: "acme/sync/a-v1-1-0".to_owned(),
                },
            },
            SuccessorRefused {
                job: None,
                error: AuthoringError::StoreBusy,
            },
        ],
    ))
    .to_string();
    assert!(text.contains("refused"), "{text}");
    assert!(text.contains("a: workflow/job-exists"), "{text}");
    assert!(text.contains("packages/store-locked"), "{text}");
    assert!(text.contains("nothing was written"), "{text}");
}

#[test]
fn json_describes_each_successor() -> TestResult {
    let document = json(&report(
        vec![
            made("a", "a-v1-1-0", ("1.0.0", "1.1.0"), Takeup::Nothing),
            made("b", "b-v1-1-0", ("1.0.0", "1.1.0"), since_changes()),
        ],
        Vec::new(),
    ))?;
    assert_eq!(document["store"], "va");
    assert_eq!(document["package"], "acme");
    assert_eq!(document["workflow"], "sync");
    assert_eq!(document["release"]["version"], "1.1.0");
    assert_eq!(document["release"]["kind"], "future");
    let first = &document["successors"][0];
    assert_eq!(first["predecessor"], "a");
    assert_eq!(first["job"], "a-v1-1-0");
    assert_eq!(first["from"], "1.0.0");
    assert_eq!(first["step"], "minor");
    assert_eq!(first["carries_over"], true);
    assert_eq!(first["interface_changes"], serde_json::json!([]));
    assert_eq!(first["hash"].as_str().map(str::len), Some(64));
    assert!(document.get("error").is_none());
    Ok(())
}

#[test]
fn json_lists_the_interface_changes_of_a_major_step() -> TestResult {
    let document = json(&report(
        vec![made("a", "a-v2-0-0", ("1.0.0", "2.0.0"), since_changes())],
        Vec::new(),
    ))?;
    let first = &document["successors"][0];
    assert_eq!(first["step"], "major");
    assert_eq!(first["carries_over"], false);
    assert_eq!(first["interface_changes"][0]["side"], "input");
    assert_eq!(first["interface_changes"][0]["name"], "since");
    assert_eq!(first["interface_changes"][0]["change"], "added");
    assert_eq!(first["interface_changes"][0]["required"], true);
    assert_eq!(first["interface_changes"][0]["text"], "input since added, and required");
    Ok(())
}

#[test]
fn json_says_when_a_major_steps_changes_are_not_known() -> TestResult {
    let document = json(&report(
        vec![made("a", "a-v2-0-0", ("1.0.0", "2.0.0"), Takeup::Unknown)],
        Vec::new(),
    ))?;
    let first = &document["successors"][0];
    assert_eq!(first["carries_over"], false);
    assert_eq!(first["interface_changes"], Value::Null);
    Ok(())
}

#[test]
fn json_for_one_refusal_is_the_error_document_every_command_gives() -> TestResult {
    let document = json(&report(
        Vec::new(),
        vec![SuccessorRefused {
            job: Some("a".to_owned()),
            error: AuthoringError::JobExists {
                key: "acme/sync/a-v1-1-0".to_owned(),
            },
        }],
    ))?;
    assert_eq!(document["error"]["code"], "workflow/job-exists");
    assert!(
        document["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("already has a file"))
    );
    assert_eq!(document["refusals"][0]["job"], "a");
    assert_eq!(document["refusals"][0]["code"], "workflow/job-exists");
    assert!(document.get("successors").is_none());
    Ok(())
}

#[test]
fn json_for_several_refusals_lists_each_and_gives_the_shared_code_when_there_is_one() -> TestResult {
    let refusal = |job: &str, error| SuccessorRefused {
        job: Some(job.to_owned()),
        error,
    };
    let same = json(&report(
        Vec::new(),
        vec![
            refusal("a", AuthoringError::JobExists { key: "x".to_owned() }),
            refusal("b", AuthoringError::JobExists { key: "y".to_owned() }),
        ],
    ))?;
    assert_eq!(same["error"]["code"], "workflow/job-exists");
    assert_eq!(same["refusals"].as_array().map(Vec::len), Some(2));
    let mixed = json(&report(
        Vec::new(),
        vec![
            refusal("a", AuthoringError::JobExists { key: "x".to_owned() }),
            refusal(
                "b",
                AuthoringError::JobKeyTaken {
                    key: "y".to_owned(),
                    files: "z.toml".to_owned(),
                },
            ),
        ],
    ))?;
    assert_eq!(mixed["error"]["code"], "workflow/transition-refused");
    assert!(mixed["error"]["message"].as_str().is_some_and(|m| m.contains("2 jobs")));
    Ok(())
}

#[test]
fn json_keeps_successors_written_before_a_write_failed() -> TestResult {
    let document = json(&report(
        vec![made("a", "a-v1-1-0", ("1.0.0", "1.1.0"), Takeup::Nothing)],
        vec![SuccessorRefused {
            job: Some("b".to_owned()),
            error: AuthoringError::JobStoreUnavailable {
                action: "the job file could not be written",
                kind: None,
            },
        }],
    ))?;
    assert_eq!(document["error"]["code"], "workflow/job-store-unavailable");
    assert_eq!(document["successors"][0]["job"], "a-v1-1-0");
    Ok(())
}
