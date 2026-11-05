//! Shared pre-order tree-flattening for a real on-disk folder tree with
//! folder and leaf nodes: the one real mechanical shape `routes::wiki_view`
//! and `routes::entity_view` both need (a label, a relative path, child
//! folders, and leaf files/pages), extracted here once Entity folders became
//! a genuine second real call site for it - this codebase's own established
//! "two real call sites justify extraction" precedent (`slug.rs`'s own
//! history: extracted only once Insights needed the slug logic Wiki already
//! had).
//!
//! What a leaf needs in order to render (a page's own href and active state,
//! a file's own href/active/`is_markdown`) differs per screen and is never
//! this module's concern: [`flatten`] takes a `leaf_view` closure that
//! converts each real leaf into whatever the caller's own template needs,
//! keeping this module itself free of any screen-specific vocabulary.

/// One real folder node in a tree [`flatten`] can walk: implemented by
/// `wiki::WikiFolder` (leaf: `wiki::WikiPageLink`) and
/// `entity_docs::EntityFolder` (leaf: `entity_docs::EntityFileLink`), whose
/// shapes are otherwise structurally identical but named differently for
/// each screen's own vocabulary.
pub trait TreeFolder: Sized {
    type Leaf;

    /// This folder's own path relative to its tree's root, empty for the
    /// root itself.
    fn relative_path(&self) -> &str;
    fn label(&self) -> &str;
    fn subfolders(&self) -> &[Self];
    fn leaves(&self) -> &[Self::Leaf];
}

/// One step of a pre-order walk over a real folder tree, matching
/// `notes_view::TreeEvent`'s identical flat-sequence shape and the same
/// reason it exists: Askama forbids a macro calling itself, so a tree of
/// unknown depth can only be rendered from a flat, well-nested event
/// sequence, never native template recursion. Nesting in the rendered HTML
/// comes entirely from `OpenFolder`/`CloseFolder` pairing, which [`flatten`]
/// guarantees by construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeEvent<Leaf> {
    OpenFolder(String, bool),
    CloseFolder,
    Leaf(Leaf),
}

/// Pre-order-flattens `root`'s own real folder/leaf tree into [`TreeEvent`]s,
/// opening every folder that sits on `anchor_dir`'s own path (itself, or a
/// real ancestor of it) and closing every other folder, matching
/// `notes_view::build_tree`'s identical "reveal the current location"
/// contract. The root folder's own children render directly, with no
/// `OpenFolder`/`CloseFolder` pair of their own - the root's own label is
/// never emitted. Each real leaf is converted to its own rendered form by
/// `leaf_view`.
#[must_use]
pub fn flatten<F, V>(root: &F, anchor_dir: &str, leaf_view: &mut impl FnMut(&F::Leaf) -> V) -> Vec<TreeEvent<V>>
where
    F: TreeFolder,
{
    let mut events = Vec::new();
    flatten_into(root, anchor_dir, leaf_view, &mut events);
    events
}

fn flatten_into<F, V>(
    folder: &F,
    anchor_dir: &str,
    leaf_view: &mut impl FnMut(&F::Leaf) -> V,
    events: &mut Vec<TreeEvent<V>>,
) where
    F: TreeFolder,
{
    for leaf in folder.leaves() {
        events.push(TreeEvent::Leaf(leaf_view(leaf)));
    }
    for child in folder.subfolders() {
        let is_open = is_on_anchor_path(child.relative_path(), anchor_dir);
        events.push(TreeEvent::OpenFolder(child.label().to_owned(), is_open));
        flatten_into(child, anchor_dir, leaf_view, events);
        events.push(TreeEvent::CloseFolder);
    }
}

/// Whether `folder_relative_path` sits on `anchor_dir`'s own path: itself,
/// or a real ancestor of it. Every folder node already carries its own full
/// `relative_path`, so a folder not on the anchor's path can never have a
/// descendant that is either - no separate "still on the path so far" flag
/// needs threading down the recursion the way a synthetic, segment-by-
/// segment tree (`notes_view::build_tree`) requires.
fn is_on_anchor_path(folder_relative_path: &str, anchor_dir: &str) -> bool {
    anchor_dir == folder_relative_path || anchor_dir.starts_with(&format!("{folder_relative_path}/"))
}

#[cfg(test)]
#[path = "tree_view_test.rs"]
mod tests;
