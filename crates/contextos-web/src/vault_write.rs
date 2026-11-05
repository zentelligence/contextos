//! Whole-file writes from the Vault Settings management pages, through `fs_write_file`, with its refusals kept as
//! data. The server's write pipeline already gives the two guarantees these pages rely on: a write with an
//! `expected_hash` succeeds only when the file still holds exactly the content that hash came from, and a write
//! without one succeeds only when nothing exists at the path yet. The screens never pass `force`.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClient};
use crate::vault_relative_path::VaultRelativePath;

/// The stable code for a write whose `expected_hash` no longer matches the file.
pub const CONFLICT: &str = "io/conflict";
/// The stable code for a write without `expected_hash` to a path that already exists.
pub const ALREADY_EXISTS: &str = "io/expected-hash-required";

/// The stable code for a note whose title is empty or repeated by its description, summary, or excerpt.
pub const NOTE_RULES: &str = "format/note";
/// The stable code for a body with a malformed Obsidian link or callout.
pub const MARKDOWN: &str = "format/markdown";

/// What a write came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WriteOutcome {
    Written,
    /// The server refused the write, with its stable code when it gave one.
    Refused {
        code: Option<String>,
    },
}

impl WriteOutcome {
    /// A sentence saying why a refused write did not happen, never echoing the server's message (which names the
    /// file's full host path).
    #[must_use]
    pub fn refusal_reason(&self) -> Option<String> {
        match self {
            Self::Written => None,
            Self::Refused { code } => Some(match code.as_deref() {
                Some(CONFLICT) => "This file changed after you opened it, so your edit was not saved. Copy anything \
                                   you want to keep, then reopen the file to see its current content."
                    .to_owned(),
                Some(ALREADY_EXISTS) => "Something with that name already exists here.".to_owned(),
                Some(NOTE_RULES) => "The title cannot be empty, and the description cannot repeat the title. Nothing \
                                     was saved."
                    .to_owned(),
                Some(MARKDOWN) => "The body has a malformed Obsidian link or callout (an unclosed or empty [[link]], \
                                   or a broken callout line), so nothing was saved. Correct it and save again."
                    .to_owned(),
                Some(code) => format!("The vault refused this change ({code})."),
                None => "The vault refused this change.".to_owned(),
            }),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Refusal {
    #[serde(default)]
    code: Option<String>,
}

/// Writes `content` to `path` in `vault_name`: replacing the file when `expected_hash` is given, or creating it when it
/// is not.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the transport itself fails.
pub async fn write_text(
    client: &McpClient,
    vault_name: &str,
    path: &VaultRelativePath,
    content: &str,
    expected_hash: Option<&str>,
) -> Result<WriteOutcome, McpCallError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{path}")));
    args.insert("content".to_owned(), Value::String(content.to_owned()));
    if let Some(hash) = expected_hash.filter(|hash| !hash.is_empty()) {
        args.insert("expected_hash".to_owned(), Value::String(hash.to_owned()));
    }
    let result = client.call_tool("fs_write_file".to_owned(), args).await?;
    if result.is_error == Some(true) {
        let code = result.into_typed::<Refusal>().ok().and_then(|refusal| refusal.code);
        return Ok(WriteOutcome::Refused { code });
    }
    Ok(WriteOutcome::Written)
}

#[cfg(test)]
#[path = "vault_write_test.rs"]
mod tests;
