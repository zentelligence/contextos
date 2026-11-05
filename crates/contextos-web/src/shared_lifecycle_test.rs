use std::cell::RefCell;
use std::fs;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct RecordSteps(RefCell<Vec<FileRemoveStep>>);

impl FileRemoveObserver for RecordSteps {
    fn reached(&self, step: FileRemoveStep) {
        self.0.borrow_mut().push(step);
    }
}

fn nightly() -> SharedJobKey {
    SharedJobKey {
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    }
}

#[test]
fn removing_a_job_file_flushes_the_folder_that_held_it_once_the_entry_is_gone() -> TestResult {
    let root = tempfile::tempdir()?;
    let folder = root.path().join("acme/sync/jobs");
    fs::create_dir_all(&folder)?;
    fs::write(
        folder.join("nightly.toml"),
        b"name = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n",
    )?;
    let observer = RecordSteps(RefCell::new(Vec::new()));

    remove_job_file(root.path(), &nightly(), &observer)?;

    assert!(!folder.join("nightly.toml").exists());
    assert_eq!(
        *observer.0.borrow(),
        [FileRemoveStep::Removed, FileRemoveStep::ParentSynced],
        "the folder is flushed before the state is written, so the state never outlives the removal"
    );
    Ok(())
}

#[test]
fn a_job_with_no_file_removes_nothing_and_reports_no_steps() -> TestResult {
    let root = tempfile::tempdir()?;
    let observer = RecordSteps(RefCell::new(Vec::new()));

    remove_job_file(root.path(), &nightly(), &observer)?;

    assert!(observer.0.borrow().is_empty());
    Ok(())
}

/// Lists the job, then lets `swap` change what is on disk, as another process could between the listing and the
/// removal.
#[cfg(unix)]
fn remove_after(
    root: &std::path::Path,
    swap: impl Fn() -> std::io::Result<()>,
) -> Result<Result<(), SharedLifecycleError>, Box<dyn std::error::Error>> {
    let failure = RefCell::new(None);
    let removed = remove_job_file_after_listing(root, &nightly(), &NoRemoveSteps, &|| {
        if let Err(error) = swap() {
            *failure.borrow_mut() = Some(error);
        }
    });
    match failure.into_inner() {
        Some(error) => Err(error.into()),
        None => Ok(removed),
    }
}

#[cfg(unix)]
#[test]
fn a_package_workflow_jobs_folder_or_job_file_swapped_for_a_link_after_listing_is_not_removed_through_and_is_refused()
-> TestResult {
    use std::os::unix::fs::symlink;

    for swapped in ["package folder", "workflow folder", "jobs folder", "job file"] {
        let root = tempfile::tempdir()?;
        let jobs = root.path().join("jobs");
        let folder = jobs.join("acme/sync/jobs");
        fs::create_dir_all(&folder)?;
        fs::write(
            folder.join("nightly.toml"),
            b"name = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n",
        )?;
        // What the swapped-in link leads to has a job file of the same name, which must survive.
        let outside = root.path().join("outside");
        fs::create_dir_all(outside.join("sync/jobs"))?;
        fs::write(outside.join("sync/jobs/nightly.toml"), b"outside")?;
        fs::write(outside.join("nightly.toml"), b"outside")?;
        let outside_workflow = outside.join("sync");
        let workflow_folder = jobs.join("acme/sync");

        let result = remove_after(&jobs, || match swapped {
            "package folder" => {
                fs::remove_dir_all(jobs.join("acme"))?;
                symlink(&outside, jobs.join("acme"))
            }
            "workflow folder" => {
                fs::remove_dir_all(&workflow_folder)?;
                symlink(&outside_workflow, &workflow_folder)
            }
            "jobs folder" => {
                fs::remove_dir_all(&folder)?;
                symlink(outside_workflow.join("jobs"), &folder)
            }
            _ => {
                fs::remove_file(folder.join("nightly.toml"))?;
                symlink(outside.join("nightly.toml"), folder.join("nightly.toml"))
            }
        })?;

        let Err(error) = result else {
            return Err(format!("{swapped}: expected a refusal, got {result:?}").into());
        };
        assert_eq!(error.code(), "workflow/job-store-unavailable", "{swapped}: {error}");
        assert_eq!(
            fs::read(outside.join("sync/jobs/nightly.toml"))?,
            b"outside",
            "{swapped}"
        );
        assert_eq!(fs::read(outside.join("nightly.toml"))?, b"outside", "{swapped}");
        if swapped == "job file" {
            assert!(
                fs::symlink_metadata(folder.join("nightly.toml"))?
                    .file_type()
                    .is_symlink(),
                "{swapped}: the link is left for the operator"
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_job_file_that_stays_plain_after_listing_is_removed_as_before() -> TestResult {
    let root = tempfile::tempdir()?;
    let folder = root.path().join("acme/sync/jobs");
    fs::create_dir_all(&folder)?;
    fs::write(
        folder.join("nightly.toml"),
        b"name = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n",
    )?;

    let result = remove_after(root.path(), || Ok(()))?;

    assert!(result.is_ok(), "{result:?}");
    assert!(!folder.join("nightly.toml").exists());
    Ok(())
}

#[tokio::test]
async fn a_job_that_is_not_on_trial_has_nothing_to_settle_even_when_the_version_it_ran_is_unknown() -> TestResult {
    let folder = tempfile::tempdir()?;
    let state = SharedStateStore::new(folder.path().to_path_buf());
    let failed = RunOutcome::Failed(contextos_workflows::RunFailure {
        kind: None,
        message: "workflow/script-error: boom".to_owned(),
    });

    let settled = settle_shared_first_run(&state, &nightly(), None, &failed, Some(1), OffsetDateTime::UNIX_EPOCH).await;

    assert_eq!(settled, Settlement::NothingToSettle);
    assert!(!state.state_path().exists(), "nothing was written");
    Ok(())
}

fn registered_version() -> JobFingerprint {
    JobFingerprint::from(&contextos_workflows::ReleaseFingerprintInput {
        store: contextos_workflows::StoreKind::Host,
        package: "acme",
        release: "1.0.0",
        digest: "sha256:0",
        job_toml: "",
        release_manifest: "",
        libraries: &[],
    })
}

/// A host state holding one shared job, `registered` on release 1.0.0 with the identity its key was given.
fn host_state_with_a_registered_job() -> Result<(tempfile::TempDir, SharedStateStore), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let store = SharedStateStore::new(directory.path().to_path_buf());
    if let Some(parent) = store.state_path().parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        store.state_path(),
        format!(
            "[acme.sync.nightly]\nfavourite = false\nlifecycle = \"registered\"\nfingerprint = \"{}\"\nidentity = \"job-1\"\nrelease = \"1.0.0\"\n",
            registered_version().as_str()
        ),
    )?;
    Ok((directory, store))
}

fn enabling() -> Result<Transition, Box<dyn std::error::Error>> {
    Ok(Transition::Enable(
        contextos_workflows::JobSchedule::try_from("0 3 * * *")?,
        registered_version(),
    ))
}

#[tokio::test]
async fn enabling_a_shared_job_on_a_release_moves_the_packages_current_and_previous_in_the_same_write() -> TestResult {
    let (_directory, store) = host_state_with_a_registered_job()?;
    let key = nightly();
    let context = ReleaseContext {
        package: "acme".to_owned(),
        predecessor: None,
        installed: vec![semver::Version::new(0, 9, 0), semver::Version::new(1, 0, 0)],
        predecessor_running: false,
        named_release: None,
    };

    let applied = change_shared_lifecycle_in_release(
        &store,
        SharedJobChange {
            key: &key,
            actor: Actor::Operator,
            transition: enabling()?,
            now: time::macros::datetime!(2026-11-03 03:00:00 UTC),
        },
        Some(&context),
    )
    .await?;

    assert_eq!(applied, Applied::To(JobLifecycle::Enabled));
    let text = fs::read_to_string(store.state_path())?;
    assert!(text.contains("[acme.sync]"), "{text}");
    assert!(text.contains("current = \"1.0.0\""), "{text}");
    assert!(text.contains("[acme.sync.\"0.9.0\".retired]"), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_shared_job_on_a_release_is_not_enabled_without_the_release_rules_and_its_file_is_unchanged() -> TestResult {
    let (_directory, store) = host_state_with_a_registered_job()?;
    let before = fs::read_to_string(store.state_path())?;
    let key = nightly();

    let refused = change_shared_lifecycle(
        &store,
        SharedJobChange {
            key: &key,
            actor: Actor::Operator,
            transition: enabling()?,
            now: time::macros::datetime!(2026-11-03 03:00:00 UTC),
        },
    )
    .await;

    assert!(
        matches!(
            refused,
            Err(SharedLifecycleError::Refused {
                source: TransitionError::ReleaseRulesApply { .. }
            })
        ),
        "{refused:?}"
    );
    assert_eq!(fs::read_to_string(store.state_path())?, before);
    Ok(())
}

mod library_lock {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use contextos_workflows::{
        LibraryLock, LockedLibrary, PackageManifest, ReleaseFingerprintInput, StoreKind, ValidationRecord,
        ValidationReport, VersionBasis,
    };
    use semver::Version;
    use time::macros::datetime;

    use super::*;
    use crate::McpClientSet;
    use crate::release_fixture::write_whole_release;
    use crate::snapshot_hooks::{AFTER_READ, set_hook};

    const NOW: OffsetDateTime = datetime!(2026-10-04 03:00:00 UTC);

    fn library(store: &Path, version: &str) -> TestResult {
        write_whole_release(
            &store.join("acme-text").join(version),
            ("acme-text", version),
            "modules = [\"m.rhai\"]\nexports = [\"m.rhai\"]\n",
            &[("m.rhai", b"fn m() { 1 }")],
        )?;
        Ok(())
    }

    fn digest_of_library(store: &Path, version: &str) -> Result<String, Box<dyn std::error::Error>> {
        let text = fs::read_to_string(store.join("acme-text").join(version).join("manifest.toml"))?;
        Ok(PackageManifest::parse(&text)?.identity.digest.as_str().to_owned())
    }

    /// A store holding `acme` 1.0.0, which depends on `acme-text` 1.x, and `acme-text` 1.0.0 and 1.1.0, and a jobs root
    /// holding the ready job `nightly`.
    fn host_files(root: &Path) -> TestResult {
        let store = root.join("packages");
        write_whole_release(
            &store.join("acme").join("1.0.0"),
            ("acme", "1.0.0"),
            "\n[[dependencies]]\nname = \"acme-text\"\nrequirement = \"^1\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n",
            &[("sync.rhai", b"()")],
        )?;
        library(&store, "1.0.0")?;
        library(&store, "1.1.0")?;
        let jobs = root.join("jobs/acme/sync/jobs");
        fs::create_dir_all(&jobs)?;
        fs::write(
            jobs.join("nightly.toml"),
            "name = \"Nightly\"\nready = true\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        )?;
        Ok(())
    }

    /// The result another job's validation would keep, made for the lock naming `acme-text` at `version`.
    fn another_jobs_result(store: &Path, version: &str) -> Result<StoredValidation, Box<dyn std::error::Error>> {
        let mut result = StoredValidation::from(ValidationRecord {
            report: &ValidationReport::default(),
            fingerprint: Some(JobFingerprint::from(&ReleaseFingerprintInput {
                store: StoreKind::Host,
                package: "acme",
                release: "1.0.0",
                digest: "sha256:0",
                job_toml: "another job",
                release_manifest: "",
                libraries: &[],
            })),
            at: NOW,
        });
        result.made_from = Some(VersionBasis {
            package: "acme".to_owned(),
            release: Version::new(1, 0, 0),
            lock: LibraryLock {
                libraries: [(
                    "acme-text".to_owned(),
                    LockedLibrary {
                        version: Version::parse(version)?,
                        digest: digest_of_library(store, version)?,
                    },
                )]
                .into(),
            },
            predecessor: None,
        });
        Ok(result)
    }

    #[tokio::test]
    async fn a_validation_that_loses_the_race_to_another_jobs_lock_is_repeated_against_the_recorded_lock() -> TestResult
    {
        let root = tempfile::tempdir()?;
        host_files(root.path())?;
        let store_root = root.path().join("packages");
        let state = SharedStateStore::new(root.path().join("state"));
        // While the first validation reads the store, another job's validation records a lock for the same release that
        // names an older library than the one this validation chose.
        let recorded = another_jobs_result(&store_root, "1.0.0")?;
        let racing = AtomicBool::new(false);
        let writer = state.clone();
        set_hook(
            &AFTER_READ,
            &store_root,
            Box::new(move |_| {
                if !racing.swap(true, Ordering::SeqCst) {
                    let _ =
                        writer.update(|jobs| jobs.keep_validation("acme", "sync", "another", recorded.clone(), NOW));
                }
            }),
        );
        let clients = Arc::new(McpClientSet::default());
        let host = SharedPassHost {
            jobs_root: &root.path().join("jobs"),
            state: &state,
            validation: SharedValidationHost {
                store_root: &store_root,
                clients: &clients,
                primary_server: "contextos",
                known_servers: &[],
            },
        };

        let report = shared_validation_pass(&host, NOW).await;

        let kept = state.read()?;
        assert_eq!(kept.lifecycle_of("acme", "sync", "nightly"), JobLifecycle::Validated);
        assert_eq!(report.validated, 1);
        let lock = kept
            .library_lock("acme", "sync", &Version::new(1, 0, 0))
            .ok_or("a lock is recorded")?;
        assert_eq!(
            lock.libraries.get("acme-text").map(|library| library.version.clone()),
            Some(Version::new(1, 0, 0))
        );
        // The job's version is the one made from the recorded lock, not from the newer library its first attempt chose.
        let text = fs::read_to_string(root.path().join("jobs/acme/sync/jobs/nightly.toml"))?;
        let from_lock =
            crate::shared_version::shared_version(&store_root, "acme", &Version::new(1, 0, 0), &text, Some(lock));
        assert_eq!(
            kept.job("acme", "sync", "nightly")
                .and_then(JobState::fingerprint)
                .cloned(),
            from_lock.known()
        );
        Ok(())
    }
}
