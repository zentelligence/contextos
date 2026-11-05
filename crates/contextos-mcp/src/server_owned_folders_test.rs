use std::path::PathBuf;

use super::*;

#[test]
fn the_events_folder_and_the_package_store_are_both_server_owned_with_their_own_codes() {
    let folders = server_owned_folders();
    let code_of = |folder: &str| {
        folders
            .iter()
            .find(|owned| owned.folder == folder)
            .map(|owned| owned.code)
    };
    assert_eq!(code_of("memory/events"), Some("events/server-owned"));
    assert_eq!(code_of(".contextos/packages"), Some("packages/server-owned"));
    assert_eq!(folders.len(), 2);
}

#[test]
fn the_package_store_leaves_nothing_open_and_names_how_to_install_instead() -> Result<(), String> {
    let folders = server_owned_folders();
    let store = folders
        .iter()
        .find(|owned| owned.folder == ".contextos/packages")
        .ok_or("the package store is not server-owned")?;
    assert_eq!(store.open.len(), 0, "{:?}", store.open);
    assert!(store.remediation.contains("contextos-web"), "{}", store.remediation);
    Ok(())
}

#[test]
fn every_server_owned_folder_is_always_a_protected_restore_path() {
    let protected = with_server_owned_restore_paths(vec![PathBuf::from("journal")]);
    assert_eq!(
        protected,
        vec![
            PathBuf::from("journal"),
            PathBuf::from("memory/events"),
            PathBuf::from(".contextos/packages"),
        ]
    );
}

#[test]
fn a_folder_already_covered_by_a_protected_path_is_not_listed_twice() {
    let protected = with_server_owned_restore_paths(vec![PathBuf::from(".contextos"), PathBuf::from("memory/events")]);
    assert_eq!(
        protected,
        vec![PathBuf::from(".contextos"), PathBuf::from("memory/events")]
    );
}
