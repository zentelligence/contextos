use super::*;

fn hit(path: &str) -> SearchHit {
    SearchHit {
        path: path.to_owned(),
        title: path.to_owned(),
        relevance: 1.0,
        snippet: String::new(),
        snippet_is_html: false,
        matched_by: Vec::new(),
        modified: None,
        metadata: HitMetadata::default(),
    }
}

#[test]
fn a_hit_at_the_vault_root_contributes_no_folder() {
    let aggregates = aggregate_by_folder(&[hit("readme.md")]);
    assert!(
        aggregates.is_empty(),
        "`aggregates` should be empty, got {aggregates:?}"
    );
}

#[test]
fn folders_are_aggregated_by_nested_path_with_counts_rolled_up_to_ancestors() -> Result<(), Box<dyn std::error::Error>>
{
    let hits = vec![
        hit("zenith/projects/orion/requirements/req-001.md"),
        hit("zenith/projects/orion/requirements/req-002.md"),
        hit("zenith/projects/orion/development/brief.md"),
        hit("personal/journal/entry.md"),
    ];
    let aggregates = aggregate_by_folder(&hits);

    let zenith = aggregates
        .iter()
        .find(|a| a.label == "zenith")
        .ok_or("zenith folder should be aggregated")?;
    assert_eq!(zenith.count, 3);
    assert_eq!(zenith.relative_dir, "zenith");
    let projects = zenith
        .children
        .iter()
        .find(|a| a.label == "projects")
        .ok_or("projects folder should be aggregated")?;
    assert_eq!(projects.count, 3);
    let orion = projects
        .children
        .iter()
        .find(|a| a.label == "orion")
        .ok_or("orion folder should be aggregated")?;
    let requirements = orion
        .children
        .iter()
        .find(|a| a.label == "requirements")
        .ok_or("requirements folder should be aggregated")?;
    assert_eq!(requirements.count, 2);
    assert_eq!(requirements.relative_dir, "zenith/projects/orion/requirements");

    let personal = aggregates
        .iter()
        .find(|a| a.label == "personal")
        .ok_or("personal folder should be aggregated")?;
    assert_eq!(personal.count, 1);
    Ok(())
}

#[test]
fn top_level_aggregates_are_sorted_alphabetically_by_label() {
    let hits = vec![hit("zebra/a.md"), hit("alpha/b.md"), hit("mid/c.md")];
    let aggregates = aggregate_by_folder(&hits);
    let labels: Vec<&str> = aggregates.iter().map(|a| a.label.as_str()).collect();
    assert_eq!(labels, ["alpha", "mid", "zebra"]);
}

#[test]
fn a_blended_hit_matched_by_text_is_marked_as_safe_html_snippet() {
    let blended = QueryBlendedResult {
        hits: vec![BlendedHitResult {
            path: "a.md".to_owned(),
            title: "A".to_owned(),
            relevance: 0.5,
            snippet: "<b>hit</b>".to_owned(),
            matched_by: vec!["text".to_owned()],
            modified: None,
            metadata: HitMetadataResult::default(),
        }],
        total: 1,
        offset: 0,
        total_capped: false,
        warnings: Vec::new(),
    };
    let response = SearchResponse::from(blended);
    assert!(response.hits[0].snippet_is_html);
}

#[test]
fn a_blended_hit_matched_only_by_semantic_is_marked_as_untrusted_plain_text() {
    let blended = QueryBlendedResult {
        hits: vec![BlendedHitResult {
            path: "a.md".to_owned(),
            title: "A".to_owned(),
            relevance: 0.5,
            snippet: "raw chunk text".to_owned(),
            matched_by: vec!["semantic".to_owned()],
            modified: None,
            metadata: HitMetadataResult::default(),
        }],
        total: 1,
        offset: 0,
        total_capped: false,
        warnings: Vec::new(),
    };
    let response = SearchResponse::from(blended);
    assert!(!response.hits[0].snippet_is_html);
}

#[test]
fn query_text_hits_are_always_marked_as_safe_html_snippet() {
    let text = QueryTextResult {
        hits: vec![TextHitResult {
            path: "a.md".to_owned(),
            relevance: 0.5,
            title: "A".to_owned(),
            snippet: "<b>hit</b>".to_owned(),
            modified: "2026-01-01T00:00:00Z".to_owned(),
            metadata: HitMetadataResult::default(),
        }],
        total: 1,
        offset: 0,
    };
    let response = SearchResponse::from(text);
    assert!(response.hits[0].snippet_is_html);
    assert_eq!(response.hits[0].matched_by, vec!["text".to_owned()]);
}

#[test]
fn query_semantic_hits_are_never_marked_as_safe_html_and_fall_back_to_the_path_for_a_missing_title() {
    let semantic = QuerySemanticResult {
        hits: vec![SemanticHitResult {
            path: "a.md".to_owned(),
            chunk: "raw chunk".to_owned(),
            relevance: 0.5,
            title: None,
            modified: None,
            metadata: HitMetadataResult::default(),
        }],
        total: 1,
        offset: 0,
        total_capped: false,
    };
    let response = SearchResponse::from(semantic);
    assert!(!response.hits[0].snippet_is_html);
    assert_eq!(response.hits[0].title, "a.md");
    assert_eq!(response.hits[0].matched_by, vec!["semantic".to_owned()]);
}

#[test]
fn a_semantic_chunk_excerpt_longer_than_the_limit_is_truncated_with_an_ellipsis() {
    let long = "x".repeat(SEMANTIC_SNIPPET_MAX_CHARS + 50);
    let truncated = truncate_chars(&long, SEMANTIC_SNIPPET_MAX_CHARS);
    assert_eq!(truncated.chars().count(), SEMANTIC_SNIPPET_MAX_CHARS + 1);
    assert!(truncated.ends_with('…'));
}

#[test]
fn a_semantic_chunk_excerpt_within_the_limit_is_left_unchanged() {
    let short = "a short chunk";
    assert_eq!(truncate_chars(short, SEMANTIC_SNIPPET_MAX_CHARS), short);
}

#[test]
fn note_title_from_path_strips_the_directory_and_extension() {
    assert_eq!(note_title_from_path("zenith/projects/orion/a-027.md"), "a-027");
    assert_eq!(note_title_from_path("root.md"), "root");
}

fn links_read(json: serde_json::Value) -> Result<Vec<LinkedNote>, serde_json::Error> {
    Ok(Vec::from(serde_json::from_value::<LinksReadResult>(json)?))
}

#[test]
fn a_resolved_link_shows_the_file_it_reaches_not_the_text_it_was_written_as() -> Result<(), serde_json::Error> {
    let panel = links_read(serde_json::json!({
        "outgoing": [
            {"target": "target", "embed": false, "form": "wikilink", "resolved": "notes/target.md"},
            {"target": "pic.png", "embed": true, "form": "wikilink-embed", "resolved": "assets/pic.png"},
        ],
        "unresolved": [],
    }))?;

    assert_eq!(
        panel,
        vec![
            LinkedNote {
                path: "notes/target.md".to_owned(),
                title: "target".to_owned(),
                is_embed: false,
                target: LinkTarget::Note,
            },
            LinkedNote {
                path: "assets/pic.png".to_owned(),
                title: "pic.png".to_owned(),
                is_embed: true,
                target: LinkTarget::File,
            },
        ]
    );
    Ok(())
}

#[test]
fn links_that_find_no_file_or_several_are_shown_with_why_instead_of_being_left_out() -> Result<(), serde_json::Error> {
    let panel = links_read(serde_json::json!({
        "outgoing": [
            {"target": "gone", "embed": false, "form": "wikilink"},
            {"target": "status", "embed": false, "form": "markdown"},
        ],
        "unresolved": [
            {"target": "gone", "form": "wikilink", "kind": "not-found"},
            {"target": "status", "form": "markdown", "kind": "ambiguous", "candidates": ["a/status.md", "b/status.md"]},
        ],
    }))?;

    assert_eq!(panel[0].target, LinkTarget::NotFound);
    assert_eq!(panel[0].path, "gone");
    assert_eq!(
        panel[1].target,
        LinkTarget::Ambiguous(vec!["a/status.md".to_owned(), "b/status.md".to_owned()])
    );
    Ok(())
}

#[test]
fn a_placeholder_link_a_repeated_link_and_a_managed_index_link_are_not_listed() -> Result<(), serde_json::Error> {
    let panel = links_read(serde_json::json!({
        "outgoing": [
            {"target": "{{slug}}", "embed": false, "form": "wikilink"},
            {"target": "b", "embed": false, "form": "wikilink", "resolved": "b.md"},
            {"target": "b", "embed": false, "form": "markdown", "resolved": "b.md"},
            {"target": "b", "embed": true, "form": "wikilink-embed", "resolved": "b.md"},
            {"target": "folder/", "embed": false, "form": "markdown", "resolved": "folder/index.md"},
        ],
        "unresolved": [],
    }))?;

    let listed: Vec<_> = panel.iter().map(|note| (note.path.as_str(), note.is_embed)).collect();
    assert_eq!(listed, vec![("b.md", false), ("b.md", true)]);
    Ok(())
}
