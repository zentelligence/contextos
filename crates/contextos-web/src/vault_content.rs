//! Shared vault-file content access: reading a file's own complete raw
//! text, splitting it at its real frontmatter boundary into an editable
//! body plus a byte-for-byte-preserved header, the concurrency hash a save
//! presents back as `expected_hash`, and the save itself. Every domain
//! module that reads and edits a real vault file (Wiki, Insights, Profile,
//! entity-folders) shares this one implementation instead of its own copy.
//!
//! Before this module existed, four of those callers each carried their
//! own near-identical `body_start_line`/`file_hash`/`body_from_lines`/
//! `header_from_lines` quartet, and their own copy of the full-text read
//! this module's own [`read_full_text`] now owns. Every one of those
//! per-module read copies had also, independently, called `fs_attach_file`
//! through a `struct AttachedTextContent { text: String }` shape and
//! `CallToolResult::into_typed`, which cannot see `fs_attach_file`'s real
//! response at all: that tool embeds its content as an MCP resource
//! content block, never a plain JSON text block, so every one of those
//! calls silently produced `Err`, and every caller silently kept serving
//! whatever `fs_read_text_file`'s own 5 KiB cap had truncated it to. One
//! shared, correctly decoded implementation removes both the duplication
//! and the bug class it let through unnoticed.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClient};
use crate::tool_result::ToolResultJson;

/// `source` after its leading frontmatter block, for a reader that wants a note's text without its properties (a
/// title from its first heading, say). A block that is never closed is not frontmatter, so the whole of `source` comes
/// back. A `---\r\n` opening line, from a note saved with Windows line endings, counts.
#[must_use]
pub fn body_of(source: &str) -> &str {
    let Some(rest) = source.strip_prefix("---\n").or_else(|| source.strip_prefix("---\r\n")) else {
        return source;
    };
    let mut offset = 0_usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return rest.get(offset.saturating_add(line.len())..).unwrap_or_default();
        }
        offset = offset.saturating_add(line.len());
    }
    source
}

/// A vault text file's complete content, and the hash of exactly the bytes that content came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachedText {
    pub text: String,
    /// The hash a later write presents as `expected_hash`. `None` only if the server did not report one.
    pub content_hash: Option<String>,
}

/// The stable code in a tool's failure result.
#[derive(Debug, Deserialize)]
struct RefusalCode {
    #[serde(default)]
    code: Option<String>,
}

/// What reading a vault file through `fs_attach_file` came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttachedRead {
    /// The tool refused: the file does not exist, is not readable, or exceeds the attachment limit. `code` is the
    /// tool's stable error code (`path/not-found` for a missing file), for a caller that must tell those apart.
    Refused {
        code: Option<String>,
    },
    /// The file was read but is not text, so it is not treated as a string.
    NotText,
    Text(AttachedText),
}

/// Reads `vault_path`'s complete text content and its hash with one `fs_attach_file` call: the one way this crate
/// reads a vault file. Never `fs_read_text_file`, which truncates at the inline threshold and so would silently give
/// a caller part of a file, and which needs a second call for the hash. A caller that must tell a file that could not
/// be read from one that is not text uses this; the rest use [`read_attached_text`].
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the transport itself fails.
pub async fn read_attached(client: &McpClient, vault_path: &str) -> Result<AttachedRead, McpCallError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(vault_path.to_owned()));
    let result = client.call_tool("fs_attach_file".to_owned(), args).await?;
    if result.is_error == Some(true) {
        let code = result.into_typed::<RefusalCode>().ok().and_then(|failure| failure.code);
        return Ok(AttachedRead::Refused { code });
    }
    // `fs_attach_file` embeds the file as a resource block followed by a hash block; the shared decoder turns that
    // into `{ content, content_hash }`, or `{ blob, mime_type }` for a binary file.
    let Ok(ToolResultJson(Value::Object(fields))) = ToolResultJson::try_from(result) else {
        return Ok(AttachedRead::NotText);
    };
    Ok(match fields.get("content").and_then(Value::as_str) {
        Some(text) => AttachedRead::Text(AttachedText {
            text: text.to_owned(),
            content_hash: fields.get("content_hash").and_then(Value::as_str).map(str::to_owned),
        }),
        None => AttachedRead::NotText,
    })
}

/// [`read_attached`] for a caller that only needs the text and its hash: `None` for a file that could not be read or
/// is not text.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the transport itself fails.
pub async fn read_attached_text(client: &McpClient, vault_path: &str) -> Result<Option<AttachedText>, McpCallError> {
    Ok(match read_attached(client, vault_path).await? {
        AttachedRead::Text(read) => Some(read),
        AttachedRead::Refused { .. } | AttachedRead::NotText => None,
    })
}

/// [`read_attached_text`], keeping only the text, for a caller that has no use for the hash.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the transport itself fails.
pub async fn read_full_text(client: &McpClient, vault_path: &str) -> Result<Option<String>, McpCallError> {
    Ok(read_attached_text(client, vault_path).await?.map(|read| read.text))
}

/// A vault Markdown note's own complete content with every link, in the body and the frontmatter, already rewritten
/// in place to a vault-rooted path or an unresolved marker ([`crate::vault_links::classify_expanded_target`]),
/// `fs_attach_file`'s own `expand_links: true` result (`d-022` in the vault). This carries no hash: it is never the
/// bytes the file holds, only a reading of it, and must never be passed to `save_body`/`fs_write_file` or any other
/// write path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpandedText {
    pub text: String,
}

/// Reads `vault_path` (a `.md` note) with every link already expanded to a vault-rooted path, via one
/// `fs_attach_file` call: the read [`crate::note_render::render_note`] resolves a note's links through,
/// never one MCP round trip per distinct link target. `None` when the tool refused the read (missing, not
/// Markdown, too large) or its result did not actually report `links_expanded` (a defensive check against a server
/// that changed shape underneath this crate, matching [`read_attached`]'s own guard against `fs_attach_file`'s
/// other embedded-resource shape).
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the transport itself fails.
pub async fn read_expanded_text(client: &McpClient, vault_path: &str) -> Result<Option<ExpandedText>, McpCallError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(vault_path.to_owned()));
    args.insert("expand_links".to_owned(), Value::Bool(true));
    let result = client.call_tool("fs_attach_file".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Ok(None);
    }
    let Ok(ToolResultJson(Value::Object(fields))) = ToolResultJson::try_from(result) else {
        return Ok(None);
    };
    let confirmed_expanded = fields.get("links_expanded").and_then(Value::as_bool) == Some(true);
    Ok(fields
        .get("content")
        .and_then(Value::as_str)
        .filter(|_| confirmed_expanded)
        .map(|text| ExpandedText { text: text.to_owned() }))
}

#[derive(Debug, Default, Deserialize)]
struct FrontmatterMetaResult {
    #[serde(default)]
    body_start_line: usize,
}

/// `frontmatter_read`'s own 1-based source line at which a real file's
/// body begins, `None` when the file is absent or not a valid frontmatter
/// document.
async fn body_start_line(client: &McpClient, vault_name: &str, full_path: &str) -> Option<usize> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    match client.call_tool("frontmatter_read".to_owned(), args).await {
        Ok(result) if result.is_error != Some(true) => result
            .into_typed::<FrontmatterMetaResult>()
            .ok()
            .map(|read| read.body_start_line),
        _ => None,
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileInfoResult {
    #[serde(default)]
    content_hash: Option<String>,
}

/// `fs_get_file_info`'s own current content hash, `None` on any failure -
/// a missing hash only disables a later save's optimistic-concurrency
/// check, never blocks the read outright.
pub async fn file_hash(client: &McpClient, vault_name: &str, full_path: &str) -> Option<String> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    match client.call_tool("fs_get_file_info".to_owned(), args).await {
        Ok(result) if result.is_error != Some(true) => result
            .into_typed::<FileInfoResult>()
            .ok()
            .and_then(|info| info.content_hash),
        _ => None,
    }
}

/// The lines from `content` at and after `body_start_line`
/// (`frontmatter_read`'s own 1-based source line number, converted here to
/// a 0-based slice index), rejoined as one trailing-newline-terminated
/// string (empty when there is nothing past the header).
#[must_use]
pub fn body_from_lines(content: &str, body_start_line: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start = body_start_line.saturating_sub(1).min(lines.len());
    let body = lines[start..].join("\n");
    if body.is_empty() { body } else { format!("{body}\n") }
}

/// The lines from `content` strictly before `body_start_line`, rejoined
/// the same way: the frontmatter block kept byte-for-byte, since a
/// body-only save never touches it.
#[must_use]
pub fn header_from_lines(content: &str, body_start_line: usize) -> String {
    let body_start_line = body_start_line.saturating_sub(1);
    let lines: Vec<&str> = content.lines().collect();
    let end = body_start_line.min(lines.len());
    let header = lines[..end].join("\n");
    if header.is_empty() {
        header
    } else {
        format!("{header}\n")
    }
}

/// A file's own current, editable body plus the hash a save must present
/// as `expected_hash` to avoid clobbering a concurrent edit - the shared
/// "load a real file's own editable body" implementation behind
/// `wiki::load_page_body`, `insights::load_note_body`,
/// `profile_docs::load_file_body`, and `entity_docs::load_file_body`.
/// `None` for the body split when the file is absent or not a valid
/// frontmatter document - the caller falls back to the file's own full raw
/// content in that case, so an edit form still has something to show
/// rather than nothing.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the transport itself fails.
pub async fn load_editable_body(
    client: &McpClient,
    vault_name: &str,
    full_path: &str,
    raw: &str,
) -> Result<(String, Option<String>), McpCallError> {
    let (start_line, hash) = tokio::join!(
        body_start_line(client, vault_name, full_path),
        file_hash(client, vault_name, full_path),
    );
    let body = start_line.map_or_else(|| raw.to_owned(), |line| body_from_lines(raw, line));
    Ok((body, hash))
}

/// Replaces a real file's own body with `new_body`, keeping its existing
/// frontmatter block byte-for-byte - the shared save implementation behind
/// `wiki::save_page_body`, `insights::save_note_body`,
/// `profile_docs::save_file_body`, and `entity_docs::save_file_body`.
/// `raw` is the file's own freshly re-read content (never trusting a
/// client-supplied header), matching every caller's own "revalidate at the
/// point of mutation" contract. `Ok(None)` when `full_path` is not a valid
/// frontmatter document - the caller maps that to its own `NotFound`
/// variant. `Ok(Some(false))` on a lost conflict (`expected_hash` no
/// longer matches the file's current content), never an error.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the transport itself fails.
pub async fn save_body(
    client: &McpClient,
    vault_name: &str,
    full_path: &str,
    raw: &str,
    new_body: &str,
    expected_hash: Option<String>,
) -> Result<Option<bool>, McpCallError> {
    let Some(start_line) = body_start_line(client, vault_name, full_path).await else {
        return Ok(None);
    };
    let mut new_content = header_from_lines(raw, start_line);
    let trimmed_body = new_body.trim_end_matches('\n');
    if !trimmed_body.is_empty() {
        new_content.push_str(trimmed_body);
        new_content.push('\n');
    }

    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    args.insert("content".to_owned(), Value::String(new_content));
    if let Some(hash) = expected_hash.filter(|hash| !hash.is_empty()) {
        args.insert("expected_hash".to_owned(), Value::String(hash));
    }
    let result = client.call_tool("fs_write_file".to_owned(), args).await?;
    Ok(Some(result.is_error != Some(true)))
}

#[cfg(test)]
#[path = "vault_content_test.rs"]
mod tests;
