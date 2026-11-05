//! The `entity` a newly created note carries by default, derived from where the note sits in its vault against the
//! vault's own `.contextos/entities.toml`: the configured entity whose top-level directory holds the
//! note, and `vault` for the vault's shared areas. The same rule the managed indexes follow.

use contextos_core::{ENTITIES_PATH, EntitiesFile, EntityRoster, ReadsVault, VaultPath, VaultPathInput, VaultSet};
use contextos_fs::Filesystem;

use crate::tool_error::ToolError;

/// One vault's configured entities, read once for the notes about to be created in it.
pub(crate) struct NoteEntities(Option<EntityRoster>);

impl NoteEntities {
    /// Reads the entity list of the vault that holds `path`. A vault with no `.contextos/entities.toml` has no known
    /// entities, so its notes are given no `entity` rather than a guess. A list that is present but malformed
    /// refuses the note: writing it without the entity its location calls for would quietly degrade it.
    pub(crate) fn read(filesystem: &Filesystem, roots: &VaultSet, path: &VaultPath) -> Result<Self, ToolError> {
        let root = roots
            .root(path.root_id())
            .ok_or(ToolError::Invalid("note vault root is not configured"))?;
        let absolute = root.path().join(ENTITIES_PATH);
        let raw = absolute
            .to_str()
            .ok_or(ToolError::Invalid("vault root path must be valid UTF-8"))?;
        let entities_path = VaultPath::try_from(VaultPathInput { roots, raw })?;
        let Some(text) = filesystem.read_optional_text(&entities_path)? else {
            return Ok(Self(None));
        };
        let file = toml::from_str::<EntitiesFile>(&text.content).map_err(|source| ToolError::EntityList { source })?;
        Ok(Self(Some(EntityRoster::from(&file))))
    }

    /// The `entity` a note created at `path` carries by default, or `None` when the vault's entities are unknown.
    pub(crate) fn entity_of(&self, path: &VaultPath) -> Option<String> {
        self.0
            .as_ref()
            .map(|roster| <&str>::from(&roster.scope_of(path)).to_owned())
    }
}
