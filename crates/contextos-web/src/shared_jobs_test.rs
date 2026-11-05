use contextos_workflows::{JobVersion, PredecessorKey};
use tempfile::tempdir;

use super::*;

fn key() -> SharedJobKey {
    SharedJobKey {
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    }
}

fn load(source: &str) -> Result<SharedJob, SharedJobError> {
    SharedJob::try_from(SharedJobSource {
        key: key(),
        text: source,
    })
}

#[test]
fn a_manifest_with_only_a_name_a_timeout_and_a_version_is_a_valid_job() -> Result<(), Box<dyn std::error::Error>> {
    let job = load("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;

    assert_eq!(job.name, "Nightly");
    assert_eq!(job.key, key());
    assert_eq!(job.timeout_secs, 60);
    assert_eq!(job.release.version, JobVersion::try_from("1.0.0")?);
    assert!(job.inputs.is_empty());
    Ok(())
}

#[test]
fn every_documented_field_is_accepted() -> Result<(), Box<dyn std::error::Error>> {
    let job = load(
        "name = \"N\"\nslug = \"nightly-2\"\ntimeout_secs = 90\nversion = \"1.2.0\"\n[inputs]\nsince = \"2026-01-01\"\n",
    )?;

    assert_eq!(job.key.job, "nightly-2");
    assert_eq!(job.timeout_secs, 90);
    assert_eq!(job.release.version, JobVersion::try_from("1.2.0")?);
    assert_eq!(job.inputs.get("since"), Some(&serde_json::json!("2026-01-01")));
    Ok(())
}

#[test]
fn the_slug_defaults_to_the_file_stem() -> Result<(), SharedJobError> {
    assert_eq!(
        load("name = \"N\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?.key.job,
        "nightly"
    );
    Ok(())
}

#[test]
fn a_shared_job_has_no_owner_field_of_its_own() {
    assert!(matches!(
        load("name = \"N\"\ntimeout_secs = 60\nversion = \"1.0.0\"\nowner = \"x\"\n"),
        Err(SharedJobError::Schema(_))
    ));
}

#[test]
fn a_shared_job_with_a_schedule_key_is_refused_as_an_unknown_key() {
    let refused = load("name = \"N\"\ntimeout_secs = 60\nversion = \"1.0.0\"\nschedule = \"0 3 * * *\"\n");
    assert!(
        matches!(&refused, Err(SharedJobError::Schema(message)) if message.contains("unknown field `schedule`")),
        "{refused:?}"
    );
}

#[test]
fn a_blank_name_is_rejected() {
    assert!(matches!(
        load("name = \" \"\ntimeout_secs = 60\nversion = \"1.0.0\"\n"),
        Err(SharedJobError::BlankName)
    ));
}

#[test]
fn a_slug_that_is_not_url_safe_is_rejected() {
    assert!(matches!(
        load("name = \"N\"\ntimeout_secs = 60\nversion = \"1.0.0\"\nslug = \"../x\"\n"),
        Err(SharedJobError::InvalidSlug { .. })
    ));
}

#[test]
fn a_job_slug_the_state_file_uses_for_a_workflows_versions_is_rejected() {
    for reserved in ["current", "previous", "future"] {
        let refused = load(&format!(
            "name = \"N\"\ntimeout_secs = 60\nversion = \"1.0.0\"\nslug = \"{reserved}\"\n"
        ));
        assert!(
            matches!(refused, Err(SharedJobError::InvalidSlug { .. })),
            "{reserved}: {refused:?}"
        );
    }
}

#[test]
fn a_job_file_at_the_documented_path_is_found_with_nothing_else_configured() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let dir = root.path().join("acme").join("sync").join("jobs");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("nightly.toml"),
        "name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n",
    )?;

    let job = load_shared_job(root.path(), &key())?;

    assert_eq!(job.name, "Nightly");
    Ok(())
}

#[test]
fn a_missing_job_file_is_reported_as_not_found() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;

    assert!(matches!(
        load_shared_job(root.path(), &key()),
        Err(SharedJobError::NotFound { .. })
    ));
    Ok(())
}

#[test]
fn a_job_file_is_never_read_from_outside_the_jobs_root() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let escaping = SharedJobKey {
        workflow: "..".to_owned(),
        ..key()
    };

    assert!(matches!(
        load_shared_job(root.path(), &escaping),
        Err(SharedJobError::InvalidSlug { .. })
    ));
    Ok(())
}

#[test]
fn a_job_file_larger_than_any_manifest_needs_is_refused_unread() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let dir = root.path().join("acme").join("sync").join("jobs");
    std::fs::create_dir_all(&dir)?;
    let padding = "#".repeat(usize::try_from(MAX_JOB_MANIFEST_BYTES)? + 1);
    std::fs::write(dir.join("nightly.toml"), format!("name = \"N\"\n{padding}"))?;

    assert!(matches!(
        load_shared_job(root.path(), &key()),
        Err(SharedJobError::Unusable(BoundedReadError::TooLarge { .. }))
    ));
    Ok(())
}

#[test]
fn a_shared_job_may_set_its_own_log_level_and_rejects_an_unknown_one() -> Result<(), SharedJobError> {
    assert_eq!(
        load("name = \"N\"\ntimeout_secs = 60\nversion = \"1.0.0\"\nlog_level = \"warn\"\n")?.log_level,
        Some(contextos_workflows::LogLevel::Warn)
    );
    assert_eq!(
        load("name = \"N\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?.log_level,
        None
    );
    assert!(matches!(
        load("name = \"N\"\ntimeout_secs = 60\nversion = \"1.0.0\"\nlog_level = \"loud\"\n"),
        Err(SharedJobError::Schema(_))
    ));
    Ok(())
}

#[test]
fn a_job_file_says_it_is_ready_with_a_boolean_and_is_not_ready_by_default() -> Result<(), SharedJobError> {
    assert!(!load("name = \"N\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?.ready);
    assert!(load("name = \"N\"\ntimeout_secs = 60\nversion = \"1.0.0\"\nready = true\n")?.ready);
    assert!(matches!(
        load("name = \"N\"\ntimeout_secs = 60\nversion = \"1.0.0\"\nready = \"yes\"\n"),
        Err(SharedJobError::Schema(_))
    ));
    Ok(())
}

fn write_job(root: &std::path::Path, relative: &str, text: &str) -> std::io::Result<()> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)
}

#[test]
fn every_job_file_in_the_store_is_listed_with_the_key_its_path_gives_it() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    write_job(root.path(), "acme/sync/jobs/nightly.toml", "name = \"A\"\n")?;
    write_job(root.path(), "acme/sync/jobs/weekly.toml", "name = \"B\"\n")?;
    write_job(root.path(), "globex/export/jobs/daily.toml", "name = \"C\"\n")?;

    let listed = list_shared_job_files(root.path())?;

    let keys: Vec<String> = listed
        .iter()
        .map(|file| format!("{}/{}/{}", file.key.package, file.key.workflow, file.key.job))
        .collect();
    assert_eq!(keys, ["acme/sync/nightly", "acme/sync/weekly", "globex/export/daily"]);
    assert_eq!(listed[0].text, "name = \"A\"\n");
    Ok(())
}

#[test]
fn files_that_are_not_job_manifests_are_not_listed() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    write_job(root.path(), "acme/sync/jobs/nightly.toml", "name = \"A\"\n")?;
    write_job(root.path(), "acme/sync/jobs/notes.md", "not a job")?;
    write_job(root.path(), "acme/stray.toml", "name = \"too shallow\"\n")?;
    write_job(root.path(), "acme/sync/jobs/deeper/extra.toml", "name = \"too deep\"\n")?;
    write_job(root.path(), "Bad Name/sync/nightly.toml", "name = \"unsafe package\"\n")?;

    let listed = list_shared_job_files(root.path())?;

    assert_eq!(listed.len(), 1, "{listed:?}");
    Ok(())
}

#[test]
fn a_store_with_no_jobs_directory_lists_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;

    let files = list_shared_job_files(&root.path().join("jobs"))?;
    assert!(
        files.is_empty(),
        "the jobs directory should hold no shared job files, got {files:?}"
    );
    Ok(())
}

#[test]
fn a_job_is_tracked_under_the_slug_its_manifest_names_and_a_slug_that_is_not_url_safe_has_no_key() {
    let named = SharedJobFile {
        key: key(),
        text: "name = \"N\"\nslug = \"renamed\"\n".to_owned(),
    };
    let unsafe_slug = SharedJobFile {
        key: key(),
        text: "name = \"N\"\nslug = \"../escape\"\n".to_owned(),
    };
    let plain = SharedJobFile {
        key: key(),
        text: "name = \"N\"\n".to_owned(),
    };

    assert_eq!(
        named.tracked_key().map(|tracked| tracked.job),
        Some("renamed".to_owned())
    );
    assert_eq!(unsafe_slug.tracked_key(), None);
    assert_eq!(plain.tracked_key(), Some(key()));
}

#[test]
fn two_job_files_that_would_share_one_state_key_are_both_set_aside() {
    let file = |stem: &str, text: &str| SharedJobFile {
        key: SharedJobKey {
            package: "acme".to_owned(),
            workflow: "sync".to_owned(),
            job: stem.to_owned(),
        },
        text: text.to_owned(),
    };
    let files = vec![
        file("a", "name = \"A\"\nslug = \"b\"\n"),
        file("b", "name = \"B\"\n"),
        file("c", "name = \"C\"\n"),
        file("d", "name = \"D\"\nslug = \"same\"\n"),
        file("e", "name = \"E\"\nslug = \"same\"\n"),
    ];

    let kept = without_clashing_keys(files);

    let stems: Vec<&str> = kept.iter().map(|file| file.key.job.as_str()).collect();
    assert_eq!(
        stems,
        ["c"],
        "a job whose key another file also claims is ambiguous, so neither is tracked"
    );
}

#[test]
fn files_with_distinct_keys_are_all_kept_in_order() {
    let file = |stem: &str| SharedJobFile {
        key: SharedJobKey {
            package: "acme".to_owned(),
            workflow: "sync".to_owned(),
            job: stem.to_owned(),
        },
        text: "name = \"N\"\n".to_owned(),
    };

    let kept = without_clashing_keys(vec![file("a"), file("b")]);

    assert_eq!(kept.len(), 2);
    assert_eq!(kept[0].key.job, "a");
}

#[test]
fn a_missing_shared_job_is_reported_by_its_key_and_not_its_path() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let jobs_root = root.path().join("jobs");

    let message = load_shared_job_file(&jobs_root, &key())
        .err()
        .map(|error| error.to_string())
        .ok_or("a missing job file loaded")?;

    assert!(
        message.contains("acme/sync/nightly"),
        "the job is named by its key: {message}"
    );
    assert!(
        !message.contains(&*root.path().to_string_lossy()),
        "no absolute host path is printed: {message}"
    );
    Ok(())
}

#[test]
fn an_oversized_or_non_text_job_file_is_refused_without_naming_where_the_jobs_folder_is()
-> Result<(), Box<dyn std::error::Error>> {
    let jobs = tempdir()?;
    let dir = jobs.path().join("acme").join("sync").join("jobs");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("big.toml"),
        "x".repeat(usize::try_from(MAX_JOB_MANIFEST_BYTES)? + 1),
    )?;
    std::fs::write(dir.join("binary.toml"), [0xff, 0xfe, 0xfd])?;
    let location = jobs.path().to_string_lossy().into_owned();

    for job in ["big", "binary"] {
        let key = SharedJobKey {
            package: "acme".to_owned(),
            workflow: "sync".to_owned(),
            job: job.to_owned(),
        };

        let error = load_shared_job_file(jobs.path(), &key).err();

        let message = error.map(|error| error.to_string()).unwrap_or_default();
        assert!(!message.is_empty(), "{job}: the file was loaded");
        assert!(
            !message.contains(&location),
            "{job}: the message holds the absolute location: {message}"
        );
    }
    Ok(())
}

#[test]
fn a_shared_job_file_without_timeout_secs_fails_its_load_as_a_schema_violation() {
    let error = load("name = \"Nightly\"\n");

    assert!(
        matches!(&error, Err(SharedJobError::Schema(message)) if message.contains("timeout_secs")),
        "{error:?}"
    );
}

#[test]
fn an_entity_key_in_a_shared_job_file_is_refused_as_unknown() {
    let error = load("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\nentity = \"personal\"\n");

    assert!(
        matches!(&error, Err(SharedJobError::Schema(message)) if message.contains("entity")),
        "{error:?}"
    );
}

const NAMED: &str = "name = \"N\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n";

#[test]
fn a_shared_job_file_without_version_fails_its_load_as_a_schema_violation() {
    let error = load("name = \"Nightly\"\ntimeout_secs = 60\n");

    assert!(
        matches!(&error, Err(SharedJobError::Schema(message)) if message.contains("version")),
        "{error:?}"
    );
}

#[test]
fn the_version_a_job_names_is_the_release_it_runs() -> Result<(), Box<dyn std::error::Error>> {
    let job = load(NAMED)?;

    assert_eq!(job.release.version, JobVersion::try_from("1.0.0")?);
    assert_eq!(job.release.predecessor, None);
    Ok(())
}

#[test]
fn a_version_that_is_not_exact_is_refused_as_an_invalid_artefact() {
    for version in ["^1.2.0", "~1.2.0", "1.2", "1.2.0+build.1", "1.2.0-rc.1", "latest", ""] {
        let error = load(&format!("name = \"N\"\ntimeout_secs = 60\nversion = {version:?}\n"));
        assert!(
            matches!(&error, Err(SharedJobError::InvalidRelease(release)) if release.code() == "validation/artefact-invalid"),
            "{version:?}: {error:?}"
        );
    }
}

#[test]
fn a_predecessor_is_the_key_of_the_job_a_successor_succeeds() -> Result<(), Box<dyn std::error::Error>> {
    let job = load(&format!("{NAMED}predecessor = \"nightly-v1-0-0\"\n"))?;

    assert_eq!(
        job.release.predecessor,
        Some(PredecessorKey::try_from("nightly-v1-0-0")?)
    );
    Ok(())
}

#[test]
fn a_predecessor_that_is_not_a_job_key_is_refused() {
    for predecessor in ["../x", "Nightly", "", "con"] {
        let error = load(&format!("{NAMED}predecessor = {predecessor:?}\n"));
        assert!(
            matches!(&error, Err(SharedJobError::InvalidRelease(_))),
            "{predecessor:?}: {error:?}"
        );
    }
}

#[test]
fn a_libraries_table_is_an_unknown_key_because_a_job_never_chooses_a_library_release() {
    for libraries in ["[libraries]\nacme-text = \"1.4.2\"\n", "[libraries]\n"] {
        let error = load(&format!("{NAMED}{libraries}"));
        assert!(
            matches!(&error, Err(SharedJobError::Schema(message)) if message.contains("libraries")),
            "{libraries:?}: {error:?}"
        );
    }
    let error = load(&format!("libraries = [\"acme-text\"]\n{NAMED}"));
    assert!(matches!(&error, Err(SharedJobError::Schema(_))), "{error:?}");
}

#[test]
fn a_refused_version_names_the_version_and_never_echoes_other_content() {
    let error = load("name = \"secret-name\"\ntimeout_secs = 60\nversion = \"^1.2\"\n[inputs]\ntoken = \"s3cret\"\n")
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();

    assert!(error.contains("^1.2"), "{error}");
    assert!(!error.contains("s3cret") && !error.contains("secret-name"), "{error}");
}
