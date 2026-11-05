//! Loads the event kinds a vault declares in its kind registry folder (`registry/events/` in the scaffold).
//!
//! Recording and `doctor` both read the registry through here, so a definition is usable for recording exactly when
//! `doctor` has nothing to say about it. The registry is read afresh each time: it is a handful of small notes, and a
//! definition changed a moment ago must apply to the very next record.

use std::collections::BTreeMap;

use contextos_core::{VaultPath, VaultSet};
use contextos_events::{EventsError, KindDefinition, KindDefinitionSource, KindRegistry};
use contextos_fs::{EntryKind, Filesystem, ListDirectoryRequest, ReadTextRequest};

use crate::events_folder::is_markdown;
use crate::tools::fs::child_path;

/// The deepest folder level searched below the registry folder.
const MAX_DEPTH: usize = 4;

/// The most definition files read, so a registry folder pointed at something huge cannot stall a record.
const MAX_FILES: usize = 1000;

/// The key series a vault's events sidecar declares, and the kind each one is mapped to by `series_kinds`.
#[derive(Clone, Debug, Default)]
pub(crate) struct SidecarSeries {
    /// Each mapped series with its kind, in series order.
    pub(crate) kinds: BTreeMap<String, String>,
}

impl SidecarSeries {
    /// The series the sidecar maps to `kind`.
    pub(crate) fn mapped_to(&self, kind: &str) -> Vec<&str> {
        self.kinds
            .iter()
            .filter(|(_, mapped)| mapped.as_str() == kind)
            .map(|(series, _)| series.as_str())
            .collect()
    }
}

/// Why a definition file cannot be used.
#[derive(Debug)]
pub(crate) enum KindProblem {
    /// The file could not be read as a kind definition.
    Invalid(EventsError),
    /// The kind it names is defined in more than one file.
    Duplicate { kind: String },
    /// It names a key field, and no series in the events sidecar's `series_kinds` maps to it.
    NoKeySeries { kind: String },
    /// More than one series in the events sidecar's `series_kinds` maps to it.
    AmbiguousKeySeries { kind: String },
}

impl KindProblem {
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Invalid(error) => error.to_string(),
            Self::Duplicate { kind } => format!("`{kind}` is defined in more than one file"),
            Self::NoKeySeries { kind } => {
                format!("`{kind}` names a key field but no series in the events sidecar's series_kinds maps to it")
            }
            Self::AmbiguousKeySeries { kind } => {
                format!("`{kind}` is mapped by more than one series in the events sidecar's series_kinds")
            }
        }
    }
}

/// A definition file that cannot be used, by its vault-relative path.
#[derive(Debug)]
pub(crate) struct UnusableKind {
    pub(crate) path: String,
    pub(crate) problem: KindProblem,
}

/// The kinds a vault can record, with the definition files that cannot be used.
#[derive(Debug, Default)]
pub(crate) struct LoadedKinds {
    pub(crate) registry: KindRegistry,
    pub(crate) unusable: Vec<UnusableKind>,
    /// Whether the registry folder held more definition files, or went deeper, than are read, so some kinds are
    /// missing from `registry`.
    pub(crate) truncated: bool,
}

/// Reads every `*.md` definition below `folder`, in sub-folders too, in path order. A folder's index note is not a
/// definition, so it is left out.
pub(crate) fn load(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root: &VaultPath,
    folder: &str,
    series: &SidecarSeries,
) -> LoadedKinds {
    let mut files = Vec::new();
    let truncated = collect_files(filesystem, roots, root, folder, 0, &mut files);
    files.sort();
    let mut loaded = LoadedKinds {
        truncated,
        ..LoadedKinds::default()
    };
    let mut definitions: Vec<(String, KindDefinition)> = Vec::new();
    for relative in files {
        let Ok(path) = child_path(root, roots, &relative) else {
            continue;
        };
        let stem = relative
            .rsplit('/')
            .next()
            .and_then(|name| name.strip_suffix(".md"))
            .unwrap_or_default()
            .to_owned();
        let text = match filesystem.read_text(&ReadTextRequest { path, limit: None }) {
            Ok(file) => file.content,
            Err(error) => {
                tracing::warn!(code = error.code(), "an event kind definition could not be read");
                loaded.unusable.push(UnusableKind {
                    path: relative,
                    problem: KindProblem::Invalid(EventsError::InvalidDefinition {
                        field: "file",
                        reason: contextos_events::DefinitionRejection::Malformed,
                    }),
                });
                continue;
            }
        };
        match KindDefinition::try_from(KindDefinitionSource {
            stem: &stem,
            text: &text,
        }) {
            Ok(definition) => definitions.push((relative, definition)),
            Err(error) => loaded.unusable.push(UnusableKind {
                path: relative,
                problem: KindProblem::Invalid(error),
            }),
        }
    }
    let mut occurrences: BTreeMap<String, usize> = BTreeMap::new();
    for (_, definition) in &definitions {
        *occurrences.entry(definition.kind.as_str().to_owned()).or_default() += 1;
    }
    for (relative, definition) in definitions {
        let kind = definition.kind.as_str().to_owned();
        if occurrences.get(&kind).copied().unwrap_or(0) > 1 {
            loaded.unusable.push(UnusableKind {
                path: relative,
                problem: KindProblem::Duplicate { kind },
            });
        } else if let Some(problem) = series_problem(&kind, definition.id_field.is_some(), series) {
            loaded.unusable.push(UnusableKind {
                path: relative,
                problem,
            });
        } else if let Err(error) = loaded.registry.insert(definition) {
            loaded.unusable.push(UnusableKind {
                path: relative,
                problem: KindProblem::Invalid(error),
            });
        }
    }
    loaded
}

/// What is wrong with the series the sidecar maps to `kind`, if anything.
fn series_problem(kind: &str, keyed: bool, series: &SidecarSeries) -> Option<KindProblem> {
    match series.mapped_to(kind).len() {
        0 if keyed => Some(KindProblem::NoKeySeries { kind: kind.to_owned() }),
        0 | 1 => None,
        _ => Some(KindProblem::AmbiguousKeySeries { kind: kind.to_owned() }),
    }
}

/// Gathers the Markdown files in `folder` and below, returning whether any were left out for want of room.
fn collect_files(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root: &VaultPath,
    folder: &str,
    depth: usize,
    files: &mut Vec<String>,
) -> bool {
    let Ok(path) = child_path(root, roots, folder) else {
        return false;
    };
    let Ok(listing) = filesystem.list_directory(&ListDirectoryRequest { path }) else {
        return false;
    };
    let mut truncated = false;
    for entry in listing.entries {
        let relative = format!("{}/{}", folder.trim_end_matches('/'), entry.name);
        match entry.kind {
            // A folder's index note lists the definitions beside it and is not one itself.
            EntryKind::File if contextos_index::is_folder_index(&entry.name) => {}
            EntryKind::File if is_markdown(&entry.name) && files.len() < MAX_FILES => files.push(relative),
            EntryKind::File if is_markdown(&entry.name) => truncated = true,
            EntryKind::Directory if depth < MAX_DEPTH => {
                truncated |= collect_files(filesystem, roots, root, &relative, depth + 1, files);
            }
            EntryKind::Directory => truncated = true,
            EntryKind::File => {}
        }
    }
    truncated
}
