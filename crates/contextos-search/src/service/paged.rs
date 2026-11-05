//! Paged, sorted, and filtered search over a vault's text and semantic
//! indexes, with each returned hit described from its source file.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::collections::hash_map::Entry;

use time::OffsetDateTime;

use crate::{
    CANDIDATE_WINDOW, Chunk, FreshnessReport, HitMetadata, IndexesText, ReadsHitMetadata, ResultControls, SearchError,
    SemanticHit, SemanticQuery, SimilarityQuery, SortKey, SortOrder, SortSpec, StoresVectors, TextHit, TextQuery,
};

use super::{SemanticWorker, VaultSearchService};

/// A full-text hit together with its note's descriptive metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct DescribedTextHit {
    pub hit: TextHit,
    pub metadata: HitMetadata,
}

/// One page of full-text results.
#[derive(Clone, Debug)]
pub struct TextResults {
    pub hits: Vec<DescribedTextHit>,
    /// Every note matching the query and controls, not only this page.
    pub total: usize,
    pub freshness: FreshnessReport,
}

/// A semantic hit together with its note's descriptive metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct DescribedSemanticHit {
    pub hit: SemanticHit,
    pub metadata: HitMetadata,
}

/// One page of semantic results.
#[derive(Clone, Debug)]
pub struct SemanticResults {
    pub hits: Vec<DescribedSemanticHit>,
    /// Chunks in the candidate pool that pass the controls.
    pub total: usize,
    /// True when the candidate pool was full, so more matches exist beyond
    /// it than `total` counts.
    pub total_capped: bool,
}

impl VaultSearchService {
    /// Runs a paged full-text query. See [`ResultControls`] for the
    /// ordering, filtering, and bounds applied.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::TextDisabled`] when text search is disabled,
    /// an invalid-page error when the page is out of bounds, an
    /// invalid-query error for unparsable syntax or filter values, and a
    /// storage error when the index cannot be read.
    pub fn query_text_page(
        &self,
        request: &TextQuery<'_>,
        controls: &ResultControls<'_>,
    ) -> Result<TextResults, SearchError> {
        controls.ensure_within(request.limit)?;
        let (freshness, page) =
            self.with_text(|text| Ok((text.refresh()?, text.index().query_page(request, controls)?)))?;
        let hits = page
            .hits
            .into_iter()
            .map(|hit| {
                let metadata = self.metadata.read(&hit.path);
                DescribedTextHit { hit, metadata }
            })
            .collect();
        Ok(TextResults {
            hits,
            total: page.total,
            freshness,
        })
    }

    /// Runs a paged semantic query over the ranked candidate pool.
    ///
    /// The pool is the [`CANDIDATE_WINDOW`] chunks most similar to the
    /// query. The minimum relevance, stale-hit removal, and note type
    /// filter apply to that pool, then it is sorted and paged, so `total`
    /// counts pool members that survive the controls.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::SemanticUnavailable`] when semantic search is
    /// disabled, an invalid-page error when the page is out of bounds, and a
    /// typed embedding or storage error when the query cannot be embedded or
    /// the vector store cannot be read.
    pub fn query_semantic_page(
        &self,
        request: &SemanticQuery<'_>,
        controls: &ResultControls<'_>,
    ) -> Result<SemanticResults, SearchError> {
        controls.ensure_within(request.limit)?;
        let semantic = self.semantic_store()?;
        let (mut entries, total_capped) = self.semantic_pool(semantic, request, f32::from(controls.min_relevance))?;

        let mut cache: HashMap<String, HitMetadata> = HashMap::new();
        let needs_metadata_to_order = controls.note_type.is_some() || controls.sort.key() != SortKey::Relevance;
        if needs_metadata_to_order {
            for entry in &mut entries {
                entry.metadata = Some(self.cached_metadata(&mut cache, &entry.hit.path));
            }
            if let Some(note_type) = controls.note_type {
                entries.retain(|entry| {
                    entry
                        .metadata
                        .as_ref()
                        .is_some_and(|metadata| metadata.note_type.as_deref() == Some(note_type))
                });
            }
        }

        let total = entries.len();
        entries.sort_by(|left, right| compare_entries(controls.sort, left, right));

        let hits = entries
            .into_iter()
            .skip(controls.offset)
            .take(request.limit)
            .map(|entry| {
                let metadata = entry
                    .metadata
                    .unwrap_or_else(|| self.cached_metadata(&mut cache, &entry.hit.path));
                DescribedSemanticHit {
                    hit: entry.hit,
                    metadata,
                }
            })
            .collect();
        Ok(SemanticResults {
            hits,
            total,
            total_capped,
        })
    }

    /// Takes the [`CANDIDATE_WINDOW`] chunks most similar to the query,
    /// keeps those at or above `threshold`, and drops any whose source
    /// changed since it was embedded or can no longer be read safely. Also
    /// reports whether more matches exist beyond the pool.
    ///
    /// A note that is unreadable or that now resolves outside the vault is
    /// skipped like a stale one, so one bad file cannot fail the whole page.
    /// Each note is read and chunked once per query however many of its
    /// chunks are in the pool.
    pub(super) fn semantic_pool(
        &self,
        semantic: &SemanticWorker,
        request: &SemanticQuery<'_>,
        threshold: f32,
    ) -> Result<(Vec<SemanticEntry>, bool), SearchError> {
        let vector = Self::embed_query(semantic, request.query)?;
        // One more than the window, so a full pool can be told apart from a
        // pool that merely happens to hold exactly the window.
        let mut pool = semantic.store().similar(&SimilarityQuery {
            vector: &vector,
            k: CANDIDATE_WINDOW + 1,
            path_prefix: request.path_prefix,
            exclude_paths: request.exclude_paths,
        })?;
        let capped = pool.len() > CANDIDATE_WINDOW;
        pool.truncate(CANDIDATE_WINDOW);

        let mut rechunked: HashMap<String, Option<Vec<Chunk>>> = HashMap::new();
        let mut entries = Vec::with_capacity(pool.len());
        for hit in pool {
            let relevance = hit.score.clamp(0.0, 1.0);
            if relevance < threshold {
                continue;
            }
            let chunks = match rechunked.entry(hit.path.clone()) {
                Entry::Occupied(known) => known.into_mut(),
                Entry::Vacant(slot) => match self.rechunk(semantic, &hit.path) {
                    Ok(chunks) => slot.insert(chunks),
                    Err(error) if is_unreadable(&error) => {
                        tracing::debug!(code = error.code(), "skipping a semantic hit whose note cannot be read");
                        slot.insert(None)
                    }
                    Err(error) => return Err(error),
                },
            };
            // A chunk whose source changed since it was embedded is skipped,
            // as the unpaged query does.
            let Some(chunk) = chunks
                .as_ref()
                .and_then(|chunks| chunks.iter().find(|chunk| chunk.ordinal() == hit.ordinal))
            else {
                continue;
            };
            entries.push(SemanticEntry {
                hit: SemanticHit {
                    path: hit.path,
                    chunk: chunk.text().to_owned(),
                    score: hit.score,
                    relevance,
                    heading_context: hit.heading_context,
                },
                ordinal: hit.ordinal,
                metadata: None,
            });
        }
        Ok((entries, capped))
    }

    /// Reads a note's metadata at most once per query.
    pub(super) fn cached_metadata(&self, cache: &mut HashMap<String, HitMetadata>, path: &str) -> HitMetadata {
        cache
            .entry(path.to_owned())
            .or_insert_with(|| self.metadata.read(path))
            .clone()
    }
}

/// One semantic candidate awaiting ordering.
pub(super) struct SemanticEntry {
    pub(super) hit: SemanticHit,
    pub(super) ordinal: usize,
    pub(super) metadata: Option<HitMetadata>,
}

/// Orders by the requested key and direction, breaking ties by ascending
/// path and then chunk ordinal so the order is total and deterministic.
fn compare_entries(sort: SortSpec, left: &SemanticEntry, right: &SemanticEntry) -> Ordering {
    let primary = match sort.key() {
        SortKey::Relevance => left.hit.relevance.total_cmp(&right.hit.relevance),
        SortKey::Title => title_of(left).cmp(&title_of(right)),
        SortKey::Modified => modified_of(left).cmp(&modified_of(right)),
    };
    let primary = match sort.order() {
        SortOrder::Ascending => primary,
        SortOrder::Descending => primary.reverse(),
    };
    primary
        .then_with(|| left.hit.path.cmp(&right.hit.path))
        .then_with(|| left.ordinal.cmp(&right.ordinal))
}

fn title_of(entry: &SemanticEntry) -> String {
    entry
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.title.clone())
        .unwrap_or_else(|| entry.hit.path.clone())
        .to_lowercase()
}

fn modified_of(entry: &SemanticEntry) -> OffsetDateTime {
    entry
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.modified)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// Whether a failure to re-read one hit's note means "this hit is gone" rather
/// than "the search is broken": the note cannot be opened, or its path is no
/// longer valid inside the vault.
fn is_unreadable(error: &SearchError) -> bool {
    matches!(
        error,
        SearchError::DocumentRead { .. } | SearchError::EmbeddingPathInvalid { .. }
    )
}

#[cfg(test)]
#[path = "paged_test.rs"]
mod tests;
