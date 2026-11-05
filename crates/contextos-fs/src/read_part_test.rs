use super::*;

use contextos_core::VaultPath;

#[test]
fn a_continuation_byte_is_not_the_start_of_a_character() {
    assert!(is_continuation(0x80));
    assert!(is_continuation(0xBF));
    assert!(!is_continuation(b'a'));
    assert!(!is_continuation(0xC3));
    assert!(!is_continuation(0xF0));
}

use crate::read_fixture::Fixture;

fn request(path: &VaultPath, offset: u64, length: u64) -> ReadPartRequest {
    ReadPartRequest {
        path: path.clone(),
        part: BytePart::from((offset, length)),
        expected_version: None,
    }
}

#[test]
fn a_file_changed_while_its_part_was_read_is_refused_as_a_conflict() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let path = fixture.write("note.md", b"abcdefgh")?;
    let file = fixture.directory.path().join("note.md");

    let result = fixture
        .filesystem
        .read_part_staged(&request(&path, 0, 4), &mut |stage| {
            if stage == ReadStage::ContentRead {
                let _ = std::fs::write(&file, b"abcdefghij");
            }
        });

    assert!(matches!(result, Err(FsError::VersionConflict { .. })), "{result:?}");
    Ok(())
}

#[test]
fn a_file_replaced_by_a_directory_before_it_is_opened_is_refused_as_not_a_file()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let path = fixture.write("note.md", b"abcdefgh")?;
    let file = fixture.directory.path().join("note.md");

    let result = fixture
        .filesystem
        .read_part_staged(&request(&path, 0, 4), &mut |stage| {
            if stage == ReadStage::Inspected {
                let _ = std::fs::remove_file(&file);
                let _ = std::fs::create_dir(&file);
            }
        });

    assert!(matches!(result, Err(FsError::NotFile { .. })), "{result:?}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_file_swapped_for_a_named_pipe_before_its_part_is_opened_is_refused_without_blocking()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let path = fixture.write("note.md", b"abcdefgh")?;
    let file = fixture.directory.path().join("note.md");
    let filesystem = fixture.filesystem.clone();
    let pipe = file.clone();

    let result = crate::read_fixture::finishes_within(&file, move || {
        filesystem.read_part_staged(&request(&path, 0, 4), &mut |stage| {
            if stage == ReadStage::Inspected {
                crate::read_fixture::replace_with_pipe(&pipe);
            }
        })
    })?;

    assert!(matches!(result, Err(FsError::NotFile { .. })), "{result:?}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_file_swapped_for_a_socket_before_its_part_is_opened_is_refused_as_not_a_file()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let path = fixture.write("note.md", b"abcdefgh")?;
    let file = fixture.directory.path().join("note.md");
    let mut listener = None;

    let result = fixture
        .filesystem
        .read_part_staged(&request(&path, 0, 4), &mut |stage| {
            if stage == ReadStage::Inspected {
                let _ = std::fs::remove_file(&file);
                listener = Some(std::os::unix::net::UnixListener::bind(&file));
            }
        });

    assert!(matches!(result, Err(FsError::NotFile { .. })), "{result:?}");
    Ok(())
}
