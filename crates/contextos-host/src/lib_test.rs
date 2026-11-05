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
