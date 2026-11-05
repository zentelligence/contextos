//! Contract tests for the task detail page's own link-field search-and-pick
//! widget (`routes::tasks_links`): reading and resolving the four real
//! task-to-task link fields (`parent`, `depends_on`, `blocks`, `related`)
//! out of a note's actual Obsidian wikilink frontmatter, the live search
//! endpoint behind the picker, and the write-then-read round trip through
//! the existing generic frontmatter PATCH route. Real `contextos-mcp`
//! sessions against real temporary vault fixtures throughout, no mocking,
//! per `testing.md`. Kept apart from `tests/tasks.rs` (already close to
//! this workspace's own file-size guidance) rather than growing that file
//! further.

mod support;

use std::fmt::Write as _;
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

async fn patch(router: &Router, path: &str, json_body: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(json_body.to_owned()))?,
        )
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

/// A plain task note, with no link fields of its own, used as the "other
/// side" of a link in these fixtures.
fn plain_task(id: &str, title: &str) -> String {
    format!(
        "---\n\
         id: {id}\n\
         type: task\n\
         title: {title}\n\
         state: next\n\
         task_type: action\n\
         entity: zenith\n\
         ---\n\n# {title}\n"
    )
}

/// A task note carrying every one of the four link fields, formatted
/// exactly as this vault's own real notes spell them: a single-valued
/// `parent` wikilink string, and `depends_on`/`blocks`/`related` as YAML
/// block lists of wikilink strings. Any argument left `&[]`/`None` omits
/// that field entirely, rather than writing it out empty, so a test can
/// also exercise "the field was never set" separately from "the field was
/// set and then cleared". `#[allow]`: a fixture builder naming each of the
/// four real link fields by its own parameter, rather than a struct or
/// builder pattern this test-only helper does not otherwise need, reads
/// more directly at every call site.
#[allow(clippy::too_many_arguments)]
fn task_with_links(
    id: &str,
    title: &str,
    parent: Option<&str>,
    depends_on: &[&str],
    blocks: &[&str],
    related: &[&str],
) -> String {
    let mut frontmatter = format!(
        "---\n\
         id: {id}\n\
         type: task\n\
         title: {title}\n\
         state: next\n\
         task_type: action\n\
         entity: zenith\n"
    );
    if let Some(parent) = parent {
        let _ = writeln!(frontmatter, "parent: '{parent}'");
    }
    for (key, values) in [("depends_on", depends_on), ("blocks", blocks), ("related", related)] {
        if !values.is_empty() {
            let _ = writeln!(frontmatter, "{key}:");
            for value in values {
                let _ = writeln!(frontmatter, "  - '{value}'");
            }
        }
    }
    let _ = write!(frontmatter, "---\n\n# {title}\n");
    frontmatter
}

// ---------------------------------------------------------------------------
// Read side: parsing and resolving real wikilink frontmatter into chips.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_resolved_wikilink_renders_as_a_chip_with_the_targets_own_title_never_the_raw_syntax() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-060.md",
        &plain_task("T-060", "Ship the widget"),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-062.md",
        &task_with_links(
            "T-062",
            "Follow up on the widget",
            Some("[[t-060|T-060]]"),
            &[],
            &[],
            &["[[t-060|T-060]]"],
        ),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-062/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body.contains("[[t-060"),
        "the raw wikilink syntax must never reach the operator"
    );
    assert!(body.contains("Ship the widget"), "the resolved title must be shown");
    // Both the `parent` chip and the `related` chip resolve the identical
    // target; each must carry its own display id.
    let parent_field_idx = body
        .find("data-field=\"parent\"")
        .ok_or("parent field container present")?;
    assert!(body[parent_field_idx..].contains("T-060"));
    let related_field_idx = body
        .find("data-field=\"related\"")
        .ok_or("related field container present")?;
    assert!(body[related_field_idx..].contains("T-060"));
    Ok(())
}

#[tokio::test]
async fn a_link_to_a_note_no_screen_shows_says_so_and_a_link_to_nothing_says_not_found() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "requirements/REQ-001.md", "# A requirement\n")?;
    write(vault_dir.path(), "areas/unique-spec.md", "# A spec\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-062.md",
        &task_with_links(
            "T-062",
            "Follow up",
            None,
            &[],
            &[],
            &[
                "[[requirements/REQ-001|REQ-001]]",
                "[[unique-spec]]",
                "[[requirements/gone]]",
            ],
        ),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, page) = get(&router, "/contract-fixture/tasks/t-062").await?;

    assert_eq!(status, StatusCode::OK);
    // Notes no screen shows are not links: a link to them would only fail.
    assert!(!page.contains("href=\"/contract-fixture/requirements/"), "{page}");
    assert!(!page.contains("href=\"/contract-fixture/areas/"), "{page}");
    assert_eq!(page.matches("No screen yet").count(), 2, "{page}");
    assert!(page.contains("No screen shows this note yet."), "{page}");
    assert_eq!(page.matches("Not found").count(), 1, "only the dead link: {page}");

    let (_, edit_page) = get(&router, "/contract-fixture/tasks/t-062/edit").await?;
    assert!(
        edit_page.contains("<input type=\"hidden\" name=\"related\" value=\"requirements/REQ-001\">"),
        "the chip keeps the target as written so saving writes the same link back: {edit_page}"
    );
    assert!(
        !edit_page.contains("href=\"/contract-fixture/requirements/"),
        "an unrouted chip is not a link: {edit_page}"
    );
    assert!(edit_page.contains("<span class=\"mono\">REQ-001</span>"), "{edit_page}");
    Ok(())
}

#[tokio::test]
async fn a_dangling_wikilink_still_renders_a_chip_labelled_with_only_its_raw_id() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-063.md",
        &task_with_links("T-063", "Orphaned parent", Some("[[t-999|T-999]]"), &[], &[], &[]),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-063/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("data-target=\"t-999\""),
        "a dangling reference must still render as a chip, not be dropped"
    );
    assert!(
        body.contains("link-chip-dangling"),
        "it must be visibly marked as unresolved"
    );
    Ok(())
}

#[tokio::test]
async fn a_note_with_no_link_fields_set_renders_every_link_field_with_no_chips() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-064.md",
        &plain_task("T-064", "A standalone task"),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-064/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    for field in ["parent", "depends_on", "blocks", "related"] {
        let idx = body
            .find(&format!("data-field=\"{field}\""))
            .ok_or_else(|| format!("{field} field container present"))?;
        let next_field = body[idx + 1..].find("task-link-field").map(|offset| idx + 1 + offset);
        let scope = &body[idx..next_field.unwrap_or(body.len())];
        assert!(!scope.contains("link-chip\""), "{field} must render with no chips");
    }
    Ok(())
}

#[tokio::test]
async fn waiting_on_still_renders_as_a_plain_text_field_not_a_link_chip() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-065.md",
        "---\nid: T-065\ntype: task\ntitle: Waiting on legal\nstate: waiting\ntask_type: action\n\
         entity: zenith\nwaiting_on: Legal counsel\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-065/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("data-field=\"waiting_on\""));
    assert!(body.contains("value=\"Legal counsel\""));
    assert!(
        !body.contains("class=\"task-field task-link-field\" data-field=\"waiting_on\""),
        "waiting_on is free text, never a link field"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The live search-and-pick endpoint.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_search_endpoint_matches_case_insensitively_against_title_or_id() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-060.md",
        &plain_task("T-060", "Ship the widget"),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-061.md",
        &plain_task("T-061", "Renew the licence"),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-062.md",
        &plain_task("T-062", "Self task"),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(
        &router,
        &format!("/{VAULT_NAME}/tasks/t-062/links/search?field=related&q=WIDGET"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("T-060"));
    assert!(body.contains("Ship the widget"));
    assert!(!body.contains("T-061"));

    let (status, id_body) = get(
        &router,
        &format!("/{VAULT_NAME}/tasks/t-062/links/search?field=related&q=t-061"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{id_body}");
    assert!(id_body.contains("T-061"));
    assert!(!id_body.contains("T-060"));
    Ok(())
}

#[tokio::test]
async fn the_search_endpoint_never_offers_the_current_task_as_a_result_of_itself() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-060.md",
        &plain_task("T-060", "Ship the widget"),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(
        &router,
        &format!("/{VAULT_NAME}/tasks/t-060/links/search?field=related&q=widget"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body.contains("T-060"),
        "a task must never be offered as a link to itself"
    );
    Ok(())
}

#[tokio::test]
async fn the_search_endpoint_rejects_an_unrecognised_field() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-060.md",
        &plain_task("T-060", "Ship it"),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = get(
        &router,
        &format!("/{VAULT_NAME}/tasks/t-060/links/search?field=bogus&q=ship"),
    )
    .await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-060/links/search?q=ship")).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a missing field must also be rejected");
    Ok(())
}

#[tokio::test]
async fn an_empty_search_query_still_returns_candidates_rather_than_an_empty_fragment() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-060.md",
        &plain_task("T-060", "Ship it"),
    )?;
    write(vault_dir.path(), "memory/tasks/t-061.md", &plain_task("T-061", "Self"))?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(
        &router,
        &format!("/{VAULT_NAME}/tasks/t-061/links/search?field=related&q="),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("T-060"));
    Ok(())
}

// ---------------------------------------------------------------------------
// The write-then-read round trip, through the existing generic frontmatter
// PATCH route: proves a wikilink string this widget's own chips would
// submit is persisted verbatim and then re-parses back into the identical
// resolved chip on the very next read.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn patching_a_single_valued_link_field_persists_the_exact_wikilink_format_and_re_resolves_on_read()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-060.md",
        &plain_task("T-060", "Ship the widget"),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-062.md",
        &plain_task("T-062", "Follow-up"),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/memory/tasks/t-062.md"),
        r#"{"patch": {"parent": "[[t-060|T-060]]"}}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The exact format a picked chip serialises to must be what actually
    // landed on disk, not a reformatted or reordered equivalent.
    let raw = std::fs::read_to_string(vault_dir.path().join("memory/tasks/t-062.md"))?;
    assert!(
        raw.contains("[[t-060|T-060]]"),
        "the note must store the exact lowercase-target/uppercase-alias wikilink form:\n{raw}"
    );

    let (status, detail_body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-062/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{detail_body}");
    assert!(!detail_body.contains("[[t-060"));
    let parent_idx = detail_body
        .find("data-field=\"parent\"")
        .ok_or("parent field container present")?;
    assert!(detail_body[parent_idx..].contains("Ship the widget"));
    Ok(())
}

#[tokio::test]
async fn patching_a_list_valued_link_field_to_an_empty_array_clears_every_chip() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-060.md",
        &plain_task("T-060", "Ship the widget"),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-062.md",
        &task_with_links("T-062", "Follow-up", None, &["[[t-060|T-060]]"], &[], &[]),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, before) = get(&router, &format!("/{VAULT_NAME}/tasks/t-062/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{before}");
    let depends_idx = before
        .find("data-field=\"depends_on\"")
        .ok_or("depends_on field container present")?;
    assert!(
        before[depends_idx..].contains("T-060"),
        "the fixture must start with a chip"
    );

    let (status, patch_body) = patch(
        &router,
        &format!("/{VAULT_NAME}/memory/tasks/t-062.md"),
        r#"{"patch": {"depends_on": []}}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{patch_body}");

    let (status, after) = get(&router, &format!("/{VAULT_NAME}/tasks/t-062/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{after}");
    let depends_idx = after
        .find("data-field=\"depends_on\"")
        .ok_or("depends_on field container present")?;
    let next_field = after[depends_idx + 1..]
        .find("task-link-field")
        .map(|offset| depends_idx + 1 + offset);
    let scope = &after[depends_idx..next_field.unwrap_or(after.len())];
    assert!(
        !scope.contains("link-chip\""),
        "an empty array patch must clear every chip"
    );
    Ok(())
}

#[tokio::test]
async fn patching_parent_to_null_clears_the_single_chip() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-060.md",
        &plain_task("T-060", "Ship the widget"),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-062.md",
        &task_with_links("T-062", "Follow-up", Some("[[t-060|T-060]]"), &[], &[], &[]),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, patch_body) = patch(
        &router,
        &format!("/{VAULT_NAME}/memory/tasks/t-062.md"),
        r#"{"patch": {"parent": null}}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{patch_body}");

    let (status, after) = get(&router, &format!("/{VAULT_NAME}/tasks/t-062/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{after}");
    let parent_idx = after
        .find("data-field=\"parent\"")
        .ok_or("parent field container present")?;
    let next_field = after[parent_idx + 1..]
        .find("task-link-field")
        .map(|offset| parent_idx + 1 + offset);
    let scope = &after[parent_idx..next_field.unwrap_or(after.len())];
    assert!(
        !scope.contains("link-chip\""),
        "a null patch must clear the parent chip"
    );
    Ok(())
}
