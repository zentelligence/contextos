//! What the `Duplicate id detection` and `Record conformance` doctor checks share: the record notes of a vault and the
//! bases that govern them.
//!
//! A base's records are exactly the notes its own top-level filters select, evaluated by the engine `base_query`
//! runs, so a base written with `file.folder ==`, `file.inFolder(...)`, property conditions, or an
//! `file.basename != "index"` exclusion is read the same way here as everywhere else. An `id` only has to be unique
//! among one base's records, and a `base` property only has to name a base that selects its note.
//!
//! A `base` property is a link, resolved from the note that holds it by the same nearest-first rule as every other
//! link: a bare `[[<name>.base]]`, whatever the name, names the base file of that name in the note's own folder or its
//! nearest ancestor, and only a name no ancestor holds is looked up across the vault, where more than one match is
//! ambiguous.
//!
//! Two kinds of note never take part. A note under `templates/` is a pattern holding placeholders, not a record. A
//! base's sidecar (the note beside `<name>.base` that carries its key scheme) describes the base and sits next to it
//! rather than among its records.

use std::collections::BTreeSet;
use std::convert::Infallible;

use contextos_core::{VaultPath, VaultPathInput, VaultRoot, VaultRootId, VaultSet};
use contextos_fs::{Filesystem, ReadTextRequest, SearchFilesRequest};
use contextos_obsidian::{
    BASE_PROPERTY, BaseDocument, BaseSidecar, FrontmatterDocument, LinkResolution, LinkTargets, resolve_link,
};
use serde_json::{Map, Value};

use super::DoctorError;
use crate::tools::base_query::{identity_scope, scope_records};

const TEMPLATES_FOLDER: &str = "templates";

/// A note that may name its base, with the `base` property and the frontmatter the `Record conformance` check reads.
pub(super) struct RecordNote {
    pub path: String,
    pub base: Option<Value>,
    pub frontmatter: Map<String, Value>,
}

/// A record a base selects, with its `id` when it has one.
pub(super) struct BaseRecord {
    pub path: String,
    pub id: Option<String>,
}

/// A `.base` file and the records its filters select.
pub(super) struct VaultBase {
    pub path: String,
    /// The base's records (its identity scope, templates and base sidecars aside), or why they could not be read:
    /// the file is not a base, or its filters could not be evaluated.
    records: Result<Vec<BaseRecord>, String>,
    /// The key scheme and template declaration of the base's sidecar, when it has one that can be read.
    pub sidecar: Option<BaseSidecar>,
}

impl VaultBase {
    /// Whether the base could be read and its filters evaluated, so its records are known.
    pub fn is_readable(&self) -> bool {
        self.records.is_ok()
    }

    /// Why the base's records could not be read, when they could not.
    pub fn unreadable_reason(&self) -> Option<&str> {
        self.records.as_ref().err().map(String::as_str)
    }

    /// Whether the base's filters select `note`; always false for a base that could not be read.
    pub fn selects(&self, note: &str) -> bool {
        self.records
            .as_deref()
            .is_ok_and(|records| records.iter().any(|record| record.path == note))
    }

    /// The path of the base's sidecar: the note of the same name beside the `.base` file.
    pub fn sidecar_path(&self) -> String {
        format!("{}.md", self.path.strip_suffix(".base").unwrap_or(&self.path))
    }

    /// The records the base selects, none for a base that could not be read.
    pub fn records(&self) -> &[BaseRecord] {
        self.records.as_deref().unwrap_or_default()
    }
}

pub(super) struct VaultBases {
    bases: Vec<VaultBase>,
}

impl VaultBases {
    pub fn load(root: &VaultRoot, roots: &VaultSet, filesystem: &Filesystem) -> Result<Self, DoctorError> {
        let root_id = root_path(root, roots)?.root_id();
        let paths = search(root, roots, filesystem, "**/*.base")?;
        let sidecars: BTreeSet<String> = paths
            .iter()
            .filter_map(|path| path.strip_suffix(".base"))
            .map(|stem| format!("{stem}.md"))
            .collect();
        let mut bases = Vec::new();
        for path in paths {
            let records =
                match read(roots, root, filesystem, &path).map(|source| BaseDocument::try_from(source.as_str())) {
                    None => Err("it could not be read".to_owned()),
                    Some(Err(error)) => Err(format!("it is not a valid base ({error})")),
                    Some(Ok(document)) => selected_records(filesystem, roots, root_id, &document, &sidecars),
                };
            let sidecar = path
                .strip_suffix(".base")
                .and_then(|stem| read(roots, root, filesystem, &format!("{stem}.md")))
                .and_then(|source| FrontmatterDocument::try_from(source.as_str()).ok())
                .and_then(|document| BaseSidecar::try_from(document.frontmatter()).ok());
            bases.push(VaultBase { path, records, sidecar });
        }
        Ok(Self { bases })
    }

    pub fn iter(&self) -> impl Iterator<Item = &VaultBase> {
        self.bases.iter()
    }

    /// Every readable base whose filters select `note`.
    pub fn selecting(&self, note: &str) -> Vec<&VaultBase> {
        self.bases.iter().filter(|base| base.selects(note)).collect()
    }

    /// The base `link`, as written in the `base` property of `note`, names, resolved as every link is.
    pub fn resolve(&self, note: &str, link: &str) -> Resolved<'_> {
        match resolve_link(self, note, link) {
            Ok(LinkResolution::Found(path)) => self
                .bases
                .iter()
                .find(|base| base.path == path)
                .map_or(Resolved::Missing, Resolved::Found),
            Ok(LinkResolution::Ambiguous { .. }) => Resolved::Ambiguous,
            Ok(LinkResolution::NotFound) | Err(_) => Resolved::Missing,
        }
    }

    /// Whether `note` is the sidecar of a base: the note of the same name beside a `.base` file.
    pub fn is_sidecar(&self, note: &str) -> bool {
        note.strip_suffix(".md").is_some_and(|stem| {
            self.bases
                .iter()
                .any(|base| base.path.strip_suffix(".base") == Some(stem))
        })
    }
}

/// The vault's bases, answering the questions link resolution asks of a `base` property. Only a `.base` file can be
/// the base a record names, so the base files are the only targets.
impl LinkTargets for VaultBases {
    type Error = Infallible;

    fn is_file(&self, path: &str) -> Result<bool, Self::Error> {
        Ok(self.bases.iter().any(|base| base.path == path))
    }

    fn files_ending_with(&self, suffix: &str) -> Result<Vec<String>, Self::Error> {
        let suffix = suffix.to_lowercase();
        let ending = format!("/{suffix}");
        Ok(self
            .bases
            .iter()
            .filter(|base| {
                let path = base.path.to_lowercase();
                path == suffix || path.ends_with(&ending)
            })
            .map(|base| base.path.clone())
            .collect())
    }
}

/// How a `base` link resolves against the bases present in the vault.
pub(super) enum Resolved<'a> {
    Found(&'a VaultBase),
    Missing,
    /// A bare name no ancestor of the note holds and that more than one base file matches, so Obsidian could not
    /// pick one either.
    Ambiguous,
}

/// The records in `document`'s identity scope, leaving out templates and base sidecars, or why its filters could not
/// be evaluated.
fn selected_records(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root_id: VaultRootId,
    document: &BaseDocument,
    sidecars: &BTreeSet<String>,
) -> Result<Vec<BaseRecord>, String> {
    let scope = identity_scope(document.definition().get("filters"));
    let selected = scope_records(filesystem, roots, root_id, scope.as_ref())
        .map_err(|error| format!("its filters could not be evaluated ({error})"))?;
    Ok(selected
        .into_iter()
        .filter(|record| !is_template(&record.path) && !sidecars.contains(&record.path))
        .map(|record| BaseRecord {
            id: id_of(&record.frontmatter),
            path: record.path,
        })
        .collect())
}

fn id_of(frontmatter: &Map<String, Value>) -> Option<String> {
    frontmatter.get("id").and_then(Value::as_str).map(str::to_owned)
}

/// Whether `note` lies under the vault's `templates/` folder.
pub(super) fn is_template(note: &str) -> bool {
    note.strip_prefix(TEMPLATES_FOLDER)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Every markdown note with readable frontmatter, except templates.
pub(super) fn load_record_notes(
    root: &VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
) -> Result<Vec<RecordNote>, DoctorError> {
    let mut notes = Vec::new();
    for path in search(root, roots, filesystem, "**/*.md")? {
        if is_template(&path) {
            continue;
        }
        let Some(content) = read(roots, root, filesystem, &path) else {
            continue;
        };
        let Ok(document) = FrontmatterDocument::try_from(content.as_str()) else {
            continue;
        };
        let frontmatter = document.frontmatter();
        notes.push(RecordNote {
            base: frontmatter.get(BASE_PROPERTY).cloned(),
            frontmatter: frontmatter.clone(),
            path,
        });
    }
    Ok(notes)
}

fn search(
    root: &VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
    pattern: &str,
) -> Result<Vec<String>, DoctorError> {
    Ok(filesystem.search_files(&SearchFilesRequest {
        path: root_path(root, roots)?,
        pattern: pattern.to_owned(),
        exclude_patterns: Vec::new(),
        max_results: usize::MAX,
    })?)
}

/// The vault root itself as a [`VaultPath`].
fn root_path(root: &VaultRoot, roots: &VaultSet) -> Result<VaultPath, DoctorError> {
    Ok(VaultPath::try_from(VaultPathInput {
        roots,
        raw: root
            .path()
            .to_str()
            .ok_or_else(|| DoctorError::NonUtf8VaultPath(root.path().to_path_buf()))?,
    })?)
}

fn read(roots: &VaultSet, root: &VaultRoot, filesystem: &Filesystem, relative: &str) -> Option<String> {
    let absolute = root.path().join(relative);
    let path = VaultPath::try_from(VaultPathInput {
        roots,
        raw: absolute.to_str()?,
    })
    .ok()?;
    filesystem
        .read_text(&ReadTextRequest { path, limit: None })
        .ok()
        .map(|read| read.content)
}
