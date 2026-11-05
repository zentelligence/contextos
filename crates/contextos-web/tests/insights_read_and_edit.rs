//! Insights: reading shards and notes, editing bodies, and the metadata panel.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod insights_common;
mod support;

use insights_common::*;

// ---------------------------------------------------------------------
// GET /{{vault_name}}/insights/{{*path}}
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_empty_insights_store_still_offers_every_fixed_shard_key_and_selects_nothing() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Select a shard"), "{body}");
    let shard_grid = body
        .split("class=\"insights-shard-grid\"")
        .nth(1)
        .ok_or("expected an insights-shard-grid section")?;
    let shard_grid_end = shard_grid.find("</div>").unwrap_or(shard_grid.len());
    let shard_grid = &shard_grid[..shard_grid_end];
    // The full fixed 36-key `a`-`z`/`0`-`9` space renders even though this
    // vault has never held a single insight note - a shard with nothing in
    // it yet must still be selectable, so a new note's slug can land there.
    for key in ["a", "q", "z", "0", "5", "9"] {
        assert!(
            shard_grid.contains(&format!(
                "class=\"insights-shard-key\" href=\"/{VAULT_NAME}/insights/{key}\">{key}</a>"
            )),
            "expected shard key {key} in {shard_grid}"
        );
    }
    assert_eq!(
        shard_grid.matches("insights-shard-key").count(),
        36,
        "expected all 36 fixed shard keys, got: {shard_grid}"
    );
    Ok(())
}

#[tokio::test]
async fn get_on_a_real_shard_path_lists_that_shards_own_notes() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/a")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Agent backbone"), "{body}");
    assert!(body.contains("adopted"), "{body}");
    assert!(
        body.contains(&format!(
            "class=\"insights-shard-key active\" href=\"/{VAULT_NAME}/insights/a\">a</a>"
        )),
        "expected shard key a marked active in {body}"
    );
    Ok(())
}

#[tokio::test]
async fn get_on_a_real_note_path_renders_its_content() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\n---\n\n# Agent backbone\n\nThree components.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/a/agent-backbone.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Three components."), "{body}");
    // Rename/restatus sits in the top header row, alongside Edit/Delete -
    // never after the rendered content and backlinks, where it would read
    // as detached from the other per-note actions.
    let header_end = body
        .find("insights-main-body")
        .ok_or("expected an insights-main-body marker")?;
    let rename_at = body.find("Rename or restatus").ok_or("expected a Rename control")?;
    assert!(rename_at < header_end, "{body}");
    Ok(())
}

#[tokio::test]
async fn get_on_a_shard_with_no_notes_yet_is_still_reachable() -> Result<(), BoxError> {
    // A shard with nothing filed in it on disk is still a real, selectable
    // shard: it is exactly where a new insight whose slug starts with `a`
    // would land, so it must not 404.
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/a")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Nothing in this shard."), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_on_a_path_segment_outside_the_fixed_shard_keyspace_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = get(&router, &format!("/{VAULT_NAME}/insights/zz")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn get_on_a_missing_note_within_a_real_shard_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = get(&router, &format!("/{VAULT_NAME}/insights/a/missing.md")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn a_notes_own_real_backlink_is_shown_via_the_wikilink_graph() -> Result<(), BoxError> {
    // A non-dot-prefixed directory name: the graph/search index storage
    // layer derives its own on-disk keyspace name from the vault root's
    // final path segment, which must be a valid URI scheme token - the
    // dot-prefixed name `tempfile::tempdir()` otherwise produces on this
    // platform is not one.
    let vault_dir = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    // The linked-to note is written first so the linking note's own
    // wikilink resolves to a real node rather than a phantom one.
    write(
        vault_dir.path(),
        "memory/insights/d/deterministic-scripts.md",
        "---\ntitle: Deterministic scripts\nstatus: adopted\n---\n\nDeterministic beats probabilistic.\n",
    )?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\n---\n\nSee [[deterministic-scripts]].\n",
    )?;
    let router = router_over_with_graph_enabled(vault_dir.path(), config_dir.path()).await?;
    // Both notes above were seeded directly on disk before this session
    // connected, so the wikilink graph has never seen them (it only
    // updates automatically as a side effect of a real write pipeline
    // mutation) - an explicit rebuild makes this assertion deterministic
    // rather than depending on incidental startup-scan timing.
    let (rebuild_status, rebuild_body, _) =
        post(&router, "/mcp/contextos/query_index_rebuild", r#"{"index":"graph"}"#).await?;
    assert_eq!(rebuild_status, StatusCode::OK, "{rebuild_body}");

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/d/deterministic-scripts.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Linked from"), "{body}");
    assert!(body.contains("Agent backbone"), "{body}");
    Ok(())
}

// ---------------------------------------------------------------------
// PATCH /{{vault_name}}/insights/{{*path}} - editing a note
// ---------------------------------------------------------------------

#[tokio::test]
async fn get_on_a_notes_edit_path_pre_fills_the_edit_form_with_the_bodys_own_text() -> Result<(), BoxError> {
    // Fix B moved the edit form out of the read view's own inline
    // `<details class="wiki-edit">` toggle into its own reserved `/edit`
    // sub-path (`docs/http-routing-response-contract.md`'s own
    // `GET /{entities}/{id}/edit` pattern) -
    // `get_on_a_notes_edit_suffix_returns_a_pre_populated_edit_form_fragment_not_the_read_view`
    // covers that shape in full; this test only re-confirms the same
    // pre-filled-body/never-frontmatter contract survived the move.
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\n---\n\nThree components.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/a/agent-backbone.md/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Three components."), "{body}");
    assert!(!body.contains("title: Agent backbone"), "{body}");
    Ok(())
}

#[tokio::test]
async fn saving_an_edited_body_persists_it_and_keeps_the_status_untouched() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\n---\n\nThree components.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/insights/a/agent-backbone.md/edit")).await?;
    let expected_hash = extract_expected_hash(&body).ok_or("expected an expected_hash field")?;

    let (status, saved_body) = patch(
        &router,
        &format!("/{VAULT_NAME}/insights/a/agent-backbone.md"),
        &format!(r#"{{"content":"Four components now.\n","expected_hash":"{expected_hash}"}}"#),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{saved_body}");
    assert!(saved_body.contains("Four components now."), "{saved_body}");

    let content = std::fs::read_to_string(vault_dir.path().join("memory/insights/a/agent-backbone.md"))?;
    assert!(content.contains("title: Agent backbone"), "{content}");
    assert!(content.contains("status: adopted"), "{content}");
    assert!(content.contains("Four components now."), "{content}");
    assert!(!content.contains("Three components."), "{content}");
    Ok(())
}

#[tokio::test]
async fn saving_with_a_stale_hash_is_rejected_with_a_conflict_message_and_leaves_the_file_untouched()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\n---\n\nOriginal body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/insights/a/agent-backbone.md"),
        r#"{"content":"Conflicting body.\n","expected_hash":"not-the-real-hash"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("This note changed since it was last loaded; reload and try again."),
        "{body}"
    );

    let content = std::fs::read_to_string(vault_dir.path().join("memory/insights/a/agent-backbone.md"))?;
    assert!(content.contains("Original body."), "{content}");
    Ok(())
}

#[tokio::test]
async fn saving_the_body_of_a_note_with_unparsable_frontmatter_reports_it_without_writing() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    // Opens a frontmatter block but never closes it - `frontmatter_read` reports this as its
    // own `Unclosed` error, real content the note still has (unlike a missing file, which
    // `read_note`'s own preceding fresh read already rules out), so `insights::save_note_body`
    // reaches `SaveNoteError::Malformed` rather than misreporting it as `NotFound` - the same
    // pre-existing `vault_content::save_body` misclassification `tests/wiki.rs`'s identical
    // `saving_the_body_of_a_page_with_unparsable_frontmatter_reports_it_without_writing` case
    // guards for Wiki's own body-save path.
    let malformed = "---\ntitle: Agent backbone\n\nBody with no closing frontmatter delimiter.\n";
    write(vault_dir.path(), "memory/insights/a/agent-backbone.md", malformed)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/insights/a/agent-backbone.md"),
        r#"{"content":"New body.\n","expected_hash":null}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("frontmatter could not be read"), "{body}");

    let unchanged = std::fs::read_to_string(vault_dir.path().join("memory/insights/a/agent-backbone.md"))?;
    assert_eq!(unchanged, malformed);
    Ok(())
}

#[tokio::test]
async fn the_metadata_panel_renders_every_real_frontmatter_field_on_a_selected_note() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\ntags:\n  - agents\n  - architecture\n---\n\nThree components.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/a/agent-backbone.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("data-field=\"title\""), "{body}");
    assert!(body.contains("data-field=\"status\""), "{body}");
    assert!(body.contains("value=\"adopted\""), "{body}");
    assert!(body.contains("data-field=\"tags\""), "{body}");
    assert!(body.contains("agents, architecture"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_note_with_no_frontmatter_fields_shows_the_generic_empty_state_message() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "Plain body with no frontmatter block at all.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/a/agent-backbone.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("This note has no frontmatter fields."), "{body}");
    Ok(())
}

#[tokio::test]
async fn patching_the_metadata_panel_saves_the_submitted_fields_and_leaves_the_body_untouched() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: proposed\n---\n\nThree components.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/insights/a/agent-backbone.md")).await?;
    let expected_hash = extract_metadata_expected_hash(&body).ok_or("expected an expected_hash field")?;

    let (status, saved_body) = patch(
        &router,
        &format!("/{VAULT_NAME}/insights/a/agent-backbone.md"),
        &format!(r#"{{"patch":{{"status":"adopted"}},"expected_hash":"{expected_hash}"}}"#),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{saved_body}");

    let content = std::fs::read_to_string(vault_dir.path().join("memory/insights/a/agent-backbone.md"))?;
    assert!(content.contains("status: adopted"), "{content}");
    assert!(content.contains("title: Agent backbone"), "{content}");
    assert!(content.contains("Three components."), "{content}");
    Ok(())
}

#[tokio::test]
async fn patching_the_metadata_panel_with_a_stale_hash_is_rejected_and_leaves_the_file_untouched()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: proposed\n---\n\nOriginal body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/insights/a/agent-backbone.md"),
        r#"{"patch":{"status":"adopted"},"expected_hash":"not-the-real-hash"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("This note changed since it was last loaded; reload and try again."),
        "{body}"
    );

    let content = std::fs::read_to_string(vault_dir.path().join("memory/insights/a/agent-backbone.md"))?;
    assert!(content.contains("status: proposed"), "{content}");
    Ok(())
}

#[tokio::test]
async fn patching_the_metadata_panel_on_a_note_with_unparsable_frontmatter_reports_it_without_writing()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    // Opens a frontmatter block but never closes it - `frontmatter_read` reports this as its
    // own `Unclosed` error, real content the note still has (unlike a missing file), so
    // `insights::load_note_frontmatter` reaches `LoadInsightError::Malformed` on its own fresh
    // re-read rather than `NotFound`, matching `tests/wiki.rs`'s identical
    // `patching_the_metadata_panel_on_a_page_with_unparsable_frontmatter_reports_it_without_writing`
    // case for the metadata-patch path instead of the body-save one.
    let malformed = "---\ntitle: Agent backbone\n\nBody with no closing frontmatter delimiter.\n";
    write(vault_dir.path(), "memory/insights/a/agent-backbone.md", malformed)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/insights/a/agent-backbone.md"),
        r#"{"patch":{"status":"adopted"},"expected_hash":null}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("frontmatter could not be read"), "{body}");

    let unchanged = std::fs::read_to_string(vault_dir.path().join("memory/insights/a/agent-backbone.md"))?;
    assert_eq!(unchanged, malformed);
    Ok(())
}

#[tokio::test]
async fn the_metadata_panel_side_swaps_the_layout_class_independently_of_the_shard_side() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\n---\n\nThree components.\n",
    )?;
    // The shard picker stays on its own shipped default (right); only the metadata panel moves
    // to the left, so the layout needs its own dedicated left column for it
    // (`notes-layout-metadata-left-panel-right`) rather than either single-side modifier.
    write(
        vault_dir.path(),
        ".contextos/insights.toml",
        "metadata_panel_side = \"left\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/a/agent-backbone.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("notes-layout-metadata-left-panel-right"), "{body}");
    Ok(())
}

#[tokio::test]
async fn saving_a_missing_note_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = patch(
        &router,
        &format!("/{VAULT_NAME}/insights/a/missing.md"),
        r#"{"content":"Anything.\n"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}
