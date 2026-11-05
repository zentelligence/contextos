//! The creation-date port reports the first source that knows a file's creation instant, and none when no source
//! does, so a caller decides its own fallback rather than a source inventing one.

use contextos_core::{
    FirstReportedCreation, NoCreationReports, ReportsCreation, VaultPath, VaultPathInput, VaultRoot, VaultRootInput,
    VaultSet,
};
use tempfile::tempdir;
use time::OffsetDateTime;
use time::macros::datetime;

#[derive(Clone, Copy, Debug)]
struct Reports(Option<OffsetDateTime>);

impl ReportsCreation for Reports {
    fn created_at(&self, _path: &VaultPath) -> Option<OffsetDateTime> {
        self.0
    }
}

fn a_vault_path() -> Result<(tempfile::TempDir, VaultPath), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "note")?;
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?])?;
    let path = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "note.md",
    })?;
    Ok((vault, path))
}

#[test]
fn the_primary_source_wins_when_it_knows_the_date() -> Result<(), Box<dyn std::error::Error>> {
    let (_vault, path) = a_vault_path()?;
    let chain = FirstReportedCreation {
        primary: Reports(Some(datetime!(2025-01-02 03:04:05 UTC))),
        fallback: Reports(Some(datetime!(2026-01-01 00:00:00 UTC))),
    };
    assert_eq!(chain.created_at(&path), Some(datetime!(2025-01-02 03:04:05 UTC)));
    Ok(())
}

#[test]
fn the_fallback_source_answers_when_the_primary_does_not() -> Result<(), Box<dyn std::error::Error>> {
    let (_vault, path) = a_vault_path()?;
    let chain = FirstReportedCreation {
        primary: Reports(None),
        fallback: Reports(Some(datetime!(2026-01-01 00:00:00 UTC))),
    };
    assert_eq!(chain.created_at(&path), Some(datetime!(2026-01-01 00:00:00 UTC)));
    Ok(())
}

#[test]
fn no_date_is_reported_when_no_source_knows_one() -> Result<(), Box<dyn std::error::Error>> {
    let (_vault, path) = a_vault_path()?;
    let chain = FirstReportedCreation {
        primary: Reports(None),
        fallback: NoCreationReports,
    };
    assert_eq!(chain.created_at(&path), None);
    Ok(())
}

#[test]
fn an_absent_source_reports_no_date_and_a_present_one_answers_for_itself() -> Result<(), Box<dyn std::error::Error>> {
    let (_vault, path) = a_vault_path()?;
    let absent: Option<Reports> = None;
    let present = Some(Reports(Some(datetime!(2025-01-02 03:04:05 UTC))));

    assert_eq!(absent.created_at(&path), None);
    assert_eq!(present.created_at(&path), Some(datetime!(2025-01-02 03:04:05 UTC)));
    Ok(())
}
