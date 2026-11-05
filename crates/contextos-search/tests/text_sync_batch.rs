//! A batch of events reaches the text index in as few commits as the ordering of its changes allows: consecutive
//! documents are indexed together, so a whole-vault rebuild costs one commit, not one per file, and removals still
//! take effect in the order the events arrived.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use contextos_core::{
    OpKind, OperationEvent, Origin, UpdatesSearch, VaultPath, VaultPathInput, VaultRoot, VaultRootId, VaultRootInput,
    VaultSet,
};
use contextos_search::{
    IndexEntry, IndexedDocument, IndexesText, ResultControls, SearchError, TantivyIndex, TextBatch, TextHit,
    TextIndexConfig, TextPage, TextQuery, TextSearchService, TextSyncConfig,
};
use serde_json::Map;
use time::OffsetDateTime;

type BoxError = Box<dyn std::error::Error>;

/// The real index, counting how many committed batches and removals reach it.
struct CountingIndex {
    inner: Arc<TantivyIndex>,
    batches: Arc<AtomicUsize>,
    removals: Arc<AtomicUsize>,
    removal_batches: Arc<AtomicUsize>,
}

impl IndexesText for CountingIndex {
    fn index(&self, documents: &[IndexedDocument]) -> Result<(), SearchError> {
        self.batches.fetch_add(1, Ordering::SeqCst);
        self.inner.index(documents)
    }

    fn remove(&self, path: &str) -> Result<(), SearchError> {
        self.removals.fetch_add(1, Ordering::SeqCst);
        self.inner.remove(path)
    }

    fn remove_all(&self, paths: &[String]) -> Result<(), SearchError> {
        self.removal_batches.fetch_add(1, Ordering::SeqCst);
        self.inner.remove_all(paths)
    }

    /// Counts a checked write as one batch of documents and, when it removes anything, one batch of removals.
    fn apply_checked(&self, build: &mut dyn FnMut() -> Result<TextBatch, SearchError>) -> Result<(), SearchError> {
        self.inner.apply_checked(&mut || {
            let batch = build()?;
            if !batch.upserts.is_empty() {
                self.batches.fetch_add(1, Ordering::SeqCst);
            }
            if !batch.removals.is_empty() {
                self.removal_batches.fetch_add(1, Ordering::SeqCst);
            }
            Ok(batch)
        })
    }

    fn query(&self, request: &TextQuery<'_>) -> Result<Vec<TextHit>, SearchError> {
        self.inner.query(request)
    }

    fn query_window(&self, request: &TextQuery<'_>, note_type: Option<&str>) -> Result<TextPage, SearchError> {
        self.inner.query_window(request, note_type)
    }

    fn query_page(&self, request: &TextQuery<'_>, controls: &ResultControls<'_>) -> Result<TextPage, SearchError> {
        self.inner.query_page(request, controls)
    }

    fn entries(&self) -> Result<Vec<IndexEntry>, SearchError> {
        self.inner.entries()
    }
}

struct Fixture {
    vault: tempfile::TempDir,
    roots: VaultSet,
    service: TextSearchService<CountingIndex>,
    index: Arc<TantivyIndex>,
    batches: Arc<AtomicUsize>,
    removals: Arc<AtomicUsize>,
    removal_batches: Arc<AtomicUsize>,
}

fn fixture(excludes: Vec<String>) -> Result<Fixture, BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let batches = Arc::new(AtomicUsize::new(0));
    let removals = Arc::new(AtomicUsize::new(0));
    let removal_batches = Arc::new(AtomicUsize::new(0));
    let shared = Arc::new(TantivyIndex::try_from(TextIndexConfig {
        directory: vault.path().join(".contextos").join("index"),
        writer_lock_wait: TextIndexConfig::DEFAULT_WRITER_LOCK_WAIT,
    })?);
    let index = CountingIndex {
        inner: Arc::clone(&shared),
        batches: Arc::clone(&batches),
        removals: Arc::clone(&removals),
        removal_batches: Arc::clone(&removal_batches),
    };
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![root.clone()])?;
    let service = TextSearchService::from(TextSyncConfig {
        root_id: VaultRootId::try_from(0_usize)?,
        root: root.path().to_path_buf(),
        excludes,
        index,
    });
    Ok(Fixture {
        vault,
        roots,
        service,
        index: shared,
        batches,
        removals,
        removal_batches,
    })
}

impl Fixture {
    fn note(&self, relative: &str, text: &str) -> Result<VaultPath, BoxError> {
        let path = self.vault.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, text)?;
        Ok(VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw: relative,
        })?)
    }

    fn hits(&self, word: &str) -> Result<Vec<String>, BoxError> {
        let fields = Map::new();
        let hits = self.index.query(&TextQuery {
            query: word,
            path_prefix: None,
            exclude_paths: &[],
            tags: &[],
            fields: &fields,
            limit: 100,
        })?;
        let mut paths = hits.into_iter().map(|hit| hit.path).collect::<Vec<_>>();
        paths.sort();
        Ok(paths)
    }
}

fn event(kind: OpKind, paths: Vec<VaultPath>) -> OperationEvent {
    OperationEvent {
        kind,
        paths,
        origin: Origin::Internal("index".to_owned()),
        summary: "test".to_owned(),
        at: OffsetDateTime::UNIX_EPOCH,
    }
}

#[test]
fn many_consecutive_creations_are_committed_as_one_batch() -> Result<(), BoxError> {
    let fixture = fixture(Vec::new())?;
    let events = (0..40)
        .map(|number| {
            fixture
                .note(&format!("area/note-{number:02}.md"), "# Note\n\nshared marker\n")
                .map(|path| event(OpKind::Create, vec![path]))
        })
        .collect::<Result<Vec<_>, _>>()?;

    fixture.service.update_all(&events).map_err(|warning| warning.message)?;

    assert_eq!(
        fixture.batches.load(Ordering::SeqCst),
        1,
        "one commit for the whole batch"
    );
    assert_eq!(fixture.hits("marker")?.len(), 40);
    Ok(())
}

#[test]
fn a_removal_between_creations_takes_effect_in_arrival_order() -> Result<(), BoxError> {
    let fixture = fixture(Vec::new())?;
    let first = fixture.note("a.md", "alpha marker\n")?;
    let second = fixture.note("b.md", "beta marker\n")?;
    let events = vec![
        event(OpKind::Create, vec![first.clone()]),
        event(OpKind::Delete, vec![first]),
        event(OpKind::Create, vec![second]),
    ];
    std::fs::remove_file(fixture.vault.path().join("a.md"))?;

    fixture.service.update_all(&events).map_err(|warning| warning.message)?;

    assert_eq!(fixture.hits("marker")?, ["b.md"]);
    // The batch replays against the disk as it is now: the delete, and the create of a file that no longer exists,
    // each remove the entry, and the surviving file is committed after them.
    assert_eq!(fixture.removals.load(Ordering::SeqCst), 2);
    Ok(())
}

#[test]
fn a_move_removes_the_old_path_and_indexes_the_new_one() -> Result<(), BoxError> {
    let fixture = fixture(Vec::new())?;
    let old = fixture.note("old.md", "moved marker\n")?;
    fixture
        .service
        .update(&event(OpKind::Create, vec![old.clone()]))
        .map_err(|warning| warning.message)?;
    std::fs::rename(fixture.vault.path().join("old.md"), fixture.vault.path().join("new.md"))?;
    let new = VaultPath::try_from(VaultPathInput {
        roots: &fixture.roots,
        raw: "new.md",
    })?;

    fixture
        .service
        .update_all(&[event(OpKind::Move, vec![old, new])])
        .map_err(|warning| warning.message)?;

    assert_eq!(fixture.hits("marker")?, ["new.md"]);
    Ok(())
}

#[test]
fn excluded_and_non_markdown_paths_are_not_indexed() -> Result<(), BoxError> {
    let fixture = fixture(vec!["private".to_owned()])?;
    let hidden = fixture.note("private/secret.md", "hidden marker\n")?;
    let plain = fixture.note("data.txt", "plain marker\n")?;
    let shown = fixture.note("shown.md", "shown marker\n")?;

    fixture
        .service
        .update_all(&[
            event(OpKind::Create, vec![hidden]),
            event(OpKind::Create, vec![plain]),
            event(OpKind::Create, vec![shown]),
        ])
        .map_err(|warning| warning.message)?;

    assert_eq!(fixture.hits("marker")?, ["shown.md"]);
    Ok(())
}

#[test]
fn an_empty_batch_touches_nothing() -> Result<(), BoxError> {
    let fixture = fixture(Vec::new())?;

    fixture.service.update_all(&[]).map_err(|warning| warning.message)?;

    assert_eq!(fixture.batches.load(Ordering::SeqCst), 0);
    Ok(())
}

#[test]
fn one_unreadable_document_does_not_stop_the_rest_of_the_batch_being_indexed() -> Result<(), BoxError> {
    let fixture = fixture(Vec::new())?;
    let before = fixture.note("before.md", "before marker\n")?;
    let unreadable = fixture.note("unreadable.md", "placeholder\n")?;
    std::fs::write(fixture.vault.path().join("unreadable.md"), [0xff_u8, 0xfe, 0xfd])?;
    let after = fixture.note("after.md", "after marker\n")?;

    let outcome = fixture.service.update_all(&[
        event(OpKind::Create, vec![before]),
        event(OpKind::Create, vec![unreadable]),
        event(OpKind::Create, vec![after]),
    ]);

    assert!(outcome.is_err(), "the unreadable document is still reported");
    assert_eq!(
        fixture.hits("marker")?,
        ["after.md", "before.md"],
        "the documents on either side of it are indexed"
    );
    Ok(())
}

#[test]
fn the_first_failure_is_the_one_reported() -> Result<(), BoxError> {
    let fixture = fixture(Vec::new())?;
    let first = fixture.note("first.md", "x\n")?;
    let second = fixture.note("second.md", "x\n")?;
    std::fs::write(fixture.vault.path().join("first.md"), [0xff_u8])?;
    std::fs::write(fixture.vault.path().join("second.md"), [0xfe_u8])?;

    let warning = fixture
        .service
        .update_all(&[event(OpKind::Create, vec![first]), event(OpKind::Create, vec![second])])
        .err()
        .ok_or("both documents are unreadable")?;

    assert!(warning.message.contains("first.md"), "{}", warning.message);
    Ok(())
}

#[test]
fn a_refresh_that_finds_many_deleted_notes_removes_them_in_one_write() -> Result<(), BoxError> {
    let fixture = fixture(Vec::new())?;
    for number in 0..5 {
        fixture.note(&format!("notes/{number}.md"), "# Note\n\nGadget prose.\n")?;
    }
    fixture.service.refresh()?;
    assert_eq!(fixture.hits("gadget")?.len(), 5);
    for number in 0..5 {
        std::fs::remove_file(fixture.vault.path().join(format!("notes/{number}.md")))?;
    }

    let report = fixture.service.refresh()?;

    assert_eq!(report.removed, 5);
    assert_eq!(
        fixture.removal_batches.load(Ordering::SeqCst),
        1,
        "one write for every removal"
    );
    assert_eq!(
        fixture.removals.load(Ordering::SeqCst),
        0,
        "no removal one note at a time"
    );
    assert!(
        fixture.hits("gadget")?.is_empty(),
        "every deleted note is gone from the index"
    );
    Ok(())
}
