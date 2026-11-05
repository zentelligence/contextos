use contextos_core::{OperationWarning, PathError};
use thiserror::Error;

/// Typed failures for the search services.
#[derive(Debug, Error)]
pub enum SearchError {
    #[error("text index storage failed: {source}")]
    IndexStorage {
        #[from]
        source: tantivy::TantivyError,
    },
    #[error("index directory {path} could not be prepared: {source}")]
    IndexDirectory { path: String, source: std::io::Error },
    #[error("query '{query}' is invalid: {reason}")]
    InvalidQuery { query: String, reason: String },
    #[error("document {path} could not be read for indexing: {source}")]
    DocumentRead { path: String, source: std::io::Error },
    #[error("frontmatter filter '{field}' must be a string, number, or boolean")]
    InvalidFieldFilter { field: String },
    #[error("link graph cache {path} could not be persisted: {source}")]
    GraphStorage { path: String, source: std::io::Error },
    #[error(transparent)]
    Store(#[from] crate::StoreFailure),
    #[error(
        "the damaged {} at {path} is still open (in {other_processes} other contextos processes, and in {this_process} \
         calls still running in this one), so it cannot be moved aside yet",
        store.label()
    )]
    StoreInUse {
        store: crate::DerivedStore,
        path: String,
        other_processes: usize,
        this_process: usize,
    },
    #[error("link graph store {path} is locked by another process")]
    GraphLocked { path: String },
    #[error("graph depth {depth} is outside the supported range 1 to 4")]
    InvalidDepth { depth: u32 },
    #[error("minimum relevance {value} is outside the supported range 0 to 1")]
    InvalidRelevance { value: f64 },
    #[error("the {operation} request is invalid: {reason}")]
    InvalidLinkReport { operation: &'static str, reason: String },
    #[error("page size {limit} exceeds the maximum of {maximum}")]
    PageLimitExceeded { limit: usize, maximum: usize },
    #[error("a page ending at result {end} extends beyond the {window}-result candidate window")]
    PageBeyondWindow { end: usize, window: usize },
    #[error("note {path} is not present in the link graph")]
    UnknownNote { path: String },
    #[error("text search is disabled for this vault")]
    TextDisabled,
    #[error("the link graph is disabled for this vault")]
    GraphDisabled,
    #[error("semantic search is not enabled for this vault")]
    SemanticUnavailable,
    #[error("neither text search nor semantic search is enabled for this vault")]
    SearchDisabled,
    #[error("search is unavailable: text search is disabled and semantic search failed ({semantic_code})")]
    SearchUnavailable { semantic_code: &'static str },
    #[error("the local embedding model at {directory} is unavailable: {reason}")]
    EmbeddingModelUnavailable { directory: String, reason: String },
    #[error("the local embedding provider is not compiled into this build (the `semantic-local` feature is off)")]
    LocalEmbeddingDisabled,
    #[error("environment variable {variable}, named by this vault's api_key_env, is not set")]
    EmbeddingApiKeyMissing { variable: String },
    #[error("embedding provider configuration is invalid: {reason}")]
    EmbeddingConfig { reason: String },
    #[error(
        "embedding endpoint {endpoint} is not https and is not a loopback address; refusing to send credentials in cleartext"
    )]
    EmbeddingInsecureEndpoint { endpoint: String },
    #[error("embedding request to {endpoint} timed out after {timeout_ms} ms")]
    EmbeddingTimeout { endpoint: String, timeout_ms: u64 },
    #[error("embedding response from {endpoint} exceeded the {limit_bytes}-byte limit")]
    EmbeddingResponseTooLarge { endpoint: String, limit_bytes: usize },
    #[error("embedding request to {endpoint} failed with status {status}: {body_excerpt}")]
    EmbeddingProviderStatus {
        endpoint: String,
        status: u16,
        body_excerpt: String,
    },
    #[error("embedding request to {endpoint} could not be completed: {reason}")]
    EmbeddingTransport { endpoint: String, reason: String },
    #[error("embedding provider returned an unexpected shape: {reason}")]
    EmbeddingShapeMismatch { reason: String },
    #[error("vector store dimension must be a positive integer, received {dimension}")]
    VectorDimensionInvalid { dimension: usize },
    #[error("vector for {path}#{ordinal} has {actual} components, but this store is configured for {expected}")]
    VectorDimensionMismatch {
        path: String,
        ordinal: usize,
        expected: usize,
        actual: usize,
    },
    #[error("chunk ordinal {ordinal} for {path} exceeds the supported range")]
    VectorOrdinalOutOfRange { path: String, ordinal: usize },
    #[error("vector store {path} failed: {source}")]
    VectorStorage {
        path: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error("vector store {path} holds a corrupt row: {reason}")]
    VectorRecordCorrupt { path: String, reason: String },
    #[error("path {path} is not valid for embedding: {source}")]
    EmbeddingPathInvalid {
        path: String,
        #[source]
        source: PathError,
    },
    #[error("file {path} cannot be recorded in the link graph: {source}")]
    GraphPathInvalid {
        path: String,
        #[source]
        source: PathError,
    },
    #[error("the vault's hidden patterns are invalid: {source}")]
    HiddenPatternsInvalid {
        #[source]
        source: contextos_fs::FsError,
    },
}

impl SearchError {
    /// Returns the stable machine-readable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::IndexStorage { .. }
            | Self::IndexDirectory { .. }
            | Self::DocumentRead { .. }
            | Self::GraphStorage { .. } => "index/storage",
            Self::Store(failure) => failure.code(),
            Self::GraphLocked { .. } => "index/locked",
            Self::StoreInUse { .. } => "index/in-use",
            Self::InvalidQuery { .. }
            | Self::InvalidFieldFilter { .. }
            | Self::InvalidDepth { .. }
            | Self::InvalidLinkReport { .. }
            | Self::InvalidRelevance { .. }
            | Self::PageLimitExceeded { .. }
            | Self::PageBeyondWindow { .. } => "index/invalid-query",
            Self::UnknownNote { .. } => "path/not-found",
            Self::TextDisabled
            | Self::GraphDisabled
            | Self::SemanticUnavailable
            | Self::SearchDisabled
            | Self::SearchUnavailable { .. } => "index/disabled",
            Self::EmbeddingModelUnavailable { .. } | Self::LocalEmbeddingDisabled => "embedding/local-unavailable",
            Self::EmbeddingApiKeyMissing { .. }
            | Self::EmbeddingConfig { .. }
            | Self::EmbeddingInsecureEndpoint { .. } => "embedding/config",
            Self::EmbeddingTimeout { .. } => "embedding/timeout",
            Self::EmbeddingResponseTooLarge { .. } => "embedding/response-too-large",
            Self::EmbeddingProviderStatus { .. } | Self::EmbeddingShapeMismatch { .. } => "embedding/provider-error",
            Self::EmbeddingTransport { .. } => "embedding/network",
            Self::VectorDimensionInvalid { .. }
            | Self::VectorDimensionMismatch { .. }
            | Self::VectorOrdinalOutOfRange { .. } => "vector/config",
            Self::VectorStorage { .. } | Self::VectorRecordCorrupt { .. } => "vector/storage",
            Self::EmbeddingPathInvalid { source, .. } | Self::GraphPathInvalid { source, .. } => source.code(),
            Self::HiddenPatternsInvalid { .. } => "index/config",
        }
    }

    /// Returns an actionable remediation hint for the operator.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::IndexStorage { .. }
            | Self::IndexDirectory { .. }
            | Self::DocumentRead { .. }
            | Self::GraphStorage { .. } => "Run query_index_rebuild to regenerate the derived index state.",
            Self::Store(failure) => failure.remediation(),
            Self::StoreInUse { .. } => {
                "This damaged store is still open, in another contextos process or in a call still running in this \
                 one, so it cannot be moved aside yet. Each instance releases a store as soon as it finds it damaged, \
                 and a call releases it when it finishes; retry shortly, and if it is still in use, stop the other \
                 contextos processes using this vault, then run the recovery again."
            }
            Self::GraphLocked { .. } => {
                "Another process already has this vault's link graph open. Close it, or wait for \
                 it to exit, then retry; the link graph is disabled for this vault meanwhile and \
                 other capabilities are unaffected."
            }
            Self::InvalidQuery { .. } => "Correct the query; plain terms and tantivy query syntax are accepted.",
            Self::InvalidFieldFilter { .. } => "Provide a scalar value for each frontmatter field filter.",
            Self::InvalidDepth { .. } => "Use a depth between 1 and 4.",
            Self::InvalidLinkReport { .. } => {
                "Give `path` as a plain vault-relative folder such as `memory/wiki`, `offset` as zero or more, and \
                 `limit` as 1 to 1000, and leave out `from`, `to`, `depth`, and `direction`, which only the \
                 traversal operations take."
            }
            Self::InvalidRelevance { .. } => "Use a minimum relevance between 0 and 1 inclusive.",
            Self::PageLimitExceeded { .. } => {
                "Request a smaller page and use offset to walk further results, up to the 200-result window."
            }
            Self::PageBeyondWindow { .. } => {
                "Search results are limited to the first 200 in the requested order; narrow the query with filters \
                 or a higher minimum relevance instead of paging beyond them."
            }
            Self::UnknownNote { .. } => "Check the note path against the vault; use the forward-slash relative path.",
            Self::TextDisabled => {
                "Enable `[vault.search] text = true` for this managed vault, and rebuild the text index."
            }
            Self::GraphDisabled => {
                "Enable `[vault.search] graph = true` for this managed vault, and rebuild the link graph."
            }
            Self::SemanticUnavailable => {
                "Enable [vault.search] semantic = true and configure an embedding provider for this vault, then retry."
            }
            Self::SearchUnavailable { .. } => {
                "Search cannot answer right now. Open the vault health check (the doctor tool or the health indicator) \
                 and the activity log to see why semantic search is failing, or enable text search so search keeps \
                 working meanwhile."
            }
            Self::SearchDisabled => {
                "Enable `[vault.search] text = true`, or `semantic = true` with an embedding provider, for this vault, then rebuild the index."
            }
            Self::EmbeddingModelUnavailable { .. } => {
                "Point model_directory at a directory holding the ONNX model and tokenizer files, or run the model pre-fetch tooling once it is available."
            }
            Self::LocalEmbeddingDisabled => {
                "Rebuild with the default `semantic-local` feature enabled, or select the openai-compatible provider instead."
            }
            Self::EmbeddingApiKeyMissing { .. } => {
                "Set the environment variable named by this vault's api_key_env, or write the key itself as api_key instead, before selecting the openai-compatible provider."
            }
            Self::EmbeddingConfig { .. } => {
                "Correct the embedding provider configuration; the endpoint must be a valid URL."
            }
            Self::EmbeddingInsecureEndpoint { .. } => {
                "Use an https:// endpoint, or point the openai-compatible provider at a loopback address (127.0.0.1, ::1, or localhost) for local testing."
            }
            Self::EmbeddingTimeout { .. } => {
                "The embedding provider did not respond in time; check its availability or increase the configured timeout."
            }
            Self::EmbeddingResponseTooLarge { .. } => {
                "The embedding provider's response exceeded the configured size limit; check the provider and batch size."
            }
            Self::EmbeddingProviderStatus { .. } => {
                "Check the embedding provider's endpoint, model name, and credentials."
            }
            Self::EmbeddingTransport { .. } => "Check network reachability to the configured embedding endpoint.",
            Self::EmbeddingShapeMismatch { .. } => {
                "The embedding provider returned a response that does not match the request; check the provider's API compatibility."
            }
            Self::VectorDimensionInvalid { .. } => {
                "Configure the vector store with a positive embedding dimension matching the selected provider."
            }
            Self::VectorDimensionMismatch { .. } => {
                "Rebuild the semantic index with query_index_rebuild after changing the embedding provider or model; vectors from a previous dimension cannot mix with the new one."
            }
            Self::VectorOrdinalOutOfRange { .. } => {
                "This chunk ordinal cannot be represented by the vector store; re-run chunking with a document producing fewer chunks."
            }
            Self::VectorStorage { .. } => {
                "Run query_index_rebuild to regenerate the vector store, or check disk space and permissions for .contextos/vectors.db."
            }
            Self::VectorRecordCorrupt { .. } => {
                "Run query_index_rebuild to regenerate the vector store from vault content."
            }
            Self::EmbeddingPathInvalid { source, .. } | Self::GraphPathInvalid { source, .. } => source.remediation(),
            Self::HiddenPatternsInvalid { .. } => {
                "Correct the vault's `hidden` patterns in the configuration, which use the same syntax as a .gitignore \
                 file."
            }
        }
    }
}

impl From<&SearchError> for OperationWarning {
    fn from(value: &SearchError) -> Self {
        Self {
            code: value.code().to_owned(),
            message: value.to_string(),
        }
    }
}

impl From<SearchError> for OperationWarning {
    fn from(value: SearchError) -> Self {
        Self {
            code: value.code().to_owned(),
            message: value.to_string(),
        }
    }
}
