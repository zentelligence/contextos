//! `base_record_create`: stamps a `.base`'s sidecar's next id/key onto a
//! newly created record note within the base's own scope, and advances the
//! sidecar's counters. See [`crate::tools::base_sidecar`] for the sidecar
//! codec this reuses.

use contextos_core::{
    ContentHash, Origin, PipelineResult, VaultPath, VaultPathInput, VaultSet, WriteMutation, WriteOutcome, WritesVault,
};
use contextos_fs::{FileInfoRequest, Filesystem, FsError};
use contextos_obsidian::{
    BASE_PROPERTY, BaseDocument, BaseLink, BaseLinkInput, BaseLinkTarget, BaseSidecar, BaseSidecarInput,
    BaseSidecarPatch, FrontmatterDocument, FrontmatterInput, NoteCreateInput, NoteDocument, QueryDefinition,
    QueryFormat, records_folder,
};
use serde_json::{Map, Value};

use crate::server::WarningMessages;
use crate::tool_error::ToolError;
use crate::tools::base_record_template::{EVERY_SERIES, TemplateReader, TemplateSpec, TemplateVault};
use crate::tools::note_entity::NoteEntities;
use crate::tools::{base_query, base_sidecar};

/// Inputs for one `base_record_create` call, already validated into vault
/// paths and plain values by the tool handler.
pub(crate) struct RecordCreateRequest<'a> {
    pub(crate) base_path: VaultPath,
    pub(crate) roots: &'a VaultSet,
    pub(crate) frontmatter: Map<String, Value>,
    pub(crate) content: String,
    pub(crate) folder: Option<&'a str>,
    pub(crate) filename: Option<&'a str>,
    /// The key series to assign the id from; `None` selects the sidecar's
    /// default series.
    pub(crate) prefix: Option<&'a str>,
    /// Fields whose values together must not already be held by a record in the base's scope.
    pub(crate) unique_fields: Option<UniqueFields>,
    pub(crate) timestamp: &'a str,
}

/// The most fields one uniqueness key may name.
const MAX_UNIQUE_FIELDS: usize = 8;

/// The frontmatter fields whose values together identify a record: 1 to [`MAX_UNIQUE_FIELDS`] distinct names, each an
/// identifier of ASCII letters, digits, and underscores starting with a letter, at most 64 characters, so a name can
/// be placed in a filter expression without quoting.
pub(crate) struct UniqueFields(Vec<String>);

impl TryFrom<Vec<String>> for UniqueFields {
    type Error = ToolError;

    fn try_from(fields: Vec<String>) -> Result<Self, Self::Error> {
        if fields.is_empty() || fields.len() > MAX_UNIQUE_FIELDS {
            return Err(ToolError::Invalid(
                "base_record_create unique_fields must name 1 to 8 fields",
            ));
        }
        let well_formed = |field: &String| {
            field.len() <= 64
                && field.starts_with(|first: char| first.is_ascii_alphabetic())
                && field
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        };
        if !fields.iter().all(well_formed) {
            return Err(ToolError::Invalid(
                "base_record_create unique_fields names must be letters, digits, and underscores, starting with a letter",
            ));
        }
        let mut seen = std::collections::HashSet::new();
        if !fields.iter().all(|field| seen.insert(field.as_str())) {
            return Err(ToolError::Invalid(
                "base_record_create unique_fields must not repeat a field",
            ));
        }
        Ok(Self(fields))
    }
}

/// Refuses the record when one already in the base's scope holds the same value for every field in `unique`, naming
/// that record. Runs before any id is reserved, inside the vault's write lock, so nothing is written for a refused
/// record and two raisers cannot both pass the check.
pub(crate) fn reject_duplicate_record(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root_id: contextos_core::VaultRootId,
    base_document: &BaseDocument,
    frontmatter: &Map<String, Value>,
    unique: &UniqueFields,
) -> Result<(), ToolError> {
    let mut clauses = Vec::with_capacity(unique.0.len());
    for field in &unique.0 {
        let value = frontmatter.get(field).ok_or(ToolError::Invalid(
            "every field named in base_record_create unique_fields must be set in frontmatter",
        ))?;
        if !(value.is_string() || value.is_number() || value.is_boolean()) {
            return Err(ToolError::Invalid(
                "a field named in base_record_create unique_fields must hold a string, number, or boolean",
            ));
        }
        let literal =
            serde_json::to_string(value).map_err(|_| ToolError::Invalid("unique field value must be JSON"))?;
        clauses.push(format!("{field} == {literal}"));
    }
    let outcome = query_scope(
        filesystem,
        roots,
        root_id,
        base_document,
        Some(clauses.join(" && ")),
        &["id", "file.path"],
    )?;
    let rows: Vec<Map<String, Value>> = serde_json::from_str(&outcome.content)
        .map_err(|_| ToolError::Invalid("base_record_create could not read its own uniqueness query"))?;
    if let Some(existing) = rows.first() {
        return Err(ToolError::DuplicateRecord {
            id: existing
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("A record without an id")
                .to_owned(),
            path: existing
                .get("file.path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            fields: unique.0.join(" and "),
        });
    }
    Ok(())
}

/// `base_record_create`'s result: the new record note's own outcome, the
/// key assigned (absent for a `FILEPATH`-keyed base), and the sidecar's
/// path plus any warnings raised advancing it.
pub(crate) struct RecordCreateOutcome {
    pub(crate) note_result: PipelineResult<WriteOutcome>,
    pub(crate) key: Option<String>,
    pub(crate) sidecar_path: String,
    pub(crate) sidecar_warnings: Vec<String>,
}

/// The `base` property value to stamp on a record created through the base at `base_path`, or `None` when the
/// caller already supplied a link to that same base (kept as written). A `base` naming any other base, or not a
/// link at all, is refused rather than repaired.
fn base_property_stamp(frontmatter: &Map<String, Value>, base_path: &VaultPath) -> Result<Option<String>, ToolError> {
    let path = base_path.portable_relative();
    match frontmatter.get(BASE_PROPERTY) {
        None => Ok(Some(
            BaseLink::try_from(BaseLinkInput { path: &path })?.as_ref().to_owned(),
        )),
        Some(Value::String(supplied)) => match BaseLinkTarget::try_from(supplied.as_str()) {
            Ok(target) if target.as_str() == path => Ok(None),
            _ => Err(ToolError::Invalid(
                "base, when supplied, must be a link to the base the record is created in; omit it to have it stamped",
            )),
        },
        Some(_) => Err(ToolError::Invalid("base, when supplied, must be a string")),
    }
}

/// Resolves `base_record_create`'s target folder: a leading `/` on `folder`
/// makes it vault-root-relative directly, ignoring the base's own folder
/// entirely; otherwise `folder` (or nothing) is measured relative to the
/// folder the `.base`'s own top-level filters confine its records to
/// ([`records_folder`]: `file.inFolder("...")` or `file.folder == "..."`),
/// which must exist in that case.
fn resolve_target_folder(definition: &Map<String, Value>, folder: Option<&str>) -> Result<String, ToolError> {
    if let Some(folder) = folder.and_then(|folder| folder.strip_prefix('/')) {
        return Ok(folder.to_owned());
    }
    let base_folder = records_folder(definition.get("filters")).ok_or(ToolError::Invalid(
        "base_record_create needs the .base's own filters to name its records' folder, with \
         file.inFolder(\"...\") or file.folder == \"...\", when folder is relative or omitted; \
         pass an absolute /-prefixed folder instead",
    ))?;
    match folder {
        // `file.folder ==` selects only the notes directly in its folder, so a record placed beneath it would not
        // be one of the base's records at all.
        Some(_) if !base_folder.includes_subfolders => Err(ToolError::Invalid(
            "base_record_create cannot place a record in a sub-folder of a .base whose filters select only the \
             notes directly in its folder (file.folder == \"...\"); omit folder, or pass an absolute /-prefixed one",
        )),
        Some(folder) => Ok(format!("{}/{folder}", base_folder.path)),
        None => Ok(base_folder.path.to_owned()),
    }
}

/// A sidecar as found on disk: its parsed key scheme and, when the file
/// already exists, the whole parsed document (so every frontmatter key and
/// the body survive a rewrite) with its content hash for conflict-checking
/// the advance write.
pub(crate) struct SidecarState {
    pub(crate) sidecar: BaseSidecar,
    pub(crate) existing: Option<ExistingSidecar>,
}

pub(crate) struct ExistingSidecar {
    pub(crate) document: FrontmatterDocument,
    pub(crate) content_hash: ContentHash,
}

/// Reads the sidecar. When none exists yet, a `FILEPATH`-keyed placeholder is
/// returned with no `existing` file, and `base_record_create` creates one
/// proactively, matching `base_create`'s own proactive-sidecar behaviour.
pub(crate) fn read_sidecar(filesystem: &Filesystem, sidecar_path: &VaultPath) -> Result<SidecarState, ToolError> {
    if let Some(existing) = contextos_core::ReadsVault::read_optional_text(filesystem, sidecar_path)? {
        let document = FrontmatterDocument::try_from(existing.content.as_str())?;
        let sidecar = BaseSidecar::try_from(document.frontmatter())?;
        Ok(SidecarState {
            sidecar,
            existing: Some(ExistingSidecar {
                document,
                content_hash: existing.content_hash,
            }),
        })
    } else {
        let sidecar = BaseSidecar::try_from(BaseSidecarInput {
            key_prefix: None,
            key_prefixes: &[],
            timestamp: "",
        })?;
        Ok(SidecarState {
            sidecar,
            existing: None,
        })
    }
}

/// A key, read against the series it should belong to.
struct SeriesKey<'a> {
    prefix: &'a str,
    key: &'a str,
}

/// The number in a key of a series: the `1000` in `REQ-1000`, whatever its zero padding.
struct KeyNumber(u64);

/// A key that is not `<prefix>-<digits>` for the series asked about, or whose number no counter could hold.
struct NotInSeries;

impl TryFrom<SeriesKey<'_>> for KeyNumber {
    type Error = NotInSeries;

    fn try_from(value: SeriesKey<'_>) -> Result<Self, NotInSeries> {
        let digits = value
            .key
            .strip_prefix(value.prefix)
            .and_then(|rest| rest.strip_prefix('-'))
            .ok_or(NotInSeries)?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(NotInSeries);
        }
        digits.parse().map(Self).map_err(|_| NotInSeries)
    }
}

/// Runs an inline query over the notes among which `base_document`'s ids must be unique
/// ([`base_query::identity_scope`]: the folder its filters name, `index` notes aside, else the notes its filters
/// select), the same scope `doctor` checks ids in, so a filter that hides closed records never frees their ids and a
/// `file.backlinks` filter never needs the link graph here; a base with no filters covers the whole vault.
/// Reuses `base_query`'s own scan and filter engine rather than a second, separately maintained frontmatter walk.
/// `extra_filter` narrows it further.
fn query_scope(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root_id: contextos_core::VaultRootId,
    base_document: &BaseDocument,
    extra_filter: Option<String>,
    columns: &[&str],
) -> Result<base_query::QueryOutcome, ToolError> {
    let scope = base_query::identity_scope(base_document.definition().get("filters"));
    let mut definition = Map::new();
    let filters = match (scope, extra_filter) {
        (Some(scope), Some(extra)) => Some(Value::Object(Map::from_iter([(
            "and".to_owned(),
            Value::Array(vec![scope, Value::String(extra)]),
        )]))),
        (Some(only), None) => Some(only),
        (None, Some(only)) => Some(Value::String(only)),
        (None, None) => None,
    };
    if let Some(filters) = filters {
        definition.insert("filters".to_owned(), filters);
    }
    definition.insert(
        "order".to_owned(),
        Value::Array(
            columns
                .iter()
                .map(|column| Value::String((*column).to_owned()))
                .collect(),
        ),
    );
    let definition = QueryDefinition::from_inline(&definition)?;
    base_query::run(filesystem, roots, root_id, &definition, QueryFormat::Json, &[])
}

/// Whether `id` already exists anywhere within `base_document`'s own declared scope. `id` uniqueness is otherwise
/// only ever enforced by the discipline of always going through `base_record_create`, which is exactly how a
/// hand-authored or otherwise out-of-band note can reuse an id the sidecar's own sequence already assigned (or will
/// assign next).
fn id_exists_in_scope(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root_id: contextos_core::VaultRootId,
    base_document: &BaseDocument,
    id: &str,
) -> Result<bool, ToolError> {
    let id_literal = serde_json::to_string(id).map_err(|_| ToolError::Invalid("id must be representable as JSON"))?;
    let outcome = query_scope(
        filesystem,
        roots,
        root_id,
        base_document,
        Some(format!("id == {id_literal}")),
        &["id"],
    )?;
    Ok(outcome.matched > 0)
}

/// Rejects a caller-supplied `id` that already exists in the base's scope. A caller-supplied id is never replaced
/// with another, so a collision cannot be stepped past the way a server-assigned key can.
fn reject_if_id_exists(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root_id: contextos_core::VaultRootId,
    base_document: &BaseDocument,
    id: &str,
) -> Result<(), ToolError> {
    if id_exists_in_scope(filesystem, roots, root_id, base_document, id)? {
        return Err(ToolError::Invalid(
            "base_record_create refuses to write a note whose id already exists elsewhere in this base's scope; \
             choose a different id or correct the existing duplicate first",
        ));
    }
    Ok(())
}

/// The highest number any note in the base's scope holds under the series `prefix`, or zero when none does. Read
/// only after the series' next key was found already taken, which means a note was written without going through
/// this tool, so it is not the normal path.
///
/// Refuses, rather than guessing, when the scope holds more notes than one query can return: the highest key could
/// then be among those left out.
fn highest_assigned_number(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root_id: contextos_core::VaultRootId,
    base_document: &BaseDocument,
    prefix: &str,
) -> Result<u64, ToolError> {
    let outcome = query_scope(filesystem, roots, root_id, base_document, None, &["id"])?;
    if outcome.truncated {
        return Err(ToolError::Invalid(
            "base_record_create cannot read back every id in this base's scope to catch its counter up; \
             set the series counter in the sidecar beyond the highest existing key",
        ));
    }
    let rows: Vec<Map<String, Value>> = serde_json::from_str(&outcome.content)
        .map_err(|_| ToolError::Invalid("base_record_create could not read back the ids in this base's scope"))?;
    Ok(rows
        .iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str))
        .filter_map(|key| KeyNumber::try_from(SeriesKey { prefix, key }).ok())
        .map(|number| number.0)
        .max()
        .unwrap_or(0))
}

fn resolve_filename(filename: Option<&str>, key: Option<&str>) -> Result<String, ToolError> {
    if let Some(filename) = filename {
        let has_md_extension = std::path::Path::new(filename)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
        if !has_md_extension {
            return Err(ToolError::Invalid("base_record_create filename must end in .md"));
        }
        return Ok(filename.to_owned());
    }
    let key = key.ok_or(ToolError::Invalid(
        "base_record_create requires an explicit filename for a FILEPATH-keyed base (it has no id to name one from)",
    ))?;
    Ok(format!("{}.md", key.to_lowercase()))
}

fn resolve_title(frontmatter: &Map<String, Value>, filename: &str) -> Option<String> {
    frontmatter
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| filename.strip_suffix(".md").map(|stem| stem.replace(['-', '_'], " ")))
}

/// Renders the advanced sidecar. An existing sidecar is patched in place
/// (RFC 7386), so a legacy single-series one is migrated to the
/// `key_prefixes` map form while every other key, such as a `template` link,
/// and the body are preserved; a missing one is created fresh.
pub(crate) fn render_sidecar_content(
    advanced: &BaseSidecar,
    existing: Option<FrontmatterDocument>,
    base_stem: &str,
) -> Result<String, ToolError> {
    let content = if let Some(mut document) = existing {
        let BaseSidecarPatch(patch) = BaseSidecarPatch::from(advanced);
        document.merge_frontmatter(patch);
        String::try_from(&document)?
    } else {
        let document = FrontmatterDocument::from(FrontmatterInput {
            frontmatter: Map::from(advanced),
            body: format!("![[{base_stem}.base|{base_stem}]]\n"),
        });
        String::try_from(&document)?
    };
    Ok(content)
}

/// The key a new record is assigned and the sidecar that assigns it, after any catch-up.
struct SettledKey {
    sidecar: BaseSidecar,
    /// `None` for a `FILEPATH`-keyed base, which assigns no id.
    key: Option<String>,
    catch_up_warning: Option<String>,
}

/// Settles the key for a new record, refusing anything that would reuse an id already in the base's scope.
///
/// The sidecar's counter decides the key. Only when that key turns out to be taken, which means a note holding it
/// was written without going through this tool, is the counter caught up to the highest key the series has in
/// scope. The sidecar stays the source of truth on every other call, and the one-key membership check is all the
/// scope scan the normal path pays for.
fn settle_key(
    filesystem: &Filesystem,
    request: &RecordCreateRequest<'_>,
    base_document: &BaseDocument,
    sidecar: BaseSidecar,
) -> Result<SettledKey, ToolError> {
    let mut key = sidecar.next_key(request.prefix)?;

    // A `Sequential` scheme assigns its own id; a caller-supplied one would silently be discarded (or contradict
    // it), so it is rejected only in that case. A `FILEPATH`-keyed base (`key` is `None`) has no id to assign
    // itself, so a caller-supplied one is accepted as an ordinary optional frontmatter field, not required.
    if key.is_some() && request.frontmatter.contains_key("id") {
        return Err(ToolError::Invalid(
            "base_record_create assigns id itself; omit it from frontmatter",
        ));
    }
    let caller_id = match request.frontmatter.get("id") {
        Some(Value::String(id)) => Some(id.as_str()),
        Some(_) => return Err(ToolError::Invalid("id, when supplied, must be a string")),
        None => None,
    };

    let root_id = request.base_path.root_id();
    let mut sidecar = sidecar;
    let mut catch_up_warning = None;
    let series = sidecar.series_prefix(request.prefix)?.map(str::to_owned);
    if let (Some(candidate), Some(series)) = (key.clone(), series) {
        if id_exists_in_scope(filesystem, request.roots, root_id, base_document, &candidate)? {
            let highest = highest_assigned_number(filesystem, request.roots, root_id, base_document, &series)?;
            // No number is left above the largest one a counter can hold, and stepping past it would wrap onto it.
            if highest == u64::MAX {
                return Err(ToolError::Invalid(
                    "base_record_create cannot step past a key holding the largest number a counter can hold; \
                     correct that note's id first",
                ));
            }
            sidecar = sidecar.caught_up_to(request.prefix, highest)?;
            key = sidecar.next_key(request.prefix)?;
            tracing::warn!(
                series = %series,
                caught_up_to = highest,
                "a key series counter was behind the records in its base"
            );
            catch_up_warning = Some(format!(
                "The {series} counter was behind the records in this base: {candidate} already existed, so the \
                 counter was caught up to {highest}, the highest {series} number found in scope. A note holds a \
                 {series} key the counter never counted: it was written by hand or by another tool."
            ));
        }
    } else if let Some(id) = caller_id {
        reject_if_id_exists(filesystem, request.roots, root_id, base_document, id)?;
    }
    Ok(SettledKey {
        sidecar,
        key,
        catch_up_warning,
    })
}

/// How many times a reservation is attempted against a sidecar another process keeps changing before the call
/// gives up with the conflict.
const RESERVATION_ATTEMPTS: usize = 3;

/// What the reservation step decided, and where the note will go.
struct Reservation {
    /// `None` for a `FILEPATH`-keyed base, which assigns no id.
    key: Option<String>,
    note_path: VaultPath,
    /// The note, fully rendered and validated before the key was reserved, so writing it cannot be refused for the
    /// caller's own input.
    note: String,
    /// Warnings from the sidecar write, led by the catch-up warning when the counter was behind.
    sidecar_warnings: Vec<String>,
}

/// What the reservation step needs to know about the base a record is created in.
struct ReservationTarget<'a> {
    base_document: &'a BaseDocument,
    /// The folder the note is created in, relative to the vault root.
    folder: &'a str,
    sidecar_path: &'a VaultPath,
    base_stem: &'a str,
    /// The `base` link to stamp on the note, or `None` when the caller already supplied it.
    base_stamp: Option<&'a str>,
}

/// Where a record note will be written: `filename` inside `folder`, as a validated vault path.
fn note_path_in(request: &RecordCreateRequest<'_>, folder: &str, filename: &str) -> Result<VaultPath, ToolError> {
    let relative = if folder.is_empty() {
        filename.to_owned()
    } else {
        format!("{folder}/{filename}")
    };
    let root = request
        .roots
        .root(request.base_path.root_id())
        .ok_or(ToolError::Invalid("base vault root is not configured"))?;
    let absolute = root.path().join(relative);
    let raw = absolute
        .to_str()
        .ok_or(ToolError::Invalid("record note path must be valid UTF-8"))?;
    Ok(VaultPath::try_from(VaultPathInput {
        roots: request.roots,
        raw,
    })?)
}

/// What one record note is rendered with, beside the caller's own request.
#[derive(Clone, Copy)]
struct RecordNote<'a> {
    /// The record's key; `None` for a `FILEPATH`-keyed base.
    key: Option<&'a str>,
    filename: &'a str,
    base_stamp: Option<&'a str>,
    /// The `entity` the note's location calls for; `None` when the vault's entities are unknown.
    entity: Option<&'a str>,
    /// The caller's frontmatter with the series template's fixed fields stamped in.
    frontmatter: &'a Map<String, Value>,
}

/// Renders and validates a record note, so anything the caller's content, title, or frontmatter gets wrong is refused
/// before a key is reserved for it.
fn render_note(request: &RecordCreateRequest<'_>, record: &RecordNote<'_>) -> Result<String, ToolError> {
    let RecordNote {
        key,
        filename,
        base_stamp,
        entity,
        frontmatter,
    } = *record;
    let title = resolve_title(frontmatter, filename)
        .ok_or(ToolError::Invalid("record note path must have a UTF-8 filename"))?;
    let mut frontmatter = frontmatter.clone();
    if let Some(stamp) = base_stamp {
        frontmatter.insert(BASE_PROPERTY.to_owned(), Value::String(stamp.to_owned()));
    }
    if let Some(key) = key {
        frontmatter.insert("id".to_owned(), Value::String(key.to_owned()));
    }
    let note = NoteDocument::try_from(NoteCreateInput {
        title: &title,
        frontmatter,
        content: &request.content,
        timestamp: request.timestamp,
        entity,
    })?;
    Ok(String::try_from(note)?)
}

/// The caller's frontmatter with the fixed fields of the template the sidecar declares for the record's series stamped
/// in, or the caller's frontmatter unchanged when the series follows no template.
///
/// Refuses, before any key is reserved, a template that cannot be used and a supplied value that differs from a fixed
/// one. A template is read on every attempt, so a record is stamped from the template as it stands then.
fn stamp_fixed_fields(
    filesystem: &Filesystem,
    request: &RecordCreateRequest<'_>,
    sidecar: &BaseSidecar,
    sidecar_path: &VaultPath,
) -> Result<Map<String, Value>, ToolError> {
    let series = sidecar.series_prefix(request.prefix)?;
    let declaration = sidecar.series_templates();
    let Some(link) = declaration.template_for(series) else {
        return Ok(request.frontmatter.clone());
    };
    let label = series.map_or_else(|| EVERY_SERIES.to_owned(), |series| format!("series {series}"));
    let root = request
        .roots
        .root(request.base_path.root_id())
        .ok_or(ToolError::Invalid("base vault root is not configured"))?;
    let reader = TemplateReader::try_from(TemplateVault {
        filesystem,
        roots: request.roots,
        name: root.name(),
    })?;
    let sidecar_note = sidecar_path.portable_relative();
    let template = reader
        .read(TemplateSpec {
            sidecar: &sidecar_note,
            link: link.as_ref(),
            fixed_fields: declaration.fixed_fields(),
        })
        .map_err(|problem| ToolError::SeriesTemplate {
            series: label,
            link: link.as_ref().to_owned(),
            problem,
        })?;
    let conflicts = template.conflicting_fields(&request.frontmatter);
    if !conflicts.is_empty() {
        return Err(ToolError::FixedFieldConflict {
            fields: conflicts,
            series: series.unwrap_or(EVERY_SERIES).to_owned(),
            template: template.path,
        });
    }
    Ok(template.stamped(&request.frontmatter))
}

/// Reserves the record's key by advancing the sidecar's counter, before the note is written.
///
/// The sidecar is the one file every process creating records in a base has to write, and the write is conflict
/// checked against the sidecar as it was read, so it serialises allocation across processes without a lock. When
/// another process changed the sidecar in between, the sidecar is read again and the key settled afresh, up to
/// [`RESERVATION_ATTEMPTS`] times. A note that cannot then be written leaves a gap in the series, never an orphaned
/// note behind a counter that did not move.
fn reserve_key<W: WritesVault<Error = FsError>>(
    filesystem: &Filesystem,
    writes: &W,
    request: &RecordCreateRequest<'_>,
    target: &ReservationTarget<'_>,
) -> Result<Reservation, ToolError> {
    // The record note lands in the base's own vault, so that vault's entities are read once for every attempt.
    let entities = NoteEntities::read(filesystem, request.roots, &request.base_path)?;
    for attempt in 1..=RESERVATION_ATTEMPTS {
        let SidecarState { sidecar, existing } = read_sidecar(filesystem, target.sidecar_path)?;
        let SettledKey {
            sidecar,
            key,
            catch_up_warning,
        } = settle_key(filesystem, request, target.base_document, sidecar)?;
        let note_filename = resolve_filename(request.filename, key.as_deref())?;
        let note_path = note_path_in(request, target.folder, &note_filename)?;
        // Refused before anything is reserved, so the commonest reason a note cannot be written leaves no gap.
        match filesystem.file_info(&FileInfoRequest {
            path: note_path.clone(),
        }) {
            Ok(_) => {
                return Err(ToolError::Invalid(
                    "base_record_create will not overwrite an existing note; choose a different filename",
                ));
            }
            Err(FsError::NotFound { .. }) => {}
            Err(error) => return Err(error.into()),
        }

        let frontmatter = stamp_fixed_fields(filesystem, request, &sidecar, target.sidecar_path)?;
        let entity = entities.entity_of(&note_path);
        let note = render_note(
            request,
            &RecordNote {
                key: key.as_deref(),
                filename: &note_filename,
                base_stamp: target.base_stamp,
                entity: entity.as_deref(),
                frontmatter: &frontmatter,
            },
        )?;

        let advanced = sidecar.advance(request.prefix, request.timestamp)?;
        let (existing_document, existing_hash) = match existing {
            Some(ExistingSidecar { document, content_hash }) => (Some(document), Some(content_hash)),
            None => (None, None),
        };
        let content = render_sidecar_content(&advanced, existing_document, target.base_stem)?;
        let written = writes.persist(&WriteMutation {
            path: target.sidecar_path.clone(),
            content: content.into(),
            expected_hash: existing_hash,
            force: false,
            origin: Origin::Tool("base_record_create".to_owned()),
        });
        match written {
            Ok(result) => {
                let WarningMessages(mut sidecar_warnings) = WarningMessages::from(result.warnings);
                if let Some(warning) = catch_up_warning {
                    sidecar_warnings.insert(0, warning);
                }
                return Ok(Reservation {
                    key,
                    note_path,
                    note,
                    sidecar_warnings,
                });
            }
            // Another process changed, or created, the sidecar after it was read.
            Err(FsError::Conflict { .. } | FsError::ExpectedHashRequired { .. }) if attempt < RESERVATION_ATTEMPTS => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(ToolError::Invalid(
        "base_record_create could not reserve a key: the sidecar kept changing",
    ))
}

/// Creates the record note after reserving its key in the sidecar, in that order.
///
/// Reserving first is what makes the counter safe to trust: the key is claimed with a conflict-checked write of
/// the sidecar, so two processes can never claim the same one, and a failure partway leaves a skipped number
/// rather than a note whose counter never moved. Both writes are ordinary `Origin::Tool` mutations, so
/// `memory/log/`, Git staging, and the text/search indexes update automatically through the existing routing
/// policy for each.
///
/// # Errors
///
/// Returns a typed validation, path, or persistence error. A sidecar that keeps changing under the call fails with
/// its conflict and nothing written. A note that cannot be written after its key was reserved fails with
/// [`ToolError::KeyReservedNoteNotWritten`], naming the key, which the series now skips.
pub(crate) fn create_record<W: WritesVault<Error = FsError>>(
    filesystem: &Filesystem,
    writes: &W,
    request: &RecordCreateRequest<'_>,
) -> Result<RecordCreateOutcome, ToolError> {
    let base_source = filesystem.read_text(&contextos_fs::ReadTextRequest {
        path: request.base_path.clone(),
        limit: None,
    })?;
    let base_document = BaseDocument::try_from(base_source.content.as_str())?;
    let folder = resolve_target_folder(base_document.definition(), request.folder)?;
    let sidecar_path = base_sidecar::sidecar_path(request.roots, &request.base_path)?;
    let base_stem = request
        .base_path
        .relative()
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or(ToolError::Invalid("base path must have a UTF-8 filename"))?;
    let base_stamp = base_property_stamp(&request.frontmatter, &request.base_path)?;
    if let Some(unique) = &request.unique_fields {
        reject_duplicate_record(
            filesystem,
            request.roots,
            request.base_path.root_id(),
            &base_document,
            &request.frontmatter,
            unique,
        )?;
    }

    let Reservation {
        key,
        note_path,
        note,
        sidecar_warnings,
    } = reserve_key(
        filesystem,
        writes,
        request,
        &ReservationTarget {
            base_document: &base_document,
            folder: &folder,
            sidecar_path: &sidecar_path,
            base_stem,
            base_stamp: base_stamp.as_deref(),
        },
    )?;

    let note_result = writes
        .persist(&WriteMutation {
            path: note_path,
            content: note.into(),
            expected_hash: None,
            force: false,
            origin: Origin::Tool("base_record_create".to_owned()),
        })
        .map_err(|source| match &key {
            Some(key) => ToolError::KeyReservedNoteNotWritten {
                key: key.clone(),
                source: Box::new(source),
            },
            None => ToolError::from(source),
        })?;

    Ok(RecordCreateOutcome {
        note_result,
        key,
        sidecar_path: sidecar_path.portable_relative(),
        sidecar_warnings,
    })
}

#[cfg(test)]
#[path = "base_record_test.rs"]
mod tests;
