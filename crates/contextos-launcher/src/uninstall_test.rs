use tempfile::tempdir;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// The temporary folder may be on another drive than the installation, as it is on some Windows machines.
#[test]
fn a_launcher_that_cannot_reach_the_first_folder_moves_to_the_next() -> TestResult {
    let directory = tempdir()?;
    let launcher = directory.path().join("contextos");
    fs::write(&launcher, b"a launcher")?;
    let unreachable = directory.path().join("missing");
    let reachable = directory.path().join("reachable");
    fs::create_dir(&reachable)?;

    assert!(move_out_of_the_way(&launcher, &[unreachable, reachable.clone()]));

    assert!(!launcher.exists());
    assert_eq!(fs::read_dir(reachable)?.count(), 1);
    Ok(())
}

#[test]
fn a_launcher_with_nowhere_to_go_is_reported() -> TestResult {
    let directory = tempdir()?;
    let launcher = directory.path().join("contextos");
    fs::write(&launcher, b"a launcher")?;

    assert!(!move_out_of_the_way(&launcher, &[directory.path().join("missing")]));

    assert!(launcher.exists());
    Ok(())
}
