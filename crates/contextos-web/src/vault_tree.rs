//! A real vault folder tree read through `fs_directory_tree`, typed for the screens that browse a configured folder
//! as a tree of files: the Vault Settings management categories. Entries a person never authors (dot-prefixed
//! entries, Python bytecode caches, and each folder's managed `index.md`, which the interface never shows) are left
//! out, folders and files each sort by name, and every node carries its path relative to the listed root so a screen
//! can address it in a URL and look it up again before acting on it.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClient};
use crate::tree_view::TreeFolder;
use crate::vault_relative_path::VaultRelativePath;

/// `fs_directory_tree`'s node shape.
#[derive(Debug, Deserialize)]
pub(crate) struct DirectoryTreeNode {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    children: Vec<DirectoryTreeNode>,
}

/// One file in a listed tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultFile {
    pub name: String,
    /// Relative to the listed root.
    pub relative_path: String,
}

impl VaultFile {
    /// The name without its final extension.
    #[must_use]
    pub fn stem(&self) -> &str {
        self.name
            .rsplit_once('.')
            .filter(|(stem, _)| !stem.is_empty())
            .map_or(self.name.as_str(), |(stem, _)| stem)
    }

    /// The final extension, lower-cased, if the name has one.
    #[must_use]
    pub fn extension(&self) -> Option<String> {
        self.name
            .rsplit_once('.')
            .filter(|(stem, _)| !stem.is_empty())
            .map(|(_, extension)| extension.to_ascii_lowercase())
    }

    #[must_use]
    pub fn is_markdown(&self) -> bool {
        self.extension().as_deref() == Some("md")
    }

    /// A folder's generated `index.md`, which [`list_folder`] never lists; kept for checks on a file found elsewhere.
    #[must_use]
    pub fn is_index(&self) -> bool {
        self.name.eq_ignore_ascii_case("index.md")
    }
}

/// One folder in a listed tree; the listed root has an empty `relative_path`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultFolder {
    pub name: String,
    pub relative_path: String,
    pub folders: Vec<VaultFolder>,
    pub files: Vec<VaultFile>,
}

fn is_listed(name: &str) -> bool {
    !(name.starts_with('.') || name == "__pycache__" || name.to_ascii_lowercase().ends_with(".pyc"))
}

fn child_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

fn build(node: &DirectoryTreeNode, relative_path: String) -> VaultFolder {
    let mut folder = VaultFolder {
        name: node.name.clone(),
        relative_path,
        folders: Vec::new(),
        files: Vec::new(),
    };
    for child in node.children.iter().filter(|child| is_listed(&child.name)) {
        let path = child_path(&folder.relative_path, &child.name);
        if child.kind == "dir" {
            folder.folders.push(build(child, path));
        } else if !child.name.eq_ignore_ascii_case("index.md") {
            folder.files.push(VaultFile {
                name: child.name.clone(),
                relative_path: path,
            });
        }
    }
    folder
        .folders
        .sort_by_cached_key(|child| child.name.to_ascii_lowercase());
    folder.files.sort_by_cached_key(|file| file.name.to_ascii_lowercase());
    folder
}

impl From<&DirectoryTreeNode> for VaultFolder {
    fn from(node: &DirectoryTreeNode) -> Self {
        build(node, String::new())
    }
}

impl VaultFolder {
    /// The folder at `relative_path` (`""` for this folder), walking real folder names only.
    #[must_use]
    pub fn find_folder(&self, relative_path: &str) -> Option<&Self> {
        let mut folder = self;
        for segment in relative_path.split('/').filter(|segment| !segment.is_empty()) {
            folder = folder.folders.iter().find(|child| child.name == segment)?;
        }
        Some(folder)
    }

    /// The file at `relative_path`, never a folder.
    #[must_use]
    pub fn find_file(&self, relative_path: &str) -> Option<&VaultFile> {
        let (parent, name) = relative_path.rsplit_once('/').unwrap_or(("", relative_path));
        self.find_folder(parent)?.files.iter().find(|file| file.name == name)
    }

    /// How many files anywhere beneath this folder satisfy `counted`.
    #[must_use]
    pub fn count_files(&self, counted: &dyn Fn(&VaultFile) -> bool) -> usize {
        self.files_depth_first().filter(|file| counted(file)).count()
    }

    /// Every file beneath this folder: a folder's own files first, then each subfolder's, in display order.
    pub fn files_depth_first(&self) -> Box<dyn Iterator<Item = &VaultFile> + '_> {
        Box::new(
            self.files
                .iter()
                .chain(self.folders.iter().flat_map(Self::files_depth_first)),
        )
    }
}

impl TreeFolder for VaultFolder {
    type Leaf = VaultFile;

    fn relative_path(&self) -> &str {
        &self.relative_path
    }

    fn label(&self) -> &str {
        &self.name
    }

    fn subfolders(&self) -> &[Self] {
        &self.folders
    }

    fn leaves(&self) -> &[Self::Leaf] {
        &self.files
    }
}

/// What listing a configured folder found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FolderListing {
    /// The folder exists; it may be empty.
    Present(VaultFolder),
    /// Nothing exists at the configured location: the vault has not provisioned it.
    Missing,
    /// The server refused the listing for another reason, carrying its stable code when it gave one.
    Unreadable { code: Option<String> },
}

impl FolderListing {
    #[must_use]
    pub fn folder(&self) -> Option<&VaultFolder> {
        match self {
            Self::Present(folder) => Some(folder),
            Self::Missing | Self::Unreadable { .. } => None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RefusalCode {
    #[serde(default)]
    code: Option<String>,
}

/// Lists `root` in `vault_name`, up to the server's own depth bound.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the transport itself fails.
pub async fn list_folder(
    client: &McpClient,
    vault_name: &str,
    root: &VaultRelativePath,
) -> Result<FolderListing, McpCallError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{root}")));
    let result = client.call_tool("fs_directory_tree".to_owned(), args).await?;
    if result.is_error == Some(true) {
        let code = result.into_typed::<RefusalCode>().ok().and_then(|refusal| refusal.code);
        return Ok(match code.as_deref() {
            Some("path/not-found") => FolderListing::Missing,
            _ => FolderListing::Unreadable { code },
        });
    }
    Ok(match result.into_typed::<DirectoryTreeNode>() {
        Ok(node) if node.kind == "dir" => FolderListing::Present(VaultFolder::from(&node)),
        Ok(_) => FolderListing::Unreadable {
            code: Some("path/not-a-directory".to_owned()),
        },
        Err(_) => FolderListing::Unreadable { code: None },
    })
}

#[cfg(test)]
#[path = "vault_tree_test.rs"]
mod tests;
