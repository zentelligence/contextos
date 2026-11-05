use std::ffi::OsString;

use tempfile::tempdir;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn a_launcher_is_copied_where_nothing_is_there() -> TestResult {
    let folder = tempdir()?;
    let launcher = folder.path().join("contextos-launcher");
    fs::write(&launcher, b"launcher")?;
    let target = folder.path().join("contextos");

    assert_eq!(place_launcher(&launcher, &target)?, Placement::Placed);

    assert_eq!(fs::read(target)?, b"launcher");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_copied_launcher_can_still_be_run() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let folder = tempdir()?;
    let launcher = folder.path().join("contextos-launcher");
    fs::write(&launcher, b"launcher")?;
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755))?;
    let target = folder.path().join("contextos");

    place_launcher(&launcher, &target)?;

    assert_eq!(fs::metadata(target)?.permissions().mode() & 0o111, 0o111);
    Ok(())
}

#[test]
fn installing_again_finds_the_launcher_already_in_place() -> TestResult {
    let folder = tempdir()?;
    let launcher = folder.path().join("contextos-launcher");
    fs::write(&launcher, b"launcher")?;
    let target = folder.path().join("contextos");
    fs::write(&target, b"launcher")?;

    assert_eq!(place_launcher(&launcher, &target)?, Placement::AlreadyInPlace);
    Ok(())
}

// Someone may already have a `contextos` of their own there, such as a build from source.
#[test]
fn a_different_file_is_left_alone() -> TestResult {
    let folder = tempdir()?;
    let launcher = folder.path().join("contextos-launcher");
    fs::write(&launcher, b"launcher")?;
    let target = folder.path().join("contextos");
    fs::write(&target, b"something of the operator's")?;

    assert_eq!(place_launcher(&launcher, &target)?, Placement::SomethingElseThere);

    assert_eq!(fs::read(target)?, b"something of the operator's");
    Ok(())
}

#[test]
fn a_failed_copy_leaves_nothing_behind() -> TestResult {
    let folder = tempdir()?;
    let target = folder.path().join("contextos");

    assert!(place_launcher(&folder.path().join("missing"), &target).is_err());

    assert_eq!(fs::read_dir(folder.path())?.count(), 0);
    Ok(())
}

#[test]
fn a_folder_on_the_path_is_found() -> TestResult {
    let folder = tempdir()?;
    let other = tempdir()?;
    let path = std::env::join_paths([other.path(), folder.path()])?;

    assert!(path_holds(Some(&path), folder.path()));
    assert!(!path_holds(Some(&OsString::from(other.path())), folder.path()));
    assert!(!path_holds(None, folder.path()));
    Ok(())
}

#[test]
fn zsh_reads_its_own_profile() {
    let home = Path::new("/home/someone");

    assert_eq!(profile_files(home, Some(OsStr::new("/bin/zsh"))), [home.join(".zshrc")]);
}

#[test]
fn bash_login_shells_read_the_profile_when_there_is_no_bash_profile() -> TestResult {
    let home = tempdir()?;

    assert_eq!(
        profile_files(home.path(), Some(OsStr::new("/usr/bin/bash"))),
        [home.path().join(".bashrc"), home.path().join(".profile")]
    );
    Ok(())
}

// Bash reads only the first login profile it finds, so `.profile` would never be reached.
#[test]
fn bash_login_shells_read_an_existing_bash_profile_first() -> TestResult {
    let home = tempdir()?;
    fs::write(home.path().join(".bash_profile"), "")?;

    assert_eq!(
        profile_files(home.path(), Some(OsStr::new("/bin/bash"))),
        [home.path().join(".bashrc"), home.path().join(".bash_profile")]
    );
    Ok(())
}

#[test]
fn an_unknown_shell_has_no_profile_to_edit() {
    let home = Path::new("/home/someone");

    assert_eq!(
        profile_files(home, Some(OsStr::new("/usr/bin/fish"))),
        Vec::<PathBuf>::new()
    );
    assert_eq!(profile_files(home, None), Vec::<PathBuf>::new());
}

#[test]
fn a_profile_gains_the_folder_and_keeps_what_it_held() -> TestResult {
    let home = tempdir()?;
    let profile = home.path().join(".zshrc");
    fs::write(&profile, "alias ll='ls -l'\n")?;

    assert!(add_to_profile(&profile)?);

    let contents = fs::read_to_string(profile)?;
    assert!(contents.starts_with("alias ll='ls -l'\n"), "{contents}");
    assert!(
        contents.contains(r#"export PATH="$HOME/.local/bin:$PATH""#),
        "{contents}"
    );
    Ok(())
}

#[test]
fn a_missing_profile_is_created() -> TestResult {
    let home = tempdir()?;
    let profile = home.path().join(".zshrc");

    assert!(add_to_profile(&profile)?);

    assert!(profile.is_file());
    Ok(())
}

#[test]
fn a_profile_is_only_changed_once() -> TestResult {
    let home = tempdir()?;
    let profile = home.path().join(".zshrc");

    add_to_profile(&profile)?;
    let once = fs::read_to_string(&profile)?;

    assert!(!add_to_profile(&profile)?);
    assert_eq!(fs::read_to_string(profile)?, once);
    Ok(())
}

#[test]
fn commands_go_in_a_folder_the_operator_owns() -> TestResult {
    let home = tempdir()?;
    let folders = HostFolders::try_from(contextos_host::HomeVariables {
        user_profile: None,
        home: Some(home.path().as_os_str().to_owned()),
    })?;

    let folder = command_folder(&folders);

    assert!(folder.starts_with(home.path()), "{}", folder.display());
    Ok(())
}
