//! Only contextos-web writes under `.contextos/packages/`: every mutating tool is refused there with
//! `packages/server-owned` before anything changes, whatever `force` says, while the rest of `.contextos/` and the
//! registry stay ordinary content.
mod support;

use contextos_mcp::ContextOsServer;
use serde_json::{Value, json};
use support::git_harness::{call, harness_with};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const RELEASE_MANIFEST: &str = ".contextos/packages/demo/1.0.0/package.toml";
const RELEASE_NOTE: &str = ".contextos/packages/demo/1.0.0/README.md";
const MANIFEST_TEXT: &str = "name = \"demo\"\nversion = \"1.0.0\"\n";

fn vault_with_a_package() -> Result<(tempfile::TempDir, ContextOsServer), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join(".contextos/packages/demo/1.0.0"))?;
    std::fs::write(vault.path().join(RELEASE_MANIFEST), MANIFEST_TEXT)?;
    std::fs::write(vault.path().join(RELEASE_NOTE), "# Demo\n")?;
    std::fs::write(vault.path().join(".contextos/jobs.toml"), "# jobs\n")?;
    std::fs::create_dir_all(vault.path().join("registry/jobs"))?;
    std::fs::write(vault.path().join("registry/jobs/note.md"), "# Job\n")?;
    std::fs::write(vault.path().join("outside.md"), "# Outside\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.destructive_delete = true;
    let server = ContextOsServer::try_from(config)?;
    Ok((vault, server))
}

async fn refused_code(
    server: &ContextOsServer,
    tool: &'static str,
    arguments: Value,
) -> Result<Option<String>, BoxError> {
    let result = call(server, tool, arguments).await?;
    assert_eq!(result.is_error, Some(true), "{tool} must be refused: {result:?}");
    Ok(result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("code"))
        .and_then(Value::as_str)
        .map(str::to_owned))
}

async fn current_hash(server: &ContextOsServer, path: &str) -> Result<String, BoxError> {
    let info = call(server, "fs_get_file_info", json!({"path": path})).await?;
    Ok(info
        .structured_content
        .as_ref()
        .and_then(|content| content.get("content_hash"))
        .and_then(Value::as_str)
        .ok_or("no content_hash")?
        .to_owned())
}

#[tokio::test]
async fn every_mutating_tool_is_refused_under_the_package_store_and_changes_nothing() -> Result<(), BoxError> {
    let (vault, server) = vault_with_a_package()?;
    let hash = current_hash(&server, RELEASE_NOTE).await?;
    let attempts: Vec<(&'static str, Value)> = vec![
        (
            "fs_write_file",
            json!({"path": ".contextos/packages/demo/1.0.0/new.toml", "content": "x"}),
        ),
        (
            "fs_write_file",
            json!({"path": RELEASE_MANIFEST, "content": "x", "force": true}),
        ),
        (
            "fs_edit_file",
            json!({"path": RELEASE_MANIFEST, "edits": [{"old_text": "demo", "new_text": "forged"}]}),
        ),
        (
            "fs_move_file",
            json!({"source": RELEASE_MANIFEST, "destination": "elsewhere.toml"}),
        ),
        (
            "fs_move_file",
            json!({"source": "outside.md", "destination": ".contextos/packages/outside.md"}),
        ),
        ("fs_delete_file", json!({"path": RELEASE_MANIFEST})),
        (
            "fs_delete_file",
            json!({"path": ".contextos/packages/demo", "hard": true}),
        ),
        ("fs_create_directory", json!({"path": ".contextos/packages/other"})),
        (
            "note_create",
            json!({"path": ".contextos/packages/demo/note.md", "frontmatter": {}, "content": "x"}),
        ),
        (
            "note_update",
            json!({"path": RELEASE_NOTE, "expected_hash": hash, "patch": {"title": "Forged"}}),
        ),
        (
            "canvas_create",
            json!({"path": ".contextos/packages/map.canvas", "nodes": [], "edges": []}),
        ),
        (
            "base_create",
            json!({"path": ".contextos/packages/new.base", "definition": {"views": [{"type": "table", "name": "All"}]}}),
        ),
    ];
    for (tool, arguments) in attempts {
        let code = refused_code(&server, tool, arguments.clone()).await?;
        assert_eq!(code.as_deref(), Some("packages/server-owned"), "{tool} {arguments}");
    }
    assert_eq!(
        std::fs::read_to_string(vault.path().join(RELEASE_MANIFEST))?,
        MANIFEST_TEXT
    );
    assert!(!vault.path().join(".contextos/packages/demo/1.0.0/new.toml").exists());
    assert!(!vault.path().join(".contextos/packages/other").exists());
    assert!(!vault.path().join("elsewhere.toml").exists());
    assert!(vault.path().join("outside.md").exists());
    Ok(())
}

#[tokio::test]
async fn moving_or_deleting_a_parent_of_the_package_store_is_refused_and_changes_nothing() -> Result<(), BoxError> {
    let (vault, server) = vault_with_a_package()?;
    std::fs::create_dir_all(vault.path().join("prepared/packages/demo"))?;
    std::fs::write(vault.path().join("prepared/packages/demo/forged.toml"), "x")?;
    let attempts: Vec<(&'static str, Value)> = vec![
        ("fs_move_file", json!({"source": ".contextos", "destination": "moved"})),
        (
            "fs_move_file",
            json!({"source": "prepared", "destination": ".contextos/packages"}),
        ),
        (
            "fs_move_file",
            json!({"source": "prepared/packages", "destination": ".contextos/packages"}),
        ),
        ("fs_delete_file", json!({"path": ".contextos"})),
        ("fs_delete_file", json!({"path": ".contextos", "hard": true})),
        ("fs_delete_file", json!({"path": ".contextos/packages", "hard": true})),
    ];
    for (tool, arguments) in attempts {
        let code = refused_code(&server, tool, arguments.clone()).await?;
        assert_eq!(code.as_deref(), Some("packages/server-owned"), "{tool} {arguments}");
    }
    assert_eq!(
        std::fs::read_to_string(vault.path().join(RELEASE_MANIFEST))?,
        MANIFEST_TEXT
    );
    assert!(!vault.path().join("moved").exists());
    assert!(vault.path().join("prepared/packages/demo/forged.toml").exists());
    Ok(())
}

#[tokio::test]
async fn the_rest_of_the_contextos_folder_and_the_registry_stay_writable() -> Result<(), BoxError> {
    let (vault, server) = vault_with_a_package()?;
    let jobs_hash = current_hash(&server, ".contextos/jobs.toml").await?;
    for (tool, arguments) in [
        (
            "fs_write_file",
            json!({"path": ".contextos/jobs.toml", "content": "# jobs v2\n", "expected_hash": jobs_hash}),
        ),
        (
            "fs_write_file",
            json!({"path": ".contextos/packages-archive/x.toml", "content": "x"}),
        ),
        (
            "fs_write_file",
            json!({"path": "registry/jobs/new.md", "content": "# New\n"}),
        ),
        (
            "fs_move_file",
            json!({"source": "registry/jobs/note.md", "destination": "registry/jobs/renamed.md"}),
        ),
    ] {
        let result = call(&server, tool, arguments.clone()).await?;
        assert_eq!(result.is_error, Some(false), "{tool} {arguments}: {result:?}");
    }
    assert_eq!(
        std::fs::read_to_string(vault.path().join(".contextos/jobs.toml"))?,
        "# jobs v2\n"
    );
    assert!(vault.path().join("registry/jobs/renamed.md").exists());
    assert!(vault.path().join(".contextos/packages-archive/x.toml").exists());
    Ok(())
}

#[tokio::test]
async fn git_restore_leaves_the_package_store_alone_even_when_the_configuration_does_not_exclude_it()
-> Result<(), BoxError> {
    let harness = harness_with(
        |config| {
            config.vaults[0].git.restore_exclude = vec!["journal".into()];
            config.vaults[0].git.destructive_delete = true;
        },
        |root| {
            std::fs::create_dir_all(root.join(".contextos/packages/demo/1.0.0"))?;
            std::fs::write(root.join(RELEASE_MANIFEST), MANIFEST_TEXT)?;
            Ok(())
        },
    )
    .await?;
    let log = harness.structured("git_log", json!({"limit": 1})).await?;
    let baseline = log
        .get("entries")
        .or_else(|| log.get("commits"))
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
        .and_then(|entry| entry.get("id").or_else(|| entry.get("commit_id")))
        .and_then(Value::as_str)
        .ok_or("no baseline commit")?
        .to_owned();
    std::fs::write(harness.vault.path().join("alpha/note.md"), "# Damaged\n")?;
    std::fs::write(
        harness.vault.path().join(RELEASE_MANIFEST),
        "changed by the installer\n",
    )?;
    std::fs::write(
        harness.vault.path().join(".contextos/packages/demo/1.0.0/later.toml"),
        "later\n",
    )?;

    let restored = harness
        .call("git_restore", json!({"path": ".", "ref": baseline}))
        .await?;

    assert_eq!(restored.is_error, Some(false), "{restored:?}");
    assert_eq!(
        std::fs::read_to_string(harness.vault.path().join("alpha/note.md"))?,
        "# Note\n"
    );
    assert_eq!(
        std::fs::read_to_string(harness.vault.path().join(RELEASE_MANIFEST))?,
        "changed by the installer\n"
    );
    assert!(
        harness
            .vault
            .path()
            .join(".contextos/packages/demo/1.0.0/later.toml")
            .exists()
    );
    Ok(())
}
