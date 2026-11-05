use super::*;

/// An absolute path on the host running the test.
fn absolute(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

#[test]
fn the_profile_variable_is_taken_before_home() -> Result<(), HostFoldersError> {
    let folders = HostFolders::try_from(HomeVariables {
        user_profile: Some(absolute("profile").into_os_string()),
        home: Some(absolute("home").into_os_string()),
    })?;

    assert_eq!(folders.home(), absolute("profile"));
    Ok(())
}

#[test]
fn home_is_used_when_the_profile_variable_is_unset() -> Result<(), HostFoldersError> {
    let folders = HostFolders::try_from(HomeVariables {
        user_profile: None,
        home: Some(absolute("home").into_os_string()),
    })?;

    assert_eq!(folders.home(), absolute("home"));
    Ok(())
}

#[test]
fn a_relative_value_is_passed_over_rather_than_resolved() -> Result<(), HostFoldersError> {
    let folders = HostFolders::try_from(HomeVariables {
        user_profile: Some("relative/profile".into()),
        home: Some(absolute("home").into_os_string()),
    })?;

    assert_eq!(folders.home(), absolute("home"));
    Ok(())
}

#[test]
fn no_usable_variable_names_no_home() {
    let variables = HomeVariables {
        user_profile: Some("relative".into()),
        home: Some(OsString::new()),
    };

    assert_eq!(HostFolders::try_from(variables), Err(HostFoldersError::NoHomeVariable));
    assert_eq!(
        HostFolders::try_from(HomeVariables::default()),
        Err(HostFoldersError::NoHomeVariable)
    );
}

#[test]
fn the_platform_folders_lie_beneath_the_home_the_variable_names() -> Result<(), HostFoldersError> {
    let folders = HostFolders::try_from(HomeVariables {
        user_profile: Some(absolute("profile").into_os_string()),
        home: None,
    })?;

    assert!(folders.config().starts_with(folders.home()));
    assert!(folders.data_local().starts_with(folders.home()));
    assert!(folders.product_data().starts_with(folders.data_local()));
    assert_ne!(folders.config(), folders.data_local());
    Ok(())
}

/// A folder holding a file called `name`, as a launcher installed on `PATH` would be.
fn installed_file(name: &str) -> std::io::Result<(tempfile::TempDir, PathBuf)> {
    let folder = tempfile::tempdir()?;
    let path = folder.path().join(name);
    std::fs::write(&path, b"launcher")?;
    Ok((folder, path))
}

#[test]
fn a_program_started_by_its_launcher_is_known_by_the_launcher() -> std::io::Result<()> {
    let (_folder, launcher) = installed_file("contextos")?;
    let running = PathBuf::from("/versions/1.0.0/bin/contextos");

    let chosen = launcher_or_running(running, Some(launcher.clone().into_os_string()));

    assert_eq!(chosen, launcher);
    Ok(())
}

#[test]
fn a_program_with_no_launcher_is_known_by_its_own_path() {
    let running = PathBuf::from("/opt/contextos/bin/contextos");

    assert_eq!(launcher_or_running(running.clone(), None), running);
}

// `contextos-web` passes its environment to the `contextos` it starts, so a launcher for one program
// must never be taken for the other's.
#[test]
fn a_launcher_for_another_program_is_ignored() -> std::io::Result<()> {
    let (_folder, launcher) = installed_file("contextos-web")?;
    let running = PathBuf::from("/versions/1.0.0/bin/contextos");

    assert_eq!(
        launcher_or_running(running.clone(), Some(launcher.into_os_string())),
        running
    );
    Ok(())
}

#[test]
fn a_launcher_that_is_not_there_is_ignored() -> std::io::Result<()> {
    let running = PathBuf::from("/versions/1.0.0/bin/contextos");
    let folder = tempfile::tempdir()?;
    let missing = folder.path().join("contextos");

    assert_eq!(
        launcher_or_running(running.clone(), Some(missing.into_os_string())),
        running
    );
    Ok(())
}

#[test]
fn a_relative_launcher_path_is_ignored() {
    let running = PathBuf::from("/versions/1.0.0/bin/contextos");

    assert_eq!(
        launcher_or_running(running.clone(), Some(OsString::from("contextos"))),
        running
    );
}
