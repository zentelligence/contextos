use contextos_search::{FilesystemHitMetadata, HitMetadata, METADATA_READ_LIMIT, ReadsHitMetadata};
use time::OffsetDateTime;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn vault() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    Ok(tempfile::Builder::new().prefix("vault").tempdir()?)
}

fn reader(vault: &tempfile::TempDir) -> FilesystemHitMetadata {
    FilesystemHitMetadata::from(vault.path().to_path_buf())
}

fn write(vault: &tempfile::TempDir, relative: &str, content: &str) -> TestResult {
    let absolute = vault.path().join(relative);
    if let Some(parent) = absolute.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(absolute, content)?;
    Ok(())
}

#[test]
fn every_described_frontmatter_field_is_read_from_the_source_file() -> TestResult {
    let vault = vault()?;
    write(
        &vault,
        "notes/full.md",
        "---\ntitle: Full Note\ndescription: A description\nsummary: A summary\nexcerpt: An excerpt\nstatus: active\n\
         type: task\nupdated: 2026-09-24\n---\n\n# Heading\n\nBody.\n",
    )?;

    let metadata = reader(&vault).read("notes/full.md");

    assert_eq!(metadata.title.as_deref(), Some("Full Note"));
    assert_eq!(metadata.description.as_deref(), Some("A description"));
    assert_eq!(metadata.summary.as_deref(), Some("A summary"));
    assert_eq!(metadata.excerpt.as_deref(), Some("An excerpt"));
    assert_eq!(metadata.status.as_deref(), Some("active"));
    assert_eq!(metadata.note_type.as_deref(), Some("task"));
    assert_eq!(metadata.updated.as_deref(), Some("2026-09-24"));
    Ok(())
}

#[test]
fn modified_is_the_file_modification_time() -> TestResult {
    let vault = vault()?;
    write(&vault, "note.md", "# Note\n")?;
    let expected = OffsetDateTime::from(std::fs::metadata(vault.path().join("note.md"))?.modified()?);

    let metadata = reader(&vault).read("note.md");

    assert_eq!(metadata.modified, Some(expected));
    Ok(())
}

#[test]
fn absent_fields_are_none_and_the_title_falls_back_to_the_first_heading() -> TestResult {
    let vault = vault()?;
    write(&vault, "plain.md", "# First Heading\n\nJust prose.\n")?;

    let metadata = reader(&vault).read("plain.md");

    assert_eq!(metadata.title.as_deref(), Some("First Heading"));
    assert_eq!(metadata.description, None);
    assert_eq!(metadata.summary, None);
    assert_eq!(metadata.excerpt, None);
    assert_eq!(metadata.status, None);
    assert_eq!(metadata.note_type, None);
    assert_eq!(metadata.updated, None);
    Ok(())
}

#[test]
fn a_missing_file_yields_empty_metadata_rather_than_an_error() -> TestResult {
    let vault = vault()?;

    let metadata = reader(&vault).read("gone.md");

    assert_eq!(metadata, HitMetadata::default());
    Ok(())
}

#[test]
fn only_string_frontmatter_values_are_reported() -> TestResult {
    let vault = vault()?;
    write(
        &vault,
        "typed.md",
        "---\nstatus: 3\ndescription:\n  - a\n  - b\nsummary: \"   \"\n---\n\nBody.\n",
    )?;

    let metadata = reader(&vault).read("typed.md");

    assert_eq!(metadata.status, None, "a number is not a status string");
    assert_eq!(metadata.description, None, "a list is not a description string");
    assert_eq!(metadata.summary, None, "a blank string is treated as absent");
    Ok(())
}

#[test]
fn a_path_that_climbs_out_of_the_vault_yields_empty_metadata() -> TestResult {
    let parent = tempfile::tempdir()?;
    let vault_root = parent.path().join("vault");
    std::fs::create_dir(&vault_root)?;
    std::fs::write(parent.path().join("outside.md"), "---\ndescription: secret\n---\n")?;
    let reader = FilesystemHitMetadata::from(vault_root);

    let metadata = reader.read("../outside.md");

    assert_eq!(metadata, HitMetadata::default());
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_symlink_pointing_outside_the_vault_yields_empty_metadata() -> TestResult {
    let parent = tempfile::tempdir()?;
    let vault_root = parent.path().join("vault");
    std::fs::create_dir(&vault_root)?;
    std::fs::write(parent.path().join("outside.md"), "---\ndescription: secret\n---\n")?;
    std::os::unix::fs::symlink(parent.path().join("outside.md"), vault_root.join("link.md"))?;
    let reader = FilesystemHitMetadata::from(vault_root);

    let metadata = reader.read("link.md");

    assert_eq!(metadata.description, None);
    assert_eq!(metadata, HitMetadata::default());
    Ok(())
}

#[test]
fn a_large_note_with_a_small_frontmatter_block_is_still_read() -> TestResult {
    let vault = vault()?;
    let body = "filler line\n".repeat(METADATA_READ_LIMIT);
    write(
        &vault,
        "big.md",
        &format!("---\ndescription: still found\n---\n\n{body}"),
    )?;

    let metadata = reader(&vault).read("big.md");

    assert_eq!(metadata.description.as_deref(), Some("still found"));
    Ok(())
}

#[test]
fn a_frontmatter_block_larger_than_the_read_limit_is_treated_as_absent() -> TestResult {
    let vault = vault()?;
    let bulk = "x".repeat(METADATA_READ_LIMIT * 2);
    write(
        &vault,
        "hostile.md",
        &format!("---\ndescription: {bulk}\nstatus: never reached\n---\n\n# Body\n"),
    )?;

    let metadata = reader(&vault).read("hostile.md");

    assert_eq!(metadata.description, None);
    assert_eq!(metadata.status, None);
    Ok(())
}

#[test]
fn metadata_becomes_available_once_a_vault_root_that_was_missing_appears() -> TestResult {
    let parent = tempfile::tempdir()?;
    let vault_root = parent.path().join("vault");
    let reader = FilesystemHitMetadata::from(vault_root.clone());

    assert_eq!(reader.read("note.md"), HitMetadata::default(), "no root yet");

    std::fs::create_dir(&vault_root)?;
    std::fs::write(vault_root.join("note.md"), "---\ndescription: now here\n---\n")?;
    let metadata = reader.read("note.md");

    assert_eq!(metadata.description.as_deref(), Some("now here"));
    Ok(())
}

#[test]
fn a_note_with_unparsable_frontmatter_still_yields_a_title_and_no_metadata() -> TestResult {
    let vault = vault()?;
    write(
        &vault,
        "broken.md",
        "---\ntitle: [unclosed\ndescription: never read\n---\n\n# Real Heading\n",
    )?;

    let metadata = reader(&vault).read("broken.md");

    assert_eq!(metadata.title.as_deref(), Some("Real Heading"));
    assert_eq!(metadata.description, None);
    Ok(())
}

#[test]
fn invalid_utf8_after_the_frontmatter_does_not_stop_it_being_read() -> TestResult {
    let vault = vault()?;
    std::fs::write(
        vault.path().join("binary.md"),
        b"---\ndescription: still readable\n---\n\xff\xfe not text",
    )?;

    let metadata = reader(&vault).read("binary.md");

    assert_eq!(metadata.description.as_deref(), Some("still readable"));
    Ok(())
}

#[test]
fn a_note_with_no_frontmatter_or_heading_is_titled_after_its_file_name() -> TestResult {
    let vault = vault()?;
    write(&vault, "plain-note.md", "Just prose, no heading.\n")?;

    let metadata = reader(&vault).read("plain-note.md");

    assert_eq!(metadata.title.as_deref(), Some("plain note"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_named_pipe_at_a_note_path_yields_empty_metadata_instead_of_blocking() -> TestResult {
    let vault = vault()?;
    let made = std::process::Command::new("mkfifo")
        .arg(vault.path().join("pipe.md"))
        .status();
    if !made.is_ok_and(|status| status.success()) {
        // `mkfifo` is unavailable here, so there is nothing to exercise.
        return Ok(());
    }

    let metadata = reader(&vault).read("pipe.md");

    assert_eq!(metadata, HitMetadata::default());
    Ok(())
}
