//! Search results view contract tests: real `contextos-mcp` sessions against real temporary
//! vault fixtures, per `testing.md`'s "no mocking" standard.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};

use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

/// A vault config with `[vault.search] text = true, graph = true`: the search results view
/// needs both the text index (`query_text`/`query_blended`) and the link graph
/// (`query_graph` backlinks), unlike `support::write_vault_config`'s own shared default, which
/// disables both for every other test's speed.
fn write_search_vault_config(dir: &Path, vault_dir: &Path) -> std::io::Result<std::path::PathBuf> {
    let path = dir.join("config.toml");
    #[allow(clippy::unnecessary_debug_formatting)]
    let vault_toml_value = format!("{vault_dir:?}");
    std::fs::write(
        &path,
        format!(
            "[[vault]]\npath = {vault_toml_value}\nname = \"{VAULT_NAME}\"\n\
             state_directory = \".contextos\"\n\
             [vault.search]\ntext = true\ngraph = true\nsemantic = false\n"
        ),
    )?;
    Ok(path)
}

async fn router_over(vault_dir: &Path, config_dir: &Path) -> Result<Router, BoxError> {
    let config_path = write_search_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
    ))
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
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

async fn get_fragment(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let mut headers = HeaderMap::new();
    headers.insert("hx-request", "true".parse()?);
    let mut request = Request::builder().uri(path);
    for (name, value) in &headers {
        request = request.header(name, value);
    }
    let response = router.clone().oneshot(request.body(Body::empty())?).await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

/// Notes written directly to disk before the router connects are never seen by the wikilink
/// graph on their own (it only updates as a side effect of a real write-pipeline mutation) - an
/// explicit rebuild makes a backlinks assertion deterministic, matching
/// `tests/insights.rs`'s own established pattern for the identical reason.
async fn rebuild_graph_index(router: &Router) -> Result<(), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/contextos/query_index_rebuild")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"index":"graph"}"#))?,
        )
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    if status != StatusCode::OK {
        return Err(format!("graph rebuild failed: {status} {}", String::from_utf8_lossy(&body)).into());
    }
    Ok(())
}

#[tokio::test]
async fn the_header_search_box_is_enabled_and_submits_to_the_search_route() -> Result<(), BoxError> {
    let vault_dir_root = tempfile::tempdir()?;
    let vault_dir = support::named_vault_dir(vault_dir_root.path())?;
    let config_dir = tempfile::tempdir()?;
    write(&vault_dir, "index.md", "# Root\n")?;
    let router = router_over(&vault_dir, config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("Search (not yet available)"));
    assert!(body.contains(&format!("action=\"/{VAULT_NAME}/search\"")));
    assert!(body.contains("name=\"q\""));
    Ok(())
}

#[tokio::test]
async fn a_text_mode_search_returns_matching_results_with_title_and_path() -> Result<(), BoxError> {
    let vault_dir_root = tempfile::tempdir()?;
    let vault_dir = support::named_vault_dir(vault_dir_root.path())?;
    let config_dir = tempfile::tempdir()?;
    write(
        &vault_dir,
        "notes/griffin.md",
        "---\ntitle: Griffin husbandry\ndescription: Feeding and housing notes\n---\n# Griffin husbandry\n\nGriffins eat fish.\n",
    )?;
    write(
        &vault_dir,
        "notes/dragon.md",
        "---\ntitle: Dragon lore\n---\n# Dragon lore\n\nDragons hoard gold.\n",
    )?;
    let router = router_over(&vault_dir, config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/search?q=griffin&mode=text")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Griffin husbandry"), "{body}");
    assert!(body.contains("Feeding and housing notes"));
    assert!(body.contains("notes/griffin.md"));
    assert!(!body.contains("Dragon lore"));
    Ok(())
}

#[tokio::test]
async fn an_empty_query_shows_a_prompt_rather_than_calling_the_backend() -> Result<(), BoxError> {
    let vault_dir_root = tempfile::tempdir()?;
    let vault_dir = support::named_vault_dir(vault_dir_root.path())?;
    let config_dir = tempfile::tempdir()?;
    write(&vault_dir, "index.md", "# Root\n")?;
    let router = router_over(&vault_dir, config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/search")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Enter a search term"));
    Ok(())
}

#[tokio::test]
async fn folder_aggregation_counts_match_the_result_set_and_selecting_one_narrows_results() -> Result<(), BoxError> {
    let vault_dir_root = tempfile::tempdir()?;
    let vault_dir = support::named_vault_dir(vault_dir_root.path())?;
    let config_dir = tempfile::tempdir()?;
    write(&vault_dir, "alpha/one.md", "# alpha one\n\nwidget content\n")?;
    write(&vault_dir, "alpha/two.md", "# alpha two\n\nwidget content\n")?;
    write(&vault_dir, "beta/three.md", "# beta three\n\nwidget content\n")?;
    let router = router_over(&vault_dir, config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/search?q=widget&mode=text")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(">alpha "), "{body}");
    assert!(body.contains(">beta "), "{body}");
    assert!(body.contains("(2)"), "alpha's count of 2 should appear: {body}");
    assert!(body.contains("(1)"), "beta's count of 1 should appear: {body}");
    assert!(
        body.contains("include=alpha"),
        "selecting alpha should link to include=alpha: {body}"
    );

    let (status, narrowed) = get(
        &router,
        &format!("/{VAULT_NAME}/search?q=widget&mode=text&include=alpha"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{narrowed}");
    assert!(narrowed.contains("alpha/one.md"));
    assert!(narrowed.contains("alpha/two.md"));
    assert!(!narrowed.contains("beta/three.md"));
    Ok(())
}

/// The entity switcher acts as a folder filter: `zenith` is this fixture vault's own real
/// top-level directory (`routes::entity_docs`'s convention), so `?entity=zenith` must
/// scope results to it exactly as `?include=zenith` would, and an explicit `include` must
/// still win over the entity switcher when both are present.
#[tokio::test]
async fn the_entity_selector_scopes_search_to_that_entitys_folder() -> Result<(), BoxError> {
    let vault_dir_root = tempfile::tempdir()?;
    let vault_dir = support::named_vault_dir(vault_dir_root.path())?;
    let config_dir = tempfile::tempdir()?;
    write(&vault_dir, "zenith/one.md", "# zen one\n\nwidget content\n")?;
    write(&vault_dir, "personal/two.md", "# personal two\n\nwidget content\n")?;
    let router = router_over(&vault_dir, config_dir.path()).await?;

    let (status, scoped) = get(
        &router,
        &format!("/{VAULT_NAME}/search?q=widget&mode=text&entity=zenith"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{scoped}");
    assert!(scoped.contains("zenith/one.md"), "{scoped}");
    assert!(!scoped.contains("personal/two.md"), "{scoped}");

    let (status, overridden) = get(
        &router,
        &format!("/{VAULT_NAME}/search?q=widget&mode=text&entity=zenith&include=personal"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{overridden}");
    assert!(
        overridden.contains("personal/two.md"),
        "an explicit include must win over the entity switcher: {overridden}"
    );
    assert!(!overridden.contains("zenith/one.md"), "{overridden}");
    Ok(())
}

#[tokio::test]
async fn a_malicious_title_and_frontmatter_field_are_rendered_escaped_not_as_markup() -> Result<(), BoxError> {
    let vault_dir_root = tempfile::tempdir()?;
    let vault_dir = support::named_vault_dir(vault_dir_root.path())?;
    let config_dir = tempfile::tempdir()?;
    write(
        &vault_dir,
        "notes/evil.md",
        "---\ntitle: \"<script>alert(1)</script>\"\ndescription: \"<img src=x onerror=alert(2)>\"\n---\n# evil\n\nunique-marker-term\n",
    )?;
    let router = router_over(&vault_dir, config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/search?q=marker&mode=text")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("<script>alert(1)</script>"));
    assert!(
        body.contains("&#60;script&#62;") || body.contains("&lt;script&gt;"),
        "{body}"
    );
    assert!(!body.contains("<img src=x onerror=alert(2)>"));
    Ok(())
}

#[tokio::test]
async fn an_out_of_range_page_size_surfaces_the_stable_error_rather_than_crashing() -> Result<(), BoxError> {
    let vault_dir_root = tempfile::tempdir()?;
    let vault_dir = support::named_vault_dir(vault_dir_root.path())?;
    let config_dir = tempfile::tempdir()?;
    write(&vault_dir, "index.md", "# Root\n")?;
    let router = router_over(&vault_dir, config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/search?q=root&mode=text&limit=500")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("search-error") || body.contains("role=\"alert\""),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn semantic_only_mode_fails_with_a_stable_error_when_semantic_search_is_disabled() -> Result<(), BoxError> {
    let vault_dir_root = tempfile::tempdir()?;
    let vault_dir = support::named_vault_dir(vault_dir_root.path())?;
    let config_dir = tempfile::tempdir()?;
    write(&vault_dir, "index.md", "# Root\n")?;
    let router = router_over(&vault_dir, config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/search?q=root&mode=semantic")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("role=\"alert\""), "{body}");
    Ok(())
}

#[tokio::test]
async fn blended_mode_degrades_to_text_with_a_warning_when_semantic_search_is_disabled() -> Result<(), BoxError> {
    let vault_dir_root = tempfile::tempdir()?;
    let vault_dir = support::named_vault_dir(vault_dir_root.path())?;
    let config_dir = tempfile::tempdir()?;
    write(&vault_dir, "notes/one.md", "# one\n\nfindable-term\n")?;
    let router = router_over(&vault_dir, config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/search?q=findable")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("notes/one.md"));
    assert!(body.contains("search-warnings"), "{body}");
    Ok(())
}

#[tokio::test]
async fn selecting_a_result_populates_its_inbound_and_outbound_link_panels() -> Result<(), BoxError> {
    let vault_dir_root = tempfile::tempdir()?;
    let vault_dir = support::named_vault_dir(vault_dir_root.path())?;
    let config_dir = tempfile::tempdir()?;
    write(
        &vault_dir,
        "notes/hub.md",
        "# hub\n\nlinks-to-target [[notes/target]]\n",
    )?;
    write(&vault_dir, "notes/target.md", "# target\n\nsearchable-marker\n")?;
    let router = router_over(&vault_dir, config_dir.path()).await?;
    rebuild_graph_index(&router).await?;

    let (status, body) = get(
        &router,
        &format!("/{VAULT_NAME}/search?q=searchable&mode=text&selected=notes/target.md"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Inbound links"));
    assert!(
        body.contains("notes/hub.md"),
        "hub should appear as an inbound link: {body}"
    );
    assert!(body.contains("Outbound links"));
    assert!(body.contains("This file has no outbound links."), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_outbound_panel_shows_files_and_broken_links_for_what_they_are() -> Result<(), BoxError> {
    let vault_dir_root = tempfile::tempdir()?;
    let vault_dir = support::named_vault_dir(vault_dir_root.path())?;
    let config_dir = tempfile::tempdir()?;
    write(
        &vault_dir,
        "notes/hub.md",
        "# hub\n\nhub-marker [[notes/target]] ![[diagram.png]] [[vanished]] [[status]] [report](report.pdf)\n",
    )?;
    write(&vault_dir, "notes/target.md", "# target\n")?;
    write(&vault_dir, "assets/diagram.png", "x")?;
    write(&vault_dir, "assets/report.pdf", "x")?;
    write(&vault_dir, "one/status.md", "# one\n")?;
    write(&vault_dir, "two/status.md", "# two\n")?;
    let router = router_over(&vault_dir, config_dir.path()).await?;
    rebuild_graph_index(&router).await?;

    let (status, body) = get(
        &router,
        &format!("/{VAULT_NAME}/search?q=hub-marker&mode=text&selected=notes/hub.md"),
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("href=\"/contract-fixture/assets/diagram.png\""), "{body}");
    assert!(body.contains("href=\"/contract-fixture/assets/report.pdf\""), "{body}");
    assert!(body.contains("vanished"), "{body}");
    assert!(body.contains("not found: no file answers to this link"), "{body}");
    assert!(
        body.contains("ambiguous: more than one file answers to this link (one/status.md, two/status.md)"),
        "{body}"
    );
    assert!(!body.contains("This file has no outbound links."), "{body}");
    assert_eq!(
        body.matches("<span class=\"hint\">(embed)</span>").count(),
        1,
        "only the embedded image is marked as an embed: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn a_file_with_no_links_renders_both_panels_as_explicitly_empty_not_an_error() -> Result<(), BoxError> {
    let vault_dir_root = tempfile::tempdir()?;
    let vault_dir = support::named_vault_dir(vault_dir_root.path())?;
    let config_dir = tempfile::tempdir()?;
    write(&vault_dir, "notes/lonely.md", "# lonely\n\nno-links-marker\n")?;
    let router = router_over(&vault_dir, config_dir.path()).await?;
    rebuild_graph_index(&router).await?;

    let (status, body) = get(
        &router,
        &format!("/{VAULT_NAME}/search?q=lonely&mode=text&selected=notes/lonely.md"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("No notes link to this file."), "{body}");
    assert!(body.contains("This file has no outbound links."), "{body}");
    Ok(())
}

#[tokio::test]
async fn selecting_a_result_via_the_hx_request_fragment_returns_only_the_link_panels() -> Result<(), BoxError> {
    let vault_dir_root = tempfile::tempdir()?;
    let vault_dir = support::named_vault_dir(vault_dir_root.path())?;
    let config_dir = tempfile::tempdir()?;
    write(&vault_dir, "notes/one.md", "# one\n\nfragment-marker\n")?;
    let router = router_over(&vault_dir, config_dir.path()).await?;

    let (status, fragment) = get_fragment(
        &router,
        &format!("/{VAULT_NAME}/search?q=fragment&mode=text&selected=notes/one.md"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{fragment}");
    assert!(
        !fragment.contains("<html"),
        "an hx-request fragment must not carry the full shell: {fragment}"
    );
    assert!(fragment.contains("Inbound links"));
    Ok(())
}

#[tokio::test]
async fn every_control_in_column_three_round_trips_through_the_url() -> Result<(), BoxError> {
    let vault_dir_root = tempfile::tempdir()?;
    let vault_dir = support::named_vault_dir(vault_dir_root.path())?;
    let config_dir = tempfile::tempdir()?;
    write(
        &vault_dir,
        "notes/one.md",
        "---\ntype: task\n---\n# one\n\nroundtrip-marker\n",
    )?;
    let router = router_over(&vault_dir, config_dir.path()).await?;

    let (status, body) = get(
        &router,
        &format!(
            "/{VAULT_NAME}/search?q=roundtrip&mode=text&sort=title&order=asc&limit=5&min_relevance=0.1&note_type=task"
        ),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("value=\"5\""), "page size should round-trip: {body}");
    assert!(
        body.contains("value=\"0.1\""),
        "minimum relevance should round-trip: {body}"
    );
    assert!(body.contains("value=\"task\""), "file type should round-trip: {body}");
    assert!(body.contains("notes/one.md"));
    Ok(())
}

#[tokio::test]
async fn pagination_controls_advance_a_page_and_walking_every_page_covers_every_hit() -> Result<(), BoxError> {
    let vault_dir_root = tempfile::tempdir()?;
    let vault_dir = support::named_vault_dir(vault_dir_root.path())?;
    let config_dir = tempfile::tempdir()?;
    write(
        &vault_dir,
        "notes/aaa.md",
        "---\ntitle: aaa\n---\n# aaa\n\npageable-marker\n",
    )?;
    write(
        &vault_dir,
        "notes/bbb.md",
        "---\ntitle: bbb\n---\n# bbb\n\npageable-marker\n",
    )?;
    let router = router_over(&vault_dir, config_dir.path()).await?;

    let (status, first_page) = get(
        &router,
        &format!("/{VAULT_NAME}/search?q=pageable&mode=text&sort=title&order=asc&limit=1"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{first_page}");
    assert!(first_page.contains("notes/aaa.md"), "{first_page}");
    assert!(!first_page.contains("notes/bbb.md"), "{first_page}");
    assert!(
        first_page.contains(">Next<"),
        "a second page should be reachable: {first_page}"
    );
    assert!(
        !first_page.contains(">Previous<"),
        "the first page has no previous page: {first_page}"
    );

    let (status, second_page) = get(
        &router,
        &format!("/{VAULT_NAME}/search?q=pageable&mode=text&sort=title&order=asc&limit=1&offset=1"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{second_page}");
    assert!(second_page.contains("notes/bbb.md"), "{second_page}");
    assert!(!second_page.contains("notes/aaa.md"), "{second_page}");
    assert!(
        second_page.contains(">Previous<"),
        "the second page should link back: {second_page}"
    );
    assert!(
        !second_page.contains(">Next<"),
        "the second page is the last one: {second_page}"
    );
    Ok(())
}
