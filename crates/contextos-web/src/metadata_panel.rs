//! The generic frontmatter metadata panel shared by every screen that edits a Markdown
//! item's frontmatter as a set of real fields rather than raw YAML text - Notes today
//! ([`crate::routes::notes`]), and Wiki, Insights, and Entity-folders next. None of the
//! three items in this module has any behaviour specific to one screen's own frontmatter
//! shape: each operates on a plain `serde_json::Map<String, Value>`, exactly as
//! `frontmatter_read`/`frontmatter_update` already do, so this module sits parallel to
//! `notes.rs`/`wiki.rs` - the domain logic a `routes::*` HTTP adapter calls into - just
//! shared across four adapters instead of belonging to one of them.

use contextos_markdown::{FileKind, LinkDestination, LinkRouter, LinkTarget};
use serde_json::{Map, Value};

use crate::mcp_client::McpClient;
use crate::screen_routes::{ScreenRoutes, expanded_frontmatter};
use crate::vault_links::{AMBIGUOUS_HINT, UNROUTED_HINT, WikiLinkValue};

/// One frontmatter field's own metadata-panel row: `key` is the real frontmatter name it
/// patches back to on save (`data-field`, matching `base_view.html`'s identical
/// convention), `value` its current value as plain text - a string field verbatim, a
/// string array comma-joined - and `editable` whether this panel offers it as a text input
/// at all. A field of any other JSON shape (a number, a boolean, a nested object, a mixed
/// array) renders `editable: false`, its `value` the field's own compact JSON text: shown
/// so the operator can see it is there, never silently dropped, but this panel does not
/// attempt to build a form control for every possible YAML shape. `created` and `updated`
/// render `editable: false` too, despite being plain strings: only the server itself ever
/// sets them (`note_create`'s own stamp, `frontmatter_update`'s automatic restamp), so no
/// screen built on this shared panel offers them as something an operator can overwrite.
#[derive(Clone)]
pub(crate) struct MetadataFieldView {
    pub(crate) key: String,
    pub(crate) value: String,
    pub(crate) editable: bool,
    /// The field's links, for a field whose every item is a wikilink (see [`wikilink_values`]); empty for any other
    /// field. Shown as short chips beside the editable `value`, which stays the source of truth on save.
    pub(crate) links: Vec<MetadataLink>,
}

/// One link of a link field, resolved for display: its `text` is the link's alias or, failing that, the last
/// segment of its target, so a full vault path never reaches this narrow panel. A link that names no note is
/// `dead`, with no `href`.
#[derive(Clone)]
pub(crate) struct MetadataLink {
    pub(crate) href: String,
    pub(crate) text: String,
    pub(crate) dead: bool,
    /// Why a dead link has no destination: no such note, or a note no screen shows.
    pub(crate) reason: &'static str,
}

/// Every one of `frontmatter`'s own fields as a panel row, in the item's own real
/// frontmatter order (`frontmatter_read`'s own "ordered YAML frontmatter" contract,
/// `serde_json`'s `preserve_order` feature) - never alphabetised or filtered down to a
/// fixed subset, matching this panel's own "full frontmatter editor" design.
pub(crate) fn metadata_field_views(frontmatter: &Map<String, Value>) -> Vec<MetadataFieldView> {
    frontmatter
        .iter()
        .map(|(key, value)| match value {
            Value::String(text) if is_server_managed_timestamp(key) => MetadataFieldView {
                key: key.clone(),
                value: text.clone(),
                editable: false,
                links: Vec::new(),
            },
            Value::String(text) => MetadataFieldView {
                key: key.clone(),
                value: text.clone(),
                editable: true,
                links: Vec::new(),
            },
            Value::Array(items) if items.iter().all(Value::is_string) => MetadataFieldView {
                key: key.clone(),
                value: items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
                editable: true,
                links: Vec::new(),
            },
            other => MetadataFieldView {
                key: key.clone(),
                value: serde_json::to_string(other).unwrap_or_default(),
                editable: false,
                links: Vec::new(),
            },
        })
        .collect()
}

/// Whether `key` is a frontmatter field only the server itself ever sets, so this panel
/// never offers it as an editable text input regardless of its JSON shape.
fn is_server_managed_timestamp(key: &str) -> bool {
    matches!(key, "created" | "updated")
}

/// A field's wikilinks in order, when the field is a string that is one wikilink or a non-empty array whose every
/// item is one; `None` for anything else, so a text field that merely mentions a link stays plain text.
pub(crate) fn wikilink_values(value: &Value) -> Option<Vec<WikiLinkValue>> {
    let links: Option<Vec<WikiLinkValue>> = match value {
        Value::String(text) => WikiLinkValue::try_from(text.as_str()).ok().map(|link| vec![link]),
        Value::Array(items) => items
            .iter()
            .map(|item| item.as_str().and_then(|text| WikiLinkValue::try_from(text).ok()))
            .collect(),
        _ => None,
    };
    links.filter(|links| !links.is_empty())
}

/// [`metadata_field_views`] with each link field's links shown as short chips that open the screen showing the note
/// each link names, by the server's own resolution of them: `expanded` is the same frontmatter as the server expanded
/// it (`fs_attach_file` or `frontmatter_read` with `expand_links`), and `routes` the routes the page is built with. A
/// field whose expanded value does not line up item for item with the value shown has its links shown as not found.
pub(crate) fn link_field_views(
    frontmatter: &Map<String, Value>,
    expanded: &Map<String, Value>,
    routes: &ScreenRoutes,
) -> Vec<MetadataFieldView> {
    let mut views = metadata_field_views(frontmatter);
    for view in &mut views {
        let Some(links) = frontmatter.get(&view.key).and_then(wikilink_values) else {
            continue;
        };
        let expanded_links = expanded
            .get(&view.key)
            .and_then(wikilink_values)
            .filter(|expanded_links| expanded_links.len() == links.len());
        for (index, link) in links.iter().enumerate() {
            let target = expanded_links
                .as_ref()
                .and_then(|expanded_links| expanded_links.get(index))
                .map(|expanded_link| {
                    LinkTarget::from(LinkDestination {
                        text: &expanded_link.target,
                        percent_encoded: false,
                    })
                });
            view.links.push(metadata_link(String::from(link), target, routes));
        }
    }
    views
}

/// One chip: where the link goes, or why it goes nowhere.
fn metadata_link(text: String, target: Option<LinkTarget>, routes: &ScreenRoutes) -> MetadataLink {
    let dead = |reason| MetadataLink {
        href: String::new(),
        text: text.clone(),
        dead: true,
        reason,
    };
    let href = match target {
        Some(LinkTarget::Vault { path, fragment }) => {
            if FileKind::from(path.as_str()).is_page() {
                let anchor = fragment.as_ref().map(contextos_markdown::Fragment::anchor);
                match routes.page(&path, anchor.as_deref()) {
                    Some(href) => href,
                    None => return dead(UNROUTED_HINT),
                }
            } else {
                routes.file(&path)
            }
        }
        Some(LinkTarget::External(url)) => url,
        Some(LinkTarget::Ambiguous(_)) => return dead(AMBIGUOUS_HINT),
        Some(LinkTarget::NotFound(_) | LinkTarget::Unresolved(_) | LinkTarget::Anchor(_)) | None => {
            return dead("No note found");
        }
    };
    MetadataLink {
        href,
        text,
        dead: false,
        reason: "",
    }
}

/// The vault paths `frontmatter`'s link values name, once expanded, so the routes for them can be loaded together.
pub(crate) fn linked_paths(expanded: &Map<String, Value>) -> Vec<String> {
    expanded
        .values()
        .filter_map(wikilink_values)
        .flatten()
        .filter_map(|link| {
            match LinkTarget::from(LinkDestination {
                text: &link.target,
                percent_encoded: false,
            }) {
                LinkTarget::Vault { path, .. } => Some(path),
                _ => None,
            }
        })
        .collect()
}

/// [`link_field_views`] for a note the page has not already read expanded: its frontmatter is read once with every
/// link value expanded, and the routes those links need are loaded together.
pub(crate) async fn linked_field_views(
    client: &McpClient,
    vault_name: &str,
    from_note: &str,
    frontmatter: &Map<String, Value>,
) -> Vec<MetadataFieldView> {
    let expanded = expanded_frontmatter(client, vault_name, from_note)
        .await
        .unwrap_or_default();
    let paths = linked_paths(&expanded);
    let routes = ScreenRoutes::load(client, vault_name, paths.iter().map(String::as_str)).await;
    link_field_views(frontmatter, &expanded, &routes)
}

/// Splits a comma-joined list back into its items (trimmed, empty ones dropped), never on a comma inside a
/// `[[wikilink]]`: a link's display text or path may itself contain one, and splitting there would corrupt the link.
fn split_list_items(value: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut depth = 0_usize;
    let mut start = 0;
    for (index, character) in value.char_indices() {
        match character {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                items.push(&value[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    items.push(&value[start..]);
    items
        .into_iter()
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .collect()
}

/// Reconstructs a real `frontmatter_update` patch from `submitted`'s own plain-text values,
/// deciding each key's shape from `current`'s own existing frontmatter rather than trusting
/// anything the client claims about it (`security.md`'s "revalidate ... at the point of
/// mutation"): a key that is currently a string array is split back into one on `,`
/// (trimmed, empty items dropped - the same shape [`metadata_field_views`] rendered it as),
/// every other key is a plain string. A submitted key the item's own frontmatter does not
/// currently have (a stale form, a tampered request) is written as a plain string: never
/// silently dropped, but never guessed to be a list either.
pub(crate) fn metadata_patch(
    current: &Map<String, Value>,
    submitted: std::collections::BTreeMap<String, String>,
) -> Map<String, Value> {
    let mut patch = Map::new();
    for (key, value) in submitted {
        let as_value = match current.get(&key) {
            Some(Value::Array(items)) if items.iter().all(Value::is_string) => Value::Array(
                split_list_items(&value)
                    .into_iter()
                    .map(|item| Value::String(item.to_owned()))
                    .collect(),
            ),
            _ => Value::String(value),
        };
        patch.insert(key, as_value);
    }
    patch
}

#[cfg(test)]
#[path = "metadata_panel_test.rs"]
mod tests;
