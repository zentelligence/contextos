use super::*;

use semver::Version;
use tempfile::tempdir;

type BoxError = Box<dyn std::error::Error>;

/// Saved jobs whose jobs folder is `jobs_root` and whose state folder is `state_dir`, which holds no state file.
fn saved(state_dir: &std::path::Path, jobs_root: &std::path::Path) -> SavedJobs {
    SavedJobs {
        jobs_root: jobs_root.to_path_buf(),
        state: crate::shared_state::SharedStateStore::new(state_dir.join("state")),
    }
}

fn reading(text: &str) -> JobFileReading {
    JobFileReading::from(JobFileSource(text))
}

#[test]
fn a_job_file_naming_an_exact_release_names_it() {
    assert_eq!(
        reading("version = \"1.2.0\"\n"),
        JobFileReading::Names(Version::new(1, 2, 0))
    );
}

#[test]
fn a_job_file_naming_a_range_a_pre_release_or_nothing_names_no_release() {
    assert_eq!(reading("version = \"^1.2\"\n"), JobFileReading::NamesNoRelease);
    assert_eq!(reading("version = \"1.2.0-rc.1\"\n"), JobFileReading::NamesNoRelease);
    assert_eq!(reading("name = \"x\"\n"), JobFileReading::NamesNoRelease);
}

#[test]
fn a_malformed_predecessor_does_not_hide_the_release_a_job_file_names() {
    assert_eq!(
        reading("version = \"1.2.0\"\npredecessor = \"Not A Key!\"\n"),
        JobFileReading::Names(Version::new(1, 2, 0))
    );
    assert_eq!(
        reading("version = \"1.2.0\"\npredecessor = 7\n"),
        JobFileReading::Names(Version::new(1, 2, 0))
    );
}

#[test]
fn a_job_file_that_is_not_toml_cannot_be_understood() {
    assert_eq!(reading("version = = ["), JobFileReading::Unreadable);
}

fn write(root: &std::path::Path, relative: &str, text: &[u8]) -> Result<(), BoxError> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

#[tokio::test]
async fn the_host_reads_each_job_file_by_the_key_it_is_tracked_under() -> Result<(), BoxError> {
    let dir = tempdir()?;
    write(dir.path(), "crm/sync/jobs/nightly.toml", b"version = \"1.0.0\"\n")?;
    write(
        dir.path(),
        "crm/sync/jobs/renamed.toml",
        b"slug = \"weekly\"\nversion = \"1.1.0\"\n",
    )?;
    write(dir.path(), "crm/sync/jobs/broken.toml", b"= not toml")?;
    write(dir.path(), "crm/sync/jobs/binary.toml", &[0xff, 0xfe, 0x00])?;
    write(dir.path(), "crm/sync/jobs/Not_A_Slug.toml", b"version = \"1.0.0\"\n")?;
    let home = HostHome::of(
        contextos_workflows::JobsState::default(),
        &saved(dir.path(), dir.path()),
    );

    let mut files = home.job_files().await?;
    files.sort_by(|a, b| a.job.cmp(&b.job));

    let seen: Vec<(&str, &JobFileReading)> = files.iter().map(|file| (file.job.as_str(), &file.reading)).collect();
    assert_eq!(
        seen,
        [
            ("binary", &JobFileReading::Unreadable),
            ("broken", &JobFileReading::Unreadable),
            ("nightly", &JobFileReading::Names(Version::new(1, 0, 0))),
            ("weekly", &JobFileReading::Names(Version::new(1, 1, 0))),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_host_with_no_jobs_folder_has_no_job_files() -> Result<(), BoxError> {
    let dir = tempdir()?;
    let home = HostHome::of(
        contextos_workflows::JobsState::default(),
        &saved(dir.path(), &dir.path().join("absent")),
    );

    assert_eq!(home.job_files().await?.len(), 0);
    Ok(())
}

#[tokio::test]
async fn a_vault_reads_each_job_file_under_its_packages_and_workflows() -> Result<(), BoxError> {
    let world = crate::source_install_world::World::new()?;
    world.write("registry/workflows/crm/sync/jobs/nightly.toml", "version = \"1.0.0\"\n")?;
    world.write("registry/workflows/crm/sync/jobs/broken.toml", "= not toml")?;
    world.write("registry/workflows/crm/sync/jobs/notes.txt", "version = \"9.9.9\"\n")?;
    world.write(
        "registry/workflows/crm/report/jobs/weekly.toml",
        "version = \"2.0.0\"\n",
    )?;
    world.write(
        "registry/workflows/Not_A_Package/sync/jobs/ignored.toml",
        "version = \"1.0.0\"\n",
    )?;
    let client = world.session().await?;
    let home = VaultHome {
        client: &client,
        vault: "vault",
    };

    let mut files = home.job_files().await?;
    files.sort_by(|a, b| (&a.workflow, &a.job).cmp(&(&b.workflow, &b.job)));

    let seen: Vec<(String, String, JobFileReading)> = files
        .into_iter()
        .map(|file| (file.workflow, file.job, file.reading))
        .collect();
    assert_eq!(
        seen,
        [
            (
                "report".to_owned(),
                "weekly".to_owned(),
                JobFileReading::Names(Version::new(2, 0, 0))
            ),
            ("sync".to_owned(), "broken".to_owned(), JobFileReading::Unreadable),
            (
                "sync".to_owned(),
                "nightly".to_owned(),
                JobFileReading::Names(Version::new(1, 0, 0))
            ),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_vault_with_no_workflows_has_no_job_files_and_its_state_is_empty() -> Result<(), BoxError> {
    let world = crate::source_install_world::World::new()?;
    let client = world.session().await?;
    let home = VaultHome {
        client: &client,
        vault: "vault",
    };

    assert_eq!(home.job_files().await?.len(), 0);
    assert_eq!(
        home.state().await?,
        StateReading {
            state: contextos_workflows::JobsState::default(),
            recorded: false
        }
    );
    Ok(())
}

#[tokio::test]
async fn a_vault_whose_server_cannot_be_asked_cannot_say_what_names_its_releases() -> Result<(), BoxError> {
    let world = crate::source_install_world::World::new()?;
    let client = world.session().await?;
    let home = VaultHome {
        client: &client,
        vault: "no-such-vault",
    };

    assert!(matches!(home.job_files().await, Err(RetentionError::Server(_))));
    Ok(())
}
