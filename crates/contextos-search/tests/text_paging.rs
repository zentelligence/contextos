mod support;

use contextos_search::{
    CANDIDATE_WINDOW, DocumentSource, IndexedDocument, IndexesText, MinRelevance, ResultControls, SearchError, SortKey,
    SortOrder, SortSpec, TantivyIndex, TextIndexConfig, TextQuery,
};
use serde_json::Map;
use support::vault_note;
use time::OffsetDateTime;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn index_at(directory: &std::path::Path) -> Result<TantivyIndex, Box<dyn std::error::Error>> {
    Ok(TantivyIndex::try_from(TextIndexConfig {
        directory: directory.join(".contextos").join("index"),
        writer_lock_wait: TextIndexConfig::DEFAULT_WRITER_LOCK_WAIT,
    })?)
}

/// A note whose file modification time is `unix_seconds`, so ordering by
/// modification time is observable.
fn note(
    vault: &tempfile::TempDir,
    relative: &str,
    content: &str,
    unix_seconds: i64,
) -> Result<IndexedDocument, Box<dyn std::error::Error>> {
    let (_roots, path) = vault_note(vault, relative, content)?;
    Ok(IndexedDocument::from(DocumentSource {
        path: &path,
        content,
        modified: OffsetDateTime::from_unix_timestamp(unix_seconds)?,
    }))
}

/// A note modified at `nanos` nanoseconds after the Unix epoch, so notes
/// saved within the same second can be told apart.
fn note_at_nanos(
    vault: &tempfile::TempDir,
    relative: &str,
    content: &str,
    nanos: i128,
) -> Result<IndexedDocument, Box<dyn std::error::Error>> {
    let (_roots, path) = vault_note(vault, relative, content)?;
    Ok(IndexedDocument::from(DocumentSource {
        path: &path,
        content,
        modified: OffsetDateTime::from_unix_timestamp_nanos(nanos)?,
    }))
}

fn query<'a>(text: &'a str, limit: usize, fields: &'a Map<String, serde_json::Value>) -> TextQuery<'a> {
    TextQuery {
        query: text,
        path_prefix: None,
        exclude_paths: &[],
        tags: &[],
        fields,
        limit,
    }
}

fn paths(hits: &[contextos_search::TextHit]) -> Vec<&str> {
    hits.iter().map(|hit| hit.path.as_str()).collect()
}

/// Five notes with clearly different keyword density, so the relevance
/// ranking has no ties.
fn ranked_vault() -> Result<(tempfile::TempDir, TantivyIndex), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    index.index(&[
        note(
            &vault,
            "notes/one.md",
            "# One\n\nwidget widget widget widget widget\n",
            100,
        )?,
        note(
            &vault,
            "notes/two.md",
            "# Two\n\nwidget widget widget widget filler\n",
            200,
        )?,
        note(
            &vault,
            "notes/three.md",
            "# Three\n\nwidget widget widget filler filler\n",
            300,
        )?,
        note(
            &vault,
            "notes/four.md",
            "# Four\n\nwidget widget filler filler filler\n",
            400,
        )?,
        note(
            &vault,
            "notes/five.md",
            "# Five\n\nwidget filler filler filler filler\n",
            500,
        )?,
    ])?;
    Ok((vault, index))
}

#[test]
fn a_page_is_the_requested_slice_of_the_ranking_with_an_exact_total() -> TestResult {
    let (_vault, index) = ranked_vault()?;
    let none = Map::new();

    let everything = index.query_page(&query("widget", 10, &none), &ResultControls::default())?;
    assert_eq!(everything.total, 5);
    assert_eq!(everything.hits.len(), 5);

    let controls = ResultControls {
        offset: 2,
        ..ResultControls::default()
    };
    let page = index.query_page(&query("widget", 2, &none), &controls)?;

    assert_eq!(page.total, 5, "total counts every match, not just the page");
    assert_eq!(paths(&page.hits), paths(&everything.hits[2..4]));
    Ok(())
}

#[test]
fn walking_every_page_yields_each_hit_exactly_once_in_ranking_order() -> TestResult {
    let (_vault, index) = ranked_vault()?;
    let none = Map::new();
    let everything = index.query_page(&query("widget", 10, &none), &ResultControls::default())?;

    let mut walked = Vec::new();
    for offset in [0, 2, 4] {
        let controls = ResultControls {
            offset,
            ..ResultControls::default()
        };
        let page = index.query_page(&query("widget", 2, &none), &controls)?;
        walked.extend(page.hits.into_iter().map(|hit| hit.path));
    }

    assert_eq!(walked, paths(&everything.hits));
    Ok(())
}

#[test]
fn an_offset_past_the_last_match_returns_no_hits_but_the_exact_total() -> TestResult {
    let (_vault, index) = ranked_vault()?;
    let none = Map::new();
    let controls = ResultControls {
        offset: 50,
        ..ResultControls::default()
    };

    let page = index.query_page(&query("widget", 10, &none), &controls)?;

    assert!(page.hits.is_empty(), "`page.hits` should be empty, got {:?}", page.hits);
    assert_eq!(page.total, 5);
    Ok(())
}

#[test]
fn a_zero_limit_returns_the_exact_total_without_hits() -> TestResult {
    let (_vault, index) = ranked_vault()?;
    let none = Map::new();

    let page = index.query_page(&query("widget", 0, &none), &ResultControls::default())?;

    assert!(page.hits.is_empty(), "`page.hits` should be empty, got {:?}", page.hits);
    assert_eq!(page.total, 5);
    Ok(())
}

#[test]
fn relevance_is_the_fraction_of_the_best_hit_with_the_best_hit_at_one() -> TestResult {
    let (_vault, index) = ranked_vault()?;
    let none = Map::new();

    let page = index.query_page(&query("widget", 10, &none), &ResultControls::default())?;

    let best = page.hits.first().ok_or("expected hits")?;
    assert!((best.relevance - 1.0).abs() < f32::EPSILON);
    for hit in &page.hits {
        assert!((0.0..=1.0).contains(&hit.relevance));
        assert!((hit.relevance - hit.score / best.score).abs() < 1e-6);
    }
    Ok(())
}

#[test]
fn minimum_relevance_excludes_weaker_hits_from_both_the_page_and_the_total() -> TestResult {
    let (_vault, index) = ranked_vault()?;
    let none = Map::new();
    let everything = index.query_page(&query("widget", 10, &none), &ResultControls::default())?;
    let third = everything.hits.get(2).ok_or("expected a third hit")?;

    let controls = ResultControls {
        min_relevance: MinRelevance::try_from(f64::from(third.relevance))?,
        ..ResultControls::default()
    };
    let page = index.query_page(&query("widget", 10, &none), &controls)?;

    assert_eq!(page.total, 3, "the boundary value itself is included");
    assert_eq!(paths(&page.hits), paths(&everything.hits[..3]));
    Ok(())
}

#[test]
fn a_minimum_relevance_just_above_a_hit_excludes_it() -> TestResult {
    let (_vault, index) = ranked_vault()?;
    let none = Map::new();
    let everything = index.query_page(&query("widget", 10, &none), &ResultControls::default())?;
    let third = everything.hits.get(2).ok_or("expected a third hit")?;

    let controls = ResultControls {
        min_relevance: MinRelevance::try_from(f64::from(third.relevance) + 1e-4)?,
        ..ResultControls::default()
    };
    let page = index.query_page(&query("widget", 10, &none), &controls)?;

    assert_eq!(page.total, 2);
    Ok(())
}

#[test]
fn sorting_by_title_orders_alphabetically_and_a_descending_order_reverses_it() -> TestResult {
    let (_vault, index) = ranked_vault()?;
    let none = Map::new();

    let ascending = ResultControls {
        sort: SortSpec::from(SortKey::Title),
        ..ResultControls::default()
    };
    let page = index.query_page(&query("widget", 10, &none), &ascending)?;
    let titles: Vec<&str> = page.hits.iter().map(|hit| hit.title.as_str()).collect();
    assert_eq!(titles, ["Five", "Four", "One", "Three", "Two"]);

    let descending = ResultControls {
        sort: SortSpec::from((SortKey::Title, Some(SortOrder::Descending))),
        ..ResultControls::default()
    };
    let page = index.query_page(&query("widget", 10, &none), &descending)?;
    let titles: Vec<&str> = page.hits.iter().map(|hit| hit.title.as_str()).collect();
    assert_eq!(titles, ["Two", "Three", "One", "Four", "Five"]);
    Ok(())
}

#[test]
fn sorting_by_modified_puts_the_most_recent_first_by_default() -> TestResult {
    let (_vault, index) = ranked_vault()?;
    let none = Map::new();
    let controls = ResultControls {
        sort: SortSpec::from(SortKey::Modified),
        ..ResultControls::default()
    };

    let page = index.query_page(&query("widget", 10, &none), &controls)?;

    assert_eq!(
        paths(&page.hits),
        [
            "notes/five.md",
            "notes/four.md",
            "notes/three.md",
            "notes/two.md",
            "notes/one.md"
        ]
    );
    Ok(())
}

#[test]
fn ties_on_the_sort_key_are_broken_by_ascending_path() -> TestResult {
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    index.index(&[
        note(&vault, "b/same.md", "# Same\n\nshared words here\n", 100)?,
        note(&vault, "a/same.md", "# Same\n\nshared words here\n", 100)?,
        note(&vault, "c/same.md", "# Same\n\nshared words here\n", 100)?,
    ])?;
    let none = Map::new();

    for key in [SortKey::Relevance, SortKey::Title, SortKey::Modified] {
        let controls = ResultControls {
            sort: SortSpec::from(key),
            ..ResultControls::default()
        };
        let page = index.query_page(&query("shared", 10, &none), &controls)?;
        assert_eq!(
            paths(&page.hits),
            ["a/same.md", "b/same.md", "c/same.md"],
            "tie-break under {key:?}"
        );
    }
    Ok(())
}

#[test]
fn the_note_type_filter_keeps_only_notes_with_that_frontmatter_type() -> TestResult {
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    index.index(&[
        note(&vault, "tasks/a.md", "---\ntype: task\n---\n\nshared words\n", 100)?,
        note(&vault, "tasks/b.md", "---\ntype: task\n---\n\nshared words\n", 100)?,
        note(&vault, "notes/c.md", "---\ntype: note\n---\n\nshared words\n", 100)?,
        note(&vault, "notes/d.md", "shared words with no frontmatter\n", 100)?,
    ])?;
    let none = Map::new();
    let controls = ResultControls {
        note_type: Some("task"),
        ..ResultControls::default()
    };

    let page = index.query_page(&query("shared", 10, &none), &controls)?;

    assert_eq!(page.total, 2);
    assert_eq!(paths(&page.hits), ["tasks/a.md", "tasks/b.md"]);
    Ok(())
}

/// Two hundred and fifty matches: docs 000 to 049 mention the term once and
/// docs 050 to 249 five times, so the 200 most relevant are exactly docs 050
/// onwards and doc 000 is outside the relevance window.
fn crowded_vault() -> Result<(tempfile::TempDir, TantivyIndex), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    let mut documents = Vec::new();
    for number in 0..250 {
        let repeats = if number < 50 { 1 } else { 5 };
        let body = "gadget ".repeat(repeats);
        documents.push(note(
            &vault,
            &format!("crowd/doc-{number:03}.md"),
            &format!("# Doc {number:03}\n\n{body}\n"),
            100,
        )?);
    }
    index.index(&documents)?;
    Ok((vault, index))
}

#[test]
fn the_total_counts_every_match_even_beyond_the_reachable_window() -> TestResult {
    let (_vault, index) = crowded_vault()?;
    let none = Map::new();

    let page = index.query_page(&query("gadget", 10, &none), &ResultControls::default())?;

    assert_eq!(page.total, 250);
    assert_eq!(page.hits.len(), 10);
    Ok(())
}

#[test]
fn a_page_ending_beyond_the_window_is_rejected_before_any_search() -> TestResult {
    let (_vault, index) = crowded_vault()?;
    let none = Map::new();
    let controls = ResultControls {
        offset: CANDIDATE_WINDOW - 9,
        ..ResultControls::default()
    };

    let error = index.query_page(&query("gadget", 10, &none), &controls);

    assert!(matches!(error, Err(SearchError::PageBeyondWindow { .. })));
    Ok(())
}

#[test]
fn title_sorting_orders_the_whole_match_set_not_just_the_most_relevant_window() -> TestResult {
    let (_vault, index) = crowded_vault()?;
    let none = Map::new();
    let controls = ResultControls {
        sort: SortSpec::from(SortKey::Title),
        ..ResultControls::default()
    };

    let page = index.query_page(&query("gadget", 3, &none), &controls)?;

    let titles: Vec<&str> = page.hits.iter().map(|hit| hit.title.as_str()).collect();
    assert_eq!(titles, ["Doc 000", "Doc 001", "Doc 002"]);
    assert_eq!(page.total, 250);
    Ok(())
}

#[test]
fn an_unpaged_query_still_returns_the_first_page_in_relevance_order() -> TestResult {
    let (_vault, index) = ranked_vault()?;
    let none = Map::new();

    let hits = index.query(&query("widget", 3, &none))?;

    assert_eq!(paths(&hits), ["notes/one.md", "notes/two.md", "notes/three.md"]);
    Ok(())
}

#[test]
fn sorting_by_modified_distinguishes_notes_saved_within_the_same_second() -> TestResult {
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    // All three are modified during second 100. Alphabetical order (the
    // tie-break) is the reverse of recency, so a sort that only saw whole
    // seconds would return a, b, c.
    index.index(&[
        note_at_nanos(&vault, "a.md", "# A\n\nshared words\n", 100_100_000_000)?,
        note_at_nanos(&vault, "b.md", "# B\n\nshared words\n", 100_500_000_000)?,
        note_at_nanos(&vault, "c.md", "# C\n\nshared words\n", 100_900_000_000)?,
    ])?;
    let none = Map::new();

    let recent_first = ResultControls {
        sort: SortSpec::from(SortKey::Modified),
        ..ResultControls::default()
    };
    let page = index.query_page(&query("shared", 10, &none), &recent_first)?;
    assert_eq!(paths(&page.hits), ["c.md", "b.md", "a.md"]);

    let oldest_first = ResultControls {
        sort: SortSpec::from((SortKey::Modified, Some(SortOrder::Ascending))),
        ..ResultControls::default()
    };
    let page = index.query_page(&query("shared", 10, &none), &oldest_first)?;
    assert_eq!(paths(&page.hits), ["a.md", "b.md", "c.md"]);
    Ok(())
}

#[test]
fn a_note_whose_type_is_not_a_string_never_matches_a_note_type_filter() -> TestResult {
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    index.index(&[
        note(&vault, "numeric.md", "---\ntype: 3\n---\n\nshared words\n", 100)?,
        note(&vault, "task.md", "---\ntype: task\n---\n\nshared words\n", 100)?,
    ])?;
    let none = Map::new();

    for (filter, expected) in [("3", 0_usize), ("task", 1)] {
        let controls = ResultControls {
            note_type: Some(filter),
            ..ResultControls::default()
        };
        let page = index.query_page(&query("shared", 10, &none), &controls)?;
        assert_eq!(page.total, expected, "note_type = {filter}");
    }
    Ok(())
}

#[test]
fn walking_every_page_under_a_title_sort_yields_each_hit_exactly_once() -> TestResult {
    let (_vault, index) = ranked_vault()?;
    let none = Map::new();

    let mut walked = Vec::new();
    for offset in [0, 2, 4] {
        let controls = ResultControls {
            offset,
            sort: SortSpec::from(SortKey::Title),
            ..ResultControls::default()
        };
        let page = index.query_page(&query("widget", 2, &none), &controls)?;
        assert_eq!(page.total, 5);
        walked.extend(page.hits.into_iter().map(|hit| hit.title));
    }

    assert_eq!(walked, ["Five", "Four", "One", "Three", "Two"]);
    Ok(())
}
