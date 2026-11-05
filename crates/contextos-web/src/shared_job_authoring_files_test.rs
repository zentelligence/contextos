//! How authoring treats job files that cannot be used, a job file that keeps its key, where a refusal points, and the
//! creation race, apart from the main behaviours in `shared_job_authoring_test.rs`.

use std::fs;
use std::io;

use contextos_fs::host_file::{FileReplaceObserver, FileReplaceStep};

use super::tests::{Host, TestResult, VALID, hash_of, key, key_of, refusal};
use super::*;

const OVERSIZE: usize = 64 * 1024 + 1;

fn oversize() -> Vec<u8> {
    vec![b'#'; OVERSIZE]
}

fn not_utf8() -> Vec<u8> {
    vec![0xff, 0xfe, 0x00, 0x80]
}

fn row<'a>(rows: &'a [JobListing], job: &str) -> Result<&'a JobListing, String> {
    rows.iter()
        .find(|row| row.key.job == job)
        .ok_or_else(|| format!("no row for {job}: {rows:#?}"))
}

#[test]
fn list_shows_a_job_file_that_is_too_large_or_not_text_with_the_reason_and_neither_its_content_nor_its_path()
-> TestResult {
    let host = Host::new()?;
    host.place("big", &oversize())?;
    host.place("binary", &not_utf8())?;
    host.place("fine", VALID)?;

    let rows = host.authoring().list()?;

    assert_eq!(rows.len(), 3, "{rows:#?}");
    let big = row(&rows, "big")?;
    assert_eq!(big.problems.len(), 1, "{big:#?}");
    assert_eq!(big.problems[0].code, "validation/artefact-invalid");
    assert!(big.problems[0].message.contains("too large"), "{big:#?}");
    let binary = row(&rows, "binary")?;
    assert_eq!(binary.problems.len(), 1, "{binary:#?}");
    assert_eq!(binary.problems[0].code, "validation/artefact-invalid");
    assert!(binary.problems[0].message.contains("not UTF-8"), "{binary:#?}");
    assert_eq!(row(&rows, "fine")?.problems.len(), 0);
    let root = host.root.path().to_string_lossy().into_owned();
    for shown in [big, binary] {
        for problem in &shown.problems {
            assert!(!problem.message.contains(&root), "{}", problem.message);
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn list_shows_a_job_file_that_cannot_be_read_with_the_kind_of_failure_and_no_path() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let host = Host::new()?;
    host.place("locked", VALID)?;
    fs::set_permissions(host.job_file("locked"), fs::Permissions::from_mode(0o000))?;

    let listed = host.authoring().list();
    fs::set_permissions(host.job_file("locked"), fs::Permissions::from_mode(0o600))?;

    let rows = listed?;
    let locked = row(&rows, "locked")?;
    assert_eq!(locked.problems.len(), 1, "{locked:#?}");
    assert_eq!(locked.problems[0].code, "validation/artefact-invalid");
    let message = &locked.problems[0].message;
    assert!(message.contains("unreadable"), "{message}");
    assert!(
        message.contains("permission denied") || message.contains("PermissionDenied"),
        "{message}"
    );
    assert!(
        !message.contains(&host.root.path().to_string_lossy().into_owned()),
        "{message}"
    );
    Ok(())
}

#[test]
fn list_shows_a_file_whose_name_is_not_a_slug_with_a_key_invalid_problem() -> TestResult {
    let host = Host::new()?;
    host.place("Bad_Name", VALID)?;

    let rows = host.authoring().list()?;

    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert!(
        rows[0]
            .problems
            .iter()
            .any(|problem| problem.code == "workflow/job-key-invalid"),
        "{rows:#?}"
    );
    Ok(())
}

#[test]
fn show_and_replace_of_a_job_whose_file_cannot_be_used_refuse_with_job_file_invalid_not_job_not_found() -> TestResult {
    for (label, bytes) in [("too large", oversize()), ("not UTF-8", not_utf8())] {
        let host = Host::new()?;
        host.place("nightly", &bytes)?;

        let shown = refusal(host.authoring().show(&key()))?;
        let replaced = refusal(host.authoring().replace(&key(), VALID, &hash_of(&bytes)))?;

        for error in [&shown, &replaced] {
            assert_eq!(error.code(), "workflow/job-file-invalid", "{label}: {error}");
            assert_eq!(error.cause(), Some("validation/artefact-invalid"), "{label}: {error}");
        }
        assert_eq!(
            fs::read(host.job_file("nightly"))?,
            bytes,
            "{label}: the file is untouched"
        );
    }
    Ok(())
}

#[test]
fn creating_at_the_path_of_a_job_file_that_cannot_be_used_is_still_job_exists() -> TestResult {
    let host = Host::new()?;
    host.place("nightly", &oversize())?;

    let error = refusal(host.authoring().create(&key(), VALID))?;

    assert_eq!(error.code(), "workflow/job-exists", "{error}");
    assert_eq!(fs::read(host.job_file("nightly"))?, oversize());
    Ok(())
}

#[test]
fn replace_keeps_the_jobs_key_whatever_file_name_it_is_stored_under() -> TestResult {
    let host = Host::new()?;
    let tracked = b"name = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"nightly\"\n";
    host.place("other", tracked)?;

    let without_slug = refusal(host.authoring().replace(&key(), VALID, &hash_of(tracked)))?;
    let other_slug = refusal(host.authoring().replace(
        &key(),
        b"name = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"renamed\"\n",
        &hash_of(tracked),
    ))?;

    assert_eq!(without_slug.code(), "workflow/job-key-mismatch", "{without_slug}");
    assert_eq!(other_slug.code(), "workflow/job-key-mismatch", "{other_slug}");
    assert_eq!(fs::read(host.job_file("other"))?, tracked, "the file is unchanged");
    assert!(!host.job_file("nightly").exists());

    let kept = b"name = \"Nightly, revised\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"nightly\"\n";
    host.authoring().replace(&key(), kept, &hash_of(tracked))?;
    assert_eq!(
        fs::read(host.job_file("other"))?,
        kept,
        "bytes that keep the slug are stored"
    );
    Ok(())
}

#[test]
fn show_and_replace_of_a_key_two_files_share_are_refused_with_key_taken() -> TestResult {
    let host = Host::new()?;
    host.place(
        "twin-a",
        b"name = \"A\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"twin\"\n",
    )?;
    host.place(
        "twin-b",
        b"name = \"B\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"twin\"\n",
    )?;
    let twin = key_of("acme", "sync", "twin");
    let replacement = b"name = \"C\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"twin\"\n";

    let shown = refusal(host.authoring().show(&twin))?;
    let replaced = refusal(host.authoring().replace(&twin, replacement, &hash_of(b"")))?;

    assert_eq!(shown.code(), "workflow/job-key-taken", "{shown}");
    assert_eq!(replaced.code(), "workflow/job-key-taken", "{replaced}");
    assert_eq!(
        fs::read(host.job_file("twin-a"))?,
        b"name = \"A\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"twin\"\n"
    );
    Ok(())
}

#[test]
fn show_and_replace_of_a_key_with_a_tracked_file_and_an_unusable_file_of_the_same_stem_are_refused_with_key_taken()
-> TestResult {
    let tracked = b"name = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"nightly\"\n";
    for (label, unusable) in [("too large", oversize()), ("not UTF-8", not_utf8())] {
        let host = Host::new()?;
        host.place("other", tracked)?;
        host.place("nightly", &unusable)?;

        let shown = refusal(host.authoring().show(&key()))?;
        let replaced = refusal(host.authoring().replace(&key(), VALID, &hash_of(tracked)))?;

        for error in [&shown, &replaced] {
            assert_eq!(error.code(), "workflow/job-key-taken", "{label}: {error}");
            let message = error.to_string();
            assert!(
                message.contains("other.toml") && message.contains("nightly.toml"),
                "{label}: both files are named: {message}"
            );
            assert!(
                !message.contains(&host.root.path().to_string_lossy().into_owned()),
                "{label}: files are named without their paths: {message}"
            );
        }
        assert_eq!(
            fs::read(host.job_file("other"))?,
            tracked,
            "{label}: the tracked file is unchanged"
        );
        assert_eq!(
            fs::read(host.job_file("nightly"))?,
            unusable,
            "{label}: the unusable file is unchanged"
        );
    }
    Ok(())
}

#[test]
fn replace_of_a_job_whose_package_is_not_installed_succeeds_with_a_package_unavailable_warning() -> TestResult {
    let host = Host::new()?;
    host.place("nightly", VALID)?;
    let revised = b"name = \"Nightly, revised\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n";

    let authored = host.authoring().replace(&key(), revised, &hash_of(VALID))?;

    assert_eq!(authored.outcome, AuthoredOutcome::Replaced);
    assert_eq!(authored.warnings.len(), 1, "{:?}", authored.warnings);
    assert_eq!(authored.warnings[0].code(), "workflow/package-unavailable");

    let installed = Host::new()?;
    installed.install_package()?;
    installed.place("nightly", VALID)?;
    let authored = installed.authoring().replace(&key(), revised, &hash_of(VALID))?;
    assert!(authored.warnings.is_empty(), "{:?}", authored.warnings);
    Ok(())
}

#[test]
fn a_replacement_keeping_its_version_in_a_store_that_cannot_be_checked_warns_that_it_could_not_be_checked() -> TestResult
{
    let host = Host::new()?;
    host.place("nightly", VALID)?;
    let directory = host.store_root().join("acme/1.0.0");
    fs::create_dir_all(&directory)?;
    fs::write(directory.join("manifest.toml"), b"this is not a manifest")?;
    let shown = host.authoring().show(&key())?;
    let revised = b"name = \"Revised\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n";

    let authored = host.authoring().replace(&key(), revised, &shown.hash)?;

    assert_eq!(authored.warnings.len(), 1, "{:?}", authored.warnings);
    assert_eq!(authored.warnings[0].code(), "workflow/package-unavailable");
    assert!(
        matches!(authored.warnings[0], AuthoringWarning::PackageUnchecked { .. }),
        "an unreadable store is a different warning from a package that is not installed: {:?}",
        authored.warnings[0]
    );
    Ok(())
}

#[test]
fn create_is_refused_with_job_exists_when_another_writer_places_the_file_just_before_the_move() -> TestResult {
    struct PlaceFile(std::path::PathBuf);
    impl FileReplaceObserver for PlaceFile {
        fn reached(&self, step: FileReplaceStep) -> io::Result<()> {
            match step {
                FileReplaceStep::TemporaryReady => fs::write(&self.0, b"placed by another writer"),
            }
        }
    }

    let host = Host::new()?;
    host.install_package()?;
    // The folder exists, so the file is the only thing the other writer's arrival can change.
    fs::create_dir_all(host.jobs_root().join("acme/sync/jobs"))?;
    let observer = PlaceFile(host.job_file("nightly"));

    let error = refusal(host.authoring().create_observed(&key(), VALID, &observer))?;

    assert_eq!(error.code(), "workflow/job-exists", "{error}");
    assert_eq!(fs::read(host.job_file("nightly"))?, b"placed by another writer");
    let names: Vec<_> = fs::read_dir(host.jobs_root().join("acme/sync/jobs"))?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<_, _>>()?;
    assert_eq!(names, ["nightly.toml"], "no temporary file is left beside it");
    Ok(())
}

#[test]
fn a_schema_error_names_the_line_and_column_and_never_the_source_line() -> TestResult {
    let host = Host::new()?;
    let unknown = b"name = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nowner = \"x\"\n";
    let mistyped = b"name = \"Nightly\"\ntimeout_secs = \"soon\"\n";
    let syntax = b"name = \"Nightly\"\nthis is not toml\n";

    let unknown = refusal(host.authoring().create(&key(), unknown))?.to_string();
    let mistyped = refusal(host.authoring().create(&key(), mistyped))?.to_string();
    let syntax = refusal(host.authoring().create(&key(), syntax))?.to_string();

    assert!(unknown.contains("line 4, column 1"), "{unknown}");
    assert!(mistyped.contains("line 2, column 16"), "{mistyped}");
    assert!(syntax.contains("line 2"), "{syntax}");
    assert!(syntax.contains("column"), "{syntax}");
    for (text, source_line) in [
        (&unknown, "owner = \"x\""),
        (&mistyped, "timeout_secs = \"soon\""),
        (&syntax, "this is not toml"),
    ] {
        assert!(!text.contains(source_line), "the source line is not repeated: {text}");
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_created_job_file_has_the_permissions_a_hand_placed_one_has_and_a_replaced_one_keeps_its_own() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let host = Host::new()?;
    host.install_package()?;
    host.place("hand-placed", VALID)?;
    let mode_of =
        |stem: &str| -> io::Result<u32> { Ok(fs::metadata(host.job_file(stem))?.permissions().mode() & 0o777) };
    let default = mode_of("hand-placed")?;

    host.authoring().create(&key(), VALID)?;
    assert_eq!(
        mode_of("nightly")?,
        default,
        "created with the umask default, not the temporary file's mode"
    );

    fs::set_permissions(host.job_file("nightly"), fs::Permissions::from_mode(0o640))?;
    host.authoring().replace(
        &key(),
        b"name = \"Nightly, revised\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n",
        &hash_of(VALID),
    )?;
    assert_eq!(
        mode_of("nightly")?,
        0o640,
        "a replaced file keeps the permissions it had"
    );
    Ok(())
}

/// A junction needs no privilege to make on Windows, so it is the link an attacker can plant under the jobs folder.
#[cfg(windows)]
#[test]
fn a_junction_at_the_package_workflow_or_job_level_is_refused_and_nothing_is_written_through_it() -> TestResult {
    let junction = |link: &std::path::Path, target: &std::path::Path| -> io::Result<()> {
        // `mklink` reads a forward slash as a switch, so every separator must be the native one.
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link.components().collect::<std::path::PathBuf>())
            .arg(target.components().collect::<std::path::PathBuf>())
            .output()?;
        if made.status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("{made:?}")))
        }
    };
    let entries = |directory: &std::path::Path| -> io::Result<usize> { Ok(fs::read_dir(directory)?.count()) };

    let host = Host::new()?;
    let outside = tempfile::tempdir()?;
    fs::create_dir_all(host.jobs_root())?;
    junction(&host.jobs_root().join("acme"), outside.path())?;
    let error = refusal(host.authoring().create(&key(), VALID))?;
    assert_eq!(error.code(), "workflow/job-store-unavailable", "{error}");
    assert_eq!(
        entries(outside.path())?,
        0,
        "nothing was made through the package junction"
    );

    let host = Host::new()?;
    let outside = tempfile::tempdir()?;
    fs::create_dir_all(host.jobs_root().join("acme"))?;
    junction(&host.jobs_root().join("acme/sync"), outside.path())?;
    let error = refusal(host.authoring().create(&key(), VALID))?;
    assert_eq!(error.code(), "workflow/job-store-unavailable", "{error}");
    assert_eq!(
        entries(outside.path())?,
        0,
        "nothing was made through the workflow junction"
    );

    let host = Host::new()?;
    let outside = tempfile::tempdir()?;
    fs::create_dir_all(host.jobs_root().join("acme/sync/jobs"))?;
    junction(&host.job_file("nightly"), outside.path())?;
    let error = refusal(host.authoring().create(&key(), VALID))?;
    assert_eq!(error.code(), "workflow/job-store-unavailable", "{error}");
    assert_eq!(entries(outside.path())?, 0, "nothing was made through the job junction");
    Ok(())
}

#[test]
fn list_names_the_job_a_retired_jobs_retirement_record_says_replaced_it_even_with_no_file() -> TestResult {
    let host = Host::new()?;
    let state: contextos_workflows::JobsState = toml::from_str(
        "[acme.sync.nightly]\nlifecycle = \"retired\"\n\n[acme.sync.nightly.retirement]\nby = \"system\"\n\
         cause = \"successor-enabled\"\nat = \"2026-10-01T00:00:00Z\"\n\
         replaced_by = { job = \"nightly-v2\", identity = \"job-2\" }\n\n\
         [acme.sync.other]\nlifecycle = \"enabled\"\n",
    )?;
    host.authoring().state.update(|jobs| *jobs = state)?;

    let rows = host.authoring().list()?;

    assert_eq!(row(&rows, "nightly")?.replaced_by.as_deref(), Some("nightly-v2"));
    assert_eq!(row(&rows, "other")?.replaced_by, None);
    Ok(())
}
