use rmcp::model::{CallToolResult, ContentBlock as Content, Resource, ResourceContents};
use serde_json::json;

use super::*;

fn decoded(result: CallToolResult) -> Result<Value, ToolResultError> {
    ToolResultJson::try_from(result).map(|decoded| decoded.0)
}

fn embedded_text(text: &str) -> Content {
    Content::resource(ResourceContents::text(text, "file:///vault/secret/path/note.md"))
}

#[test]
fn structured_content_is_returned_as_it_is() -> Result<(), ToolResultError> {
    let result = CallToolResult::structured(json!({ "vaults": [{ "name": "a" }] }));

    assert_eq!(decoded(result)?, json!({ "vaults": [{ "name": "a" }] }));
    Ok(())
}

#[test]
fn a_json_text_block_is_parsed_when_there_is_no_structured_content() -> Result<(), ToolResultError> {
    let result = CallToolResult::success(vec![Content::text(r#"{"count":2}"#)]);

    assert_eq!(decoded(result)?, json!({ "count": 2 }));
    Ok(())
}

#[test]
fn a_text_block_that_is_not_json_is_unreadable() {
    let result = CallToolResult::success(vec![Content::text("not json")]);

    assert!(matches!(decoded(result), Err(ToolResultError::NotJson(_))));
}

#[test]
fn an_embedded_text_resource_becomes_content_with_its_hash() -> Result<(), ToolResultError> {
    let result = CallToolResult::success(vec![
        embedded_text("# Note\n"),
        Content::text(r#"{"content_hash":"abc123"}"#),
    ]);

    assert_eq!(
        decoded(result)?,
        json!({ "content": "# Note\n", "content_hash": "abc123" })
    );
    Ok(())
}

#[test]
fn the_resource_uri_is_never_exposed_to_a_script() -> Result<(), ToolResultError> {
    let result = CallToolResult::success(vec![embedded_text("x")]);

    assert!(!decoded(result)?.to_string().contains("/vault/secret/path"));
    Ok(())
}

#[test]
fn an_expanded_note_reports_links_expanded_and_no_hash() -> Result<(), ToolResultError> {
    let result = CallToolResult::success(vec![
        embedded_text("see [n](a/b.md)"),
        Content::text(r#"{"links_expanded":true}"#),
    ]);

    assert_eq!(
        decoded(result)?,
        json!({ "content": "see [n](a/b.md)", "links_expanded": true })
    );
    Ok(())
}

#[test]
fn a_marker_block_can_never_overwrite_the_content() -> Result<(), ToolResultError> {
    let result = CallToolResult::success(vec![embedded_text("real"), Content::text(r#"{"content":"forged"}"#)]);

    assert_eq!(decoded(result)?, json!({ "content": "real" }));
    Ok(())
}

#[test]
fn a_binary_resource_is_returned_as_base64_with_its_type() -> Result<(), ToolResultError> {
    let result = CallToolResult::success(vec![Content::resource(ResourceContents::BlobResourceContents {
        uri: "file:///x.png".to_owned(),
        mime_type: Some("image/png".to_owned()),
        blob: "aGk=".to_owned(),
        meta: None,
    })]);

    assert_eq!(decoded(result)?, json!({ "blob": "aGk=", "mime_type": "image/png" }));
    Ok(())
}

#[test]
fn a_result_with_no_content_at_all_is_null() -> Result<(), ToolResultError> {
    assert_eq!(decoded(CallToolResult::success(Vec::new()))?, Value::Null);
    Ok(())
}

fn unsupported(result: CallToolResult) -> bool {
    matches!(decoded(result), Err(ToolResultError::Unsupported { .. }))
}

#[test]
fn a_block_with_no_script_form_is_rejected_rather_than_dropped() {
    let link = Content::resource_link(Resource::new("file:///x.md", "x"));

    assert!(unsupported(CallToolResult::success(vec![link])));
    assert!(unsupported(CallToolResult::success(vec![Content::image(
        "aGk=",
        "image/png"
    )])));
}

#[test]
fn a_second_embedded_resource_is_rejected_rather_than_dropped() {
    let result = CallToolResult::success(vec![embedded_text("one"), embedded_text("two")]);

    assert!(unsupported(result));
}

#[test]
fn several_text_blocks_without_a_resource_are_rejected_rather_than_truncated() {
    let result = CallToolResult::success(vec![Content::text(r#"{"a":1}"#), Content::text(r#"{"b":2}"#)]);

    assert!(unsupported(result));
}

#[test]
fn a_text_block_beside_a_resource_that_is_not_a_json_object_is_rejected() {
    assert!(unsupported(CallToolResult::success(vec![
        embedded_text("x"),
        Content::text("plain words")
    ])));
    assert!(unsupported(CallToolResult::success(vec![
        embedded_text("x"),
        Content::text("[1,2]")
    ])));
}
