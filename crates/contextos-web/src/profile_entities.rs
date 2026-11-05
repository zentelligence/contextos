//! Entities domain logic: unlike Identity/Operating, this group is driven
//! by `.contextos/entities.toml` (`entities::load_entities`, the Command
//! Centre entity switcher's own source, never a second duplicated entity
//! list), not a plain directory listing. One row per configured entity, in
//! its own row order, cross-checked against the real files directly under
//! `memory/entities/` and each entity's own real top-level directory
//! (`entity_docs`'s own root); any real file matching no configured slug is
//! kept as an orphan, shown but never hidden.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{Map, Value};
use time::OffsetDateTime;

use crate::entities::{self, EntityEntry};
use crate::mcp_client::{McpCallError, McpClient};
use crate::notes_template::{self, NotesTemplateError};
use crate::profile_config::{ENTITIES_DIR, ProfileConfig};
use crate::profile_docs::{self, ProfileFileLink};
use crate::vault_content::read_attached_text;

/// One configured entity's own Profile row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfiguredEntityRow {
    pub slug: String,
    pub name: String,
    /// Whether `memory/entities/{{slug}}.md` already exists.
    pub file_exists: bool,
    /// Whether the entity's own top-level directory (`entity_docs`'s own
    /// root) already exists.
    pub folder_exists: bool,
}

impl ConfiguredEntityRow {
    /// Whether either half of "being a configured entity" is still missing
    /// - the sidebar's own flag-for-provisioning condition.
    #[must_use]
    pub fn needs_provisioning(&self) -> bool {
        !self.file_exists || !self.folder_exists
    }
}

/// The Entities group's full view: configured rows first, in
/// `entities.toml`'s own order, then any real file under `memory/entities/`
/// matching no configured slug.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EntitiesView {
    pub configured: Vec<ConfiguredEntityRow>,
    pub orphans: Vec<ProfileFileLink>,
}

/// The files in `files` whose own label matches none of `configured_slugs`,
/// in their existing order.
fn partition_orphans(configured_slugs: &[String], files: Vec<ProfileFileLink>) -> Vec<ProfileFileLink> {
    let configured: HashSet<&str> = configured_slugs.iter().map(String::as_str).collect();
    files
        .into_iter()
        .filter(|file| !configured.contains(file.label.as_str()))
        .collect()
}

#[derive(Debug, Deserialize)]
struct FileInfoResult {
    kind: String,
}

/// Whether a real directory exists at `slug` (the vault's own top-level, not
/// under `memory/entities/`) - `entity_docs`'s own root for this entity.
async fn folder_exists(client: &McpClient, vault_name: &str, slug: &str) -> bool {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{slug}")));
    match client.call_tool("fs_get_file_info".to_owned(), args).await {
        Ok(result) if result.is_error != Some(true) => result
            .into_typed::<FileInfoResult>()
            .is_ok_and(|info| info.kind == "dir"),
        _ => false,
    }
}

/// Loads the Entities group's full view. Never fails outright: an absent
/// `memory/entities/` directory, no configured entities at all, or an
/// unreachable session all degrade to an empty view, matching every other
/// Profile/Knowledge loader's "not yet provisioned is not a fault"
/// contract. The per-entity folder-existence check is a small, bounded
/// sequential fan-out (one call per configured entity), matching
/// `insights::list_shard_notes`'s identical "a real vault's own list is
/// small, not a bulk corpus" reasoning.
#[must_use]
pub async fn load_view(client: &McpClient, vault_name: &str) -> EntitiesView {
    let (configured_entities, files) = tokio::join!(
        entities::load_entities(client, vault_name),
        profile_docs::list_files(client, vault_name, ENTITIES_DIR),
    );

    let mut configured = Vec::with_capacity(configured_entities.len());
    for entity in &configured_entities {
        let expected = format!("{}.md", entity.slug);
        let file_exists = files.iter().any(|file| file.relative_path == expected);
        let folder_exists = folder_exists(client, vault_name, &entity.slug).await;
        configured.push(ConfiguredEntityRow {
            slug: entity.slug.clone(),
            name: entity.name.clone(),
            file_exists,
            folder_exists,
        });
    }

    let configured_slugs: Vec<String> = configured_entities
        .into_iter()
        .map(|entity: EntityEntry| entity.slug)
        .collect();
    let orphans = partition_orphans(&configured_slugs, files);
    EntitiesView { configured, orphans }
}

// ---------------------------------------------------------------------
// Provisioning a configured entity's own missing pieces
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ProvisionEntityError {
    #[error("this entity is not configured")]
    NotConfigured,
    #[error(transparent)]
    Template(#[from] NotesTemplateError),
    #[error(transparent)]
    Write(#[from] McpCallError),
    #[error("the entity's own folder could not be created")]
    FolderRejected,
    #[error("the entity's own profile page could not be created")]
    FileRejected,
}

/// The configured entity-profile template's own raw source, or an empty
/// string for anything absent/unreadable: matches
/// `notes::read_template_source`'s identical "an unresolvable dependency
/// degrades the template, it never fails the whole screen" contract. An
/// empty `entity_template_path` (no template configured at all) never
/// issues a read call.
async fn read_template_source(client: &McpClient, vault_name: &str, config: &ProfileConfig) -> String {
    if config.entity_template_path.is_empty() {
        return String::new();
    }
    read_attached_text(client, &format!("{vault_name}://{}", config.entity_template_path))
        .await
        .ok()
        .flatten()
        .map(|read| read.text)
        .unwrap_or_default()
}

/// Creates whichever of a configured entity's own two pieces - its
/// `memory/entities/{{slug}}.md` profile page, its own top-level directory -
/// does not yet exist, re-checking each fresh rather than trusting a
/// caller-supplied view (matching `profile_docs::save_file_body`'s identical
/// "revalidate at the point of mutation" reasoning). A piece that already
/// exists is left untouched; provisioning an entity with nothing missing is
/// a harmless no-op, never an error.
///
/// # Errors
///
/// Returns [`ProvisionEntityError::NotConfigured`] when `slug` names no
/// entity in `.contextos/entities.toml`, [`ProvisionEntityError::Template`]
/// when the configured template contains a Templater expression or
/// frontmatter line this crate's small substitution subset does not
/// recognise, [`ProvisionEntityError::Write`] when the MCP transport itself
/// fails, and [`ProvisionEntityError::FolderRejected`]/[`ProvisionEntityError::FileRejected`]
/// when the corresponding create call is itself refused.
pub async fn provision_entity(
    client: &McpClient,
    vault_name: &str,
    config: &ProfileConfig,
    slug: &str,
    at: OffsetDateTime,
) -> Result<(), ProvisionEntityError> {
    let configured = entities::load_entities(client, vault_name).await;
    let Some(entity) = configured.into_iter().find(|entity| entity.slug == slug) else {
        return Err(ProvisionEntityError::NotConfigured);
    };

    if !folder_exists(client, vault_name, slug).await {
        let mut args = Map::new();
        args.insert("path".to_owned(), Value::String(format!("{vault_name}://{slug}")));
        let result = client.call_tool("fs_create_directory".to_owned(), args).await?;
        if result.is_error == Some(true) {
            return Err(ProvisionEntityError::FolderRejected);
        }
    }

    let expected = format!("{slug}.md");
    let files = profile_docs::list_files(client, vault_name, ENTITIES_DIR).await;
    if files.iter().any(|file| file.relative_path == expected) {
        return Ok(());
    }

    let template_source = read_template_source(client, vault_name, config).await;
    let rendered = notes_template::render_new_note(&template_source, at)?;
    let mut frontmatter = rendered.frontmatter;
    frontmatter
        .entry("type")
        .or_insert_with(|| Value::String("entity-profile".to_owned()));
    frontmatter
        .entry("title")
        .or_insert_with(|| Value::String(entity.name.clone()));
    let content = if rendered.body.is_empty() {
        format!("# {}\n", entity.name)
    } else {
        rendered.body
    };

    let full_path = profile_docs::file_path(ENTITIES_DIR, &expected);
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    args.insert("frontmatter".to_owned(), Value::Object(frontmatter));
    args.insert("content".to_owned(), Value::String(content));
    let result = client.call_tool("note_create".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Err(ProvisionEntityError::FileRejected);
    }
    Ok(())
}

#[cfg(test)]
#[path = "profile_entities_test.rs"]
mod tests;
