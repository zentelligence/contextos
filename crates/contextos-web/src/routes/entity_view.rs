//! Pure view-model construction for the Entity folders screen's own
//! sidebar: a collapsible tree matching `routes::wiki_view`'s identical
//! accordion shape, adapted to Entity folders' own already-real
//! `EntityFolder` tree (`entity_docs::load_tree`'s own "whole tree, one
//! call" contract). The actual pre-order walk lives in [`crate::tree_view`],
//! shared with `routes::wiki_view`: this module supplies only what is
//! genuinely Entity folders' own - [`EntityFolder`]'s [`TreeFolder`]
//! implementation, and how a real [`EntityFileLink`] becomes a rendered
//! [`TreeFile`] (its own href, active state, and whether it is markdown, so
//! the template can mark a non-markdown file's row distinctly).

use crate::entity_docs::{EntityFileLink, EntityFolder};
use crate::tree_view::{self, TreeFolder};

impl TreeFolder for EntityFolder {
    type Leaf = EntityFileLink;

    fn relative_path(&self) -> &str {
        &self.relative_path
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn subfolders(&self) -> &[Self] {
        &self.folders
    }

    fn leaves(&self) -> &[Self::Leaf] {
        &self.files
    }
}

/// One real file, ready to render as a leaf in the tree: a markdown file
/// links to this screen's own read view, a non-markdown file to the generic
/// vault content route instead (`routes::entity_docs`'s own existing
/// redirect contract).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeFile {
    pub label: String,
    pub href: String,
    pub active: bool,
    pub is_markdown: bool,
}

pub type TreeEvent = tree_view::TreeEvent<TreeFile>;

/// Pre-order-flattens `root`'s own real folder/file tree into [`TreeEvent`]s
/// ([`tree_view::flatten`]), opening every folder that sits on `anchor_dir`'s
/// own path and marking exactly the file at `selected_path`, if any, active
/// - matching `wiki_view::flatten`'s identical contract.
#[must_use]
pub fn flatten(
    vault_name: &str,
    entity_slug: &str,
    root: &EntityFolder,
    anchor_dir: &str,
    selected_path: Option<&str>,
) -> Vec<TreeEvent> {
    tree_view::flatten(root, anchor_dir, &mut |file: &EntityFileLink| TreeFile {
        label: file.label.clone(),
        // A note or another page opens on this screen; any other file is served from its own vault address.
        href: if file.is_markdown || crate::file_view::PageFile::try_from(file.relative_path.as_str()).is_ok() {
            file_href(vault_name, entity_slug, &file.relative_path)
        } else {
            general_route_href(vault_name, entity_slug, &file.relative_path)
        },
        active: selected_path == Some(file.relative_path.as_str()),
        is_markdown: file.is_markdown,
    })
}

fn file_href(vault_name: &str, entity_slug: &str, relative_path: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).entity_file(entity_slug, relative_path)
}

/// The direct, general vault content route for a non-markdown file: matches
/// `routes::entity_docs::general_route_href`'s identical target, so a
/// non-markdown leaf in the tree opens exactly where its own sidebar row
/// already sends it today.
fn general_route_href(vault_name: &str, entity_slug: &str, relative_path: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).file(&format!("{entity_slug}/{relative_path}"))
}

#[cfg(test)]
#[path = "entity_view_test.rs"]
mod tests;
