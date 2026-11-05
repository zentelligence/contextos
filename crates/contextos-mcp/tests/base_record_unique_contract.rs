//! `base_record_create`'s `unique_fields`: a record whose values for the named fields match a record already in the
//! base's scope is refused, naming that record, so a raiser that runs again finds the task it already raised instead
//! of raising a second. Exercised through the MCP adapter, including two server instances racing.
mod support;

use contextos_mcp::ContextOsServer;
use rmcp::model::CallToolResult;
use serde_json::{Map, Value, json};
use support::git_harness::call;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const SIDECAR: &str = "---\nkey_prefix: A\nkey_format: '{key_prefix}-{key_id}'\nlast_create: 2026-10-08T00:00:00Z\nlast_update: 2026-10-08T00:00:00Z\nkey_prefixes:\n  A: 0\n---\n";

fn structured(result: &CallToolResult) -> Result<&Map<String, Value>, BoxError> {
    result
        .structured_content
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| "tool result omitted structured content".into())
}

fn task_vault() -> Result<(tempfile::TempDir, ContextOsServer), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("memory/tasks"))?;
    std::fs::write(
        vault.path().join("memory/tasks.base"),
        "filters: file.inFolder(\"memory/tasks\") && file.basename != \"index\"\nviews:\n  - type: table\n    name: All\n    order:\n      - id\n",
    )?;
    std::fs::write(vault.path().join("memory/tasks.md"), SIDECAR)?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    Ok((vault, server))
}

fn raise(origin: &str, entity: &str, unique_fields: &Value) -> Value {
    json!({
        "path": "memory/tasks.base",
        "frontmatter": {"title": "Classify inbox item", "state": "next", "origin": origin, "entity": entity},
        "content": "# Classify inbox item\n",
        "unique_fields": unique_fields,
    })
}

/// The record notes in the tasks folder, leaving out the `index.md` the server maintains there.
fn task_files(vault: &std::path::Path) -> Result<usize, BoxError> {
    let mut count = 0;
    for entry in std::fs::read_dir(vault.join("memory/tasks"))? {
        if entry?.file_name() != "index.md" {
            count += 1;
        }
    }
    Ok(count)
}

#[tokio::test]
async fn a_second_record_with_the_same_unique_value_is_refused_naming_the_first() -> Result<(), BoxError> {
    let (vault, server) = task_vault()?;
    let fields = json!(["origin"]);
    let first = call(
        &server,
        "base_record_create",
        raise("sweep:classify:abc", "demo", &fields),
    )
    .await?;
    assert_ne!(first.is_error, Some(true), "{first:?}");
    let sidecar_before = std::fs::read_to_string(vault.path().join("memory/tasks.md"))?;

    let second = call(
        &server,
        "base_record_create",
        raise("sweep:classify:abc", "demo", &fields),
    )
    .await?;
    assert_eq!(second.is_error, Some(true), "{second:?}");
    let body = structured(&second)?;
    assert_eq!(body.get("code"), Some(&json!("io/duplicate-record")));
    assert_eq!(body.get("path"), Some(&json!("memory/tasks/a-001.md")), "{body:?}");
    let message = body.get("message").and_then(Value::as_str).ok_or("message missing")?;
    assert!(message.contains("A-001"), "{message}");
    assert!(message.contains("origin"), "{message}");
    assert_eq!(
        task_files(vault.path())?,
        1,
        "nothing may be written for a refused record"
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join("memory/tasks.md"))?,
        sidecar_before,
        "no id may be reserved for a refused record"
    );
    Ok(())
}

#[tokio::test]
async fn a_different_unique_value_is_created() -> Result<(), BoxError> {
    let (vault, server) = task_vault()?;
    let fields = json!(["origin"]);
    call(
        &server,
        "base_record_create",
        raise("sweep:classify:abc", "demo", &fields),
    )
    .await?;
    let other = call(
        &server,
        "base_record_create",
        raise("sweep:classify:def", "demo", &fields),
    )
    .await?;
    assert_ne!(other.is_error, Some(true), "{other:?}");
    assert_eq!(task_files(vault.path())?, 2);
    Ok(())
}

#[tokio::test]
async fn several_unique_fields_form_one_key_that_must_match_in_full() -> Result<(), BoxError> {
    let (vault, server) = task_vault()?;
    let fields = json!(["origin", "entity"]);
    call(
        &server,
        "base_record_create",
        raise("sweep:classify:abc", "demo", &fields),
    )
    .await?;
    let other_entity = call(
        &server,
        "base_record_create",
        raise("sweep:classify:abc", "other", &fields),
    )
    .await?;
    assert_ne!(other_entity.is_error, Some(true), "{other_entity:?}");
    let repeat = call(
        &server,
        "base_record_create",
        raise("sweep:classify:abc", "other", &fields),
    )
    .await?;
    assert_eq!(structured(&repeat)?.get("code"), Some(&json!("io/duplicate-record")));
    assert_eq!(task_files(vault.path())?, 2);
    Ok(())
}

#[tokio::test]
async fn a_matching_note_outside_the_bases_scope_does_not_block_creation() -> Result<(), BoxError> {
    let (vault, server) = task_vault()?;
    std::fs::create_dir_all(vault.path().join("notes"))?;
    std::fs::write(
        vault.path().join("notes/elsewhere.md"),
        "---\norigin: sweep:classify:abc\nentity: demo\n---\n# Elsewhere\n",
    )?;
    let created = call(
        &server,
        "base_record_create",
        raise("sweep:classify:abc", "demo", &json!(["origin"])),
    )
    .await?;
    assert_ne!(created.is_error, Some(true), "{created:?}");
    Ok(())
}

#[tokio::test]
async fn unique_fields_that_are_empty_repeated_malformed_absent_or_not_scalar_are_refused() -> Result<(), BoxError> {
    let (vault, server) = task_vault()?;
    let mut list_valued = raise("sweep:classify:abc", "demo", &json!(["origin"]));
    list_valued["frontmatter"]["origin"] = json!(["a", "b"]);
    for arguments in [
        raise("sweep:classify:abc", "demo", &json!([])),
        raise("sweep:classify:abc", "demo", &json!(["origin", "origin"])),
        raise("sweep:classify:abc", "demo", &json!(["not a name"])),
        raise("sweep:classify:abc", "demo", &json!(["missing_field"])),
        raise(
            "sweep:classify:abc",
            "demo",
            &json!(["a", "b", "c", "d", "e", "f", "g", "h", "i"]),
        ),
        list_valued,
    ] {
        let result = call(&server, "base_record_create", arguments.clone()).await?;
        assert_eq!(result.is_error, Some(true), "{arguments}");
        assert_eq!(
            structured(&result)?.get("code"),
            Some(&json!("io/invalid-argument")),
            "{arguments}"
        );
    }
    assert_eq!(task_files(vault.path())?, 0);
    Ok(())
}

#[tokio::test]
async fn two_server_instances_raising_the_same_record_at_once_create_exactly_one() -> Result<(), BoxError> {
    let (vault, first) = task_vault()?;
    let second = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    let fields = json!(["origin"]);
    let (left, right) = tokio::join!(
        call(
            &first,
            "base_record_create",
            raise("sweep:classify:abc", "demo", &fields)
        ),
        call(
            &second,
            "base_record_create",
            raise("sweep:classify:abc", "demo", &fields)
        ),
    );
    let outcomes = [left?, right?];
    assert_eq!(
        outcomes.iter().filter(|result| result.is_error != Some(true)).count(),
        1,
        "{outcomes:?}"
    );
    assert_eq!(task_files(vault.path())?, 1);
    Ok(())
}
