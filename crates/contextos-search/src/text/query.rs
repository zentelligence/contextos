//! Paged, sorted, and filtered full-text queries over the Tantivy index.
//!
//! Every match is scored so that the total, the minimum relevance threshold,
//! and the sort order are all exact over the whole match set. Only the
//! returned page's stored documents are then loaded for snippets. The one
//! exception is title sorting, which must read every candidate's stored
//! title because titles are not a fast field.

use std::cmp::Ordering;

use serde_json::Value;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{IndexRecordOption, TantivyDocument};
use tantivy::snippet::SnippetGenerator;
use tantivy::{DateTime, DocAddress, Searcher};

use super::{
    TantivyIndex, TextHit, TextPage, TextQuery, facet_clause, json_term, render_snippet, stored_date, stored_text,
};
use crate::{ResultControls, SearchError, SortKey, SortOrder, SortSpec};

/// One scored match awaiting ordering.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    address: DocAddress,
    score: f32,
    relevance: f32,
}

impl TantivyIndex {
    /// Runs `request` under `controls` without bounding the page, so the
    /// unpaged entry point keeps accepting any `limit`. Paged callers go
    /// through `TantivyIndex::query_page`, which validates the page first.
    pub(super) fn run_query(
        &self,
        request: &TextQuery<'_>,
        controls: &ResultControls<'_>,
    ) -> Result<TextPage, SearchError> {
        let searcher = self.current_searcher()?;
        let query = self.build_query(request, controls)?;

        let capacity = usize::try_from(searcher.num_docs()).unwrap_or(usize::MAX).max(1);
        let matches = searcher.search(&query, &TopDocs::with_limit(capacity).order_by_score())?;

        let admitted = admit_relevant(&matches, f32::from(controls.min_relevance));
        let total = admitted.len();
        let ranked = self.order(&searcher, admitted, controls.sort)?;

        let mut generator = SnippetGenerator::create(&searcher, &*query, self.fields.body)?;
        generator.set_max_num_chars(240);

        let mut hits = Vec::with_capacity(request.limit.min(total));
        for candidate in ranked.into_iter().skip(controls.offset).take(request.limit) {
            let stored: TantivyDocument = searcher.doc(candidate.address)?;
            let body = stored_text(&stored, self.fields.body);
            hits.push(TextHit {
                path: stored_text(&stored, self.fields.path),
                score: candidate.score,
                relevance: candidate.relevance,
                title: stored_text(&stored, self.fields.title),
                snippet: render_snippet(&generator, &body),
                modified: stored_date(&stored, self.fields.modified),
            });
        }
        Ok(TextPage { hits, total })
    }

    fn build_query(
        &self,
        request: &TextQuery<'_>,
        controls: &ResultControls<'_>,
    ) -> Result<Box<dyn Query>, SearchError> {
        let mut parser = QueryParser::for_index(
            &self.index,
            vec![self.fields.title, self.fields.headings, self.fields.body],
        );
        parser.set_field_boost(self.fields.title, 2.0);
        parser.set_field_boost(self.fields.headings, 1.5);
        let parsed = parser
            .parse_query(request.query)
            .map_err(|error| SearchError::InvalidQuery {
                query: request.query.to_owned(),
                reason: error.to_string(),
            })?;

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, parsed)];
        if let Some(prefix) = request.path_prefix {
            clauses.push(facet_clause(self.fields.path_facet, prefix, Occur::Must));
        }
        for excluded in request.exclude_paths {
            clauses.push(facet_clause(self.fields.path_facet, excluded, Occur::MustNot));
        }
        for tag in request.tags {
            clauses.push(facet_clause(self.fields.tags, tag, Occur::Must));
        }
        for (field, value) in request.fields {
            let term = json_term(self.fields.frontmatter, field, value)?;
            clauses.push((Occur::Must, Box::new(TermQuery::new(term, IndexRecordOption::Basic))));
        }
        if let Some(note_type) = controls.note_type {
            let term = json_term(self.fields.frontmatter, "type", &Value::String(note_type.to_owned()))?;
            clauses.push((Occur::Must, Box::new(TermQuery::new(term, IndexRecordOption::Basic))));
        }
        Ok(Box::new(BooleanQuery::new(clauses)))
    }

    /// Orders candidates by `sort`, breaking every tie by ascending path so
    /// the order is total and deterministic.
    fn order(
        &self,
        searcher: &Searcher,
        candidates: Vec<Candidate>,
        sort: SortSpec,
    ) -> Result<Vec<Candidate>, SearchError> {
        match sort.key() {
            SortKey::Relevance => {
                let keyed = candidates
                    .into_iter()
                    .map(|candidate| (i64::from(candidate.score.to_bits()), candidate))
                    .collect();
                self.order_numerically(searcher, keyed, sort)
            }
            SortKey::Modified => {
                let columns = searcher
                    .segment_readers()
                    .iter()
                    .map(|reader| reader.fast_fields().date("modified"))
                    .collect::<Result<Vec<_>, _>>()?;
                let keyed = candidates
                    .into_iter()
                    .map(|candidate| {
                        let modified = usize::try_from(candidate.address.segment_ord)
                            .ok()
                            .and_then(|segment| columns.get(segment))
                            .and_then(|column| column.first(candidate.address.doc_id))
                            .map_or(0, DateTime::into_timestamp_nanos);
                        (modified, candidate)
                    })
                    .collect();
                self.order_numerically(searcher, keyed, sort)
            }
            SortKey::Title => self.order_by_title(searcher, candidates, sort.order()),
        }
    }

    /// Sorts by a numeric key, then resolves runs of equal keys. Stored
    /// documents are read only for candidates that actually tie.
    ///
    /// The `modified` fast column holds whole seconds, so notes saved within
    /// the same second tie on it; those ties are resolved by the stored
    /// modification time, which keeps full precision, before falling back to
    /// path. Every other tie goes straight to path.
    fn order_numerically(
        &self,
        searcher: &Searcher,
        mut keyed: Vec<(i64, Candidate)>,
        sort: SortSpec,
    ) -> Result<Vec<Candidate>, SearchError> {
        let order = sort.order();
        keyed.sort_by(|left, right| match order {
            SortOrder::Ascending => left.0.cmp(&right.0),
            SortOrder::Descending => right.0.cmp(&left.0),
        });

        let mut ordered = Vec::with_capacity(keyed.len());
        let mut start = 0;
        while start < keyed.len() {
            let key = keyed[start].0;
            let end = keyed[start..]
                .iter()
                .position(|(other, _)| *other != key)
                .map_or(keyed.len(), |offset| start + offset);
            if end - start == 1 {
                ordered.push(keyed[start].1);
            } else {
                let mut tied = Vec::with_capacity(end - start);
                for (_, candidate) in &keyed[start..end] {
                    let stored: TantivyDocument = searcher.doc(candidate.address)?;
                    let precise = (sort.key() == SortKey::Modified).then(|| stored_date(&stored, self.fields.modified));
                    tied.push((precise, stored_text(&stored, self.fields.path), *candidate));
                }
                tied.sort_by(|left, right| {
                    let by_time = match (left.0, right.0) {
                        (Some(first), Some(second)) => match order {
                            SortOrder::Ascending => first.cmp(&second),
                            SortOrder::Descending => second.cmp(&first),
                        },
                        _ => Ordering::Equal,
                    };
                    by_time.then_with(|| left.1.cmp(&right.1))
                });
                ordered.extend(tied.into_iter().map(|(_, _, candidate)| candidate));
            }
            start = end;
        }
        Ok(ordered)
    }

    fn order_by_title(
        &self,
        searcher: &Searcher,
        candidates: Vec<Candidate>,
        order: SortOrder,
    ) -> Result<Vec<Candidate>, SearchError> {
        let mut keyed = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let stored: TantivyDocument = searcher.doc(candidate.address)?;
            let title = stored_text(&stored, self.fields.title).to_lowercase();
            keyed.push((title, stored_text(&stored, self.fields.path), candidate));
        }
        keyed.sort_by(|left, right| {
            let by_title = match order {
                SortOrder::Ascending => left.0.cmp(&right.0),
                SortOrder::Descending => right.0.cmp(&left.0),
            };
            match by_title {
                Ordering::Equal => left.1.cmp(&right.1),
                other => other,
            }
        });
        Ok(keyed.into_iter().map(|(_, _, candidate)| candidate).collect())
    }
}

/// Converts scored matches, best first, into candidates carrying their
/// relevance as a fraction of the best score, keeping those at or above the
/// threshold.
fn admit_relevant(scored: &[(f32, DocAddress)], threshold: f32) -> Vec<Candidate> {
    let Some(&(best, _)) = scored.first() else {
        return Vec::new();
    };
    scored
        .iter()
        .filter_map(|&(score, address)| {
            let relevance = if best > 0.0 {
                (score / best).clamp(0.0, 1.0)
            } else {
                1.0
            };
            (relevance >= threshold).then_some(Candidate {
                address,
                score,
                relevance,
            })
        })
        .collect()
}
