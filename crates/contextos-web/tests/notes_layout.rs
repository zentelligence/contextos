//! Notes screen: sidebar layout, dated and regular panes, and the metadata panel.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod notes_common;
mod support;

use notes_common::*;

#[tokio::test]
async fn a_note_placed_directly_under_notes_dir_renders_as_a_root_level_entry_not_dropped() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/dated.md",
        "---\ntitle: Dated note\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nDated body.\n",
    )?;
    // Hand-placed directly under `notes_dir`, with no folder at all: the
    // default `{{YYYY}}/{{MM}}/` partition is only ever a default for a
    // brand-new note, never a shape every real note is assumed to fit.
    write(
        vault_dir.path(),
        "notes/stray.md",
        "---\ntitle: Stray note\nstatus: draft\ntags: []\n---\n\nStray body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Stray note"), "{body}");
    assert!(body.contains("Dated note"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_note_directly_under_notes_dir_supports_the_same_full_crud_as_a_nested_one() -> Result<(), BoxError> {
    // The bare root (`/{{vault_name}}/notes`, `/{{vault_name}}/notes/`) is never a valid
    // target for save/rename/delete - only `notes_dir` itself, never a file this screen
    // acts on. A note with no folder at all is still a single, real path segment on the
    // `{{*path}}` wildcard (`notes/stray.md`), routed identically to a nested one
    // (`notes/2026/09/dated.md`) - never mistaken for the bare root's own distinct
    // `get_root`/`create_root`/`delete_root` handlers, which axum only ever reaches for a
    // request with no path segment after `/notes` at all.
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/stray.md",
        "---\ntitle: Stray note\nstatus: draft\ntags: []\n---\n\nStray body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // GET reaches `get_path`, not `get_root`'s own "most recent" fallback path.
    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/stray.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Stray body."), "{body}");
    let expected_hash = extract_expected_hash(&body).ok_or("expected_hash present in the rendered form")?;

    // PATCH (body) reaches `patch_body`'s plain content branch.
    let (status, body, _) = patch(
        &router,
        &format!("/{VAULT_NAME}/notes/stray.md"),
        &format!(r#"{{"content": "Edited stray body.", "expected_hash": "{expected_hash}"}}"#),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Edited stray body."), "{body}");

    // PATCH .../rename reaches `patch_rename_dispatch`, not `patch_body`'s plain branch.
    let (status, body, headers) = patch(
        &router,
        &format!("/{VAULT_NAME}/notes/stray.md/rename"),
        r#"{"new_name": "renamed-stray"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let renamed = redirected_note(&headers).ok_or("HX-Redirect header present with a note path")?;
    assert_eq!(renamed, "renamed-stray.md");
    assert!(vault_dir.path().join("notes/renamed-stray.md").exists());

    // DELETE reaches `delete_path`, never `delete_root`'s own unconditional 404.
    let (status, body) = delete(&router, &format!("/{VAULT_NAME}/notes/renamed-stray.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!vault_dir.path().join("notes/renamed-stray.md").exists());
    Ok(())
}

#[tokio::test]
async fn an_entirely_operator_defined_folder_structure_renders_correctly_not_just_the_default_partition()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    // No date partition anywhere in sight: a project folder, three levels
    // deep, organised entirely by the operator's own hand, not by any
    // convention this crate imposes.
    write(
        vault_dir.path(),
        "notes/projects/website-redesign/planning/kickoff.md",
        "---\ntitle: Kickoff\nstatus: draft\ntags: []\n---\n\nKickoff body.\n",
    )?;
    write(
        vault_dir.path(),
        "notes/projects/website-redesign/planning/retro.md",
        "---\ntitle: Retro\nstatus: draft\ntags: []\n---\n\nRetro body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(">projects<"), "{body}");
    assert!(body.contains(">website-redesign<"), "{body}");
    assert!(body.contains(">planning<"), "{body}");
    assert!(body.contains("Kickoff"), "{body}");
    assert!(body.contains("Retro"), "{body}");
    Ok(())
}

#[tokio::test]
async fn with_the_default_settings_a_mix_of_dated_and_operator_named_notes_render_in_separate_panes()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/2026-09-14-1858.md",
        "---\ntitle: Dated note\nstatus: draft\ntags: []\n---\n\nDated body.\n",
    )?;
    write(
        vault_dir.path(),
        "notes/projects/kickoff.md",
        "---\ntitle: Kickoff\nstatus: draft\ntags: []\n---\n\nKickoff body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("notes-layout-split"), "{body}");
    assert!(body.contains(">Dated<"), "{body}");
    assert!(body.contains(">Regular<"), "{body}");
    assert!(body.contains("Dated note"), "{body}");
    assert!(body.contains("Kickoff"), "{body}");
    // Right by default (the shipped `DateStructureSide` default): the
    // dated pane's own label appears after the regular pane's in the raw
    // markup, matching source order left-to-right.
    let regular_at = body.find(">Regular<").ok_or("missing Regular label")?;
    let dated_at = body.find(">Dated<").ok_or("missing Dated label")?;
    assert!(regular_at < dated_at, "expected Regular (left) before Dated (right)");
    let notes_main_at = body.find("class=\"notes-main\"").ok_or("missing notes-main pane")?;
    assert!(
        regular_at < notes_main_at && notes_main_at < dated_at,
        "expected the content pane between Regular (left) and Dated (right): {body}"
    );
    Ok(())
}

#[tokio::test]
async fn a_dated_note_missing_the_default_formats_own_time_suffix_still_lands_in_the_dated_pane() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    // A real, reported shape: filed under a genuinely dated `{{YYYY}}/{{MM}}/`
    // folder, but the filename itself lacks the shipped default format's
    // own trailing `-HHmm` (a dictated or hand-renamed note commonly does).
    write(
        vault_dir.path(),
        "notes/2026/05/2026-05-11.md",
        "---\ntitle: No time suffix\nstatus: draft\ntags: []\n---\n\nBody.\n",
    )?;
    write(
        vault_dir.path(),
        "notes/anthropic-plugin-inventory/analytics/analyze.md",
        "---\ntitle: Analyze\nstatus: draft\ntags: []\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Each pane is its own `<aside class="notes-sidebar">...</aside>` block,
    // with the selected note's own main panel rendered between the two
    // (`notes-layout-split`'s own regular/main/dated document order) - bound
    // each slice to its own `</aside>`, never to the other pane's own start,
    // so a selected note whose own title happens to match the other pane's
    // label text (as "No time suffix" now correctly does, once it is the
    // selected dated note this test proves it lands as) is never
    // misattributed to the wrong pane.
    let dated_start = body.find(">Dated<").ok_or("missing Dated label")?;
    let regular_start = body.find(">Regular<").ok_or("missing Regular label")?;
    let dated_panel = &body[dated_start..][..body[dated_start..].find("</aside>").ok_or("unterminated dated aside")?];
    let regular_panel = &body[regular_start..][..body[regular_start..]
        .find("</aside>")
        .ok_or("unterminated regular aside")?];
    assert!(dated_panel.contains("No time suffix"), "{dated_panel}");
    assert!(!regular_panel.contains("No time suffix"), "{regular_panel}");
    assert!(!regular_panel.contains(">2026<"), "{regular_panel}");
    assert!(regular_panel.contains("Analyze"), "{regular_panel}");
    Ok(())
}

#[tokio::test]
async fn a_note_with_no_frontmatter_title_shows_its_own_filename_not_its_full_relative_path() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/projects/website/2026-09-14-1858.md",
        "---\nstatus: draft\ntags: []\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The rename/save action URLs legitimately carry the full
    // `/notes/projects/website/2026-09-14-1858.md` relative path - only the
    // *displayed identity* (the sidebar row's own title, and the rename
    // form's own starting value) must never fall back to it.
    assert!(
        body.contains("class=\"notes-list-item-title\">2026-09-14-1858</span>"),
        "{body}"
    );
    assert!(
        !body.contains("class=\"notes-list-item-title\">projects/website/2026-09-14-1858</span>"),
        "the full relative path must never stand in for a missing title: {body}"
    );
    assert!(body.contains("value=\"2026-09-14-1858\""), "{body}");
    assert!(!body.contains("value=\"projects/website/2026-09-14-1858\""), "{body}");
    Ok(())
}

#[tokio::test]
async fn setting_date_structure_side_to_left_moves_the_dated_pane_before_the_regular_one() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/notes.toml",
        "notes_dir = \"notes\"\n\
         template_path = \"templates/unique-note.md\"\n\
         date_format = \"YYYY/MM/YYYY-MM-DD-HHmm\"\n\
         date_structure_active = true\n\
         date_structure_side = \"left\"\n",
    )?;
    write(
        vault_dir.path(),
        "notes/2026/09/2026-09-14-1858.md",
        "---\ntitle: Dated note\nstatus: draft\ntags: []\n---\n\nDated body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let dated_at = body.find(">Dated<").ok_or("missing Dated label")?;
    let regular_at = body.find(">Regular<").ok_or("missing Regular label")?;
    assert!(dated_at < regular_at, "expected Dated (left) before Regular (right)");
    // The content pane must sit between the two sidebars, not drift to
    // whichever edge the source order happens to leave free: the grid's
    // three columns are 220px/flexible-middle/220px regardless of which
    // side `date_structure_side` puts the dated pane on, so the flexible
    // middle column always has to land on `notes-main` in the markup.
    let notes_main_at = body.find("class=\"notes-main\"").ok_or("missing notes-main pane")?;
    assert!(
        dated_at < notes_main_at && notes_main_at < regular_at,
        "expected the content pane between Dated (left) and Regular (right), not beside Dated: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn the_metadata_panel_stacks_above_whichever_pane_its_own_setting_names_independently_of_the_dated_pane_side()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/notes.toml",
        "notes_dir = \"notes\"\n\
         template_path = \"templates/unique-note.md\"\n\
         date_format = \"YYYY/MM/YYYY-MM-DD-HHmm\"\n\
         date_structure_active = true\n\
         date_structure_side = \"right\"\n\
         metadata_panel_side = \"right\"\n",
    )?;
    write(
        vault_dir.path(),
        "notes/2026/09/2026-09-14-1858.md",
        "---\ntitle: Dated note\nstatus: draft\ntags: []\n---\n\nDated body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let metadata_at = body
        .find("class=\"notes-metadata-panel\"")
        .ok_or("missing metadata panel")?;
    let dated_at = body.find(">Dated<").ok_or("missing Dated label")?;
    let regular_at = body.find(">Regular<").ok_or("missing Regular label")?;
    // `date_structure_side = "right"` puts the dated pane on the right,
    // matching `metadata_panel_side = "right"` - the metadata panel must
    // stack above that same (dated) pane, not the regular one on the left.
    assert!(
        regular_at < metadata_at && metadata_at < dated_at,
        "expected the metadata panel between Regular (left) and Dated (right), stacked above Dated: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn the_metadata_panel_renders_every_real_frontmatter_field_as_an_editable_data_field() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/note.md",
        "---\ntitle: A note\nstatus: draft\ntags: [note, draft]\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/2026/09/note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("data-field=\"title\"") && body.contains("value=\"A note\""),
        "{body}"
    );
    assert!(
        body.contains("data-field=\"status\"") && body.contains("value=\"draft\""),
        "{body}"
    );
    assert!(
        body.contains("data-field=\"tags\"") && body.contains("value=\"note, draft\""),
        "{body}"
    );
    Ok(())
}

/// htmx evaluates an `hx-vals="js:..."` expression as a bare
/// `Function(...)()` call (confirmed against the bundled `htmx.min.js`,
/// never `.call(element, ...)`), so `this` inside it is the global object,
/// not the form: `this.querySelector(...)` throws `TypeError: this.
/// querySelector is not a function` in a real browser, the request never
/// fires, and the save silently does nothing. The metadata form must look
/// its own element up by `id` instead, matching every other fixed control's
/// own save button.
#[tokio::test]
async fn the_metadata_form_reads_its_own_fields_by_id_not_this() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/note.md",
        "---\ntitle: A note\nstatus: draft\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/2026/09/note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("id=\"notes-metadata-form\""), "{body}");
    assert!(
        body.contains("document.getElementById('notes-metadata-form')"),
        "{body}"
    );
    assert!(!body.contains("(this)"), "{body}");
    Ok(())
}

#[tokio::test]
async fn saving_metadata_through_the_route_persists_the_edited_field_and_re_renders_the_screen() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/note.md",
        "---\ntitle: A note\nstatus: draft\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _) = patch(
        &router,
        &format!("/{VAULT_NAME}/notes/2026/09/note.md"),
        r#"{"patch": {"status": "final"}, "expected_hash": null}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("value=\"final\""), "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join("notes/2026/09/note.md"))?;
    assert!(saved.contains("status: final"), "{saved}");
    assert!(saved.contains("Body."), "{saved}");
    Ok(())
}

#[tokio::test]
async fn a_dated_notes_sidebar_row_never_duplicates_its_own_filename_beside_its_clean_date_and_time()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/2026-09-14-1858.md",
        "---\ndate: 2026-09-14\ntime: 18:58\nstatus: draft\ntags: []\n---\n\nUntitled dated note.\n",
    )?;
    write(
        vault_dir.path(),
        "notes/2026/09/2026-09-15-0900.md",
        "---\ntitle: A real title\ndate: 2026-09-15\ntime: 09:00\nstatus: draft\ntags: []\n---\n\nTitled dated note.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // No custom title: the filename-fallback title is never shown a second
    // time beside the clean date/time that already says the same thing.
    assert!(
        !body.contains("class=\"notes-list-item-title\">2026-09-14-1858</span>"),
        "{body}"
    );
    assert!(
        body.contains("class=\"notes-list-item-meta hint mono\">2026-09-14 &middot; 18:58</span>"),
        "{body}"
    );
    // A real custom title is still shown, alongside its own date/time.
    assert!(
        body.contains("class=\"notes-list-item-title\">A real title</span>"),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn disabling_date_structure_active_renders_one_plain_notes_pane_with_no_split() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/notes.toml",
        "notes_dir = \"notes\"\n\
         template_path = \"templates/unique-note.md\"\n\
         date_format = \"YYYY/MM/YYYY-MM-DD-HHmm\"\n\
         date_structure_active = false\n\
         date_structure_side = \"right\"\n",
    )?;
    write(
        vault_dir.path(),
        "notes/2026/09/2026-09-14-1858.md",
        "---\ntitle: Dated note\nstatus: draft\ntags: []\n---\n\nDated body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("notes-layout-split"), "{body}");
    assert!(!body.contains(">Dated<"), "{body}");
    assert!(!body.contains(">Regular<"), "{body}");
    assert!(body.contains("Dated note"), "{body}");
    Ok(())
}
