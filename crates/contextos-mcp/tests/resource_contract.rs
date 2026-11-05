//! Every eligible vault file (not only `.md`) exposed as MCP resources
//! (list and read) for hosts that support resource attachment:
//! root-confined, read-only, and size-capped, excluding any path matching
//! the vault's `hidden` configuration. A listed binary file is not
//! readable via `resources/read` here.
//! Exercised through
//! a real MCP transport (`.claude/rules/mcp-contracts.md`: tests invoke
//! the adapter, not the underlying service).

mod resource_support;

mod support;
use contextos_mcp::{Config, ContextOsServer};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ReadResourceRequestParams, ResourceContents};

use resource_support::{
    BoxError, config_allowing_all_resources, connect, named_uri, named_vault_config,
    named_vault_config_without_resources_list,
};

/// With no `resources_list_include` configured, `resources/list` reports
/// nothing, even though real, otherwise-eligible
/// files exist in the vault. This is the actual fix for the original
/// complaint (a vault holding thousands of files has little value in an
/// eager, unconditional dump); `resources/read` remains fully able to
/// serve any of these files directly, unaffected by this default.
#[tokio::test]
async fn with_no_include_patterns_configured_resources_list_reports_nothing() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "# Note\n")?;
    std::fs::write(vault.path().join("registry.md"), "# Registry\n")?;

    let config = named_vault_config_without_resources_list(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    assert_eq!(
        listed.resources,
        Vec::new(),
        "an unconfigured vault must list nothing, per the empty-include default"
    );

    // `resources/read` still serves the file directly, by hand-built URI,
    // proving the empty list is a discovery-scope decision, not a
    // read-access restriction (the same `hidden` independence already
    // established for the older exclude-list mechanism).
    let uri = named_uri("vault", std::path::Path::new("note.md"));
    let read = client.read_resource(ReadResourceRequestParams::new(uri)).await?;
    match &read.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => assert_eq!(text, "# Note\n"),
        other => {
            return Err(std::io::Error::other(format!("expected text resource contents, got {other:?}")).into());
        }
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// A configured include pattern scopes `resources/list` to only the
/// matching files, leaving other real, otherwise-eligible
/// files in the same vault unlisted, the "targeted list of selected
/// files" the allowlist exists to produce.
#[tokio::test]
async fn resources_list_only_enumerates_files_matching_configured_include_patterns() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("registry"))?;
    std::fs::write(vault.path().join("registry/skill.md"), "# Skill\n")?;
    std::fs::create_dir_all(vault.path().join("journal"))?;
    std::fs::write(vault.path().join("journal/entry.md"), "# Entry\n")?;

    let config = Config::try_from(
        format!(
            "[[vault]]\npath = {:?}\nstate_directory = \".contextos\"\n\
             resources_list_include = [\"registry/**\"]\n",
            vault.path()
        )
        .as_str(),
    )?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let names: Vec<&str> = listed.resources.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["registry/skill.md"],
        "only the configured registry/** pattern should be enumerated; \
         journal/entry.md is a real, eligible file but outside every \
         configured pattern"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// The `hidden` exclusion still applies within a configured
/// `resources_list_include` allowlist: the two pattern axes stay
/// independent.
#[tokio::test]
async fn a_broad_include_pattern_still_excludes_hidden_patterns() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "# Note\n")?;
    std::fs::create_dir_all(vault.path().join("nested"))?;
    std::fs::write(vault.path().join("nested/child.md"), "# Child\n")?;
    std::fs::write(vault.path().join("not-markdown.txt"), "ignore me")?;
    std::fs::create_dir_all(vault.path().join(".contextos"))?;
    std::fs::write(vault.path().join(".contextos/derived.md"), "# derived\n")?;
    std::fs::create_dir_all(vault.path().join(".git"))?;
    std::fs::write(vault.path().join(".git/COMMIT_EDITMSG.md"), "# git\n")?;

    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let mut names: Vec<String> = listed.resources.iter().map(|r| r.name.clone()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "nested/child.md".to_owned(),
            "not-markdown.txt".to_owned(),
            "note.md".to_owned(),
        ],
        "not-markdown.txt is eligible; .contextos and .git stay \
         hidden even under a broad **/* include pattern"
    );
    for resource in &listed.resources {
        let is_markdown = std::path::Path::new(&resource.name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
        let expected_mime = if is_markdown { "text/markdown" } else { "text/plain" };
        assert_eq!(resource.mime_type.as_deref(), Some(expected_mime));
        assert!(
            resource.uri.starts_with("vault://"),
            "unexpected resource uri: {}",
            resource.uri
        );
        assert!(resource.size.is_some());
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn a_directory_is_never_listed_as_a_resource() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("notes"))?;
    std::fs::write(vault.path().join("notes/note.md"), "# Note\n")?;

    let config = config_allowing_all_resources(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let names: Vec<&str> = listed.resources.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["notes/note.md"],
        "the 'notes' directory itself must never appear as a resource"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn resources_read_serves_a_png_as_base64_blob_with_correct_mime_type() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("image.png"), [0_u8, 1, 2, 3])?;

    let config = config_allowing_all_resources(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let resource = listed
        .resources
        .iter()
        .find(|resource| resource.name == "image.png")
        .ok_or_else(|| std::io::Error::other("image.png was not listed"))?;
    assert_eq!(resource.mime_type.as_deref(), Some("image/png"));

    let read = client
        .read_resource(ReadResourceRequestParams::new(resource.uri.clone()))
        .await?;
    assert_eq!(read.contents.len(), 1);
    match &read.contents[0] {
        ResourceContents::BlobResourceContents { blob, mime_type, .. } => {
            assert_eq!(blob, "AAECAw==");
            assert_eq!(mime_type.as_deref(), Some("image/png"));
        }
        other => {
            return Err(std::io::Error::other(format!("expected blob resource contents, got {other:?}")).into());
        }
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn resources_read_serves_an_unknown_extension_as_octet_stream_blob() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("mystery.zzzznotreal"), [0_u8, 159, 146, 150])?;

    let config = config_allowing_all_resources(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let resource = listed
        .resources
        .iter()
        .find(|resource| resource.name == "mystery.zzzznotreal")
        .ok_or_else(|| std::io::Error::other("mystery.zzzznotreal was not listed"))?;
    assert_eq!(
        resource.mime_type.as_deref(),
        Some("application/octet-stream"),
        "no format allow-list: an unrecognised extension still gets listed and read"
    );

    let read = client
        .read_resource(ReadResourceRequestParams::new(resource.uri.clone()))
        .await?;
    assert!(matches!(
        &read.contents[0],
        ResourceContents::BlobResourceContents { mime_type, .. }
            if mime_type.as_deref() == Some("application/octet-stream")
    ));

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn resources_read_binary_content_matches_fs_attach_files_existing_encoding() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("image.png"), [0_u8, 1, 2, 3, 255, 254])?;

    let config = config_allowing_all_resources(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let resource_uri = listed
        .resources
        .iter()
        .find(|resource| resource.name == "image.png")
        .ok_or_else(|| std::io::Error::other("image.png was not listed"))?
        .uri
        .clone();
    let read = client
        .read_resource(ReadResourceRequestParams::new(resource_uri))
        .await?;
    let (resource_blob, resource_mime) = match &read.contents[0] {
        ResourceContents::BlobResourceContents { blob, mime_type, .. } => (blob.clone(), mime_type.clone()),
        other => {
            return Err(std::io::Error::other(format!("expected blob resource contents, got {other:?}")).into());
        }
    };

    let arguments = serde_json::json!({ "path": "image.png" })
        .as_object()
        .cloned()
        .ok_or_else(|| std::io::Error::other("expected a JSON object"))?;
    let attached = client
        .call_tool(CallToolRequestParams::new("fs_attach_file").with_arguments(arguments))
        .await?;
    let attached_resource = attached
        .content
        .first()
        .and_then(rmcp::model::ContentBlock::as_resource)
        .ok_or_else(|| std::io::Error::other("fs_attach_file did not embed a resource"))?;
    let (attached_blob, attached_mime) = match &attached_resource.resource {
        ResourceContents::BlobResourceContents { blob, mime_type, .. } => (blob.clone(), mime_type.clone()),
        other => {
            return Err(std::io::Error::other(format!("expected blob resource contents, got {other:?}")).into());
        }
    };

    assert_eq!(
        resource_blob, attached_blob,
        "resources/read and fs_attach_file must encode identical bytes for the same file"
    );
    assert_eq!(resource_mime, attached_mime);

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn resources_read_rejects_a_binary_file_over_the_attachment_size_cap() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let oversized = std::fs::File::create(vault.path().join("oversized.bin"))?;
    oversized.set_len(10 * 1024 * 1024 + 1)?;

    // The configurable text cap defaults to 5 MB and would reject
    // this file before binary detection even runs; raise it so the read
    // reaches binary detection and actually exercises `read_attachment`'s
    // own, independent, fixed 10 MiB cap.
    let source = format!(
        "[[vault]]\npath = {:?}\nstate_directory = \".contextos\"\n\
         resources_list_include = [\"**/*\"]\n[vault.limits]\nmax_read_mb = 15\n",
        vault.path()
    );
    let config = Config::try_from(source.as_str())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let resource = listed
        .resources
        .iter()
        .find(|resource| resource.name == "oversized.bin")
        .ok_or_else(|| std::io::Error::other("oversized.bin was not listed"))?;
    let result = client
        .read_resource(ReadResourceRequestParams::new(resource.uri.clone()))
        .await;
    assert!(
        result.is_err(),
        "a binary resource over read_attachment's fixed 10 MiB cap must be rejected"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn fs_attach_file_and_resources_read_agree_on_the_uri_for_the_same_path() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "# Note\n")?;

    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let listed_uri = listed
        .resources
        .first()
        .ok_or_else(|| std::io::Error::other("no resources listed"))?
        .uri
        .clone();

    let arguments = serde_json::json!({ "path": "note.md" })
        .as_object()
        .cloned()
        .ok_or_else(|| std::io::Error::other("expected a JSON object"))?;
    let attached = client
        .call_tool(CallToolRequestParams::new("fs_attach_file").with_arguments(arguments))
        .await?;
    let attached_resource = attached
        .content
        .first()
        .and_then(rmcp::model::ContentBlock::as_resource)
        .ok_or_else(|| std::io::Error::other("fs_attach_file did not embed a resource"))?;
    let attached_uri = match &attached_resource.resource {
        ResourceContents::TextResourceContents { uri, .. } | ResourceContents::BlobResourceContents { uri, .. } => {
            uri.clone()
        }
        other => {
            return Err(std::io::Error::other(format!("unexpected resource contents: {other:?}")).into());
        }
    };

    assert!(attached_uri.starts_with("vault://"));
    assert_eq!(
        attached_uri, listed_uri,
        "fs_attach_file and resources/read must address the same file with the same URI"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn resources_read_returns_the_exact_file_content_by_the_listed_uri() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "# Note\n\nBody text.\n")?;

    let config = config_allowing_all_resources(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let resource = listed
        .resources
        .first()
        .ok_or_else(|| std::io::Error::other("no resources listed"))?;
    let read = client
        .read_resource(ReadResourceRequestParams::new(resource.uri.clone()))
        .await?;
    assert_eq!(read.contents.len(), 1);
    match &read.contents[0] {
        ResourceContents::TextResourceContents {
            text, mime_type, uri, ..
        } => {
            assert_eq!(text, "# Note\n\nBody text.\n");
            assert_eq!(mime_type.as_deref(), Some("text/markdown"));
            assert_eq!(uri, &resource.uri);
        }
        other => {
            return Err(std::io::Error::other(format!("expected text resource contents, got {other:?}")).into());
        }
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// `resources/read` on a directory URI returns that directory's own
/// immediate children as a JSON `{"entries": [...]}` payload, letting a
/// client browse a vault level-by-level without ever calling the eager
/// `resources/list` walk. `{name}://.` addresses the vault root, the same
/// shortcut `VaultPath::try_from_vault_selector` already recognises.
#[tokio::test]
async fn resources_read_on_the_vault_root_lists_its_immediate_children() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "# Note\n")?;
    std::fs::create_dir_all(vault.path().join("notes"))?;
    std::fs::write(vault.path().join("notes/nested.md"), "# Nested\n")?;

    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let uri = named_uri("vault", std::path::Path::new("."));
    let read = client.read_resource(ReadResourceRequestParams::new(uri)).await?;
    assert_eq!(read.contents.len(), 1);
    let (text, mime_type) = match &read.contents[0] {
        ResourceContents::TextResourceContents { text, mime_type, .. } => (text.clone(), mime_type.clone()),
        other => {
            return Err(std::io::Error::other(format!("expected text resource contents, got {other:?}")).into());
        }
    };
    assert_eq!(mime_type.as_deref(), Some("application/json"));
    let body: serde_json::Value = serde_json::from_str(&text)?;
    let mut entries: Vec<(String, String, String)> = body["entries"]
        .as_array()
        .ok_or_else(|| std::io::Error::other("expected an entries array"))?
        .iter()
        .map(|entry| {
            (
                entry["name"].as_str().unwrap_or_default().to_owned(),
                entry["kind"].as_str().unwrap_or_default().to_owned(),
                entry["uri"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    entries.sort_unstable();
    assert_eq!(
        entries,
        vec![
            ("note.md".to_owned(), "file".to_owned(), "vault://note.md".to_owned()),
            ("notes".to_owned(), "dir".to_owned(), "vault://notes".to_owned()),
        ],
        "only the root's own immediate children, not a recursive walk into 'notes'"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// A nested directory read returns only that directory's own children,
/// never recursing further: recursion is `fs_directory_tree`'s job, not
/// this one.
#[tokio::test]
async fn resources_read_on_a_nested_directory_lists_only_its_own_children() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("nested/deeper"))?;
    std::fs::write(vault.path().join("nested/child.md"), "# Child\n")?;
    std::fs::write(vault.path().join("nested/deeper/grandchild.md"), "# Grandchild\n")?;

    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let uri = named_uri("vault", std::path::Path::new("nested"));
    let read = client.read_resource(ReadResourceRequestParams::new(uri)).await?;
    let text = match &read.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => text.clone(),
        other => {
            return Err(std::io::Error::other(format!("expected text resource contents, got {other:?}")).into());
        }
    };
    let body: serde_json::Value = serde_json::from_str(&text)?;
    let mut names: Vec<String> = body["entries"]
        .as_array()
        .ok_or_else(|| std::io::Error::other("expected an entries array"))?
        .iter()
        .map(|entry| entry["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec!["child.md".to_owned(), "deeper".to_owned()],
        "'grandchild.md' must not appear: a directory read is one level deep, not recursive"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// A directory resource read excludes hidden children exactly as
/// `fs_list_directory` already does (the same underlying domain call):
/// direct access to the directory itself is unaffected by `hidden`, but its
/// listed contents still honour it.
#[tokio::test]
async fn resources_read_on_a_directory_excludes_hidden_children() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "# Note\n")?;
    std::fs::create_dir_all(vault.path().join(".git"))?;
    std::fs::write(vault.path().join(".git/COMMIT_EDITMSG"), "hidden")?;

    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let uri = named_uri("vault", std::path::Path::new("."));
    let read = client.read_resource(ReadResourceRequestParams::new(uri)).await?;
    let text = match &read.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => text.clone(),
        other => {
            return Err(std::io::Error::other(format!("expected text resource contents, got {other:?}")).into());
        }
    };
    let body: serde_json::Value = serde_json::from_str(&text)?;
    let names: Vec<String> = body["entries"]
        .as_array()
        .ok_or_else(|| std::io::Error::other("expected an entries array"))?
        .iter()
        .map(|entry| entry["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        names,
        vec!["note.md".to_owned()],
        ".git must stay hidden from a direct directory read, matching fs_list_directory"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// An empty directory reads as an empty `entries` array, not an error.
#[tokio::test]
async fn resources_read_on_an_empty_directory_returns_no_entries() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("empty"))?;

    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let uri = named_uri("vault", std::path::Path::new("empty"));
    let read = client.read_resource(ReadResourceRequestParams::new(uri)).await?;
    let text = match &read.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => text.clone(),
        other => {
            return Err(std::io::Error::other(format!("expected text resource contents, got {other:?}")).into());
        }
    };
    let body: serde_json::Value = serde_json::from_str(&text)?;
    assert_eq!(body["entries"].as_array().map(Vec::len), Some(0));

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// A non-existent path is still rejected, not silently treated as an empty
/// directory: the new directory branch only ever engages on `NotFile`, an
/// actually-existing non-regular-file path.
#[tokio::test]
async fn resources_read_of_a_nonexistent_path_is_still_rejected() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;

    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let uri = named_uri("vault", std::path::Path::new("missing"));
    let result = client.read_resource(ReadResourceRequestParams::new(uri)).await;
    assert!(
        result.is_err(),
        "a non-existent path must still be rejected, not treated as an empty directory"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn a_resource_uri_outside_every_configured_root_is_rejected() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let outside = tempfile::tempdir()?;
    std::fs::write(outside.path().join("secret.md"), "# secret\n")?;

    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let escaping_uri = named_uri("vault", &outside.path().join("secret.md"));
    let result = client.read_resource(ReadResourceRequestParams::new(escaping_uri)).await;
    assert!(
        result.is_err(),
        "reading a resource outside every configured root must fail, not succeed"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_resource_reached_only_through_a_symlink_escaping_the_root_is_rejected() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let outside = tempfile::tempdir()?;
    std::fs::write(outside.path().join("secret.md"), "# secret\n")?;
    std::os::unix::fs::symlink(outside.path(), vault.path().join("linked-outside"))?;

    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let uri = named_uri("vault", &vault.path().join("linked-outside/secret.md"));
    let result = client.read_resource(ReadResourceRequestParams::new(uri)).await;
    assert!(
        result.is_err(),
        "a resource reached only through a symlink escaping the vault root must be rejected"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn a_resource_over_the_configured_size_cap_is_rejected_without_partial_content() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let big = "a".repeat(2 * 1024 * 1024);
    std::fs::write(vault.path().join("big.md"), &big)?;
    let source = format!(
        "[[vault]]\npath = {:?}\nname = \"vault\"\nstate_directory = \".contextos\"\n\
         [vault.limits]\nmax_read_mb = 1\n",
        vault.path()
    );
    let config = Config::try_from(source.as_str())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let uri = named_uri("vault", &vault.path().join("big.md"));
    let result = client.read_resource(ReadResourceRequestParams::new(uri)).await;
    assert!(
        result.is_err(),
        "a resource over the configured size cap must be rejected, not truncated"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn get_info_advertises_the_resources_capability() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let peer_info = client
        .peer_info()
        .ok_or_else(|| std::io::Error::other("no peer info negotiated"))?;
    assert!(peer_info.capabilities.resources.is_some());
    assert!(peer_info.capabilities.tools.is_some());

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// `mcp-contracts.md` checklist item 7: parity across transports.
#[tokio::test]
async fn resources_are_reachable_over_the_streamable_http_transport_too() -> Result<(), BoxError> {
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "# Note\n")?;
    let config = config_allowing_all_resources(vault.path())?;
    let server = ContextOsServer::try_from(config)?;

    let token = "resource-parity-token";
    let http = contextos_mcp::HttpConfig {
        bind: "127.0.0.1:0".to_owned(),
        token: token.to_owned(),
        token_env: String::new(),
        max_body_kb: 2048,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = contextos_mcp::build_router(server, &http)?;
    let shutdown = CancellationToken::new();
    let shutdown_for_serve = shutdown.clone();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown_for_serve.cancelled().await })
            .await;
    });
    let url = format!("http://{addr}{}", contextos_mcp::HTTP_MOUNT_PATH);

    let client_config = StreamableHttpClientTransportConfig::with_uri(url).auth_header(token.to_owned());
    let transport = StreamableHttpClientTransport::from_config(client_config);
    let client = ().serve(transport).await?;

    let listed = client.list_resources(None).await?;
    assert_eq!(listed.resources.len(), 1);
    let read = client
        .read_resource(ReadResourceRequestParams::new(listed.resources[0].uri.clone()))
        .await?;
    match &read.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => {
            assert_eq!(text, "# Note\n");
        }
        other => {
            return Err(std::io::Error::other(format!("expected text resource contents, got {other:?}")).into());
        }
    }

    client.cancel().await?;
    shutdown.cancel();
    handle.await?;
    Ok(())
}
