//! Link graph service.
//!
//! `LinkGraph` builds an in-memory directed graph of vault notes from
//! `contextos-obsidian` link parses. Every indexed note is a graph node, and
//! every link is resolved with the one Obsidian-style resolver against every
//! file in the vault: a link to a file that is not a note reaches a file node,
//! and a target nothing (or several files) answers to becomes a phantom node
//! that remembers which, mirroring Obsidian's own treatment of a link to a
//! note that does not yet exist. The graph is persisted incrementally
//! behind the [`StoresGraph`] trait boundary alongside the vault's other
//! derived state, so it need not be rebuilt from scratch on every server
//! start and so a single-note edit writes only the records that changed
//! rather than the whole graph. The concrete backend is configurable per
//! vault (`GraphBackend`): `sqlite` (default, WAL mode, tolerates more than
//! one server process against the same vault), `fjall` (an embedded KV
//! store, single-process-exclusive), or `serde` (a single JSON file, for a
//! small, starter, or experimental vault).
//!
//! `upsert_note` and `remove_note` apply per-file changes as the write
//! pipeline observes them; `rebuild` repopulates the whole graph from a
//! full vault scan.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contextos_obsidian::{BaseRecords, LinkForm, ObsidianLink, VaultFiles, is_placeholder_target};
use petgraph::Direction;
use petgraph::stable_graph::{EdgeIndex, NodeIndex, StableDiGraph};
use serde::{Deserialize, Serialize};

use crate::SearchError;
use crate::store::{DerivedStore, StoreFailure, StoreFailureKind, StoreHealth, StoreWriteLock};

mod fjall_store;
mod inspect;
mod mutate;
mod query;
mod resolve;
mod serde_store;
mod sqlite_store;
mod sync;

use fjall_store::FjallGraphStore;
pub use inspect::inspect_graph_store;
use serde_store::SerdeGraphStore;
use sqlite_store::SqliteGraphStore;

/// The link graph store format identifier. Bumping this forces every
/// existing store to be treated as incompatible, triggering a full
/// rebuild, the same role `CACHE_FORMAT` played for the former JSON cache.
/// Shared across every [`StoresGraph`] backend: it versions the domain
/// record shape, not any one storage engine's own on-disk layout.
const STORE_FORMAT: u32 = 3;

/// What a graph node stands for.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GraphNodeKind {
    /// A note the graph indexes: a Markdown file that is not excluded from search.
    Note,
    /// An existing file the graph does not index, such as an image, a PDF, a canvas, a base, or a note in an excluded
    /// folder. It exists only because a link reaches it, so a link to it resolves without making it a note: every
    /// note-oriented report (orphans, unlinked notes, a note's own links) leaves it out, and it goes when the last link
    /// to it does.
    File,
    /// A link target no file answers to: a stand-in for a file that does not (yet) exist.
    NotFound,
    /// A link target that more than one file answers to, none nearer the linking note than the others. The node keeps
    /// the candidates.
    Ambiguous,
}

/// One vault note, linked file, or unresolved link target in the link graph.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    /// Forward-slash relative vault path, or, for a [`GraphNodeKind::NotFound`] or [`GraphNodeKind::Ambiguous`] node,
    /// the link target as written.
    pub path: String,
    /// The note's derived title, or the same text as `path` for any other kind.
    pub title: String,
    /// What the node stands for.
    pub kind: GraphNodeKind,
    /// The files an [`GraphNodeKind::Ambiguous`] target answers to, sorted; empty for every other kind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    /// Why the note's links could not be read, when they could not: its outgoing links are then unknown, and it holds
    /// none. Only a [`GraphNodeKind::Note`] has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreadable: Option<String>,
    /// The base the note declares in its `base` property, as the link target it wrote: it settles a name several files
    /// answer to by the folder the base keeps its records in. Only a [`GraphNodeKind::Note`] has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The note's file as it was when its links were read, so a later pass can tell by a cheap comparison whether the
    /// note has been written since, however it was written. Only a [`GraphNodeKind::Note`] has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stamp: Option<NoteStamp>,
}

/// A note's file as it was when read: when it was last modified and how long it was. If either differs later, the note
/// has been written since.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NoteStamp {
    /// Whole seconds since the Unix epoch.
    pub modified_seconds: i64,
    /// The nanoseconds past `modified_seconds`.
    pub modified_nanoseconds: u32,
    /// The file's length in bytes.
    pub length: u64,
}

impl GraphNode {
    /// A node of `kind` at `path`, titled by its path.
    fn stand_in(path: &str, kind: GraphNodeKind) -> Self {
        Self {
            path: path.to_owned(),
            title: path.to_owned(),
            kind,
            candidates: Vec::new(),
            unreadable: None,
            base: None,
            stamp: None,
        }
    }

    /// Whether the node is a stand-in for a link target that did not resolve to one file.
    #[must_use]
    pub const fn phantom(&self) -> bool {
        matches!(self.kind, GraphNodeKind::NotFound | GraphNodeKind::Ambiguous)
    }
}

/// One directed link between two graph nodes, with the link as it was written.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphEdge {
    /// Forward-slash relative path of the linking note.
    pub from: String,
    /// Forward-slash relative path of the file the link reaches, or the link target as written when it resolves to
    /// nothing or to several files.
    pub to: String,
    /// How the link was written.
    pub form: LinkForm,
    /// The link's destination as it is resolved: without heading or block reference, with URL encoding decoded.
    pub target: String,
    /// The 1-based line the link is written on.
    pub line: usize,
    /// The link exactly as written.
    pub raw: String,
}

impl GraphEdge {
    /// Whether the link's target is a template placeholder, which no report of unresolved links should count.
    #[must_use]
    pub fn is_placeholder(&self) -> bool {
        is_placeholder_target(self.form, &self.target)
    }

    /// The deterministic order every view shares.
    fn order_key(&self) -> (&str, &str, &str, usize, &str) {
        (&self.from, &self.to, self.form.label(), self.line, &self.raw)
    }
}

/// One note as a rebuild takes it: its forward-slash relative path, its title, and its outgoing links.
pub type NoteLinks = (String, String, Vec<ObsidianLink>);

/// A note whose links could not be read, as a rebuild takes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnreadableNote {
    /// Forward-slash relative path of the note.
    pub path: String,
    /// The note's derived title.
    pub title: String,
    /// Why its links could not be read: names a line and a fault, never the note's content.
    pub reason: String,
}

/// One note, or one other file, as a batch of changes takes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileEntry {
    /// A note written, with what was read of it.
    Note(NoteEntry),
    /// A file that is not an indexed note (an image, a PDF, a canvas, a base, a note in an excluded folder), now present.
    File(String),
    /// A file, or with `folder` every file inside the folder of that name, that no longer exists.
    Removed { path: String, folder: bool },
}

/// A note as a batch of changes takes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteEntry {
    /// Forward-slash relative path of the note.
    pub path: String,
    /// The note's derived title.
    pub title: String,
    /// Its outgoing links, or why they could not be read.
    pub links: Result<Vec<ObsidianLink>, String>,
    /// The base the note declares, as the link target it wrote.
    pub base: Option<String>,
    /// The note's file as it was when read.
    pub stamp: Option<NoteStamp>,
}

/// What a graph holds that a pass comparing it with the vault needs: the files it believes the vault holds, and what it
/// read of each note.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GraphKnowledge {
    /// Every file the graph believes the vault holds.
    pub files: BTreeSet<String>,
    /// Every note the graph holds as a node, with the stamp its file had when read, if it was recorded.
    pub notes: BTreeMap<String, Option<NoteStamp>>,
    /// Every note or linked file the graph holds as a node.
    pub nodes: BTreeSet<String>,
}

/// Lists the vault's files, as forward-slash relative paths, leaving out any the vault hides.
pub type FileLister = Arc<dyn Fn() -> Result<Vec<String>, SearchError> + Send + Sync>;

/// Reads the folder a base file keeps its records in.
pub trait ReadsBaseFolders: Send + Sync {
    /// The records folder of the base file at `base`, or `None` when the file cannot be read or confines its records to
    /// no single folder, in which case it settles no name.
    fn records_folder(&self, base: &str) -> Option<BaseRecords>;
}

/// Everything a rebuild learns from walking a vault.
#[derive(Clone, Debug, Default)]
pub struct VaultScan {
    /// The notes whose links were read.
    pub notes: Vec<NoteLinks>,
    /// The notes whose links could not be read.
    pub unreadable: Vec<UnreadableNote>,
    /// Every file in the vault, notes included, as forward-slash relative paths: what links resolve against.
    pub files: Vec<String>,
    /// The base each note declares, as the link target it wrote, by the note's path.
    pub bases: BTreeMap<String, String>,
    /// Each note's file as it was when read, by the note's path.
    pub stamps: BTreeMap<String, NoteStamp>,
}

/// Where [`LinkGraph::rebuild_from`] takes its scan from: given the notes written or removed through the store since
/// the caller's scan, or `None` when that cannot be told, it returns everything the rebuilt graph holds.
pub type NoteSource<'a> = dyn FnMut(Option<&std::collections::BTreeSet<String>>) -> Result<VaultScan, SearchError> + 'a;

/// An artefact-renderable slice of the link graph.
///
/// Every returned view lists exactly the nodes touched by its edges plus
/// the query's focus node(s). Nodes are sorted by `path`; edges are sorted
/// by `(from, to, kind)`, so repeated queries against an unchanged graph
/// render identically.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphView {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// The traversal direction for a link graph query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphDirection {
    /// Follow outgoing links and embeds only.
    Out,
    /// Follow incoming links and embeds only.
    In,
    /// Follow both directions.
    Both,
}

impl GraphDirection {
    /// Returns the `petgraph` directions to traverse for this query
    /// direction.
    const fn directions(self) -> &'static [Direction] {
        match self {
            Self::Out => &[Direction::Outgoing],
            Self::In => &[Direction::Incoming],
            Self::Both => &[Direction::Outgoing, Direction::Incoming],
        }
    }
}

/// Trusted location of the link graph's persisted store: a directory each
/// [`GraphBackend`] owns exclusively, laying out its own file(s) inside it
/// (`fjall`'s own embedded-database directory, `sqlite`'s `graph.sqlite3`,
/// `serde`'s `graph.json`), never a single fixed file shared across
/// backends.
#[derive(Clone, Debug)]
pub struct LinkGraphConfig {
    pub store_directory: PathBuf,
    pub backend: GraphBackend,
    /// How long a write to a `sqlite` store waits for another instance's write before failing as `index/busy`;
    /// [`crate::DEFAULT_WRITE_WAIT`] in every server and CLI instance. The single-instance backends ignore it.
    pub write_wait: std::time::Duration,
}

/// The configurable persistence backend for one vault's link graph
/// (`[vault.search] graph_backend`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GraphBackend {
    /// A single JSON file, rewritten whole on every persisted change.
    /// Simplest to inspect by hand; intended for a small, starter, or
    /// experimental vault, not concurrent multi-process use, since a plain
    /// file has no locking of its own.
    Serde,
    /// An embedded `fjall` key-value store, single-process-exclusive.
    /// Fastest when only one server process ever opens the vault; a
    /// second process opening the same vault's store instead observes
    /// [`SearchError::GraphLocked`].
    Fjall,
    /// A `rusqlite`-backed `SQLite` database in WAL journal mode with an
    /// explicit busy timeout, so more than one `contextos` process can
    /// hold the same vault's graph store open concurrently. The default:
    /// a vault is routinely opened by more than one server process from
    /// the same operator at once (a host's general connector plus a
    /// per-task instance, or the CLI beside a running server), and under
    /// `fjall` every process after the first loses its link graph. The
    /// only backend that also tracks a `generation` counter for
    /// cross-instance propagation.
    #[default]
    Sqlite,
}

/// Whether a [`LinkGraph`]'s most recent cross-instance catch-up applied a
/// partial delta or fell back to a full reload.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CatchUpKind {
    /// Only the records persisted since this instance's last-seen
    /// generation were retrieved and applied: the common-case fast path,
    /// cost proportional to what actually changed.
    Partial,
    /// This instance's last-seen generation had aged out of the store's
    /// retained tombstone history, so the whole in-memory graph was
    /// reloaded from scratch instead: the correctness-preserving fallback
    /// for a reader that has been idle a long time.
    FullReload,
}

/// Cross-instance propagation status for one [`LinkGraph`]. `None` from
/// [`LinkGraph::sync_status`] when the configured
/// backend does not track propagation (`fjall`, `serde`): both are
/// unaffected by design, see [`GraphBackend::Sqlite`]'s own documentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SyncStatus {
    /// This instance's last-seen generation: caught up to at least this
    /// point, possibly further if nothing has changed since.
    pub generation: u64,
    /// The kind of the most recent catch-up this instance actually
    /// performed, or `None` if this instance has not yet needed one (no
    /// sibling write observed since it opened).
    pub last_catch_up: Option<CatchUpKind>,
}

/// The edge weight actually held in the in-memory graph: the link as it was
/// written plus a persisted, process-restart-stable identity used as this
/// edge's key in the `edges` keyspace.
///
/// `petgraph`'s own `EdgeIndex` cannot serve this role: it is an
/// implementation-internal handle whose value depends on insertion and
/// removal order within one `StableDiGraph` instance, not a durable
/// identity. `store_id` is instead assigned from a monotonic counter
/// persisted in the `metadata` keyspace, so it survives a reopen and,
/// critically, stays distinct across parallel edges: a note that links to
/// the same target twice legitimately produces two edges (`StableDiGraph`
/// permits parallel edges, and `wire_links` adds one per parsed link), and
/// a key of only `(from, to)` would collapse both writes into one
/// stored record, silently losing the second edge on reopen.
#[derive(Clone, Debug, Eq, PartialEq)]
struct EdgeRecord {
    store_id: u64,
    link: EdgeLink,
}

/// A link as written: what an edge keeps so it can be resolved again when the files around it change, and reported.
#[derive(Clone, Debug, Eq, PartialEq)]
struct EdgeLink {
    form: LinkForm,
    target: String,
    line: usize,
    raw: String,
}

impl From<&ObsidianLink> for EdgeLink {
    fn from(link: &ObsidianLink) -> Self {
        Self {
            form: link.form,
            target: link.target.clone(),
            line: link.line,
            raw: link.raw.clone(),
        }
    }
}

/// What a graph store's metadata says about its content, from [`StoresGraph::read_metadata`].
enum StoredGraphState {
    /// The store has never been written.
    Empty,
    /// The store was written in a format other than [`STORE_FORMAT`].
    Incompatible,
    /// The store holds a graph in [`STORE_FORMAT`], described by this metadata.
    Ready(StoredGraphMetadata),
}

/// Whether a write records the store as holding the vault's whole graph, as [`StoresGraph::persist`] takes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StoreCompleteness {
    /// A rebuild: the store now holds the whole graph, so any earlier incomplete mark is cleared.
    Complete,
    /// A note-by-note write to a store already holding the whole graph: its mark is left as it is.
    Unchanged,
    /// A note-by-note write to a store never rebuilt: it holds only notes written since, so it is marked incomplete,
    /// and opens as needing a rebuild rather than as the vault's graph.
    Incomplete,
}

/// Metadata read back from a graph store in [`STORE_FORMAT`]: the next-edge-id allocator and the generation counter.
struct StoredGraphMetadata {
    /// Whether the store holds the vault's whole graph: `false` when it has only ever been written note by note,
    /// since its creation or since it was found in another format. A store written before this mark existed has none,
    /// and reads as complete, as it always did.
    complete: bool,
    next_edge_id: u64,
    /// The store's generation counter as of this read, `0`
    /// for `fjall`/`serde` (meaningless there, never consulted since
    /// [`LinkGraph`] only checks propagation state for `sqlite`) and for a
    /// `sqlite` store predating this field's introduction (treated
    /// leniently as `0`, the oldest possible generation, rather than as a
    /// format mismatch: an in-place additive migration, not a forced
    /// rebuild, per this store's own migration comment).
    generation: u64,
}

/// One node or edge change to apply to a graph store in one atomic write,
/// carrying the fully resolved current value rather than a reference into
/// [`LinkGraph`]'s own in-memory graph: every [`StoresGraph`] implementation
/// owns only durable storage of the records it is handed, never a second
/// copy of graph structure, traversal, or edge-id-allocation logic, all of
/// which stay in [`LinkGraph`] itself.
#[derive(Debug)]
enum GraphChange {
    UpsertNode(GraphNode),
    RemoveNode(String),
    UpsertEdge {
        store_id: u64,
        edge: GraphEdge,
    },
    RemoveEdge {
        store_id: u64,
    },
    /// A file now exists in the vault. Carried to other instances sharing the store, which keep their own listing of
    /// the vault's files, and recorded in the change history only: the store holds no listing of its own.
    FileAdded(String),
    /// A file, or with `folder` every file inside the folder of that name, no longer exists in the vault. Recorded as
    /// [`GraphChange::FileAdded`] is.
    FileRemoved {
        path: String,
        folder: bool,
    },
    /// Discards every stored node and edge before the changes that follow
    /// it in the same [`StoresGraph::persist`] call are applied. It is only
    /// ever the first change in a call ([`LinkGraph::persist_changes`]
    /// emits it that way), and backends may rely on that: `fjall` keeps any
    /// record the call upserts regardless of position, where an
    /// order-applying backend would also discard an upsert placed before a
    /// later `Reset`.
    /// [`LinkGraph::rebuild`] replaces the whole graph, which the ordinary
    /// upsert/remove vocabulary cannot express without naming every record
    /// it drops. The discard and the repopulation are one atomic write, so
    /// a reader sees either the previous graph or the rebuilt one, and a
    /// failed write leaves the previous graph in place. A sibling
    /// instance's [`StoresGraph::changes_since`] call must treat a `Reset`
    /// anywhere in its requested range as forcing a full reload, never a
    /// partial merge of old-plus-new content: applying only the fresh
    /// upserts on top of stale in-memory state would silently keep records
    /// the rebuild actually dropped.
    Reset,
}

/// Proof that the caller holds a graph store's write lock, for as long as it lives. A mutation takes it before catching
/// up with what other instances have written and holds it through its own persist, so no other instance can write in
/// between: without it, an edge id this instance allocates, or one it removes, could name a different edge another
/// instance wrote meanwhile. A single-instance backend has no lock to take.
pub(super) struct GraphWriteGuard {
    _lock: Option<StoreWriteLock>,
}

impl GraphWriteGuard {
    /// A guard holding `lock`.
    const fn locked(lock: StoreWriteLock) -> Self {
        Self { _lock: Some(lock) }
    }

    /// A guard for a store that needs no lock.
    const fn unlocked() -> Self {
        Self { _lock: None }
    }
}

/// Every node, and every `(store_id, edge)` pair, a [`StoresGraph::load_all`]
/// call read back from its backend.
type GraphRecords = (Vec<GraphNode>, Vec<(u64, GraphEdge)>);

/// A reconstructed in-memory graph, its path index, and its `store_id`
/// index, as [`build_graph`] returns and [`LoadedSnapshot`] carries.
type BuiltGraph = (
    StableDiGraph<GraphNode, EdgeRecord>,
    HashMap<String, NodeIndex>,
    HashMap<u64, EdgeIndex>,
);

/// Everything a [`StoresGraph::changes_since`] call returns: every change
/// persisted strictly after the requested generation, in the same relative
/// order [`LinkGraph::persist_changes`] originally wrote them (node
/// removals, then node upserts, then edge removals, then edge upserts,
/// oldest generation first), so applying them in order never references an
/// edge's endpoint before that endpoint itself has been applied. `generation`
/// is the generation this delta brings the caller up to date with, recorded
/// as the new last-seen value once applied.
struct GraphDelta {
    changes: Vec<GraphChange>,
    generation: u64,
}

/// Backend-agnostic persistence port for one vault's link graph.
/// [`LinkGraph`] holds the in-memory `petgraph` structure and
/// is the sole owner of graph *logic*; every implementation of this trait
/// owns only durable storage of the `GraphNode`/`GraphEdge` records it is
/// handed. Mirrors `vector_store.rs`'s `StoresVectors` shape: `Send + Sync`,
/// `&self` methods over an interior-mutable connection, so a concrete store
/// (for example a `Mutex`-guarded `rusqlite::Connection`) can be shared
/// through a `Box<dyn StoresGraph>` without `LinkGraph` itself needing
/// `&mut` access to it.
///
/// Every read reports why it failed as a [`StoreFailure`], so a caller can tell a store that was never written,
/// or written in another format, from one that is busy, damaged, holds an undecodable record, or cannot be read.
trait StoresGraph: Send + Sync {
    /// Reads whether this store has been written, in which format, and, in [`STORE_FORMAT`], its next-edge-id
    /// allocator and generation.
    ///
    /// # Errors
    ///
    /// Returns the failure that kept the metadata from being read, including a value that cannot be decoded.
    fn read_metadata(&self) -> Result<StoredGraphState, StoreFailure>;

    /// Reads every currently stored node and `(store_id, edge)` pair.
    ///
    /// # Errors
    ///
    /// Returns the failure for the first record that cannot be read or decoded: one bad record fails the whole load
    /// rather than admitting a partially reconstructed graph.
    fn load_all(&self) -> Result<GraphRecords, StoreFailure>;

    /// Takes this store's write lock, which [`StoresGraph::persist`] requires.
    ///
    /// # Errors
    ///
    /// Returns a busy failure when another instance holds the lock past the store's write wait, and an I/O failure
    /// when the lock cannot be taken.
    fn write_guard(&self) -> Result<GraphWriteGuard, SearchError>;

    /// Applies `changes` and unconditionally (re)writes this store's
    /// format identifier and `next_edge_id` allocator, in one atomic
    /// write, so a call that establishes an empty-but-valid graph (an
    /// empty vault's [`LinkGraph::rebuild`]) still records a store that
    /// reopens as `needs_rebuild() == false`.
    ///
    /// A [`GraphChange::Reset`] discards every previously stored node and
    /// edge as part of that same write, so no reader, and no failure part
    /// way through, ever observes the emptied intermediate state: on error
    /// the store holds exactly what it held before the call.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the write cannot be committed.
    fn persist(
        &self,
        guard: &GraphWriteGuard,
        changes: &[GraphChange],
        next_edge_id: u64,
        completeness: StoreCompleteness,
    ) -> Result<(), SearchError>;

    /// Returns this store's current generation counter, or `None` when the backend does not track propagation
    /// (`fjall`, `serde`, always `None`) or this store has never been written.
    ///
    /// # Errors
    ///
    /// Returns the failure that kept the counter from being read.
    fn current_generation(&self) -> Result<Option<u64>, StoreFailure>;

    /// Returns every node/edge upsert or removal persisted strictly after
    /// `since`, in application order, alongside the generation this delta
    /// brings the caller up to date with.
    ///
    /// `None` means either: the backend does not track propagation
    /// (`fjall`, `serde`), or `since` is older than this store's oldest
    /// retained history, so the caller must fall back to
    /// [`StoresGraph::load_all`] (a full reload) instead of a partial
    /// catch-up. Both cases collapse to `None` deliberately: [`LinkGraph`]
    /// already knows its own configured backend and only ever calls this
    /// for `sqlite`, so no caller needs to distinguish "unsupported" from
    /// "too stale" from the return value alone.
    ///
    /// # Errors
    ///
    /// Returns the failure for the first changelog record that cannot be read or decoded.
    fn changes_since(&self, since: u64) -> Result<Option<GraphDelta>, StoreFailure>;
}

/// Node and edge identities touched by one call to [`LinkGraph::upsert_note`],
/// [`LinkGraph::remove_note`], or [`LinkGraph::rebuild`], collected as the
/// in-memory graph mutates and applied to the `fjall` store in one batch at
/// the end of the call. This is what makes a single-note edit write only
/// its changed records rather than the whole graph.
#[derive(Default)]
struct PendingChanges {
    upserted_nodes: Vec<NodeIndex>,
    removed_node_keys: Vec<String>,
    upserted_edges: Vec<EdgeIndex>,
    removed_edge_ids: Vec<u64>,
    /// Nodes that lost an incoming edge, which may now be unreferenced stand-ins to prune.
    unreferenced: Vec<NodeIndex>,
    /// Files that came to exist, which other instances sharing the store must learn of even when no link changed.
    files_added: Vec<String>,
    /// Files, or folders with `true`, that ceased to exist.
    files_removed: Vec<(String, bool)>,
}

/// In-memory wikilink graph over a vault, persisted incrementally behind
/// the [`StoresGraph`] trait boundary alongside the vault's other derived
/// state.
pub struct LinkGraph {
    graph: StableDiGraph<GraphNode, EdgeRecord>,
    index: HashMap<String, NodeIndex>,
    /// `store_id` to edge index, maintained alongside every local or
    /// externally-applied edge add/remove: the sole reason
    /// this exists is applying an externally-sourced
    /// [`GraphChange::RemoveEdge`] in O(1) rather than a linear scan over
    /// every edge in the graph, which would make catch-up cost scale with
    /// total graph size again, defeating the whole point of a partial
    /// delta.
    edge_by_store_id: HashMap<u64, EdgeIndex>,
    /// The `store_id` of every edge, by the key of the file name its target names (see `resolve::target_key`). A file
    /// that appears or goes can change only the links whose target names its file name, so this finds them without
    /// looking at every edge.
    edges_by_name: HashMap<String, BTreeSet<u64>>,
    /// Every file in the vault, which links resolve against. Not persisted: a vault is listed once per process (or by a
    /// rebuild) and kept in step with the files the graph is told come and go.
    files: VaultFiles,
    /// Whether `files` is a listing of the vault, rather than only the notes written since this instance opened.
    files_listed: bool,
    /// Lists the vault again when the graph needs it listed, which is when it opens and after a full reload from the
    /// store, which holds no listing. `None` for a graph that is told its files instead.
    lister: Option<FileLister>,
    /// Reads the folder of a base file a note declares.
    bases: Option<Arc<dyn ReadsBaseFolders>>,
    /// The records folder of each base file asked about, `None` for one that confines no folder. Dropped for a base
    /// file whenever that file comes, goes, or changes.
    base_records: HashMap<String, Option<BaseRecords>>,
    store: Box<dyn StoresGraph>,
    /// The directory the store lives in, named in every failure this instance reports.
    store_directory: PathBuf,
    /// The backend this instance was opened with. `LinkGraph` keeps this
    /// rather than inferring it from `store`'s trait-object type, so
    /// cross-instance catch-up can cheaply decide whether to
    /// attempt it at all without downcasting `Box<dyn StoresGraph>`.
    backend: GraphBackend,
    next_edge_id: u64,
    /// What this instance found when it last loaded the store; anything but [`StoreHealth::Ready`] means the graph
    /// should be rebuilt from the vault.
    health: StoreHealth,
    /// The generation this instance has applied changes up to. Meaningless
    /// when `backend` is not [`GraphBackend::Sqlite`],
    /// left at `0` and never consulted in that case.
    last_seen_generation: u64,
    /// The kind of this instance's most recent cross-instance catch-up, or
    /// `None` if it has not yet needed one. Meaningless when `backend` is
    /// not [`GraphBackend::Sqlite`].
    last_catch_up: Option<CatchUpKind>,
}

/// A freshly loaded snapshot from a graph store: everything
/// [`LinkGraph::try_from`] needs on first open, and everything a
/// cross-instance catch-up's full-reload fallback needs to
/// replace an existing instance's in-memory state with. Factored out so
/// both call sites share one loading path rather than two.
struct LoadedSnapshot {
    graph: StableDiGraph<GraphNode, EdgeRecord>,
    index: HashMap<String, NodeIndex>,
    edge_by_store_id: HashMap<u64, EdgeIndex>,
    next_edge_id: u64,
    generation: u64,
    complete: bool,
}

/// What a full load from a store found.
enum StoreSnapshot {
    /// A complete graph in [`STORE_FORMAT`].
    Ready(LoadedSnapshot),
    /// The store has never been written.
    Empty,
    /// The store was written in another format.
    Incompatible,
}

/// Attempts a full load from `store`: its metadata plus every node/edge record, reconstructed into an in-memory
/// graph.
///
/// # Errors
///
/// Returns the failure that kept the metadata or any record from being read, or an undecodable failure for an edge
/// naming a node the store does not hold.
fn load_snapshot(store: &dyn StoresGraph, directory: &Path) -> Result<StoreSnapshot, StoreFailure> {
    let metadata = match store.read_metadata()? {
        StoredGraphState::Empty => return Ok(StoreSnapshot::Empty),
        StoredGraphState::Incompatible => return Ok(StoreSnapshot::Incompatible),
        StoredGraphState::Ready(metadata) => metadata,
    };
    let (graph, index, edge_by_store_id) = build_graph(store.load_all()?).ok_or_else(|| {
        StoreFailure::undecodable(
            DerivedStore::Graph,
            directory.display().to_string(),
            "an edge names a node the store does not hold",
        )
    })?;
    Ok(StoreSnapshot::Ready(LoadedSnapshot {
        graph,
        index,
        edge_by_store_id,
        next_edge_id: metadata.next_edge_id,
        generation: metadata.generation,
        complete: metadata.complete,
    }))
}

impl TryFrom<LinkGraphConfig> for LinkGraph {
    type Error = SearchError;

    /// Opens the graph's configured store.
    ///
    /// A store never written ([`StoreHealth::Empty`]) or written in another format ([`StoreHealth::Incompatible`])
    /// yields an empty graph to be rebuilt. A store whose content cannot be read yields an empty graph flagged
    /// [`StoreHealth::Unreadable`] with the failure, which every later call reports until a load succeeds: the rest of
    /// the vault's search keeps working, and the failure is visible rather than looking like a vault with no links. A
    /// store that cannot even be opened does fail construction; see `fjall_store`'s documentation for why.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the underlying store cannot be opened,
    /// and [`SearchError::GraphLocked`] when the `fjall` backend's store is
    /// already held open by another process.
    fn try_from(value: LinkGraphConfig) -> Result<Self, Self::Error> {
        let store: Box<dyn StoresGraph> = match value.backend {
            GraphBackend::Fjall => Box::new(FjallGraphStore::open(&value.store_directory)?),
            GraphBackend::Sqlite => Box::new(SqliteGraphStore::open(&value.store_directory, value.write_wait)?),
            GraphBackend::Serde => Box::new(SerdeGraphStore::open(&value.store_directory)?),
        };

        let mut graph = Self {
            graph: StableDiGraph::new(),
            index: HashMap::new(),
            edge_by_store_id: HashMap::new(),
            edges_by_name: HashMap::new(),
            files: VaultFiles::default(),
            files_listed: false,
            lister: None,
            bases: None,
            base_records: HashMap::new(),
            store,
            backend: value.backend,
            store_directory: value.store_directory,
            next_edge_id: 0,
            health: StoreHealth::Empty,
            last_seen_generation: 0,
            last_catch_up: None,
        };
        match load_snapshot(graph.store.as_ref(), &graph.store_directory) {
            Ok(snapshot) => graph.adopt(snapshot),
            Err(failure) => {
                tracing::warn!(
                    code = failure.code(),
                    path = %failure.path(),
                    %failure,
                    "link graph store could not be read; the link graph reports it until the store is rebuilt"
                );
                graph.health = StoreHealth::Unreadable(failure);
            }
        }
        Ok(graph)
    }
}

/// Reconstructs a [`StableDiGraph`], its path index, and its `store_id`
/// index from the raw records a [`StoresGraph::load_all`] call returned, or
/// `None` when an edge references a node path absent from `nodes`: the same
/// all-or-nothing load contract [`StoresGraph::load_all`] itself documents,
/// just completed one level up once both node and edge records are in hand.
/// Shared by every backend so this reconstruction is written once, not
/// duplicated per implementation.
fn build_graph(records: GraphRecords) -> Option<BuiltGraph> {
    let (nodes, edges) = records;
    let mut graph = StableDiGraph::new();
    let mut index = HashMap::new();
    for node in nodes {
        let path = node.path.clone();
        let idx = graph.add_node(node);
        index.insert(path, idx);
    }
    let mut edge_by_store_id = HashMap::new();
    for (store_id, edge) in edges {
        let from = *index.get(&edge.from)?;
        let to = *index.get(&edge.to)?;
        let edge_idx = graph.add_edge(
            from,
            to,
            EdgeRecord {
                store_id,
                link: EdgeLink {
                    form: edge.form,
                    target: edge.target,
                    line: edge.line,
                    raw: edge.raw,
                },
            },
        );
        edge_by_store_id.insert(store_id, edge_idx);
    }
    Some((graph, index, edge_by_store_id))
}

/// A stored record's JSON decode failure, described by its kind and position only: `serde_json`'s own message can
/// quote the stored value (a note's path or title), which must not reach a log or a tool result.
struct JsonDecodeFailure<'a>(&'a serde_json::Error);

impl std::fmt::Display for JsonDecodeFailure<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.0.classify() {
            serde_json::error::Category::Io => "could not be read",
            serde_json::error::Category::Syntax => "is not valid JSON",
            serde_json::error::Category::Data => "is JSON of the wrong shape",
            serde_json::error::Category::Eof => "is truncated JSON",
        };
        write!(formatter, "{kind} (line {}, column {})", self.0.line(), self.0.column())
    }
}

/// Encodes `value` as JSON for storage, mapping a failure to
/// [`SearchError::GraphStorage`] under `directory`'s label.
fn encode<T: Serialize>(value: &T, directory: &Path) -> Result<Vec<u8>, SearchError> {
    serde_json::to_vec(value).map_err(|source| graph_storage_error(directory, source))
}

/// Builds [`SearchError::GraphStorage`] for a failure against the store at
/// `directory`, preserving this crate's convention (already used for the
/// link graph's own former JSON encoding failures, and for other stores'
/// non-`std::io::Error` failure types) of carrying a non-I/O source through
/// `std::io::Error::other` rather than adding a second source type to the
/// error enum for what is, from an operator's perspective, the same
/// `index/storage` failure.
fn graph_storage_error<E>(directory: &Path, source: E) -> SearchError
where
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    SearchError::GraphStorage {
        path: directory.display().to_string(),
        source: std::io::Error::other(source),
    }
}
