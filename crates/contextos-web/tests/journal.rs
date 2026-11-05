//! Journal screen contract tests: real `contextos-mcp` sessions against real
//! temporary vault fixtures. No mocking, per `testing.md`.

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

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

fn today() -> String {
    contextos_web::journal::format_date(time::OffsetDateTime::now_utc().date())
}

/// An operator's own template: nothing about it is known to the server.
const TEMPLATE: &str = "---\ndate: <% tp.date.now(\"YYYY-MM-DD\") %>\ntags: [daily]\n---\n\n\
    # <% tp.date.now(\"dddd, Do MMMM YYYY\") %>\n\n\
    ## Gym\n\n> What I lifted.\n\n<!-- sets and reps -->\n\n---\n\n\
    ## Reading\n\n> Pages read today.\n\n- \n\n---\n\n\
    ## New actions\n\n- [ ] \n\n---\n\n\
    ## Agent log\n\n> Appended by agents.\n";

fn seed_template(vault_dir: &Path) -> std::io::Result<()> {
    write(vault_dir, "templates/daily-journal.md", TEMPLATE)
}

fn read(dir: &Path, relative: &str) -> std::io::Result<String> {
    std::fs::read_to_string(dir.join(relative))
}

async fn hx_get(router: &Router, path: &str) -> Result<(StatusCode, String, axum::http::HeaderMap), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("hx-request", "true")
                .body(Body::empty())?,
        )
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned(), headers))
}

async fn hx_patch(
    router: &Router,
    path: &str,
    json_body: &str,
) -> Result<(StatusCode, String, axum::http::HeaderMap), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(path)
                .header("hx-request", "true")
                .header("content-type", "application/json")
                .body(Body::from(json_body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned(), headers))
}

/// The `value` of the form field named `name` in an edit form.
fn field_value(body: &str, name: &str) -> Option<String> {
    let marker = format!("name=\"{name}\" value=\"");
    let start = body.find(&marker)? + marker.len();
    let end = body[start..].find('"')? + start;
    Some(
        body[start..end]
            .replace("&amp;", "&")
            .replace("&#x27;", "'")
            .replace("&quot;", "\""),
    )
}

/// A save request body, as the edit form's JSON encoding sends it.
fn save_body(heading: &str, base: &str, content: &str) -> Result<String, BoxError> {
    Ok(serde_json::to_string(
        &serde_json::json!({ "heading": heading, "base": base, "content": content }),
    )?)
}

/// Opens section `index` of `date` for editing and returns the heading and base its form carries.
async fn open_editor(router: &Router, date: &str, index: usize) -> Result<(String, String), BoxError> {
    let (status, body, _) = hx_get(router, &format!("/{VAULT_NAME}/journal/{date}/sections/{index}/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let heading = field_value(&body, "heading").ok_or("the edit form names its heading")?;
    let base = field_value(&body, "base").ok_or("the edit form carries its base version")?;
    Ok((heading, base))
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/journal and /{{vault_name}}/journal/ (bare root)
// ---------------------------------------------------------------------

/// A redirect response's own `Location` header, matching
/// `tests/entity_docs.rs`'s identical "read the real header, not the body"
/// contract for a full-navigation redirect.
fn location(headers: &axum::http::HeaderMap) -> Option<String> {
    headers.get("location")?.to_str().ok().map(str::to_owned)
}

async fn get_with_headers(
    router: &Router,
    path: &str,
) -> Result<(StatusCode, String, axum::http::HeaderMap), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned(), headers))
}

#[tokio::test]
async fn get_on_the_bare_root_redirects_to_todays_own_date() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = get_with_headers(&router, &format!("/{VAULT_NAME}/journal")).await?;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    let target = location(&headers).ok_or("expected a Location header")?;
    assert_eq!(target, format!("/{VAULT_NAME}/journal/{}", today()));

    // The trailing-slash form redirects identically.
    let (status, body, headers) = get_with_headers(&router, &format!("/{VAULT_NAME}/journal/")).await?;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    let target = location(&headers).ok_or("expected a Location header")?;
    assert_eq!(target, format!("/{VAULT_NAME}/journal/{}", today()));
    Ok(())
}

#[tokio::test]
async fn an_hx_request_on_the_bare_root_gets_an_hx_redirect_not_a_real_3xx() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // Both the bare and trailing-slash forms of the root resolve to the
    // identical `get_root` handler, so both get the identical `HX-Redirect`
    // treatment.
    for path in [format!("/{VAULT_NAME}/journal"), format!("/{VAULT_NAME}/journal/")] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&path)
                    .header("hx-request", "true")
                    .body(Body::empty())?,
            )
            .await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let body = String::from_utf8_lossy(&body).into_owned();
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        assert_eq!(body, "", "{path}: an HX-Redirect response carries no body of its own");
        let target = headers
            .get("HX-Redirect")
            .ok_or("expected an HX-Redirect header")?
            .to_str()?;
        assert_eq!(target, format!("/{VAULT_NAME}/journal/{}", today()), "{path}");
    }
    Ok(())
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/journal/{{date}}
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_new_days_tabs_are_the_templates_sections_in_its_order_with_its_guidance() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_template(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/journal/2021-03-04")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let order: Vec<usize> = [
        "data-pane=\"0\">Gym<",
        "data-pane=\"1\">Reading<",
        "data-pane=\"2\">New actions<",
        "data-pane=\"3\">Agent log<",
    ]
    .iter()
    .map(|tab| body.find(tab).ok_or(format!("missing tab {tab}")))
    .collect::<Result<_, _>>()?;
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "tabs follow the template's order: {body}"
    );
    assert!(
        body.contains("What I lifted."),
        "the template's guidance is shown: {body}"
    );
    // Nothing of any other journal structure is shown.
    for absent in [
        "Morning intention",
        "Loops opened",
        "Evening Reflection",
        "Activity Log",
        "Wins &amp;",
    ] {
        assert!(!body.contains(absent), "{absent}: {body}");
    }
    // Viewing a day creates nothing.
    assert!(!vault_dir.path().join("journal/2021/03/2021-03-04.md").exists());
    Ok(())
}

#[tokio::test]
async fn an_existing_entrys_tabs_are_its_own_sections_not_the_templates() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_template(vault_dir.path())?;
    write(
        vault_dir.path(),
        "journal/2020/01/2020-01-01.md",
        "---\ndate: 2020-01-01\n---\n\n# Old day\n\n## Alpha\n\n- Ship the Journal screen\n\n\
         ## Beta\n\n- [reference](https://example.com/reference)\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/journal/2020-01-01")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("data-pane=\"0\">Alpha<"), "{body}");
    assert!(body.contains("data-pane=\"1\">Beta<"), "{body}");
    assert!(!body.contains(">Gym<"), "{body}");
    assert!(body.contains("Ship the Journal screen"), "{body}");
    // A Markdown link is rendered as a link.
    assert!(body.contains("href=\"https://example.com/reference\""), "{body}");
    Ok(())
}

#[tokio::test]
async fn an_entry_with_no_sections_is_one_editable_tab() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "journal/2020/01/2020-01-01.md",
        "# Old day\n\nJust prose.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/journal/2020-01-01")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("data-pane=\"0\">Entry<"), "{body}");
    assert!(body.contains("Just prose."), "{body}");
    assert!(body.contains("journal/2020-01-01/sections/0/edit"), "{body}");
    Ok(())
}

#[tokio::test]
async fn every_day_offers_each_section_for_editing_except_configured_read_only_ones() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_template(vault_dir.path())?;
    write(
        vault_dir.path(),
        ".contextos/journal.toml",
        "read_only_sections = [\"agent LOG\"]\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // A past day, a future day: neither is any different from today.
    for date in ["2020-01-01", "2099-12-31"] {
        let (status, body) = get(&router, &format!("/{VAULT_NAME}/journal/{date}")).await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        for index in 0..3 {
            assert!(
                body.contains(&format!("journal/{date}/sections/{index}/edit")),
                "{date} {index}: {body}"
            );
        }
        assert!(
            !body.contains(&format!("journal/{date}/sections/3/edit")),
            "{date}: {body}"
        );
    }
    let (status, _body, _) = hx_get(&router, &format!("/{VAULT_NAME}/journal/2020-01-01/sections/3/edit")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND, "a read-only section has no edit form");
    Ok(())
}

#[tokio::test]
async fn the_sidebar_tree_lists_a_real_entry_and_always_includes_today() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "journal/2020/01/2020-01-01.md", "# Old day\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/journal/{}", today())).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("2020-01-01"), "{body}");
    assert!(body.contains(&today()), "{body}");
    // Day links are path segments, never a `?date=` query parameter.
    assert!(body.contains(&format!("/{VAULT_NAME}/journal/2020-01-01\"")), "{body}");
    assert!(!body.contains("journal/?date="), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_vault_configured_journal_directory_is_honoured() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/journal.toml", "journal_dir = \"diary\"\n")?;
    write(vault_dir.path(), "diary/2020/01/2020-01-01.md", "# Old day\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/journal/{}", today())).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("2020-01-01"), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_day_views_own_prev_and_next_links_are_real_date_path_segments() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/journal/2020-01-15")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(&format!("/{VAULT_NAME}/journal/2020-01-14")), "{body}");
    assert!(body.contains(&format!("/{VAULT_NAME}/journal/2020-01-16")), "{body}");
    assert!(!body.contains("journal/?date="), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_date_segment_that_is_not_a_real_calendar_date_is_a_not_found() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    for path in [
        "not-a-date",
        "2026-02-30",
        "2026-02-30/sections/0",
        "2026-02-30/sections/0/edit",
        "2020-01-01/sections/abc",
        "2020-01-01/sections/01/edit",
    ] {
        let (status, _body) = get(&router, &format!("/{VAULT_NAME}/journal/{path}")).await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
    Ok(())
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/journal/{{date}}/sections/{{index}}[/edit]
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_sections_edit_form_holds_its_markdown_without_the_templates_guidance() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_template(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _) = hx_get(&router, &format!("/{VAULT_NAME}/journal/2021-03-04/sections/1/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("<html"), "an HTMX request gets a fragment: {body}");
    assert!(
        body.contains("hx-patch=\"/contract-fixture/journal/2021-03-04/sections/1\""),
        "{body}"
    );
    assert_eq!(field_value(&body, "heading").as_deref(), Some("Reading"));
    assert!(
        field_value(&body, "base").is_some_and(|base| base.len() == 64),
        "{body}"
    );
    let textarea = body
        .split("<textarea")
        .nth(1)
        .and_then(|rest| rest.split("</textarea>").next())
        .ok_or("an edit form has a textarea")?;
    assert!(
        textarea.ends_with(">-"),
        "the textarea holds the section's own content: {textarea}"
    );
    assert!(
        body.contains("Pages read today."),
        "the guidance is shown beside it: {body}"
    );

    // Cancelling returns the section's read view; a full navigation gets a whole page.
    let (status, body, _) = hx_get(&router, &format!("/{VAULT_NAME}/journal/2021-03-04/sections/1")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("journal/2021-03-04/sections/1/edit"), "{body}");
    let (status, page) = get(&router, &format!("/{VAULT_NAME}/journal/2021-03-04/sections/1/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("<html"), "{page}");
    assert!(page.contains("<textarea"), "{page}");

    let (status, _body, _) = hx_get(&router, &format!("/{VAULT_NAME}/journal/2021-03-04/sections/9/edit")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

// ---------------------------------------------------------------------
// PATCH /{{vault_name}}/journal/{{date}}/sections/{{index}}
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_first_save_on_a_new_day_creates_the_entry_as_the_template_with_that_section_filled() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_template(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (heading, base) = open_editor(&router, "2021-03-04", 0).await?;
    let (status, body, _) = hx_patch(
        &router,
        &format!("/{VAULT_NAME}/journal/2021-03-04/sections/0"),
        &save_body(&heading, &base, "- Squats 5x5")?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Squats 5x5"), "{body}");

    let saved = read(vault_dir.path(), "journal/2021/03/2021-03-04.md")?;
    let expected = "---\ndate: 2021-03-04\ntags: [daily]\n---\n\n# Thursday, 4th March 2021\n\n\
        ## Gym\n\n> What I lifted.\n\n<!-- sets and reps -->\n\n- Squats 5x5\n\n---\n\n\
        ## Reading\n\n> Pages read today.\n\n- \n\n---\n\n\
        ## New actions\n\n- [ ] \n\n---\n\n\
        ## Agent log\n\n> Appended by agents.\n";
    assert_eq!(saved, expected);
    Ok(())
}

/// An entry with content the old fixed-structure editor lost: prose before a
/// sub-label, an extra sub-label, nested items, ticked boxes, a code block
/// holding a `## ` line, and a section no template names.
const RICH_ENTRY: &str = "---\ndate: 2020-01-01\n---\n\n# Old day\n\n<< [[2019-12-31]] | [[2020-01-02]] >>\n\n\
    ## Gym\n\n> What I lifted.\n\n<!-- sets and reps -->\n\n- [x] Squats\n  - 5x5 at 100kg\n\n---\n\n\
    ## Evening Reflection\n\nPreamble prose.\n\n**Wins & Successes**\n\n- [x] won\n\n**Energy**\n\n7/10\n\n\
    ```sh\n## not a heading\n```\n\n---\n\n\
    ## Reading\n\n> Pages read today.\n\n- Chapter 3\n\n---\n";

#[tokio::test]
async fn saving_one_section_leaves_every_other_byte_of_the_entry_exactly_as_it_was() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_template(vault_dir.path())?;
    write(vault_dir.path(), "journal/2020/01/2020-01-01.md", RICH_ENTRY)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (heading, base) = open_editor(&router, "2020-01-01", 2).await?;
    assert_eq!(heading, "Reading");
    let (status, body, _) = hx_patch(
        &router,
        &format!("/{VAULT_NAME}/journal/2020-01-01/sections/2"),
        &save_body(&heading, &base, "- Chapter 3\n- Chapter 4")?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let saved = read(vault_dir.path(), "journal/2020/01/2020-01-01.md")?;
    assert_eq!(saved, RICH_ENTRY.replace("- Chapter 3\n", "- Chapter 3\n- Chapter 4\n"));

    // Saving a section unchanged writes nothing different, however often.
    let (heading, base) = open_editor(&router, "2020-01-01", 1).await?;
    let (_, edit_form, _) = hx_get(&router, &format!("/{VAULT_NAME}/journal/2020-01-01/sections/1/edit")).await?;
    assert!(edit_form.contains("Preamble prose."), "{edit_form}");
    let unchanged = "Preamble prose.\n\n**Wins & Successes**\n\n- [x] won\n\n**Energy**\n\n7/10\n\n\
                     ```sh\n## not a heading\n```\n\n---";
    for _ in 0..2 {
        let (status, body, _) = hx_patch(
            &router,
            &format!("/{VAULT_NAME}/journal/2020-01-01/sections/1"),
            &save_body(&heading, &base, unchanged)?,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    assert_eq!(read(vault_dir.path(), "journal/2020/01/2020-01-01.md")?, saved);
    Ok(())
}

#[tokio::test]
async fn a_save_from_an_editor_opened_before_that_section_changed_is_refused_and_keeps_the_typed_text()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_template(vault_dir.path())?;
    write(vault_dir.path(), "journal/2020/01/2020-01-01.md", RICH_ENTRY)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (heading, base) = open_editor(&router, "2020-01-01", 2).await?;
    let changed = RICH_ENTRY.replace("- Chapter 3\n", "- Chapter 3, written elsewhere\n");
    write(vault_dir.path(), "journal/2020/01/2020-01-01.md", &changed)?;

    let (status, body, _) = hx_patch(
        &router,
        &format!("/{VAULT_NAME}/journal/2020-01-01/sections/2"),
        &save_body(&heading, &base, "- My unsaved words")?,
    )
    .await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "a refused form save re-renders the form: {body}"
    );
    assert!(body.contains("changed since you opened it"), "{body}");
    assert!(body.contains("My unsaved words"), "the typed text is kept: {body}");
    assert!(
        body.contains("written elsewhere"),
        "the current content is shown: {body}"
    );
    assert_eq!(read(vault_dir.path(), "journal/2020/01/2020-01-01.md")?, changed);

    // The re-rendered form carries the current version, so saving again replaces it deliberately.
    let rebased = field_value(&body, "base").ok_or("the form carries the current version")?;
    let (status, body, _) = hx_patch(
        &router,
        &format!("/{VAULT_NAME}/journal/2020-01-01/sections/2"),
        &save_body(&heading, &rebased, "- My unsaved words")?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(read(vault_dir.path(), "journal/2020/01/2020-01-01.md")?.contains("- My unsaved words\n"));
    Ok(())
}

#[tokio::test]
async fn a_change_to_another_section_since_the_editor_opened_does_not_refuse_the_save() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_template(vault_dir.path())?;
    write(vault_dir.path(), "journal/2020/01/2020-01-01.md", RICH_ENTRY)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (heading, base) = open_editor(&router, "2020-01-01", 2).await?;
    let appended = RICH_ENTRY.replace("- [x] Squats\n", "- [x] Squats\n- Deadlifts, added by an agent\n");
    write(vault_dir.path(), "journal/2020/01/2020-01-01.md", &appended)?;

    let (status, body, _) = hx_patch(
        &router,
        &format!("/{VAULT_NAME}/journal/2020-01-01/sections/2"),
        &save_body(&heading, &base, "- Chapter 4")?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        read(vault_dir.path(), "journal/2020/01/2020-01-01.md")?,
        appended.replace("- Chapter 3\n", "- Chapter 4\n")
    );
    Ok(())
}

#[tokio::test]
async fn a_save_naming_a_heading_no_longer_at_that_place_is_refused() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "journal/2020/01/2020-01-01.md", RICH_ENTRY)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_heading, base) = open_editor(&router, "2020-01-01", 2).await?;
    let (status, body, _) = hx_patch(
        &router,
        &format!("/{VAULT_NAME}/journal/2020-01-01/sections/2"),
        &save_body("Gym", &base, "- overwritten")?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("sections changed"), "{body}");
    assert!(body.contains("overwritten"), "the typed text is kept: {body}");
    assert_eq!(read(vault_dir.path(), "journal/2020/01/2020-01-01.md")?, RICH_ENTRY);
    Ok(())
}

/// The version of section `index` of `text` as the edit form would carry it, read without a template.
fn fingerprint_of(text: &str, index: usize) -> Result<String, BoxError> {
    let entry = contextos_web::JournalEntry::from(contextos_web::EntryInput { text, template: "" });
    Ok(entry.fingerprint(index).ok_or("no such section")?.as_str().to_owned())
}

#[tokio::test]
async fn a_read_only_section_cannot_be_saved() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/journal.toml",
        "read_only_sections = [\"Reading\"]\n",
    )?;
    write(vault_dir.path(), "journal/2020/01/2020-01-01.md", RICH_ENTRY)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // The section's real version, so only the read-only rule can refuse the save.
    let base = fingerprint_of(RICH_ENTRY, 2)?;
    let (status, body, _) = hx_patch(
        &router,
        &format!("/{VAULT_NAME}/journal/2020-01-01/sections/2"),
        &save_body("Reading", &base, "- x")?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("read-only"), "{body}");
    assert!(
        !body.contains("<textarea"),
        "a read-only section is answered with its read view: {body}"
    );
    assert_eq!(read(vault_dir.path(), "journal/2020/01/2020-01-01.md")?, RICH_ENTRY);
    Ok(())
}

#[tokio::test]
async fn a_save_that_would_swallow_the_sections_after_it_is_refused_and_keeps_the_typed_text() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "journal/2020/01/2020-01-01.md", RICH_ENTRY)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (heading, base) = open_editor(&router, "2020-01-01", 0).await?;
    let (status, body, _) = hx_patch(
        &router,
        &format!("/{VAULT_NAME}/journal/2020-01-01/sections/0"),
        &save_body(&heading, &base, "```sh\nan unclosed code block")?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("rest of the day"), "{body}");
    assert!(
        body.contains("an unclosed code block"),
        "the typed text is kept: {body}"
    );
    assert_eq!(read(vault_dir.path(), "journal/2020/01/2020-01-01.md")?, RICH_ENTRY);
    Ok(())
}

#[tokio::test]
async fn an_entry_that_cannot_be_read_is_never_shown_or_saved_as_a_new_day() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_template(vault_dir.path())?;
    let entry = vault_dir.path().join("journal/2020/01/2020-01-01.md");
    std::fs::create_dir_all(entry.parent().ok_or("an entry has a folder")?)?;
    let not_text: &[u8] = &[0xff, 0xfe, 0x00, 0x9f, 0x92, 0x96];
    std::fs::write(&entry, not_text)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/journal/2020-01-01")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("could not be read"), "{body}");
    assert!(
        !body.contains(">Gym<"),
        "the template is not offered in its place: {body}"
    );

    let base = fingerprint_of("## Gym\n", 0)?;
    let (status, body, _) = hx_patch(
        &router,
        &format!("/{VAULT_NAME}/journal/2020-01-01/sections/0"),
        &save_body("Gym", &base, "- x")?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("not saved"), "{body}");
    assert_eq!(std::fs::read(&entry)?, not_text);
    Ok(())
}

#[tokio::test]
async fn a_malformed_save_request_is_rejected_and_writes_nothing() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "journal/2020/01/2020-01-01.md", RICH_ENTRY)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (heading, base) = open_editor(&router, "2020-01-01", 2).await?;
    let unknown_field = format!(r#"{{"heading":"{heading}","base":"{base}","content":"x","force":true}}"#);
    for body in [unknown_field.as_str(), r#"{"content":"x"}"#, "not json"] {
        let (status, response, _) =
            hx_patch(&router, &format!("/{VAULT_NAME}/journal/2020-01-01/sections/2"), body).await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {response}");
        assert!(response.contains("Malformed request"), "{response}");
    }
    let (status, _, _) = hx_patch(
        &router,
        &format!("/{VAULT_NAME}/journal/not-a-date/sections/2"),
        &save_body(&heading, &base, "x")?,
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(read(vault_dir.path(), "journal/2020/01/2020-01-01.md")?, RICH_ENTRY);
    Ok(())
}

#[tokio::test]
async fn content_that_adds_a_heading_refreshes_the_whole_day_so_its_tabs_match() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "journal/2020/01/2020-01-01.md", RICH_ENTRY)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (heading, base) = open_editor(&router, "2020-01-01", 2).await?;
    let (status, body, headers) = hx_patch(
        &router,
        &format!("/{VAULT_NAME}/journal/2020-01-01/sections/2"),
        &save_body(&heading, &base, "- Chapter 3\n\n## Writing\n\n- 500 words")?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        headers.get("HX-Retarget").and_then(|value| value.to_str().ok()),
        Some(".journal-screen")
    );
    assert_eq!(
        headers.get("HX-Reswap").and_then(|value| value.to_str().ok()),
        Some("outerHTML")
    );
    assert!(body.contains("data-pane=\"3\">Writing<"), "{body}");
    Ok(())
}
