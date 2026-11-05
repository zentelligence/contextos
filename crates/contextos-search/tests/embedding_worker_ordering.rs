//! The embedding worker never commits vectors for content older than what the note holds by the time they are
//! written: a note that changes while its chunks are being embedded, in this instance or another, is embedded again
//! from its new content, and a note that reappears before its removal is committed keeps its vectors.

use std::collections::BTreeMap;
use std::error::Error;
use std::sync::{Arc, Mutex, PoisonError};

use contextos_core::SystemClock;
use contextos_search::{
    Chunk, DEFAULT_WRITE_WAIT, EmbeddingWorker, EmbeddingWorkerConfig, EmbedsText, PathEmbeddingOutcome,
    ReadsChunkSource, SearchError, SqliteVecConfig, SqliteVecStore, StoresVectors,
};

type TestResult = Result<(), Box<dyn Error>>;

/// Vault content shared between the worker and the test, which edits it while the worker runs.
#[derive(Clone, Default)]
struct SharedContent(Arc<Mutex<BTreeMap<String, String>>>);

impl SharedContent {
    fn set(&self, path: &str, text: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.to_owned(), text.to_owned());
    }

    fn remove(&self, path: &str) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(path);
    }
}

impl ReadsChunkSource for SharedContent {
    fn read(&self, path: &str) -> Result<Option<String>, SearchError> {
        Ok(self.0.lock().unwrap_or_else(PoisonError::into_inner).get(path).cloned())
    }
}

/// An embedder that, the first time it is called, runs `during_first_embed`: an edit landing while a note's chunks
/// are being embedded. Counts the chunks it embeds.
struct RacingEmbedder {
    during_first_embed: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    embedded: Mutex<usize>,
}

impl RacingEmbedder {
    fn new(during_first_embed: impl FnOnce() + Send + 'static) -> Self {
        Self {
            during_first_embed: Mutex::new(Some(Box::new(during_first_embed))),
            embedded: Mutex::new(0),
        }
    }
}

impl EmbedsText for RacingEmbedder {
    fn embed(&self, chunks: &[Chunk]) -> Result<Vec<Vec<f32>>, SearchError> {
        let edit = self
            .during_first_embed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(edit) = edit {
            edit();
        }
        *self.embedded.lock().unwrap_or_else(PoisonError::into_inner) += chunks.len();
        Ok(chunks.iter().map(|_| vec![1.0, 0.0, 0.0, 0.0]).collect())
    }

    fn dimension(&self) -> Option<usize> {
        Some(4)
    }
}

type Worker<E> = EmbeddingWorker<SqliteVecStore, E, SharedContent, SystemClock>;

fn worker<E: EmbedsText>(
    root: &tempfile::TempDir,
    state: &tempfile::TempDir,
    embedder: E,
    content: SharedContent,
) -> Result<Worker<E>, Box<dyn Error>> {
    let store = SqliteVecStore::try_from(SqliteVecConfig {
        path: state.path().join("vectors.db"),
        dimension: 4,
        write_wait: DEFAULT_WRITE_WAIT,
    })?;
    Ok(EmbeddingWorker::try_from(EmbeddingWorkerConfig {
        root: root.path().to_path_buf(),
        store,
        embedder,
        content,
        clock: SystemClock,
    })?)
}

fn vault() -> Result<(tempfile::TempDir, tempfile::TempDir), Box<dyn Error>> {
    Ok((
        tempfile::Builder::new().prefix("vault").tempdir()?,
        tempfile::tempdir()?,
    ))
}

/// How many chunks a fresh pass over `path` would still have to embed: none when the store already holds the
/// note's current content.
fn chunks_still_to_embed(
    root: &tempfile::TempDir,
    state: &tempfile::TempDir,
    content: &SharedContent,
    path: &str,
) -> Result<usize, Box<dyn Error>> {
    let check = worker(root, state, RacingEmbedder::new(|| {}), content.clone())?;
    check.enqueue(path);
    match check.process_one() {
        Some(PathEmbeddingOutcome::Embedded { embedded, .. }) => Ok(embedded),
        other => Err(format!("expected the check pass to embed, got {other:?}").into()),
    }
}

#[test]
fn a_note_edited_while_its_chunks_are_embedded_is_embedded_again_from_the_new_content() -> TestResult {
    let (root, state) = vault()?;
    let content = SharedContent::default();
    content.set("a.md", "# A\n\nThe first version.\n");
    let edit = content.clone();
    let worker = worker(
        &root,
        &state,
        RacingEmbedder::new(move || edit.set("a.md", "# A\n\nA second version, written meanwhile.\n")),
        content.clone(),
    )?;

    worker.enqueue("a.md");
    let outcomes = worker.drain();

    assert!(
        matches!(outcomes.first(), Some(PathEmbeddingOutcome::Superseded { path }) if path == "a.md"),
        "the first pass must not commit what it read before the edit: {outcomes:?}"
    );
    assert!(
        matches!(outcomes.last(), Some(PathEmbeddingOutcome::Embedded { path, .. }) if path == "a.md"),
        "the note must be embedded again from its new content: {outcomes:?}"
    );
    assert_eq!(worker.status().pending, 0);
    assert_eq!(
        chunks_still_to_embed(&root, &state, &content, "a.md")?,
        0,
        "the store must hold the new content"
    );
    Ok(())
}

#[test]
fn a_note_recreated_before_its_removal_is_committed_keeps_its_vectors() -> TestResult {
    let (root, state) = vault()?;
    let content = SharedContent::default();
    content.set("a.md", "# A\n\nKept.\n");
    let first = worker(&root, &state, RacingEmbedder::new(|| {}), content.clone())?;
    first.enqueue("a.md");
    first.drain();

    // A pass that reads the note as deleted, then finds it recreated by the time it commits the removal.
    let recreated = content.clone();
    let reader = PausingSource {
        inner: content.clone(),
        after_first_read: Mutex::new(Some(Box::new(move || recreated.set("a.md", "# A\n\nKept.\n")))),
    };
    content.remove("a.md");
    let second = EmbeddingWorker::try_from(EmbeddingWorkerConfig {
        root: root.path().to_path_buf(),
        store: SqliteVecStore::try_from(SqliteVecConfig {
            path: state.path().join("vectors.db"),
            dimension: 4,
            write_wait: DEFAULT_WRITE_WAIT,
        })?,
        embedder: RacingEmbedder::new(|| {}),
        content: reader,
        clock: SystemClock,
    })?;
    second.enqueue("a.md");
    second.drain();

    assert!(
        second.store().stats()?.chunks > 0,
        "the recreated note's vectors must not be removed"
    );
    assert_eq!(chunks_still_to_embed(&root, &state, &content, "a.md")?, 0);
    Ok(())
}

/// A content source that runs `after_first_read` once its first read has returned: a write landing between the
/// worker's read of a note and its commit.
struct PausingSource {
    inner: SharedContent,
    after_first_read: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl ReadsChunkSource for PausingSource {
    fn read(&self, path: &str) -> Result<Option<String>, SearchError> {
        let read = self.inner.read(path);
        let after = self
            .after_first_read
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(after) = after {
            after();
        }
        read
    }
}

/// An embedder that rewrites `path` every time it is called: a note changing faster than it can be embedded.
struct RewritingEmbedder {
    content: SharedContent,
    path: &'static str,
    calls: Mutex<usize>,
}

impl EmbedsText for RewritingEmbedder {
    fn embed(&self, chunks: &[Chunk]) -> Result<Vec<Vec<f32>>, SearchError> {
        let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
        *calls += 1;
        self.content.set(self.path, &format!("# A\n\nVersion {calls}.\n"));
        Ok(chunks.iter().map(|_| vec![1.0, 0.0, 0.0, 0.0]).collect())
    }

    fn dimension(&self) -> Option<usize> {
        Some(4)
    }
}

#[test]
fn a_note_rewritten_faster_than_it_can_be_embedded_is_left_queued_rather_than_holding_the_pass() -> TestResult {
    let (root, state) = vault()?;
    let content = SharedContent::default();
    content.set("a.md", "# A\n\nVersion 0.\n");
    content.set("b.md", "# B\n\nSteady.\n");
    let embedder = RewritingEmbedder {
        content: content.clone(),
        path: "a.md",
        calls: Mutex::new(0),
    };
    let worker = worker(&root, &state, embedder, content.clone())?;
    worker.enqueue("a.md");
    worker.enqueue("b.md");

    let outcomes = worker.drain();

    let superseded = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, PathEmbeddingOutcome::Superseded { path } if path == "a.md"))
        .count();
    assert_eq!(superseded, 3, "{outcomes:?}");
    assert!(
        outcomes
            .iter()
            .any(|outcome| matches!(outcome, PathEmbeddingOutcome::Embedded { path, .. } if path == "b.md")),
        "the other queued note must still be embedded: {outcomes:?}"
    );
    assert_eq!(
        worker.status().pending,
        1,
        "the rewritten note stays queued for the next pass"
    );
    Ok(())
}
