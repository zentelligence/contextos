use contextos_workflows::JobLifecycle;

use super::*;
use crate::job_runs::ScriptStarted;
use crate::shared_claim::SharedJobKey;

fn row(job: &str, predecessor: Option<&str>, successor: Option<&str>) -> JobListing {
    JobListing {
        key: SharedJobKey {
            package: "ops".to_owned(),
            workflow: "sync".to_owned(),
            job: job.to_owned(),
        },
        file: None,
        name: None,
        lifecycle: JobLifecycle::Enabled,
        schedule: None,
        ready: None,
        version: None,
        release_kind: None,
        predecessor: predecessor.map(str::to_owned),
        successor: successor.map(str::to_owned),
        replaced_by: None,
        hash: None,
        problems: Vec::new(),
        suspension: None,
    }
}

/// A retired job whose retirement record names `replaced_by`, with no job left naming it as a predecessor in the listing.
fn retired(job: &str, predecessor: Option<&str>, replaced_by: &str) -> JobListing {
    let mut listing = row(job, predecessor, None);
    listing.lifecycle = JobLifecycle::Retired;
    listing.replaced_by = Some(replaced_by.to_owned());
    listing
}

fn chain_of(job: &str, listings: &[JobListing]) -> Vec<String> {
    JobChain::from(ChainInput {
        package: "ops",
        workflow: "sync",
        job,
        listings,
    })
    .members()
    .to_vec()
}

fn run(job: &str, number: u64, started: &str) -> RunSummary {
    RunSummary {
        path: String::new(),
        run_id: format!("R-{number}"),
        job: job.to_owned(),
        release: String::new(),
        dry_run: false,
        trigger: "scheduled".to_owned(),
        status: "succeeded".to_owned(),
        started: started.to_owned(),
        completed: String::new(),
        error: String::new(),
        code: None,
        script_started: ScriptStarted::Absent,
    }
}

#[test]
fn a_job_with_no_predecessor_or_successor_is_a_chain_of_itself() {
    assert_eq!(chain_of("nightly", &[row("nightly", None, None)]), ["nightly"]);
}

#[test]
fn a_chain_is_the_same_from_any_of_its_jobs_oldest_first() {
    let listings = [
        row("nightly", None, Some("nightly-v2")),
        row("nightly-v2", Some("nightly"), Some("nightly-v3")),
        row("nightly-v3", Some("nightly-v2"), None),
    ];

    for from in ["nightly", "nightly-v2", "nightly-v3"] {
        assert_eq!(
            chain_of(from, &listings),
            ["nightly", "nightly-v2", "nightly-v3"],
            "{from}"
        );
    }
}

#[test]
fn a_chain_whose_earlier_jobs_were_retired_is_the_same_from_every_job() {
    let listings = [
        retired("a", None, "b"),
        // The standing successor of a retired job is the next job that names it, so `b` has none of its own.
        retired("b", Some("a"), "c"),
        row("c", Some("b"), None),
    ];

    for from in ["a", "b", "c"] {
        assert_eq!(chain_of(from, &listings), ["a", "b", "c"], "{from}");
    }
}

#[test]
fn a_retired_job_whose_file_is_gone_still_leads_to_the_job_that_replaced_it() {
    let mut gone = retired("a", None, "b");
    gone.file = None;
    gone.predecessor = None;

    assert_eq!(chain_of("a", &[gone, row("b", Some("a"), None)]), ["a", "b"]);
}

#[test]
fn a_predecessor_whose_file_is_gone_is_still_in_the_chain() {
    let listings = [row("nightly-v2", Some("nightly"), None)];

    assert_eq!(chain_of("nightly-v2", &listings), ["nightly", "nightly-v2"]);
}

#[test]
fn another_workflows_job_of_the_same_key_is_not_in_the_chain() {
    let mut other = row("nightly-v2", Some("nightly"), None);
    other.key.workflow = "elsewhere".to_owned();

    assert_eq!(chain_of("nightly", &[row("nightly", None, None), other]), ["nightly"]);
}

#[test]
fn links_that_loop_end_the_chain_instead_of_following_forever() {
    let listings = [row("a", Some("b"), Some("b")), row("b", Some("a"), Some("a"))];

    assert_eq!(chain_of("a", &listings), ["b", "a"]);
}

#[test]
fn runs_of_several_jobs_are_merged_newest_first_and_cut_to_the_limit() {
    let merged = merge_runs(
        vec![
            vec![
                run("nightly", 2, "2026-11-02T03:00:00Z"),
                run("nightly", 1, "2026-11-01T03:00:00Z"),
            ],
            vec![
                run("nightly-v2", 2, "2026-11-04T03:00:00Z"),
                run("nightly-v2", 1, "2026-11-03T03:00:00Z"),
            ],
        ],
        3,
    );

    let order: Vec<(&str, &str)> = merged
        .iter()
        .map(|run| (run.job.as_str(), run.run_id.as_str()))
        .collect();
    assert_eq!(
        order,
        [("nightly-v2", "R-2"), ("nightly-v2", "R-1"), ("nightly", "R-2")]
    );
}

#[test]
fn runs_started_together_are_ordered_by_number_then_job() {
    let merged = merge_runs(
        vec![
            vec![run("b", 1, "2026-11-02T03:00:00Z")],
            vec![run("a", 1, "2026-11-02T03:00:00Z"), run("a", 2, "2026-11-02T03:00:00Z")],
        ],
        10,
    );

    let order: Vec<(&str, &str)> = merged
        .iter()
        .map(|run| (run.job.as_str(), run.run_id.as_str()))
        .collect();
    assert_eq!(order, [("a", "R-2"), ("a", "R-1"), ("b", "R-1")]);
}
