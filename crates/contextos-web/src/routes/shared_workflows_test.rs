use std::path::Path;

use contextos_workflows::{
    Actor, JobFingerprint, JobSchedule, ReleaseFingerprintInput, StoreKind, SuspensionReason, Transition,
};
use tempfile::tempdir;
use time::macros::datetime;

use super::*;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn write(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)
}

fn roots(base: &Path) -> SharedRoots {
    SharedRoots {
        store_root: base.join("packages"),
        jobs_root: base.join("jobs"),
        runs_root: base.join("runs"),
        state_root: base.join("state"),
        claims_root: base.join("claims"),
    }
}

fn install(roots: &SharedRoots, package: &str, version: &str, slug: &str) -> std::io::Result<()> {
    let dir = roots.store_root.join(package).join(version);
    write(
        &dir.join("manifest.toml"),
        &format!(
            "name = \"{package}\"\nversion = \"{version}\"\ndigest = \"{DIGEST}\"\n\n[[workflow]]\nslug = \"{slug}\"\nentry = \"{slug}.rhai\"\n"
        ),
    )?;
    write(&dir.join(format!("{slug}.rhai")), "()")?;
    crate::release_fixture::seal_io(&dir)
}

fn job(roots: &SharedRoots, package: &str, workflow: &str, file: &str, text: &str) -> std::io::Result<()> {
    write(
        &roots
            .jobs_root
            .join(package)
            .join(workflow)
            .join("jobs")
            .join(format!("{file}.toml")),
        text,
    )
}

fn find<'a>(all: &'a [SharedEntry], package: &str, slug: &str) -> Result<&'a SharedEntry, String> {
    all.iter()
        .find(|shared| shared.package == package && shared.slug == slug)
        .ok_or_else(|| format!("no workflow {package}/{slug}"))
}

fn suspend(roots: &SharedRoots, job: &str) -> TestResult {
    let version = JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Host,
        package: "acme",
        release: "1.0.0",
        digest: "sha256:0",
        job_toml: "",
        release_manifest: "",
        libraries: &[],
    });
    let schedule = JobSchedule::try_from("0 3 * * *")?;
    let reason = SuspensionReason::try_from("held")?;
    let at = datetime!(2026-10-01 03:00:00 UTC);
    SharedStateStore::new(roots.state_root.clone()).update(
        |jobs| -> Result<(), contextos_workflows::TransitionError> {
            let mut step = |actor, transition: Transition| {
                jobs.transition("acme", "sync", job, actor, &transition, at).map(|_| ())
            };
            step(Actor::Validator, Transition::Validated(version.clone()))?;
            step(Actor::DryRun, Transition::DryRunCompleted(version.clone()))?;
            step(Actor::Operator, Transition::Enable(schedule, version))?;
            step(
                Actor::Operator,
                Transition::Suspend(contextos_workflows::Suspension::operator(reason)),
            )
        },
    )??;
    Ok(())
}

#[test]
fn a_job_is_a_card_under_its_shared_workflow_with_a_shared_url_and_no_favourite_or_run_button() -> TestResult {
    let base = tempdir()?;
    let roots = roots(base.path());
    install(&roots, "acme", "1.0.0", "sync")?;
    job(
        &roots,
        "acme",
        "sync",
        "nightly",
        "name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n",
    )?;

    let all = entries(&roots, StateFilter::All)?;

    let workflow = find(&all, "acme", "sync")?;
    assert_eq!(workflow.entry.name, "acme / sync");
    assert_eq!(workflow.entry.href, "/shared/workflows/acme/sync");
    assert!(workflow.entry.problem.is_none());
    let card = &workflow.entry.jobs[0];
    assert_eq!(card.name, "Nightly");
    assert_eq!(card.href, "/shared/workflows/acme/sync/jobs/nightly");
    assert_eq!(card.lifecycle_label, "draft");
    assert!(card.attention && card.registers);
    assert!(
        card.favourite_toggle_html.is_empty(),
        "`card.favourite_toggle_html` should be empty, got {:?}",
        card.favourite_toggle_html
    );
    Ok(())
}

#[test]
fn a_workflow_no_installed_package_provides_is_flagged_not_installed() -> TestResult {
    let base = tempdir()?;
    let roots = roots(base.path());
    job(
        &roots,
        "gone",
        "sync",
        "nightly",
        "name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n",
    )?;

    let all = entries(&roots, StateFilter::All)?;

    let problem = find(&all, "gone", "sync")?
        .entry
        .problem
        .as_ref()
        .map(|problem| problem.badge);
    assert_eq!(problem, Some("Not installed"));
    Ok(())
}

#[test]
fn a_workflows_facts_name_its_package_release_and_script() -> TestResult {
    let base = tempdir()?;
    let roots = roots(base.path());
    install(&roots, "acme", "1.0.0", "sync")?;
    install(&roots, "acme", "1.3.0", "sync")?;

    let all = entries(&roots, StateFilter::All)?;

    let facts: Vec<(&str, &str)> = find(&all, "acme", "sync")?
        .entry
        .facts
        .iter()
        .map(|fact| (fact.label, fact.value.as_str()))
        .collect();
    assert_eq!(
        facts,
        [("Package", "acme"), ("Release", "1.3.0"), ("Script", "sync.rhai")]
    );
    Ok(())
}

#[test]
fn a_job_file_that_does_not_load_is_a_card_that_says_so() -> TestResult {
    let base = tempdir()?;
    let roots = roots(base.path());
    install(&roots, "acme", "1.0.0", "sync")?;
    job(
        &roots,
        "acme",
        "sync",
        "broken",
        "name = \"Broken\"\nschedule = \"0 3 * * *\"\n",
    )?;

    let all = entries(&roots, StateFilter::All)?;

    let card = &find(&all, "acme", "sync")?.entry.jobs[0];
    assert_eq!(card.name, "broken");
    assert!(!card.registers);
    Ok(())
}

#[test]
fn the_filter_keeps_only_the_jobs_it_admits() -> TestResult {
    let base = tempdir()?;
    let roots = roots(base.path());
    install(&roots, "acme", "1.0.0", "sync")?;
    job(
        &roots,
        "acme",
        "sync",
        "held",
        "name = \"Held\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n",
    )?;
    job(
        &roots,
        "acme",
        "sync",
        "fresh",
        "name = \"Fresh\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n",
    )?;
    suspend(&roots, "held")?;

    let suspended = entries(&roots, StateFilter::State(JobLifecycle::Suspended))?;
    let drafts = entries(&roots, StateFilter::State(JobLifecycle::Draft))?;
    let attention = entries(&roots, StateFilter::Attention)?;

    let names = |all: &[SharedEntry]| -> Result<Vec<String>, String> {
        Ok(find(all, "acme", "sync")?
            .entry
            .jobs
            .iter()
            .map(|job| job.name.clone())
            .collect())
    };
    assert_eq!(names(&suspended)?, ["Held"]);
    assert_eq!(names(&drafts)?, ["Fresh"]);
    assert_eq!(
        names(&attention)?,
        ["Fresh", "Held"],
        "drafts and suspended jobs, in slug order"
    );
    Ok(())
}

#[test]
fn a_state_file_that_does_not_parse_is_an_error_and_not_an_empty_list() -> TestResult {
    let base = tempdir()?;
    let roots = roots(base.path());
    install(&roots, "acme", "1.0.0", "sync")?;
    write(&roots.state_root.join("shared-jobs.toml"), "packages = 7\n")?;

    let refused = entries(&roots, StateFilter::All);

    assert!(refused.is_err());
    Ok(())
}

#[test]
fn a_url_component_that_could_leave_a_folder_names_no_job() {
    assert!(key_of("acme", "sync", "nightly").is_some());
    for (package, workflow, job) in [
        ("..", "sync", "nightly"),
        ("acme", "a/b", "nightly"),
        ("acme", "sync", ""),
    ] {
        assert!(key_of(package, workflow, job).is_none(), "{package}/{workflow}/{job}");
    }
}

#[test]
fn a_workflow_whose_package_name_cannot_be_part_of_a_url_is_left_out_and_not_linked_to_a_dead_page() -> TestResult {
    let base = tempdir()?;
    let roots = roots(base.path());
    install(&roots, "acme", "1.0.0", "sync")?;
    install(&roots, "Mixed_Case", "1.0.0", "sync")?;

    let all = entries(&roots, StateFilter::All)?;

    let packages: Vec<&str> = all.iter().map(|shared| shared.package.as_str()).collect();
    assert_eq!(packages, ["acme"]);
    Ok(())
}
