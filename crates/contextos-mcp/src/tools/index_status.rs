//! Per-index status reporting, shared by `vault_info`'s
//! `indexes` block (`tools::vault`) and `query_index_status`'s full
//! result (`tools::query`): one conversion from `contextos_search`'s
//! status types, reused by both tools rather than duplicated, so the two
//! surfaces always agree about the same store, and agree with the doctor's
//! store checks, which report the same failure codes and remediations.

use contextos_search::{
    CatchUpKind, GraphIndexStatus, IndexStatusReport, ScanFailure, SearchError, SemanticIndexStatus, StoreFailure,
    StoreHealth, StoreRecovery, TextIndexStatus,
};
use rmcp::schemars;
use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize, schemars::JsonSchema)]
pub(crate) struct IndexesStatusToolResult {
    /// The vault's derived-state directory (omitted when search is disabled
    /// entirely for this vault), so an operator can confirm a separately
    /// invoked `contextos index` and this running server resolve to the
    /// same on-disk store rather than two silently diverged ones.
    // `#[serde(default)]` is required alongside `schema_with` here: the
    // schema-generation macro derives whether a field counts as "optional"
    // (and thus absent from the schema's `required` array) from the same
    // overridden schema type `schema_with` supplies, and a bare function
    // override's wrapper type is not itself recognised as an `Option`.
    // Without this, state_directory would be incorrectly marked required
    // despite genuinely being omitted whenever search is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    state_directory: Option<String>,
    /// Why the vault or its state directory could not be scanned for staleness or build times; present only then.
    /// Every store below still reports its own health.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure: Option<StoreFailureToolResult>,
    text: TextIndexStatusToolResult,
    graph: GraphIndexStatusToolResult,
    semantic: SemanticIndexStatusToolResult,
}

impl From<IndexStatusReport> for IndexesStatusToolResult {
    fn from(value: IndexStatusReport) -> Self {
        Self {
            state_directory: Some(value.state_directory.display().to_string()),
            failure: value.scan_failure.as_ref().map(StoreFailureToolResult::from),
            text: TextIndexStatusToolResult::from(value.text),
            graph: GraphIndexStatusToolResult::from(value.graph),
            semantic: SemanticIndexStatusToolResult::from(value.semantic),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, schemars::JsonSchema)]
struct TextIndexStatusToolResult {
    enabled: bool,
    /// Whether the text index can be read: `ready`; `unreadable` (`failure` says why, and no documents are counted);
    /// or `disabled`.
    health: StoreHealthToolResult,
    /// Why the index could not be read; present only when `health` is `unreadable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure: Option<StoreFailureToolResult>,
    documents: usize,
    stale_estimate: usize,
    /// Omitted (never `null`) whenever this index has not yet built, most
    /// commonly because it is disabled: `#[serde(default)]` alongside
    /// `schema_with` is required for the same reason documented on
    /// `IndexesStatusToolResult::state_directory` above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    last_build: Option<String>,
    /// When this server last found the index damaged and moved it aside to recreate it, and where the damaged files
    /// were kept; omitted when it has not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_recovery: Option<StoreRecoveryToolResult>,
}

impl From<TextIndexStatus> for TextIndexStatusToolResult {
    fn from(value: TextIndexStatus) -> Self {
        let (health, failure) = health_and_failure(value.health.as_ref());
        Self {
            enabled: value.enabled,
            health,
            failure,
            documents: value.documents,
            stale_estimate: value.stale_estimate,
            last_build: value.last_build.map(|when| when.to_string()),
            last_recovery: value.last_recovery.as_ref().map(StoreRecoveryToolResult::from),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, schemars::JsonSchema)]
struct GraphIndexStatusToolResult {
    enabled: bool,
    nodes: usize,
    edges: usize,
    needs_rebuild: bool,
    /// What the graph found in its store: `ready` (the vault's graph); `empty` (not rebuilt from the vault, so it holds
    /// at most the notes written since); `incompatible` (written in another build's format); `unreadable` (its
    /// content could not be read: `failure` says why, and no nodes or edges are reported); or `disabled`.
    health: StoreHealthToolResult,
    /// Why the store could not be read; present only when `health` is `unreadable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure: Option<StoreFailureToolResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    last_build: Option<String>,
    /// When this server last found the store damaged and moved it aside to rebuild it, and where the damaged files
    /// were kept; omitted when it has not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_recovery: Option<StoreRecoveryToolResult>,
    /// This vault's link graph's current generation counter, omitted when
    /// the configured `graph_backend` does not track one (`fjall`, `serde`)
    /// or search is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_u64_schema")]
    generation: Option<u64>,
    /// Whether this instance's most recent cross-instance catch-up
    /// applied a partial delta (`"partial"`) or fell back to
    /// a full reload (`"full-reload"`), omitted for the same reasons as
    /// `generation`, or when this instance has not yet needed a catch-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    last_catch_up: Option<String>,
}

/// What a derived store held when last read, as the status tools report it. Its variants carry no doc comments on
/// purpose: a described variant makes the schema a `oneOf`, which some hosts' tool registries reject, so the values are
/// described on the field that carries them.
#[derive(Clone, Copy, Debug, Default, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum StoreHealthToolResult {
    Ready,
    Empty,
    Incompatible,
    Unreadable,
    #[default]
    Disabled,
}

/// Why a derived store could not be read, as the status tools report it.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct StoreFailureToolResult {
    /// Stable machine-readable code: `index/busy`, `index/corrupt`, `index/undecodable`, or `index/storage`.
    code: String,
    message: String,
    /// What to do about it.
    remediation: String,
}

impl From<&SearchError> for StoreFailureToolResult {
    fn from(value: &SearchError) -> Self {
        Self {
            code: value.code().to_owned(),
            message: value.to_string(),
            remediation: value.remediation().to_owned(),
        }
    }
}

impl From<&ScanFailure> for StoreFailureToolResult {
    fn from(value: &ScanFailure) -> Self {
        Self {
            code: value.code.to_owned(),
            message: value.message.clone(),
            remediation: value.remediation.to_owned(),
        }
    }
}

impl From<&StoreFailure> for StoreFailureToolResult {
    fn from(value: &StoreFailure) -> Self {
        Self::from(&SearchError::from(value.clone()))
    }
}

/// A damaged store moved aside and recreated.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct StoreRecoveryToolResult {
    /// Which store: `text`, `graph`, or `vectors`.
    store: String,
    /// The directory the damaged store's files were moved into and kept, for diagnosis; deleting it is the
    /// operator's decision.
    moved_to: String,
    /// When it was moved aside.
    at: String,
}

impl From<&StoreRecovery> for StoreRecoveryToolResult {
    fn from(value: &StoreRecovery) -> Self {
        Self {
            store: value.store.name().to_owned(),
            moved_to: value.moved_to.display().to_string(),
            at: value.at.to_string(),
        }
    }
}

/// The reported health and, for an unreadable store, its failure; `disabled` for a store that is not enabled.
fn health_and_failure(health: Option<&StoreHealth>) -> (StoreHealthToolResult, Option<StoreFailureToolResult>) {
    match health {
        None => (StoreHealthToolResult::Disabled, None),
        Some(StoreHealth::Ready) => (StoreHealthToolResult::Ready, None),
        Some(StoreHealth::Empty) => (StoreHealthToolResult::Empty, None),
        Some(StoreHealth::Incompatible) => (StoreHealthToolResult::Incompatible, None),
        Some(StoreHealth::Unreadable(failure)) => (
            StoreHealthToolResult::Unreadable,
            Some(StoreFailureToolResult::from(failure)),
        ),
    }
}

impl From<GraphIndexStatus> for GraphIndexStatusToolResult {
    fn from(value: GraphIndexStatus) -> Self {
        let (health, failure) = health_and_failure(value.health.as_ref());
        Self {
            enabled: value.enabled,
            nodes: value.nodes,
            edges: value.edges,
            needs_rebuild: value.needs_rebuild,
            health,
            failure,
            last_build: value.last_build.map(|when| when.to_string()),
            last_recovery: value.last_recovery.as_ref().map(StoreRecoveryToolResult::from),
            generation: value.generation,
            last_catch_up: value.last_catch_up.map(|kind| {
                match kind {
                    CatchUpKind::Partial => "partial",
                    CatchUpKind::FullReload => "full-reload",
                }
                .to_owned()
            }),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, schemars::JsonSchema)]
struct SemanticIndexStatusToolResult {
    enabled: bool,
    /// Whether the vector store can be read: `ready`; `unreadable` (`failure` says why, and no vectors are counted);
    /// or `disabled`.
    health: StoreHealthToolResult,
    /// Why the store could not be read; present only when `health` is `unreadable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure: Option<StoreFailureToolResult>,
    documents: usize,
    chunks: usize,
    stale_estimate: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    last_build: Option<String>,
    /// When this server last found the store damaged and moved it aside, queueing the vault to be embedded again, and
    /// where the damaged files were kept; omitted when it has not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_recovery: Option<StoreRecoveryToolResult>,
}

impl From<SemanticIndexStatus> for SemanticIndexStatusToolResult {
    fn from(value: SemanticIndexStatus) -> Self {
        let (health, failure) = health_and_failure(value.health.as_ref());
        Self {
            enabled: value.enabled,
            health,
            failure,
            documents: value.documents,
            chunks: value.chunks,
            stale_estimate: value.stale_estimate,
            last_build: value.last_build.map(|when| when.to_string()),
            last_recovery: value.last_recovery.as_ref().map(StoreRecoveryToolResult::from),
        }
    }
}
