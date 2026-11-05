use super::*;

use crate::read_fixture::Fixture;

fn attachment(path: &VaultPath) -> AttachmentRequest {
    AttachmentRequest { path: path.clone() }
}

#[test]
fn an_attachment_that_grows_past_the_cap_while_it_is_read_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let path = fixture.write("grows.bin", b"12345678")?;
    let file = fixture.directory.path().join("grows.bin");

    let result = fixture
        .filesystem
        .read_attachment_staged(&attachment(&path), 8, &mut |stage| {
            if stage == ReadStage::Opened {
                let _ = std::fs::write(&file, b"123456789012");
            }
        });

    assert!(
        matches!(result, Err(FsError::TooLarge { maximum: 8, .. })),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn an_attachment_swapped_for_a_larger_file_before_it_is_opened_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let path = fixture.write("swapped.bin", b"1234")?;
    let file = fixture.directory.path().join("swapped.bin");

    let result = fixture
        .filesystem
        .read_attachment_staged(&attachment(&path), 8, &mut |stage| {
            if stage == ReadStage::Inspected {
                let _ = std::fs::write(&file, b"123456789012");
            }
        });

    assert!(
        matches!(result, Err(FsError::TooLarge { maximum: 8, .. })),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn an_attachment_replaced_by_a_directory_before_it_is_opened_is_refused_as_not_a_file()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let path = fixture.write("swapped.bin", b"1234")?;
    let file = fixture.directory.path().join("swapped.bin");

    let result = fixture
        .filesystem
        .read_attachment_staged(&attachment(&path), 8, &mut |stage| {
            if stage == ReadStage::Inspected {
                let _ = std::fs::remove_file(&file);
                let _ = std::fs::create_dir(&file);
            }
        });

    assert!(matches!(result, Err(FsError::NotFile { .. })), "{result:?}");
    Ok(())
}

#[test]
fn a_whole_read_of_a_file_replaced_by_a_directory_before_it_is_opened_is_refused_as_not_a_file()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let path = fixture.write("note.md", b"text")?;
    let file = fixture.directory.path().join("note.md");

    let result = fixture.filesystem.read_text_staged(
        &ReadTextRequest {
            path: path.clone(),
            limit: None,
        },
        None,
        &mut |stage| {
            if stage == ReadStage::Inspected {
                let _ = std::fs::remove_file(&file);
                let _ = std::fs::create_dir(&file);
            }
        },
    );

    assert!(matches!(result, Err(FsError::NotFile { .. })), "{result:?}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_attachment_swapped_for_a_named_pipe_before_it_is_opened_is_refused_without_blocking()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let path = fixture.write("swapped.bin", b"1234")?;
    let file = fixture.directory.path().join("swapped.bin");
    let filesystem = fixture.filesystem.clone();
    let pipe = file.clone();

    let result = crate::read_fixture::finishes_within(&file, move || {
        filesystem.read_attachment_staged(&attachment(&path), 8, &mut |stage| {
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
fn a_whole_read_of_a_file_swapped_for_a_named_pipe_before_it_is_opened_is_refused_without_blocking()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let path = fixture.write("note.md", b"text")?;
    let file = fixture.directory.path().join("note.md");
    let filesystem = fixture.filesystem.clone();
    let pipe = file.clone();

    let result = crate::read_fixture::finishes_within(&file, move || {
        filesystem.read_text_staged(&ReadTextRequest { path, limit: None }, None, &mut |stage| {
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
fn a_whole_read_and_an_attachment_of_a_file_swapped_for_a_socket_are_refused_as_not_a_file()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let path = fixture.write("note.md", b"text")?;
    let file = fixture.directory.path().join("note.md");
    let mut listeners = Vec::new();
    let mut swap = |stage| {
        if stage == ReadStage::Inspected {
            let _ = std::fs::remove_file(&file);
            listeners.push(std::os::unix::net::UnixListener::bind(&file));
        }
    };

    let whole = fixture.filesystem.read_text_staged(
        &ReadTextRequest {
            path: path.clone(),
            limit: None,
        },
        None,
        &mut swap,
    );
    let attached = fixture
        .filesystem
        .read_attachment_staged(&attachment(&path), 8, &mut swap);

    assert!(matches!(whole, Err(FsError::NotFile { .. })), "{whole:?}");
    assert!(matches!(attached, Err(FsError::NotFile { .. })), "{attached:?}");
    Ok(())
}
