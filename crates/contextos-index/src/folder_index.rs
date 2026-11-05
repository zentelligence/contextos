//! Which notes are a folder's own index rather than content of the folder.

/// The names of the notes that index a folder: the index the service maintains, and the older underscored form it still
/// recognises. Neither is content of its folder, so a reader that treats each note in a folder as one item of a
/// collection, such as a registry of definitions, leaves both out.
pub const FOLDER_INDEX_NAMES: [&str; 2] = ["index.md", "_index.md"];

/// Whether a directory entry named `name` is its folder's index note.
#[must_use]
pub fn is_folder_index(name: &str) -> bool {
    FOLDER_INDEX_NAMES.contains(&name)
}

#[cfg(test)]
#[path = "folder_index_test.rs"]
mod tests;
