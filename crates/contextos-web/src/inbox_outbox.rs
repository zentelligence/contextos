//! Inbox/Outbox/processed-marker directory listing and navigation: every
//! screen browses one level of a real directory tree at a time (folders
//! and files both), discovered through `fs_list_directory` like
//! `apps.rs`/`jobs.rs` already discover their own registries, never
//! through the generic vault content route's `index.md`-only rendering.
//! Each tree (`inbox_outbox_config::InboxOutboxConfig`'s `inbox_dir`,
//! `outbox_dir`, `processed_dir`) is assumed to nest arbitrarily deep, per
//! the task this module was built for: routes navigate into a subfolder by
//! its own path beneath the screen's root directory, never a flattened,
//! single-level view of the configured root alone.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::inbox_outbox_config::InboxOutboxConfig;
use crate::mcp_client::{McpCallError, McpClient};

/// One file sitting directly inside a browsed directory. `index.md` is
/// excluded: it is the directory's own managed listing, not an item
/// within it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryItem {
    pub name: String,
    pub size: Option<u64>,
    pub modified: Option<String>,
}

/// One subfolder sitting directly inside a browsed directory, by name
/// only: a folder row's own size/modified time is never shown (the design
/// mock's per-item metadata is file-only), and its own contents are learnt
/// only by navigating into it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryFolder {
    pub name: String,
}

/// One directory level's own real contents: its immediate subfolders and
/// its immediate files, each in name order. Never recursive: browsing
/// nested content means navigating into a listed folder, one level at a
/// time, matching an ordinary file browser rather than a flattened view.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DirectoryListing {
    pub folders: Vec<DirectoryFolder>,
    pub files: Vec<DirectoryItem>,
}

#[derive(Debug, Deserialize)]
struct RawEntry {
    name: String,
    kind: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    modified: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListDirectoryResult {
    entries: Vec<RawEntry>,
}

/// Splits `entries` into `DirectoryListing`'s own folders and files (never
/// `index.md`), each in name order: the one piece of this module with no
/// MCP call in it, so it is exercised directly by unit tests rather than
/// only through a real vault's contract tests.
fn listing_from_entries(entries: Vec<RawEntry>) -> DirectoryListing {
    let mut folders: Vec<DirectoryFolder> = entries
        .iter()
        .filter(|entry| entry.kind == "dir")
        .map(|entry| DirectoryFolder {
            name: entry.name.clone(),
        })
        .collect();
    folders.sort_by(|a, b| a.name.cmp(&b.name));
    let mut files: Vec<DirectoryItem> = entries
        .into_iter()
        .filter(|entry| entry.kind == "file" && entry.name != "index.md")
        .map(|entry| DirectoryItem {
            name: entry.name,
            size: entry.size,
            modified: entry.modified,
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    DirectoryListing { folders, files }
}

/// Lists `directory`'s own immediate contents (one level, never
/// recursive). An absent directory, or one this vault has no listing
/// permission for, is not an error: it yields an empty listing, matching
/// `apps.rs`'s/`jobs.rs`'s identical "registry directory simply does not
/// exist yet" handling for their own discovery passes.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself
/// fails.
pub async fn list_directory(
    client: &McpClient,
    vault_name: &str,
    directory: &str,
) -> Result<DirectoryListing, McpCallError> {
    Ok(list_existing_directory(client, vault_name, directory)
        .await?
        .unwrap_or_default())
}

/// [`list_directory`], but telling a directory that does not exist (or cannot be listed) apart from one that is
/// empty: `None` for the former. A screen uses this to answer a request for a folder that is not there with "not
/// found" instead of an empty folder that offers uploads.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself
/// fails.
pub async fn list_existing_directory(
    client: &McpClient,
    vault_name: &str,
    directory: &str,
) -> Result<Option<DirectoryListing>, McpCallError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{directory}")));
    args.insert("with_sizes".to_owned(), Value::Bool(true));
    let result = client.call_tool("fs_list_directory".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Ok(None);
    }
    let Ok(listing) = result.into_typed::<ListDirectoryResult>() else {
        return Ok(None);
    };
    Ok(Some(listing_from_entries(listing.entries)))
}

/// The normalised vault-relative path of `raw`, which must name something inside the pending Inbox: a folder or
/// file beneath `config.inbox_dir`, never the Inbox directory itself, the processed tree, or any other part of the
/// vault. What an ingest request may act on.
///
/// # Errors
///
/// Returns a short, user-facing reason when `raw` is malformed or names anything outside the pending Inbox.
pub fn pending_item_path(config: &InboxOutboxConfig, raw: &str) -> Result<String, &'static str> {
    let segments = sanitised_relative_path(raw)?;
    match segments.split_first() {
        Some((first, rest)) if *first == config.inbox_dir && !rest.is_empty() => Ok(segments.join("/")),
        _ => Err("must name an item inside the pending Inbox"),
    }
}

/// Best-effort MIME type for a file by its own name, via `mime_guess`'s registry-backed
/// extension table: a directory listing carries no file content to content-sniff, so extension
/// guessing is the only source available here (unlike an upload's own boundary check, which has
/// the just-submitted bytes in hand). Returns `None` for anything `mime_guess` doesn't recognise;
/// callers fall back to a generic type rather than leaving the row blank.
#[must_use]
pub fn mime_type_for_name(name: &str) -> Option<String> {
    mime_guess::from_path(name)
        .first()
        .map(|mime| mime.essence_str().to_owned())
}

/// A byte count as a short, human-scale label: whole kibibytes below one
/// mebibyte, one decimal place of mebibytes at or above it. Matches the
/// Command Centre design mock's own size formatting for Inbox/Outbox rows.
/// `bytes as f64` never meaningfully loses precision here: an Inbox/Outbox
/// item is capped in the tens of megabytes by the upload limit, many
/// orders of magnitude below `f64`'s 52-bit mantissa's own exact-integer
/// range.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn format_size(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{:.1} MB", bytes as f64 / MIB as f64)
    } else {
        let kib = (bytes as f64 / 1024.0).round().max(1.0);
        format!("{kib:.0} KB")
    }
}

/// A `modified` timestamp (an RFC 3339 instant, e.g.
/// `2026-09-14T10:23:45.123456789+00:00`) as a short `YYYY-MM-DD HH:MM`
/// label. A value shorter than expected, or absent, renders as `"-"` rather
/// than panicking on a slice out of range: this is a display nicety, never
/// a parsed, validated instant a security or ordering decision depends on.
#[must_use]
pub fn format_modified(modified: Option<&str>) -> String {
    modified
        .filter(|raw| raw.len() >= 16)
        .map_or_else(|| "-".to_owned(), |raw| raw[..16].replacen('T', " ", 1))
}

/// Validates `raw` as a single path segment: non-empty (after trimming),
/// carrying no `/` or `\`, and not `.`/`..`. Shared by the Inbox upload
/// form's own filename, the Inbox/Outbox settings section's own
/// `inbox_dir`/`outbox_dir`/`processed_dir` fields, and (one segment at a
/// time) [`sanitised_relative_path`]: all are operator-supplied strings
/// that must resolve to a real, unambiguous path component, never a
/// caller-chosen traversal of their own.
///
/// # Errors
///
/// Returns a short, user-facing reason naming what is wrong, not a typed
/// error: every caller renders this directly into an HTML error message.
pub fn sanitised_path_segment(raw: &str) -> Result<&str, &'static str> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("must not be empty");
    }
    if name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err("must be a single plain name, not a path");
    }
    Ok(name)
}

/// Validates `raw` as a `/`-separated relative path with no empty, `.`, or
/// `..` segment: the shape a browsed Inbox/Outbox `path` query parameter
/// must have. An empty `raw` (the screen's own root) is valid and yields
/// no segments at all.
///
/// # Errors
///
/// Returns a short, user-facing reason naming what is wrong.
pub fn sanitised_relative_path(raw: &str) -> Result<Vec<&str>, &'static str> {
    let trimmed = raw.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    trimmed.split('/').map(sanitised_path_segment).collect()
}

/// Percent-encodes `value` for safe embedding as a URL path segment (or a
/// query-string component, for a caller that still needs one): every byte
/// outside RFC 3986's unreserved set (`ALPHA` / `DIGIT` / `-` / `.` / `_` /
/// `~`) is escaped, except a literal `/`, which stays readable in a browsed
/// Inbox/Outbox/Wiki `path` link and is not itself a path or query-string
/// delimiter that needs escaping.
#[must_use]
pub fn encode_query_value(value: &str) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => encoded.push(byte as char),
            // `write!` to a `String` never actually fails; the `Result` it
            // returns exists only because `fmt::Write` is a shared trait
            // with fallible implementors.
            _ => drop(write!(encoded, "%{byte:02X}")),
        }
    }
    encoded
}

/// The inverse of [`encode_query_value`]: decodes a `%XX`-escaped string back to its original
/// bytes, `None` for a malformed escape or a result that is not valid UTF-8. Shared by every
/// consumer that parses its own percent-encoding back out by hand rather than relying on axum's
/// own extractor-level decoding (a cookie value, or a raw request path inspected by middleware
/// before routing) - `recently_accessed`'s own cookie-decoding need and `last_visited`'s identical
/// one are this workspace's own "two real call sites justify extraction" precedent
/// (`routes::wiki_view`'s own doc comment names the same pattern for `tree_view`).
#[must_use]
pub fn decode_percent_encoded(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..=index + 2]).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// Where an Inbox screen request is browsing: the merged top-level view
/// (both `inbox_dir` and `processed_dir` at once), or a real subtree of
/// exactly one of them. Outbox has no equivalent duality (`OutboxLocation`
/// below): every Outbox request browses `outbox_dir`'s own single tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InboxLocation {
    MergedRoot,
    Tree {
        root_dir: String,
        status: &'static str,
        sub_segments: Vec<String>,
    },
}

impl InboxLocation {
    /// The real vault-relative directory this location names, or `None`
    /// for the merged root (which has no single directory of its own).
    #[must_use]
    pub fn directory(&self) -> Option<String> {
        match self {
            Self::MergedRoot => None,
            Self::Tree {
                root_dir, sub_segments, ..
            } => {
                let mut directory = root_dir.clone();
                for segment in sub_segments {
                    directory.push('/');
                    directory.push_str(segment);
                }
                Some(directory)
            }
        }
    }

    /// The segments that address this location in the screen's own URL, after `/{vault}/inbox/`: a pending
    /// location's own subfolders (the Inbox directory itself is the screen's root, never a segment), or the
    /// processed directory's name followed by its subfolders. Empty at the merged root, which renders no ancestor
    /// trail. What [`ancestor_rows`] needs.
    #[must_use]
    pub fn url_segments(&self) -> Vec<String> {
        match self {
            Self::MergedRoot => Vec::new(),
            Self::Tree {
                root_dir,
                status,
                sub_segments,
            } => {
                let mut segments = Vec::with_capacity(sub_segments.len() + 1);
                if *status == "processed" {
                    segments.push(root_dir.clone());
                }
                segments.extend(sub_segments.iter().cloned());
                segments
            }
        }
    }

    /// [`Self::url_segments`] joined into the path that follows `/{vault}/inbox/`, empty at the merged root.
    #[must_use]
    pub fn url_path(&self) -> String {
        self.url_segments().join("/")
    }
}

/// Resolves a browsed Inbox `path` (the wildcard segment of `GET /{{vault_name}}/inbox/{{*path}}`, or the
/// `/execute/upload`/`/execute/ingest` action routes' own identically addressed target folder) against `config`'s
/// own two trees. The Inbox directory is the screen's root, so a `path` names a folder beneath it directly, without
/// repeating the directory's own name; the one reserved first segment is `config.processed_dir`'s name, which
/// selects the processed tree instead (so a pending folder that shares that name is reachable only through the
/// processed tree's own address). An absent or empty `raw_path` is the merged root. A `path` can only ever name a
/// directory within one of the two trees, never an unrelated part of the vault.
///
/// # Errors
///
/// Returns a short, user-facing reason when `raw_path` itself is
/// malformed (a traversal or empty segment).
pub fn resolve_inbox_location(
    config: &InboxOutboxConfig,
    raw_path: Option<&str>,
) -> Result<InboxLocation, &'static str> {
    let Some(raw_path) = raw_path.filter(|path| !path.trim().is_empty()) else {
        return Ok(InboxLocation::MergedRoot);
    };
    let segments = sanitised_relative_path(raw_path)?;
    let Some((first, rest)) = segments.split_first() else {
        return Ok(InboxLocation::MergedRoot);
    };
    if *first == config.processed_dir {
        return Ok(InboxLocation::Tree {
            root_dir: config.processed_dir.clone(),
            status: "processed",
            sub_segments: rest.iter().map(|segment| (*segment).to_owned()).collect(),
        });
    }
    Ok(InboxLocation::Tree {
        root_dir: config.inbox_dir.clone(),
        status: "pending",
        sub_segments: segments.iter().map(|segment| (*segment).to_owned()).collect(),
    })
}

/// Resolves a browsed Outbox `path` (the wildcard segment of `GET /{{vault_name}}/outbox/{{*path}}`) against
/// `outbox_dir`: the real directory to list (`outbox_dir` itself at the root) and the URL segments
/// [`ancestor_rows`] needs (empty at the root, since the root renders no ancestor trail). The Outbox directory is
/// the screen's root, so a `path` names a folder beneath it directly, without repeating the directory's own name,
/// and can never name anything outside it.
///
/// # Errors
///
/// Returns a short, user-facing reason when `raw_path` itself is
/// malformed (a traversal or empty segment).
pub fn resolve_outbox_directory(
    outbox_dir: &str,
    raw_path: Option<&str>,
) -> Result<(String, Vec<String>), &'static str> {
    let Some(raw_path) = raw_path.filter(|path| !path.trim().is_empty()) else {
        return Ok((outbox_dir.to_owned(), Vec::new()));
    };
    let url_segments: Vec<String> = sanitised_relative_path(raw_path)?
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut directory = outbox_dir.to_owned();
    for segment in &url_segments {
        directory.push('/');
        directory.push_str(segment);
    }
    Ok((directory, url_segments))
}

/// One row of the ancestor trail rendered above a browsed folder's own
/// contents: a named, indented row per folder on the way down from the
/// screen's own root, the current folder itself shown last with no link.
/// Never a `".."` row: a non-technical operator reads a real folder name,
/// not a directory-navigation idiom.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AncestorRow {
    pub label: String,
    pub href: Option<String>,
    pub depth: usize,
}

/// Builds the ancestor trail for a browsed folder: `screen_label` (`"Inbox"`/`"Outbox"`) linking back to the
/// screen's root, then one row per segment of `url_segments` (the segments that follow the screen's own address, so
/// the screen's root directory is the first row itself and never a second, repeated one), the last one current and
/// unlinked.
#[must_use]
pub fn ancestor_rows(
    vault_name: &str,
    breadcrumb: &str,
    screen_label: &str,
    url_segments: &[String],
) -> Vec<AncestorRow> {
    if url_segments.is_empty() {
        return Vec::new();
    }
    let mut rows = vec![AncestorRow {
        label: screen_label.to_owned(),
        href: Some(crate::urls::VaultUrls::from(vault_name).tray(breadcrumb, "")),
        depth: 0,
    }];
    let mut accumulated = String::new();
    let last_index = url_segments.len() - 1;
    for (index, segment) in url_segments.iter().enumerate() {
        if !accumulated.is_empty() {
            accumulated.push('/');
        }
        accumulated.push_str(segment);
        let is_current = index == last_index;
        rows.push(AncestorRow {
            label: segment.clone(),
            href: (!is_current).then(|| crate::urls::VaultUrls::from(vault_name).tray(breadcrumb, &accumulated)),
            depth: index + 1,
        });
    }
    rows
}

#[cfg(test)]
#[path = "inbox_outbox_test.rs"]
mod tests;
