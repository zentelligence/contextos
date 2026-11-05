//! Pure view-model construction for the Wiki screen's own sidebar: a
//! collapsible tree mirroring `notes_view`'s own accordion shape
//! (`notes_view.rs`'s own module doc), adapted to Wiki's own already-real
//! `WikiFolder` tree (`wiki::load_tree`'s own "whole tree, one call"
//! contract) rather than built up from a flat list of summaries the way
//! Notes' synthetic tree is: every `WikiFolder` node already carries its own
//! real `relative_path`, so this module only ever walks it, never
//! reconstructs it.
//!
//! The actual pre-order walk lives in [`crate::tree_view`], shared with
//! `routes::entity_view`'s identical accordion shape over `EntityFolder`
//! (this codebase's own established "two real call sites justify
//! extraction" precedent, `slug.rs`'s own history). This module supplies
//! only what is genuinely Wiki's own: [`WikiFolder`]'s [`TreeFolder`]
//! implementation, and how a real [`WikiPageLink`] becomes a rendered
//! [`TreePage`] (its own href and active state).

use crate::tree_view::{self, TreeFolder};
use crate::wiki::{WikiFolder, WikiPageLink};

impl TreeFolder for WikiFolder {
    type Leaf = WikiPageLink;

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
        &self.pages
    }
}

/// One real page, ready to render as a leaf in the tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreePage {
    pub label: String,
    pub href: String,
    pub active: bool,
}

pub type TreeEvent = tree_view::TreeEvent<TreePage>;

/// Pre-order-flattens `root`'s own real folder/page tree into [`TreeEvent`]s
/// ([`tree_view::flatten`]), opening every folder that sits on `anchor_dir`'s
/// own path (itself, or a real ancestor of it - the selected page's own
/// containing folder, or the currently browsed folder itself when nothing is
/// selected; `routes::wiki::render_wiki_fragment`'s own resolution) and
/// marking exactly the page at `selected_path`, if any, active.
#[must_use]
pub fn flatten(vault_name: &str, root: &WikiFolder, anchor_dir: &str, selected_path: Option<&str>) -> Vec<TreeEvent> {
    tree_view::flatten(root, anchor_dir, &mut |page: &WikiPageLink| TreePage {
        label: page.label.clone(),
        // Every file opens at its address on this screen, which shows a page and answers any other file as itself.
        href: page_href(vault_name, &page.relative_path),
        active: selected_path == Some(page.relative_path.as_str()),
    })
}

fn page_href(vault_name: &str, relative_path: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).wiki(relative_path)
}

#[cfg(test)]
#[path = "wiki_view_test.rs"]
mod tests;
