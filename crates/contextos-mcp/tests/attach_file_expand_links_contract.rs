//! `fs_attach_file` with `expand_links` returns a Markdown note whose links are rewritten in place to vault-rooted
//! paths, and reports that plainly instead of a `content_hash`, so expanded text can never be mistaken for the file's
//! bytes or supplied back as an overwrite. Exercised through a real MCP transport, as every sibling `*_contract.rs`
//! file is; each keeps its own `call_tool` copy, matching this directory's existing pattern.

mod support;
use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, ResourceContents};
use serde_json::{Map, Value, json};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Value,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    let arguments: Map<String, Value> = serde_json::from_value(arguments)?;
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let mut client = ().serve(client_transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    client.close().await?;
    server_handle.await??;
    Ok(result?)
}

fn server_for(vault: &tempfile::TempDir) -> Result<ContextOsServer, Box<dyn std::error::Error + Send + Sync>> {
    Ok(ContextOsServer::try_from(support::scoped_config(vec![
        vault.path().to_path_buf(),
    ])?)?)
}

fn write(vault: &tempfile::TempDir, relative: &str, content: &str) -> TestResult {
    let path = vault.path().join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

fn resource_text(result: &CallToolResult) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    match result
        .content
        .first()
        .and_then(ContentBlock::as_resource)
        .map(|resource| &resource.resource)
    {
        Some(ResourceContents::TextResourceContents { text, .. }) => Ok(text.clone()),
        _ => Err(std::io::Error::other("the first content block is not an embedded text resource").into()),
    }
}

fn metadata_block(result: &CallToolResult) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let block = result
        .content
        .get(1)
        .and_then(ContentBlock::as_text)
        .ok_or_else(|| std::io::Error::other("no text block follows the embedded resource"))?;
    Ok(serde_json::from_str(&block.text)?)
}

fn code(result: &CallToolResult) -> Option<&Value> {
    result.structured_content.as_ref().and_then(|value| value.get("code"))
}

#[tokio::test]
async fn expanded_content_roots_every_link_in_the_body_and_frontmatter_and_says_it_is_expanded() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "wiki/Foo.md", "foo\n")?;
    write(&vault, "wiki/pic.png", "png")?;
    write(&vault, "a/dup.md", "a")?;
    write(&vault, "b/dup.md", "b")?;
    let source = "---\nparent: '[[Foo]]'\n---\nSee [[Foo]], ![[pic.png|300]], [x](Foo.md), [[typo]], [[dup]].\n";
    write(&vault, "wiki/note.md", source)?;
    let server = server_for(&vault)?;

    let result = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "wiki/note.md", "expand_links": true }),
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        resource_text(&result)?,
        "---\nparent: '[[/wiki/Foo.md|Foo]]'\n---\nSee [[/wiki/Foo.md|Foo]], ![[/wiki/pic.png|300]], \
         [x](/wiki/Foo.md), [[/:!typo|typo]], [[/:?dup|dup]].\n"
    );
    assert_eq!(metadata_block(&result)?, json!({ "links_expanded": true }));
    assert_eq!(std::fs::read_to_string(vault.path().join("wiki/note.md"))?, source);
    Ok(())
}

#[tokio::test]
async fn notes_under_memory_tasks_are_expanded_and_resolved_like_any_other_note() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(
        &vault,
        "memory/tasks/A-029.md",
        "---\nparent: '[[A-030]]'\n---\nSee [[Foo]] and [x](A-030.md).\n",
    )?;
    write(&vault, "memory/tasks/A-030.md", "task\n")?;
    write(&vault, "wiki/Foo.md", "foo\n")?;
    write(&vault, "wiki/note.md", "Blocked by [[A-029]], see [t](A-030.md).\n")?;

    let from_task = call_tool(
        server_for(&vault)?,
        "fs_attach_file",
        json!({ "path": "memory/tasks/A-029.md", "expand_links": true }),
    )
    .await?;
    assert_eq!(from_task.is_error, Some(false));
    assert_eq!(
        resource_text(&from_task)?,
        "---\nparent: '[[/memory/tasks/A-030.md|A-030]]'\n---\nSee [[/wiki/Foo.md|Foo]] and [x](/memory/tasks/A-030.md).\n"
    );
    assert_eq!(metadata_block(&from_task)?, json!({ "links_expanded": true }));

    let to_task = call_tool(
        server_for(&vault)?,
        "fs_attach_file",
        json!({ "path": "wiki/note.md", "expand_links": true }),
    )
    .await?;
    assert_eq!(
        resource_text(&to_task)?,
        "Blocked by [[/memory/tasks/A-029.md|A-029]], see [t](/memory/tasks/A-030.md).\n"
    );
    Ok(())
}

#[tokio::test]
async fn expanded_content_carries_no_content_hash_anywhere() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "note.md", "plain\n")?;
    let server = server_for(&vault)?;

    let result = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "note.md", "expand_links": true }),
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    assert!(
        result
            .content
            .iter()
            .filter_map(ContentBlock::as_text)
            .all(|block| !block.text.contains("content_hash")),
        "expanded content must not offer a hash that could authorise an overwrite"
    );
    Ok(())
}

#[tokio::test]
async fn expanded_content_cannot_be_written_back_over_the_note_without_a_hash() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "wiki/Foo.md", "foo\n")?;
    write(&vault, "wiki/note.md", "See [[Foo]].\n")?;
    let server = server_for(&vault)?;
    let expanded = call_tool(
        server.clone(),
        "fs_attach_file",
        json!({ "path": "wiki/note.md", "expand_links": true }),
    )
    .await?;

    let refused = call_tool(
        server,
        "fs_write_file",
        json!({ "path": "wiki/note.md", "content": resource_text(&expanded)? }),
    )
    .await?;

    assert_eq!(refused.is_error, Some(true));
    assert_eq!(code(&refused), Some(&json!("io/expected-hash-required")));
    assert_eq!(
        std::fs::read_to_string(vault.path().join("wiki/note.md"))?,
        "See [[Foo]].\n"
    );
    Ok(())
}

#[tokio::test]
async fn without_expand_links_the_file_is_returned_verbatim_with_its_hash() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "wiki/Foo.md", "foo\n")?;
    write(&vault, "wiki/note.md", "See [[Foo]].\n")?;
    let server = server_for(&vault)?;

    for arguments in [
        json!({ "path": "wiki/note.md" }),
        json!({ "path": "wiki/note.md", "expand_links": false }),
    ] {
        let result = call_tool(server.clone(), "fs_attach_file", arguments).await?;
        assert_eq!(resource_text(&result)?, "See [[Foo]].\n");
        let metadata = metadata_block(&result)?;
        assert!(metadata.get("content_hash").is_some());
        assert!(metadata.get("links_expanded").is_none());
    }
    Ok(())
}

#[tokio::test]
async fn expand_links_on_a_file_that_is_not_markdown_is_rejected_not_ignored() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "plain.txt", "[[Foo]]\n")?;
    write(&vault, "image.png", "png")?;
    write(&vault, "view.base", "filters: {}\n")?;
    let server = server_for(&vault)?;

    for path in ["plain.txt", "image.png", "view.base"] {
        let refused = call_tool(
            server.clone(),
            "fs_attach_file",
            json!({ "path": path, "expand_links": true }),
        )
        .await?;
        assert_eq!(refused.is_error, Some(true), "{path}");
        assert_eq!(code(&refused), Some(&json!("format/not-markdown")), "{path}");
        assert!(
            refused.content.iter().all(|block| block.as_resource().is_none()),
            "{path}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_directory_a_missing_file_or_a_symlink_out_of_the_vault_never_satisfies_a_link() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let outside = tempfile::Builder::new().prefix("outside").tempdir()?;
    write(&vault, "wiki/topic/inner.md", "inner\n")?;
    std::fs::write(outside.path().join("secret.md"), "secret")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path().join("secret.md"), vault.path().join("escape.md"))?;
    write(
        &vault,
        "note.md",
        "[[topic]] [[wiki]] [x](wiki/topic) [[escape]] [[/missing]]\n",
    )?;
    let server = server_for(&vault)?;

    let result = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "note.md", "expand_links": true }),
    )
    .await?;

    assert_eq!(
        resource_text(&result)?,
        "[[/:!topic|topic]] [[/:!wiki|wiki]] [x](/:!wiki/topic) [[/:!escape|escape]] [[/:!/missing|/missing]]\n"
    );
    Ok(())
}

#[tokio::test]
async fn a_note_outside_the_notes_tree_is_found_by_name_including_one_created_outside_the_server() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "elsewhere/deep/Target.md", "target\n")?;
    write(&vault, "wiki/note.md", "[[Target]] [[deep/target]] [x](Target.md)\n")?;
    let server = server_for(&vault)?;

    let result = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "wiki/note.md", "expand_links": true }),
    )
    .await?;

    assert_eq!(
        resource_text(&result)?,
        "[[/elsewhere/deep/Target.md|Target]] [[/elsewhere/deep/Target.md|deep/target]] \
         [x](/elsewhere/deep/Target.md)\n"
    );
    assert_eq!(metadata_block(&result)?, json!({ "links_expanded": true }));
    Ok(())
}

#[tokio::test]
async fn a_link_found_in_the_note_tree_needs_no_text_search() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "wiki/Foo.md", "foo\n")?;
    write(&vault, "wiki/deep/note.md", "[[Foo]]\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].search.text = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "wiki/deep/note.md", "expand_links": true }),
    )
    .await?;

    assert_eq!(resource_text(&result)?, "[[/wiki/Foo.md|Foo]]\n");
    assert_eq!(metadata_block(&result)?, json!({ "links_expanded": true }));
    Ok(())
}

#[tokio::test]
async fn resolving_a_name_outside_the_tree_does_not_depend_on_text_search() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "elsewhere/Target.md", "target\n")?;
    write(&vault, "assets/pic.png", "png")?;
    write(&vault, "wiki/note.md", "[[Target]] ![[pic.png|200]]\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].search.text = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "wiki/note.md", "expand_links": true }),
    )
    .await?;

    assert_eq!(
        resource_text(&result)?,
        "[[/elsewhere/Target.md|Target]] ![[/assets/pic.png|200]]\n"
    );
    assert_eq!(metadata_block(&result)?, json!({ "links_expanded": true }));
    Ok(())
}

#[tokio::test]
async fn a_file_created_after_the_index_was_built_is_found_by_the_single_rescan_a_miss_triggers() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "elsewhere/First.md", "first\n")?;
    write(&vault, "wiki/note.md", "[[First]] [[Later]]\n")?;
    let server = server_for(&vault)?;
    let before = call_tool(
        server.clone(),
        "fs_attach_file",
        json!({ "path": "wiki/note.md", "expand_links": true }),
    )
    .await?;
    assert_eq!(
        resource_text(&before)?,
        "[[/elsewhere/First.md|First]] [[/:!Later|Later]]\n"
    );

    write(&vault, "elsewhere/Later.md", "later\n")?;
    let after = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "wiki/note.md", "expand_links": true }),
    )
    .await?;

    assert_eq!(
        resource_text(&after)?,
        "[[/elsewhere/First.md|First]] [[/elsewhere/Later.md|Later]]\n"
    );
    Ok(())
}

#[tokio::test]
async fn expanded_content_keeps_the_resource_uri_of_the_note_itself() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "wiki/note.md", "plain\n")?;
    let server = server_for(&vault)?;

    let plain = call_tool(server.clone(), "fs_attach_file", json!({ "path": "wiki/note.md" })).await?;
    let expanded = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "wiki/note.md", "expand_links": true }),
    )
    .await?;

    let uri = |result: &CallToolResult| {
        result
            .content
            .first()
            .and_then(ContentBlock::as_resource)
            .and_then(|resource| match &resource.resource {
                ResourceContents::TextResourceContents { uri, .. }
                | ResourceContents::BlobResourceContents { uri, .. } => Some(uri.clone()),
                _ => None,
            })
    };
    assert!(uri(&plain).is_some());
    assert_eq!(uri(&plain), uri(&expanded));
    Ok(())
}

#[tokio::test]
async fn a_note_naming_too_many_distinct_links_is_refused_with_a_stable_code_and_no_content() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let links = (0..10_001).fold(String::new(), |mut links, number| {
        links += "[[missing-";
        links += &number.to_string();
        links += "]] ";
        links
    });
    write(&vault, "note.md", &links)?;
    let server = server_for(&vault)?;

    let refused = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "note.md", "expand_links": true }),
    )
    .await?;

    assert_eq!(refused.is_error, Some(true));
    assert_eq!(code(&refused), Some(&json!("format/too-many-links")));
    assert!(refused.content.iter().all(|block| block.as_resource().is_none()));
    Ok(())
}

#[tokio::test]
async fn expanded_content_larger_than_the_attachment_cap_is_refused() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "note.md", &"[[a]]".repeat(1_900_000))?;
    let server = server_for(&vault)?;

    let refused = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "note.md", "expand_links": true }),
    )
    .await?;

    assert_eq!(refused.is_error, Some(true));
    assert_eq!(code(&refused), Some(&json!("io/too-large")));
    Ok(())
}

#[tokio::test]
async fn an_indexed_file_deleted_outside_the_server_is_not_found_rather_than_linked() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "elsewhere/Gone.md", "gone\n")?;
    write(&vault, "wiki/note.md", "[[Gone]]\n")?;
    let server = server_for(&vault)?;
    let indexed = call_tool(
        server.clone(),
        "fs_attach_file",
        json!({ "path": "wiki/note.md", "expand_links": true }),
    )
    .await?;
    assert_eq!(resource_text(&indexed)?, "[[/elsewhere/Gone.md|Gone]]\n");
    std::fs::remove_file(vault.path().join("elsewhere/Gone.md"))?;

    let result = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "wiki/note.md", "expand_links": true }),
    )
    .await?;

    assert_eq!(resource_text(&result)?, "[[/:!Gone|Gone]]\n");
    Ok(())
}

#[tokio::test]
async fn link_names_with_characters_a_regular_expression_treats_specially_do_not_fail_the_read() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(
        &vault,
        "note.md",
        "[[a<b]] [[Foo\u{a0}Bar]] [x](a>b.md) [[c[d]] [[e^f]]\n",
    )?;
    let server = server_for(&vault)?;

    let result = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "note.md", "expand_links": true }),
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    assert!(resource_text(&result)?.contains("[[/:!a<b|a<b]]"));
    Ok(())
}

#[test]
fn warming_the_file_name_indexes_without_a_runtime_is_harmless() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "note.md", "plain\n")?;
    let server = server_for(&vault)?;

    server.warm_file_name_indexes();
    Ok(())
}

#[tokio::test]
async fn links_resolve_the_same_whether_or_not_the_indexes_were_warmed_first() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "elsewhere/Target.md", "target\n")?;
    write(&vault, "wiki/note.md", "[[Target]]\n")?;
    let server = server_for(&vault)?;
    server.warm_file_name_indexes();

    let result = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "wiki/note.md", "expand_links": true }),
    )
    .await?;

    assert_eq!(resource_text(&result)?, "[[/elsewhere/Target.md|Target]]\n");
    Ok(())
}

#[tokio::test]
async fn a_canvas_has_its_text_node_links_and_file_nodes_expanded_and_says_so() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "boards/Plan.md", "plan\n")?;
    write(
        &vault,
        "boards/plan.canvas",
        r#"{"nodes":[{"id":"t","type":"text","text":"See [[Plan]]","x":0,"y":0,"width":9,"height":9},{"id":"f","type":"file","file":"boards/Plan.md","x":0,"y":0,"width":9,"height":9},{"id":"g","type":"file","file":"gone.md","x":0,"y":0,"width":9,"height":9}],"edges":[]}"#,
    )?;
    let server = server_for(&vault)?;

    let result = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "boards/plan.canvas", "expand_links": true }),
    )
    .await?;
    assert_ne!(result.is_error, Some(true));
    let canvas: Value = serde_json::from_str(&resource_text(&result)?)?;
    assert_eq!(canvas["nodes"][0]["text"], "See [[/boards/Plan.md|Plan]]");
    assert_eq!(canvas["nodes"][1]["file"], "/boards/Plan.md");
    assert_eq!(canvas["nodes"][2]["file"], "/:!gone.md");
    assert_eq!(metadata_block(&result)?, json!({ "links_expanded": true }));
    Ok(())
}

#[tokio::test]
async fn a_canvas_that_cannot_be_read_is_refused_with_the_canvas_code() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "broken.canvas", "{ not json")?;
    let server = server_for(&vault)?;

    let refused = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "broken.canvas", "expand_links": true }),
    )
    .await?;
    assert_eq!(refused.is_error, Some(true));
    assert_eq!(code(&refused), Some(&json!("format/canvas-schema")));
    assert!(refused.content.iter().all(|block| block.as_resource().is_none()));
    Ok(())
}

#[tokio::test]
async fn a_reference_definition_is_expanded_like_an_inline_link() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "wiki/Foo.md", "foo\n")?;
    write(&vault, "wiki/a.md", "See [the foo][f].\n\n[f]: Foo.md\n")?;
    let server = server_for(&vault)?;

    let result = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "wiki/a.md", "expand_links": true }),
    )
    .await?;
    assert_eq!(resource_text(&result)?, "See [the foo][f].\n\n[f]: /wiki/Foo.md\n");
    Ok(())
}

#[tokio::test]
async fn a_name_several_files_answer_to_resolves_inside_the_base_the_note_declares() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(
        &vault,
        "tasks.base",
        "filters:\n  and:\n    - file.inFolder(\"tasks\")\n",
    )?;
    write(&vault, "tasks/mcp/a-2.md", "two\n")?;
    write(&vault, "archive/a-2.md", "old two\n")?;
    write(
        &vault,
        "tasks/web/a-1.md",
        "---\nbase: \"[[tasks.base]]\"\n---\nDepends on [[a-2]].\n",
    )?;
    write(&vault, "notes/loose.md", "Mentions [[a-2]].\n")?;
    let server = server_for(&vault)?;

    let declared = call_tool(
        server.clone(),
        "fs_attach_file",
        json!({ "path": "tasks/web/a-1.md", "expand_links": true }),
    )
    .await?;
    assert!(resource_text(&declared)?.ends_with("Depends on [[/tasks/mcp/a-2.md|a-2]].\n"));

    let undeclared = call_tool(
        server,
        "fs_attach_file",
        json!({ "path": "notes/loose.md", "expand_links": true }),
    )
    .await?;
    assert_eq!(resource_text(&undeclared)?, "Mentions [[/:?a-2|a-2]].\n");
    Ok(())
}
