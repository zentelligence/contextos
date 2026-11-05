use super::*;

#[cfg(unix)]
#[test]
fn hashing_a_named_pipe_is_refused_as_not_a_file_without_blocking() -> Result<(), Box<dyn std::error::Error>> {
    // Hashing is reached with no inspection of the path's type by the conflict check before a write.
    let directory = tempfile::tempdir()?;
    let pipe = directory.path().join("note.md");
    crate::read_fixture::replace_with_pipe(&pipe);
    let hashed = pipe.clone();

    let result = crate::read_fixture::finishes_within(&pipe, move || hash_file(&hashed))?;

    assert!(matches!(result, Err(FsError::NotFile { .. })), "{result:?}");
    Ok(())
}

#[test]
fn hashing_a_regular_file_returns_the_hash_of_its_bytes() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let file = directory.path().join("note.md");
    std::fs::write(&file, b"text")?;

    let hash = hash_file(&file)?;

    assert_eq!(hash, ContentHash::from(<[u8; 32]>::from(Sha256::digest(b"text"))));
    Ok(())
}
