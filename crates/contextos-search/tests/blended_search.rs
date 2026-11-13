mod support;

use std::fs;
use std::time::{Duration, SystemTime};

use contextos_core::VaultRootId;
use contextos_search::{
    BlendedQuery, BlendedResults, CANDIDATE_WINDOW, Chunk, EmbedsText, FakeEmbedder, GraphBackend, MatchMode,
    MinRelevance, RebuildTarget, ResultControls, SearchError, SearchMode, SemanticConfig, SemanticQuery, SortKey,
    SortSpec, TextQuery, VaultSearchConfig, VaultSearchService, fuse_by_rank,
};
use serde_json::Map;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A provider that is always unreachable, to prove blended search degrades
/// rather than failing when the semantic side does.
struct UnreachableEmbedder;

impl EmbedsText for UnreachableEmbedder {
    fn embed(&self, _chunks: &[Chunk]) -> Result<Vec<Vec<f32>>, SearchError> {
        Err(SearchError::EmbeddingTimeout {
            endpoint: "http://embeddings.invalid".to_owned(),
            timeout_ms: 1,
        })
    }

    fn dimension(&self) -> Option<usize> {
        Some(16)
    }
}

#[derive(Clone, Copy)]
enum Semantic {
    Off,
    Working,
    Unreachable,
}

fn vault_dir() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    Ok(tempfile::Builder::new().prefix("vault").tempdir()?)
}

fn service_at(
    vault: &tempfile::TempDir,
    text_enabled: bool,
    semantic: Semantic,
) -> Result<VaultSearchService, Box<dyn std::error::Error>> {
    let embedder: Option<Box<dyn EmbedsText>> = match semantic {
        Semantic::Off => None,
        Semantic::Working => Some(Box::new(FakeEmbedder::default())),
        Semantic::Unreachable => Some(Box::new(UnreachableEmbedder)),
    };
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
        semantic: embedder.map(|embedder| SemanticConfig {
            embedder,
            vector_store_path: vault.path().join(".contextos/vectors.db"),
        }),
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
fn write_topics(vault: &tempfile::TempDir) -> TestResult {
    write(
        vault,
        "topics/alpha.md",
        "---\ntitle: Alpha\ntype: task\ndescription: About alpha\n---\n\nAlpha content paragraph.\n",
        3000,
    )?;
    write(
        vault,
        "topics/bravo.md",
        "---\ntitle: Bravo\ntype: note\ndescription: About bravo\n---\n\nBravo content paragraph.\n",
        2000,
    )?;
    write(
        vault,
        "topics/charlie.md",
        "---\ntitle: Charlie\ntype: task\ndescription: About charlie\n---\n\nCharlie content paragraph.\n",
        1000,
    )?;
    Ok(())
}

fn topic_service(
    text_enabled: bool,
    semantic: Semantic,
) -> Result<(tempfile::TempDir, VaultSearchService), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    write_topics(&vault)?;
    let service = service_at(&vault, text_enabled, semantic)?;
    if matches!(semantic, Semantic::Working) {
        service.rebuild(RebuildTarget::Semantic)?;
    }
    Ok((vault, service))
}

const NO_EXCLUDES: &[String] = &[];

fn blended(query: &str, limit: usize, mode: SearchMode) -> BlendedQuery<'_> {
    BlendedQuery {
        query,
        limit,
        path_prefix: None,
        exclude_paths: NO_EXCLUDES,
        mode,
    }
}

fn paths(results: &BlendedResults) -> Vec<&str> {
    results.hits.iter().map(|hit| hit.path.as_str()).collect()
}

fn text_only_paths(service: &VaultSearchService, query: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let none = Map::new();
    let results = service.query_text_page(
        &TextQuery {
            query,
            path_prefix: None,
            exclude_paths: &[],
            tags: &[],
            fields: &none,
            limit: 100,
        },
        &ResultControls::default(),
    )?;
    Ok(results.hits.into_iter().map(|described| described.hit.path).collect())
}

fn semantic_only_paths(service: &VaultSearchService, query: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let results = service.query_semantic_page(
        &SemanticQuery {
            query,
            limit: 100,
            path_prefix: None,
            exclude_paths: &[],
        },
        &ResultControls::default(),
    )?;
    Ok(results.hits.into_iter().map(|described| described.hit.path).collect())
}

fn approx(left: f32, right: f32) -> bool {
    (left - right).abs() < 1e-5
}

#[test]
fn blended_results_match_fusing_the_two_single_mode_rankings_independently() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Working)?;
    let text = text_only_paths(&service, "content")?;
    let semantic = semantic_only_paths(&service, "content")?;
    let expected = fuse_by_rank(&[&text, &semantic]);

    let results = service.query_blended(&blended("content", 10, SearchMode::Blended), &ResultControls::default())?;

    assert_eq!(results.total, 3);
    assert!(
        results.warnings.is_empty(),
        "`results.warnings` should be empty, got {:?}",
        results.warnings
    );
    assert_eq!(
        paths(&results),
        expected.iter().map(|file| file.path.as_str()).collect::<Vec<_>>()
    );
    for (hit, fused) in results.hits.iter().zip(&expected) {
        assert!(approx(hit.relevance, fused.relevance), "{}", hit.path);
    }
    Ok(())
}

#[test]
fn a_file_found_by_both_indexes_says_so_text_first() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Working)?;

    let results = service.query_blended(&blended("content", 10, SearchMode::Blended), &ResultControls::default())?;

    assert_eq!(results.hits.len(), 3);
    for hit in &results.hits {
        assert_eq!(hit.matched_by, [MatchMode::Text, MatchMode::Semantic], "{}", hit.path);
        assert!(!hit.snippet.is_empty(), "`hit.snippet` should not be empty");
        assert!(hit.metadata.description.is_some());
        assert!(hit.relevance > 0.0 && hit.relevance <= 1.0);
    }
    Ok(())
}

#[test]
fn a_file_only_the_semantic_index_finds_is_reported_with_its_title_and_half_relevance_at_best() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Working)?;

    let results = service.query_blended(
        &blended("zzzunmatchedzzz", 10, SearchMode::Blended),
        &ResultControls::default(),
    )?;

    assert_eq!(results.total, 3, "every note is a semantic candidate");
    let best = results.hits.first().ok_or("expected hits")?;
    assert!(
        approx(best.relevance, 0.5),
        "rank 1 in one of two modes scores one half"
    );
    for hit in &results.hits {
        assert_eq!(hit.matched_by, [MatchMode::Semantic]);
        assert!(["Alpha", "Bravo", "Charlie"].contains(&hit.title.as_str()));
    }
    Ok(())
}

#[test]
fn text_mode_returns_the_text_ranking_with_exact_totals_and_text_relevance() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Working)?;
    let text = text_only_paths(&service, "content")?;

    let results = service.query_blended(&blended("content", 10, SearchMode::Text), &ResultControls::default())?;

    assert_eq!(paths(&results), text.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(!results.total_capped);
    assert!(results.freshness.is_some());
    let best = results.hits.first().ok_or("expected hits")?;
    assert!(
        approx(best.relevance, 1.0),
        "text relevance is a fraction of the best hit"
    );
    assert!(results.hits.iter().all(|hit| hit.matched_by == [MatchMode::Text]));
    Ok(())
}

#[test]
fn semantic_mode_returns_one_hit_per_file_ranked_by_similarity() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Working)?;
    let semantic = semantic_only_paths(&service, "content")?;

    let results = service.query_blended(
        &blended("content", 10, SearchMode::Semantic),
        &ResultControls::default(),
    )?;

    assert_eq!(paths(&results), semantic.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(results.freshness.is_none(), "the text index was not consulted");
    assert!(results.hits.iter().all(|hit| hit.matched_by == [MatchMode::Semantic]));
    Ok(())
}

#[test]
fn blended_mode_degrades_to_text_with_a_typed_warning_when_semantic_search_is_off() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Off)?;
    let text = text_only_paths(&service, "content")?;

    let results = service.query_blended(&blended("content", 10, SearchMode::Blended), &ResultControls::default())?;

    assert_eq!(paths(&results), text.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(results.warnings.len(), 1);
    assert_eq!(results.warnings[0].code, "index/disabled");
    Ok(())
}

#[test]
fn blended_mode_degrades_to_text_with_the_providers_own_code_when_the_provider_fails() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Unreachable)?;
    let text = text_only_paths(&service, "content")?;

    let results = service.query_blended(&blended("content", 10, SearchMode::Blended), &ResultControls::default())?;

    assert_eq!(paths(&results), text.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(results.warnings.len(), 1);
    assert_eq!(results.warnings[0].code, "embedding/timeout");
    Ok(())
}

#[test]
fn blended_mode_degrades_to_semantic_with_a_typed_warning_when_the_text_index_is_off() -> TestResult {
    let (_vault, service) = topic_service(false, Semantic::Working)?;
    let semantic = semantic_only_paths(&service, "content")?;

    let results = service.query_blended(&blended("content", 10, SearchMode::Blended), &ResultControls::default())?;

    assert_eq!(paths(&results), semantic.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(results.warnings.len(), 1);
    assert_eq!(results.warnings[0].code, "index/disabled");
    assert!(results.freshness.is_none());
    Ok(())
}

#[test]
fn a_single_index_mode_fails_with_that_indexs_stable_error_instead_of_degrading() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Off)?;
    let error = service.query_blended(
        &blended("content", 10, SearchMode::Semantic),
        &ResultControls::default(),
    );
    assert!(matches!(error, Err(SearchError::SemanticUnavailable)));

    let (_vault, service) = topic_service(false, Semantic::Working)?;
    let error = service.query_blended(&blended("content", 10, SearchMode::Text), &ResultControls::default());
    assert!(matches!(error, Err(SearchError::TextDisabled)));

    let (_vault, service) = topic_service(true, Semantic::Unreachable)?;
    let error = service.query_blended(
        &blended("content", 10, SearchMode::Semantic),
        &ResultControls::default(),
    );
    assert!(matches!(error, Err(SearchError::EmbeddingTimeout { .. })));
    Ok(())
}

#[test]
fn with_neither_index_available_the_call_fails_with_the_stable_disabled_code() -> TestResult {
    let (_vault, service) = topic_service(false, Semantic::Off)?;

    let error = service.query_blended(&blended("content", 10, SearchMode::Blended), &ResultControls::default());

    let error = error.err().ok_or("expected an error")?;
    assert_eq!(error.code(), "index/disabled");
    Ok(())
}

#[test]
fn an_unparsable_text_query_degrades_to_semantic_results_with_a_warning_in_blended_mode() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Working)?;

    // A colon means a field search to the text index, and this one has no
    // field name or terms, so it does not parse.
    let results = service.query_blended(&blended("title:(", 10, SearchMode::Blended), &ResultControls::default())?;

    assert_eq!(results.total, 3, "semantic search can still answer");
    assert_eq!(results.warnings.len(), 1);
    assert_eq!(results.warnings[0].code, "index/invalid-query");
    for hit in &results.hits {
        assert_eq!(hit.matched_by, [MatchMode::Semantic]);
    }
    Ok(())
}

#[test]
fn text_mode_rejects_an_unparsable_query_while_semantic_mode_never_reads_full_text_syntax() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Working)?;

    let text = service.query_blended(&blended("title:(", 10, SearchMode::Text), &ResultControls::default());
    assert!(matches!(text, Err(SearchError::InvalidQuery { .. })));

    let semantic = service.query_blended(
        &blended("title:(", 10, SearchMode::Semantic),
        &ResultControls::default(),
    )?;
    assert_eq!(semantic.total, 3);
    assert!(
        semantic.warnings.is_empty(),
        "`semantic.warnings` should be empty, got {:?}",
        semantic.warnings
    );
    Ok(())
}

#[test]
fn an_unparsable_query_fails_with_the_parse_error_when_semantic_search_cannot_answer_instead() -> TestResult {
    for semantic in [Semantic::Off, Semantic::Unreachable] {
        let (_vault, service) = topic_service(true, semantic)?;

        let error = service.query_blended(&blended("title:(", 10, SearchMode::Blended), &ResultControls::default());

        assert!(matches!(error, Err(SearchError::InvalidQuery { .. })));
    }
    Ok(())
}

#[test]
fn degraded_blended_results_carry_rank_fusion_relevance_over_the_index_that_ran() -> TestResult {
    for (text_enabled, semantic) in [
        (true, Semantic::Off),
        (false, Semantic::Working),
        (true, Semantic::Unreachable),
    ] {
        let (_vault, service) = topic_service(text_enabled, semantic)?;

        let results =
            service.query_blended(&blended("content", 10, SearchMode::Blended), &ResultControls::default())?;

        assert_eq!(results.hits.len(), 3);
        for (index, hit) in results.hits.iter().enumerate() {
            let rank = f32::from(u16::try_from(index + 1)?);
            assert!(
                approx(hit.relevance, 61.0 / (60.0 + rank)),
                "rank {rank} scored {} with text={text_enabled}",
                hit.relevance
            );
        }
    }
    Ok(())
}

#[test]
fn with_the_text_index_off_and_the_provider_failing_the_call_fails_with_the_generic_disabled_code() -> TestResult {
    let (_vault, service) = topic_service(false, Semantic::Unreachable)?;

    let error = service
        .query_blended(&blended("content", 10, SearchMode::Blended), &ResultControls::default())
        .err()
        .ok_or("expected an error")?;

    assert_eq!(
        error.code(),
        "index/disabled",
        "one generic code for a non-technical operator"
    );
    assert!(
        error.to_string().contains("embedding/timeout"),
        "the cause stays in the message for the logs"
    );
    assert!(error.remediation().contains("health"));
    Ok(())
}

#[test]
fn minimum_relevance_applies_to_the_fused_relevance() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Working)?;
    let everything = service.query_blended(
        &blended("zzzunmatchedzzz", 10, SearchMode::Blended),
        &ResultControls::default(),
    )?;
    let second = everything.hits.get(1).ok_or("expected a second hit")?.relevance;
    let expected = everything.hits.iter().filter(|hit| hit.relevance >= second).count();

    let controls = ResultControls {
        min_relevance: MinRelevance::try_from(f64::from(second))?,
        ..ResultControls::default()
    };
    let results = service.query_blended(&blended("zzzunmatchedzzz", 10, SearchMode::Blended), &controls)?;

    assert_eq!(results.total, expected);
    assert_eq!(results.hits.len(), expected);
    assert!(expected < everything.total, "the threshold must exclude something");
    Ok(())
}

#[test]
fn the_note_type_filter_covers_files_found_only_by_semantic_search() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Working)?;
    let controls = ResultControls {
        note_type: Some("task"),
        ..ResultControls::default()
    };

    let semantic_only = service.query_blended(&blended("zzzunmatchedzzz", 10, SearchMode::Blended), &controls)?;
    let both = service.query_blended(&blended("content", 10, SearchMode::Blended), &controls)?;

    for results in [&semantic_only, &both] {
        assert_eq!(results.total, 2);
        let mut found = paths(results);
        found.sort_unstable();
        assert_eq!(found, ["topics/alpha.md", "topics/charlie.md"]);
    }
    Ok(())
}

#[test]
fn blended_results_sort_by_title_and_by_modification_time() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Working)?;
    let titles = |results: &BlendedResults| results.hits.iter().map(|hit| hit.title.clone()).collect::<Vec<_>>();

    let by_title = ResultControls {
        sort: SortSpec::from(SortKey::Title),
        ..ResultControls::default()
    };
    let results = service.query_blended(&blended("content", 10, SearchMode::Blended), &by_title)?;
    assert_eq!(titles(&results), ["Alpha", "Bravo", "Charlie"]);

    let by_modified = ResultControls {
        sort: SortSpec::from(SortKey::Modified),
        ..ResultControls::default()
    };
    let results = service.query_blended(&blended("content", 10, SearchMode::Blended), &by_modified)?;
    assert_eq!(titles(&results), ["Charlie", "Bravo", "Alpha"]);
    Ok(())
}

#[test]
fn pages_are_slices_of_the_same_ranking_with_a_stable_total() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Working)?;
    let everything = service.query_blended(&blended("content", 10, SearchMode::Blended), &ResultControls::default())?;

    let mut walked = Vec::new();
    for offset in [0, 2] {
        let controls = ResultControls {
            offset,
            ..ResultControls::default()
        };
        let page = service.query_blended(&blended("content", 2, SearchMode::Blended), &controls)?;
        assert_eq!(page.total, 3);
        walked.extend(page.hits.into_iter().map(|hit| hit.path));
    }

    assert_eq!(walked, paths(&everything));
    Ok(())
}

#[test]
fn oversized_pages_are_rejected_before_any_index_is_consulted() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Working)?;

    let too_big = service.query_blended(
        &blended("content", 101, SearchMode::Blended),
        &ResultControls::default(),
    );
    assert!(matches!(too_big, Err(SearchError::PageLimitExceeded { .. })));

    let controls = ResultControls {
        offset: CANDIDATE_WINDOW - 9,
        ..ResultControls::default()
    };
    let past_window = service.query_blended(&blended("content", 10, SearchMode::Blended), &controls);
    assert!(matches!(past_window, Err(SearchError::PageBeyondWindow { .. })));
    Ok(())
}

#[test]
fn a_truncated_candidate_pool_marks_the_total_as_capped() -> TestResult {
    let vault = vault_dir()?;
    for number in 0..(CANDIDATE_WINDOW + 10) {
        write(
            &vault,
            &format!("crowd/note-{number:03}.md"),
            &format!("Distinct paragraph number {number} with words {number}.\n"),
            0,
        )?;
    }
    let service = service_at(&vault, true, Semantic::Working)?;
    service.rebuild(RebuildTarget::Semantic)?;

    let results = service.query_blended(
        &blended("paragraph", 10, SearchMode::Blended),
        &ResultControls::default(),
    )?;

    assert!(results.total_capped);
    assert!(results.total >= CANDIDATE_WINDOW);
    Ok(())
}

#[test]
fn the_path_scope_applies_to_both_indexes() -> TestResult {
    let vault = vault_dir()?;
    write(&vault, "keep/note.md", "Shared words appear here.\n", 0)?;
    write(&vault, "drop/note.md", "Shared words appear here.\n", 0)?;
    let service = service_at(&vault, true, Semantic::Working)?;
    service.rebuild(RebuildTarget::Semantic)?;
    let excluded = vec!["drop".to_owned()];

    let results = service.query_blended(
        &BlendedQuery {
            query: "shared words",
            limit: 10,
            path_prefix: None,
            exclude_paths: &excluded,
            mode: SearchMode::Blended,
        },
        &ResultControls::default(),
    )?;
    assert_eq!(paths(&results), ["keep/note.md"]);

    let results = service.query_blended(
        &BlendedQuery {
            query: "shared words",
            limit: 10,
            path_prefix: Some("keep"),
            exclude_paths: NO_EXCLUDES,
            mode: SearchMode::Blended,
        },
        &ResultControls::default(),
    )?;
    assert_eq!(paths(&results), ["keep/note.md"]);
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_semantic_candidate_that_escapes_the_vault_is_skipped_in_blended_and_semantic_modes() -> TestResult {
    let (vault, service) = topic_service(true, Semantic::Working)?;
    let outside = tempfile::tempdir()?;
    let target = outside.path().join("secret.md");
    fs::write(&target, "---\ntitle: Secret\n---\n\nOutside the vault.\n")?;
    let link = vault.path().join("topics/bravo.md");
    fs::remove_file(&link)?;
    std::os::unix::fs::symlink(&target, &link)?;

    for mode in [SearchMode::Blended, SearchMode::Semantic] {
        let results = service.query_blended(&blended("content", 10, mode), &ResultControls::default())?;
        assert_eq!(results.total, 2, "{mode:?}");
        assert!(!paths(&results).contains(&"topics/bravo.md"), "{mode:?}");
    }
    Ok(())
}

#[test]
fn a_pool_holding_exactly_the_window_is_not_reported_as_capped_in_blended_mode() -> TestResult {
    let vault = vault_dir()?;
    for number in 0..CANDIDATE_WINDOW {
        write(
            &vault,
            &format!("crowd/note-{number:03}.md"),
            &format!("Distinct paragraph number {number} with words {number}.\n"),
            0,
        )?;
    }
    let service = service_at(&vault, true, Semantic::Working)?;
    service.rebuild(RebuildTarget::Semantic)?;

    let results = service.query_blended(
        &blended("paragraph", 10, SearchMode::Blended),
        &ResultControls::default(),
    )?;

    assert!(!results.total_capped, "exactly the window is not more than the window");
    assert_eq!(results.total, CANDIDATE_WINDOW);
    Ok(())
}

#[test]
fn minimum_relevance_has_an_inclusive_boundary_in_the_single_index_modes_too() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Working)?;
    for mode in [SearchMode::Text, SearchMode::Semantic] {
        let everything = service.query_blended(&blended("content", 10, mode), &ResultControls::default())?;
        let second = everything.hits.get(1).ok_or("expected a second hit")?.relevance;
        let expected = everything.hits.iter().filter(|hit| hit.relevance >= second).count();

        let controls = ResultControls {
            min_relevance: MinRelevance::try_from(f64::from(second))?,
            ..ResultControls::default()
        };
        let results = service.query_blended(&blended("content", 10, mode), &controls)?;

        assert_eq!(results.total, expected, "{mode:?}");
        assert!(results.hits.iter().all(|hit| hit.relevance >= second), "{mode:?}");
    }
    Ok(())
}

#[test]
fn walking_every_blended_page_under_a_title_sort_yields_each_file_once() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Working)?;
    let mut walked = Vec::new();
    for offset in 0..3 {
        let controls = ResultControls {
            offset,
            sort: SortSpec::from(SortKey::Title),
            ..ResultControls::default()
        };
        let page = service.query_blended(&blended("content", 1, SearchMode::Blended), &controls)?;
        assert_eq!(page.total, 3);
        walked.extend(page.hits.into_iter().map(|hit| hit.title));
    }
    assert_eq!(walked, ["Alpha", "Bravo", "Charlie"]);
    Ok(())
}

#[test]
fn a_zero_limit_blended_query_returns_only_the_total() -> TestResult {
    let (_vault, service) = topic_service(true, Semantic::Working)?;

    let results = service.query_blended(&blended("content", 0, SearchMode::Blended), &ResultControls::default())?;

    assert!(
        results.hits.is_empty(),
        "`results.hits` should be empty, got {:?}",
        results.hits
    );
    assert_eq!(results.total, 3);
    Ok(())
}

#[test]
fn a_note_with_many_chunks_is_one_blended_hit_and_a_semantic_only_snippet_is_a_bounded_excerpt() -> TestResult {
    let vault = vault_dir()?;
    let sections = (0..6)
        .map(|number| format!("# Section {number}\n\nParagraph text for section {number}.\n\n"))
        .collect::<Vec<_>>()
        .concat();
    write(&vault, "many.md", &sections, 0)?;
    write(&vault, "long.md", &format!("# Long\n\n{}\n", "word ".repeat(200)), 0)?;
    let service = service_at(&vault, true, Semantic::Working)?;
    service.rebuild(RebuildTarget::Semantic)?;

    let results = service.query_blended(
        &blended("zzzunmatchedzzz", 10, SearchMode::Blended),
        &ResultControls::default(),
    )?;

    let files = paths(&results);
    assert_eq!(
        files.iter().filter(|path| **path == "many.md").count(),
        1,
        "one hit per file"
    );
    assert_eq!(results.total, 2);
    for hit in &results.hits {
        assert_eq!(hit.matched_by, [MatchMode::Semantic]);
        assert!(!hit.snippet.is_empty(), "`hit.snippet` should not be empty");
        assert!(hit.snippet.chars().count() <= 240, "{}", hit.path);
    }
    let long = results
        .hits
        .iter()
        .find(|hit| hit.path == "long.md")
        .ok_or("long.md missing")?;
    assert_eq!(
        long.snippet.chars().count(),
        240,
        "a long chunk is cut to the excerpt length"
    );
    Ok(())
}
