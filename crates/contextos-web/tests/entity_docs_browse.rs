//! Entity documents: browsing, reading, and creating folders.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod entity_docs_common;
mod support;

use entity_docs_common::*;

#[tokio::test]
async fn a_vault_with_no_configured_entities_shows_a_plain_prompt() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = get(&router, &format!("/{VAULT_NAME}/entity/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("This vault has no configured entities yet."), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_new_folder_control_is_a_closed_disclosure_not_an_always_open_form() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The input still exists in the markup (a real `<details>` disclosure,
    // not a fragment fetched on demand), but its own `<details>` carries no
    // `open` attribute - the operator sees a plain "New folder" button
    // until they choose to expand it.
    let marker = "<details class=\"header-disclosure\">";
    let disclosure_at = body.find(marker).ok_or("expected a header-disclosure")?;
    assert!(
        !body[disclosure_at..disclosure_at + marker.len() + 8].contains(" open"),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn get_on_the_bare_root_redirects_to_the_first_configured_entitys_own_url() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = get(&router, &format!("/{VAULT_NAME}/entity/")).await?;
    // `axum::response::Redirect::to`'s own default status, matching
    // `routes::vault::get_server_root`'s identical established precedent.
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    let target = headers.get("location").ok_or("expected a Location header")?.to_str()?;
    assert_eq!(target, format!("/{VAULT_NAME}/entity/zenith"));
    Ok(())
}

#[tokio::test]
async fn get_on_a_real_entitys_own_root_path_lists_that_entitys_own_real_top_level_content() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(
        vault_dir.path(),
        "zenith/finance/overview.md",
        "# Overview\n\nBudget notes.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Zenith"), "{body}");
    assert!(body.contains("finance"), "{body}");
    // The entity's own name renders once, in the header row - not a second
    // time in the tree sidebar's own heading, which would read as the
    // entity's own top-level folder sharing its name with the entity
    // itself, an accurate but redundant repetition of information already
    // shown once.
    let marker = "<div class=\"section-label\">Zenith</div>";
    assert_eq!(body.matches(marker).count(), 1, "{body}");
    Ok(())
}

#[tokio::test]
async fn get_on_a_real_nested_folder_path_lists_that_folders_own_real_children_never_synthesised_ones()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(
        vault_dir.path(),
        "zenith/finance/overview.md",
        "# Overview\n\nBudget notes.\n",
    )?;
    write(vault_dir.path(), "zenith/finance/export.csv", "a,b\n1,2\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("overview"), "{body}");
    assert!(body.contains("export.csv"), "{body}");
    // Never the mock's own synthetic agent-domain folder names - only what
    // is genuinely on disk.
    assert!(!body.contains("Marketing"), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_on_a_real_markdown_file_path_renders_it_inline() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(
        vault_dir.path(),
        "zenith/finance/overview.md",
        "# Overview\n\nBudget notes for this quarter.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/overview.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Budget notes for this quarter."), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_read_views_edit_button_targets_the_whole_screen_the_edit_response_actually_is() -> Result<(), BoxError> {
    // `render_edit_response` (`routes/entity_docs.rs`) returns the whole
    // `.entity-docs-screen`, sidebar tree included - never a bare
    // `#entity-docs-main-body` fragment alone, because the header's own
    // Edit/Delete cluster lives outside that div. A read-view button that
    // still targeted `#entity-docs-main-body` with `hx-swap="outerHTML"`
    // would nest that whole second screen inside the original screen's
    // surviving chrome instead of replacing it.
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(
        vault_dir.path(),
        "zenith/finance/overview.md",
        "# Overview\n\nBudget notes for this quarter.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/overview.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (before, after) = body
        .split_once("title=\"Edit file\"")
        .ok_or("read view must render an Edit button")?;
    let button_start = before.rfind("<button").ok_or("Edit button must open with <button")?;
    let button_end = after.find("</button>").ok_or("Edit button must close")?;
    let button_markup = &before[button_start..];
    let edit_button = format!("{button_markup}title=\"Edit file\"{}", &after[..button_end]);
    assert!(
        edit_button.contains("hx-target=\"closest .entity-docs-screen\""),
        "Edit button must target the same element the `/edit` response replaces wholesale: {edit_button}"
    );
    Ok(())
}

#[tokio::test]
async fn get_on_a_real_non_markdown_file_path_answers_with_the_file_itself() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(vault_dir.path(), "zenith/finance/export.csv", "a,b\n1,2\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/export.csv")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "a,b\n1,2\n");
    let content_type = headers.get("content-type").ok_or("expected a content type")?.to_str()?;
    assert!(content_type.starts_with("text/csv"), "{content_type}");
    Ok(())
}

#[tokio::test]
async fn a_non_markdown_files_own_row_links_to_the_general_vault_content_route_instead_of_this_screen()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(vault_dir.path(), "zenith/finance/export.csv", "a,b\n1,2\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains(&format!("href=\"/{VAULT_NAME}/zenith/finance/export.csv\"")),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn get_on_an_unconfigured_entity_slug_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body, _headers) = get(&router, &format!("/{VAULT_NAME}/entity/nonexistent")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn get_on_a_path_naming_neither_a_real_folder_nor_a_real_file_under_a_real_entity_is_a_404()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body, _headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/missing.md")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

// ---------------------------------------------------------------------
// The accordion folder tree
// ---------------------------------------------------------------------

#[tokio::test]
async fn browsing_a_deep_folder_renders_a_nested_accordion_with_only_the_anchor_path_open() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(
        vault_dir.path(),
        "zenith/finance/tax.md",
        "---\ntitle: Tax\n---\n\n# Tax\n",
    )?;
    write(
        vault_dir.path(),
        "zenith/marketing/campaigns.md",
        "---\ntitle: Campaigns\n---\n\n# Campaigns\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get_fragment(&router, &format!("/{VAULT_NAME}/entity/zenith/finance")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let tree = body
        .split("class=\"notes-sidebar\"")
        .nth(1)
        .ok_or("expected a sidebar section")?;
    assert!(tree.matches("class=\"nav-group\"").count() >= 2, "{tree}");
    let finance_index = tree.find(">finance<").ok_or("missing finance folder heading")?;
    let marketing_index = tree.find(">marketing<").ok_or("missing marketing folder heading")?;
    let finance_open = tree[..finance_index]
        .rfind("<details class=\"nav-group\"")
        .is_some_and(|start| tree[start..finance_index].contains("open"));
    assert!(finance_open, "{tree}");
    let marketing_open = tree[..marketing_index]
        .rfind("<details class=\"nav-group\"")
        .is_some_and(|start| tree[start..marketing_index].contains("open"));
    assert!(!marketing_open, "{tree}");
    Ok(())
}

// ---------------------------------------------------------------------
// POST /{{vault_name}}/entity/{{*path}} - the "New" file action,
// and GET /{{vault_name}}/entity/{{*path}}/new - the dedicated
// create-file form: see tests/entity_docs_create.rs, kept apart from this
// file for the identical reason tests/wiki_create.rs is kept apart from
// tests/wiki.rs.
// ---------------------------------------------------------------------

// ---------------------------------------------------------------------
// POST /{{vault_name}}/entity/{{*path}}/execute/new-folder - "New
// folder", reachable anywhere including the entity's own root
// ---------------------------------------------------------------------

#[tokio::test]
async fn new_folder_is_reachable_at_the_entitys_own_root() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/execute/new-folder"),
        r#"{"name":"Finance"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let redirected = redirected_path(&headers).ok_or("expected an HX-Redirect target")?;
    assert_eq!(redirected, "finance");
    assert!(vault_dir.path().join("zenith/finance").is_dir());
    Ok(())
}

#[tokio::test]
async fn new_folder_is_reachable_one_level_deep() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(vault_dir.path(), "zenith/finance/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/finance/execute/new-folder"),
        r#"{"name":"Tax"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let redirected = redirected_path(&headers).ok_or("expected an HX-Redirect target")?;
    assert_eq!(redirected, "finance/tax");
    assert!(vault_dir.path().join("zenith/finance/tax").is_dir());
    Ok(())
}

#[tokio::test]
async fn new_folder_with_an_empty_name_is_refused() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/execute/new-folder"),
        r#"{"name":"   "}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Redirect").is_none(), "{body}");
    assert!(body.contains("A folder needs a name."), "{body}");
    Ok(())
}
