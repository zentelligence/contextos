//! `.contextos/entities.toml`: the Command Centre entity switcher's
//! definitive source, read and written through ordinary MCP tool calls, mirroring `.contextos/jobs.toml`'s own
//! read/seed/write discipline ([`crate::scheduler`]'s `read_state`/`write_state`).
//!
//! Distinct from [`crate::rendering::shell`]'s existing `entity_for` accent, which colours the *vault* switcher by
//! `vault_info` position and predates this file: the two concepts share the `--entity-N` token family
//! but are assigned from different sources and must never
//! be conflated.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde_json::{Map, Value};

use crate::mcp_client::McpClient;
use crate::vault_content::read_attached_text;

/// The hand-authored index this list is seeded from on first use.
const ENTITY_INDEX_PATH: &str = "memory/entities/index.md";
/// `--entity-1` to `--entity-6`: the accent cycles
/// back to `--entity-1` past this count rather than growing unbounded.
const MAX_ACCENT: u8 = 6;

/// `.contextos/entities.toml`'s full contents and one entry of it: defined once in `contextos-core`, which the
/// server's own managed-index service also reads to derive each `index.md`'s `entity`.
pub use contextos_core::{ENTITIES_PATH, EntitiesFile as EntitiesState, EntityRecord as EntityEntry};

/// One row parsed from the vault's own hand-authored entity index table:
/// a link-derived slug plus display name, in document row order. Never the
/// `<!-- contextos:index:begin -->` managed block a directory's own
/// `index.md` also carries (alphabetically auto-generated, carrying no
/// operator-chosen order): [`parse_index_rows`] stops at the first table's
/// own closing tag, before that block's own table is ever reached.
#[derive(Debug, PartialEq, Eq)]
struct IndexRow {
    slug: String,
    name: String,
}

/// Extracts `markdown`'s first GFM table's first two columns (`File`,
/// `Entity` in the base scaffold's own convention) as slug/name pairs, in
/// row order. A row whose first column carries no link and no text, or
/// whose second column is empty, is skipped rather than recorded with a
/// blank field. Returns an empty list when `markdown` has no table at all.
fn parse_index_rows(markdown: &str) -> Vec<IndexRow> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(markdown, options);

    let mut rows = Vec::new();
    let mut in_table = false;
    let mut table_done = false;
    let mut in_row = false;
    let mut cell_index = 0usize;
    let mut cell_link_dest: Option<String> = None;
    let mut cell_text = String::new();
    let mut row_slug: Option<String> = None;
    let mut row_name: Option<String> = None;

    for event in parser {
        if table_done {
            break;
        }
        match event {
            Event::Start(Tag::Table(_)) => in_table = true,
            Event::End(TagEnd::Table) => table_done = true,
            Event::Start(Tag::TableRow) if in_table => {
                in_row = true;
                cell_index = 0;
                row_slug = None;
                row_name = None;
            }
            Event::End(TagEnd::TableRow) if in_row => {
                in_row = false;
                if let (Some(slug), Some(name)) = (row_slug.take(), row_name.take())
                    && !slug.is_empty()
                    && !name.is_empty()
                {
                    rows.push(IndexRow { slug, name });
                }
            }
            Event::Start(Tag::TableCell) if in_row => {
                cell_text.clear();
                cell_link_dest = None;
            }
            Event::Start(Tag::Link { dest_url, .. }) if in_row => {
                cell_link_dest = Some(dest_url.into_string());
            }
            Event::Text(text) if in_row => cell_text.push_str(&text),
            Event::End(TagEnd::TableCell) if in_row => {
                match cell_index {
                    0 => {
                        let source = cell_link_dest.take().unwrap_or_else(|| cell_text.clone());
                        row_slug = slug_from_link_target(&source);
                    }
                    1 => row_name = Some(cell_text.trim().to_owned()),
                    _ => {}
                }
                cell_index += 1;
            }
            _ => {}
        }
    }
    rows
}

/// The file-stem slug a table cell's link target (or, absent a link, its
/// plain text) names: `personal.md` and `memory/entities/widgets.md`
/// (the base scaffold's own two authoring styles for this column) both
/// yield their final path segment with any `.md` suffix stripped. `None`
/// for an empty result.
fn slug_from_link_target(target: &str) -> Option<String> {
    let trimmed = target.trim();
    let file_name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let stem = file_name.strip_suffix(".md").unwrap_or(file_name);
    if stem.is_empty() { None } else { Some(stem.to_owned()) }
}

/// The `--entity-N` accent (1 to `MAX_ACCENT`) for a row at `index`
/// (0-based), cycling back to `--entity-1` past the sixth row.
fn accent_for(index: usize) -> u8 {
    u8::try_from(index % usize::from(MAX_ACCENT)).unwrap_or(0) + 1
}

/// Builds a fresh [`EntitiesState`] from `markdown` (`memory/entities/index.md`'s
/// own content), assigning each row's accent by its position.
fn seed_from_index(markdown: &str) -> EntitiesState {
    let entities = parse_index_rows(markdown)
        .into_iter()
        .enumerate()
        .map(|(index, row)| EntityEntry {
            slug: row.slug,
            name: row.name,
            accent: accent_for(index),
        })
        .collect();
    EntitiesState { entities }
}

async fn read_index_markdown(client: &McpClient, vault_name: &str) -> Option<String> {
    read_attached_text(client, &format!("{vault_name}://{ENTITY_INDEX_PATH}"))
        .await
        .ok()
        .flatten()
        .map(|read| read.text)
}

async fn read_state(client: &McpClient, vault_name: &str) -> Option<EntitiesState> {
    let read = read_attached_text(client, &format!("{vault_name}://{ENTITIES_PATH}"))
        .await
        .ok()
        .flatten()?;
    toml::from_str(&read.text).ok()
}

/// Writes `state` to `.contextos/entities.toml`, best-effort: a failure
/// here is not surfaced (mirroring `shell::build_nav`'s own "the chrome
/// renders with less data rather than the whole page failing over a
/// nav-shell-only concern"). A page render that seeded a list this call
/// failed to persist simply re-seeds identically on its next render, since
/// `memory/entities/index.md` itself is untouched by that failure.
async fn write_state(client: &McpClient, vault_name: &str, state: &EntitiesState) {
    let Ok(content) = toml::to_string(state) else {
        return;
    };
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{ENTITIES_PATH}")),
    );
    args.insert("content".to_owned(), Value::String(content));
    // `.contextos/entities.toml` has one writer (this seeding path) until an
    // operator edits it directly; an `expected_hash` chain would only guard
    // against a second concurrent writer that does not exist here, matching
    // `scheduler.rs::write_state`'s identical `jobs.toml` reasoning.
    args.insert("force".to_owned(), Value::Bool(true));
    let _ = client.call_tool("fs_write_file".to_owned(), args).await;
}

/// Loads the vault's configured entities: `.contextos/entities.toml` when present and well-formed, otherwise seeded
/// once from `memory/entities/index.md`'s row order and persisted for next time. Never fails outright: an absent
/// index, an unreachable MCP session, or a malformed `entities.toml` all degrade to an empty list (a vault may
/// legitimately have zero configured entities), never an error.
pub async fn load_entities(client: &McpClient, vault_name: &str) -> Vec<EntityEntry> {
    if let Some(state) = read_state(client, vault_name).await {
        return state.entities;
    }
    let Some(index_markdown) = read_index_markdown(client, vault_name).await else {
        return Vec::new();
    };
    let seeded = seed_from_index(&index_markdown);
    if !seeded.entities.is_empty() {
        write_state(client, vault_name, &seeded).await;
    }
    seeded.entities
}

#[cfg(test)]
#[path = "entities_test.rs"]
mod tests;
