use tempfile::tempdir;

use super::*;

fn job() -> SharedJobKey {
    SharedJobKey {
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    }
}

#[test]
fn a_claim_is_held_on_a_file_beside_the_job_and_the_file_outlives_it() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;

    let claim = SharedClaim::acquire(root.path(), &job())?;
    let path = root
        .path()
        .join("shared")
        .join("acme")
        .join("sync")
        .join("nightly.lock");
    assert!(path.is_file());

    drop(claim);
    assert!(
        path.is_file(),
        "the claim file is never deleted, so another run cannot race its removal"
    );
    Ok(())
}

#[test]
fn a_second_claim_on_the_same_job_is_refused_while_the_first_is_held() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let first = SharedClaim::acquire(root.path(), &job())?;

    let second = SharedClaim::acquire(root.path(), &job());

    assert!(matches!(second, Err(SharedClaimError::Busy { .. })));
    drop(first);
    Ok(())
}

#[test]
fn the_busy_error_says_the_job_is_running_and_names_the_job_and_not_the_claim_file()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let _first = SharedClaim::acquire(root.path(), &job())?;

    let message = SharedClaim::acquire(root.path(), &job())
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();

    assert!(message.contains("already running"), "{message}");
    assert!(message.contains("nightly"), "{message}");
    assert!(
        !message.contains("nightly.lock"),
        "the claim file is not named: {message}"
    );
    assert!(
        !message.contains("delete"),
        "nobody is asked to clear a live claim: {message}"
    );
    Ok(())
}

#[test]
fn a_claim_file_nobody_holds_is_taken_by_the_next_run() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let dir = root.path().join("shared").join("acme").join("sync");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("nightly.lock"), "")?;

    assert!(SharedClaim::acquire(root.path(), &job()).is_ok());
    Ok(())
}

#[test]
fn different_jobs_never_block_each_other() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let other = SharedJobKey {
        job: "weekly".to_owned(),
        ..job()
    };

    let _first = SharedClaim::acquire(root.path(), &job())?;
    let second = SharedClaim::acquire(root.path(), &other);

    assert!(second.is_ok());
    Ok(())
}

#[test]
fn the_claim_is_released_when_the_holder_panics() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let path = root.path().to_path_buf();

    let result = std::panic::catch_unwind(move || {
        let _claim = SharedClaim::acquire(&path, &job());
        std::panic::resume_unwind(Box::new("run aborted"));
    });

    assert!(result.is_err());
    assert!(SharedClaim::acquire(root.path(), &job()).is_ok());
    Ok(())
}

#[test]
fn a_name_that_could_leave_the_jobs_directory_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let escaping = SharedJobKey {
        package: "..".to_owned(),
        ..job()
    };

    assert!(matches!(
        SharedClaim::acquire(root.path(), &escaping),
        Err(SharedClaimError::InvalidName { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn an_explicit_release_allows_the_next_run_and_keeps_the_file() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let claim = SharedClaim::acquire(root.path(), &job())?;

    claim.release().await?;

    assert!(root.path().join("shared/acme/sync/nightly.lock").is_file());
    assert!(SharedClaim::acquire(root.path(), &job()).is_ok());
    Ok(())
}

#[test]
fn a_name_that_would_name_an_alternate_data_stream_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let stream = SharedJobKey {
        job: "nightly:stream".to_owned(),
        ..job()
    };

    assert!(matches!(
        SharedClaim::acquire(root.path(), &stream),
        Err(SharedClaimError::InvalidName { .. })
    ));
    Ok(())
}
