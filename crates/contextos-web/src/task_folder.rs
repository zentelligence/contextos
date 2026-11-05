//! The folder a task note is filed in beneath the tasks folder (`memory/tasks/`), as an operator types it on the task
//! create form or the detail page's move form. Like [`VaultRelativePath`] it is a shape check, not the security
//! boundary (`contextos-mcp` confines every path to the vault root), but it keeps a task inside the tasks folder: a
//! traversal, an absolute or Windows form, a hidden folder that Obsidian would never show, or a character that would
//! break a link naming a task by its path (`#`, `|`, `^`, `[`, `]`, or a line break, as the journal's loop entries do)
//! or that Windows refuses in a file name is refused rather than repaired.

use std::collections::BTreeSet;

use crate::vault_links::TASKS_DIR;
use crate::vault_relative_path::{VaultRelativePath, VaultRelativePathError};

/// A folder beneath the tasks folder, or the tasks folder itself (the top level).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TaskFolder(Option<VaultRelativePath>);

/// Why a submitted string is not a [`TaskFolder`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TaskFolderError {
    #[error("{0}")]
    Path(VaultRelativePathError),
    #[error("must not contain a folder name starting with `.`, which Obsidian hides")]
    Hidden,
    #[error("must not contain a control character or any of # | ^ [ ] < > \" ? *")]
    Character,
    #[error("must be text")]
    NotText,
}

impl TaskFolderError {
    /// A stable machine-readable code for this rejection.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Path(reason) => reason.code(),
            Self::Hidden => "path/hidden-segment",
            Self::Character => "path/unsupported-character",
            Self::NotText => "path/not-text",
        }
    }
}

/// The characters a task folder refuses beyond [`VaultRelativePath`]'s own: those that end or redirect a wikilink, and
/// those Windows refuses in a file name.
const UNSUPPORTED_CHARACTERS: [char; 9] = ['#', '|', '^', '[', ']', '<', '>', '"', '?'];

impl TryFrom<&str> for TaskFolder {
    type Error = TaskFolderError;

    /// Validates `raw` as [`VaultRelativePath`] does (surrounding whitespace and one trailing `/` dropped), with an
    /// empty or blank `raw` naming the top level of the tasks folder.
    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        if raw
            .trim()
            .chars()
            .any(|character| character.is_control() || character == '*' || UNSUPPORTED_CHARACTERS.contains(&character))
        {
            return Err(TaskFolderError::Character);
        }
        let path = match VaultRelativePath::try_from(raw) {
            Ok(path) => path,
            Err(VaultRelativePathError::Empty) => return Ok(Self(None)),
            Err(reason) => return Err(TaskFolderError::Path(reason)),
        };
        if path.as_str().split('/').any(|segment| segment.starts_with('.')) {
            return Err(TaskFolderError::Hidden);
        }
        Ok(Self(Some(path)))
    }
}

impl TryFrom<&serde_json::Value> for TaskFolder {
    type Error = TaskFolderError;

    /// A submitted form value: text is validated as [`TaskFolder`]'s `&str` form, and `null` names the top level.
    /// Anything else is refused rather than coerced to text.
    fn try_from(value: &serde_json::Value) -> Result<Self, Self::Error> {
        match value {
            serde_json::Value::String(text) => Self::try_from(text.as_str()),
            serde_json::Value::Null => Ok(Self(None)),
            _ => Err(TaskFolderError::NotText),
        }
    }
}

impl TaskFolder {
    /// The folder relative to the tasks folder, empty for the top level.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_ref().map_or("", VaultRelativePath::as_str)
    }

    /// The vault-relative path of a note named `file_name` filed in this folder.
    #[must_use]
    pub fn note_path(&self, file_name: &str) -> String {
        match &self.0 {
            Some(folder) => format!("{TASKS_DIR}/{folder}/{file_name}"),
            None => format!("{TASKS_DIR}/{file_name}"),
        }
    }
}

/// Where an existing task note is filed: its folder beneath the tasks folder, and its own file name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskNoteLocation {
    pub folder: TaskFolder,
    pub file_name: String,
}

/// Why a vault-relative note path is not a [`TaskNoteLocation`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TaskNoteLocationError {
    #[error("the note is not filed beneath {TASKS_DIR}")]
    OutsideTasks,
    #[error("the note's folder {0}")]
    Folder(TaskFolderError),
    #[error("the note's folder has surrounding spaces or a trailing `/`, which a folder typed here cannot name")]
    NotAsTyped,
}

impl TryFrom<&str> for TaskNoteLocation {
    type Error = TaskNoteLocationError;

    /// Reads a task note's vault-relative path, such as `memory/tasks/clients/a-001.md`.
    fn try_from(note_path: &str) -> Result<Self, Self::Error> {
        let within = note_path
            .strip_prefix(TASKS_DIR)
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|rest| !rest.is_empty())
            .ok_or(TaskNoteLocationError::OutsideTasks)?;
        let (folder, file_name) = within.rsplit_once('/').unwrap_or(("", within));
        if file_name.is_empty() {
            return Err(TaskNoteLocationError::OutsideTasks);
        }
        let parsed = TaskFolder::try_from(folder).map_err(TaskNoteLocationError::Folder)?;
        // Typed input is trimmed, but an existing folder is read exactly: ` inbox` is not `inbox`.
        if parsed.as_str() != folder {
            return Err(TaskNoteLocationError::NotAsTyped);
        }
        Ok(Self {
            folder: parsed,
            file_name: file_name.to_owned(),
        })
    }
}

impl TaskNoteLocation {
    /// The vault-relative path this note would have filed in `folder` under its own file name.
    #[must_use]
    pub fn moved_to(&self, folder: &TaskFolder) -> String {
        folder.note_path(&self.file_name)
    }
}

/// Every folder beneath the tasks folder that holds one of `note_paths`, with each of its ancestors, sorted: the
/// folders a create or move form suggests. The top level is not listed, since an empty folder already names it.
#[must_use]
pub fn known_task_folders<'a>(note_paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut folders = BTreeSet::new();
    for path in note_paths {
        let Ok(location) = TaskNoteLocation::try_from(path) else {
            continue;
        };
        let mut folder = location.folder.as_str();
        while !folder.is_empty() {
            folders.insert(folder.to_owned());
            folder = folder.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
    }
    folders.into_iter().collect()
}

#[cfg(test)]
#[path = "task_folder_test.rs"]
mod tests;
