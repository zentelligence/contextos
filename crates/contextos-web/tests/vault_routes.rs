//! Vault content route contract tests: real `contextos-mcp` sessions
//! against real temporary vault fixtures, exercising `GET`/`POST`/`PATCH`/
//! `PUT`/`DELETE /{{vault_name}}/{{relative-path}}` end to end. No mocking,
//! per `testing.md`.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

async fn router_over(vault_dir: &Path, config_dir: &Path) -> Result<Router, BoxError> {
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
    ))
}

async fn get(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

async fn request(router: &Router, method: &str, path: &str, body: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

// ---------------------------------------------------------------------
// 404
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_unconfigured_vault_name_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = get(&router, "/does-not-exist/some-note.md").await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn a_nonexistent_path_in_a_real_vault_is_a_404_with_no_listing_fallback() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/does-not-exist.md")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.to_ascii_lowercase().contains("index of"));
    Ok(())
}

// ---------------------------------------------------------------------
// Directory route
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_folder_no_section_owns_is_not_shown_and_neither_is_its_index() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Vault root\n\nWelcome.\n")?;
    write(
        vault_dir.path(),
        "docs/index.md",
        "# Docs directory\n\nSome operator prose.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    for path in ["docs/", "docs/index.md"] {
        let (status, body) = get(&router, &format!("/{VAULT_NAME}/{path}")).await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert!(!body.contains("Some operator prose."), "{path}");
    }
    Ok(())
}

#[tokio::test]
async fn the_vault_root_dashboard_never_renders_its_own_index_md_body() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    // Unlike every other directory, the vault root is the Command Centre
    // dashboard: its own view is the Task widget strip, never a fall
    // through to `index.md`'s rendered body.
    write(vault_dir.path(), "index.md", "# Vault root\n\nWelcome.\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("dashboard-task-widgets"));
    assert!(!body.contains("Vault root"));
    assert!(!body.contains("Welcome."));
    Ok(())
}

#[tokio::test]
async fn the_vault_root_renders_the_overview_dashboard_with_every_panel() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Vault root\n\nWelcome.\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // A discrete dashboard page, not the vault root's index note: the Task lifecycle strip renders with the page...
    assert!(body.contains("vault-overview"));
    assert!(body.contains("Task lifecycle"));
    assert!(body.contains("dashboard-task-widgets"));
    // ...beside every other panel, each loaded from the vault as its own region rather than shown as a placeholder.
    for heading in [
        "Bridge",
        "System status",
        "Needs a call from you",
        "Alert rail",
        "Inputs",
    ] {
        assert!(body.contains(heading), "missing panel: {heading}");
    }
    assert!(!body.contains("Not yet available"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_bare_folder_path_goes_where_its_trailing_slash_form_does() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/topics/one.md", "# One\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let slash = see_other(&router, &format!("/{VAULT_NAME}/memory/wiki/topics/")).await?;
    let bare = see_other(&router, &format!("/{VAULT_NAME}/memory/wiki/topics")).await?;
    assert_eq!(slash.as_deref(), Some("/contract-fixture/wiki/"));
    assert_eq!(bare, slash);
    Ok(())
}

/// The `Location` of a `303 See Other` answer to `path`, or `None` for any other answer.
async fn see_other(router: &Router, path: &str) -> Result<Option<String>, BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    Ok((response.status() == StatusCode::SEE_OTHER)
        .then(|| {
            response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        })
        .flatten())
}

#[tokio::test]
async fn a_bare_vault_root_with_no_trailing_slash_renders_the_same_as_its_trailing_slash_form() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status_slash, body_slash) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    let (status_bare, body_bare) = get(&router, &format!("/{VAULT_NAME}")).await?;
    assert_eq!(status_slash, StatusCode::OK);
    assert_eq!(status_bare, StatusCode::OK);
    assert_eq!(body_slash, body_bare);
    Ok(())
}

#[tokio::test]
async fn a_nested_page_files_breadcrumb_links_each_ancestor_to_its_screens_folder() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/boards/plan.canvas",
        "{\"nodes\":[],\"edges\":[]}",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/boards/plan.canvas")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The board is shown on the wiki screen, so each ancestor is that screen's own folder.
    assert!(!body.contains(&format!("href=\"/{VAULT_NAME}/memory/\"")), "{body}");
    assert!(
        body.contains(&format!("<a href=\"/{VAULT_NAME}/wiki/\">wiki</a>")),
        "{body}"
    );
    assert!(
        body.contains(&format!("<a href=\"/{VAULT_NAME}/wiki/boards/\">boards</a>")),
        "{body}"
    );
    assert!(
        body.contains("<span class=\"breadcrumb-current\">plan.canvas</span>"),
        "{body}"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// Bare HTTP server root
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_bare_server_root_redirects_to_the_first_configured_vault() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let response = router.oneshot(Request::builder().uri("/").body(Body::empty())?).await?;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let Some(location) = response.headers().get("location") else {
        return Err("a redirect carries a Location header".into());
    };
    assert_eq!(location, &format!("/{VAULT_NAME}/"));
    Ok(())
}

/// A real `contextos` process started against a `config.toml` naming zero vaults: the whole pipeline this exercises
/// (real MCP handshake against a real zero-vault server, `contextos-web`'s own routing over that connection) is
/// what a fresh install, before any vault has been added yet, actually looks like.
#[tokio::test]
async fn the_bare_server_root_redirects_to_settings_when_no_vault_is_configured_at_all() -> Result<(), BoxError> {
    let config_dir = tempfile::tempdir()?;
    let config_path = support::write_zero_vault_config(config_dir.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    let router = contextos_web::build_router(clients, Some(config_dir.path()), &config_dir.path().join("web.toml"));

    let response = router.oneshot(Request::builder().uri("/").body(Body::empty())?).await?;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let Some(location) = response.headers().get("location") else {
        return Err("a redirect carries a Location header".into());
    };
    assert_eq!(location, "/settings/");
    Ok(())
}

// ---------------------------------------------------------------------
// Markdown rendering: wikilinks, embeds, fences, callouts
// ---------------------------------------------------------------------

#[tokio::test]
async fn wikilink_resolution_covers_live_dead_and_doubly_nested_embed() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/target-note.md",
        "# Target\n\nThis is the target note.\n",
    )?;
    write(
        vault_dir.path(),
        "memory/wiki/third-file.md",
        "# Third\n\nThird-file content.\n",
    )?;
    write(
        vault_dir.path(),
        "memory/wiki/embed-target.md",
        "# Embed target\n\n![[third-file]]\n",
    )?;
    write(
        vault_dir.path(),
        "memory/wiki/example-note.md",
        "# Example\n\n\
         See [[target-note]] for a live link.\n\n\
         See [[does-not-exist]] for a dead link.\n\n\
         ![[embed-target]]\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/example-note.md")).await?;
    assert_eq!(status, StatusCode::OK);

    // Live link resolves to the target's own screen. The fixtures live in the wiki folder because a note no screen
    // owns has no route.
    assert!(body.contains(&format!("href=\"/{VAULT_NAME}/wiki/target-note.md\"")));
    assert!(body.contains("class=\"wikilink\""));

    // Dead link renders as a visually distinct span, never a broken anchor.
    assert!(body.contains("wikilink dead"));
    assert!(body.contains("data-target=\"does-not-exist\""));

    // The embed inlines embed-target's own content...
    assert!(body.contains("Embed target"));
    // ...and the doubly-nested embed inside it (embed-target embeds
    // third-file) renders as a plain link at that second level, not a
    // second recursive inline.
    assert!(
        body.contains(&format!("href=\"/{VAULT_NAME}/wiki/third-file.md\"")),
        "{body}"
    );
    assert!(!body.contains("Third-file content."));
    Ok(())
}

#[tokio::test]
async fn a_heading_reference_link_resolves_to_the_targets_own_slugged_heading_id() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/target-note.md",
        "# Target\n\n## Some Heading\n\nDetail.\n",
    )?;
    write(
        vault_dir.path(),
        "memory/wiki/note.md",
        "# Note\n\nSee [[target-note#Some Heading]].\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(&format!("href=\"/{VAULT_NAME}/wiki/target-note.md#some-heading\"")));

    let (status, target_body) = get(&router, &format!("/{VAULT_NAME}/wiki/target-note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{target_body}");
    assert!(target_body.contains("<h2 id=\"some-heading\">Some Heading</h2>"));
    Ok(())
}

#[tokio::test]
async fn a_block_reference_link_resolves_to_an_invisible_anchor_at_the_blocks_own_position() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/target-note.md",
        "# Target\n\nThis paragraph can be linked to. ^my-block-id\n",
    )?;
    write(
        vault_dir.path(),
        "memory/wiki/note.md",
        "# Note\n\nSee [[target-note#^my-block-id]].\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // A block's anchor is `^` and its id, so it can never be mistaken for a heading's; the `^` is encoded in the URL.
    assert!(body.contains(&format!("href=\"/{VAULT_NAME}/wiki/target-note.md#%5Emy-block-id\"")));

    let (status, target_body) = get(&router, &format!("/{VAULT_NAME}/wiki/target-note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{target_body}");
    assert!(target_body.contains("<span class=\"block-anchor\" id=\"^my-block-id\"></span>"));
    Ok(())
}

#[tokio::test]
async fn a_base_embed_renders_its_own_card_grid_not_a_raw_yaml_dump() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/tasks.base",
        "views:\n  - type: table\n    name: \"Active\"\n    order:\n      - file.path\n      - status\n",
    )?;
    write(
        vault_dir.path(),
        "memory/wiki/task-one.md",
        "---\nstatus: active\n---\n# Task one\n",
    )?;
    write(vault_dir.path(), "memory/wiki/note.md", "# Note\n\n![[tasks.base]]\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // `task-one.md` is a wiki note, so its card opens it on the wiki screen.
    assert!(
        body.contains(&format!("href=\"/{VAULT_NAME}/wiki/task-one.md\"")),
        "{body}"
    );
    assert!(body.contains("active"));
    assert!(
        !body.contains("views:"),
        "the raw YAML definition must not leak into the embed"
    );
    Ok(())
}

#[tokio::test]
async fn a_block_reference_embed_inlines_only_the_referenced_block() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/target-note.md",
        "# Target\n\n\
         - Item 1\n\
         - Item 2\n\
         - Item 3\n\n\
         ^list-id\n\n\
         This paragraph is not part of the referenced block.\n",
    )?;
    write(
        vault_dir.path(),
        "memory/wiki/note.md",
        "# Note\n\n![[target-note#^list-id]]\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Item 1"));
    assert!(body.contains("Item 2"));
    assert!(body.contains("Item 3"));
    assert!(!body.contains("not part of the referenced block"));
    Ok(())
}

#[tokio::test]
async fn an_image_embed_renders_as_an_img_tag_not_a_dead_link() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    // Content need not be a real JPEG: MIME detection falls back to the
    // extension, and the embed path never decodes the bytes at all.
    write(vault_dir.path(), "memory/wiki/photo.jpg", "not-a-real-jpeg")?;
    write(vault_dir.path(), "memory/wiki/note.md", "# Note\n\n![[photo.jpg]]\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(&format!(
        "<img class=\"embed-image\" src=\"/{VAULT_NAME}/memory/wiki/photo.jpg\""
    )));
    assert!(!body.contains("wikilink dead"));
    Ok(())
}

#[tokio::test]
async fn an_image_embeds_size_hint_becomes_width_and_height_attributes() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/photo.jpg", "not-a-real-jpeg")?;
    write(
        vault_dir.path(),
        "memory/wiki/note.md",
        "# Note\n\n![[photo.jpg|640x480]]\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("width=\"640\" height=\"480\""));
    Ok(())
}

#[tokio::test]
async fn audio_and_pdf_embeds_render_as_media_not_dead_links() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/clip.mp3", "not-a-real-mp3")?;
    write(vault_dir.path(), "memory/wiki/handout.pdf", "not-a-real-pdf")?;
    write(
        vault_dir.path(),
        "memory/wiki/note.md",
        "# Note\n\n![[clip.mp3]]\n\n![[handout.pdf]]\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(&format!(
        "<audio class=\"embed-audio\" controls src=\"/{VAULT_NAME}/memory/wiki/clip.mp3\""
    )));
    // A PDF opens in the browser's own viewer, in a frame.
    assert!(body.contains(&format!(
        "<iframe class=\"embed-pdf\" src=\"/{VAULT_NAME}/memory/wiki/handout.pdf\" title=\"handout\"></iframe>"
    )));
    assert!(!body.contains("wikilink dead"));
    Ok(())
}

#[tokio::test]
async fn ofm_extensions_render_together_comments_highlight_tags_and_a_callout_alias() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/note.md",
        "# Note\n\n\
         Visible %%but this stays hidden%% text.\n\n\
         This is ==important==.\n\n\
         Tagged as #project today.\n\n\
         > [!hint] Did you know\n\
         > A hint is styled as a tip.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Visible"));
    assert!(!body.contains("but this stays hidden"));
    assert!(body.contains("<mark>important</mark>"));
    assert!(body.contains("<span class=\"tag\">#project</span>"));
    // The documented alias ("hint") is styled as the type it stands for
    // ("tip") but keeps its own literal kind for the default title.
    assert!(body.contains("class=\"callout callout-tip\""));
    assert!(body.contains("data-callout=\"hint\""));
    Ok(())
}

#[tokio::test]
async fn triple_colon_fences_and_callouts_render_with_raw_source_available() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/note.md",
        "# Note\n\n\
         :::warning\nThis may break things.\n:::\n\n\
         :::mystery-fence\nUnrecognised body.\n:::\n\n\
         > [!tip] Handy hint\n> Do this instead.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/note.md")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("fence-recognised"));
    assert!(body.contains("This may break things."));
    assert!(body.contains("fence-unrecognised"));
    assert!(body.contains("mystery-fence"));
    assert!(body.contains("callout-tip"));
    assert!(body.contains("Handy hint"));

    let (status, raw) = get(&router, &format!("/{VAULT_NAME}/memory/wiki/note.md?raw=1")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(raw.contains(":::warning"));
    assert!(raw.contains("> [!tip]"));
    Ok(())
}

// ---------------------------------------------------------------------
// Mutation-method dispatch on `.md` and generic files
// ---------------------------------------------------------------------

#[tokio::test]
async fn patching_a_notes_frontmatter_dispatches_frontmatter_update() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "note.md", "---\nstatus: pending\n---\n# Note\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = request(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/note.md"),
        r#"{"patch": {"status": "active"}}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let content = std::fs::read_to_string(vault_dir.path().join("note.md"))?;
    assert!(content.contains("status: active"));
    Ok(())
}

#[tokio::test]
async fn posting_to_a_note_route_has_no_defined_target_and_is_405() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "note.md", "# Note\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = request(&router, "POST", &format!("/{VAULT_NAME}/note.md"), "{}").await?;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    Ok(())
}

#[tokio::test]
async fn deleting_any_file_dispatches_fs_delete_file() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "note.md", "# Note\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = request(&router, "DELETE", &format!("/{VAULT_NAME}/note.md"), "").await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!vault_dir.path().join("note.md").exists());
    Ok(())
}

// ---------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------

#[tokio::test]
async fn rendering_the_same_note_twice_produces_byte_identical_html() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "target-note.md", "# Target\n\nContent.\n")?;
    write(
        vault_dir.path(),
        "note.md",
        "# Note\n\n\
         [[target-note]] and [[missing-note]].\n\n\
         :::note\nBody.\n:::\n\n\
         > [!info] Info\n> Detail.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, first) = get(&router, &format!("/{VAULT_NAME}/note.md")).await?;
    let (_, second) = get(&router, &format!("/{VAULT_NAME}/note.md")).await?;
    assert_eq!(first, second);
    Ok(())
}

// ---------------------------------------------------------------------
// Truncated reads retry through fs_attach_file (`fs_read_text_file` caps
// out at 5 KiB by default)
// ---------------------------------------------------------------------

/// Content past `fs_read_text_file`'s default 5 KiB threshold, ending in a
/// marker only present past that point: proves a page render (or embed)
/// carries the full content rather than a silently truncated preview.
fn oversized_note_body() -> String {
    let mut body = "# Big note\n\n".to_owned();
    body.push_str(&"Lorem ipsum filler line.\n".repeat(300));
    body.push_str("UNIQUE-TAIL-MARKER-XYZ\n");
    body
}

#[tokio::test]
async fn a_note_past_the_read_text_threshold_still_renders_its_full_content() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/big-note.md", &oversized_note_body())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/big-note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("UNIQUE-TAIL-MARKER-XYZ"));
    Ok(())
}

#[tokio::test]
async fn an_embed_of_a_note_past_the_read_text_threshold_still_inlines_its_full_content() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/big-target.md", &oversized_note_body())?;
    write(vault_dir.path(), "memory/wiki/note.md", "# Note\n\n![[big-target]]\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("UNIQUE-TAIL-MARKER-XYZ"));
    Ok(())
}

#[tokio::test]
async fn a_vault_svg_opened_directly_cannot_run_script_in_the_interface() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/evil.svg",
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><script>fetch('/mcp/contextos/fs_delete_file')</script></svg>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/{VAULT_NAME}/memory/wiki/evil.svg"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    assert_eq!(header("x-content-type-options").as_deref(), Some("nosniff"));
    let policy = header("content-security-policy").unwrap_or_default();
    assert!(
        policy.starts_with("sandbox;") && policy.contains("default-src 'none'"),
        "{policy}"
    );
    Ok(())
}

#[tokio::test]
async fn script_written_in_a_note_never_reaches_the_rendered_page() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/clipped.md",
        "# Clipped\n\n<img src=\"x\" onerror=\"alert(1)\">\n\n<script>alert(2)</script>\n\n\
         [click](javascript:alert(3)) and <a href=\"jav&#97;script:alert(4)\">this</a> ==<b onclick=\"x\">bold</b>==\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/clipped.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rendered = body
        .split("<div class=\"wiki-rendered\">")
        .nth(1)
        .and_then(|rest| rest.split("</article>").next())
        .unwrap_or_default();
    for forbidden in [
        "onerror",
        "alert(2)",
        "href=\"javascript",
        "src=\"javascript",
        "onclick",
        "<script",
    ] {
        assert!(!rendered.contains(forbidden), "{forbidden} in {rendered}");
    }
    // The link that named a script address is shown as unavailable, its text kept only as inert data.
    assert!(
        rendered.contains("<span class=\"internal-link dead\" data-target=\"javascript:alert(3)\">click</span>"),
        "{rendered}"
    );
    assert!(rendered.contains("<mark><b>bold</b></mark>"), "{rendered}");
    Ok(())
}
