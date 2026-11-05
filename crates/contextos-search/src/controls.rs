//! Result controls shared by the ranked search tools: sort, minimum
//! relevance, note type filtering, and bounded paging.
//!
//! Every paged entry point takes a request (which already carries `limit`)
//! together with a [`ResultControls`]. The controls are validated by
//! [`ResultControls::ensure_within`] before any index is read, so an
//! oversized page or offset is rejected up front rather than allocated for.

use crate::SearchError;

/// Largest page a caller may request from any paged search entry point.
pub const MAX_PAGE_LIMIT: usize = 100;

/// Depth of the ranked window every paged search entry point exposes. A page
/// must end inside it, so the largest offset is this minus the page size.
///
/// Semantic and blended search draw their candidates from a pool of exactly
/// this depth, so it is also the most they can enumerate. Full-text search
/// counts every match exactly but exposes the same window, so all three tools
/// share one paging rule.
pub const CANDIDATE_WINDOW: usize = 200;

/// The property a result list is ordered by.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortKey {
    /// Most relevant first by default.
    #[default]
    Relevance,
    /// Alphabetical by title, case-insensitively.
    Title,
    /// By file modification time.
    Modified,
}

/// The direction a result list is ordered in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

/// A sort key with its direction. Ties are always broken by ascending path,
/// so every ordering is total and deterministic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SortSpec {
    key: SortKey,
    order: SortOrder,
}

impl SortSpec {
    /// The property results are ordered by.
    #[must_use]
    pub const fn key(&self) -> SortKey {
        self.key
    }

    /// The direction results are ordered in.
    #[must_use]
    pub const fn order(&self) -> SortOrder {
        self.order
    }
}

impl Default for SortSpec {
    fn default() -> Self {
        Self::from(SortKey::Relevance)
    }
}

impl From<SortKey> for SortSpec {
    /// Uses the natural direction for the key: most relevant first, most
    /// recently modified first, and alphabetical for titles.
    fn from(key: SortKey) -> Self {
        let order = match key {
            SortKey::Relevance | SortKey::Modified => SortOrder::Descending,
            SortKey::Title => SortOrder::Ascending,
        };
        Self { key, order }
    }
}

impl From<(SortKey, Option<SortOrder>)> for SortSpec {
    /// Uses the explicit direction when given, otherwise the key's natural
    /// direction.
    fn from(value: (SortKey, Option<SortOrder>)) -> Self {
        let natural = Self::from(value.0);
        Self {
            key: value.0,
            order: value.1.unwrap_or(natural.order),
        }
    }
}

/// A minimum relevance threshold in the closed range 0 to 1.
///
/// Relevance is a fraction of an ideal match, so `0.5` means "at least half
/// as relevant as the best possible result". What that fraction is measured
/// against depends on the search mode and is defined by each entry point.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct MinRelevance(f32);

impl TryFrom<f64> for MinRelevance {
    type Error = SearchError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        if !(0.0..=1.0).contains(&value) {
            return Err(SearchError::InvalidRelevance { value });
        }
        // The range check above guarantees the value is representable.
        #[allow(clippy::cast_possible_truncation)]
        Ok(Self(value as f32))
    }
}

impl From<MinRelevance> for f32 {
    fn from(value: MinRelevance) -> Self {
        value.0
    }
}

/// Ordering, filtering, and offset applied to a ranked result list.
///
/// The page size is the request's own `limit`. The default is the first
/// page in relevance order with no filtering, which is exactly what the
/// unpaged entry points return.
#[derive(Clone, Copy, Debug, Default)]
pub struct ResultControls<'a> {
    /// Only notes whose frontmatter `type` equals this value.
    pub note_type: Option<&'a str>,
    /// Number of ranked results to skip.
    pub offset: usize,
    pub sort: SortSpec,
    pub min_relevance: MinRelevance,
}

impl ResultControls<'_> {
    /// Checks that a page of `limit` results at this offset is within
    /// bounds.
    ///
    /// # Errors
    ///
    /// Returns a typed invalid-page error when the limit exceeds
    /// [`MAX_PAGE_LIMIT`] or the page ends beyond [`CANDIDATE_WINDOW`].
    pub fn ensure_within(&self, limit: usize) -> Result<(), SearchError> {
        if limit > MAX_PAGE_LIMIT {
            return Err(SearchError::PageLimitExceeded {
                limit,
                maximum: MAX_PAGE_LIMIT,
            });
        }
        let end = self.offset.saturating_add(limit);
        if end > CANDIDATE_WINDOW {
            return Err(SearchError::PageBeyondWindow {
                end,
                window: CANDIDATE_WINDOW,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "controls_test.rs"]
mod tests;
