//! What `links_read` reports for a note: every link in the form it was written, the file each resolved link reaches,
//! and each link that finds no file or several, resolved by the one rule every other reader of a link uses.

mod support;

use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

async fn call_tool(server: ContextOsServer, arguments: Value) -> Result<CallToolResult, TestError> {
    let arguments: Map<String, Value> = serde_json::from_value(arguments)?;
    let (server_transport, client_transport) = tokio::io::duplex(65536);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), TestError>(())
    });
    let mut client = ().serve(client_transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new("links_read").with_arguments(arguments))
        .await;
    client.close().await?;
    server_handle.await??;
    Ok(result?)
}

fn put(vault: &tempfile::TempDir, relative: &str, content: &str) -> TestResult {
    let path = vault.path().join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

fn server_over(vault: &tempfile::TempDir) -> Result<ContextOsServer, TestError> {
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    Ok(ContextOsServer::try_from(config)?)
}

async fn read(vault: &tempfile::TempDir, note: &str) -> Result<Map<String, Value>, TestError> {
    let result = call_tool(server_over(vault)?, json!({"path": note})).await?;
    assert_ne!(result.is_error, Some(true), "{result:?}");
    result
        .structured_content
        .and_then(|content| content.as_object().cloned())
        .ok_or_else(|| std::io::Error::other("links_read omitted structured content").into())
}

fn array<'a>(content: &'a Map<String, Value>, key: &str) -> Result<&'a Vec<Value>, TestError> {
    content
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| std::io::Error::other(format!("links_read omitted {key}")).into())
}

#[tokio::test]
async fn every_link_form_is_reported_with_the_file_a_resolved_link_reaches() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    put(
        &vault,
        "notes/a.md",
        "[[b]]\n![[pic.png]]\n[m](../raw/my%20note.md)\n![i](<../raw/pic 2.png>)\n[r][ref]\n\n[ref]: /raw/c.md\n",
    )?;
    put(&vault, "notes/b.md", "x\n")?;
    put(&vault, "assets/pic.png", "x")?;
    put(&vault, "raw/my note.md", "x\n")?;
    put(&vault, "raw/pic 2.png", "x")?;
    put(&vault, "raw/c.md", "x\n")?;

    let content = read(&vault, "notes/a.md").await?;

    let seen: Vec<_> = array(&content, "outgoing")?
        .iter()
        .map(|link| {
            (
                link["target"].clone(),
                link["form"].clone(),
                link["embed"].clone(),
                link["resolved"].clone(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            (json!("b"), json!("wikilink"), json!(false), json!("notes/b.md")),
            (
                json!("pic.png"),
                json!("wikilink-embed"),
                json!(true),
                json!("assets/pic.png")
            ),
            (
                json!("../raw/my note.md"),
                json!("markdown"),
                json!(false),
                json!("raw/my note.md")
            ),
            (
                json!("../raw/pic 2.png"),
                json!("markdown-embed"),
                json!(true),
                json!("raw/pic 2.png")
            ),
            (json!("/raw/c.md"), json!("markdown"), json!(false), json!("raw/c.md")),
        ]
    );
    assert_eq!(array(&content, "unresolved")?.len(), 0);
    Ok(())
}

#[tokio::test]
async fn links_to_attachments_canvases_bases_and_folders_resolve() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    put(
        &vault,
        "a.md",
        "[[board.canvas]] [[tasks.base]] [r](report.pdf) [f](raw/2026/) [[private-note]]\n",
    )?;
    put(&vault, "board.canvas", "{}")?;
    put(&vault, "tasks.base", "views: []\n")?;
    put(&vault, "report.pdf", "x")?;
    put(&vault, "raw/2026/index.md", "x\n")?;
    put(&vault, "templates/private-note.md", "x\n")?;

    let content = read(&vault, "a.md").await?;

    assert_eq!(array(&content, "unresolved")?.len(), 0, "{content:?}");
    let resolved: Vec<_> = array(&content, "outgoing")?
        .iter()
        .map(|link| link["resolved"].clone())
        .collect();
    assert_eq!(
        resolved,
        vec![
            json!("board.canvas"),
            json!("tasks.base"),
            json!("report.pdf"),
            json!("raw/2026/index.md"),
            json!("templates/private-note.md"),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_link_finding_no_file_or_several_is_unresolved_with_its_form_and_kind() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    put(
        &vault,
        "a.md",
        "[[gone]]\n![[gone.png]]\n[x](nowhere.md)\n![y](nowhere.png)\n[[status]]\n[[gone]]\n",
    )?;
    put(&vault, "one/status.md", "x\n")?;
    put(&vault, "two/status.md", "x\n")?;

    let content = read(&vault, "a.md").await?;

    assert_eq!(
        array(&content, "unresolved")?,
        &vec![
            json!({"target": "gone", "form": "wikilink", "kind": "not-found"}),
            json!({"target": "gone.png", "form": "wikilink-embed", "kind": "not-found"}),
            json!({"target": "nowhere.md", "form": "markdown", "kind": "not-found"}),
            json!({"target": "nowhere.png", "form": "markdown-embed", "kind": "not-found"}),
            json!({"target": "status", "form": "wikilink", "kind": "ambiguous",
                   "candidates": ["one/status.md", "two/status.md"]}),
        ]
    );
    let outgoing = array(&content, "outgoing")?;
    assert_eq!(outgoing[0].get("resolved"), None);
    Ok(())
}

#[tokio::test]
async fn a_name_that_exists_once_resolves_and_placeholder_links_are_neither_resolved_nor_unresolved() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    put(&vault, "a.md", "[[status]] [[{{slug}}]] [[<slug>]]\n")?;
    put(&vault, "one/status.md", "x\n")?;

    let content = read(&vault, "a.md").await?;

    assert_eq!(array(&content, "unresolved")?.len(), 0, "{content:?}");
    let outgoing = array(&content, "outgoing")?;
    assert_eq!(outgoing[0]["resolved"], json!("one/status.md"));
    assert!(outgoing[1..].iter().all(|link| link.get("resolved").is_none()));
    Ok(())
}

#[tokio::test]
async fn a_note_declaring_a_base_settles_a_shared_name_by_the_base_folder() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    put(
        &vault,
        "tasks/tasks.base",
        "filters: 'file.inFolder(\"tasks\")'\nviews:\n  - type: table\n    name: T\n",
    )?;
    put(
        &vault,
        "tasks/t-1.md",
        "---\nbase: \"[[tasks/tasks.base]]\"\n---\n[[status]]\n",
    )?;
    put(&vault, "tasks/status.md", "x\n")?;
    put(&vault, "other/status.md", "x\n")?;

    let content = read(&vault, "tasks/t-1.md").await?;

    assert_eq!(array(&content, "unresolved")?.len(), 0, "{content:?}");
    Ok(())
}

fn code(result: &CallToolResult) -> Option<String> {
    result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("code"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

async fn read_with(server: &ContextOsServer, arguments: Value) -> Result<Map<String, Value>, TestError> {
    let result = call_tool(server.clone(), arguments).await?;
    assert_ne!(result.is_error, Some(true), "{result:?}");
    result
        .structured_content
        .and_then(|content| content.as_object().cloned())
        .ok_or_else(|| std::io::Error::other("links_read omitted structured content").into())
}

fn linked_vault() -> Result<tempfile::TempDir, TestError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    put(&vault, "target.md", "# Target\n[[gone]]\n")?;
    put(&vault, "z.md", "x\n[[target]]\n")?;
    put(
        &vault,
        "a.md",
        "[[target]] and [t](target.md)\n\n![[target]]\n[[target]]\n",
    )?;
    put(&vault, "m/b.md", "[again](../target.md)\n")?;
    put(&vault, "other.md", "no link here\n")?;
    Ok(vault)
}

#[tokio::test]
async fn the_incoming_direction_lists_one_entry_per_link_sorted_by_source_then_line_and_nothing_else() -> TestResult {
    let vault = linked_vault()?;

    let content = read_with(&server_over(&vault)?, json!({"path": "target.md", "direction": "in"})).await?;

    assert_eq!(
        array(&content, "incoming")?,
        &vec![
            json!({"source": "a.md", "line": 1, "form": "markdown"}),
            json!({"source": "a.md", "line": 1, "form": "wikilink"}),
            json!({"source": "a.md", "line": 3, "form": "wikilink-embed"}),
            json!({"source": "a.md", "line": 4, "form": "wikilink"}),
            json!({"source": "m/b.md", "line": 1, "form": "markdown"}),
            json!({"source": "z.md", "line": 2, "form": "wikilink"}),
        ]
    );
    assert_eq!(content.get("outgoing"), None);
    assert_eq!(content.get("unresolved"), None);
    Ok(())
}

#[tokio::test]
async fn the_both_direction_adds_the_incoming_links_to_the_outgoing_and_unresolved_ones() -> TestResult {
    let vault = linked_vault()?;

    let content = read_with(&server_over(&vault)?, json!({"path": "target.md", "direction": "both"})).await?;

    assert_eq!(array(&content, "incoming")?.len(), 6);
    assert_eq!(array(&content, "outgoing")?.len(), 1);
    assert_eq!(
        array(&content, "unresolved")?,
        &vec![json!({"target": "gone", "form": "wikilink", "kind": "not-found"})]
    );
    Ok(())
}

#[tokio::test]
async fn the_outgoing_direction_never_includes_incoming() -> TestResult {
    let vault = linked_vault()?;

    let content = read_with(&server_over(&vault)?, json!({"path": "target.md"})).await?;

    assert_eq!(content.get("incoming"), None);
    assert!(content.contains_key("outgoing"));
    assert!(content.contains_key("unresolved"));
    Ok(())
}

#[tokio::test]
async fn a_note_nothing_links_to_has_an_empty_incoming_list() -> TestResult {
    let vault = linked_vault()?;

    let content = read_with(&server_over(&vault)?, json!({"path": "other.md", "direction": "in"})).await?;

    assert_eq!(array(&content, "incoming")?.len(), 0);
    Ok(())
}

#[tokio::test]
async fn incoming_and_both_fail_on_a_vault_whose_graph_is_off_but_outgoing_still_reads() -> TestResult {
    let vault = linked_vault()?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].search.graph = false;
    let server = ContextOsServer::try_from(config)?;

    for direction in ["in", "both"] {
        let result = call_tool(server.clone(), json!({"path": "target.md", "direction": direction})).await?;
        assert_eq!(result.is_error, Some(true), "{direction}");
        assert_eq!(code(&result).as_deref(), Some("index/disabled"), "{direction}");
    }
    let outgoing = read_with(&server, json!({"path": "target.md"})).await?;
    assert_eq!(array(&outgoing, "outgoing")?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_link_written_outside_the_server_is_counted_by_the_next_incoming_read() -> TestResult {
    let vault = linked_vault()?;
    let server = server_over(&vault)?;
    let before = read_with(&server, json!({"path": "target.md", "direction": "in"})).await?;
    assert_eq!(array(&before, "incoming")?.len(), 6);

    put(&vault, "late.md", "see [[target]]\n")?;
    std::fs::remove_file(vault.path().join("z.md"))?;

    let after = read_with(&server, json!({"path": "target.md", "direction": "in"})).await?;
    let sources: Vec<_> = array(&after, "incoming")?
        .iter()
        .map(|link| link["source"].clone())
        .collect();
    assert_eq!(
        sources,
        vec![
            json!("a.md"),
            json!("a.md"),
            json!("a.md"),
            json!("a.md"),
            json!("late.md"),
            json!("m/b.md")
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_link_from_an_excluded_folder_still_counts_as_incoming() -> TestResult {
    let vault = linked_vault()?;
    put(&vault, "templates/t.md", "[[target]]\n")?;

    let content = read_with(&server_over(&vault)?, json!({"path": "target.md", "direction": "in"})).await?;

    let sources: Vec<_> = array(&content, "incoming")?
        .iter()
        .map(|link| link["source"].clone())
        .collect();
    assert!(sources.contains(&json!("templates/t.md")), "{sources:?}");
    Ok(())
}

#[tokio::test]
async fn an_attachment_reports_the_notes_that_embed_it() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    put(&vault, "pic.png", "x")?;
    put(&vault, "a.md", "![[pic.png]]\n")?;

    let content = read_with(&server_over(&vault)?, json!({"path": "pic.png", "direction": "in"})).await?;

    assert_eq!(
        array(&content, "incoming")?,
        &vec![json!({"source": "a.md", "line": 1, "form": "wikilink-embed"})]
    );
    Ok(())
}

#[tokio::test]
async fn an_unknown_direction_is_refused() -> TestResult {
    let vault = linked_vault()?;

    let result = call_tool(
        server_over(&vault)?,
        json!({"path": "target.md", "direction": "sideways"}),
    )
    .await;

    assert!(result.is_err() || result?.is_error == Some(true));
    Ok(())
}
