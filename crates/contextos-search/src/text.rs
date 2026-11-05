use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use tantivy::directory::Directory as _;
use tantivy::directory::error::LockError;
use tantivy::query::{Occur, Query, TermQuery};
use tantivy::schema::{
    FAST, Facet, FacetOptions, Field, IndexRecordOption, JsonObjectOptions, OwnedValue, STORED, STRING, Schema, TEXT,
    TantivyDocument, Term, TextFieldIndexing, Value as _,
};
use tantivy::snippet::SnippetGenerator;
use tantivy::{DateTime, Index, IndexReader, IndexWriter, ReloadPolicy};
use time::OffsetDateTime;

mod query;

use crate::document::strip_closing_hashes;
use crate::store::{DerivedStore, StoreFailure, StoreHealth, StoreInspection};
use crate::{CANDIDATE_WINDOW, IndexedDocument, ResultControls, SearchError};

/// One ranked full-text search request.
#[derive(Clone, Copy, Debug)]
pub struct TextQuery<'a> {
    pub query: &'a str,
    pub path_prefix: Option<&'a str>,
    /// Forward-slash relative path prefixes to exclude, matching whole path
    /// segments with the same semantics as `path_prefix`:
    /// `exclude_paths = ["old"]` excludes `"old"` itself and everything
    /// under `"old/"`, never `"oldstuff.md"`. A hit is excluded if it
    /// matches any entry. Composable with `path_prefix`: both are applied.
    pub exclude_paths: &'a [String],
    pub tags: &'a [String],
    pub fields: &'a Map<String, Value>,
    pub limit: usize,
}

/// One ranked search hit with a highlighted, heading-aware snippet.
#[derive(Clone, Debug, PartialEq)]
pub struct TextHit {
    pub path: String,
    /// The raw full-text score, comparable only within one result set.
    pub score: f32,
    /// The score as a fraction of the best hit for the same query, from 0
    /// to 1, so a threshold means the same thing for every query.
    pub relevance: f32,
    pub title: String,
    pub snippet: String,
    pub modified: OffsetDateTime,
}

/// One page of ranked hits with the exact number of matches behind it.
#[derive(Clone, Debug, PartialEq)]
pub struct TextPage {
    pub hits: Vec<TextHit>,
    /// Every note matching the query and controls, not only this page.
    pub total: usize,
}

/// Stored identity of one indexed document for freshness comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexEntry {
    pub path: String,
    /// Lowercase SHA-256 hex of the complete source content.
    pub content_hash: String,
    pub modified: OffsetDateTime,
}

/// Documents to upsert and paths to remove in one committed write to the text index.
#[derive(Debug, Default)]
pub struct TextBatch {
    pub upserts: Vec<IndexedDocument>,
    pub removals: Vec<String>,
}

/// Port for the ranked full-text index.
pub trait IndexesText: Send + Sync {
    /// Upserts the given documents in one committed batch.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the index cannot be written.
    fn index(&self, documents: &[IndexedDocument]) -> Result<(), SearchError>;

    /// Removes the document stored under the given relative path.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the index cannot be written.
    fn remove(&self, path: &str) -> Result<(), SearchError>;

    /// Removes the documents stored under each of `paths` in one committed batch.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the index cannot be written.
    fn remove_all(&self, paths: &[String]) -> Result<(), SearchError> {
        for path in paths {
            self.remove(path)?;
        }
        Ok(())
    }

    /// Applies the batch `build` produces in one committed write, calling `build` only once this index's writer lock
    /// is held, so what it reads from the vault cannot be overtaken by another writer before the commit. A rebuild
    /// uses it to check, at commit, that each file it read earlier is still as it read it.
    ///
    /// # Errors
    ///
    /// Returns `build`'s own error, a busy failure when another instance holds the writer lock past the configured
    /// wait, and a storage error when the index cannot be written.
    fn apply_checked(&self, build: &mut dyn FnMut() -> Result<TextBatch, SearchError>) -> Result<(), SearchError>;

    /// Returns ranked hits for the request.
    ///
    /// # Errors
    ///
    /// Returns an invalid-query error for unparsable syntax or filter
    /// values, and a storage error when the index cannot be read.
    fn query(&self, request: &TextQuery<'_>) -> Result<Vec<TextHit>, SearchError>;

    /// Returns the [`CANDIDATE_WINDOW`] most relevant hits, best first, with
    /// the exact match count, ignoring the request's `limit`. This is the
    /// text candidate pool that rank fusion draws from.
    ///
    /// # Errors
    ///
    /// Returns an invalid-query error for unparsable syntax or filter
    /// values, and a storage error when the index cannot be read.
    fn query_window(&self, request: &TextQuery<'_>, note_type: Option<&str>) -> Result<TextPage, SearchError>;

    /// Returns one page of ranked hits with the exact match count.
    ///
    /// # Errors
    ///
    /// Returns an invalid-page error when the page is outside the bounds
    /// [`ResultControls::ensure_within`] enforces, an invalid-query error
    /// for unparsable syntax or filter values, and a storage error when the
    /// index cannot be read.
    fn query_page(&self, request: &TextQuery<'_>, controls: &ResultControls<'_>) -> Result<TextPage, SearchError>;

    /// Returns every stored document identity for freshness comparison.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the index cannot be read.
    fn entries(&self) -> Result<Vec<IndexEntry>, SearchError>;
}

/// The store failure tantivy's `error` amounts to for the index at `location`, when it is damage: a file tantivy reports
/// as corrupt, an index format it cannot read, or a stored schema that does not match this build's. Any other error is
/// not the index's files' fault.
#[must_use]
pub(crate) fn tantivy_damage(location: &str, error: &tantivy::TantivyError) -> Option<StoreFailure> {
    match error {
        tantivy::TantivyError::DataCorruption(_)
        | tantivy::TantivyError::IncompatibleIndex(_)
        | tantivy::TantivyError::SchemaError(_) => {
            Some(StoreFailure::corrupt(DerivedStore::Text, location, error.to_string()))
        }
        _ => None,
    }
}

/// Inspects the text index in `directory` without writing: whether it has been built, whether every file it lists
/// passes its checksum, and how many documents it holds. Opening an index takes no lock and writes nothing; only a
/// writer does.
#[must_use]
pub fn inspect_text_index(directory: &Path) -> StoreInspection {
    let location = directory.display().to_string();
    let inspection = |health, records| StoreInspection {
        store: DerivedStore::Text,
        location: directory.to_path_buf(),
        health,
        records,
    };
    let unreadable = |error: &tantivy::TantivyError| {
        let failure = tantivy_damage(&location, error)
            .unwrap_or_else(|| StoreFailure::io(DerivedStore::Text, location.clone(), error.to_string()));
        inspection(Some(StoreHealth::Unreadable(failure)), 0)
    };
    if !directory.join("meta.json").exists() {
        return inspection(None, 0);
    }
    let index = match Index::open_in_dir(directory) {
        Ok(index) => index,
        Err(error) => return unreadable(&error),
    };
    match index.validate_checksum() {
        Ok(damaged) if !damaged.is_empty() => {
            let failure = StoreFailure::corrupt(
                DerivedStore::Text,
                location.clone(),
                format!("{} index files fail their checksum", damaged.len()),
            );
            return inspection(Some(StoreHealth::Unreadable(failure)), 0);
        }
        Ok(_) => {}
        Err(error) => return unreadable(&error),
    }
    let reader: Result<IndexReader, _> = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into();
    match reader {
        Ok(reader) => inspection(
            Some(StoreHealth::Ready),
            usize::try_from(reader.searcher().num_docs()).unwrap_or(usize::MAX),
        ),
        Err(error) => unreadable(&error),
    }
}

/// Trusted location of the tantivy index directory, and how long a write waits for another writer.
#[derive(Clone, Debug)]
pub struct TextIndexConfig {
    pub directory: PathBuf,
    /// How long a write waits for the index's writer lock while another instance holds it, before failing as
    /// a busy store failure (`index/busy`).
    pub writer_lock_wait: Duration,
}

impl TextIndexConfig {
    /// The writer-lock wait every server and CLI instance uses: the same budget the link graph's `sqlite` store
    /// gives a busy database.
    pub const DEFAULT_WRITER_LOCK_WAIT: Duration = Duration::from_secs(5);
}

#[derive(Clone, Copy, Debug)]
struct TextFields {
    path: Field,
    path_facet: Field,
    title: Field,
    headings: Field,
    body: Field,
    tags: Field,
    frontmatter: Field,
    modified: Field,
    content_hash: Field,
}

/// Default `IndexesText` implementation over a tantivy directory, shared by every `contextos` instance on the vault.
///
/// The exclusive tantivy `IndexWriter` lock is acquired only for the duration of a write (`index`/`remove`), never
/// held for the lifetime of this handle, so any number of instances can open the same index. A write that finds
/// another instance writing waits up to [`TextIndexConfig::writer_lock_wait`] for it, and every read first reloads
/// the reader, so a commit from any instance is visible to the next query here.
pub struct TantivyIndex {
    index: Index,
    reader: IndexReader,
    write_gate: Mutex<()>,
    fields: TextFields,
    directory: PathBuf,
    writer_lock_wait: Duration,
    /// Whether this handle has already removed the segment files earlier abandoned merges left behind; done once,
    /// on its first write, since a write that waits for its merges leaves none of its own.
    orphans_removed: AtomicBool,
}

/// The memory arena one write's `IndexWriter` indexes into.
const WRITER_ARENA_BYTES: usize = 32_000_000;

/// How long a write pauses between attempts at a writer lock another instance holds.
const WRITER_LOCK_RETRY: Duration = Duration::from_millis(20);

/// How old an unlisted segment file must be before the sweep removes it. A build older than this one drops its
/// writer before its merges finish, releasing the writer lock while its merge threads may still be writing, so a
/// segment file written in the last few minutes may still be in use by such an instance.
const ORPHAN_GRACE: Duration = Duration::from_secs(600);

impl TryFrom<TextIndexConfig> for TantivyIndex {
    type Error = SearchError;

    fn try_from(value: TextIndexConfig) -> Result<Self, Self::Error> {
        std::fs::create_dir_all(&value.directory).map_err(|source| SearchError::IndexDirectory {
            path: value.directory.display().to_string(),
            source,
        })?;
        let (schema, fields) = build_schema();
        let directory =
            tantivy::directory::MmapDirectory::open(&value.directory).map_err(tantivy::TantivyError::from)?;
        let index = Index::open_or_create(directory, schema)?;
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
        Ok(Self {
            index,
            reader,
            write_gate: Mutex::new(()),
            fields,
            directory: value.directory,
            writer_lock_wait: value.writer_lock_wait,
            orphans_removed: AtomicBool::new(false),
        })
    }
}

impl IndexesText for TantivyIndex {
    fn index(&self, documents: &[IndexedDocument]) -> Result<(), SearchError> {
        if documents.is_empty() {
            return Ok(());
        }
        self.with_writer(|writer| {
            for document in documents {
                writer.delete_term(Term::from_field_text(self.fields.path, document.path()));
                writer.add_document(stored_document(self.fields, document))?;
            }
            Ok(())
        })
    }

    fn remove(&self, path: &str) -> Result<(), SearchError> {
        self.with_writer(|writer| {
            writer.delete_term(Term::from_field_text(self.fields.path, path));
            Ok(())
        })
    }

    fn remove_all(&self, paths: &[String]) -> Result<(), SearchError> {
        if paths.is_empty() {
            return Ok(());
        }
        self.with_writer(|writer| {
            for path in paths {
                writer.delete_term(Term::from_field_text(self.fields.path, path));
            }
            Ok(())
        })
    }

    fn apply_checked(&self, build: &mut dyn FnMut() -> Result<TextBatch, SearchError>) -> Result<(), SearchError> {
        self.with_writer(|writer| {
            let batch = build()?;
            for path in &batch.removals {
                writer.delete_term(Term::from_field_text(self.fields.path, path));
            }
            for document in &batch.upserts {
                writer.delete_term(Term::from_field_text(self.fields.path, document.path()));
                writer.add_document(stored_document(self.fields, document))?;
            }
            Ok(())
        })
    }

    fn query(&self, request: &TextQuery<'_>) -> Result<Vec<TextHit>, SearchError> {
        Ok(self.run_query(request, &ResultControls::default())?.hits)
    }

    fn query_window(&self, request: &TextQuery<'_>, note_type: Option<&str>) -> Result<TextPage, SearchError> {
        let windowed = TextQuery {
            limit: CANDIDATE_WINDOW,
            ..*request
        };
        self.run_query(
            &windowed,
            &ResultControls {
                note_type,
                ..ResultControls::default()
            },
        )
    }

    fn query_page(&self, request: &TextQuery<'_>, controls: &ResultControls<'_>) -> Result<TextPage, SearchError> {
        controls.ensure_within(request.limit)?;
        self.run_query(request, controls)
    }

    fn entries(&self) -> Result<Vec<IndexEntry>, SearchError> {
        let searcher = self.current_searcher()?;
        let addresses = searcher.search(&tantivy::query::AllQuery, &tantivy::collector::DocSetCollector)?;
        let mut entries = Vec::with_capacity(addresses.len());
        for address in addresses {
            let stored: TantivyDocument = searcher.doc(address)?;
            entries.push(IndexEntry {
                path: stored_text(&stored, self.fields.path),
                content_hash: stored_text(&stored, self.fields.content_hash),
                modified: stored_date(&stored, self.fields.modified),
            });
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(entries)
    }
}

impl TantivyIndex {
    /// Acquires the exclusive tantivy `IndexWriter` lock, runs `mutate`, commits, and lets every merge the commit
    /// started finish before releasing the lock. Waiting for the merges is what keeps the index compact: dropping a
    /// writer abandons its in-flight merges, so a writer per write that is simply dropped adds one segment per write
    /// that is never merged, and every later commit and reload slows with the segment count. Tantivy collects the files
    /// a finished merge no longer needs as part of it.
    ///
    /// `write_gate` serialises this process's own threads; tantivy's directory lock excludes every other instance.
    ///
    /// # Errors
    ///
    /// Returns a busy store failure (`index/busy`) when another instance still holds the writer lock after
    /// `writer_lock_wait`, and a storage error when the write, commit, or merge fails.
    fn with_writer(&self, mutate: impl FnOnce(&mut IndexWriter) -> Result<(), SearchError>) -> Result<(), SearchError> {
        let _gate = self.write_gate.lock().unwrap_or_else(PoisonError::into_inner);
        let mut writer = self.acquire_writer()?;
        if !self.orphans_removed.swap(true, Ordering::Relaxed) {
            self.remove_orphaned_segment_files();
        }
        mutate(&mut writer)?;
        writer.commit()?;
        writer.wait_merging_threads()?;
        Ok(())
    }

    /// Opens a writer, retrying while another instance holds the writer lock, for up to `writer_lock_wait` (a wait
    /// too long to represent waits without limit).
    fn acquire_writer(&self) -> Result<IndexWriter, SearchError> {
        let deadline = Instant::now().checked_add(self.writer_lock_wait);
        loop {
            match self.index.writer_with_num_threads(1, WRITER_ARENA_BYTES) {
                Ok(writer) => return Ok(writer),
                Err(tantivy::TantivyError::LockFailure(LockError::LockBusy, _)) => {
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return Err(StoreFailure::busy(
                            DerivedStore::Text,
                            self.directory.display().to_string(),
                            format!(
                                "another instance held the writer lock for longer than the {} ms a write waits",
                                self.writer_lock_wait.as_millis()
                            ),
                        )
                        .into());
                    }
                    std::thread::sleep(WRITER_LOCK_RETRY);
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Deletes the files of every segment that no commit lists and that has not been written for [`ORPHAN_GRACE`]:
    /// the output of a merge abandoned before it finished, by a writer dropped mid-merge or a process that exited.
    ///
    /// Tantivy's own garbage collection does not remove them. Tantivy records every file it creates in the index's
    /// managed list, but each handle loads that list once, when it opens, and later rewrites it from its own copy.
    /// A file registered by a handle that then abandoned its merge is dropped from the list by the next handle to
    /// rewrite it, and nothing ever collects it.
    ///
    /// Called only while this handle holds the writer lock and before it writes anything, so no writer of this build
    /// has a merge in flight; the grace period covers an older build's merge threads, which outlive its lock.
    /// Tantivy's meta lock is held throughout, as its own garbage collection holds it, so no reader reloads
    /// mid-sweep. Only files named for a tantivy segment (a 32-character hexadecimal segment id, then an extension)
    /// are considered. A file that cannot be removed (on Windows, one another process still has mapped) is left for
    /// a later instance, never failing the write.
    fn remove_orphaned_segment_files(&self) {
        let _meta_lock = match self.index.directory().acquire_lock(&tantivy::directory::META_LOCK) {
            Ok(lock) => lock,
            Err(error) => {
                tracing::warn!(code = "index/storage", %error, "text index meta lock unavailable; abandoned segment files are kept");
                return;
            }
        };
        let live = match self.index.searchable_segment_ids() {
            Ok(ids) => ids
                .iter()
                .map(tantivy::index::SegmentId::uuid_string)
                .collect::<HashSet<_>>(),
            Err(error) => {
                tracing::warn!(code = "index/storage", %error, "text index segments could not be listed; abandoned segment files are kept");
                return;
            }
        };
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(code = "index/storage", %error, "text index directory could not be listed; abandoned segment files are kept");
                return;
            }
        };
        let mut removed = 0_usize;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(segment) = name.to_str().and_then(segment_id_of) else {
                continue;
            };
            if live.contains(segment) || !is_older_than(&entry, ORPHAN_GRACE) {
                continue;
            }
            match std::fs::remove_file(entry.path()) {
                Ok(()) => removed = removed.saturating_add(1),
                Err(error) => tracing::debug!(%error, "abandoned text index segment file kept for a later instance"),
            }
        }
        if removed > 0 {
            tracing::info!(removed, "removed text index files left by abandoned merges");
        }
    }

    /// The searcher over the index as it now stands on disk, including every other instance's commits.
    pub(super) fn current_searcher(&self) -> Result<tantivy::Searcher, SearchError> {
        self.reader.reload()?;
        Ok(self.reader.searcher())
    }
}

/// Whether `entry` was last modified more than `age` ago; `false` when that cannot be told, so the file is kept.
fn is_older_than(entry: &std::fs::DirEntry, age: Duration) -> bool {
    entry
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|elapsed| elapsed > age)
}

/// The segment id a tantivy segment file is named for (`<32 hexadecimal characters>.<extension>`), or `None` for any
/// other file.
fn segment_id_of(file_name: &str) -> Option<&str> {
    let (id, extension) = file_name.split_once('.')?;
    let is_segment_id = id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    (is_segment_id && !extension.is_empty()).then_some(id)
}

fn build_schema() -> (Schema, TextFields) {
    let mut builder = Schema::builder();
    let path = builder.add_text_field("path", STRING | STORED);
    let path_facet = builder.add_facet_field("path_facet", FacetOptions::default());
    let title = builder.add_text_field("title", TEXT | STORED);
    let headings = builder.add_text_field("headings", TEXT);
    let body = builder.add_text_field("body", TEXT | STORED);
    let tags = builder.add_facet_field("tags", FacetOptions::default());
    let frontmatter = builder.add_json_field(
        "frontmatter",
        JsonObjectOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("raw")
                .set_index_option(IndexRecordOption::Basic),
        ),
    );
    let modified = builder.add_date_field("modified", STORED | FAST);
    let content_hash = builder.add_text_field("content_hash", STRING | STORED);
    let schema = builder.build();
    (
        schema,
        TextFields {
            path,
            path_facet,
            title,
            headings,
            body,
            tags,
            frontmatter,
            modified,
            content_hash,
        },
    )
}

fn stored_document(fields: TextFields, document: &IndexedDocument) -> TantivyDocument {
    let mut stored = TantivyDocument::new();
    stored.add_text(fields.path, document.path());
    stored.add_facet(fields.path_facet, segment_facet(document.path()));
    stored.add_text(fields.title, document.title());
    for heading in document.headings() {
        stored.add_text(fields.headings, heading);
    }
    stored.add_text(fields.body, document.body());
    for tag in document.tags() {
        stored.add_facet(fields.tags, segment_facet(tag));
    }
    if !document.frontmatter().is_empty() {
        let object: BTreeMap<String, OwnedValue> = document
            .frontmatter()
            .iter()
            .map(|(key, value)| (key.clone(), OwnedValue::from(value.clone())))
            .collect();
        stored.add_object(fields.frontmatter, object);
    }
    stored.add_date(fields.modified, DateTime::from_utc(document.modified()));
    let hash: &str = document.content_hash().into();
    stored.add_text(fields.content_hash, hash);
    stored
}

/// Builds a facet from forward-slash segments, escaping each component.
fn segment_facet(path: &str) -> Facet {
    Facet::from_path(path.split('/'))
}

fn facet_clause(field: Field, path: &str, occur: Occur) -> (Occur, Box<dyn Query>) {
    let term = Term::from_facet(field, &segment_facet(path));
    (occur, Box::new(TermQuery::new(term, IndexRecordOption::Basic)))
}

fn json_term(field: Field, path: &str, value: &Value) -> Result<Term, SearchError> {
    let mut term = Term::from_field_json_path(field, path, false);
    match value {
        Value::String(text) => term.append_type_and_str(text),
        Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                term.append_type_and_fast_value(int);
            } else if let Some(float) = number.as_f64() {
                term.append_type_and_fast_value(float);
            } else {
                return Err(SearchError::InvalidFieldFilter { field: path.to_owned() });
            }
        }
        Value::Bool(flag) => term.append_type_and_fast_value(*flag),
        Value::Null | Value::Array(_) | Value::Object(_) => {
            return Err(SearchError::InvalidFieldFilter { field: path.to_owned() });
        }
    }
    Ok(term)
}

fn stored_text(document: &TantivyDocument, field: Field) -> String {
    document
        .get_first(field)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned()
}

fn stored_date(document: &TantivyDocument, field: Field) -> OffsetDateTime {
    document
        .get_first(field)
        .and_then(|value| value.as_datetime())
        .map_or(OffsetDateTime::UNIX_EPOCH, DateTime::into_utc)
}

fn render_snippet(generator: &SnippetGenerator, body: &str) -> String {
    let snippet = generator.snippet(body);
    if snippet.is_empty() {
        return fallback_snippet(body);
    }
    let html = snippet.to_html();
    match heading_before(body, snippet.fragment()) {
        Some(heading) => format!("{heading} › {html}"),
        None => html,
    }
}

/// Returns a plain leading excerpt when the query matched outside the body.
fn fallback_snippet(body: &str) -> String {
    let trimmed = body.trim();
    let mut end = trimmed.len().min(160);
    while !trimmed.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    trimmed.get(..end).unwrap_or_default().to_owned()
}

/// Finds the closest heading above the snippet fragment for context.
fn heading_before(body: &str, fragment: &str) -> Option<String> {
    if fragment.is_empty() {
        return None;
    }
    let position = body.find(fragment)?;
    let mut heading = None;
    let mut offset = 0_usize;
    let mut in_fence = false;
    for line in body.split_inclusive('\n') {
        if offset > position {
            break;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        } else if !in_fence {
            let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
            if (1..=6).contains(&hashes)
                && let Some(text) = trimmed.get(hashes..)
                && (text.starts_with(' ') || text.starts_with('\t'))
            {
                let text = strip_closing_hashes(text.trim());
                if !text.is_empty() {
                    heading = Some(text.to_owned());
                }
            }
        }
        offset = offset.saturating_add(line.len());
    }
    heading
}
