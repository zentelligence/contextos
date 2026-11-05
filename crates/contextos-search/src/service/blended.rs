//! Blended search: full-text and semantic results fused into one ranked,
//! file-level list.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use contextos_core::OperationWarning;
use serde_json::Map;
use time::OffsetDateTime;

use crate::{
    CANDIDATE_WINDOW, FreshnessReport, HitMetadata, IndexesText, ResultControls, SearchError, SemanticQuery, SortKey,
    SortOrder, TextHit, TextPage, TextQuery, fuse_by_rank,
};

use super::VaultSearchService;

/// The longest semantic chunk excerpt used as a snippet, in characters.
const SNIPPET_CHARACTERS: usize = 240;

/// Which indexes a blended query consults.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SearchMode {
    /// Both indexes, fused by rank. Falls back to whichever is available.
    #[default]
    Blended,
    /// Full-text search only.
    Text,
    /// Semantic search only.
    Semantic,
}

/// An index that matched a file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchMode {
    Text,
    Semantic,
}

/// One blended search request. Only filters both indexes can honour are
/// offered: the text index's tag and frontmatter field filters have no
/// semantic equivalent, so they stay on the full-text tool.
#[derive(Clone, Copy, Debug)]
pub struct BlendedQuery<'a> {
    pub query: &'a str,
    /// Page size.
    pub limit: usize,
    pub path_prefix: Option<&'a str>,
    pub exclude_paths: &'a [String],
    pub mode: SearchMode,
}

/// One file in a blended result list.
#[derive(Clone, Debug, PartialEq)]
pub struct BlendedHit {
    pub path: String,
    pub title: String,
    /// Fraction of a perfect match, from 0 to 1. What it is measured
    /// against depends on the mode: the best text hit, cosine similarity, or
    /// the best fused rank score.
    pub relevance: f32,
    /// The highlighted text snippet when the text index matched, otherwise
    /// the best matching semantic chunk.
    pub snippet: String,
    pub matched_by: Vec<MatchMode>,
    /// The file's modification time: from the note itself where it can be
    /// read, otherwise the time the text index recorded.
    pub modified: Option<OffsetDateTime>,
    pub metadata: HitMetadata,
}

/// One page of blended results.
#[derive(Clone, Debug)]
pub struct BlendedResults {
    pub hits: Vec<BlendedHit>,
    /// Files passing the controls. Exact for text mode; otherwise counted
    /// over the candidate pools.
    pub total: usize,
    /// True when a candidate pool was truncated, so more matches exist than
    /// `total` counts.
    pub total_capped: bool,
    /// Typed notes about degraded behaviour, such as an index that could not
    /// be consulted.
    pub warnings: Vec<OperationWarning>,
    /// Text index freshness, when the text index was consulted.
    pub freshness: Option<FreshnessReport>,
}

impl VaultSearchService {
    /// Runs a blended query. See [`SearchMode`] for what each mode consults.
    ///
    /// In [`SearchMode::Blended`], an index that is disabled or whose
    /// provider fails, and a query the text index cannot parse, are skipped
    /// with a typed warning and the other index's results are returned,
    /// ranked by rank fusion over whichever index ran. In a single-index mode
    /// the same conditions fail with that index's stable error, except that
    /// [`SearchMode::Semantic`] never reads full-text syntax at all.
    ///
    /// # Errors
    ///
    /// Returns an invalid-page error when the page is out of bounds; the
    /// disabled error of whichever index a single-index mode needs; in
    /// blended mode, an invalid-query error when the query is unparsable and
    /// semantic search cannot answer instead, the generic disabled error when
    /// neither index is enabled or when text search is off and semantic
    /// search is failing, and a typed storage or embedding error otherwise.
    pub fn query_blended(
        &self,
        request: &BlendedQuery<'_>,
        controls: &ResultControls<'_>,
    ) -> Result<BlendedResults, SearchError> {
        controls.ensure_within(request.limit)?;
        match request.mode {
            SearchMode::Text => self.blended_text_only(request, controls),
            SearchMode::Semantic => self.blended_semantic_only(request, controls),
            SearchMode::Blended => self.blended_fused(request, controls),
        }
    }

    /// Full-text results only, keeping the text index's exact totals.
    fn blended_text_only(
        &self,
        request: &BlendedQuery<'_>,
        controls: &ResultControls<'_>,
    ) -> Result<BlendedResults, SearchError> {
        let no_fields = Map::new();
        let results = self.query_text_page(
            &TextQuery {
                query: request.query,
                path_prefix: request.path_prefix,
                exclude_paths: request.exclude_paths,
                tags: &[],
                fields: &no_fields,
                limit: request.limit,
            },
            controls,
        )?;
        let hits = results
            .hits
            .into_iter()
            .map(|described| {
                let title = if described.hit.title.is_empty() {
                    described
                        .metadata
                        .title
                        .clone()
                        .unwrap_or_else(|| described.hit.path.clone())
                } else {
                    described.hit.title
                };
                BlendedHit {
                    path: described.hit.path,
                    title,
                    relevance: described.hit.relevance,
                    snippet: described.hit.snippet,
                    matched_by: vec![MatchMode::Text],
                    modified: described.metadata.modified.or(Some(described.hit.modified)),
                    metadata: described.metadata,
                }
            })
            .collect();
        Ok(BlendedResults {
            hits,
            total: results.total,
            total_capped: false,
            warnings: Vec::new(),
            freshness: Some(results.freshness),
        })
    }

    /// Semantic results only, collapsed to one hit per file, ranked by
    /// similarity. The query is embedded as written; full-text syntax is
    /// never read.
    fn blended_semantic_only(
        &self,
        request: &BlendedQuery<'_>,
        controls: &ResultControls<'_>,
    ) -> Result<BlendedResults, SearchError> {
        let semantic = self.semantic_store()?;
        let (chunks, capped) = self.semantic_pool(semantic, &semantic_request(request), 0.0)?;
        let mut cache = HashMap::new();
        let files = self.note_type_filtered(&mut cache, semantic_files(chunks), controls.note_type);
        let outcome = Outcome {
            limit: request.limit,
            total_capped: capped,
            warnings: Vec::new(),
            freshness: None,
        };
        Ok(self.finish(files, controls, outcome, &mut cache))
    }

    /// Whichever of the two indexes can answer, fused by rank.
    ///
    /// An index that is disabled, a semantic provider or vector store that
    /// fails, and a query the text index cannot parse are each skipped with a
    /// warning, and the remaining index's results are ranked by rank fusion
    /// over the indexes that ran, so relevance means the same thing however
    /// many of them did. When neither can answer the call fails: with the
    /// text parse error if that is what stopped the text side (a mistake in
    /// the request is the actionable problem), otherwise with the generic
    /// disabled error, so a non-technical operator sees one message and the
    /// cause is found through the health check and activity log.
    fn blended_fused(
        &self,
        request: &BlendedQuery<'_>,
        controls: &ResultControls<'_>,
    ) -> Result<BlendedResults, SearchError> {
        let mut warnings = Vec::new();
        let mut freshness = None;
        let mut rejected_query = None;
        let mut text_side = None;
        let mut text_failure = None;
        if self.text.is_some() {
            let no_fields = Map::new();
            let query = TextQuery {
                query: request.query,
                path_prefix: request.path_prefix,
                exclude_paths: request.exclude_paths,
                tags: &[],
                fields: &no_fields,
                limit: CANDIDATE_WINDOW,
            };
            let searched = self.with_text(|text| {
                let refreshed = text.refresh()?;
                Ok((refreshed, text.index().query_window(&query, controls.note_type)))
            });
            match searched {
                Ok((refreshed, window)) => {
                    freshness = Some(refreshed);
                    match window {
                        Ok(window) => text_side = Some(window),
                        Err(error @ SearchError::InvalidQuery { .. }) => {
                            warnings.push(OperationWarning::from(&error));
                            rejected_query = Some(error);
                        }
                        Err(error) => return Err(error),
                    }
                }
                // A text index that cannot be opened or read is skipped like a failing semantic side, so the other
                // index still answers; the failure is reported when neither can.
                Err(error @ SearchError::Store(_)) => {
                    warnings.push(OperationWarning::from(&error));
                    text_failure = Some(error);
                }
                Err(error) => return Err(error),
            }
        } else {
            warnings.push(OperationWarning::from(SearchError::TextDisabled));
        }

        let mut cache = HashMap::new();
        let mut semantic_side = None;
        let mut semantic_failure = None;
        if self.semantic.is_some() {
            match self
                .semantic_store()
                .and_then(|semantic| self.semantic_pool(semantic, &semantic_request(request), 0.0))
            {
                Ok((chunks, capped)) => {
                    let files = self.note_type_filtered(&mut cache, semantic_files(chunks), controls.note_type);
                    semantic_side = Some((files, capped));
                }
                Err(error) if is_degradable(&error) => {
                    warnings.push(OperationWarning::from(&error));
                    semantic_failure = Some(error);
                }
                Err(error) => return Err(error),
            }
        } else {
            warnings.push(OperationWarning::from(SearchError::SemanticUnavailable));
        }

        if text_side.is_none() && semantic_side.is_none() {
            return Err(match (rejected_query, semantic_failure) {
                (Some(rejected), _) => rejected,
                (None, None) if let Some(failure) = text_failure => failure,
                (None, Some(failure)) => SearchError::SearchUnavailable {
                    semantic_code: failure.code(),
                },
                (None, None) => SearchError::SearchDisabled,
            });
        }

        let (files, capped) = fuse_sides(text_side, semantic_side);
        let outcome = Outcome {
            limit: request.limit,
            total_capped: capped,
            warnings,
            freshness,
        };
        Ok(self.finish(files, controls, outcome, &mut cache))
    }

    /// Keeps only files whose frontmatter `type` equals `note_type`, when
    /// one is requested. Files found by the text index are already filtered
    /// by the index, so this is applied to the semantic side.
    fn note_type_filtered(
        &self,
        cache: &mut HashMap<String, HitMetadata>,
        files: Vec<FileEntry>,
        note_type: Option<&str>,
    ) -> Vec<FileEntry> {
        let Some(note_type) = note_type else {
            return files;
        };
        files
            .into_iter()
            .filter_map(|mut file| {
                let metadata = self.cached_metadata(cache, &file.path);
                let keep = metadata.note_type.as_deref() == Some(note_type);
                file.metadata = Some(metadata);
                keep.then_some(file)
            })
            .collect()
    }

    /// Applies the minimum relevance, orders, pages, and describes files.
    fn finish(
        &self,
        mut files: Vec<FileEntry>,
        controls: &ResultControls<'_>,
        outcome: Outcome,
        cache: &mut HashMap<String, HitMetadata>,
    ) -> BlendedResults {
        let Outcome {
            limit,
            total_capped,
            warnings,
            freshness,
        } = outcome;
        let threshold = f32::from(controls.min_relevance);
        files.retain(|file| file.relevance >= threshold);
        let total = files.len();

        if controls.sort.key() != SortKey::Relevance {
            for file in &mut files {
                if file.metadata.is_none() {
                    file.metadata = Some(self.cached_metadata(cache, &file.path));
                }
            }
        }
        files.sort_by(|left, right| compare_files(controls.sort.key(), controls.sort.order(), left, right));

        let hits = files
            .into_iter()
            .skip(controls.offset)
            .take(limit)
            .map(|file| {
                let metadata = file
                    .metadata
                    .clone()
                    .unwrap_or_else(|| self.cached_metadata(cache, &file.path));
                BlendedHit {
                    title: title_of(&file, &metadata),
                    snippet: snippet_of(&file),
                    modified: metadata
                        .modified
                        .or_else(|| file.text_hit.as_ref().map(|hit| hit.modified)),
                    path: file.path,
                    relevance: file.relevance,
                    matched_by: file.matched_by,
                    metadata,
                }
            })
            .collect();
        BlendedResults {
            hits,
            total,
            total_capped,
            warnings,
            freshness,
        }
    }
}

/// What the caller already knows about a result list before it is paged.
struct Outcome {
    limit: usize,
    total_capped: bool,
    warnings: Vec<OperationWarning>,
    freshness: Option<FreshnessReport>,
}

/// One file awaiting ordering and paging.
struct FileEntry {
    path: String,
    relevance: f32,
    matched_by: Vec<MatchMode>,
    text_hit: Option<TextHit>,
    chunk_excerpt: Option<String>,
    metadata: Option<HitMetadata>,
}

/// Fuses whichever sides answered into one entry per file, and reports whether
/// either side's candidate pool was truncated. The relevance of each file is
/// its rank-fusion score over the sides that ran.
fn fuse_sides(text_side: Option<TextPage>, semantic_side: Option<(Vec<FileEntry>, bool)>) -> (Vec<FileEntry>, bool) {
    let mut lists: Vec<Vec<String>> = Vec::new();
    let mut modes = Vec::new();
    let mut text_hits: HashMap<String, TextHit> = HashMap::new();
    let mut text_total = 0;
    if let Some(window) = text_side {
        lists.push(window.hits.iter().map(|hit| hit.path.clone()).collect());
        modes.push(MatchMode::Text);
        text_total = window.total;
        text_hits = window.hits.into_iter().map(|hit| (hit.path.clone(), hit)).collect();
    }
    let mut excerpts: HashMap<String, (String, Option<HitMetadata>)> = HashMap::new();
    let mut semantic_capped = false;
    if let Some((files, capped)) = semantic_side {
        lists.push(files.iter().map(|file| file.path.clone()).collect());
        modes.push(MatchMode::Semantic);
        semantic_capped = capped;
        excerpts = files
            .into_iter()
            .map(|file| (file.path, (file.chunk_excerpt.unwrap_or_default(), file.metadata)))
            .collect();
    }

    let ranked: Vec<&[String]> = lists.iter().map(Vec::as_slice).collect();
    let files = fuse_by_rank(&ranked)
        .into_iter()
        .map(|fused| {
            let matched_by = modes
                .iter()
                .zip(&fused.ranks)
                .filter_map(|(mode, rank)| rank.map(|_| *mode))
                .collect();
            let (chunk_excerpt, metadata) = match excerpts.remove(&fused.path) {
                Some((excerpt, metadata)) => (Some(excerpt), metadata),
                None => (None, None),
            };
            FileEntry {
                text_hit: text_hits.remove(&fused.path),
                path: fused.path,
                relevance: fused.relevance,
                matched_by,
                chunk_excerpt,
                metadata,
            }
        })
        .collect();
    (files, text_total > CANDIDATE_WINDOW || semantic_capped)
}

fn semantic_request<'a>(request: &BlendedQuery<'a>) -> SemanticQuery<'a> {
    SemanticQuery {
        query: request.query,
        limit: CANDIDATE_WINDOW,
        path_prefix: request.path_prefix,
        exclude_paths: request.exclude_paths,
    }
}

/// Collapses ranked chunks to one entry per file, keeping each file's best
/// chunk (the first, since chunks arrive most similar first).
fn semantic_files(chunks: Vec<super::paged::SemanticEntry>) -> Vec<FileEntry> {
    let mut seen = HashSet::new();
    chunks
        .into_iter()
        .filter(|entry| seen.insert(entry.hit.path.clone()))
        .map(|entry| FileEntry {
            relevance: entry.hit.relevance,
            matched_by: vec![MatchMode::Semantic],
            text_hit: None,
            chunk_excerpt: Some(entry.hit.chunk.chars().take(SNIPPET_CHARACTERS).collect()),
            metadata: entry.metadata,
            path: entry.hit.path,
        })
        .collect()
}

/// Whether a semantic failure is one of availability (no provider, provider
/// unreachable, store unreadable) rather than a fault in the request.
fn is_degradable(error: &SearchError) -> bool {
    matches!(error, SearchError::SemanticUnavailable)
        || error.code().starts_with("embedding/")
        || error.code().starts_with("vector/")
}

fn title_of(file: &FileEntry, metadata: &HitMetadata) -> String {
    file.text_hit
        .as_ref()
        .map(|hit| hit.title.clone())
        .filter(|title| !title.is_empty())
        .or_else(|| metadata.title.clone())
        .unwrap_or_else(|| file.path.clone())
}

fn snippet_of(file: &FileEntry) -> String {
    file.text_hit
        .as_ref()
        .map(|hit| hit.snippet.clone())
        .or_else(|| file.chunk_excerpt.clone())
        .unwrap_or_default()
}

fn modified_of(file: &FileEntry) -> OffsetDateTime {
    file.metadata
        .as_ref()
        .and_then(|metadata| metadata.modified)
        .or_else(|| file.text_hit.as_ref().map(|hit| hit.modified))
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// Orders by the requested key and direction, breaking ties by ascending
/// path so the order is total and deterministic.
fn compare_files(key: SortKey, order: SortOrder, left: &FileEntry, right: &FileEntry) -> Ordering {
    let primary = match key {
        SortKey::Relevance => left.relevance.total_cmp(&right.relevance),
        SortKey::Title => {
            let by = |file: &FileEntry| {
                let metadata = file.metadata.clone().unwrap_or_default();
                title_of(file, &metadata).to_lowercase()
            };
            by(left).cmp(&by(right))
        }
        SortKey::Modified => modified_of(left).cmp(&modified_of(right)),
    };
    let primary = match order {
        SortOrder::Ascending => primary,
        SortOrder::Descending => primary.reverse(),
    };
    primary.then_with(|| left.path.cmp(&right.path))
}
