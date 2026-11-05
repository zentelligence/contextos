//! Pure view-model construction for the Notes screen's own sidebar: a
//! collapsible tree mirroring `notes_dir`'s real on-disk folder structure,
//! at whatever depth and under whatever names it actually has.
//!
//! The `{{YYYY}}/{{MM}}/{{YYYY-MM-DD-HHmm}}.md` layout `notes::default_relative_path`
//! produces is only ever the *default* for a brand-new note: the Notes
//! screen's own vault settings can change `notes_dir` itself, and nothing
//! stops an operator from filing existing notes into their own folders by
//! hand (a topic, a project, anything). This tree is therefore a genuine
//! n-ary tree over each note's own path segments - never a fixed
//! year/month shape - so it reflects whatever is really there rather than
//! forcing every note through an assumed convention (an earlier version of
//! this module did exactly that, and silently mis-grouped anything that
//! did not fit).

use crate::date_structure;
use crate::notes::NoteSummary;
use crate::notes_config::NotesConfig;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeNote {
    pub title: String,
    /// [`NoteSummary::has_custom_title`]'s own doc: whether the sidebar
    /// row's own title is real, or only the filename fallback.
    pub has_custom_title: bool,
    pub date: String,
    pub time: String,
    pub href: String,
    pub active: bool,
}

/// One real folder under `notes_dir`, however deep: `label` is that
/// folder's own literal name (never reinterpreted as a year or month -
/// see the module doc for why), `folders` its own subfolders in
/// first-seen order (which, since [`build_tree`]'s caller already sorts
/// most-recent-first, already means "the folder holding the most
/// recently dated note sorts first" with no second pass), and `notes`
/// whatever real notes sit directly in it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeFolder {
    pub label: String,
    /// Whether this folder starts expanded: only every folder on the
    /// anchor note's own path (the selected note, or the most recent one
    /// when nothing is selected) - the root folder is always open.
    /// Matches a file explorer's own "reveal the current file" behaviour,
    /// generalised to arbitrary depth rather than Journal's fixed
    /// year/month levels.
    pub is_open: bool,
    pub folders: Vec<TreeFolder>,
    pub notes: Vec<TreeNote>,
}

/// One step of [`flatten`]'s own pre-order walk: entering a folder
/// (`label`, `is_open`), leaving the most recently entered still-open
/// folder, or one note leaf - a flat sequence a template can render with a
/// single loop, since Askama forbids a macro calling itself (there is no
/// other native way to render a tree of genuinely unknown depth). Nesting
/// in the rendered HTML comes entirely from `OpenFolder`/`CloseFolder`
/// pairing, never from template block nesting: the final HTML is
/// well-formed as long as every `OpenFolder` this walk emits is matched by
/// exactly one later `CloseFolder`, which [`flatten`] guarantees by
/// construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeEvent {
    OpenFolder(String, bool),
    CloseFolder,
    Note(TreeNote),
}

/// Pre-order-flattens `folder`'s own recursive structure ([`build_tree`])
/// into the flat sequence [`TreeEvent`] documents. The root folder's own
/// (always empty) `label` is never emitted - only its real contents.
#[must_use]
pub fn flatten(folder: &TreeFolder) -> Vec<TreeEvent> {
    let mut events = Vec::new();
    flatten_into(folder, &mut events);
    events
}

fn flatten_into(folder: &TreeFolder, events: &mut Vec<TreeEvent>) {
    for note in &folder.notes {
        events.push(TreeEvent::Note(note.clone()));
    }
    for child in &folder.folders {
        events.push(TreeEvent::OpenFolder(child.label.clone(), child.is_open));
        flatten_into(child, events);
        events.push(TreeEvent::CloseFolder);
    }
}

/// The sidebar's own real folder/note tree, built entirely from every
/// note's own `relative_path` ([`NoteSummary::relative_path`], already
/// most-recent-first per `notes::list_notes`'s own contract) - never from
/// an assumed partition shape. A note sitting directly under `notes_dir`
/// (no folder at all) is a leaf of the returned root itself, not a
/// special case: every real note has a well-defined place in this tree
/// regardless of how deeply, or how, it is nested.
#[must_use]
pub fn build_tree(vault_name: &str, summaries: &[NoteSummary], selected: Option<&str>) -> TreeFolder {
    let anchor = selected.or_else(|| summaries.first().map(|summary| summary.relative_path.as_str()));
    let anchor_segments: Vec<&str> = anchor.map(|path| path.split('/').collect()).unwrap_or_default();

    let mut root = TreeFolder {
        label: String::new(),
        is_open: true,
        folders: Vec::new(),
        notes: Vec::new(),
    };
    for summary in summaries {
        let segments: Vec<&str> = summary.relative_path.split('/').collect();
        let folder_segments = &segments[..segments.len().saturating_sub(1)];
        let mut node = &mut root;
        let mut on_anchor_path = true;
        for (depth, folder_name) in folder_segments.iter().enumerate() {
            on_anchor_path = on_anchor_path && anchor_segments.get(depth) == Some(folder_name);
            let index = if let Some(index) = node.folders.iter().position(|folder| folder.label == *folder_name) {
                index
            } else {
                node.folders.push(TreeFolder {
                    label: (*folder_name).to_owned(),
                    is_open: on_anchor_path,
                    folders: Vec::new(),
                    notes: Vec::new(),
                });
                node.folders.len() - 1
            };
            node = &mut node.folders[index];
        }
        node.notes.push(TreeNote {
            title: summary.title.clone(),
            has_custom_title: summary.has_custom_title,
            date: summary.date.clone().unwrap_or_default(),
            time: summary.time.clone().unwrap_or_default(),
            href: crate::urls::VaultUrls::from(vault_name).notes(&summary.relative_path),
            active: selected == Some(summary.relative_path.as_str()),
        });
    }
    root
}

/// The sidebar's own two panes when `NotesConfig::date_structure_active` is
/// on: `dated` (every note whose own path matches `date_format`, most
/// recent first by its own real calendar value - not insertion order) and
/// `regular` (everything else, alphabetically ascending by relative path -
/// a plain, predictable order for content this tree can say nothing about
/// the recency of). `dated` is `None` when the split is off entirely; the
/// single tree that case still needs sits in `regular` instead, unchanged
/// from before this split existed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplitView {
    pub dated: Option<TreeFolder>,
    pub regular: TreeFolder,
}

/// Builds the sidebar's own split (or, with the split off, single) view.
/// See [`SplitView`]'s own doc for what each side means and how it is
/// ordered.
#[must_use]
pub fn build_split_view(
    vault_name: &str,
    summaries: &[NoteSummary],
    selected: Option<&str>,
    config: &NotesConfig,
) -> SplitView {
    if !config.date_structure_active {
        return SplitView {
            dated: None,
            regular: build_tree(vault_name, summaries, selected),
        };
    }
    let mut dated: Vec<(Vec<i64>, &NoteSummary)> = Vec::new();
    let mut regular: Vec<&NoteSummary> = Vec::new();
    for summary in summaries {
        match date_structure::match_date_format(&config.date_format, &summary.relative_path) {
            Some(key) => dated.push((key, summary)),
            None => regular.push(summary),
        }
    }
    // Most calendar-recent first: `build_tree`'s own "first-seen order"
    // preserves whatever order it is handed, so pre-sorting here is the
    // whole mechanism - no further sorting happens inside the tree itself.
    dated.sort_by(|(a, _), (b, _)| b.cmp(a));
    let dated_summaries: Vec<NoteSummary> = dated.into_iter().map(|(_, summary)| summary.clone()).collect();
    regular.sort_by_key(|summary| summary.relative_path.clone());
    let regular_summaries: Vec<NoteSummary> = regular.into_iter().cloned().collect();

    SplitView {
        dated: Some(build_tree(vault_name, &dated_summaries, selected)),
        regular: build_tree(vault_name, &regular_summaries, selected),
    }
}

/// The most calendar-recent note under `config`'s own date hierarchy - real
/// chronological order parsed from each note's own path
/// ([`date_structure::match_date_format`]), the same key [`build_split_view`]
/// sorts its own `dated` pane by, never a frontmatter field (which a
/// hand-authored or differently templated note may leave absent or
/// inconsistent with where it actually sits). `None` when the date
/// structure is off, or no note's own path actually matches
/// `config.date_format` - the caller's own signal to fall back to whatever
/// order it already has.
///
/// Two notes sharing a folder but whose own filename does not fully spell
/// out the configured leaf shape ([`date_structure::matched_fields`]'s own
/// "the leaf is matched best-effort" contract) can produce the exact same
/// chronological key - a hand-renamed note carries no finer-grained real
/// date to break the tie with. `summaries` is walked in its own given order
/// (`notes::list_notes`'s frontmatter-recency order, for every caller this
/// module actually has) and the first note to reach a given key wins ties
/// rather than the last, so that existing order still decides a tie this
/// path-only key genuinely cannot.
#[must_use]
pub fn most_recent_dated_note<'a>(summaries: &'a [NoteSummary], config: &NotesConfig) -> Option<&'a NoteSummary> {
    if !config.date_structure_active {
        return None;
    }
    let mut best: Option<(Vec<i64>, &NoteSummary)> = None;
    for summary in summaries {
        let Some(key) = date_structure::match_date_format(&config.date_format, &summary.relative_path) else {
            continue;
        };
        if best.as_ref().is_none_or(|(best_key, _)| key > *best_key) {
            best = Some((key, summary));
        }
    }
    best.map(|(_, summary)| summary)
}

#[cfg(test)]
#[path = "notes_view_test.rs"]
mod tests;
