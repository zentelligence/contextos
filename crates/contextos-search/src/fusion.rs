//! Reciprocal rank fusion of ranked file lists.
//!
//! Full-text scores and semantic similarities live on unrelated scales, so
//! blended search fuses the two by rank instead. Each list contributes
//! `1 / (K + rank)` for every file it contains, and a file's fused score is
//! the sum of its contributions. The score is then expressed as a fraction of
//! the best score a single file could reach, so it reads as 0 to 1 and can be
//! compared with a minimum relevance threshold.

/// The rank constant `K`. A larger value flattens the gap between the top
/// ranks; 60 is the value the technique is conventionally used with.
pub const FUSION_RANK_CONSTANT: f32 = 60.0;

use std::collections::{BTreeMap, BTreeSet};

/// One file after fusion.
#[derive(Clone, Debug, PartialEq)]
pub struct FusedFile {
    pub path: String,
    /// The fused score as a fraction of the best attainable score, in the
    /// range above 0 up to 1.
    pub relevance: f32,
    /// The file's 1-based rank in each input list, in input order, or `None`
    /// where that list did not contain it.
    pub ranks: Vec<Option<usize>>,
}

/// Fuses ranked lists of file paths, each best first, into one list ordered
/// by fused relevance, breaking ties by ascending path.
///
/// Every list passed counts as a mode that ran, even when empty, so a file
/// found by only one of two modes is scored against what two modes could
/// have given it. A path repeated within one list counts once, at its first
/// (best) position.
#[must_use]
pub fn fuse_by_rank(lists: &[&[String]]) -> Vec<FusedFile> {
    let mut scored: BTreeMap<&str, (f32, Vec<Option<usize>>)> = BTreeMap::new();
    for (index, list) in lists.iter().enumerate() {
        let mut seen = BTreeSet::new();
        let mut rank = 0_usize;
        for path in *list {
            if !seen.insert(path.as_str()) {
                continue;
            }
            rank = rank.saturating_add(1);
            let (score, ranks) = scored
                .entry(path.as_str())
                .or_insert_with(|| (0.0, vec![None; lists.len()]));
            *score += reciprocal(rank);
            if let Some(slot) = ranks.get_mut(index) {
                *slot = Some(rank);
            }
        }
    }

    let attainable: f32 = lists.iter().map(|_| reciprocal(1)).sum();
    let mut fused: Vec<FusedFile> = scored
        .into_iter()
        .map(|(path, (score, ranks))| FusedFile {
            path: path.to_owned(),
            relevance: (score / attainable).min(1.0),
            ranks,
        })
        .collect();
    fused.sort_by(|left, right| {
        right
            .relevance
            .total_cmp(&left.relevance)
            .then_with(|| left.path.cmp(&right.path))
    });
    fused
}

/// The contribution of a file at the 1-based `rank`. Ranks come from a
/// bounded candidate window, far below `u16::MAX`.
fn reciprocal(rank: usize) -> f32 {
    1.0 / (FUSION_RANK_CONSTANT + f32::from(u16::try_from(rank).unwrap_or(u16::MAX)))
}

#[cfg(test)]
#[path = "fusion_test.rs"]
mod tests;
