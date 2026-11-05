use serde::{Deserialize, Serialize};

use crate::VaultPath;

/// The `entity` value for a path that belongs to no configured entity: the vault's own shared areas
/// (`memory/` generally, `registry/`, `templates/`, `standards/`, `scripts/`, and the vault root).
pub const VAULT_SCOPE: &str = "vault";

/// Where a vault keeps its configured entities, relative to the vault root: [`EntitiesFile`]'s one location.
pub const ENTITIES_PATH: &str = ".contextos/entities.toml";

/// `.contextos/entities.toml`'s full contents: the vault's own configured entities, in their own row order.
///
/// The one definition of the file's schema, shared by every crate that reads or writes it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EntitiesFile {
    #[serde(default)]
    pub entities: Vec<EntityRecord>,
}

/// One configured entity, as offered in the Command Centre's entity switcher.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntityRecord {
    pub slug: String,
    pub name: String,
    /// The `--entity-N` accent (1 to 6) this entity carries in the switcher, assigned once at seeding time and
    /// thereafter fixed until an operator edits the file directly.
    pub accent: u8,
}

/// Where a vault path sits relative to the vault's configured entities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntityScope {
    /// Inside one entity's own top-level directory; carries the configured slug.
    Entity(String),
    /// Outside every entity: the vault's shared areas.
    Vault,
}

impl<'a> From<&'a EntityScope> for &'a str {
    fn from(value: &'a EntityScope) -> Self {
        match value {
            EntityScope::Entity(slug) => slug,
            EntityScope::Vault => VAULT_SCOPE,
        }
    }
}

/// The slugs of a vault's configured entities, used to derive the [`EntityScope`] of a path.
///
/// A path belongs to an entity only when its first segment is that entity's slug (`zenith/projects/beacon`).
/// Matching ignores ASCII case, so a case-insensitive filesystem cannot turn an entity directory into a shared one,
/// and the scope reports the configured spelling. Every other path, including one below `memory/` that has an
/// entity's name as a later segment, is [`EntityScope::Vault`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EntityRoster {
    slugs: Vec<String>,
}

impl From<&EntitiesFile> for EntityRoster {
    fn from(value: &EntitiesFile) -> Self {
        Self {
            slugs: value
                .entities
                .iter()
                .map(|entity| entity.slug.trim())
                .filter(|slug| !slug.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }
}

impl EntityRoster {
    /// Derives the scope of a validated vault path.
    #[must_use]
    pub fn scope_of(&self, path: &VaultPath) -> EntityScope {
        let portable = path.portable_relative();
        let segments = portable
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        self.scope_of_segments(&segments)
    }

    pub(crate) fn scope_of_segments(&self, segments: &[&str]) -> EntityScope {
        let Some(first) = segments.first() else {
            return EntityScope::Vault;
        };
        self.slug_matching(first)
            .map_or(EntityScope::Vault, |slug| EntityScope::Entity(slug.to_owned()))
    }

    fn slug_matching(&self, segment: &str) -> Option<&str> {
        self.slugs
            .iter()
            .find(|slug| slug.eq_ignore_ascii_case(segment))
            .map(String::as_str)
    }
}

#[cfg(test)]
#[path = "entity_test.rs"]
mod tests;
