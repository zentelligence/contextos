//! `frontmatter_link`: adds and removes wikilinks in the link fields a base's
//! sidecar declares, writing the reciprocal on the linked note.
//!
//! The note is edited in one atomic write, with the same conflict check as
//! `frontmatter_update`. Everything that can be refused is decided before the
//! first write, so a refused call changes nothing. Reciprocals are secondary
//! to that completed write: one that cannot be written is reported, and
//! repeating the same call repairs a failed write, because both add and
//! remove are idempotent.
//!
//! A link is written relative to the governing base when the target is in its
//! scope: the bare key for a keyed base, or the path within the base's folder
//! otherwise. A target outside the base is named by its full vault path.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use contextos_core::{ContentHash, Origin, VaultPath, VaultPathInput, VaultRootId, VaultSet, WriteMutation};
use contextos_fs::{Filesystem, FsError, ReadTextRequest, SearchFilesRequest};
use contextos_obsidian::{
    BASE_PROPERTY, BaseDocument, BaseKeyScheme, BaseLinkTarget, BaseSidecar, BaseSidecarError, FrontmatterDocument,
    LinkError, LinkField, LinkOutcome, LinkTarget, QueryDefinition, QueryFormat, RecordsFolder, WikiLink,
    WikiLinkInput, records_folder,
};
use serde_json::{Map, Value};

use crate::server::{RoutedMutationService, WarningMessages};
use crate::tool_error::ToolError;
use crate::tools::base_query;
use crate::tools::base_sidecar::sidecar_path;
use crate::tools::frontmatter_link_types::{
    FrontmatterLinkToolResult, LinkChangeOutcome, LinkChangeToolResult, LinkOperationInput, LinkOperationKind,
    MAX_LINK_OPERATIONS, ReciprocalToolResult,
};

/// Everything one call needs.
pub(crate) struct LinkRequest<'a> {
    pub(crate) note: VaultPath,
    pub(crate) roots: &'a VaultSet,
    pub(crate) operations: Vec<LinkOperationInput>,
    pub(crate) expected_hash: Option<ContentHash>,
    pub(crate) timestamp: &'a str,
}

/// The base that governs a note: the narrowest one whose scope contains it
/// and whose sidecar declares link fields.
struct GoverningBase {
    scope: FolderScope,
    /// Whether the base assigns keys (`{key_prefix}-{key_id}`), so that a
    /// record's file name is normally its key.
    keyed: bool,
    fields: Vec<LinkField>,
}

impl GoverningBase {
    fn field(&self, name: &str) -> Result<&LinkField, LinkError> {
        self.fields
            .iter()
            .find(|field| field.name() == name)
            .ok_or_else(|| LinkError::UndeclaredField {
                field: name.to_owned(),
                declared: self.fields.iter().map(|field| field.name().to_owned()).collect(),
            })
    }

    /// Whether `relative` (a vault-relative path) lies within this base's scope.
    fn contains(&self, relative: &str) -> bool {
        self.scope.contains(relative)
    }

    /// `relative` without the base's folder and its extension, when it lies
    /// within the scope: the note's path in the base.
    fn path_in_scope(&self, relative: &str) -> Option<String> {
        let within = if self.scope.path.is_empty() {
            relative
        } else {
            relative.strip_prefix(&format!("{}/", self.scope.path))?
        };
        Some(within.trim_end_matches(".md").to_owned())
    }
}

/// The folder a base's records sit in, as its top-level filters name it: an empty `path` for a base whose filters
/// name none, which covers the whole vault.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
struct FolderScope {
    path: String,
    /// False for `file.folder == "..."`, whose records sit directly in the folder and never beneath it.
    includes_subfolders: bool,
}

impl FolderScope {
    /// Whether `relative` (a vault-relative path) lies where this base's records may be.
    fn contains(&self, relative: &str) -> bool {
        self.path.is_empty()
            || RecordsFolder {
                path: &self.path,
                includes_subfolders: self.includes_subfolders,
            }
            .contains(relative)
    }

    /// The filter clause that selects this folder's notes, `None` for the whole vault.
    fn clause(&self) -> Option<String> {
        if self.path.is_empty() {
            None
        } else if self.includes_subfolders {
            Some(format!("file.inFolder(\"{}\")", self.path))
        } else {
            Some(format!("file.folder == \"{}\"", self.path))
        }
    }
}

fn has_markdown_extension(text: &str) -> bool {
    std::path::Path::new(text)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn portable(text: &str) -> String {
    text.replace('\\', "/")
}

fn stem_of(relative: &str) -> Option<&str> {
    std::path::Path::new(relative)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
}

/// A note that is, or is being linked to: its path and parsed frontmatter.
#[derive(Clone)]
struct NoteRef {
    path: VaultPath,
    relative: String,
    stem: String,
    document: FrontmatterDocument,
    hash: ContentHash,
}

impl NoteRef {
    fn new(path: &VaultPath, content: &str, hash: ContentHash) -> Result<Self, ToolError> {
        let relative = path.portable_relative();
        let stem = stem_of(&relative)
            .ok_or(ToolError::Invalid("note path must have a UTF-8 filename"))?
            .to_owned();
        Ok(Self {
            path: path.clone(),
            relative,
            stem,
            document: FrontmatterDocument::try_from(content)?,
            hash,
        })
    }

    /// The vault path of the base the note's `base` property links to, when it has one and it is a link to a
    /// `.base` file.
    fn declared_base(&self) -> Option<String> {
        let Value::String(stored) = self.document.frontmatter().get(BASE_PROPERTY)? else {
            return None;
        };
        BaseLinkTarget::try_from(stored.as_str())
            .ok()
            .map(|target| target.as_str().to_owned())
    }

    /// Whether the file is named for the note's `id` (compared without regard
    /// to case), as the file of a record created by `base_record_create` is.
    fn named_for_its_key(&self) -> bool {
        self.document
            .frontmatter()
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.eq_ignore_ascii_case(&self.stem))
    }

    /// The texts a link to this note may show, most preferred first: its
    /// `id`, its `title`, its file name.
    fn display_candidates(&self) -> Vec<&str> {
        let frontmatter = self.document.frontmatter();
        let mut candidates: Vec<&str> = ["id", "title"]
            .into_iter()
            .filter_map(|key| frontmatter.get(key).and_then(Value::as_str))
            .collect();
        candidates.push(&self.stem);
        candidates
    }
}

/// What a stored entry is compared against to decide whether it links to a
/// target, whichever form it was written in: the full vault path, the path
/// within the base, the bare key, or a bare file name that is unique in the
/// vault.
struct Matcher {
    path_key: String,
    scope_key: Option<String>,
    key: Option<String>,
    stem_key: String,
    bare_name_matches: bool,
}

impl Matcher {
    fn matches(&self, entry: &str) -> bool {
        LinkTarget::try_from(entry).is_ok_and(|found| {
            let text = found
                .as_str()
                .trim_start_matches('/')
                .trim_end_matches(".md")
                .to_ascii_lowercase();
            text == self.path_key
                || self.scope_key.as_deref() == Some(text.as_str())
                || self.key.as_deref() == Some(text.as_str())
                || (self.bare_name_matches && !text.contains('/') && text == self.stem_key)
        })
    }
}

/// A resolved link target: an existing note, or, for a removal only, the
/// vault path of a note that is no longer there.
enum Target {
    Existing(Box<NoteRef>),
    Dangling { relative: String },
}

enum SidecarState {
    Absent,
    Valid(BaseSidecar),
    Malformed(BaseSidecarError),
    /// The sidecar could not be read or parsed as a document at all. Only
    /// matters when its base contains the note in question.
    Unreadable,
}

/// A base whose folder scope and sidecar were read, before it is known
/// whether it governs a particular note.
struct BaseCandidate {
    base_path: String,
    scope: FolderScope,
    sidecar: SidecarState,
}

struct Context<'a> {
    filesystem: &'a Filesystem,
    roots: &'a VaultSet,
    root_id: VaultRootId,
    root_path: VaultPath,
    candidates: Vec<BaseCandidate>,
    /// Lower-cased file name to the number of notes with it, built by one
    /// walk of the vault the first time it is needed.
    stem_counts: RefCell<Option<HashMap<String, usize>>>,
    /// Ids already resolved this call, per base scope.
    resolved_ids: RefCell<HashMap<(FolderScope, String), String>>,
}

fn vault_path(roots: &VaultSet, root_id: VaultRootId, relative: &str) -> Result<VaultPath, ToolError> {
    let root = roots
        .root(root_id)
        .ok_or(ToolError::Invalid("vault root is not configured"))?;
    let absolute = root.path().join(relative);
    let raw = absolute
        .to_str()
        .ok_or(ToolError::Invalid("note path must be valid UTF-8"))?;
    Ok(VaultPath::try_from(VaultPathInput { roots, raw })?)
}

fn read_note(filesystem: &Filesystem, path: &VaultPath) -> Result<Option<NoteRef>, ToolError> {
    contextos_core::ReadsVault::read_optional_text(filesystem, path)?
        .map(|text| NoteRef::new(path, &text.content, text.content_hash))
        .transpose()
}

/// Rejects text that the base filter grammar, which has no escape sequences,
/// could not carry inside a quoted literal.
fn ensure_filter_safe(text: &str, what: &'static str) -> Result<(), ToolError> {
    if text.contains(['"', '\'', '\\']) || text.chars().any(char::is_control) {
        return Err(ToolError::Invalid(what));
    }
    Ok(())
}

/// The folder a `.base`'s top-level filters confine its records to
/// ([`records_folder`]: `file.inFolder("...")` or `file.folder == "..."`),
/// or `None` when it cannot be read or parsed and so cannot govern any note.
fn declared_scope(filesystem: &Filesystem, base_path: &VaultPath) -> Option<FolderScope> {
    let source = contextos_core::ReadsVault::read_optional_text(filesystem, base_path)
        .ok()
        .flatten()?;
    let definition = BaseDocument::try_from(source.content.as_str()).ok()?;
    // A base naming no folder covers the whole vault.
    Some(
        records_folder(definition.definition().get("filters"))
            .map(|folder| FolderScope {
                path: folder.path.to_owned(),
                includes_subfolders: folder.includes_subfolders,
            })
            .unwrap_or_default(),
    )
}

fn read_sidecar_state(
    filesystem: &Filesystem,
    roots: &VaultSet,
    base_path: &VaultPath,
) -> Result<SidecarState, ToolError> {
    let Some(text) = contextos_core::ReadsVault::read_optional_text(filesystem, &sidecar_path(roots, base_path)?)?
    else {
        return Ok(SidecarState::Absent);
    };
    let document = FrontmatterDocument::try_from(text.content.as_str())?;
    Ok(match BaseSidecar::try_from(document.frontmatter()) {
        Ok(sidecar) => SidecarState::Valid(sidecar),
        Err(error) => SidecarState::Malformed(error),
    })
}

impl<'a> Context<'a> {
    /// Reads every `.base` in the vault with the folder scope it declares and
    /// its sidecar, once per call. A base or sidecar that cannot be read is
    /// recorded, not raised: it only matters if it contains the note.
    fn load(filesystem: &'a Filesystem, roots: &'a VaultSet, root_id: VaultRootId) -> Result<Self, ToolError> {
        let root = roots
            .root(root_id)
            .ok_or(ToolError::Invalid("vault root is not configured"))?;
        let root_path = VaultPath::try_from(VaultPathInput {
            roots,
            raw: root
                .path()
                .to_str()
                .ok_or(ToolError::Invalid("vault root must be valid UTF-8"))?,
        })?;
        let base_files = filesystem.search_files(&SearchFilesRequest {
            path: root_path.clone(),
            pattern: "**/*.base".to_owned(),
            exclude_patterns: Vec::new(),
            max_results: usize::MAX,
        })?;

        let mut candidates = Vec::new();
        for relative in base_files {
            let relative = portable(&relative);
            let Ok(base_path) = vault_path(roots, root_id, &relative) else {
                continue;
            };
            let Some(scope) = declared_scope(filesystem, &base_path) else {
                continue;
            };
            let sidecar = read_sidecar_state(filesystem, roots, &base_path).unwrap_or(SidecarState::Unreadable);
            candidates.push(BaseCandidate {
                base_path: relative,
                scope,
                sidecar,
            });
        }
        Ok(Self {
            filesystem,
            roots,
            root_id,
            root_path,
            candidates,
            stem_counts: RefCell::new(None),
            resolved_ids: RefCell::new(HashMap::new()),
        })
    }

    /// The base governing the note at `relative`: of the bases whose scope
    /// contains it and whose sidecar declares link fields, the one with the
    /// narrowest scope. A base declaring none does not shadow a wider one.
    ///
    /// # Errors
    ///
    /// [`LinkError::NoLinkFields`] when none qualifies, [`LinkError::AmbiguousBase`]
    /// when the narrowest scope is shared, and the sidecar's own error when a
    /// containing base's sidecar is malformed or unreadable.
    fn governing(&self, note: &NoteRef) -> Result<GoverningBase, ToolError> {
        let relative = note.relative.as_str();
        let declared = note.declared_base();
        let mut qualifying: Vec<(&BaseCandidate, &BaseSidecar)> = Vec::new();
        // A sidecar that cannot be used only blocks the call when no base the note names governs it, because it
        // might have been the governing one. The note's own base is reported ahead of any other.
        let mut unusable: Option<ToolError> = None;
        for candidate in &self.candidates {
            if !candidate.scope.contains(relative) {
                continue;
            }
            let failure = match &candidate.sidecar {
                SidecarState::Absent => None,
                SidecarState::Malformed(error) => Some(ToolError::BaseSidecarOf {
                    base: candidate.base_path.clone(),
                    source: error.clone(),
                }),
                SidecarState::Unreadable => Some(
                    match vault_path(self.roots, self.root_id, &candidate.base_path)
                        .and_then(|base_path| read_sidecar_state(self.filesystem, self.roots, &base_path))
                    {
                        Err(ToolError::Frontmatter(source)) => ToolError::BaseSidecarUnparsable {
                            base: candidate.base_path.clone(),
                            source,
                        },
                        Err(error) => error,
                        Ok(_) => ToolError::Invalid("a base sidecar became unreadable during this call"),
                    },
                ),
                SidecarState::Valid(sidecar) => {
                    if !sidecar.link_fields().is_empty() {
                        qualifying.push((candidate, sidecar));
                    }
                    None
                }
            };
            if let Some(failure) = failure
                && (unusable.is_none() || declared.as_deref() == Some(candidate.base_path.as_str()))
            {
                unusable = Some(failure);
            }
        }
        // A note that names its base, when that base contains it and declares link fields, is governed by it
        // whatever other bases also qualify. A `base` naming anything else is ignored.
        let declared_choice = declared.as_deref().and_then(|declared| {
            qualifying
                .iter()
                .find(|(candidate, _)| candidate.base_path == declared)
                .copied()
        });
        if let Some((candidate, sidecar)) = declared_choice {
            return Ok(Self::governing_base(candidate, sidecar));
        }
        if let Some(failure) = unusable {
            return Err(failure);
        }
        let narrowest = qualifying
            .iter()
            .map(|(candidate, _)| candidate.scope.path.len())
            .max()
            .ok_or(LinkError::NoLinkFields)?;
        let mut narrowest_bases = qualifying
            .into_iter()
            .filter(|(candidate, _)| candidate.scope.path.len() == narrowest);
        let Some((candidate, sidecar)) = narrowest_bases.next() else {
            return Err(LinkError::NoLinkFields.into());
        };
        let rivals: Vec<String> = narrowest_bases.map(|(rival, _)| rival.base_path.clone()).collect();
        if !rivals.is_empty() {
            let mut bases = vec![candidate.base_path.clone()];
            bases.extend(rivals);
            return Err(LinkError::AmbiguousBase { bases }.into());
        }
        Ok(Self::governing_base(candidate, sidecar))
    }

    fn governing_base(candidate: &BaseCandidate, sidecar: &BaseSidecar) -> GoverningBase {
        GoverningBase {
            scope: candidate.scope.clone(),
            keyed: matches!(sidecar.scheme(), BaseKeyScheme::Sequential { .. }),
            fields: sidecar.link_fields().to_vec(),
        }
    }

    /// Finds the one note in `base`'s scope whose `id` is `id`.
    fn find_by_id(&self, base: &GoverningBase, id: &str) -> Result<String, ToolError> {
        ensure_filter_safe(
            id,
            "a link target id may not contain quotes, backslashes, or control characters; pass the note's vault path instead",
        )?;
        ensure_filter_safe(
            &base.scope.path,
            "the base's folder scope contains quotes, backslashes, or control characters; pass the note's vault path instead",
        )?;
        let cache_key = (base.scope.clone(), id.to_owned());
        if let Some(found) = self.resolved_ids.borrow().get(&cache_key) {
            return Ok(found.clone());
        }
        let id_literal = format!("\"{id}\"");
        let filters = match base.scope.clause() {
            Some(clause) => format!("{clause} && id == {id_literal}"),
            None => format!("id == {id_literal}"),
        };
        let mut definition = Map::new();
        definition.insert("filters".to_owned(), Value::String(filters));
        definition.insert(
            "order".to_owned(),
            Value::Array(vec![Value::String("file.path".to_owned())]),
        );
        let definition = QueryDefinition::from_inline(&definition)?;
        let outcome = base_query::run(
            self.filesystem,
            self.roots,
            self.root_id,
            &definition,
            QueryFormat::Json,
            &[],
        )?;
        let rows: Vec<Map<String, Value>> = serde_json::from_str(&outcome.content)
            .map_err(|_| ToolError::Invalid("the id lookup returned an unreadable result"))?;
        let mut matches: Vec<String> = rows
            .iter()
            .filter_map(|row| row.get("file.path").and_then(Value::as_str).map(portable))
            .collect();
        let found = match matches.len() {
            0 => return Err(LinkError::TargetNotFound { target: id.to_owned() }.into()),
            1 => matches.remove(0),
            _ => {
                return Err(LinkError::TargetAmbiguous {
                    target: id.to_owned(),
                    matches,
                }
                .into());
            }
        };
        self.resolved_ids.borrow_mut().insert(cache_key, found.clone());
        Ok(found)
    }

    /// Resolves a target named by id or by vault path. A path to a note that
    /// is not there is returned as [`Target::Dangling`], which only a removal
    /// accepts.
    fn resolve(&self, base: &GoverningBase, argument: &str) -> Result<Target, ToolError> {
        let by_path = argument.contains('/') || has_markdown_extension(argument);
        let relative = if by_path {
            let trimmed = argument.trim_start_matches('/');
            if has_markdown_extension(trimmed) {
                trimmed.to_owned()
            } else {
                format!("{trimmed}.md")
            }
        } else {
            self.find_by_id(base, argument)?
        };
        let path = vault_path(self.roots, self.root_id, &relative)?;
        // A symlink can lead into another configured root, whose paths, bases,
        // and write lock are not this call's.
        if path.root_id() != self.root_id {
            return Err(LinkError::TargetNotFound {
                target: argument.to_owned(),
            }
            .into());
        }
        match read_note(self.filesystem, &path)? {
            Some(note) => Ok(Target::Existing(Box::new(note))),
            None if by_path => Ok(Target::Dangling { relative }),
            None => Err(LinkError::TargetNotFound {
                target: argument.to_owned(),
            }
            .into()),
        }
    }

    /// How many notes in the vault have this file name (without regard to
    /// case), from one walk of the vault per call.
    fn stem_count(&self, stem: &str) -> Result<usize, ToolError> {
        let mut counts = self.stem_counts.borrow_mut();
        if counts.is_none() {
            let files = self.filesystem.search_files(&SearchFilesRequest {
                path: self.root_path.clone(),
                pattern: "**/*.md".to_owned(),
                exclude_patterns: Vec::new(),
                max_results: usize::MAX,
            })?;
            let mut index: HashMap<String, usize> = HashMap::new();
            for file in files {
                if let Some(name) = stem_of(&file) {
                    *index.entry(name.to_ascii_lowercase()).or_default() += 1;
                }
            }
            *counts = Some(index);
        }
        Ok(counts
            .as_ref()
            .and_then(|index| index.get(&stem.to_ascii_lowercase()))
            .copied()
            .unwrap_or(0))
    }

    /// The matcher for `target`, as a note governed by `base` would link to it.
    fn matcher(&self, base: &GoverningBase, target: &Target) -> Result<Matcher, ToolError> {
        let (relative, stem, named_for_its_key) = match target {
            Target::Existing(note) => (note.relative.as_str(), note.stem.as_str(), note.named_for_its_key()),
            Target::Dangling { relative } => (relative.as_str(), stem_of(relative).unwrap_or_default(), false),
        };
        let count = self.stem_count(stem)?;
        Ok(Matcher {
            path_key: relative.trim_end_matches(".md").to_ascii_lowercase(),
            scope_key: base.path_in_scope(relative).map(|path| path.to_ascii_lowercase()),
            key: (base.keyed && named_for_its_key && base.contains(relative)).then(|| stem.to_ascii_lowercase()),
            stem_key: stem.to_ascii_lowercase(),
            // A bare file name stands for this note when it names exactly one
            // note (this one), or, for a note that is gone, no note at all.
            bare_name_matches: match target {
                Target::Existing(_) => count == 1,
                Target::Dangling { .. } => count == 0,
            },
        })
    }

    /// The link to `target` as written from a note governed by `base`.
    ///
    /// Inside the base the target is named by its key, when the base assigns
    /// keys and the file is named for its key, and otherwise by its path
    /// within the base's folder: both are unique within the base by
    /// construction, and do not change when a note elsewhere shares a name.
    /// Anywhere else it is named by its full vault path.
    fn link_to(base: &GoverningBase, target: &NoteRef) -> Result<WikiLink, ToolError> {
        let text = match base.path_in_scope(&target.relative) {
            Some(_) if base.keyed && target.named_for_its_key() => target.stem.clone(),
            Some(within) => within,
            None => target.relative.trim_end_matches(".md").to_owned(),
        };
        let mut last_error = None;
        for display in target.display_candidates() {
            match WikiLink::try_from(WikiLinkInput { target: &text, display }) {
                Ok(link) => return Ok(link),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.map_or(ToolError::Invalid("a link needs display text"), ToolError::from))
    }
}

/// What to do to a link field.
enum LinkAction<'a> {
    Add(&'a WikiLink),
    Remove,
}

/// Applies `value` to `current`, and records it in `patch`.
fn apply(current: &mut Map<String, Value>, patch: &mut Map<String, Value>, field: &str, value: Value) {
    if value.is_null() {
        current.shift_remove(field);
    } else {
        current.insert(field.to_owned(), value.clone());
    }
    patch.insert(field.to_owned(), value);
}

/// Runs one edit of `field` against `current`, applying any change to it and
/// to `patch`.
fn edit_field(
    field: &LinkField,
    action: &LinkAction<'_>,
    current: &mut Map<String, Value>,
    patch: &mut Map<String, Value>,
    matcher: &Matcher,
) -> Result<LinkOutcome, LinkError> {
    let same = |entry: &str| matcher.matches(entry);
    let edit = match action {
        LinkAction::Add(link) => field.with_link(current, link, &same)?,
        LinkAction::Remove => field.without_links(current, &same)?,
    };
    if let Some(value) = edit.value {
        apply(current, patch, field.name(), value);
    }
    Ok(edit.outcome)
}

/// The edits accumulating for one linked note.
struct LinkedNoteEdit {
    note: NoteRef,
    current: Map<String, Value>,
    patch: Map<String, Value>,
}

struct InverseJob {
    target: NoteRef,
    field: String,
    inverse: String,
    kind: LinkOperationKind,
}

/// Why a reciprocal was not planned.
enum Skip {
    /// Ordinary: the linked note is not governed by a base declaring the
    /// pair. Reported by outcome only.
    Expected,
    /// Something to fix: reported by outcome and as a warning.
    Notable(String),
}

/// Plans the reciprocal on `edit` for `job`: the link from the linked note
/// back to `note`.
fn plan_inverse(
    context: &Context<'_>,
    note: &NoteRef,
    job: &InverseJob,
    edit: &mut LinkedNoteEdit,
) -> Result<LinkOutcome, Skip> {
    let notable = |reason: String| Skip::Notable(format!("no reciprocal written on {}: {reason}", job.target.relative));
    let base = match context.governing(&job.target) {
        Ok(base) => base,
        Err(ToolError::Link(LinkError::NoLinkFields)) => return Err(Skip::Expected),
        Err(error) => return Err(notable(error.to_string())),
    };
    let Ok(field) = base.field(&job.inverse) else {
        return Err(Skip::Expected);
    };
    if field.inverse() != Some(job.field.as_str()) {
        return Err(Skip::Expected);
    }
    let matcher = context
        .matcher(&base, &Target::Existing(Box::new(note.clone())))
        .map_err(|error| notable(error.to_string()))?;
    let outcome = match job.kind {
        LinkOperationKind::Add => {
            let link = Context::link_to(&base, note).map_err(|error| notable(error.to_string()))?;
            edit_field(
                field,
                &LinkAction::Add(&link),
                &mut edit.current,
                &mut edit.patch,
                &matcher,
            )
        }
        LinkOperationKind::Remove => {
            edit_field(field, &LinkAction::Remove, &mut edit.current, &mut edit.patch, &matcher)
        }
    };
    outcome.map_err(|error| notable(error.to_string()))
}

fn stamped_content(
    document: &FrontmatterDocument,
    patch: Map<String, Value>,
    timestamp: &str,
) -> Result<String, ToolError> {
    let mut document = document.clone();
    document.apply_merge_patch(patch, timestamp);
    Ok(String::try_from(&document)?)
}

/// The forward edits to the note itself, and the reciprocals they call for.
struct ForwardPlan {
    patch: Map<String, Value>,
    changes: Vec<LinkChangeToolResult>,
    jobs: Vec<InverseJob>,
}

/// Plans one operation against the note's frontmatter in memory.
fn plan_operation(
    context: &Context<'_>,
    base: &GoverningBase,
    note: &NoteRef,
    operation: &LinkOperationInput,
    working: (&mut Map<String, Value>, &mut Map<String, Value>),
) -> Result<(LinkChangeToolResult, Option<InverseJob>), ToolError> {
    let (current, patch) = working;
    let field = base.field(&operation.field)?;
    let target = context.resolve(base, &operation.target)?;
    if let Target::Existing(existing) = &target
        && existing.path == note.path
    {
        return Err(LinkError::SelfReference.into());
    }
    let matcher = context.matcher(base, &target)?;
    let (outcome, link) = match (&target, operation.op) {
        (Target::Existing(existing), LinkOperationKind::Add) => {
            let link = Context::link_to(base, existing)?;
            let outcome = edit_field(field, &LinkAction::Add(&link), current, patch, &matcher)?;
            (outcome, link.as_ref().to_owned())
        }
        (Target::Dangling { .. }, LinkOperationKind::Add) => {
            return Err(LinkError::TargetNotFound {
                target: operation.target.clone(),
            }
            .into());
        }
        (_, LinkOperationKind::Remove) => {
            let removed = field.entries(current)?.into_iter().find(|entry| matcher.matches(entry));
            let outcome = edit_field(field, &LinkAction::Remove, current, patch, &matcher)?;
            (outcome, removed.unwrap_or_default())
        }
    };
    let relative = match &target {
        Target::Existing(existing) => existing.relative.clone(),
        Target::Dangling { relative } => relative.clone(),
    };
    let job = match (&target, field.inverse()) {
        (Target::Existing(existing), Some(inverse)) => Some(InverseJob {
            target: (**existing).clone(),
            field: field.name().to_owned(),
            inverse: inverse.to_owned(),
            kind: operation.op,
        }),
        _ => None,
    };
    Ok((
        LinkChangeToolResult {
            field: field.name().to_owned(),
            target: relative,
            link,
            outcome: outcome.into(),
        },
        job,
    ))
}

/// Applies every operation to the note's frontmatter in memory, resolving and
/// validating each target, without writing anything.
fn plan_forward(
    context: &Context<'_>,
    base: &GoverningBase,
    note: &NoteRef,
    operations: &[LinkOperationInput],
) -> Result<ForwardPlan, ToolError> {
    let mut current = note.document.frontmatter().clone();
    let mut patch = Map::new();
    let mut changes = Vec::with_capacity(operations.len());
    let mut jobs = Vec::new();
    for operation in operations {
        let (change, job) = plan_operation(context, base, note, operation, (&mut current, &mut patch))?;
        changes.push(change);
        jobs.extend(job);
    }
    Ok(ForwardPlan { patch, changes, jobs })
}

/// The reciprocal edits, grouped per linked note so each is written once,
/// with a report per reciprocal and a warning for each skipped for a reason
/// worth fixing.
struct ReciprocalPlan {
    edits: BTreeMap<String, LinkedNoteEdit>,
    reports: Vec<ReciprocalToolResult>,
    warnings: Vec<String>,
}

fn plan_reciprocals(context: &Context<'_>, note: &NoteRef, jobs: &[InverseJob]) -> ReciprocalPlan {
    let mut plan = ReciprocalPlan {
        edits: BTreeMap::new(),
        reports: Vec::with_capacity(jobs.len()),
        warnings: Vec::new(),
    };
    for job in jobs {
        let edit = plan
            .edits
            .entry(job.target.relative.clone())
            .or_insert_with(|| LinkedNoteEdit {
                current: job.target.document.frontmatter().clone(),
                patch: Map::new(),
                note: job.target.clone(),
            });
        let outcome = match plan_inverse(context, note, job, edit) {
            Ok(outcome) => LinkChangeOutcome::from(outcome),
            Err(Skip::Expected) => LinkChangeOutcome::Skipped,
            Err(Skip::Notable(reason)) => {
                plan.warnings.push(reason);
                LinkChangeOutcome::Skipped
            }
        };
        plan.reports.push(ReciprocalToolResult {
            path: job.target.relative.clone(),
            field: job.inverse.clone(),
            outcome,
            written: false,
        });
    }
    plan
}

/// Writes each linked note's edits, in order. A failure is a warning, not an
/// error: the note's own write has already completed.
fn write_reciprocals(
    mutations: &RoutedMutationService,
    plan: ReciprocalPlan,
    timestamp: &str,
) -> (Vec<ReciprocalToolResult>, Vec<String>) {
    let ReciprocalPlan {
        edits,
        mut reports,
        mut warnings,
    } = plan;
    for (relative, edit) in edits {
        if edit.patch.is_empty() {
            continue;
        }
        let written = stamped_content(&edit.note.document, edit.patch, timestamp).and_then(|content| {
            mutations
                .write_file(&WriteMutation {
                    path: edit.note.path.clone(),
                    content: content.into(),
                    expected_hash: Some(edit.note.hash.clone()),
                    force: false,
                    origin: Origin::Tool("frontmatter_link".to_owned()),
                })
                .map_err(ToolError::from)
        });
        match written {
            Ok(result) => {
                let WarningMessages(secondary) = WarningMessages::from(result.warnings);
                warnings.extend(secondary);
                for entry in reports.iter_mut().filter(|entry| entry.path == relative) {
                    entry.written = matches!(entry.outcome, LinkChangeOutcome::Added | LinkChangeOutcome::Removed);
                }
            }
            Err(error) => warnings.push(format!(
                "the reciprocal on {relative} was not written ({error}); repeat this call to retry it"
            )),
        }
    }
    (reports, warnings)
}

/// Refuses the call when the caller's `expected_hash` is not the note's
/// current hash, whether or not the note itself would change: a repeat call
/// that only repairs a reciprocal is held to the same conflict check.
fn ensure_current(note: &NoteRef, expected: Option<&ContentHash>) -> Result<(), ToolError> {
    match expected {
        Some(expected) if *expected != note.hash => {
            let path: &std::path::Path = (&note.path).into();
            Err(FsError::Conflict {
                path: path.to_path_buf(),
                current_hash: Some(note.hash.clone()),
            }
            .into())
        }
        _ => Ok(()),
    }
}

/// Edits the links of `request.note` and writes their reciprocals.
///
/// # Errors
///
/// Returns a typed error, with nothing written, when the operations are
/// empty or too many, the note cannot be read or is not a Markdown note, no
/// base governs it, a field is undeclared, a target cannot be found or would
/// be a self-link, a single-valued field is already set, or the note has
/// changed since `expected_hash`.
pub(crate) fn edit_links(
    filesystem: &Filesystem,
    mutations: &RoutedMutationService,
    request: &LinkRequest<'_>,
) -> Result<FrontmatterLinkToolResult, ToolError> {
    if request.operations.is_empty() {
        return Err(ToolError::Invalid("frontmatter_link requires at least one operation"));
    }
    if request.operations.len() > MAX_LINK_OPERATIONS {
        return Err(ToolError::Invalid(
            "frontmatter_link accepts at most 100 operations per call",
        ));
    }
    if !has_markdown_extension(&request.note.portable_relative()) {
        return Err(ToolError::Invalid(
            "frontmatter_link edits Markdown notes; path must end in .md",
        ));
    }

    // A folder only the server writes in is refused before anything is read or planned, whatever the note's links say.
    filesystem.ensure_not_server_owned(&request.note, &Origin::Tool("frontmatter_link".to_owned()))?;
    let source = filesystem.read_text(&ReadTextRequest {
        path: request.note.clone(),
        limit: None,
    })?;
    let note = NoteRef::new(&request.note, &source.content, source.content_hash)?;
    ensure_current(&note, request.expected_hash.as_ref())?;
    let context = Context::load(filesystem, request.roots, request.note.root_id())?;
    let base = context.governing(&note)?;

    // Everything that can be refused is decided here, before the first write.
    let ForwardPlan { patch, changes, jobs } = plan_forward(&context, &base, &note, &request.operations)?;
    let reciprocals = plan_reciprocals(&context, &note, &jobs);

    // The note's own write is the primary one: when it fails, nothing else
    // has been written.
    let mut content_hash = note.hash.clone();
    let mut warnings = Vec::new();
    if !patch.is_empty() {
        let result = mutations.write_file(&WriteMutation {
            path: note.path.clone(),
            content: stamped_content(&note.document, patch, request.timestamp)?.into(),
            expected_hash: Some(note.hash.clone()),
            force: false,
            origin: Origin::Tool("frontmatter_link".to_owned()),
        })?;
        content_hash = result.value.content_hash;
        let WarningMessages(secondary) = WarningMessages::from(result.warnings);
        warnings.extend(secondary);
    }

    let (reciprocal, reciprocal_warnings) = write_reciprocals(mutations, reciprocals, request.timestamp);
    warnings.extend(reciprocal_warnings);

    let hash: &str = (&content_hash).into();
    Ok(FrontmatterLinkToolResult {
        path: note.relative,
        content_hash: hash.to_owned(),
        changes,
        reciprocal,
        warnings,
    })
}
