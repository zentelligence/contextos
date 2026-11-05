use super::*;
use crate::read_fixture::Fixture;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn events_folder() -> ServerOwnedFolders {
    ServerOwnedFolders::from(vec![ServerOwnedFolder {
        folder: "memory/events".to_owned(),
        open: vec!["memory/events/index.md".to_owned()],
        code: "events/server-owned",
        remediation: "Use the recording tool.",
    }])
}

fn tool() -> Origin {
    Origin::Tool("fs_write_file".to_owned())
}

#[test]
fn a_tool_change_at_or_below_the_folder_is_refused_with_the_folders_code() -> TestResult {
    let fixture = Fixture::new()?;
    for name in ["memory/events", "memory/events/2026/10/x.md", "memory/events/x.md"] {
        let path = fixture.write_path(name)?;
        let refused = events_folder().check(&path, &tool());
        assert!(
            matches!(
                &refused,
                Err(FsError::ServerOwned {
                    code: "events/server-owned",
                    ..
                })
            ),
            "{name}: {refused:?}"
        );
    }
    Ok(())
}

#[test]
fn a_path_the_folder_leaves_open_and_a_sibling_with_a_longer_name_are_allowed() -> TestResult {
    let fixture = Fixture::new()?;
    for name in [
        "memory/events/index.md",
        "memory/events-archive/x.md",
        "memory/event/x.md",
        "memory/x.md",
    ] {
        let path = fixture.write_path(name)?;
        assert!(events_folder().check(&path, &tool()).is_ok(), "{name}");
    }
    Ok(())
}

#[test]
fn an_internal_write_is_never_refused() -> TestResult {
    let fixture = Fixture::new()?;
    let path = fixture.write_path("memory/events/2026/10/x.md")?;
    assert!(
        events_folder()
            .check(&path, &Origin::Internal("events".to_owned()))
            .is_ok()
    );
    Ok(())
}

#[test]
fn the_folder_is_matched_without_regard_to_case_because_a_case_insensitive_file_system_reaches_it_under_any()
-> TestResult {
    let fixture = Fixture::new()?;
    let path = fixture.write_path("Memory/EVENTS/x.md")?;
    assert!(events_folder().check(&path, &tool()).is_err());
    Ok(())
}

#[test]
fn a_filesystem_with_no_server_owned_folders_refuses_nothing() -> TestResult {
    let fixture = Fixture::new()?;
    let path = fixture.write_path("memory/events/x.md")?;
    assert!(ServerOwnedFolders::default().check(&path, &tool()).is_ok());
    Ok(())
}

#[test]
fn a_tool_change_to_a_whole_subtree_is_refused_when_the_folder_lies_inside_it() -> TestResult {
    let fixture = Fixture::new()?;
    for name in ["memory", "Memory", "memory/events", "memory/events/2026"] {
        let path = fixture.write_path(name)?;
        let refused = events_folder().check_subtree(&path, &tool());
        assert!(
            matches!(
                &refused,
                Err(FsError::ServerOwned {
                    code: "events/server-owned",
                    ..
                })
            ),
            "{name}: {refused:?}"
        );
    }
    Ok(())
}

#[test]
fn a_tool_change_to_a_subtree_that_does_not_contain_the_folder_is_allowed() -> TestResult {
    let fixture = Fixture::new()?;
    for name in ["memory/events-archive", "memory/tasks", "other", "memory/event"] {
        let path = fixture.write_path(name)?;
        assert!(events_folder().check_subtree(&path, &tool()).is_ok(), "{name}");
    }
    Ok(())
}

#[test]
fn an_internal_subtree_change_is_never_refused() -> TestResult {
    let fixture = Fixture::new()?;
    let path = fixture.write_path("memory")?;
    assert!(
        events_folder()
            .check_subtree(&path, &Origin::Internal("events".to_owned()))
            .is_ok()
    );
    Ok(())
}

#[test]
fn a_refusal_carries_the_folders_own_remediation() -> TestResult {
    let fixture = Fixture::new()?;
    let path = fixture.write_path("memory/events/x.md")?;
    let refused = events_folder().check(&path, &tool());
    assert!(
        matches!(&refused, Err(error @ FsError::ServerOwned { .. }) if error.remediation() == "Use the recording tool."),
        "{refused:?}"
    );
    Ok(())
}

fn events_and_package_store() -> ServerOwnedFolders {
    ServerOwnedFolders::from(vec![
        ServerOwnedFolder {
            folder: "memory/events".to_owned(),
            open: vec!["memory/events/index.md".to_owned()],
            code: "events/server-owned",
            remediation: "Use the recording tool.",
        },
        ServerOwnedFolder {
            folder: ".contextos/packages".to_owned(),
            open: Vec::new(),
            code: "packages/server-owned",
            remediation: "Use the installer.",
        },
    ])
}

#[test]
fn each_of_several_folders_refuses_with_its_own_code_and_a_folder_with_nothing_open_refuses_everything() -> TestResult {
    let fixture = Fixture::new()?;
    let folders = events_and_package_store();
    for (name, expected) in [
        (".contextos/packages", "packages/server-owned"),
        (".contextos/packages/demo/1.0.0/package.toml", "packages/server-owned"),
        ("memory/events/x.md", "events/server-owned"),
    ] {
        let path = fixture.write_path(name)?;
        let refused = folders.check(&path, &tool());
        assert!(
            matches!(&refused, Err(FsError::ServerOwned { code, .. }) if *code == expected),
            "{name}: {refused:?}"
        );
    }
    Ok(())
}

#[test]
fn the_rest_of_a_parent_folder_stays_writable_but_its_subtree_changes_are_refused() -> TestResult {
    let fixture = Fixture::new()?;
    let folders = events_and_package_store();
    for name in [
        ".contextos/jobs.toml",
        ".contextos/packages-archive/x.toml",
        "registry/jobs/x.md",
    ] {
        let path = fixture.write_path(name)?;
        assert!(folders.check(&path, &tool()).is_ok(), "{name}");
        assert!(folders.check_subtree(&path, &tool()).is_ok(), "{name}");
    }
    let parent = fixture.write_path(".contextos")?;
    assert!(folders.check(&parent, &tool()).is_ok());
    assert!(
        matches!(
            folders.check_subtree(&parent, &tool()),
            Err(FsError::ServerOwned {
                code: "packages/server-owned",
                ..
            })
        ),
        "moving or deleting .contextos takes the package store with it"
    );
    Ok(())
}
