use super::*;

#[test]
fn a_token_round_trips_through_its_text() {
    let token = "0123456789abcdef0123456789abcdef";
    let version = FileVersion::try_from(token);
    assert!(matches!(version, Ok(ref version) if <&str>::from(version) == token));
}

#[test]
fn metadata_of_the_same_file_state_gives_the_same_token() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("note.md");
    std::fs::write(&path, b"content")?;

    let first = FileVersion::from(&std::fs::metadata(&path)?);
    let second = FileVersion::from(&std::fs::metadata(&path)?);

    assert_eq!(first, second);
    assert_eq!(<&str>::from(&first).len(), TOKEN_LENGTH);
    Ok(())
}

/// A file state on Unix that differs from another only where a test changes it.
#[cfg(unix)]
const fn unix_state(changed_seconds: i64, changed_nanos: i64) -> FileState {
    FileState {
        size: 7,
        modified_nanos: 1_700_000_000_000_000_000,
        identity: Identity {
            device: 2049,
            inode: 131_072,
            changed_seconds,
            changed_nanos,
        },
    }
}

// The kernel may record a change time only to the tick of a coarse clock, so a test that changes a real file twice in
// quick succession cannot rely on the change time moving. The token's sensitivity is proved on explicit file states
// instead, and the reading of real metadata is proved separately.
#[cfg(unix)]
#[test]
fn a_change_to_the_change_time_nanoseconds_alone_changes_the_token() {
    let before = FileVersion::from(&unix_state(1_700_000_000, 100));
    let after = FileVersion::from(&unix_state(1_700_000_000, 101));

    assert_ne!(before, after);
}

#[cfg(unix)]
#[test]
fn a_change_to_the_change_time_seconds_alone_changes_the_token() {
    let before = FileVersion::from(&unix_state(1_700_000_000, 100));
    let after = FileVersion::from(&unix_state(1_700_000_001, 100));

    assert_ne!(before, after);
}

#[cfg(unix)]
#[test]
fn the_same_file_state_always_gives_the_same_token() {
    assert_eq!(
        FileVersion::from(&unix_state(1_700_000_000, 100)),
        FileVersion::from(&unix_state(1_700_000_000, 100))
    );
}

#[cfg(unix)]
#[test]
fn a_file_state_reads_the_size_modification_time_and_unix_identity_from_metadata()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::MetadataExt;

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("note.md");
    std::fs::write(&path, b"content")?;
    let metadata = std::fs::metadata(&path)?;

    let state = FileState::from(&metadata);

    assert_eq!(state.size, 7);
    assert_eq!(
        state.modified_nanos,
        OffsetDateTime::from(metadata.modified()?).unix_timestamp_nanos()
    );
    assert_eq!(
        state.identity,
        Identity {
            device: metadata.dev(),
            inode: metadata.ino(),
            changed_seconds: metadata.ctime(),
            changed_nanos: metadata.ctime_nsec(),
        }
    );
    assert_eq!(FileVersion::from(&metadata), FileVersion::from(&state));
    Ok(())
}
