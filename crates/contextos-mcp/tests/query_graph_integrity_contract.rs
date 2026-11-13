//! The `unresolved` and `unlinked` operations of `query_graph`: the links that resolve to nothing or to several
//! files, and the notes no other note links to, with paging, a folder scope, the excluded source folders, and the
//! requests the tool refuses.

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
        .call_tool(CallToolRequestParams::new("query_graph").with_arguments(arguments))
        .await;
    client.close().await?;
    server_handle.await??;
    Ok(result?)
}

fn content(result: &CallToolResult) -> Result<&Map<String, Value>, TestError> {
    result
        .structured_content
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| std::io::Error::other("query_graph omitted structured content").into())
}

fn list<'a>(content: &'a Map<String, Value>, key: &str) -> Result<&'a Vec<Value>, TestError> {
    content
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| std::io::Error::other(format!("query_graph omitted {key}")).into())
}

fn assert_stable_error(result: &CallToolResult, code: &str) -> TestResult {
    assert_eq!(result.is_error, Some(true));
    let content = content(result)?;
    assert_eq!(content.get("code"), Some(&json!(code)), "{content:?}");
    assert!(
        content
            .get("remediation")
            .and_then(Value::as_str)
            .is_some_and(|remediation| !remediation.is_empty()),
        "an error must carry a remediation hint"
    );
    Ok(())
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

fn broken_vault() -> Result<tempfile::TempDir, TestError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    put(&vault, "wiki/a.md", "intro\n[[gone|Gone]]\n[x](../raw/old.md)\n")?;
    put(&vault, "wiki/b.md", "![[nothing.png]] and [[status]]\n")?;
    put(&vault, "one/status.md", "x\n")?;
    put(&vault, "two/status.md", "x\n")?;
    put(&vault, "templates/t.md", "[[template-only-gap]]\n")?;
    Ok(vault)
}

#[tokio::test]
async fn unresolved_lists_each_broken_link_with_its_line_text_target_form_and_kind() -> TestResult {
    let vault = broken_vault()?;

    let result = call_tool(server_over(&vault)?, json!({"operation": "unresolved"})).await?;

    let content = content(&result)?;
    assert_eq!(content.get("total"), Some(&json!(4)));
    assert_eq!(content.get("offset"), Some(&json!(0)));
    assert_eq!(content.get("nodes"), None);
    assert_eq!(
        list(content, "links")?,
        &vec![
            json!({"source": "wiki/a.md", "line": 2, "text": "[[gone|Gone]]", "target": "gone",
                   "form": "wikilink", "kind": "not-found"}),
            json!({"source": "wiki/a.md", "line": 3, "text": "[x](../raw/old.md)", "target": "../raw/old.md",
                   "form": "markdown", "kind": "not-found"}),
            json!({"source": "wiki/b.md", "line": 1, "text": "![[nothing.png]]", "target": "nothing.png",
                   "form": "wikilink-embed", "kind": "not-found"}),
            json!({"source": "wiki/b.md", "line": 1, "text": "[[status]]", "target": "status",
                   "form": "wikilink", "kind": "ambiguous", "candidates": ["one/status.md", "two/status.md"]}),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn unresolved_skips_the_default_excluded_source_folders() -> TestResult {
    let vault = broken_vault()?;

    let result = call_tool(server_over(&vault)?, json!({"operation": "unresolved"})).await?;

    let sources: Vec<_> = list(content(&result)?, "links")?
        .iter()
        .filter_map(|link| link.get("source").and_then(Value::as_str))
        .collect();
    assert!(
        !sources.iter().any(|source| source.starts_with("templates/")),
        "{sources:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_configured_link_exclusion_list_replaces_the_default() -> TestResult {
    let vault = broken_vault()?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].links.exclude = vec!["wiki".to_owned()];
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, json!({"operation": "unresolved"})).await?;

    let sources: Vec<_> = list(content(&result)?, "links")?
        .iter()
        .filter_map(|link| link.get("source").and_then(Value::as_str))
        .collect();
    assert_eq!(sources, vec!["templates/t.md"]);
    Ok(())
}

#[tokio::test]
async fn unresolved_leaves_out_links_whose_target_lies_in_a_folder_not_checked_as_link_targets() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("journal"))?;
    std::fs::write(vault.path().join("journal/day.md"), "[[inbox/gone]]\n[[kept/gone]]\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].links.ignore_targets = vec!["inbox".to_owned()];
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, json!({"operation": "unresolved"})).await?;

    let body = content(&result)?;
    let targets: Vec<_> = list(body, "links")?
        .iter()
        .filter_map(|link| link.get("target").and_then(Value::as_str))
        .collect();
    assert_eq!(targets, vec!["kept/gone"]);
    assert_eq!(body.get("total"), Some(&json!(1)));
    Ok(())
}

#[tokio::test]
async fn the_query_graph_description_names_both_link_settings() -> TestResult {
    let catalogue = ContextOsServer::catalogue().list_all();
    let description = catalogue
        .iter()
        .find(|tool| tool.name == "query_graph")
        .and_then(|tool| tool.description.as_deref())
        .ok_or("query_graph is not advertised")?;

    assert!(description.contains("`[vault.links] exclude`"), "{description}");
    assert!(description.contains("`[vault.links] ignore_targets`"), "{description}");
    Ok(())
}

#[tokio::test]
async fn unresolved_pages_with_offset_and_limit_and_reports_the_total_of_the_whole() -> TestResult {
    let vault = broken_vault()?;
    let server = server_over(&vault)?;

    let page = call_tool(
        server.clone(),
        json!({"operation": "unresolved", "offset": 1, "limit": 2}),
    )
    .await?;
    let only_total = call_tool(server, json!({"operation": "unresolved", "limit": 0})).await?;

    let page = content(&page)?;
    assert_eq!(page.get("total"), Some(&json!(4)));
    assert_eq!(page.get("offset"), Some(&json!(1)));
    let lines: Vec<_> = list(page, "links")?.iter().map(|link| link["text"].clone()).collect();
    assert_eq!(lines, vec![json!("[x](../raw/old.md)"), json!("![[nothing.png]]")]);
    let only_total = content(&only_total)?;
    assert_eq!(only_total.get("total"), Some(&json!(4)));
    assert_eq!(list(only_total, "links")?.len(), 0);
    Ok(())
}

#[tokio::test]
async fn unresolved_is_limited_to_a_folder_by_path() -> TestResult {
    let vault = broken_vault()?;

    let result = call_tool(server_over(&vault)?, json!({"operation": "unresolved", "path": "wiki"})).await?;

    assert_eq!(content(&result)?.get("total"), Some(&json!(4)));
    let none = call_tool(server_over(&vault)?, json!({"operation": "unresolved", "path": "one"})).await?;
    assert_eq!(content(&none)?.get("total"), Some(&json!(0)));
    Ok(())
}

#[tokio::test]
async fn an_unreadable_note_is_listed_once_with_the_reason_and_no_line() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    put(&vault, "bad.md", "text\n[[unclosed\n")?;

    let result = call_tool(server_over(&vault)?, json!({"operation": "unresolved"})).await?;

    let links = list(content(&result)?, "links")?;
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["kind"], json!("unreadable"));
    assert_eq!(links[0]["source"], json!("bad.md"));
    assert!(
        links[0]["text"].as_str().is_some_and(|text| text.contains("line 2")),
        "{}",
        links[0]
    );
    for absent in ["line", "target", "form", "candidates"] {
        assert_eq!(links[0].get(absent), None, "{absent}");
    }
    Ok(())
}

#[tokio::test]
async fn unlinked_lists_notes_no_other_note_links_to_sorted_and_paged() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    put(&vault, "a.md", "# Alpha\n[b](b.md)\n")?;
    put(&vault, "b.md", "x\n")?;
    put(&vault, "wiki/c.md", "x\n")?;
    put(&vault, "wiki/index.md", "x\n")?;
    put(&vault, "templates/t.md", "[[b]] [[wiki/c]]\n")?;
    let server = server_over(&vault)?;

    let all = call_tool(server.clone(), json!({"operation": "unlinked"})).await?;
    let scoped = call_tool(server.clone(), json!({"operation": "unlinked", "path": "wiki"})).await?;
    let paged = call_tool(server, json!({"operation": "unlinked", "offset": 1, "limit": 1})).await?;

    let all = content(&all)?;
    assert_eq!(
        list(all, "notes")?,
        &vec![
            json!({"path": "a.md", "title": "Alpha"}),
            json!({"path": "wiki/c.md", "title": "c"})
        ]
    );
    assert_eq!(all.get("total"), Some(&json!(2)));
    assert_eq!(all.get("links"), None);
    assert_eq!(all.get("nodes"), None);
    let scoped = content(&scoped)?;
    assert_eq!(list(scoped, "notes")?.len(), 1);
    assert_eq!(scoped.get("total"), Some(&json!(1)));
    let paged = content(&paged)?;
    assert_eq!(paged.get("offset"), Some(&json!(1)));
    assert_eq!(list(paged, "notes")?, &vec![json!({"path": "wiki/c.md", "title": "c"})]);
    Ok(())
}

#[tokio::test]
async fn the_link_report_operations_refuse_what_they_cannot_honour() -> TestResult {
    let vault = broken_vault()?;
    let server = server_over(&vault)?;

    for operation in ["unresolved", "unlinked"] {
        for rejected in [
            json!({"operation": operation, "limit": 1001}),
            json!({"operation": operation, "limit": -1}),
            json!({"operation": operation, "offset": -1}),
            json!({"operation": operation, "from": "wiki/a.md"}),
            json!({"operation": operation, "to": "wiki/a.md"}),
            json!({"operation": operation, "depth": 2}),
            json!({"operation": operation, "direction": "out"}),
        ] {
            let result = call_tool(server.clone(), rejected.clone()).await?;
            assert_stable_error(&result, "index/invalid-query").map_err(|error| format!("{rejected}: {error}"))?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn the_traversal_operations_refuse_the_link_report_controls() -> TestResult {
    let vault = broken_vault()?;
    let server = server_over(&vault)?;

    for rejected in [
        json!({"operation": "orphans", "path": "wiki"}),
        json!({"operation": "neighbours", "from": "wiki/a.md", "limit": 5}),
        json!({"operation": "backlinks", "from": "wiki/a.md", "offset": 1}),
    ] {
        let result = call_tool(server.clone(), rejected).await?;
        assert_stable_error(&result, "index/invalid-query")?;
    }
    Ok(())
}

#[tokio::test]
async fn an_unknown_field_is_refused() -> TestResult {
    let vault = broken_vault()?;

    let result = call_tool(
        server_over(&vault)?,
        json!({"operation": "unresolved", "folder": "wiki"}),
    )
    .await;

    assert!(result.is_err() || result?.is_error == Some(true));
    Ok(())
}

#[tokio::test]
async fn the_description_names_the_link_integrity_operations() -> TestResult {
    let tool = ContextOsServer::catalogue()
        .list_all()
        .into_iter()
        .find(|tool| tool.name == "query_graph")
        .ok_or_else(|| std::io::Error::other("query_graph is not advertised"))?;

    let description = tool.description.unwrap_or_default();

    for operation in ["unresolved", "unlinked", "orphans"] {
        assert!(description.contains(operation), "{description}");
    }
    Ok(())
}

#[tokio::test]
async fn an_edit_made_outside_the_server_shows_in_the_next_report() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    put(&vault, "a.md", "[[b]]\n")?;
    put(&vault, "b.md", "x\n")?;
    let server = server_over(&vault)?;
    let before = call_tool(server.clone(), json!({"operation": "unresolved"})).await?;
    assert_eq!(content(&before)?.get("total"), Some(&json!(0)));

    std::fs::remove_file(vault.path().join("b.md"))?;

    let after = call_tool(server, json!({"operation": "unresolved"})).await?;
    assert_eq!(content(&after)?.get("total"), Some(&json!(1)));
    Ok(())
}

fn two_named_vaults() -> Result<(tempfile::TempDir, tempfile::TempDir, ContextOsServer), TestError> {
    let first = tempfile::Builder::new().prefix("vault").tempdir()?;
    let second = tempfile::Builder::new().prefix("vault").tempdir()?;
    put(&first, "wiki/a.md", "[[gone]]\n")?;
    put(&second, "wiki/b.md", "[[gone]]\n")?;
    let mut config = support::scoped_config(vec![first.path().to_path_buf(), second.path().to_path_buf()])?;
    for (vault, name) in config.vaults.iter_mut().zip(["alpha", "beta"]) {
        vault.name = Some(name.to_owned());
        vault.index_md.enabled = false;
    }
    Ok((first, second, ContextOsServer::try_from(config)?))
}

#[tokio::test]
async fn a_scope_that_escapes_the_vault_is_refused_with_its_path_code() -> TestResult {
    let vault = broken_vault()?;
    let server = server_over(&vault)?;

    for operation in ["unresolved", "unlinked"] {
        for (scope, code) in [
            ("../outside", "path/outside-root"),
            ("wiki/../../outside", "path/outside-root"),
            ("/abs", "path/outside-root"),
            // A drive path is a real path on Windows, and outside the vault; anywhere else it cannot be one.
            (
                "C:\\Windows",
                if cfg!(windows) {
                    "path/outside-root"
                } else {
                    "path/invalid-windows-path"
                },
            ),
            ("wiki/a.md:stream", "path/invalid-windows-path"),
        ] {
            let result = call_tool(server.clone(), json!({"operation": operation, "path": scope})).await?;
            assert_stable_error(&result, code).map_err(|error| format!("{operation} {scope:?}: {error}"))?;
        }
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_scope_that_is_a_symlink_out_of_the_vault_is_refused() -> TestResult {
    let vault = broken_vault()?;
    let outside = tempfile::Builder::new().prefix("outside").tempdir()?;
    std::os::unix::fs::symlink(outside.path(), vault.path().join("escape"))?;

    let result = call_tool(
        server_over(&vault)?,
        json!({"operation": "unresolved", "path": "escape"}),
    )
    .await?;

    assert_stable_error(&result, "path/symlink-escape")
}

#[tokio::test]
async fn a_scope_naming_a_folder_that_does_not_exist_is_refused_rather_than_answered_empty() -> TestResult {
    let vault = broken_vault()?;
    let server = server_over(&vault)?;

    for operation in ["unresolved", "unlinked"] {
        let result = call_tool(server.clone(), json!({"operation": operation, "path": "wkii"})).await?;
        assert_stable_error(&result, "path/not-found")?;
    }
    Ok(())
}

#[tokio::test]
async fn a_scope_naming_a_file_is_refused_as_not_a_directory() -> TestResult {
    let vault = broken_vault()?;
    let server = server_over(&vault)?;

    for operation in ["unresolved", "unlinked"] {
        let result = call_tool(server.clone(), json!({"operation": operation, "path": "wiki/a.md"})).await?;
        assert_stable_error(&result, "path/not-a-directory")?;
    }
    Ok(())
}

#[tokio::test]
async fn a_scope_in_the_named_vault_form_limits_the_report_to_that_folder() -> TestResult {
    let (_first, _second, server) = two_named_vaults()?;

    let result = call_tool(
        server,
        json!({"operation": "unresolved", "vault": "alpha://.", "path": "alpha://wiki"}),
    )
    .await?;

    let content = content(&result)?;
    assert_eq!(content.get("total"), Some(&json!(1)), "{content:?}");
    assert_eq!(list(content, "links")?[0]["source"], json!("wiki/a.md"));
    Ok(())
}

#[tokio::test]
async fn a_scope_in_another_vault_than_the_one_queried_is_refused() -> TestResult {
    let (_first, _second, server) = two_named_vaults()?;

    for operation in ["unresolved", "unlinked"] {
        let result = call_tool(
            server.clone(),
            json!({"operation": operation, "vault": "alpha://.", "path": "beta://wiki"}),
        )
        .await?;
        assert_stable_error(&result, "index/invalid-query")?;
    }
    Ok(())
}

#[tokio::test]
async fn a_scope_naming_the_vault_root_limits_nothing() -> TestResult {
    let vault = broken_vault()?;

    let result = call_tool(server_over(&vault)?, json!({"operation": "unresolved", "path": "."})).await?;

    assert_eq!(content(&result)?.get("total"), Some(&json!(4)));
    Ok(())
}

#[tokio::test]
async fn the_traversal_operations_refuse_every_link_report_control_even_when_it_is_the_default() -> TestResult {
    let vault = broken_vault()?;
    let server = server_over(&vault)?;

    for operation in ["neighbours", "backlinks", "path", "orphans"] {
        for control in [json!({"path": "wiki"}), json!({"offset": 0}), json!({"limit": 200})] {
            let mut arguments = json!({"operation": operation, "from": "wiki/a.md", "to": "wiki/b.md"});
            if let (Some(arguments), Some(control)) = (arguments.as_object_mut(), control.as_object()) {
                arguments.extend(control.clone());
            }
            let result = call_tool(server.clone(), arguments.clone()).await?;
            assert_stable_error(&result, "index/invalid-query").map_err(|error| format!("{arguments}: {error}"))?;
        }
    }
    Ok(())
}
