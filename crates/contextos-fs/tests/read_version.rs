//! The version token every read reports and the `expected_version` a continuing read passes back: the same token
//! while the file is unchanged, a new one when its size, modification time, or identity changes, and a refusal
//! naming the current token before any other check of the request.

mod read_support;

use std::fs::File;
use std::time::{Duration, SystemTime};

use contextos_fs::{FileVersion, FsError, LineRange, ReadLimit, ReadManyRequest, ReadTextRequest};
use read_support::{TestResult, Vault};

/// A fixed modification time, so no test depends on the clock or on the filesystem's timestamp resolution.
fn fixed_time(seconds_after_2023: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seconds_after_2023)
}

fn set_modified(vault: &Vault, name: &str, time: SystemTime) -> TestResult {
    File::options()
        .write(true)
        .open(vault.root.join(name))?
        .set_modified(time)?;
    Ok(())
}

fn version_of(vault: &Vault, name: &str) -> Result<FileVersion, Box<dyn std::error::Error>> {
    Ok(vault.read(&vault.path(name)?, None)?.version)
}

fn token(version: &FileVersion) -> &str {
    version.into()
}

/// Asserts the read was refused because the file changed, naming the version the file has now.
fn assert_refused_as_changed<T: std::fmt::Debug>(
    result: Result<T, FsError>,
    current: &FileVersion,
    context: &str,
) -> TestResult {
    match result {
        Err(FsError::VersionConflict { current_version, .. }) => {
            assert_eq!(&current_version, current, "{context}");
            Ok(())
        }
        other => Err(format!("{context}: expected a version conflict, got {other:?}").into()),
    }
}

#[test]
fn every_read_of_an_unchanged_file_reports_the_same_version() -> TestResult {
    let vault = Vault::new()?;
    let path = vault.write("note.md", b"one\ntwo\nthree\n")?;

    let whole = vault.read(&path, None)?.version;
    let head = vault.read(&path, Some(ReadLimit::Head(1)))?.version;
    let tail = vault.read(&path, Some(ReadLimit::Tail(1)))?.version;
    let range = vault
        .read(&path, Some(ReadLimit::Range(LineRange::try_from((2, 2))?)))?
        .version;
    let part = vault.part(&path, 4, 4)?.version;
    let batch = vault
        .filesystem
        .read_many(ReadManyRequest {
            paths: vec![path.clone()],
        })?
        .into_iter()
        .next()
        .and_then(|item| item.version)
        .ok_or("the batch item carries no version")?;

    assert_eq!(token(&whole).len(), 32, "a read reports a version");
    for (selection, version) in [
        ("head", head),
        ("tail", tail),
        ("range", range),
        ("part", part),
        ("batch", batch),
    ] {
        assert_eq!(version, whole, "{selection}");
    }
    Ok(())
}

#[test]
fn a_version_is_thirty_two_lowercase_hexadecimal_characters() -> TestResult {
    let vault = Vault::new()?;
    vault.write("note.md", b"content")?;

    let version = version_of(&vault, "note.md")?;

    let text = token(&version);
    assert_eq!(text.len(), 32, "{text}");
    assert!(
        text.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{text}"
    );
    Ok(())
}

#[test]
fn a_rewrite_with_a_different_size_changes_the_version() -> TestResult {
    let vault = Vault::new()?;
    vault.write("note.md", b"one")?;
    set_modified(&vault, "note.md", fixed_time(0))?;
    let before = version_of(&vault, "note.md")?;

    vault.write("note.md", b"three")?;
    set_modified(&vault, "note.md", fixed_time(0))?;
    let after = version_of(&vault, "note.md")?;

    assert_ne!(before, after);
    Ok(())
}

#[test]
fn a_rewrite_of_the_same_size_with_a_later_modification_time_changes_the_version() -> TestResult {
    let vault = Vault::new()?;
    vault.write("note.md", b"aaaa")?;
    set_modified(&vault, "note.md", fixed_time(0))?;
    let before = version_of(&vault, "note.md")?;

    vault.write("note.md", b"bbbb")?;
    set_modified(&vault, "note.md", fixed_time(1))?;
    let after = version_of(&vault, "note.md")?;

    assert_ne!(before, after);
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_file_replaced_by_rename_with_the_same_size_and_time_has_a_new_version() -> TestResult {
    let vault = Vault::new()?;
    vault.write("note.md", b"aaaa")?;
    set_modified(&vault, "note.md", fixed_time(0))?;
    let before = version_of(&vault, "note.md")?;

    vault.write("replacement.tmp", b"bbbb")?;
    set_modified(&vault, "replacement.tmp", fixed_time(0))?;
    std::fs::rename(vault.root.join("replacement.tmp"), vault.root.join("note.md"))?;
    let after = version_of(&vault, "note.md")?;

    assert_ne!(
        before, after,
        "a different file at the same path is a different version"
    );
    Ok(())
}

#[test]
fn a_part_with_a_stale_expected_version_is_refused_with_the_current_version() -> TestResult {
    let vault = Vault::new()?;
    let path = vault.write("note.md", b"first version")?;
    let stale = vault.read(&path, None)?.version;
    vault.write("note.md", b"the second, longer version")?;
    let current = version_of(&vault, "note.md")?;

    assert_refused_as_changed(vault.try_part(&path, 0, 4, Some(stale)), &current, "part")
}

#[test]
fn a_whole_or_line_read_with_a_stale_expected_version_is_refused_with_the_current_version() -> TestResult {
    let vault = Vault::new()?;
    let path = vault.write("note.md", b"one\ntwo\n")?;
    let stale = vault.read(&path, None)?.version;
    vault.write("note.md", b"one\ntwo\nthree\n")?;
    let current = version_of(&vault, "note.md")?;

    for limit in [
        None,
        Some(ReadLimit::Head(1)),
        Some(ReadLimit::Tail(1)),
        Some(ReadLimit::Range(LineRange::try_from((1, 2))?)),
    ] {
        let result = vault.filesystem.read_text_if_unchanged(
            &ReadTextRequest {
                path: path.clone(),
                limit,
            },
            &stale,
        );
        assert_refused_as_changed(result, &current, &format!("{limit:?}"))?;
    }
    Ok(())
}

#[test]
fn a_stale_expected_version_is_refused_before_an_offset_the_change_made_invalid() -> TestResult {
    let vault = Vault::new()?;
    let shrunk = vault.write("shrunk.md", b"abcdefgh")?;
    let shrunk_stale = vault.read(&shrunk, None)?.version;
    vault.write("shrunk.md", b"ab")?;
    let shrunk_current = version_of(&vault, "shrunk.md")?;
    let widened = vault.write("widened.md", b"abcdef")?;
    let widened_stale = vault.read(&widened, None)?.version;
    // Offset 1 now falls inside the first "é".
    vault.write("widened.md", "éééé".as_bytes())?;
    let widened_current = version_of(&vault, "widened.md")?;

    assert_refused_as_changed(
        vault.try_part(&shrunk, 4, 4, Some(shrunk_stale)),
        &shrunk_current,
        "an offset now beyond the end",
    )?;
    assert_refused_as_changed(
        vault.try_part(&widened, 1, 4, Some(widened_stale)),
        &widened_current,
        "an offset now inside a character",
    )
}

#[test]
fn a_stale_expected_version_is_refused_before_the_read_maximum_is_applied() -> TestResult {
    let vault = Vault::with_max_read_bytes(8)?;
    let path = vault.write("note.md", b"small")?;
    let stale = vault.read(&path, None)?.version;
    vault.write("note.md", b"now above the read maximum")?;
    // A head read is not bound by the read maximum, so it can report the version the file now has.
    let current = vault.read(&path, Some(ReadLimit::Head(1)))?.version;

    let result = vault.filesystem.read_text_if_unchanged(
        &ReadTextRequest {
            path: path.clone(),
            limit: None,
        },
        &stale,
    );

    assert_refused_as_changed(result, &current, "a whole read of a file that grew above the maximum")
}

#[test]
fn a_read_with_a_matching_expected_version_succeeds() -> TestResult {
    let vault = Vault::new()?;
    let path = vault.write("note.md", b"same content")?;
    let version = vault.read(&path, None)?.version;

    let whole = vault.filesystem.read_text_if_unchanged(
        &ReadTextRequest {
            path: path.clone(),
            limit: None,
        },
        &version,
    )?;
    let part = vault.try_part(&path, 0, 4, Some(version.clone()))?;

    assert_eq!(whole.content, "same content");
    assert_eq!(part.content, "same");
    assert_eq!(part.version, version);
    Ok(())
}

#[test]
fn a_malformed_version_token_is_refused() {
    let thirty_one = "0".repeat(31);
    let thirty_three = "0".repeat(33);
    let uppercase = "ABCDEF0123456789ABCDEF0123456789";
    for malformed in [
        "",
        "abc",
        thirty_one.as_str(),
        thirty_three.as_str(),
        uppercase,
        "0123456789abcdef0123456789abcdeg",
    ] {
        let result = FileVersion::try_from(malformed);
        assert!(
            matches!(result, Err(ref error) if error.code() == "io/invalid-version"),
            "{malformed:?}: {result:?}"
        );
    }
    assert!(FileVersion::try_from("0123456789abcdef0123456789abcdef").is_ok());
}

#[test]
fn a_batch_item_reports_the_size_and_version_of_the_file_it_read() -> TestResult {
    let vault = Vault::new()?;
    let path = vault.write("note.md", b"hello")?;
    let missing = vault.path("missing.md")?;
    let version = vault.read(&path, None)?.version;

    let items = vault.filesystem.read_many(ReadManyRequest {
        paths: vec![path, missing],
    })?;

    let read = items.first().ok_or("no first item")?;
    let failed = items.get(1).ok_or("no second item")?;
    assert_eq!(read.size, Some(5));
    assert_eq!(read.version.as_ref(), Some(&version));
    assert_eq!(
        (failed.size, failed.version.as_ref()),
        (None, None),
        "a failed item has neither"
    );
    Ok(())
}

#[test]
fn a_stale_expected_version_is_refused_before_a_part_that_is_too_short() -> TestResult {
    let vault = Vault::new()?;
    let path = vault.write("note.md", b"abcdefgh")?;
    let stale = vault.read(&path, None)?.version;
    vault.write("note.md", b"abcdefghij")?;
    let current = version_of(&vault, "note.md")?;

    assert_refused_as_changed(
        vault.try_part(&path, 0, 2, Some(stale)),
        &current,
        "a length below the minimum",
    )
}
