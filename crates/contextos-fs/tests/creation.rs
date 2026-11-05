//! A file's filesystem birth time is reported through the creation-date port, and a path the filesystem cannot
//! describe reports none.

use contextos_core::{ReportsCreation, VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet};
use contextos_fs::{Filesystem, FilesystemConfig, FsLimits, default_hidden_patterns};
use tempfile::tempdir;
use time::OffsetDateTime;

type BoxError = Box<dyn std::error::Error>;

fn filesystem_over(path: &std::path::Path) -> Result<(Filesystem, VaultSet), BoxError> {
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: path.to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![FsLimits {
            max_read_bytes: 1024,
            max_batch_files: 10,
        }],
        hidden: vec![default_hidden_patterns()],
        atomic_write_guard: None,
    })?;
    Ok((filesystem, roots))
}

#[test]
fn an_existing_file_reports_the_birth_time_the_filesystem_records() -> Result<(), BoxError> {
    let vault = tempdir()?;
    let file = vault.path().join("note.md");
    std::fs::write(&file, "note")?;
    let (filesystem, roots) = filesystem_over(vault.path())?;
    let path = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "note.md",
    })?;

    let reported = filesystem.created_at(&path);

    // Not every platform and filesystem records a birth time; where the platform does not, none is reported.
    let recorded = std::fs::metadata(&file)?.created().ok().map(OffsetDateTime::from);
    assert_eq!(reported, recorded);
    Ok(())
}

#[test]
fn a_file_removed_after_validation_reports_no_creation_date() -> Result<(), BoxError> {
    let vault = tempdir()?;
    let file = vault.path().join("gone.md");
    std::fs::write(&file, "note")?;
    let (filesystem, roots) = filesystem_over(vault.path())?;
    let path = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "gone.md",
    })?;
    std::fs::remove_file(&file)?;

    assert_eq!(filesystem.created_at(&path), None);
    Ok(())
}
