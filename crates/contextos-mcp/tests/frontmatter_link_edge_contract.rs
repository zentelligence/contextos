//! Edge-case contract tests for `frontmatter_link`: the conflict check on a
//! call that would change nothing, removing dangling links, display text, keyed bases, reciprocals in other
//! bases, and inputs the base filter grammar cannot carry.

mod support;
use contextos_mcp::ContextOsServer;
use contextos_obsidian::FrontmatterDocument;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Map<String, Value>,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    let (server_transport, client_transport) = tokio::io::duplex(8192);
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

fn structured(result: &CallToolResult) -> Result<&Map<String, Value>, Box<dyn std::error::Error + Send + Sync>> {
    result
        .structured_content
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| std::io::Error::other("tool result omitted structured content").into())
}

fn arguments(value: Value) -> Result<Map<String, Value>, serde_json::Error> {
    serde_json::from_value(value)
}

const SIDECAR: &str = "---\nkey_format: FILEPATH\nlast_id: 0\nlast_create: t0\nlast_update: t0\nlink_fields:\n  depends_on:\n    inverse: blocks\n  blocks:\n    inverse: depends_on\n  related:\n    inverse: related\n  parent:\n    single: true\n---\n![[tasks.base|tasks]]\n";
const BASE: &str = "filters: file.inFolder(\"tasks\")\nviews:\n- type: table\n  name: All\n";
fn note(id: &str) -> String {
    format!("---\nid: {id}\ntitle: {id} note\n---\nBody of {id}.\n")
}

fn write(vault: &tempfile::TempDir, relative: &str, content: &str) -> Result<(), std::io::Error> {
    let path = vault.path().join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

fn vault() -> Result<(tempfile::TempDir, ContextOsServer), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for (relative, content) in [
        ("tasks.base", BASE.to_owned()),
        ("tasks.md", SIDECAR.to_owned()),
        ("tasks/alpha.md", note("Alpha")),
        ("tasks/beta.md", note("Beta")),
        ("tasks/gamma.md", note("Gamma")),
    ] {
        write(&vault, relative, &content)?;
    }
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    Ok((vault, server))
}

fn frontmatter(
    vault: &tempfile::TempDir,
    relative: &str,
) -> Result<Map<String, Value>, Box<dyn std::error::Error + Send + Sync>> {
    let content = std::fs::read_to_string(vault.path().join(relative))?;
    Ok(FrontmatterDocument::try_from(content.as_str())?.frontmatter().clone())
}

async fn link(
    server: &ContextOsServer,
    path: &str,
    operations: Value,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    call_tool(
        server.clone(),
        "frontmatter_link",
        arguments(json!({"path": path, "operations": operations}))?,
    )
    .await
}

fn assert_rejected_with_code(result: &CallToolResult, code: &str) -> TestResult {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert_eq!(structured(result)?.get("code"), Some(&json!(code)), "{result:?}");
    Ok(())
}

fn field(document: &Map<String, Value>, name: &str) -> Value {
    document.get(name).cloned().unwrap_or(Value::Null)
}

async fn assert_refused_and_nothing_changed(
    vault: &tempfile::TempDir,
    server: &ContextOsServer,
    path: &str,
    operations: Value,
    code: &str,
) -> TestResult {
    let files = ["tasks/alpha.md", "tasks/beta.md", "tasks/gamma.md", "tasks.md"];
    let read = || -> Result<Vec<String>, std::io::Error> {
        files
            .iter()
            .map(|file| std::fs::read_to_string(vault.path().join(file)))
            .collect()
    };
    let before = read()?;

    let result = link(server, path, operations.clone()).await?;

    assert_rejected_with_code(&result, code).map_err(|error| format!("{operations}: {error}"))?;
    assert_eq!(before, read()?, "{operations}: a refused call changed a file");
    Ok(())
}

// ---------------------------------------------------------------------------
// Conflict check on a call that would change nothing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stale_expected_hash_is_refused_even_when_the_note_would_not_change() -> TestResult {
    let (vault, server) = vault()?;
    let add = json!([{"op": "add", "field": "depends_on", "target": "Beta"}]);
    link(&server, "tasks/alpha.md", add.clone()).await?;
    // The other side is lost, so a repeat call would repair it.
    write(&vault, "tasks/beta.md", &note("Beta"))?;

    let result = call_tool(
        server.clone(),
        "frontmatter_link",
        arguments(json!({"path": "tasks/alpha.md", "expected_hash": "0".repeat(64), "operations": add}))?,
    )
    .await?;

    assert_rejected_with_code(&result, "io/conflict")?;
    assert!(
        !frontmatter(&vault, "tasks/beta.md")?.contains_key("blocks"),
        "the reciprocal must not be repaired by a call that was refused"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Removing a link whose target has gone
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_link_to_a_note_that_no_longer_exists_can_be_removed_by_its_path() -> TestResult {
    let (vault, server) = vault()?;
    write(
        &vault,
        "tasks/alpha.md",
        "---\nid: Alpha\ndepends_on:\n- '[[tasks/gone|Gone]]'\n- '[[beta|Beta]]'\n---\nBody of Alpha.\n",
    )?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "remove", "field": "depends_on", "target": "tasks/gone.md"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let change = &structured(&result)?["changes"][0];
    assert_eq!(change["outcome"], json!("removed"));
    assert_eq!(
        change["link"],
        json!("[[tasks/gone|Gone]]"),
        "a remove reports the entry it removed"
    );
    assert_eq!(
        structured(&result)?["reciprocal"],
        json!([]),
        "there is no note to write a reciprocal on"
    );
    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "depends_on"),
        json!(["[[beta|Beta]]"])
    );
    Ok(())
}

#[tokio::test]
async fn a_dangling_link_in_the_scope_form_is_removed_and_a_bare_name_outside_it_only_when_no_other_note_has_it()
-> TestResult {
    let (vault, server) = vault()?;

    // Inside the base a bare name is the note's path in the base.
    write(
        &vault,
        "tasks/alpha.md",
        "---\nid: Alpha\nrelated:\n- '[[gone]]'\n- '[[beta]]'\n---\n",
    )?;
    let in_scope = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "remove", "field": "related", "target": "tasks/gone.md"}]),
    )
    .await?;
    assert_eq!(structured(&in_scope)?["changes"][0]["outcome"], json!("removed"));
    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "related"),
        json!(["[[beta]]"])
    );

    // Outside the base a bare name is only a name: it is this dangling note's while no other note has it.
    write(
        &vault,
        "tasks/alpha.md",
        "---\nid: Alpha\nrelated:\n- '[[gone]]'\n---\n",
    )?;
    let unclaimed = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "remove", "field": "related", "target": "misc/gone.md"}]),
    )
    .await?;
    assert_eq!(structured(&unclaimed)?["changes"][0]["outcome"], json!("removed"));

    write(&vault, "elsewhere/gone.md", "---\nid: Elsewhere\n---\n")?;
    write(
        &vault,
        "tasks/alpha.md",
        "---\nid: Alpha\nrelated:\n- '[[gone]]'\n---\n",
    )?;
    let claimed = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "remove", "field": "related", "target": "misc/gone.md"}]),
    )
    .await?;
    assert_eq!(structured(&claimed)?["changes"][0]["outcome"], json!("absent"));
    Ok(())
}

#[tokio::test]
async fn a_link_to_a_note_whose_title_cannot_be_written_in_a_wikilink_can_still_be_removed() -> TestResult {
    let (vault, server) = vault()?;
    write(
        &vault,
        "tasks/weird.md",
        "---\ntitle: 'A [weird] | title'\n---\nBody.\n",
    )?;
    write(
        &vault,
        "tasks/alpha.md",
        "---\nid: Alpha\nrelated:\n- '[[tasks/weird|weird]]'\n---\n",
    )?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "remove", "field": "related", "target": "tasks/weird.md"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(field(&frontmatter(&vault, "tasks/alpha.md")?, "related"), json!([]));
    Ok(())
}

#[tokio::test]
async fn a_missing_target_is_still_refused_for_an_add_and_for_a_remove_by_id() -> TestResult {
    let (vault, server) = vault()?;

    assert_refused_and_nothing_changed(
        &vault,
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "tasks/gone.md"}]),
        "link/target-not-found",
    )
    .await?;
    assert_refused_and_nothing_changed(
        &vault,
        &server,
        "tasks/alpha.md",
        json!([{"op": "remove", "field": "depends_on", "target": "Gone"}]),
        "link/target-not-found",
    )
    .await
}

// ---------------------------------------------------------------------------
// Display text
// ---------------------------------------------------------------------------

#[tokio::test]
async fn display_text_falls_back_to_the_file_name_when_the_id_and_title_cannot_be_written() -> TestResult {
    let (vault, server) = vault()?;
    write(&vault, "tasks/nine.md", "---\ntitle: 'Fix [urgent] bug'\n---\nBody.\n")?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "tasks/nine.md"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "depends_on"),
        json!(["[[nine|nine]]"])
    );
    Ok(())
}

#[tokio::test]
async fn display_text_prefers_the_id_then_the_title() -> TestResult {
    let (vault, server) = vault()?;
    write(&vault, "tasks/titled.md", "---\ntitle: A Titled Note\n---\nBody.\n")?;

    link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "tasks/titled.md"}]),
    )
    .await?;

    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "depends_on"),
        json!(["[[titled|A Titled Note]]"])
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Other fields, keyed bases, and reciprocals in other bases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn editing_one_field_leaves_every_other_link_field_as_written() -> TestResult {
    let (vault, server) = vault()?;
    write(
        &vault,
        "tasks/alpha.md",
        "---\nid: Alpha\nrelated:\n- '[[tasks/gamma|the gamma one]]'\nparent: '[[tasks/gamma|G]]'\n---\nBody of Alpha.\n",
    )?;

    link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
    )
    .await?;

    let alpha = frontmatter(&vault, "tasks/alpha.md")?;
    assert_eq!(field(&alpha, "related"), json!(["[[tasks/gamma|the gamma one]]"]));
    assert_eq!(field(&alpha, "parent"), json!("[[tasks/gamma|G]]"));
    Ok(())
}

#[tokio::test]
async fn a_keyed_base_links_records_by_id_in_the_short_form() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::create_dir_all(vault.path().join("keyed"))?;
    let created = call_tool(
        server.clone(),
        "base_create",
        arguments(json!({
            "path": "keyed.base",
            "definition": {"filters": "file.inFolder(\"keyed\")", "views": [{"type": "table", "name": "All"}]},
            "key_prefix": "REC"
        }))?,
    )
    .await?;
    assert_eq!(created.is_error, Some(false), "{created:?}");
    let sidecar = std::fs::read_to_string(vault.path().join("keyed.md"))?;
    write(
        &vault,
        "keyed.md",
        &sidecar.replacen(
            "---\n",
            "---\nlink_fields:\n  depends_on:\n    inverse: blocks\n  blocks:\n    inverse: depends_on\n",
            1,
        ),
    )?;
    for _ in 0..2 {
        let record = call_tool(
            server.clone(),
            "base_record_create",
            arguments(json!({"path": "keyed.base", "frontmatter": {}, "content": "Body.\n"}))?,
        )
        .await?;
        assert_eq!(record.is_error, Some(false), "{record:?}");
    }

    let result = link(
        &server,
        "keyed/rec-001.md",
        json!([{"op": "add", "field": "depends_on", "target": "REC-002"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        field(&frontmatter(&vault, "keyed/rec-001.md")?, "depends_on"),
        json!(["[[rec-002|REC-002]]"])
    );
    assert_eq!(
        field(&frontmatter(&vault, "keyed/rec-002.md")?, "blocks"),
        json!(["[[rec-001|REC-001]]"])
    );
    Ok(())
}

#[tokio::test]
async fn a_nested_note_in_a_filepath_base_is_linked_by_its_path_within_the_base() -> TestResult {
    let (vault, server) = vault()?;
    write(&vault, "tasks/deep/inner/one.md", &note("One"))?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "One"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "depends_on"),
        json!(["[[deep/inner/one|One]]"]),
        "the key of a note in a file-path base is its path relative to the base's folder"
    );
    assert_eq!(
        field(&frontmatter(&vault, "tasks/deep/inner/one.md")?, "blocks"),
        json!(["[[alpha|Alpha]]"])
    );
    Ok(())
}

#[tokio::test]
async fn an_existing_link_in_the_scope_form_or_the_bare_key_is_recognised_as_the_same_target() -> TestResult {
    let (vault, server) = vault()?;
    write(&vault, "tasks/deep/inner/one.md", &note("One"))?;
    for existing in [
        "[[deep/inner/one|earlier]]",
        "[[tasks/deep/inner/one|earlier]]",
        "[[one|earlier]]",
    ] {
        write(
            &vault,
            "tasks/alpha.md",
            &format!("---\nid: Alpha\ndepends_on:\n- '{existing}'\n---\nBody of Alpha.\n"),
        )?;

        let result = link(
            &server,
            "tasks/alpha.md",
            json!([{"op": "add", "field": "depends_on", "target": "One"}]),
        )
        .await?;

        assert_eq!(
            structured(&result)?["changes"][0]["outcome"],
            json!("already-present"),
            "{existing}"
        );
        assert_eq!(
            field(&frontmatter(&vault, "tasks/alpha.md")?, "depends_on"),
            json!([existing]),
            "no duplicate is written"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_keyed_base_links_a_nested_record_by_its_key_and_one_with_a_custom_file_name_by_its_path() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::create_dir_all(vault.path().join("keyed"))?;
    let created = call_tool(
        server.clone(),
        "base_create",
        arguments(json!({
            "path": "keyed.base",
            "definition": {"filters": "file.inFolder(\"keyed\")", "views": [{"type": "table", "name": "All"}]},
            "key_prefix": "REC"
        }))?,
    )
    .await?;
    assert_eq!(created.is_error, Some(false), "{created:?}");
    let sidecar = std::fs::read_to_string(vault.path().join("keyed.md"))?;
    write(
        &vault,
        "keyed.md",
        &sidecar.replacen("---\n", "---\nlink_fields:\n  related:\n    inverse: related\n", 1),
    )?;
    for request in [
        json!({"path": "keyed.base", "frontmatter": {}, "content": "Body.\n"}),
        json!({"path": "keyed.base", "folder": "sub", "frontmatter": {}, "content": "Body.\n"}),
        json!({"path": "keyed.base", "folder": "sub", "filename": "custom.md", "frontmatter": {}, "content": "Body.\n"}),
    ] {
        let record = call_tool(server.clone(), "base_record_create", arguments(request)?).await?;
        assert_eq!(record.is_error, Some(false), "{record:?}");
    }

    let nested = link(
        &server,
        "keyed/rec-001.md",
        json!([{"op": "add", "field": "related", "target": "REC-002"}]),
    )
    .await?;
    assert_eq!(nested.is_error, Some(false), "{nested:?}");
    let custom = link(
        &server,
        "keyed/rec-001.md",
        json!([{"op": "add", "field": "related", "target": "REC-003"}]),
    )
    .await?;
    assert_eq!(custom.is_error, Some(false), "{custom:?}");

    assert_eq!(
        field(&frontmatter(&vault, "keyed/rec-001.md")?, "related"),
        json!(["[[rec-002|REC-002]]", "[[sub/custom|REC-003]]"]),
        "a nested record is linked by its key, but a record whose file name is not its key by its path in the base"
    );
    Ok(())
}

fn other_base(vault: &tempfile::TempDir, sidecar_links: &str) -> Result<(), std::io::Error> {
    write(
        vault,
        "other.base",
        "filters: file.inFolder(\"other\")\nviews:\n- type: table\n  name: All\n",
    )?;
    write(
        vault,
        "other.md",
        &format!(
            "---\nkey_format: FILEPATH\nlast_id: 0\nlast_create: t0\nlast_update: t0\nlink_fields:\n{sidecar_links}---\n"
        ),
    )
}

const OTHER_PAIR: &str = "  blocks:\n    inverse: depends_on\n  depends_on:\n    inverse: blocks\n";

#[tokio::test]
async fn a_reciprocal_is_written_into_a_second_declaring_base_that_declares_the_pair() -> TestResult {
    let (vault, server) = vault()?;
    other_base(&vault, OTHER_PAIR)?;
    write(&vault, "other/outsider.md", "---\nid: Outsider\n---\nBody.\n")?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "other/outsider.md"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "depends_on"),
        json!(["[[other/outsider|Outsider]]"])
    );
    assert_eq!(
        field(&frontmatter(&vault, "other/outsider.md")?, "blocks"),
        json!(["[[tasks/alpha|Alpha]]"]),
        "alpha is outside the other base's scope, so it is linked by path"
    );
    Ok(())
}

#[tokio::test]
async fn a_reciprocal_is_skipped_silently_when_the_other_base_does_not_declare_the_pair() -> TestResult {
    let (vault, server) = vault()?;
    other_base(&vault, "  blocks: {}\n")?;
    write(&vault, "other/outsider.md", "---\nid: Outsider\n---\nBody.\n")?;
    let outsider_before = std::fs::read_to_string(vault.path().join("other/outsider.md"))?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "other/outsider.md"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(structured(&result)?["reciprocal"][0]["outcome"], json!("skipped"));
    assert_eq!(structured(&result)?["warnings"], json!([]));
    assert_eq!(
        std::fs::read_to_string(vault.path().join("other/outsider.md"))?,
        outsider_before
    );
    Ok(())
}

#[tokio::test]
async fn a_reciprocal_that_cannot_be_planned_because_the_other_note_is_malformed_is_a_warning() -> TestResult {
    let (vault, server) = vault()?;
    other_base(&vault, OTHER_PAIR)?;
    write(
        &vault,
        "other/outsider.md",
        "---\nid: Outsider\nblocks: 5\n---\nBody.\n",
    )?;
    let outsider_before = std::fs::read_to_string(vault.path().join("other/outsider.md"))?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "other/outsider.md"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "the note's own write stands: {result:?}");
    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "depends_on"),
        json!(["[[other/outsider|Outsider]]"])
    );
    assert_eq!(structured(&result)?["reciprocal"][0]["outcome"], json!("skipped"));
    let warnings = structured(&result)?["warnings"]
        .as_array()
        .ok_or("no warnings")?
        .clone();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].as_str().is_some_and(|text| text.contains("blocks")),
        "{warnings:?}"
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join("other/outsider.md"))?,
        outsider_before
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Inputs the base filter grammar or the note model cannot carry
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_id_the_filter_grammar_cannot_carry_is_refused_with_a_way_forward() -> TestResult {
    let (vault, server) = vault()?;

    for hostile in ["A\"B", "A'B", "A\\B", "a\nb", "\" || true || \""] {
        let before = std::fs::read_to_string(vault.path().join("tasks/alpha.md"))?;

        let result = link(
            &server,
            "tasks/alpha.md",
            json!([{"op": "add", "field": "depends_on", "target": hostile}]),
        )
        .await?;

        assert_rejected_with_code(&result, "io/invalid-argument").map_err(|error| format!("{hostile:?}: {error}"))?;
        let message = structured(&result)?["message"].as_str().ok_or("no message")?.to_owned();
        assert!(message.contains("vault path"), "{message}");
        assert_eq!(std::fs::read_to_string(vault.path().join("tasks/alpha.md"))?, before);
    }
    Ok(())
}

#[tokio::test]
async fn a_file_name_containing_glob_characters_is_judged_unique_correctly() -> TestResult {
    let (vault, server) = vault()?;
    write(&vault, "tasks/odd[1].md", &note("Odd"))?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "tasks/odd[1].md"}]),
    )
    .await;

    // `[` and `]` cannot be written in a wikilink, so this is refused for that reason,
    // not because a search pattern was malformed.
    let result = result?;
    assert_rejected_with_code(&result, "link/unlinkable-text")
}

#[tokio::test]
async fn only_a_markdown_note_can_be_edited() -> TestResult {
    let (vault, server) = vault()?;
    let base_before = std::fs::read_to_string(vault.path().join("tasks.base"))?;

    let result = link(
        &server,
        "tasks.base",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
    )
    .await?;

    assert_rejected_with_code(&result, "io/invalid-argument")?;
    assert_eq!(std::fs::read_to_string(vault.path().join("tasks.base"))?, base_before);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_target_reached_through_a_symlink_into_another_root_is_refused() -> TestResult {
    let (vault, _server) = vault()?;
    let other_root = tempfile::Builder::new().prefix("second-root").tempdir()?;
    std::fs::create_dir_all(other_root.path().join("shared"))?;
    std::fs::write(other_root.path().join("shared/x.md"), note("Shared"))?;
    std::os::unix::fs::symlink(other_root.path(), vault.path().join("tasks/into-other-root"))?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![
        vault.path().to_path_buf(),
        other_root.path().to_path_buf(),
    ])?)?;
    let other_before = std::fs::read_to_string(other_root.path().join("shared/x.md"))?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "tasks/into-other-root/shared/x.md"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert_eq!(
        std::fs::read_to_string(other_root.path().join("shared/x.md"))?,
        other_before,
        "nothing is written outside the note's own root"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The declaration is left alone by every other tool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn base_record_create_and_base_apply_leave_the_link_declaration_byte_identical() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(&vault, "tasks.base", BASE)?;
    let declaration = "link_fields:\n  related:\n    inverse: related\n  parent:\n    single: true\n";
    write(
        &vault,
        "tasks.md",
        &format!(
            "---\nkey_prefix: REC\nkey_format: '{{key_prefix}}-{{key_id}}'\nlast_id: 3\nlast_create: t0\nlast_update: t0\n{declaration}---\n![[tasks.base|tasks]]\n"
        ),
    )?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let created = call_tool(
        server.clone(),
        "base_record_create",
        arguments(json!({"path": "tasks.base", "frontmatter": {}, "content": "Body.\n"}))?,
    )
    .await?;
    assert_eq!(created.is_error, Some(false), "{created:?}");
    let applied = call_tool(
        server.clone(),
        "base_apply",
        arguments(json!({
            "path": "tasks.base",
            "operations": [{"op": "add_view", "name": "Other", "view": {"type": "table"}}]
        }))?,
    )
    .await?;
    assert_eq!(applied.is_error, Some(false), "{applied:?}");

    let sidecar = std::fs::read_to_string(vault.path().join("tasks.md"))?;
    let document = FrontmatterDocument::try_from(sidecar.as_str())?;
    assert_eq!(
        document.frontmatter().get("link_fields"),
        Some(&json!({"related": {"inverse": "related"}, "parent": {"single": true}})),
        "{sidecar}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The advertised limit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_empty_or_over_long_operation_list_is_rejected_with_the_bad_argument_code() -> TestResult {
    let (_vault, server) = vault()?;
    let one = json!({"op": "add", "field": "related", "target": "Beta"});

    for operations in [json!([]), Value::Array(vec![one; 101])] {
        // The schema may refuse it before the tool runs; either way it is refused.
        if let Ok(result) = link(&server, "tasks/alpha.md", operations.clone()).await {
            assert_rejected_with_code(&result, "io/invalid-argument")?;
        }
    }
    Ok(())
}

#[test]
fn the_advertised_schema_bounds_the_number_of_operations() -> TestResult {
    let tools = ContextOsServer::catalogue().list_all();
    let tool = tools
        .iter()
        .find(|candidate| candidate.name == "frontmatter_link")
        .ok_or("frontmatter_link is not advertised")?;
    let operations = tool
        .input_schema
        .get("properties")
        .and_then(|properties| properties.get("operations"))
        .ok_or("no operations property")?;

    assert_eq!(operations.get("minItems"), Some(&json!(1)), "{operations}");
    assert_eq!(operations.get("maxItems"), Some(&json!(100)), "{operations}");
    Ok(())
}
