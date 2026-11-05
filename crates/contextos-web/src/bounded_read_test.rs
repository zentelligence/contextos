use tempfile::tempdir;

use super::*;

#[test]
fn a_file_within_the_limit_is_read_whole() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("a.toml");
    std::fs::write(&path, "hello")?;

    assert_eq!(read_text_within(&path, 5)?, "hello");
    Ok(())
}

#[test]
fn a_file_over_the_limit_is_refused_without_being_read_whole() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("big.toml");
    std::fs::write(&path, "x".repeat(1000))?;

    let error = read_text_within(&path, 10).err();

    assert!(
        matches!(error, Some(BoundedReadError::TooLarge { limit: 10, .. })),
        "{error:?}"
    );
    Ok(())
}

#[test]
fn a_file_that_is_not_utf8_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("bin");
    std::fs::write(&path, [0xff, 0xfe, 0xfd])?;

    assert!(matches!(
        read_text_within(&path, 100),
        Err(BoundedReadError::NotText { .. })
    ));
    Ok(())
}

#[test]
fn a_missing_file_keeps_its_not_found_kind() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;

    let error = read_text_within(&dir.path().join("none"), 10).err();

    assert!(matches!(&error, Some(BoundedReadError::Io(io)) if io.kind() == std::io::ErrorKind::NotFound));
    Ok(())
}

#[test]
fn a_refusal_never_displays_the_absolute_location_of_the_file() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let big = dir.path().join("big.toml");
    std::fs::write(&big, "x".repeat(1000))?;
    let binary = dir.path().join("binary.toml");
    std::fs::write(&binary, [0xff, 0xfe, 0xfd])?;
    let location = dir.path().to_string_lossy().into_owned();

    for (path, limit) in [(&big, 10), (&binary, 100)] {
        let message = read_text_within(path, limit)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();

        assert!(!message.is_empty(), "{path:?} was read");
        assert!(
            !message.contains(&location),
            "the message holds the absolute location: {message}"
        );
        assert!(
            message.contains("big.toml") || message.contains("binary.toml"),
            "the message still says which file it means: {message}"
        );
    }
    Ok(())
}
