mod support;

use std::fs;
use std::time::{Duration, SystemTime};

use contextos_core::VaultRootId;
use contextos_search::{
    CANDIDATE_WINDOW, FakeEmbedder, GraphBackend, MinRelevance, RebuildTarget, ResultControls, SearchError,
    SemanticConfig, SemanticQuery, SortKey, SortOrder, SortSpec, TextQuery, VaultSearchConfig, VaultSearchService,
};
use serde_json::Map;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn vault_dir() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    Ok(tempfile::Builder::new().prefix("vault").tempdir()?)
}

fn service_at(
    vault: &tempfile::TempDir,
    text_enabled: bool,
    semantic_enabled: bool,
) -> Result<VaultSearchService, Box<dyn std::error::Error>> {
    let semantic = semantic_enabled.then(|| SemanticConfig {
        embedder: Box::new(FakeEmbedder::default()),
        vector_store_path: vault.path().join(".contextos/vectors.db"),
    });
    Ok(VaultSearchService::try_from(VaultSearchConfig {
        root_id: VaultRootId::try_from(0_usize)?,
        root: vault.path().to_path_buf(),
        excludes: vec![],
        link_excludes: Vec::new(),
        link_ignore_targets: Vec::new(),
        hidden: Vec::new(),
        state_directory: vault.path().join(".contextos"),
        text_enabled,
        graph_enabled: false,
        graph_backend: GraphBackend::Fjall,
        semantic,
    })?)
}

fn write(vault: &tempfile::TempDir, relative: &str, content: &str, age_seconds: u64) -> TestResult {
    let absolute = vault.path().join(relative);
    if let Some(parent) = absolute.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&absolute, content)?;
    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_770_000_000_u64.saturating_sub(age_seconds));
    fs::File::options()
        .write(true)
        .open(&absolute)?
        .set_modified(modified)?;
    Ok(())
}

/// Three notes, oldest to newest: alpha (task), bravo (note), charlie (task).
fn topic_vault() -> Result<(tempfile::TempDir, VaultSearchService), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    write(
        &vault,
        "topics/alpha.md",
        "---\ntitle: Alpha\ntype: task\ndescription: About alpha\n---\n\nAlpha content paragraph.\n",
        3000,
    )?;
    write(
        &vault,
        "topics/bravo.md",
        "---\ntitle: Bravo\ntype: note\ndescription: About bravo\n---\n\nBravo content paragraph.\n",
        2000,
    )?;
    write(
        &vault,
        "topics/charlie.md",
        "---\ntitle: Charlie\ntype: task\ndescription: About charlie\n---\n\nCharlie content paragraph.\n",
        1000,
    )?;
    let service = service_at(&vault, true, true)?;
    service.rebuild(RebuildTarget::Semantic)?;
    Ok((vault, service))
}

fn semantic_query(limit: usize) -> SemanticQuery<'static> {
    SemanticQuery {
        query: "content paragraph",
        limit,
        path_prefix: None,
        exclude_paths: &[],
    }
}

fn semantic_paths(results: &contextos_search::SemanticResults) -> Vec<&str> {
    results
        .hits
        .iter()
        .map(|described| described.hit.path.as_str())
        .collect()
}

fn titles(results: &contextos_search::SemanticResults) -> Vec<&str> {
    results
        .hits
        .iter()
        .map(|described| described.metadata.title.as_deref().unwrap_or(""))
        .collect()
}

#[test]
fn a_semantic_page_is_the_requested_slice_of_the_ranked_pool_with_a_total() -> TestResult {
    let (_vault, service) = topic_vault()?;
    let everything = service.query_semantic_page(&semantic_query(10), &ResultControls::default())?;
    assert_eq!(everything.total, 3);
    assert!(!everything.total_capped);

    let controls = ResultControls {
        offset: 1,
        ..ResultControls::default()
    };
    let page = service.query_semantic_page(&semantic_query(1), &controls)?;

    assert_eq!(page.total, 3);
    assert_eq!(semantic_paths(&page), semantic_paths(&everything)[1..2]);
    Ok(())
}

#[test]
fn every_semantic_hit_carries_its_notes_metadata_and_a_relevance_fraction() -> TestResult {
    let (_vault, service) = topic_vault()?;

    let results = service.query_semantic_page(&semantic_query(10), &ResultControls::default())?;

    assert_eq!(results.hits.len(), 3, "every note should be a hit");
    for described in &results.hits {
        assert!((0.0..=1.0).contains(&described.hit.relevance));
        assert!(described.metadata.description.is_some(), "{}", described.hit.path);
        assert!(described.metadata.modified.is_some());
    }
    Ok(())
}

#[test]
fn a_semantic_minimum_relevance_excludes_weaker_hits_from_the_page_and_total() -> TestResult {
    let (_vault, service) = topic_vault()?;
    let everything = service.query_semantic_page(&semantic_query(10), &ResultControls::default())?;
    let best = everything.hits.first().ok_or("expected hits")?.hit.relevance;
    let expected = everything
        .hits
        .iter()
        .filter(|described| described.hit.relevance >= best)
        .count();

    let controls = ResultControls {
        min_relevance: MinRelevance::try_from(f64::from(best))?,
        ..ResultControls::default()
    };
    let page = service.query_semantic_page(&semantic_query(10), &controls)?;

    assert_eq!(page.total, expected);
    assert_eq!(page.hits.len(), expected);
    Ok(())
}

#[test]
fn the_semantic_note_type_filter_keeps_only_matching_notes() -> TestResult {
    let (_vault, service) = topic_vault()?;
    let controls = ResultControls {
        note_type: Some("task"),
        ..ResultControls::default()
    };

    let results = service.query_semantic_page(&semantic_query(10), &controls)?;

    assert_eq!(results.total, 2);
    let mut found = semantic_paths(&results);
    found.sort_unstable();
    assert_eq!(found, ["topics/alpha.md", "topics/charlie.md"]);
    Ok(())
}

#[test]
fn semantic_results_sort_by_title_and_by_modification_time() -> TestResult {
    let (_vault, service) = topic_vault()?;

    let by_title = ResultControls {
        sort: SortSpec::from(SortKey::Title),
        ..ResultControls::default()
    };
    let results = service.query_semantic_page(&semantic_query(10), &by_title)?;
    assert_eq!(titles(&results), ["Alpha", "Bravo", "Charlie"]);

    let by_title_descending = ResultControls {
        sort: SortSpec::from((SortKey::Title, Some(SortOrder::Descending))),
        ..ResultControls::default()
    };
    let results = service.query_semantic_page(&semantic_query(10), &by_title_descending)?;
    assert_eq!(titles(&results), ["Charlie", "Bravo", "Alpha"]);

    let by_modified = ResultControls {
        sort: SortSpec::from(SortKey::Modified),
        ..ResultControls::default()
    };
    let results = service.query_semantic_page(&semantic_query(10), &by_modified)?;
    assert_eq!(titles(&results), ["Charlie", "Bravo", "Alpha"], "most recent first");
    Ok(())
}

#[test]
fn a_semantic_pool_that_fills_the_window_reports_a_capped_total() -> TestResult {
    let vault = vault_dir()?;
    for number in 0..(CANDIDATE_WINDOW + 10) {
        write(
            &vault,
            &format!("crowd/note-{number:03}.md"),
            &format!("Distinct paragraph number {number} with words {number}.\n"),
            0,
        )?;
    }
    let service = service_at(&vault, false, true)?;
    service.rebuild(RebuildTarget::Semantic)?;

    let results = service.query_semantic_page(&semantic_query(10), &ResultControls::default())?;

    assert_eq!(results.total, CANDIDATE_WINDOW, "the pool is the window, not the vault");
    assert!(results.total_capped);
    Ok(())
}

#[test]
fn a_semantic_hit_whose_source_changed_is_dropped_from_the_page_and_the_total() -> TestResult {
    let (vault, service) = topic_vault()?;
    fs::write(vault.path().join("topics/bravo.md"), "# Bravo\n")?;

    let results = service.query_semantic_page(&semantic_query(10), &ResultControls::default())?;

    assert_eq!(results.total, 2);
    assert!(!semantic_paths(&results).contains(&"topics/bravo.md"));
    Ok(())
}

#[test]
fn a_semantic_page_ending_beyond_the_window_is_rejected() -> TestResult {
    let (_vault, service) = topic_vault()?;
    let controls = ResultControls {
        offset: CANDIDATE_WINDOW - 9,
        ..ResultControls::default()
    };

    let error = service.query_semantic_page(&semantic_query(10), &controls);

    assert!(matches!(error, Err(SearchError::PageBeyondWindow { .. })));
    Ok(())
}

#[test]
fn a_paged_semantic_query_without_semantic_search_fails_with_the_stable_disabled_error() -> TestResult {
    let vault = vault_dir()?;
    let service = service_at(&vault, true, false)?;

    let error = service.query_semantic_page(&semantic_query(10), &ResultControls::default());

    assert!(matches!(error, Err(SearchError::SemanticUnavailable)));
    Ok(())
}

#[test]
fn a_text_page_carries_metadata_an_exact_total_and_index_freshness() -> TestResult {
    let (_vault, service) = topic_vault()?;
    let none = Map::new();
    let request = TextQuery {
        query: "content",
        path_prefix: None,
        exclude_paths: &[],
        tags: &[],
        fields: &none,
        limit: 2,
    };

    let results = service.query_text_page(&request, &ResultControls::default())?;

    assert_eq!(results.total, 3);
    assert_eq!(results.hits.len(), 2);
    for described in &results.hits {
        assert!(described.metadata.description.is_some(), "{}", described.hit.path);
        assert!(described.hit.relevance > 0.0);
    }
    Ok(())
}

#[test]
fn a_text_page_ending_beyond_the_window_is_rejected_at_the_service() -> TestResult {
    let (_vault, service) = topic_vault()?;
    let none = Map::new();
    let request = TextQuery {
        query: "content",
        path_prefix: None,
        exclude_paths: &[],
        tags: &[],
        fields: &none,
        limit: 10,
    };
    let controls = ResultControls {
        offset: CANDIDATE_WINDOW,
        ..ResultControls::default()
    };

    let error = service.query_text_page(&request, &controls);

    assert!(matches!(error, Err(SearchError::PageBeyondWindow { .. })));
    Ok(())
}

/// Replaces `relative` with a symlink to a file outside the vault, the way an
/// indexed note could later be swapped for a link that escapes the root.
#[cfg(unix)]
fn replace_with_escaping_symlink(vault: &tempfile::TempDir, relative: &str) -> TestResult {
    let outside = tempfile::tempdir()?;
    let target = outside.path().join("secret.md");
    fs::write(&target, "---\ntitle: Secret\n---\n\nOutside the vault.\n")?;
    let link = vault.path().join(relative);
    fs::remove_file(&link)?;
    std::os::unix::fs::symlink(&target, &link)?;
    // Keep the target alive for the rest of the test.
    std::mem::forget(outside);
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_semantic_hit_that_now_escapes_the_vault_is_skipped_rather_than_failing_the_page() -> TestResult {
    let (vault, service) = topic_vault()?;
    replace_with_escaping_symlink(&vault, "topics/bravo.md")?;

    let results = service.query_semantic_page(&semantic_query(10), &ResultControls::default())?;

    assert_eq!(results.total, 2);
    assert!(!semantic_paths(&results).contains(&"topics/bravo.md"));
    Ok(())
}

#[test]
fn a_note_with_several_chunks_appears_once_per_chunk_in_semantic_results() -> TestResult {
    let vault = vault_dir()?;
    let sections = (0..8)
        .map(|number| format!("# Section {number}\n\nParagraph text for section {number}.\n\n"))
        .collect::<Vec<_>>()
        .concat();
    write(&vault, "long.md", &sections, 0)?;
    let service = service_at(&vault, false, true)?;
    service.rebuild(RebuildTarget::Semantic)?;

    let results = service.query_semantic_page(&semantic_query(10), &ResultControls::default())?;

    assert!(results.total >= 2, "the note must yield several chunks");
    assert!(results.hits.iter().all(|described| described.hit.path == "long.md"));
    Ok(())
}

fn write_crowd(vault: &tempfile::TempDir, count: usize) -> TestResult {
    for number in 0..count {
        write(
            vault,
            &format!("crowd/note-{number:03}.md"),
            &format!("Distinct paragraph number {number} with words {number}.\n"),
            0,
        )?;
    }
    Ok(())
}

#[test]
fn a_pool_holding_exactly_the_window_is_not_reported_as_capped_but_one_more_match_is() -> TestResult {
    for (notes, expected_capped) in [(CANDIDATE_WINDOW, false), (CANDIDATE_WINDOW + 1, true)] {
        let vault = vault_dir()?;
        write_crowd(&vault, notes)?;
        let service = service_at(&vault, false, true)?;
        service.rebuild(RebuildTarget::Semantic)?;

        let results = service.query_semantic_page(&semantic_query(10), &ResultControls::default())?;

        assert_eq!(results.total, CANDIDATE_WINDOW, "{notes} notes");
        assert_eq!(results.total_capped, expected_capped, "{notes} notes");
    }
    Ok(())
}

#[test]
fn walking_every_semantic_page_yields_each_hit_once_in_the_requested_order() -> TestResult {
    let (_vault, service) = topic_vault()?;
    for (sort, expected) in [
        (SortKey::Title, ["Alpha", "Bravo", "Charlie"]),
        (SortKey::Modified, ["Charlie", "Bravo", "Alpha"]),
    ] {
        let mut walked = Vec::new();
        for offset in 0..3 {
            let controls = ResultControls {
                offset,
                sort: SortSpec::from(sort),
                ..ResultControls::default()
            };
            let page = service.query_semantic_page(&semantic_query(1), &controls)?;
            assert_eq!(page.total, 3);
            walked.extend(titles(&page).into_iter().map(str::to_owned));
        }
        assert_eq!(walked, expected, "{sort:?}");
    }
    Ok(())
}

#[test]
fn a_zero_limit_semantic_query_returns_only_the_total() -> TestResult {
    let (_vault, service) = topic_vault()?;

    let results = service.query_semantic_page(&semantic_query(0), &ResultControls::default())?;

    assert!(
        results.hits.is_empty(),
        "`results.hits` should be empty, got {:?}",
        results.hits
    );
    assert_eq!(results.total, 3);
    Ok(())
}
