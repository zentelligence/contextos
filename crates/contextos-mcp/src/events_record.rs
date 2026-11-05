//! Recording an event of a declared kind through `base_record_create`.
//!
//! The vault's `memory/events.base`, when its sidecar names a kind registry (`event_kinds`), is the one events base. A
//! record created in it is not an
//! ordinary keyed note: the request is validated against its kind's definition, `at` and the path are stamped by the
//! server, and a key is assigned only when the kind declares a key field, from the series the request names and the sidecar's
//! `series_kinds` maps to that kind. Every refusal
//! happens before a key is reserved or anything is written. The note itself is an `Origin::Internal` write, so Git
//! follows it and neither the managed index nor the operation log does, exactly as for an event the server fires.

use contextos_core::{
    ContentHash, Origin, PipelineResult, VaultPath, VaultPathInput, VaultSet, WriteMutation, WritesVault,
};
use contextos_events::{
    EventsError, FieldRejection, KindDefinition, RecordContext, RecordRequest, SubjectNote, SubjectRecord,
    SubjectRejection,
};
use contextos_fs::FileNameIndex;
use contextos_fs::{Filesystem, FsError, ReadTextRequest};
use contextos_obsidian::{
    BASE_PROPERTY, BaseDocument, BaseLinkTarget, FrontmatterDocument, LinkResolution, LinkTarget, resolve_link,
};
use serde_json::{Map, Value};
use time::OffsetDateTime;

use crate::event_kinds::{self, SidecarSeries};
use crate::events_writer::{EventWriteError, EventWriter};
use crate::server::WarningMessages;
use crate::tool_error::ToolError;
use crate::tools::base_record::{
    RecordCreateOutcome, RecordCreateRequest, SidecarState, read_sidecar, reject_duplicate_record,
    render_sidecar_content,
};
use crate::tools::base_sidecar;
use crate::tools::fs::child_path;
use crate::vault_events_scope::{TASKS_BASE, TaskScope};

/// How many times a reservation is attempted against a sidecar another process keeps changing.
const RESERVATION_ATTEMPTS: usize = 3;

/// The sidecar key that names the kind registry folder.
const EVENT_KINDS_KEY: &str = "event_kinds";

/// The kind registry folder the base's sidecar names, when the base is the vault's events base. `event_kinds` on any
/// other base's sidecar names nothing: such a base is an ordinary keyed base.
///
/// # Errors
///
/// Returns the filesystem or frontmatter error from reading a sidecar that exists.
pub(crate) fn kind_registry_folder(
    filesystem: &Filesystem,
    roots: &VaultSet,
    base_path: &VaultPath,
) -> Result<Option<String>, ToolError> {
    if base_path.portable_relative() != crate::events_folder::EVENTS_BASE {
        return Ok(None);
    }
    let sidecar_path = base_sidecar::sidecar_path(roots, base_path)?;
    let Some(sidecar) = contextos_core::ReadsVault::read_optional_text(filesystem, &sidecar_path)? else {
        return Ok(None);
    };
    let Ok(document) = FrontmatterDocument::try_from(sidecar.content.as_str()) else {
        return Ok(None);
    };
    Ok(document
        .frontmatter()
        .get(EVENT_KINDS_KEY)
        .and_then(Value::as_str)
        .map(|folder| folder.trim().trim_matches('/').to_owned())
        .filter(|folder| !folder.is_empty()))
}

/// The sidecar key that maps each key series to the kind whose key it assigns.
const SERIES_KINDS_KEY: &str = "series_kinds";

/// The kind each key series is mapped to by the sidecar's `series_kinds`. An entry that is not a series naming a kind
/// is left out, so the kind it meant is reported as unmapped.
pub(crate) fn sidecar_series(state: &SidecarState) -> SidecarSeries {
    let declared: Vec<&str> = state
        .sidecar
        .series()
        .iter()
        .map(contextos_obsidian::KeySeries::prefix)
        .collect();
    let kinds = state
        .existing
        .as_ref()
        .and_then(|existing| existing.document.frontmatter().get(SERIES_KINDS_KEY))
        .and_then(Value::as_object)
        .map(|mapped| {
            mapped
                .iter()
                .filter(|(series, _)| declared.contains(&series.as_str()))
                .filter_map(|(series, kind)| Some((series.clone(), kind.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    SidecarSeries { kinds }
}

/// The vault's root as a path, from any path inside it.
fn vault_root(roots: &VaultSet, inside: &VaultPath) -> Result<VaultPath, ToolError> {
    let root = roots
        .root(inside.root_id())
        .ok_or(ToolError::Invalid("base vault root is not configured"))?;
    Ok(VaultPath::try_from(VaultPathInput {
        roots,
        raw: &format!("{}://.", root.name()),
    })?)
}

fn invalid_value(kind: Option<&str>, field: &str) -> EventsError {
    EventsError::InvalidFields {
        kind: kind.unwrap_or_default().to_owned(),
        field: field.to_owned(),
        reason: FieldRejection::InvalidValue,
    }
}

/// Splits the caller's frontmatter into the common event fields and the kind's own fields.
fn event_request(frontmatter: &Map<String, Value>, content: &str) -> Result<RecordRequest, EventsError> {
    let mut fields = frontmatter.clone();
    let kind = fields.remove("kind");
    let kind_name = kind.as_ref().and_then(Value::as_str).map(str::to_owned);
    let mut text = |name: &str| -> Result<Option<String>, EventsError> {
        match fields.remove(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            Some(_) => Err(invalid_value(kind_name.as_deref(), name)),
        }
    };
    let (severity, subject, entity) = (text("severity")?, text("subject")?, text("entity")?);
    // `type` is the template's fixed field: saying it again is harmless, contradicting it is not.
    match fields.remove("type") {
        None => {}
        Some(Value::String(declared)) if declared == "event" => {}
        Some(_) => return Err(invalid_value(kind_name.as_deref(), "type")),
    }
    if matches!(kind, Some(ref value) if !value.is_string() && !value.is_null()) {
        return Err(invalid_value(None, "kind"));
    }
    Ok(RecordRequest {
        kind: kind_name,
        severity,
        subject,
        entity,
        content: Some(content.to_owned()).filter(|text| !text.trim().is_empty()),
        fields,
    })
}

/// The note an event's subject link names, read for what the event stamps: its path, its key and base when it is a
/// record of a keyed base, and its `entity`.
pub(crate) struct ResolvedSubject {
    note: SubjectNote,
    entity: Option<String>,
}

/// What a recorded event's subject link names, looked up before the vault's write lock is taken so a name lookup never
/// holds up other writers.
pub(crate) enum SubjectLookup {
    /// The request is not an event with a subject link, or the link names no note.
    Unnamed,
    /// The one note the link names.
    Found(ResolvedSubject),
    /// More than one note answers to the link.
    Ambiguous,
}

/// Where a subject link is resolved from: the events folder, as a link written in an event note is.
const SUBJECT_LINK_SOURCE: &str = "memory/events/event.md";

/// Looks up the subject of a request to record an event in the events base at `base_path`, as every link is resolved:
/// nearest the events folder first, then by name through the vault's shared file name index, which is walked again
/// once when a name is not in it. A record's base is the task base for a task note that base selects, else its `base`
/// property, so a recorded event about a task is the same subject as the events the server fires about it.
///
/// # Errors
///
/// Returns the filesystem error of a vault that could not be asked.
pub(crate) fn look_up_subject(
    filesystem: &Filesystem,
    roots: &VaultSet,
    names: &FileNameIndex,
    base_path: &VaultPath,
    frontmatter: &Map<String, Value>,
) -> Result<SubjectLookup, ToolError> {
    if base_path.portable_relative() != crate::events_folder::EVENTS_BASE {
        return Ok(SubjectLookup::Unnamed);
    }
    let Some(link) = frontmatter.get("subject").and_then(Value::as_str) else {
        return Ok(SubjectLookup::Unnamed);
    };
    let Ok(target) = LinkTarget::try_from(link) else {
        return Ok(SubjectLookup::Unnamed);
    };
    let root = vault_root(roots, base_path)?;
    let Some(vault) = roots
        .root(root.root_id())
        .map(|configured| configured.name().to_owned())
    else {
        return Ok(SubjectLookup::Unnamed);
    };
    let targets = crate::tools::fs_expand::VaultLinkTargets::over_index(filesystem, roots, &vault, names);
    let path = match resolve_link(&targets, SUBJECT_LINK_SOURCE, target.as_str())? {
        LinkResolution::Found(path) => path,
        LinkResolution::NotFound => return Ok(SubjectLookup::Unnamed),
        LinkResolution::Ambiguous { .. } => return Ok(SubjectLookup::Ambiguous),
    };
    let note = child_path(&root, roots, &path)?;
    let frontmatter = filesystem
        .read_text(&ReadTextRequest {
            path: note,
            limit: None,
        })
        .ok()
        .and_then(|read| FrontmatterDocument::try_from(read.content.as_str()).ok())
        .map(|document| document.frontmatter().clone())
        .unwrap_or_default();
    let text = |name: &str| {
        frontmatter
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let base = if is_task_record(filesystem, roots, &root, &path, &frontmatter) {
        Some(TASKS_BASE.to_owned())
    } else {
        text(BASE_PROPERTY)
            .and_then(|stored| BaseLinkTarget::try_from(stored.as_str()).ok())
            .map(|target| target.as_str().to_owned())
    };
    let record = text("id").zip(base).map(|(key, base)| SubjectRecord { key, base });
    Ok(SubjectLookup::Found(ResolvedSubject {
        note: SubjectNote { path, record },
        entity: text("entity"),
    }))
}

/// The subject `lookup` found before the write lock was taken, as it stands now the lock is held: a note deleted since
/// names nothing.
///
/// # Errors
///
/// Returns [`SubjectRejection::Ambiguous`] for the kind of `definition` when the link named more than one note.
fn subject_under_lock(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root: &VaultPath,
    definition: &KindDefinition,
    lookup: SubjectLookup,
) -> Result<Option<ResolvedSubject>, ToolError> {
    match lookup {
        SubjectLookup::Ambiguous => Err(EventsError::InvalidSubject {
            kind: definition.kind.to_string(),
            reason: SubjectRejection::Ambiguous,
        }
        .into()),
        SubjectLookup::Found(resolved)
            if child_path(root, roots, &resolved.note.path)
                .is_ok_and(|path| filesystem.is_file(&path).unwrap_or(false)) =>
        {
            Ok(Some(resolved))
        }
        SubjectLookup::Found(_) | SubjectLookup::Unnamed => Ok(None),
    }
}

/// Whether the note at `path` with `frontmatter` is a task the vault's task base selects.
fn is_task_record(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root: &VaultPath,
    path: &str,
    frontmatter: &Map<String, Value>,
) -> bool {
    frontmatter.get("type").and_then(Value::as_str) == Some("task")
        && TaskScope::source(filesystem, root, roots)
            .and_then(|source| TaskScope::try_from(source.as_str()).ok())
            .is_some_and(|scope| scope.holds(path))
}

/// Takes the next key of `series` by advancing the sidecar's counter with a conflict-checked write, so two processes
/// can never take the same one. Returns the key and the warnings the sidecar write raised.
fn reserve_key<W: WritesVault<Error = FsError>>(
    filesystem: &Filesystem,
    writes: &W,
    request: &RecordCreateRequest<'_>,
    series: &str,
) -> Result<(String, Vec<String>), ToolError> {
    let sidecar_path = base_sidecar::sidecar_path(request.roots, &request.base_path)?;
    let base_stem = request
        .base_path
        .relative()
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or(ToolError::Invalid("base path must have a UTF-8 filename"))?;
    for attempt in 1..=RESERVATION_ATTEMPTS {
        let SidecarState { sidecar, existing } = read_sidecar(filesystem, &sidecar_path)?;
        let key = sidecar
            .next_key(Some(series))?
            .ok_or(ToolError::Invalid("the events sidecar declares no key to assign"))?;
        let advanced = sidecar.advance(Some(series), request.timestamp)?;
        let (document, expected_hash): (Option<FrontmatterDocument>, Option<ContentHash>) = match existing {
            Some(existing) => (Some(existing.document), Some(existing.content_hash)),
            None => (None, None),
        };
        let content = render_sidecar_content(&advanced, document, base_stem)?;
        match writes.persist(&WriteMutation {
            path: sidecar_path.clone(),
            content: content.into(),
            expected_hash,
            force: false,
            origin: Origin::Tool("base_record_create".to_owned()),
        }) {
            Ok(result) => {
                let WarningMessages(warnings) = WarningMessages::from(result.warnings);
                return Ok((key, warnings));
            }
            Err(FsError::Conflict { .. } | FsError::ExpectedHashRequired { .. }) if attempt < RESERVATION_ATTEMPTS => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(ToolError::Invalid(
        "base_record_create could not reserve a key: the sidecar kept changing",
    ))
}

/// Records an event in an events base, whose kind registry is the folder `kinds_folder`.
///
/// # Errors
///
/// Returns the stable `events/*` refusal for a record that does not match its kind, before a key is reserved or
/// anything is written, or the filesystem error of a write that failed.
pub(crate) fn record_event<W: WritesVault<Error = FsError>>(
    filesystem: &Filesystem,
    writes: &W,
    request: &RecordCreateRequest<'_>,
    kinds_folder: &str,
    subject: SubjectLookup,
    at: OffsetDateTime,
) -> Result<RecordCreateOutcome, ToolError> {
    if request.filename.is_some() {
        return Err(EventsError::ServerStamped {
            field: "filename".to_owned(),
        }
        .into());
    }
    if request.folder.is_some() {
        return Err(EventsError::ServerStamped {
            field: "path".to_owned(),
        }
        .into());
    }
    let sidecar_path = base_sidecar::sidecar_path(request.roots, &request.base_path)?;
    let state = read_sidecar(filesystem, &sidecar_path)?;
    let series = sidecar_series(&state);
    let root = vault_root(request.roots, &request.base_path)?;
    let loaded = event_kinds::load(filesystem, request.roots, &root, kinds_folder, &series);

    let event = event_request(&request.frontmatter, &request.content)?;
    let definition: &KindDefinition = loaded.registry.lookup(event.kind.as_deref())?;
    let mapped = series.mapped_to(definition.kind.as_str());
    let key_series = definition.select_series(request.prefix, &mapped)?;
    // A refused record reserves nothing: validate with a stand-in key first, then again with the real one. The first
    // pass leaves the subject unread, so a refusal that needs no lookup is reported as itself.
    let stand_in = definition.id_field.as_ref().map(|_| "S-0".to_owned());
    let unread = RecordContext {
        at,
        subject_entity: None,
        subject_note: None,
        recorder_entity: None,
        assigned_key: stand_in.clone(),
    };
    definition.validate(&event, &unread)?;
    let subject = subject_under_lock(filesystem, request.roots, &root, definition, subject)?;
    let context = |assigned_key: Option<String>| RecordContext {
        at,
        subject_entity: subject.as_ref().and_then(|resolved| resolved.entity.clone()),
        subject_note: subject.as_ref().map(|resolved| resolved.note.clone()),
        recorder_entity: None,
        assigned_key,
    };
    definition.validate(&event, &context(stand_in))?;
    if let Some(unique) = &request.unique_fields {
        let source = filesystem.read_text(&ReadTextRequest {
            path: request.base_path.clone(),
            limit: None,
        })?;
        let base_document = BaseDocument::try_from(source.content.as_str())?;
        reject_duplicate_record(
            filesystem,
            request.roots,
            request.base_path.root_id(),
            &base_document,
            &request.frontmatter,
            unique,
        )?;
    }

    let (key, sidecar_warnings) = match key_series {
        Some(series) => {
            let (key, warnings) = reserve_key(filesystem, writes, request, &series)?;
            (Some(key), warnings)
        }
        None => (None, Vec::new()),
    };
    let record = definition.validate(&event, &context(key.clone()))?;
    let written = EventWriter {
        writes,
        roots: request.roots,
        vault: &root,
    }
    .write(&record)
    .map_err(|error| match (error, &key) {
        (EventWriteError::PathsExhausted, _) => ToolError::Invalid("no unused event path was found for this record"),
        (EventWriteError::Write(ToolError::Filesystem(source)), Some(key)) => ToolError::KeyReservedNoteNotWritten {
            key: key.clone(),
            source: Box::new(source),
        },
        (EventWriteError::Write(error), _) => error,
    })?;
    Ok(RecordCreateOutcome {
        note_result: PipelineResult {
            value: written.outcome,
            event: written.event,
            warnings: written.warnings,
        },
        key,
        sidecar_path: sidecar_path.portable_relative(),
        sidecar_warnings,
    })
}
