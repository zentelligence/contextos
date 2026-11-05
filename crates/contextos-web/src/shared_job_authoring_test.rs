use std::fs;
use std::io;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

use contextos_workflows::{
    Actor, JobFingerprint, JobSchedule, ReleaseFingerprintInput, ReleaseKind, ReleaseKinds, StoreKind, Transition,
    TransitionError,
};
use sha2::{Digest, Sha256};
use time::macros::datetime;

use super::*;
use crate::shared_jobs::{MAX_JOB_MANIFEST_BYTES, SharedJob, SharedJobSource};

pub(super) type TestResult = Result<(), Box<dyn std::error::Error>>;

pub(super) const NOW: time::OffsetDateTime = datetime!(2026-10-04 03:00:00 UTC);
pub(super) const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(super) const VALID: &[u8] = b"name = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n";

/// A host's jobs folder, package store, and state, in a temporary directory.
pub(super) struct Host {
    pub(super) root: tempfile::TempDir,
}

impl Host {
    pub(super) fn new() -> Result<Self, io::Error> {
        Ok(Self {
            root: tempfile::tempdir()?,
        })
    }

    pub(super) fn jobs_root(&self) -> PathBuf {
        self.root.path().join("jobs")
    }

    pub(super) fn store_root(&self) -> PathBuf {
        self.root.path().join("packages")
    }

    pub(super) fn state_directory(&self) -> PathBuf {
        self.root.path().join("state")
    }

    pub(super) fn authoring(&self) -> SharedJobAuthoring {
        SharedJobAuthoring {
            jobs_root: self.jobs_root(),
            store_root: self.store_root(),
            state: SharedStateStore::new(self.state_directory()),
        }
    }

    /// Installs `acme` 1.0.0 with the workflow `sync`.
    pub(super) fn install_package(&self) -> io::Result<()> {
        self.install_version("1.0.0")
    }

    /// Installs `acme` at `version` with the workflow `sync`.
    pub(super) fn install_version(&self, version: &str) -> io::Result<()> {
        let directory = self.store_root().join(format!("acme/{version}"));
        fs::create_dir_all(&directory)?;
        fs::write(
            directory.join("manifest.toml"),
            format!(
                "name = \"acme\"\nversion = \"{version}\"\ndigest = \"{DIGEST}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n"
            ),
        )?;
        fs::write(directory.join("sync.rhai"), "log_info(\"a\");")?;
        crate::release_fixture::seal_io(&directory)
    }

    pub(super) fn job_file(&self, stem: &str) -> PathBuf {
        self.jobs_root().join("acme/sync/jobs").join(format!("{stem}.toml"))
    }

    /// Places a job file by hand, as an operator would.
    pub(super) fn place(&self, stem: &str, bytes: &[u8]) -> io::Result<()> {
        let path = self.job_file(stem);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, bytes)
    }

    /// Gives the job a state entry, as a validation would.
    pub(super) fn validated(&self, job: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.authoring().state.update(|jobs| {
            jobs.transition(
                "acme",
                "sync",
                job,
                Actor::Validator,
                &Transition::Validated(version()),
                NOW,
            )
        })??;
        Ok(())
    }

    /// Takes the job to `enabled`, as a validation, a dry run, and the operator's schedule would.
    pub(super) fn enabled(&self, job: &str) -> Result<(), Box<dyn std::error::Error>> {
        let schedule = JobSchedule::try_from("0 3 * * *")?;
        self.authoring().state.update(|jobs| -> Result<(), TransitionError> {
            let v = version();
            jobs.transition(
                "acme",
                "sync",
                job,
                Actor::Validator,
                &Transition::Validated(v.clone()),
                NOW,
            )?;
            jobs.transition(
                "acme",
                "sync",
                job,
                Actor::DryRun,
                &Transition::DryRunCompleted(v.clone()),
                NOW,
            )?;
            jobs.transition(
                "acme",
                "sync",
                job,
                Actor::Operator,
                &Transition::Enable(schedule, v),
                NOW,
            )?;
            Ok(())
        })??;
        Ok(())
    }

    /// Leaves the job at the stage named: `draft` (no state), `validated`, `registered`, `enabled` (already so),
    /// `suspended`, or `retired`.
    pub(super) fn settle_at(&self, stage: &str, job: &str) -> Result<(), Box<dyn std::error::Error>> {
        let v = version();
        let reason = contextos_workflows::SuspensionReason::try_from("paused for the test")?;
        self.authoring().state.update(|jobs| -> Result<(), TransitionError> {
            match stage {
                "validated" => jobs
                    .transition("acme", "sync", job, Actor::Validator, &Transition::Validated(v), NOW)
                    .map(|_| ()),
                "registered" => {
                    jobs.transition(
                        "acme",
                        "sync",
                        job,
                        Actor::Validator,
                        &Transition::Validated(v.clone()),
                        NOW,
                    )?;
                    jobs.transition("acme", "sync", job, Actor::DryRun, &Transition::DryRunCompleted(v), NOW)
                        .map(|_| ())
                }
                "suspended" => jobs
                    .transition(
                        "acme",
                        "sync",
                        job,
                        Actor::Operator,
                        &Transition::Suspend(contextos_workflows::Suspension::operator(reason)),
                        NOW,
                    )
                    .map(|_| ()),
                "retired" => jobs
                    .transition("acme", "sync", job, Actor::Operator, &Transition::Retire, NOW)
                    .map(|_| ()),
                _ => Ok(()),
            }
        })??;
        Ok(())
    }

    pub(super) fn state_file_exists(&self) -> bool {
        self.authoring().state.state_path().exists()
    }
}

pub(super) fn version() -> JobFingerprint {
    JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Host,
        package: "acme",
        release: "1.0.0",
        digest: "sha256:0",
        job_toml: "",
        release_manifest: "",
        libraries: &[],
    })
}

pub(super) fn key() -> SharedJobKey {
    key_of("acme", "sync", "nightly")
}

pub(super) fn key_of(package: &str, workflow: &str, job: &str) -> SharedJobKey {
    SharedJobKey {
        package: package.to_owned(),
        workflow: workflow.to_owned(),
        job: job.to_owned(),
    }
}

pub(super) fn hash_of(bytes: &[u8]) -> ContentHash {
    ContentHash::from(<[u8; 32]>::from(Sha256::digest(bytes)))
}

pub(super) fn refusal<T: std::fmt::Debug>(result: Result<T, AuthoringError>) -> Result<AuthoringError, String> {
    match result {
        Err(error) => Ok(error),
        Ok(value) => Err(format!("expected a refusal, got {value:?}")),
    }
}

#[test]
fn a_created_job_holds_exactly_the_bytes_given_including_comments_and_crlf_line_endings() -> TestResult {
    let host = Host::new()?;
    host.install_package()?;
    let bytes = b"# why this runs\r\nname = \"Nightly\"   # trailing\r\ntimeout_secs = 120\r\nversion = \"1.0.0\"\r\nready = true\r\n";

    let authored = host.authoring().create(&key(), bytes)?;

    assert_eq!(authored.outcome, AuthoredOutcome::Created);
    assert_eq!(authored.hash, hash_of(bytes));
    assert!(authored.warnings.is_empty(), "{:?}", authored.warnings);
    assert_eq!(fs::read(host.job_file("nightly"))?, bytes);
    Ok(())
}

#[test]
fn create_accepts_and_refuses_exactly_what_loading_a_placed_file_accepts_and_refuses() -> TestResult {
    let refused: [(&str, &str); 7] = [
        (
            "name = \"N\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nowner = \"x\"\n",
            "validation/artefact-invalid",
        ),
        (
            "name = \"   \"\ntimeout_secs = 120\nversion = \"1.0.0\"\n",
            "validation/artefact-invalid",
        ),
        (
            "name = \"N\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"Bad Slug\"\n",
            "validation/artefact-invalid",
        ),
        (
            "name = \"N\"\ntimeout_secs = 120\nversion = \"^1\"\n",
            "validation/artefact-invalid",
        ),
        (
            "name = \"N\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nschedule = \"0 3 * * *\"\n",
            "validation/artefact-invalid",
        ),
        ("this is = = not toml", "validation/artefact-invalid"),
        ("", "validation/artefact-invalid"),
    ];
    for (text, cause) in refused {
        let host = Host::new()?;
        let loaded = SharedJob::try_from(SharedJobSource { key: key(), text });
        assert!(loaded.is_err(), "loading a placed file must refuse {text:?}");

        let error = refusal(host.authoring().create(&key(), text.as_bytes()))?;

        assert_eq!(error.code(), "workflow/job-file-invalid", "{text:?}: {error}");
        assert_eq!(error.cause(), Some(cause), "{text:?}");
        assert!(!host.job_file("nightly").exists(), "{text:?}");
    }

    let complete = "name = \"N\"\ntimeout_secs = 90\nversion = \"1.2.0\"\nlog_level = \"debug\"\nready = true\n\n[inputs]\nsince = \"x\"\n";
    assert!(
        SharedJob::try_from(SharedJobSource {
            key: key(),
            text: complete
        })
        .is_ok()
    );
    let host = Host::new()?;
    host.install_version("1.2.0")?;
    let authored = host.authoring().create(&key(), complete.as_bytes())?;
    assert_eq!(authored.outcome, AuthoredOutcome::Created);
    Ok(())
}

/// An endless reader that counts what was taken from it.
struct Endless {
    served: u64,
}

impl io::Read for Endless {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        buffer.fill(b'a');
        self.served += u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        Ok(buffer.len())
    }
}

#[test]
fn toml_larger_than_64_kib_is_refused_after_reading_at_most_one_byte_more() -> TestResult {
    let mut endless = Endless { served: 0 };

    let error = refusal(read_job_bytes(&mut endless))?;

    assert_eq!(error.code(), "workflow/job-file-invalid");
    assert_eq!(error.cause(), Some("validation/artefact-invalid"));
    assert!(
        endless.served <= MAX_JOB_MANIFEST_BYTES + 1,
        "read {} bytes",
        endless.served
    );
    Ok(())
}

#[test]
fn input_within_the_limit_is_read_whole_and_unaltered() -> TestResult {
    let bytes = b"# c\r\nname = \"N\"\r\ntimeout_secs = 120\r\nversion = \"1.0.0\"\r\n".to_vec();

    let read = read_job_bytes(io::Cursor::new(bytes.clone()))?;

    assert_eq!(read, bytes);
    Ok(())
}

#[test]
fn toml_that_is_not_utf8_is_refused_and_nothing_is_written() -> TestResult {
    let host = Host::new()?;

    let error = refusal(host.authoring().create(
        &key(),
        b"name = \"\xff\xfe\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n",
    ))?;

    assert_eq!(error.code(), "workflow/job-file-invalid");
    assert_eq!(error.cause(), Some("validation/artefact-invalid"));
    assert!(!host.job_file("nightly").exists());
    assert!(!host.jobs_root().exists());
    Ok(())
}

#[test]
fn a_schedule_key_is_refused_as_an_unknown_key() -> TestResult {
    let host = Host::new()?;

    let error = refusal(host.authoring().create(
        &key(),
        b"name = \"N\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nschedule = \"0 3 * * *\"\n",
    ))?;

    assert_eq!(error.code(), "workflow/job-file-invalid");
    assert_eq!(error.cause(), Some("validation/artefact-invalid"));
    assert!(error.to_string().contains("validation/artefact-invalid"), "{error}");
    Ok(())
}

#[test]
fn a_slug_key_other_than_the_job_argument_is_refused_and_an_equal_one_is_accepted() -> TestResult {
    let host = Host::new()?;
    host.install_package()?;

    let mismatch = refusal(host.authoring().create(
        &key(),
        b"name = \"N\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"other\"\n",
    ))?;
    assert_eq!(mismatch.code(), "workflow/job-key-mismatch");
    assert!(!host.job_file("nightly").exists());
    assert!(!host.job_file("other").exists());

    let equal = host.authoring().create(
        &key(),
        b"name = \"N\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"nightly\"\n",
    )?;
    assert_eq!(equal.outcome, AuthoredOutcome::Created);
    Ok(())
}

#[test]
fn creating_over_a_different_file_is_refused_with_job_exists_and_leaves_it_untouched() -> TestResult {
    let host = Host::new()?;
    host.place(
        "nightly",
        b"name = \"Placed by hand\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n",
    )?;

    let error = refusal(host.authoring().create(&key(), VALID))?;

    assert_eq!(error.code(), "workflow/job-exists");
    assert_eq!(
        fs::read(host.job_file("nightly"))?,
        b"name = \"Placed by hand\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n"
    );
    Ok(())
}

#[test]
fn creating_the_same_bytes_again_reports_unchanged_and_does_not_rewrite_the_file() -> TestResult {
    let host = Host::new()?;
    host.install_package()?;
    let first = host.authoring().create(&key(), VALID)?;
    assert_eq!(first.outcome, AuthoredOutcome::Created);
    let before = fs::metadata(host.job_file("nightly"))?;

    let again = host.authoring().create(&key(), VALID)?;

    assert_eq!(again.outcome, AuthoredOutcome::Unchanged);
    assert_eq!(again.hash, hash_of(VALID));
    let after = fs::metadata(host.job_file("nightly"))?;
    assert_eq!(before.modified()?, after.modified()?);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(before.ino(), after.ino(), "the file was replaced by a rewrite");
    }
    Ok(())
}

#[test]
fn a_key_another_file_is_tracked_under_is_refused_with_key_taken() -> TestResult {
    let host = Host::new()?;
    host.place(
        "other",
        b"name = \"Other\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"nightly\"\n",
    )?;

    let error = refusal(host.authoring().create(&key(), VALID))?;

    assert_eq!(error.code(), "workflow/job-key-taken");
    assert!(error.to_string().contains("other"), "{error}");
    assert!(!host.job_file("nightly").exists());
    Ok(())
}

#[test]
fn a_key_with_stored_state_and_no_file_is_refused_with_state_remains_and_the_state_is_untouched() -> TestResult {
    let host = Host::new()?;
    host.validated("nightly")?;
    let before = fs::read(host.authoring().state.state_path())?;

    let error = refusal(host.authoring().create(&key(), VALID))?;

    assert_eq!(error.code(), "workflow/job-state-remains");
    assert!(!host.job_file("nightly").exists());
    assert_eq!(fs::read(host.authoring().state.state_path())?, before);
    Ok(())
}

/// The same answer for every release, for a test that is about what each kind of release allows and not about how current and previous are recorded.
struct SameKind(ReleaseKind);

impl ReleaseKinds for SameKind {
    fn release_kind(&self, _package: &str, _workflow: &str, _release: &Version) -> ReleaseKind {
        self.0
    }
}

fn job_naming(version: &str) -> Vec<u8> {
    format!("name = \"Nightly\"\ntimeout_secs = 120\nversion = \"{version}\"\n").into_bytes()
}

#[test]
fn a_job_naming_a_release_that_is_not_installed_is_refused_as_package_unavailable_and_nothing_is_written() -> TestResult
{
    let host = Host::new()?;

    let nothing_installed = refusal(host.authoring().create(&key(), VALID))?;
    host.install_package()?;
    let other_version = refusal(host.authoring().create(&key(), &job_naming("2.0.0")))?;
    let other_workflow = refusal(host.authoring().create(&key_of("acme", "other", "nightly"), VALID))?;
    let other_package = refusal(host.authoring().create(&key_of("beta", "sync", "nightly"), VALID))?;

    for refused in [nothing_installed, other_version, other_workflow, other_package] {
        assert_eq!(refused.code(), "workflow/package-unavailable");
        assert!(refused.to_string().contains("not installed"), "{refused}");
    }
    assert!(!host.job_file("nightly").exists());
    assert!(!host.state_file_exists());
    Ok(())
}

#[test]
fn a_package_store_that_cannot_be_read_gives_package_unavailable_saying_it_could_not_be_checked() -> TestResult {
    let host = Host::new()?;
    fs::write(host.store_root(), b"a file where the store folder belongs")?;

    let refused = refusal(host.authoring().create(&key(), VALID))?;

    assert_eq!(refused.code(), "workflow/package-unavailable");
    assert!(refused.to_string().contains("could not be checked"), "{refused}");
    assert!(!host.job_file("nightly").exists());
    Ok(())
}

#[test]
fn a_job_may_be_created_naming_an_installed_release_with_no_current_which_is_a_future_release() -> TestResult {
    let host = Host::new()?;
    host.install_package()?;

    let created = host.authoring().create(&key(), VALID)?;

    assert_eq!(created.outcome, AuthoredOutcome::Created);
    assert!(created.warnings.is_empty(), "{:?}", created.warnings);
    Ok(())
}

#[test]
fn a_new_job_may_name_a_current_or_future_release_and_not_a_previous_or_retired_one() -> TestResult {
    for (kind, allowed) in [
        (ReleaseKind::Current, true),
        (ReleaseKind::Future, true),
        (ReleaseKind::Previous, false),
        (ReleaseKind::Retired, false),
    ] {
        let host = Host::new()?;
        host.install_package()?;

        let result = host.authoring().create_with_kinds(&key(), VALID, &SameKind(kind));

        if allowed {
            assert_eq!(result?.outcome, AuthoredOutcome::Created, "{kind:?}");
        } else {
            let refused = refusal(result)?;
            assert_eq!(refused.code(), "workflow/transition-refused", "{kind:?}");
            assert!(refused.to_string().contains(kind.as_str()), "{refused}");
            assert!(!host.job_file("nightly").exists(), "{kind:?}");
        }
    }
    Ok(())
}

#[test]
fn a_key_a_standing_successor_names_as_its_predecessor_is_taken() -> TestResult {
    let host = Host::new()?;
    host.install_package()?;
    host.place(
        "nightly-v2",
        b"name = \"N2\"\ntimeout_secs = 120\nversion = \"1.0.0\"\npredecessor = \"nightly\"\n",
    )?;

    let refused = refusal(host.authoring().create(&key(), VALID))?;

    assert_eq!(refused.code(), "workflow/job-key-taken");
    assert!(refused.to_string().contains("nightly-v2"), "{refused}");
    assert!(!host.job_file("nightly").exists());
    Ok(())
}

#[test]
fn a_successor_that_has_been_retired_no_longer_holds_the_key() -> TestResult {
    let host = Host::new()?;
    host.install_package()?;
    host.place(
        "nightly-v2",
        b"name = \"N2\"\ntimeout_secs = 120\nversion = \"1.0.0\"\npredecessor = \"nightly\"\n",
    )?;
    host.enabled("nightly-v2")?;
    host.authoring()
        .state
        .update(|jobs| jobs.transition("acme", "sync", "nightly-v2", Actor::Operator, &Transition::Retire, NOW))??;

    let created = host.authoring().create(&key(), VALID)?;

    assert_eq!(created.outcome, AuthoredOutcome::Created);
    Ok(())
}

#[test]
fn a_job_that_is_enabled_suspended_or_retired_cannot_change_its_version_or_predecessor() -> TestResult {
    for stage in ["enabled", "suspended", "retired"] {
        let host = Host::new()?;
        host.install_package()?;
        host.install_version("2.0.0")?;
        host.place("nightly", VALID)?;
        host.enabled("nightly")?;
        host.settle_at(stage, "nightly")?;
        let shown = host.authoring().show(&key())?;
        let before = fs::read(host.job_file("nightly"))?;

        for replacement in [
            job_naming("2.0.0"),
            b"name = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\npredecessor = \"old\"\n".to_vec(),
        ] {
            let refused = refusal(host.authoring().replace(&key(), &replacement, &shown.hash))?;

            assert_eq!(refused.code(), "workflow/transition-refused", "{stage}");
            assert!(refused.to_string().contains("jobs successor"), "{stage}: {refused}");
            assert!(refused.to_string().contains(stage), "{stage}: {refused}");
        }
        assert_eq!(fs::read(host.job_file("nightly"))?, before, "{stage}");
    }
    Ok(())
}

#[test]
fn a_standing_job_may_still_change_anything_but_its_version_and_predecessor() -> TestResult {
    let host = Host::new()?;
    host.install_package()?;
    host.place("nightly", VALID)?;
    host.enabled("nightly")?;
    let shown = host.authoring().show(&key())?;
    let renamed = b"name = \"Renamed\"\ntimeout_secs = 90\nversion = \"1.0.0\"\n";

    let replaced = host.authoring().replace(&key(), renamed, &shown.hash)?;

    assert_eq!(replaced.outcome, AuthoredOutcome::Replaced);
    Ok(())
}

#[test]
fn a_job_not_yet_enabled_may_change_its_version_to_an_installed_current_or_future_release() -> TestResult {
    for stage in ["draft", "validated", "registered"] {
        let host = Host::new()?;
        host.install_package()?;
        host.install_version("2.0.0")?;
        host.place("nightly", VALID)?;
        host.settle_at(stage, "nightly")?;
        let shown = host.authoring().show(&key())?;

        let replaced = host.authoring().replace(&key(), &job_naming("2.0.0"), &shown.hash)?;

        assert_eq!(replaced.outcome, AuthoredOutcome::Replaced, "{stage}");
    }
    Ok(())
}

#[test]
fn a_job_not_yet_enabled_cannot_move_to_a_release_that_is_not_installed_or_whose_role_takes_no_job() -> TestResult {
    let host = Host::new()?;
    host.install_package()?;
    host.install_version("2.0.0")?;
    host.place("nightly", VALID)?;
    let shown = host.authoring().show(&key())?;

    let missing = refusal(host.authoring().replace(&key(), &job_naming("3.0.0"), &shown.hash))?;
    let previous = refusal(host.authoring().replace_with_kinds(
        &key(),
        &job_naming("2.0.0"),
        &shown.hash,
        &SameKind(ReleaseKind::Previous),
    ))?;

    assert_eq!(missing.code(), "workflow/package-unavailable");
    assert_eq!(previous.code(), "workflow/transition-refused");
    assert!(previous.to_string().contains("previous"), "{previous}");
    assert_eq!(fs::read(host.job_file("nightly"))?, VALID);
    Ok(())
}

#[test]
fn a_replacement_that_keeps_its_version_is_stored_with_a_warning_when_its_package_is_not_installed() -> TestResult {
    let host = Host::new()?;
    host.place("nightly", VALID)?;
    let shown = host.authoring().show(&key())?;
    let revised = b"name = \"Revised\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n";

    let replaced = host.authoring().replace(&key(), revised, &shown.hash)?;

    assert_eq!(replaced.outcome, AuthoredOutcome::Replaced);
    assert_eq!(
        replaced.warnings,
        [AuthoringWarning::PackageUnavailable {
            package: "acme".to_owned(),
            workflow: "sync".to_owned()
        }]
    );
    assert_eq!(replaced.warnings[0].code(), "workflow/package-unavailable");
    Ok(())
}

#[test]
fn a_replacement_keeping_its_version_warns_when_that_release_is_not_installed_though_the_package_is() -> TestResult {
    let host = Host::new()?;
    host.install_package()?;
    host.place("nightly", &job_naming("5.0.0"))?;
    let shown = host.authoring().show(&key())?;
    let revised = b"name = \"Revised\"\ntimeout_secs = 120\nversion = \"5.0.0\"\n";

    let replaced = host.authoring().replace(&key(), revised, &shown.hash)?;

    assert_eq!(replaced.outcome, AuthoredOutcome::Replaced);
    assert_eq!(
        replaced.warnings,
        [AuthoringWarning::PackageUnavailable {
            package: "acme".to_owned(),
            workflow: "sync".to_owned()
        }]
    );
    Ok(())
}

#[test]
fn create_show_and_replace_never_create_or_change_the_state_file() -> TestResult {
    let host = Host::new()?;
    host.install_package()?;
    let authoring = host.authoring();

    let created = authoring.create(&key(), VALID)?;
    let shown = authoring.show(&key())?;
    let replaced = authoring.replace(
        &key(),
        b"name = \"Renamed\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n",
        &shown.hash,
    )?;

    assert_eq!(created.outcome, AuthoredOutcome::Created);
    assert_eq!(replaced.outcome, AuthoredOutcome::Replaced);
    assert!(!host.state_file_exists(), "no state file was made");

    host.validated("elsewhere")?;
    let before = fs::read(authoring.state.state_path())?;
    let shown = authoring.show(&key())?;
    authoring.replace(&key(), VALID, &shown.hash)?;
    authoring.create(&key_of("acme", "sync", "second"), VALID)?;
    assert_eq!(
        fs::read(authoring.state.state_path())?,
        before,
        "the state is byte for byte as it was"
    );
    Ok(())
}

#[test]
fn key_parts_that_are_not_slugs_or_are_device_names_are_refused_before_anything_is_touched() -> TestResult {
    let bad = ["..", "A", "a/b", "a\\b", "con", "nul", "com1", "lpt9", ""];
    for value in bad {
        for position in 0..3 {
            let host = Host::new()?;
            let mut parts = ["acme", "sync", "nightly"];
            parts[position] = value;
            let attempted = key_of(parts[0], parts[1], parts[2]);

            let error = refusal(host.authoring().create(&attempted, VALID))?;

            assert_eq!(
                error.code(),
                "workflow/job-key-invalid",
                "{value:?} at {position}: {error}"
            );
            assert!(
                !host.jobs_root().exists(),
                "{value:?} at {position}: the jobs folder was made"
            );
            assert!(
                !host.state_directory().exists(),
                "{value:?} at {position}: the state was touched"
            );
        }
    }
    Ok(())
}

#[test]
fn a_job_key_the_state_file_uses_for_a_workflows_versions_is_refused_on_create_and_replace() -> TestResult {
    for reserved in ["current", "previous", "future"] {
        let host = Host::new()?;
        host.install_package()?;
        let attempted = key_of("acme", "sync", reserved);

        let created = refusal(host.authoring().create(&attempted, VALID))?;
        let replaced = refusal(host.authoring().replace(&attempted, VALID, &hash_of(VALID)))?;

        assert_eq!(created.code(), "workflow/job-key-invalid", "{reserved}: {created}");
        assert_eq!(replaced.code(), "workflow/job-key-invalid", "{reserved}: {replaced}");
        assert!(!host.job_file(reserved).exists(), "{reserved}");
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_link_at_the_package_workflow_or_job_level_is_refused_and_nothing_is_written_through_it() -> TestResult {
    use std::os::unix::fs::symlink;

    let entries = |directory: &Path| -> io::Result<usize> { Ok(fs::read_dir(directory)?.count()) };

    // Control: with no link in the way the same request succeeds, so a refusal below is the link's doing.
    let control = Host::new()?;
    control.install_package()?;
    assert_eq!(
        control.authoring().create(&key(), VALID)?.outcome,
        AuthoredOutcome::Created
    );

    // The package folder is a link.
    let host = Host::new()?;
    let outside = tempfile::tempdir()?;
    fs::create_dir_all(host.jobs_root())?;
    symlink(outside.path(), host.jobs_root().join("acme"))?;
    let error = refusal(host.authoring().create(&key(), VALID))?;
    assert_eq!(error.code(), "workflow/job-store-unavailable", "{error}");
    assert_eq!(entries(outside.path())?, 0, "nothing was made through the package link");

    // The workflow folder is a link.
    let host = Host::new()?;
    let outside = tempfile::tempdir()?;
    fs::create_dir_all(host.jobs_root().join("acme"))?;
    symlink(outside.path(), host.jobs_root().join("acme/sync"))?;
    let error = refusal(host.authoring().create(&key(), VALID))?;
    assert_eq!(error.code(), "workflow/job-store-unavailable", "{error}");
    assert_eq!(
        entries(outside.path())?,
        0,
        "nothing was made through the workflow link"
    );

    // The job file is a link, to nothing and to a file that exists.
    let host = Host::new()?;
    let outside = tempfile::tempdir()?;
    fs::create_dir_all(host.jobs_root().join("acme/sync/jobs"))?;
    let dangling = outside.path().join("dangling.toml");
    symlink(&dangling, host.job_file("nightly"))?;
    let error = refusal(host.authoring().create(&key(), VALID))?;
    assert_eq!(error.code(), "workflow/job-store-unavailable", "{error}");
    assert!(!dangling.exists(), "nothing was written through the job link");

    let host = Host::new()?;
    let real = outside.path().join("real.toml");
    fs::write(&real, b"name = \"Real\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n")?;
    fs::create_dir_all(host.jobs_root().join("acme/sync/jobs"))?;
    symlink(&real, host.job_file("nightly"))?;
    let error = refusal(host.authoring().replace(
        &key(),
        VALID,
        &hash_of(b"name = \"Real\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n"),
    ))?;
    assert_eq!(error.code(), "workflow/job-store-unavailable", "{error}");
    assert_eq!(
        fs::read(&real)?,
        b"name = \"Real\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n"
    );
    Ok(())
}
