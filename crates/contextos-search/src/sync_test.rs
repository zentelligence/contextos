use contextos_core::{Origin, VaultRootInput};
use serde_json::Map;

use super::*;
use crate::{TantivyIndex, TextIndexConfig, TextQuery};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    vault: tempfile::TempDir,
    roots: VaultSet,
    service: TextSearchService<TantivyIndex>,
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?])?;
    let index = TantivyIndex::try_from(TextIndexConfig {
        directory: vault.path().join(".contextos").join("index"),
        writer_lock_wait: TextIndexConfig::DEFAULT_WRITER_LOCK_WAIT,
    })?;
    let service = TextSearchService::from(TextSyncConfig {
        root_id: VaultRootId::try_from(0_usize)?,
        root: vault.path().to_path_buf(),
        excludes: vec![".contextos".to_owned()],
        index,
    });
    Ok(Fixture { vault, roots, service })
}

impl Fixture {
    /// Writes `content` to `relative` and indexes it the way the write pipeline does after a tool's write.
    fn write_through_pipeline(&self, relative: &str, content: &str) -> TestResult {
        std::fs::write(self.vault.path().join(relative), content)?;
        let path = VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw: relative,
        })?;
        let event = OperationEvent {
            kind: OpKind::Modify,
            paths: vec![path],
            origin: Origin::Tool("fs_write_file".to_owned()),
            summary: "test".to_owned(),
            at: OffsetDateTime::UNIX_EPOCH,
        };
        self.service
            .update(&event)
            .map_err(|warning| format!("indexing the write failed: {warning:?}"))?;
        Ok(())
    }

    fn hits(&self, term: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let fields = Map::new();
        let hits = self.service.index().query(&TextQuery {
            query: term,
            path_prefix: None,
            exclude_paths: &[],
            tags: &[],
            fields: &fields,
            limit: 10,
        })?;
        Ok(hits.into_iter().map(|hit| hit.path).collect())
    }
}

#[test]
fn a_refresh_never_puts_back_a_document_older_than_one_written_while_it_ran() -> TestResult {
    let fixture = fixture()?;
    fixture.write_through_pipeline("a.md", "# A\n\nbaseline\n")?;
    // An edit made outside the pipeline, which the refresh then reads.
    std::fs::write(fixture.vault.path().join("a.md"), "# A\n\nexternal\n")?;
    let plan = fixture.service.plan_refresh()?;

    // A tool writes a newer version, and indexes it, before the refresh commits what it read.
    fixture.write_through_pipeline("a.md", "# A\n\nnewest version from a tool\n")?;
    fixture.service.commit_refresh(plan)?;

    assert_eq!(
        fixture.hits("newest")?,
        vec!["a.md".to_owned()],
        "the newer write must survive the refresh"
    );
    assert!(
        fixture.hits("external")?.is_empty(),
        "the refresh must not put back what it read before the write"
    );
    Ok(())
}

#[test]
fn a_refresh_never_removes_a_note_recreated_while_it_ran() -> TestResult {
    let fixture = fixture()?;
    fixture.write_through_pipeline("a.md", "# A\n\nbaseline\n")?;
    std::fs::remove_file(fixture.vault.path().join("a.md"))?;
    let plan = fixture.service.plan_refresh()?;

    fixture.write_through_pipeline("a.md", "# A\n\nrecreated by a tool\n")?;
    fixture.service.commit_refresh(plan)?;

    assert_eq!(
        fixture.hits("recreated")?,
        vec!["a.md".to_owned()],
        "the recreated note must stay indexed"
    );
    Ok(())
}

#[test]
fn a_refresh_indexes_a_note_deleted_while_it_ran_as_removed() -> TestResult {
    let fixture = fixture()?;
    fixture.write_through_pipeline("a.md", "# A\n\nbaseline\n")?;
    std::fs::write(fixture.vault.path().join("a.md"), "# A\n\nexternal\n")?;
    let plan = fixture.service.plan_refresh()?;

    std::fs::remove_file(fixture.vault.path().join("a.md"))?;
    fixture.service.commit_refresh(plan)?;

    assert!(
        fixture.hits("external")?.is_empty(),
        "a note gone by the commit must not be indexed"
    );
    assert!(
        fixture.hits("baseline")?.is_empty(),
        "a note gone by the commit must be removed"
    );
    Ok(())
}
