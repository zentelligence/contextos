//! Links shown by contextos-web resolve by the server's own rules, against a real `contextos-mcp` session over a real
//! temporary vault: nearest first, then by name, a name several notes answer to settled by the base a record names in
//! its `base` property, and a frontmatter link chip exactly as the server expands the link.

mod support;

use std::path::Path;

use contextos_web::McpClientSet;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const ORIGIN: &str = "memory/tasks/sub/origin.md";

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

/// A wide `memory/tasks.base` and a nested base over `memory/tasks/sub` selected by `sub_filters`, with a name
/// (`twin`) two notes under the wide base answer to and only one under the nested base, and an `areas/tasks.base`
/// that makes a bare `tasks.base` ambiguous across the vault. The record at `origin` links to `[[twin]]`.
fn vault(sub_filters: &str, origin: &str, origin_base: Option<&str>) -> Result<tempfile::TempDir, BoxError> {
    let vault = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    write(
        vault.path(),
        "memory/tasks.base",
        "filters: file.inFolder(\"memory/tasks\") && file.basename != \"index\"\nviews:\n  - type: table\n    name: All\n",
    )?;
    write(
        vault.path(),
        "memory/tasks/sub.base",
        &format!("filters: {sub_filters}\nviews:\n  - type: table\n    name: All\n"),
    )?;
    write(
        vault.path(),
        "areas/tasks.base",
        "filters: file.inFolder(\"areas\")\nviews:\n  - type: table\n    name: All\n",
    )?;
    write(vault.path(), "memory/tasks/sub/twin.md", &task("REC-020"))?;
    write(vault.path(), "memory/tasks/other/twin.md", &task("REC-021"))?;
    let base_line = origin_base.map_or_else(String::new, |base| format!("base: '{base}'\n"));
    write(
        vault.path(),
        origin,
        &format!("---\nid: REC-022\n{base_line}title: Origin\n---\n\nDepends on [[twin]].\n"),
    )?;
    Ok(vault)
}

const SUB_TWIN: &str = "<a class=\"wikilink\" href=\"/contract-fixture/tasks/rec-020\">twin</a>";
const AMBIGUOUS_TWIN: &str = "<span class=\"wikilink dead ambiguous\" data-target=\"twin\"";

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
async fn a_link_finds_the_note_of_its_name_beside_it_whatever_base_the_record_names() -> Result<(), BoxError> {
    for base in [None, Some("[[tasks.base]]"), Some("[[sub.base|Sub]]")] {
        let vault = vault("file.inFolder(\"memory/tasks/sub\")", ORIGIN, base)?;
        let (status, page) = get_body(vault.path(), "/contract-fixture/tasks/rec-022").await?;
        assert_eq!(status, axum::http::StatusCode::OK, "{base:?}");
        assert!(page.contains(SUB_TWIN), "{base:?}: {page}");
    }
    Ok(())
}

#[tokio::test]
async fn a_link_from_a_deeper_folder_finds_the_nearest_ancestors_note() -> Result<(), BoxError> {
    let vault = vault(
        "file.folder == \"memory/tasks/sub\"",
        "memory/tasks/sub/deeper/origin.md",
        None,
    )?;
    let (_, page) = get_body(vault.path(), "/contract-fixture/tasks/rec-022").await?;
    assert!(page.contains(SUB_TWIN), "{page}");
    Ok(())
}

#[tokio::test]
async fn a_name_two_notes_answer_to_is_settled_by_the_base_the_record_names() -> Result<(), BoxError> {
    // From `memory/tasks/elsewhere/`, no ancestor holds a `twin`, so the name is looked up across the vault, where two
    // notes answer to it: the record's own base keeps exactly one of them.
    let declared = vault(
        "file.inFolder(\"memory/tasks/sub\")",
        "memory/tasks/elsewhere/origin.md",
        Some("[[sub.base]]"),
    )?;
    let (_, page) = get_body(declared.path(), "/contract-fixture/tasks/rec-022").await?;
    assert!(page.contains(SUB_TWIN), "{page}");

    let undeclared = vault(
        "file.inFolder(\"memory/tasks/sub\")",
        "memory/tasks/elsewhere/origin.md",
        None,
    )?;
    let (_, page) = get_body(undeclared.path(), "/contract-fixture/tasks/rec-022").await?;
    assert!(page.contains(AMBIGUOUS_TWIN), "{page}");
    Ok(())
}

#[tokio::test]
async fn a_metadata_link_chip_resolves_to_the_nearest_note_as_the_server_expands_it() -> Result<(), BoxError> {
    let vault = vault("file.inFolder(\"memory/tasks/sub\")", ORIGIN, None)?;
    write(vault.path(), "notes/2026/dup.md", "# Near\n")?;
    write(vault.path(), "areas/alpha/dup.md", "# Far\n")?;
    write(vault.path(), "areas/beta/dup.md", "# Far\n")?;
    write(
        vault.path(),
        "notes/2026/one.md",
        "---\ntitle: One\nrelated: '[[dup]]'\nelsewhere: '[[twin]]'\n---\n\nBody.\n",
    )?;

    let (status, page) = get_body(vault.path(), "/contract-fixture/notes/2026/one.md").await?;

    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(
        page.contains("<a class=\"metadata-link\" href=\"/contract-fixture/notes/2026/dup.md\">dup</a>"),
        "{page}"
    );
    assert!(
        page.contains("twin<span class=\"visually-hidden\"> (More than one note answers to this name.)</span>"),
        "{page}"
    );
    Ok(())
}
