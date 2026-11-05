//! The shared vault link resolver against a real `contextos-mcp` session over a real temporary vault: how a link's
//! text becomes a vault path and a web route, given the base the linking note belongs to.

mod support;

use std::path::Path;

use contextos_web::McpClientSet;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const TASKS_BASE_YAML: &str = "filters: file.inFolder(\"memory/tasks\") && file.basename != \"index\"\n\
    views:\n  - type: table\n    name: All\n    order:\n      - file.path\n";

const NESTED_NOTE: &str = "memory/tasks/zenith/mcp/rec-007.md";
const SIBLING_NOTE: &str = "memory/tasks/zenith/mcp/rec-008.md";

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

fn task(id: &str) -> String {
    format!("---\nid: {id}\ntitle: Task {id}\n---\n\nBody.\n")
}

fn vault() -> Result<tempfile::TempDir, BoxError> {
    let vault = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    write(vault.path(), "memory/tasks.base", TASKS_BASE_YAML)?;
    write(vault.path(), NESTED_NOTE, &task("REC-007"))?;
    write(vault.path(), SIBLING_NOTE, &task("REC-008"))?;
    write(vault.path(), "memory/tasks/other/rec-009.md", &task("REC-009"))?;
    write(vault.path(), "memory/tasks/no-id.md", "---\ntitle: No id\n---\n")?;
    write(vault.path(), "notes/2026/one.md", "---\ntitle: One\n---\n")?;
    write(vault.path(), "journal/2026-09-28.md", "---\ntitle: Day\n---\n")?;
    write(vault.path(), "areas/alpha/dup.md", "# Dup\n")?;
    write(vault.path(), "areas/beta/dup.md", "# Dup\n")?;
    write(vault.path(), "areas/alpha/unique-name.md", "# Unique\n")?;
    write(vault.path(), "areas/Release 1.2.md", "# Release\n")?;
    write(vault.path(), "twin.md", "# Root twin\n")?;
    write(vault.path(), "memory/tasks/x/twin.md", "---\ntitle: Twin x\n---\n")?;
    write(vault.path(), "memory/tasks/y/twin.md", "---\ntitle: Twin y\n---\n")?;
    Ok(vault)
}

async fn get_body(vault: &Path, path: &str) -> Result<(axum::http::StatusCode, String), BoxError> {
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt as _;

    let config_dir = tempfile::tempdir()?;
    let config_path = support::write_vault_config(config_dir.path(), vault)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    let router = contextos_web::build_router(clients, Some(config_dir.path()), &config_dir.path().join("web.toml"));
    let response = router
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

#[tokio::test]
async fn every_link_form_in_a_task_body_reaches_the_linked_tasks_own_screen() -> Result<(), BoxError> {
    let vault = vault()?;
    write(
        vault.path(),
        SIBLING_NOTE,
        "---\nid: REC-008\ntitle: Task REC-008\n---\n\nBare [[rec-007]], scoped [[zenith/mcp/rec-007|scoped]], \
         full [[memory/tasks/zenith/mcp/rec-007|full]], dead [[nothing-here]].\n",
    )?;

    let (status, page) = get_body(vault.path(), "/contract-fixture/tasks/rec-008").await?;

    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(
        page.matches("href=\"/contract-fixture/tasks/rec-007\"").count(),
        3,
        "{page}"
    );
    assert!(!page.contains("href=\"/contract-fixture/nothing-here"), "{page}");
    Ok(())
}

#[tokio::test]
async fn a_link_field_in_the_metadata_panel_shows_short_chips_and_keeps_the_raw_value_editable() -> Result<(), BoxError>
{
    let vault = vault()?;
    write(
        vault.path(),
        "notes/2026/one.md",
        "---\ntitle: One\nrelated:\n  - '[[memory/tasks/zenith/mcp/rec-007]]'\n  - '[[nothing-here|Gone]]'\n---\n\nBody.\n",
    )?;

    let (status, page) = get_body(vault.path(), "/contract-fixture/notes/2026/one.md").await?;

    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(
        page.contains("<a class=\"metadata-link\" href=\"/contract-fixture/tasks/rec-007\">rec-007</a>"),
        "{page}"
    );
    assert!(page.contains("<span class=\"metadata-link dead\""), "{page}");
    assert!(
        page.contains("Gone<span class=\"visually-hidden\"> (No note found)</span>"),
        "{page}"
    );
    assert!(
        page.contains("data-field=\"related\" value=\"[[memory/tasks/zenith/mcp/rec-007]], [[nothing-here|Gone]]\""),
        "{page}"
    );
    Ok(())
}

#[tokio::test]
async fn a_link_with_a_heading_or_block_fragment_stays_on_its_targets_own_screen() -> Result<(), BoxError> {
    let vault = vault()?;
    write(
        vault.path(),
        "memory/wiki/topic.md",
        "---\ntitle: Topic\n---\n\n## Some Heading\n\nA paragraph. ^blk1\n",
    )?;
    write(
        vault.path(),
        "memory/wiki/other.md",
        "---\ntitle: Other\n---\n\nSee [[topic#Some Heading]], [[topic#^blk1]], [[topic^blk1]], and the task [[memory/tasks/zenith/mcp/rec-007#Body]].\n",
    )?;

    let (status, page) = get_body(vault.path(), "/contract-fixture/wiki/other.md").await?;

    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(
        page.contains("href=\"/contract-fixture/wiki/topic.md#some-heading\""),
        "a heading link must keep the wiki screen and its menus: {page}"
    );
    assert!(
        page.contains("href=\"/contract-fixture/wiki/topic.md#%5Eblk1\""),
        "a block link must keep the wiki screen and its menus: {page}"
    );
    // Obsidian writes a block reference after `#`; a caret without one is part of the name, which names no note.
    assert!(page.contains("data-target=\"topic^blk1\""), "{page}");
    assert!(
        page.contains("href=\"/contract-fixture/tasks/rec-007#body\""),
        "a link into a task must keep the task screen: {page}"
    );
    assert!(
        !page.contains("href=\"/contract-fixture/memory/wiki/")
            && !page.contains("href=\"/contract-fixture/memory/tasks/"),
        "no link in the body may name a bare vault path: {page}"
    );
    Ok(())
}

/// Where a link is shown going, read from the rendered page.
#[derive(Debug, PartialEq, Eq)]
enum Shown {
    Route(String),
    /// The note exists but no screen shows it.
    Unrouted,
    NotFound,
    Ambiguous,
}

/// The note a probe link is written in: a task inside the tasks base, or a wiki page outside it.
#[derive(Clone, Copy, Debug)]
enum From {
    Task,
    Note,
}

/// How a link to `target`, written in a note at `from`, is shown on that note's own screen.
async fn shown(from: From, target: &str) -> Result<Shown, BoxError> {
    let vault = vault()?;
    let page_path = match from {
        From::Task => {
            write(
                vault.path(),
                SIBLING_NOTE,
                &format!("---\nid: REC-008\ntitle: Task REC-008\n---\n\nSee [[{target}|probe]].\n"),
            )?;
            "/contract-fixture/tasks/rec-008"
        }
        From::Note => {
            write(
                vault.path(),
                "memory/wiki/probe.md",
                &format!("---\ntitle: Probe\n---\n\nSee [[{target}|probe]].\n"),
            )?;
            "/contract-fixture/wiki/probe.md"
        }
    };
    let (status, page) = get_body(vault.path(), page_path).await?;
    if status != axum::http::StatusCode::OK {
        return Err(format!("{page_path} answered {status}").into());
    }
    let end = page.find(">probe</").ok_or("the probe link is on the page")?;
    let opening = page
        .get(..end)
        .and_then(|before| before.rfind('<').and_then(|start| before.get(start..)))
        .unwrap_or_default();
    Ok(
        if let Some(href) = opening.split("href=\"").nth(1).and_then(|rest| rest.split('"').next()) {
            Shown::Route(href.to_owned())
        } else if opening.contains("dead unrouted") {
            Shown::Unrouted
        } else if opening.contains("dead ambiguous") {
            Shown::Ambiguous
        } else {
            Shown::NotFound
        },
    )
}

fn task_route(id: &str) -> Shown {
    Shown::Route(format!("/contract-fixture/tasks/{id}"))
}

#[tokio::test]
async fn a_bare_key_or_a_path_within_the_tasks_folder_reaches_the_task_screen() -> Result<(), BoxError> {
    assert_eq!(shown(From::Task, "rec-007").await?, task_route("rec-007"));
    assert_eq!(shown(From::Task, "zenith/mcp/rec-007").await?, task_route("rec-007"));
    Ok(())
}

#[tokio::test]
async fn a_full_vault_path_reaches_the_task_screen_from_any_note() -> Result<(), BoxError> {
    for from in [From::Task, From::Note] {
        assert_eq!(
            shown(from, "memory/tasks/zenith/mcp/rec-007").await?,
            task_route("rec-007"),
            "{from:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_task_note_without_an_id_exists_but_has_no_screen() -> Result<(), BoxError> {
    assert_eq!(shown(From::Note, "memory/tasks/no-id").await?, Shown::Unrouted);
    Ok(())
}

#[tokio::test]
async fn a_note_in_a_screen_folder_opens_on_that_screen_even_from_a_task() -> Result<(), BoxError> {
    let notes = Shown::Route("/contract-fixture/notes/2026/one.md".to_owned());
    assert_eq!(shown(From::Note, "notes/2026/one").await?, notes);
    assert_eq!(shown(From::Task, "/notes/2026/one").await?, notes);
    assert_eq!(shown(From::Task, "/rec-007").await?, Shown::NotFound);
    Ok(())
}

#[tokio::test]
async fn a_name_is_found_across_the_vault_when_one_note_has_it_and_never_guessed_when_several_do()
-> Result<(), BoxError> {
    assert_eq!(shown(From::Note, "unique-name").await?, Shown::Unrouted);
    assert_eq!(shown(From::Note, "dup").await?, Shown::Ambiguous);
    assert_eq!(shown(From::Note, "beta/dup").await?, Shown::Unrouted);
    Ok(())
}

#[tokio::test]
async fn a_missing_note_is_not_found_and_a_glob_character_is_never_a_pattern() -> Result<(), BoxError> {
    for target in ["missing", "*", "un?que-name"] {
        assert_eq!(shown(From::Note, target).await?, Shown::NotFound, "{target}");
    }
    Ok(())
}

#[tokio::test]
async fn a_note_whose_name_contains_a_dot_is_found_by_its_name_or_its_path() -> Result<(), BoxError> {
    for target in ["Release 1.2", "areas/Release 1.2", "areas/Release 1.2.md"] {
        assert_eq!(shown(From::Note, target).await?, Shown::Unrouted, "{target}");
    }
    Ok(())
}

#[tokio::test]
async fn a_name_in_an_ancestor_folder_is_found_there_before_any_lookup_by_name() -> Result<(), BoxError> {
    // `twin` is answered by two notes under the tasks folder and by one at the vault root, an ancestor of every note:
    // the nearest wins, as Obsidian reads a shortest-path link.
    assert_eq!(shown(From::Task, "twin").await?, Shown::Unrouted);
    assert_eq!(shown(From::Note, "/twin").await?, Shown::Unrouted);
    Ok(())
}
