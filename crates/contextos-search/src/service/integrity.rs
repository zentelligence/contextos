//! Link integrity: the links that resolve to nothing or to several files, and the notes no other note links to. Both
//! are plain reads of the link graph, which is brought up to date with the vault first, so they answer for the vault as
//! it is now and not as it was when the graph was last built.

use std::collections::BTreeSet;

use contextos_core::VaultPath;
use contextos_obsidian::LinkForm;

use crate::{GraphEdge, GraphNode, GraphNodeKind, SearchError};

use super::VaultSearchService;
use super::scan::is_excluded;

/// Why a link is reported as unresolved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnresolvedKind {
    /// The link names no file.
    NotFound,
    /// The link names a file that more than one file answers to.
    Ambiguous,
    /// The note's links could not be read, so what it links to is not known.
    Unreadable,
}

/// One link that does not resolve, or one note whose links cannot be read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnresolvedLink {
    /// The note holding the link, as a forward-slash relative path.
    pub source: String,
    /// The 1-based line of the link; `None` for an unreadable note.
    pub line: Option<usize>,
    /// The link exactly as written; for an unreadable note, the reason its links cannot be read.
    pub text: String,
    /// The target as written, without heading or block reference, angle brackets, or URL encoding; `None` for an
    /// unreadable note.
    pub target: Option<String>,
    /// How the link was written; `None` for an unreadable note.
    pub form: Option<LinkForm>,
    /// Why the link is reported.
    pub kind: UnresolvedKind,
    /// For an ambiguous link, every file its target answers to, sorted; empty otherwise.
    pub candidates: Vec<String>,
}

/// What the link graph says of the vault's links: how many were checked and which do not resolve.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LinkIntegrity {
    /// How many links were checked: every link in a checked note except template placeholders, resolved or not.
    pub checked: usize,
    /// How many links were left unchecked because their target lies under a folder whose files are not checked as
    /// link targets; they are neither counted in `checked` nor listed.
    pub ignored: usize,
    /// The links that do not resolve and the notes that cannot be read, sorted by source then line.
    pub unresolved: Vec<UnresolvedLink>,
}

/// A note no other note links to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnlinkedNote {
    /// The note's forward-slash relative path.
    pub path: String,
    /// The note's derived title.
    pub title: String,
}

/// One link to a file, as the link graph holds it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncomingLink {
    /// The note holding the link, as a forward-slash relative path.
    pub source: String,
    /// The 1-based line the link is written on.
    pub line: usize,
    /// How the link was written.
    pub form: LinkForm,
}

impl VaultSearchService {
    /// Every link that reaches `target`, one entry for each, sorted by source, line, form, and text. The graph is
    /// brought up to date with the vault first, and a link from a link-excluded folder still counts, as it does for
    /// [`VaultSearchService::graph_backlinks`]. A file the graph does not hold has no incoming links.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::GraphDisabled`] when the link graph is disabled for this vault, and a storage error when
    /// the graph cannot be read.
    pub fn incoming_links(&self, target: &VaultPath) -> Result<Vec<IncomingLink>, SearchError> {
        let target = target.portable_relative();
        self.reconcile_before_query();
        let view = self.with_graph(crate::LinkGraph::full_view)?;
        if !view
            .nodes
            .iter()
            .any(|node| node.path == target && matches!(node.kind, GraphNodeKind::Note | GraphNodeKind::File))
        {
            return Ok(Vec::new());
        }
        let mut edges: Vec<_> = view.edges.iter().filter(|edge| edge.to == target).collect();
        edges.sort_by(|left, right| {
            (&left.from, left.line, left.form.label(), &left.raw).cmp(&(
                &right.from,
                right.line,
                right.form.label(),
                &right.raw,
            ))
        });
        Ok(edges
            .into_iter()
            .map(|edge| IncomingLink {
                source: edge.from.clone(),
                line: edge.line,
                form: edge.form,
            })
            .collect())
    }

    /// Whether the note at `path` is a link source the reports check: in the folder `scope` when one is given, and in
    /// neither a link-excluded nor a search-excluded folder.
    fn is_checked_source(&self, path: &str, scope: Option<&str>) -> bool {
        scope.is_none_or(|folder| is_excluded(&[folder.trim_end_matches('/').to_owned()], path))
            && !is_excluded(&self.link_excludes, path)
            && !is_excluded(&self.excludes, path)
    }

    /// Whether the link `edge`, reaching a node of `kind`, has a target under a folder whose files are not checked as link
    /// targets: the file it resolves to, or, when it resolves to no file, the path it names. An ambiguous link is never
    /// ignored, since which file it reaches is the question its report asks.
    fn is_ignored_target(&self, edge: &GraphEdge, kind: GraphNodeKind) -> bool {
        if self.link_ignore_targets.is_empty() {
            return false;
        }
        match kind {
            GraphNodeKind::Note | GraphNodeKind::File => is_excluded(&self.link_ignore_targets, &edge.to),
            GraphNodeKind::NotFound => {
                named_path(edge).is_some_and(|path| is_excluded(&self.link_ignore_targets, &path))
            }
            GraphNodeKind::Ambiguous => false,
        }
    }

    /// Every link that does not resolve, sorted by source then line, from the notes in the folder `scope` (the whole
    /// vault when `None` or the vault root; the caller has checked that it is a folder of this vault) outside the link-excluded folders. A note whose links cannot be read is listed once.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::GraphDisabled`] when the link graph is disabled for this vault, and a storage error when
    /// the graph cannot be read.
    pub fn unresolved_links(&self, scope: Option<&VaultPath>) -> Result<Vec<UnresolvedLink>, SearchError> {
        Ok(self.link_integrity_in(scope)?.unresolved)
    }

    /// The link graph's verdict on every link in the vault: how many were checked and which do not resolve.
    ///
    /// # Errors
    ///
    /// As [`VaultSearchService::unresolved_links`].
    pub fn link_integrity(&self) -> Result<LinkIntegrity, SearchError> {
        self.link_integrity_in(None)
    }

    fn link_integrity_in(&self, scope: Option<&VaultPath>) -> Result<LinkIntegrity, SearchError> {
        let folder = scope
            .map(VaultPath::portable_relative)
            .filter(|folder| !folder.is_empty());
        let scope = folder.as_deref();
        self.reconcile_before_query();
        let view = self.with_graph(crate::LinkGraph::full_view)?;
        let nodes: std::collections::BTreeMap<&str, &GraphNode> =
            view.nodes.iter().map(|node| (node.path.as_str(), node)).collect();
        let mut report = LinkIntegrity::default();
        for node in &view.nodes {
            if node.kind != GraphNodeKind::Note || !self.is_checked_source(&node.path, scope) {
                continue;
            }
            if let Some(reason) = &node.unreadable {
                report.unresolved.push(UnresolvedLink {
                    source: node.path.clone(),
                    line: None,
                    text: reason.clone(),
                    target: None,
                    form: None,
                    kind: UnresolvedKind::Unreadable,
                    candidates: Vec::new(),
                });
            }
        }
        for edge in &view.edges {
            if edge.is_placeholder() {
                continue;
            }
            let Some(source) = nodes.get(edge.from.as_str()) else {
                continue;
            };
            if source.kind != GraphNodeKind::Note || !self.is_checked_source(&edge.from, scope) {
                continue;
            }
            let Some(reached) = nodes.get(edge.to.as_str()) else {
                report.checked = report.checked.saturating_add(1);
                continue;
            };
            if self.is_ignored_target(edge, reached.kind) {
                report.ignored = report.ignored.saturating_add(1);
                continue;
            }
            report.checked = report.checked.saturating_add(1);
            let kind = match reached.kind {
                GraphNodeKind::NotFound => UnresolvedKind::NotFound,
                GraphNodeKind::Ambiguous => UnresolvedKind::Ambiguous,
                GraphNodeKind::Note | GraphNodeKind::File => continue,
            };
            report.unresolved.push(UnresolvedLink {
                source: edge.from.clone(),
                line: Some(edge.line),
                text: edge.raw.clone(),
                target: Some(edge.target.clone()),
                form: Some(edge.form),
                kind,
                candidates: reached.candidates.clone(),
            });
        }
        report.unresolved.sort_by(|left, right| {
            (&left.source, left.line, &left.text).cmp(&(&right.source, right.line, &right.text))
        });
        Ok(report)
    }

    /// Every note no other note links to, sorted by path, from the folder `scope` (the whole vault when `None`).
    /// `index.md` files and notes in link-excluded folders are never reported, and a link from a link-excluded folder,
    /// or from the note to itself, does not count.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::GraphDisabled`] when the link graph is disabled for this vault, and a storage error when
    /// the graph cannot be read.
    pub fn unlinked_notes(&self, scope: Option<&VaultPath>) -> Result<Vec<UnlinkedNote>, SearchError> {
        let folder = scope
            .map(VaultPath::portable_relative)
            .filter(|folder| !folder.is_empty());
        let scope = folder.as_deref();
        self.reconcile_before_query();
        let view = self.with_graph(crate::LinkGraph::full_view)?;
        let linked: BTreeSet<&str> = view
            .edges
            .iter()
            .filter(|edge| edge.from != edge.to && self.is_checked_source(&edge.from, None))
            .map(|edge| edge.to.as_str())
            .collect();
        let mut notes: Vec<UnlinkedNote> = view
            .nodes
            .iter()
            .filter(|node| {
                node.kind == GraphNodeKind::Note
                    && !is_index_note(&node.path)
                    && self.is_checked_source(&node.path, scope)
                    && !linked.contains(node.path.as_str())
            })
            .map(|node| UnlinkedNote {
                path: node.path.clone(),
                title: node.title.clone(),
            })
            .collect();
        notes.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(notes)
    }
}

/// The vault path a link names, read from the vault root, or, for a Markdown link with a relative path, from its note's
/// folder; `None` when it climbs above the vault root. `.` segments and empty segments are dropped and `..` climbs.
fn named_path(edge: &GraphEdge) -> Option<String> {
    let relative_to_note =
        matches!(edge.form, LinkForm::Markdown | LinkForm::MarkdownEmbed) && !edge.target.starts_with('/');
    let folder = if relative_to_note {
        edge.from.rsplit_once('/').map_or("", |(folder, _)| folder)
    } else {
        ""
    };
    let mut segments: Vec<&str> = Vec::new();
    for segment in folder.split('/').chain(edge.target.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            name => segments.push(name),
        }
    }
    Some(segments.join("/"))
}

/// Whether `path` is a folder's `index.md`.
fn is_index_note(path: &str) -> bool {
    path == "index.md" || path.ends_with("/index.md")
}
