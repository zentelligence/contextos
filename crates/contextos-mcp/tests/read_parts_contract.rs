//! Through a real MCP transport: `fs_read_text_file` reads a byte part inline, with `size`, `offset`, `length`,
//! `next_offset` (-1 at the end), `truncated`, and `version`, and no `content_hash`, `line_count`, or resource link;
//! every read adds `size` and `version` and keeps the fields it had; `expected_version` refuses a changed file with
//! its current version in the error's structured details; and malformed or combined selections are refused.

mod resource_support;

use std::fmt::Write as _;

use contextos_mcp::{Config, ContextOsServer};
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use resource_support::{BoxError, connect};

#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "Debug quoting/escaping is required to embed the path as a valid TOML string"
)]
fn threshold_config(vault: &std::path::Path, threshold_kb: u64) -> Result<Config, BoxError> {
    let source = format!(
        "[[vault]]\npath = {vault:?}\nname = \"vault\"\nstate_directory = \".contextos\"\n\
         [server]\nresource_link_threshold_kb = {threshold_kb}\n"
    );
    Ok(Config::try_from(source.as_str())?)
}

struct Session {
    _vault: tempfile::TempDir,
    root: std::path::PathBuf,
    client: rmcp::service::RunningService<rmcp::RoleClient, ()>,
    handle: tokio::task::JoinHandle<Result<(), BoxError>>,
}

impl Session {
    async fn open(threshold_kb: u64) -> Result<Self, BoxError> {
        let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
        let root = vault.path().to_path_buf();
        let server = ContextOsServer::try_from(threshold_config(&root, threshold_kb)?)?;
        let (client, handle) = connect(server).await?;
        Ok(Self {
            _vault: vault,
            root,
            client,
            handle,
        })
    }

    fn write(&self, name: &str, bytes: &[u8]) -> Result<(), BoxError> {
        std::fs::write(self.root.join(name), bytes)?;
        Ok(())
    }

    async fn call(&self, tool: &'static str, arguments: Value) -> Result<CallToolResult, BoxError> {
        let arguments: Map<String, Value> = arguments
            .as_object()
            .cloned()
            .ok_or_else(|| std::io::Error::other("arguments must be a JSON object"))?;
        Ok(self
            .client
            .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
            .await?)
    }

    /// Calls `fs_read_text_file` and returns its structured result, failing the test on a tool error.
    async fn read(&self, arguments: Value) -> Result<Value, BoxError> {
        let result = self.call("fs_read_text_file", arguments).await?;
        if result.is_error != Some(false) {
            return Err(std::io::Error::other(format!("the read was refused: {result:?}")).into());
        }
        Ok(structured(&result)?.clone())
    }

    /// Calls `fs_read_text_file` and returns its structured error, failing the test if the read succeeded.
    async fn refusal(&self, arguments: Value) -> Result<Value, BoxError> {
        let result = self.call("fs_read_text_file", arguments).await?;
        if result.is_error != Some(true) {
            return Err(std::io::Error::other(format!("the read was not refused: {result:?}")).into());
        }
        Ok(structured(&result)?.clone())
    }

    async fn close(self) -> Result<(), BoxError> {
        self.client.cancel().await?;
        self.handle.await??;
        Ok(())
    }
}

fn structured(result: &CallToolResult) -> Result<&Value, BoxError> {
    result
        .structured_content
        .as_ref()
        .ok_or_else(|| std::io::Error::other(format!("the result has no structured content: {result:?}")).into())
}

fn field_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn field_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn hash_of(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

fn is_version(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|token| token.len() == 32 && token.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
}

fn has_resource_link(result: &CallToolResult) -> bool {
    result.content.iter().any(|block| block.as_resource_link().is_some())
}

#[tokio::test]
async fn a_part_reports_content_size_offset_length_next_offset_truncated_and_version() -> Result<(), BoxError> {
    let session = Session::open(5).await?;
    session.write("note.md", b"alpha\nbeta\ngamma")?;

    let part = session
        .read(json!({ "path": "note.md", "offset": 6, "length": 4 }))
        .await?;

    assert_eq!(text(&part, "content"), "beta");
    assert_eq!(field_u64(&part, "size"), Some(16));
    assert_eq!(
        (field_u64(&part, "offset"), field_u64(&part, "length")),
        (Some(6), Some(4))
    );
    assert_eq!(field_i64(&part, "next_offset"), Some(10));
    assert_eq!(part.get("truncated"), Some(&json!(true)));
    assert!(is_version(part.get("version")), "{part}");
    session.close().await
}

#[tokio::test]
async fn a_part_has_no_content_hash_line_count_or_resource_link() -> Result<(), BoxError> {
    let session = Session::open(1).await?;
    session.write("note.md", "line\n".repeat(1000).as_bytes())?;

    let result = session
        .call("fs_read_text_file", json!({ "path": "note.md", "offset": 0 }))
        .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert!(
        !has_resource_link(&result),
        "a part is continued by offset, never by a resource link"
    );
    let part = structured(&result)?;
    for absent in ["content_hash", "line_count"] {
        assert_eq!(part.get(absent), None, "a part carries no {absent}: {part}");
    }
    assert_eq!(field_u64(part, "length"), Some(1024));
    session.close().await
}

#[tokio::test]
async fn a_file_is_read_whole_in_parts_of_at_most_the_threshold_until_next_offset_is_minus_one() -> Result<(), BoxError>
{
    let session = Session::open(1).await?;
    let content = "€ transcript with no line breaks 🙂 ".repeat(200);
    session.write("transcript.md", content.as_bytes())?;
    let version = session
        .read(json!({ "path": "transcript.md" }))
        .await?
        .get("version")
        .cloned()
        .ok_or("a whole read carries no version")?;

    let mut joined = String::new();
    let mut offset = 0_u64;
    let mut parts = 0;
    loop {
        let result = session
            .call(
                "fs_read_text_file",
                json!({ "path": "transcript.md", "offset": offset, "length": 100_000, "expected_version": version }),
            )
            .await?;
        assert_eq!(result.is_error, Some(false), "{result:?}");
        assert!(!has_resource_link(&result));
        let part = structured(&result)?;
        let piece = text(part, "content");
        assert!(piece.len() <= 1024, "a part is never larger than the inline threshold");
        assert_eq!(field_u64(part, "offset"), Some(offset));
        joined.push_str(piece);
        parts += 1;
        match field_i64(part, "next_offset") {
            Some(-1) => break,
            Some(next) => offset = u64::try_from(next)?,
            None => return Err("a part carries no next_offset".into()),
        }
    }

    assert_eq!(joined, content);
    assert!(parts > 1);
    session.close().await
}

#[tokio::test]
async fn a_part_with_only_an_offset_is_as_long_as_the_threshold_allows() -> Result<(), BoxError> {
    let session = Session::open(1).await?;
    session.write("note.md", "a".repeat(3000).as_bytes())?;

    let part = session.read(json!({ "path": "note.md", "offset": 1000 })).await?;

    assert_eq!(field_u64(&part, "length"), Some(1024));
    assert_eq!(field_i64(&part, "next_offset"), Some(2024));
    session.close().await
}

#[tokio::test]
async fn a_part_with_only_a_length_starts_at_the_beginning() -> Result<(), BoxError> {
    let session = Session::open(5).await?;
    session.write("note.md", b"abcdefgh")?;

    let part = session.read(json!({ "path": "note.md", "length": 4 })).await?;

    assert_eq!(text(&part, "content"), "abcd");
    assert_eq!(field_u64(&part, "offset"), Some(0));
    assert_eq!(field_i64(&part, "next_offset"), Some(4));
    session.close().await
}

#[tokio::test]
async fn a_whole_read_keeps_its_fields_and_adds_size_and_version() -> Result<(), BoxError> {
    let session = Session::open(5).await?;
    session.write("note.md", b"one\ntwo\n")?;

    let whole = session.read(json!({ "path": "note.md" })).await?;
    let head = session.read(json!({ "path": "note.md", "head": 1 })).await?;

    assert_eq!(text(&whole, "content"), "one\ntwo\n");
    assert_eq!(field_u64(&whole, "line_count"), Some(2));
    assert_eq!(text(&whole, "content_hash"), hash_of(b"one\ntwo\n"));
    assert_eq!(whole.get("truncated"), Some(&json!(false)));
    assert_eq!(field_u64(&whole, "size"), Some(8));
    assert!(is_version(whole.get("version")), "{whole}");
    for absent in ["offset", "length", "next_offset"] {
        assert_eq!(whole.get(absent), None, "only a part carries {absent}");
    }
    assert_eq!(head.get("truncated"), Some(&json!(true)));
    assert_eq!(text(&head, "content_hash"), hash_of(b"one\ntwo\n"));
    assert_eq!(head.get("version"), whole.get("version"));
    session.close().await
}

#[tokio::test]
async fn a_preview_keeps_truncated_and_its_resource_link_and_gains_no_next_offset() -> Result<(), BoxError> {
    let session = Session::open(1).await?;
    session.write("note.md", "line one\n".repeat(200).as_bytes())?;

    let result = session.call("fs_read_text_file", json!({ "path": "note.md" })).await?;

    assert!(has_resource_link(&result));
    let preview = structured(&result)?;
    assert_eq!(preview.get("truncated"), Some(&json!(true)));
    assert_eq!(field_u64(preview, "size"), Some(1800));
    assert!(is_version(preview.get("version")), "{preview}");
    assert_eq!(preview.get("next_offset"), None);
    session.close().await
}

#[tokio::test]
async fn a_length_below_four_is_refused_as_an_invalid_range() -> Result<(), BoxError> {
    let session = Session::open(5).await?;
    session.write("note.md", b"abcdefgh")?;

    for length in 0..4 {
        let error = session
            .refusal(json!({ "path": "note.md", "offset": 0, "length": length }))
            .await?;
        assert_eq!(text(&error, "code"), "io/invalid-range", "length {length}: {error}");
    }
    session.close().await
}

#[tokio::test]
async fn an_offset_beyond_the_end_is_refused_as_an_invalid_range_naming_next_offset() -> Result<(), BoxError> {
    let session = Session::open(5).await?;
    session.write("note.md", b"abc")?;

    let error = session.refusal(json!({ "path": "note.md", "offset": 4 })).await?;

    assert_eq!(text(&error, "code"), "io/invalid-range", "{error}");
    assert!(text(&error, "remediation").contains("next_offset"), "{error}");
    session.close().await
}

#[tokio::test]
async fn an_offset_inside_a_character_is_refused_as_an_invalid_range() -> Result<(), BoxError> {
    let session = Session::open(5).await?;
    session.write("note.md", "é".as_bytes())?;

    let error = session.refusal(json!({ "path": "note.md", "offset": 1 })).await?;

    assert_eq!(text(&error, "code"), "io/invalid-range", "{error}");
    session.close().await
}

#[tokio::test]
async fn offset_or_length_with_head_tail_or_range_is_refused_as_mutually_exclusive() -> Result<(), BoxError> {
    let session = Session::open(5).await?;
    session.write("note.md", b"one\ntwo\n")?;

    for (selection, value) in [
        ("head", json!(1)),
        ("tail", json!(1)),
        ("range", json!({ "from_line": 1, "to_line": 1 })),
    ] {
        for (part, part_value) in [("offset", 0), ("length", 4)] {
            let mut arguments = json!({ "path": "note.md" });
            if let Some(object) = arguments.as_object_mut() {
                object.insert(selection.to_owned(), value.clone());
                object.insert(part.to_owned(), json!(part_value));
            }
            let error = session.refusal(arguments).await?;
            assert_eq!(
                text(&error, "code"),
                "io/invalid-argument",
                "{selection} with {part}: {error}"
            );
            let message = text(&error, "message");
            assert!(
                message.contains("mutually exclusive") && message.contains(part),
                "{selection} with {part}: {message}"
            );
        }
    }
    session.close().await
}

#[tokio::test]
async fn a_stale_expected_version_is_refused_with_the_current_version_in_structured_details() -> Result<(), BoxError> {
    let session = Session::open(5).await?;
    session.write("note.md", b"first")?;
    let stale = session
        .read(json!({ "path": "note.md" }))
        .await?
        .get("version")
        .cloned()
        .ok_or("a whole read carries no version")?;
    session.write("note.md", b"the second version")?;
    let current = session
        .read(json!({ "path": "note.md" }))
        .await?
        .get("version")
        .cloned()
        .ok_or("a whole read carries no version")?;

    for selection in [json!({ "offset": 0 }), json!({}), json!({ "head": 1 })] {
        let mut arguments = json!({ "path": "note.md", "expected_version": stale });
        if let (Some(object), Some(extra)) = (arguments.as_object_mut(), selection.as_object()) {
            object.extend(extra.clone());
        }
        let error = session.refusal(arguments).await?;
        assert_eq!(text(&error, "code"), "io/conflict", "{selection}: {error}");
        assert_eq!(error.get("current_version"), Some(&current), "{selection}: {error}");
        assert_eq!(error.get("content"), None);
    }
    session.close().await
}

#[tokio::test]
async fn a_stale_expected_version_is_reported_before_an_offset_the_change_made_invalid() -> Result<(), BoxError> {
    let session = Session::open(5).await?;
    session.write("note.md", b"abcdefgh")?;
    let stale = session
        .read(json!({ "path": "note.md" }))
        .await?
        .get("version")
        .cloned()
        .ok_or("a whole read carries no version")?;
    session.write("note.md", b"ab")?;

    let error = session
        .refusal(json!({ "path": "note.md", "offset": 4, "expected_version": stale }))
        .await?;

    assert_eq!(text(&error, "code"), "io/conflict", "{error}");
    session.close().await
}

#[tokio::test]
async fn a_malformed_expected_version_is_refused_as_an_invalid_version() -> Result<(), BoxError> {
    let session = Session::open(5).await?;
    session.write("note.md", b"content")?;

    for malformed in ["not-a-version", "ABCDEF0123456789ABCDEF0123456789", ""] {
        let error = session
            .refusal(json!({ "path": "note.md", "offset": 0, "expected_version": malformed }))
            .await?;
        assert_eq!(text(&error, "code"), "io/invalid-version", "{malformed:?}: {error}");
    }
    session.close().await
}

#[tokio::test]
async fn each_batch_item_adds_size_and_version_and_keeps_truncated() -> Result<(), BoxError> {
    let session = Session::open(1).await?;
    session.write("small.md", b"small")?;
    session.write("large.md", "line one\n".repeat(200).as_bytes())?;

    let result = session
        .call(
            "fs_read_multiple_files",
            json!({ "paths": ["small.md", "large.md", "missing.md"] }),
        )
        .await?;

    let files = structured(&result)?
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let item = |path: &str| {
        files
            .iter()
            .find(|file| file.get("path") == Some(&json!(path)))
            .cloned()
            .ok_or_else(|| std::io::Error::other(format!("{path} missing from the batch")))
    };
    let small = item("small.md")?;
    let large = item("large.md")?;
    let missing = item("missing.md")?;

    assert_eq!(field_u64(&small, "size"), Some(5));
    assert!(is_version(small.get("version")), "{small}");
    assert_eq!(small.get("truncated"), Some(&json!(false)));
    assert_eq!(text(&small, "content_hash"), hash_of(b"small"));
    assert_eq!(field_u64(&large, "size"), Some(1800));
    assert!(is_version(large.get("version")), "{large}");
    assert_eq!(large.get("truncated"), Some(&json!(true)));
    assert!(missing.get("error").is_some());
    for absent in ["size", "version"] {
        assert_eq!(missing.get(absent), None, "a failed item has no {absent}");
    }
    session.close().await
}

#[tokio::test]
async fn an_attachment_reports_its_size_and_version_beside_its_content_hash() -> Result<(), BoxError> {
    let session = Session::open(5).await?;
    session.write("note.md", b"hello")?;
    session.write("image.png", &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0])?;

    let metadata_of = |result: &CallToolResult| -> Result<Value, BoxError> {
        Ok(result
            .content
            .iter()
            .find_map(ContentBlock::as_text)
            .map(|block| serde_json::from_str(&block.text))
            .transpose()?
            .unwrap_or_default())
    };
    for (path, bytes) in [
        ("note.md", b"hello".to_vec()),
        ("image.png", vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0]),
    ] {
        let result = session.call("fs_attach_file", json!({ "path": path })).await?;
        let metadata = metadata_of(&result)?;
        assert_eq!(text(&metadata, "content_hash"), hash_of(&bytes), "{path}");
        assert_eq!(
            field_u64(&metadata, "size"),
            Some(u64::try_from(bytes.len())?),
            "{path}"
        );
        assert!(is_version(metadata.get("version")), "{path}: {metadata}");
    }
    let expanded = session
        .call("fs_attach_file", json!({ "path": "note.md", "expand_links": true }))
        .await?;
    assert_eq!(
        metadata_of(&expanded)?.get("version"),
        None,
        "expanded content is a derived view and carries no version"
    );
    session.close().await
}

#[test]
fn the_read_schema_advertises_offset_length_expected_version_and_a_signed_next_offset() -> Result<(), BoxError> {
    let catalogue = ContextOsServer::catalogue();
    let tool = catalogue
        .get("fs_read_text_file")
        .ok_or("fs_read_text_file is not advertised")?;

    let inputs = tool
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or("the input schema has no properties")?;
    for name in ["offset", "length", "expected_version"] {
        assert!(inputs.contains_key(name), "the input schema has no {name}");
    }
    let outputs = tool
        .output_schema
        .as_ref()
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object)
        .ok_or("the output schema has no properties")?;
    for name in [
        "size",
        "version",
        "offset",
        "length",
        "next_offset",
        "truncated",
        "content_hash",
        "line_count",
    ] {
        assert!(outputs.contains_key(name), "the output schema has no {name}");
    }
    let next_offset = outputs.get("next_offset").ok_or("no next_offset")?;
    assert_eq!(next_offset.get("type"), Some(&json!("integer")), "{next_offset}");
    assert_ne!(next_offset.get("format"), Some(&json!("uint64")), "{next_offset}");
    assert!(
        next_offset
            .get("minimum")
            .and_then(Value::as_i64)
            .is_none_or(|minimum| minimum <= -1),
        "next_offset must admit -1: {next_offset}"
    );
    Ok(())
}

#[test]
fn the_read_tool_descriptions_say_which_returns_a_preview_and_how_to_read_the_rest() -> Result<(), BoxError> {
    let catalogue = ContextOsServer::catalogue();
    let description = |name: &str| -> Result<String, BoxError> {
        Ok(catalogue
            .get(name)
            .ok_or_else(|| std::io::Error::other(format!("{name} is not advertised")))?
            .description
            .as_deref()
            .unwrap_or_default()
            .to_owned())
    };

    let read = description("fs_read_text_file")?;
    for phrase in ["offset", "length", "expected_version", "next_offset", "truncated"] {
        assert!(
            read.contains(phrase),
            "fs_read_text_file's description omits {phrase}: {read}"
        );
    }
    let batch = description("fs_read_multiple_files")?;
    for phrase in ["truncated", "fs_attach_file"] {
        assert!(
            batch.contains(phrase),
            "fs_read_multiple_files's description omits {phrase}: {batch}"
        );
    }
    let attach = description("fs_attach_file")?;
    assert!(
        attach.contains("whole file"),
        "fs_attach_file's description omits that it returns the whole file: {attach}"
    );
    Ok(())
}

#[tokio::test]
async fn a_stale_expected_version_is_reported_before_a_length_that_is_too_short() -> Result<(), BoxError> {
    let session = Session::open(5).await?;
    session.write("note.md", b"abcdefgh")?;
    let stale = session
        .read(json!({ "path": "note.md" }))
        .await?
        .get("version")
        .cloned()
        .ok_or("a whole read carries no version")?;
    session.write("note.md", b"abcdefghij")?;

    let error = session
        .refusal(json!({ "path": "note.md", "offset": 0, "length": 2, "expected_version": stale }))
        .await?;

    assert_eq!(text(&error, "code"), "io/conflict", "{error}");
    session.close().await
}

#[tokio::test]
async fn a_part_holding_a_nul_or_invalid_utf8_is_refused_as_binary() -> Result<(), BoxError> {
    let session = Session::open(5).await?;
    session.write("zero.md", b"ab\0cdef")?;
    session.write("invalid.md", &[b'a', 0xFF, b'b', b'c', b'd'])?;

    for name in ["zero.md", "invalid.md"] {
        let error = session.refusal(json!({ "path": name, "offset": 0 })).await?;
        assert_eq!(text(&error, "code"), "io/binary", "{name}: {error}");
    }
    session.close().await
}

#[tokio::test]
async fn a_part_that_could_not_advance_is_refused_as_binary_rather_than_returned_empty() -> Result<(), BoxError> {
    let session = Session::open(5).await?;
    session.write("stalled.md", &[0xE2, 0x80, 0x80, 0x80, 0x80])?;

    let error = session
        .refusal(json!({ "path": "stalled.md", "offset": 0, "length": 4 }))
        .await?;

    assert_eq!(text(&error, "code"), "io/binary", "{error}");
    session.close().await
}

#[tokio::test]
async fn a_server_built_with_a_zero_inline_threshold_is_refused() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = threshold_config(vault.path(), 5)?;
    config.server.resource_link_threshold_kb = 0;

    let result = ContextOsServer::try_from(config);

    let message = result.err().map(|error| error.to_string()).unwrap_or_default();
    assert!(message.contains("resource_link_threshold_kb"), "{message}");
    Ok(())
}
