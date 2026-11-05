use std::fmt::Write as _;
use std::path::Path;

use tempfile::tempdir;

use super::*;
use crate::release_fixture::write_whole_release;

fn write(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)
}

/// Installs `package` at `version` with the workflows `slugs`, each with the entry `{slug}.rhai`.
fn install(store: &Path, package: &str, version: &str, slugs: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let mut body = String::new();
    let mut files: Vec<(String, &[u8])> = Vec::new();
    for slug in slugs {
        let _ = write!(body, "\n[[workflow]]\nslug = \"{slug}\"\nentry = \"{slug}.rhai\"\n");
        files.push((format!("{slug}.rhai"), b"()"));
    }
    let files: Vec<(&str, &[u8])> = files.iter().map(|(path, bytes)| (path.as_str(), *bytes)).collect();
    write_whole_release(&store.join(package).join(version), (package, version), &body, &files)?;
    Ok(())
}

fn job(jobs: &Path, package: &str, workflow: &str, file: &str, text: &str) -> std::io::Result<()> {
    write(
        &jobs
            .join(package)
            .join(workflow)
            .join("jobs")
            .join(format!("{file}.toml")),
        text,
    )
}

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn a_host_with_no_packages_and_no_jobs_has_an_empty_index() -> TestResult {
    let root = tempdir()?;

    let index = index_shared(&root.path().join("packages"), &root.path().join("jobs"))?;

    assert!(index.is_empty());
    Ok(())
}

#[test]
fn an_installed_workflow_is_listed_with_no_jobs_and_the_newest_release_it_comes_from() -> TestResult {
    let root = tempdir()?;
    let (store, jobs) = (root.path().join("packages"), root.path().join("jobs"));
    install(&store, "acme", "1.0.0", &["sync"])?;
    install(&store, "acme", "1.2.0", &["sync", "report"])?;

    let index = index_shared(&store, &jobs)?;

    let slugs: Vec<(&str, &str)> = index.iter().map(|w| (w.package.as_str(), w.slug.as_str())).collect();
    assert_eq!(slugs, [("acme", "report"), ("acme", "sync")]);
    assert!(index.iter().all(|workflow| workflow.jobs.is_empty()));
    assert_eq!(
        index[0].release.as_ref().map(ToString::to_string),
        Some("1.2.0".to_owned())
    );
    Ok(())
}

#[test]
fn a_job_is_listed_under_its_workflow_and_its_package() -> TestResult {
    let root = tempdir()?;
    let (store, jobs) = (root.path().join("packages"), root.path().join("jobs"));
    install(&store, "acme", "1.0.0", &["sync"])?;
    install(&store, "globex", "1.0.0", &["sync"])?;
    job(
        &jobs,
        "acme",
        "sync",
        "nightly",
        "name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n",
    )?;
    job(
        &jobs,
        "globex",
        "sync",
        "weekly",
        "name = \"Weekly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n",
    )?;

    let index = index_shared(&store, &jobs)?;

    assert_eq!(
        index.len(),
        2,
        "the same workflow slug in two packages is two workflows"
    );
    assert_eq!(index[0].jobs[0].slug, "nightly");
    assert_eq!(index[1].jobs[0].slug, "weekly");
    assert!(index[0].jobs[0].loaded.is_some());
    Ok(())
}

#[test]
fn a_job_whose_workflow_is_not_installed_is_still_listed() -> TestResult {
    let root = tempdir()?;
    let (store, jobs) = (root.path().join("packages"), root.path().join("jobs"));
    job(
        &jobs,
        "gone",
        "sync",
        "nightly",
        "name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n",
    )?;

    let index = index_shared(&store, &jobs)?;

    assert_eq!(index.len(), 1);
    assert!(index[0].release.is_none(), "nothing installed provides it");
    assert_eq!(index[0].jobs[0].slug, "nightly");
    Ok(())
}

#[test]
fn a_job_file_that_does_not_load_is_listed_unloaded() -> TestResult {
    let root = tempdir()?;
    let (store, jobs) = (root.path().join("packages"), root.path().join("jobs"));
    install(&store, "acme", "1.0.0", &["sync"])?;
    job(
        &jobs,
        "acme",
        "sync",
        "broken",
        "name = \"Broken\"\nschedule = \"0 3 * * *\"\n",
    )?;

    let index = index_shared(&store, &jobs)?;

    assert_eq!(index[0].jobs[0].slug, "broken");
    assert!(
        index[0].jobs[0].loaded.is_none(),
        "a schedule key is not allowed in a job file"
    );
    Ok(())
}

#[test]
fn a_job_is_listed_under_the_slug_its_file_names() -> TestResult {
    let root = tempdir()?;
    let (store, jobs) = (root.path().join("packages"), root.path().join("jobs"));
    install(&store, "acme", "1.0.0", &["sync"])?;
    job(
        &jobs,
        "acme",
        "sync",
        "some-file",
        "name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\nslug = \"nightly\"\n",
    )?;

    let index = index_shared(&store, &jobs)?;

    assert_eq!(index[0].jobs[0].slug, "nightly");
    Ok(())
}

#[test]
fn two_files_that_name_one_job_are_both_set_aside() -> TestResult {
    let root = tempdir()?;
    let (store, jobs) = (root.path().join("packages"), root.path().join("jobs"));
    install(&store, "acme", "1.0.0", &["sync"])?;
    job(
        &jobs,
        "acme",
        "sync",
        "first",
        "name = \"A\"\ntimeout_secs = 60\nversion = \"1.0.0\"\nslug = \"same\"\n",
    )?;
    job(
        &jobs,
        "acme",
        "sync",
        "second",
        "name = \"B\"\ntimeout_secs = 60\nversion = \"1.0.0\"\nslug = \"same\"\n",
    )?;

    let index = index_shared(&store, &jobs)?;

    assert!(index[0].jobs.is_empty(), "neither is tracked until one is renamed");
    Ok(())
}
