//! The Decisions register in Vault Settings: one tree of records of two types, decisions and questions, with the
//! register's configured settings, a summary table filtered by type and status, and the record detail view, which
//! reads a record (its fields beside its rendered body) by default and edits fields and body together in one form.
//!
//! Beneath `/{vault}/settings/decisions/`: the root lists records (`?type=` and `?status=` narrow it) and `PATCH` on it
//! saves the register's settings, `edit` loads those settings for editing, `new?type=decision|question` loads the
//! new-record form and `POST` on the root creates the record through `base_record_create` (the register's sidecar
//! assigns the id from that type's prefix), and `{record}` (its note's path without `.md`) reads one record, with
//! `{record}/edit` loading its editor and `PATCH` saving it.
//!
//! A save writes only what changed, in one `note_update` call conflict-checked against the version the form was
//! rendered from: the changed fields and, when it differs, the body, applied whole or refused whole.

use askama::Template;
use axum::body::Bytes;
use axum::http::{Method, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::entities::{EntityEntry, load_entities};
use crate::note_render::{self, NoteView};
use crate::register_records::{
    self, FieldKind, ListRecordsError, RecordRow, Submitted, ValidationContext, classify, fields, initial_status,
    is_iso_date, list_records, status_tone, text, validate_fields,
};
use crate::routes::vault::{not_found, unreachable};
use crate::templater;
use crate::vault_catalogue::ManagementCategory;
use crate::vault_content::{AttachedRead, read_attached};
use crate::vault_management_config::{RecordType, RegisterSettings};
use crate::vault_relative_path::VaultRelativePath;
use crate::vault_tree::FolderListing;
use crate::vault_write::{CONFLICT, WriteOutcome};

use super::vault_management::{ManagementContext, Outcome, Panel, malformed};
use super::vault_management_paths::{PathsView, RegisterSubmission, save_register_settings};

pub(crate) struct Filter {
    label: String,
    href: String,
    count: usize,
    active: bool,
}

pub(crate) struct RowView {
    id: String,
    type_label: &'static str,
    title: String,
    href: String,
    status: String,
    tone: &'static str,
    updated: String,
    related: String,
    related_href: Option<String>,
}

pub(crate) struct NewLink {
    label: String,
    href: String,
}

pub(crate) struct ListView {
    type_filters: Vec<Filter>,
    status_filters: Vec<Filter>,
    rows: Vec<RowView>,
    new_links: Vec<NewLink>,
    filtered_out: bool,
}

pub(crate) struct ChoiceOption {
    value: String,
    label: String,
    selected: bool,
}

pub(crate) enum Input {
    Text,
    Date,
    Select(Vec<ChoiceOption>),
    Lines,
}

pub(crate) struct FieldView {
    key: &'static str,
    label: &'static str,
    value: String,
    values: Vec<String>,
    link_href: Option<String>,
    input: Input,
    required: bool,
}

/// A fact about a record shown but never edited here.
pub(crate) struct Fact {
    label: &'static str,
    value: String,
}

pub(crate) struct RecordView {
    id: String,
    type_label: &'static str,
    title: String,
    status: String,
    tone: &'static str,
    path: String,
    facts: Vec<Fact>,
    fields: Vec<FieldView>,
    body_html: String,
    body: String,
    expected_hash: String,
    self_href: String,
    edit_href: String,
    list_href: String,
    error: Option<String>,
}

pub(crate) struct NewView {
    record_type: &'static str,
    type_label: &'static str,
    fields: Vec<FieldView>,
    body: String,
    guidance: Option<String>,
    create_href: String,
    list_href: String,
    notice: Option<String>,
    error: Option<String>,
}

pub(crate) enum MainView {
    List(ListView),
    Record(RecordView),
    Edit(RecordView),
    New(NewView),
}

#[derive(Template)]
#[template(path = "vm_register.html")]
struct RegisterTemplate {
    icon: &'static str,
    problem: Option<String>,
    paths: PathsView,
    provisioned: bool,
    main: MainView,
}

pub(super) struct Register<'a> {
    pub(super) settings: &'a RegisterSettings,
    listing: &'a FolderListing,
}

impl Register<'_> {
    fn record_path(&self, stem: &str) -> Option<VaultRelativePath> {
        let relative = VaultRelativePath::try_from(format!("{stem}.md")).ok()?;
        Some(self.settings.paths.dir.join(&relative))
    }

    fn holds(&self, stem: &str) -> bool {
        self.listing
            .folder()
            .is_some_and(|folder| folder.find_file(&format!("{stem}.md")).is_some())
    }
}

fn href(context: &ManagementContext<'_>, location: &str) -> String {
    context.href(ManagementCategory::Decisions, location)
}

fn type_label(kind: Option<RecordType>) -> &'static str {
    kind.map_or("Unclassified", RecordType::label)
}

pub(super) fn render(
    context: &ManagementContext<'_>,
    register: &Register<'_>,
    main: MainView,
    paths: PathsView,
) -> Panel {
    let listing_problem = match register.listing {
        FolderListing::Unreadable { code } => Some(format!(
            "`{}` could not be listed{}.",
            register.settings.paths.dir,
            code.as_ref().map(|code| format!(" ({code})")).unwrap_or_default()
        )),
        FolderListing::Present(_) | FolderListing::Missing => None,
    };
    let template = RegisterTemplate {
        icon: ManagementCategory::Decisions.icon(),
        problem: listing_problem.or_else(|| context.catalogue.loaded.problem()),
        paths,
        provisioned: register.listing.folder().is_some(),
        main,
    };
    Panel {
        title: "Decisions".to_owned(),
        html: template
            .render()
            .unwrap_or_else(|_| "<p class=\"hint error\">This page could not be rendered.</p>".to_owned()),
    }
}

pub(super) fn paths_view(context: &ManagementContext<'_>) -> PathsView {
    PathsView::register(&context.catalogue.loaded, href(context, ""))
}

/// Every record, or the response saying why the register could not be listed: a failed query is never shown as an
/// empty register.
async fn records(context: &ManagementContext<'_>, register: &Register<'_>) -> Result<Vec<RecordRow>, Outcome> {
    if register.listing.folder().is_none() {
        return Ok(Vec::new());
    }
    match list_records(context.client, context.vault_name, register.settings).await {
        Ok(rows) => Ok(rows),
        Err(ListRecordsError::Unreachable(_)) => Err(Outcome::from(unreachable())),
        Err(ListRecordsError::Refused { code }) => Err(Outcome::from(
            (
                StatusCode::BAD_GATEWAY,
                [("content-type", "text/html; charset=utf-8")],
                format!(
                    "<p class=\"hint error\" role=\"alert\">The register could not be listed{}.</p>",
                    code.map(|code| format!(" ({code})")).unwrap_or_default()
                ),
            )
                .into_response(),
        )),
    }
}

fn decision_href(context: &ManagementContext<'_>, rows: &[RecordRow], id: &str) -> Option<String> {
    rows.iter()
        .find(|row| row.id == id && row.record_type == Some(RecordType::Decision))
        .map(|row| href(context, &row.stem))
}

fn filter_href(context: &ManagementContext<'_>, kind: Option<RecordType>, status: Option<&str>) -> String {
    let mut params = Vec::new();
    if let Some(kind) = kind {
        params.push(format!("type={}", kind.value()));
    }
    if let Some(status) = status {
        params.push(format!("status={}", crate::inbox_outbox::encode_query_value(status)));
    }
    let base = href(context, "");
    if params.is_empty() {
        base
    } else {
        format!("{base}?{}", params.join("&"))
    }
}

fn capitalise(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn selected_type(context: &ManagementContext<'_>) -> Option<RecordType> {
    context
        .query
        .get("type")
        .and_then(|value| RecordType::try_from(value.as_str()).ok())
}

fn list_view(context: &ManagementContext<'_>, rows: &[RecordRow]) -> MainView {
    let selected_type = selected_type(context);
    let of_type: Vec<&RecordRow> = rows
        .iter()
        .filter(|row| selected_type.is_none_or(|kind| row.record_type == Some(kind)))
        .collect();
    let statuses: Vec<&str> = match selected_type {
        Some(kind) => register_records::statuses(kind).to_vec(),
        None => RecordType::ALL
            .into_iter()
            .flat_map(|kind| register_records::statuses(kind).iter().copied())
            .collect(),
    };
    let selected_status = context
        .query
        .get("status")
        .map(String::as_str)
        .filter(|status| statuses.contains(status));
    let mut type_filters = vec![Filter {
        label: "All".to_owned(),
        href: filter_href(context, None, None),
        count: rows.len(),
        active: selected_type.is_none(),
    }];
    type_filters.extend(RecordType::ALL.into_iter().map(|kind| Filter {
        label: kind.plural_label().to_owned(),
        href: filter_href(context, Some(kind), None),
        count: rows.iter().filter(|row| row.record_type == Some(kind)).count(),
        active: selected_type == Some(kind),
    }));
    let mut status_filters = vec![Filter {
        label: "Any status".to_owned(),
        href: filter_href(context, selected_type, None),
        count: of_type.len(),
        active: selected_status.is_none(),
    }];
    status_filters.extend(statuses.iter().map(|status| Filter {
        label: capitalise(status),
        href: filter_href(context, selected_type, Some(status)),
        count: of_type.iter().filter(|row| row.status == *status).count(),
        active: selected_status == Some(*status),
    }));
    let visible: Vec<RowView> = of_type
        .iter()
        .filter(|row| selected_status.is_none_or(|status| row.status == status))
        .map(|row| RowView {
            id: row.id.clone(),
            type_label: type_label(row.record_type),
            title: row.title.clone(),
            href: href(context, &row.stem),
            status: row.status.clone(),
            tone: status_tone(&row.status),
            updated: row.updated.clone(),
            related_href: decision_href(context, rows, &row.related),
            related: row.related.clone(),
        })
        .collect();
    MainView::List(ListView {
        filtered_out: visible.is_empty() && !rows.is_empty(),
        type_filters,
        status_filters,
        rows: visible,
        new_links: RecordType::ALL
            .into_iter()
            .map(|kind| NewLink {
                label: format!("New {}", kind.value()),
                href: format!("{}?type={}", href(context, "new"), kind.value()),
            })
            .collect(),
    })
}

fn options(values: impl IntoIterator<Item = (String, String)>, current: &str, blank: bool) -> Vec<ChoiceOption> {
    let mut options: Vec<ChoiceOption> = Vec::new();
    if blank {
        options.push(ChoiceOption {
            value: String::new(),
            label: "None".to_owned(),
            selected: current.is_empty(),
        });
    }
    for (value, label) in values {
        options.push(ChoiceOption {
            selected: value == current,
            value,
            label,
        });
    }
    if !current.is_empty() && !options.iter().any(|option| option.selected) {
        options.push(ChoiceOption {
            value: current.to_owned(),
            label: current.to_owned(),
            selected: true,
        });
    }
    options
}

struct FieldSources<'a> {
    entities: &'a [EntityEntry],
    records: &'a [RecordRow],
    /// The record being shown, so a decision is never offered as its own successor.
    own_id: &'a str,
}

fn field_input(spec_kind: FieldKind, value: &str, sources: &FieldSources<'_>) -> Input {
    match spec_kind {
        // A date input cannot hold a stored timestamp; such a value is offered as text so it is kept as it is.
        FieldKind::Date if value.is_empty() || is_iso_date(value) => Input::Date,
        FieldKind::Text | FieldKind::Date => Input::Text,
        FieldKind::List => Input::Lines,
        FieldKind::Choice(allowed) => Input::Select(options(
            allowed.iter().map(|value| ((*value).to_owned(), capitalise(value))),
            value,
            false,
        )),
        FieldKind::Entity if sources.entities.is_empty() => Input::Text,
        FieldKind::Entity => Input::Select(options(
            sources
                .entities
                .iter()
                .map(|entity| (entity.slug.clone(), entity.name.clone())),
            value,
            true,
        )),
        FieldKind::DecisionLink => Input::Select(options(
            sources
                .records
                .iter()
                .filter(|row| {
                    row.record_type == Some(RecordType::Decision) && row.id != sources.own_id && !row.id.is_empty()
                })
                .map(|row| (row.id.clone(), format!("{} · {}", row.id, row.title))),
            value,
            true,
        )),
    }
}

fn field_views(
    context: &ManagementContext<'_>,
    kind: Option<RecordType>,
    values: &Map<String, Value>,
    sources: &FieldSources<'_>,
) -> Vec<FieldView> {
    fields(kind)
        .iter()
        .map(|spec| {
            let value = text(values, spec.key);
            let link_href = matches!(spec.kind, FieldKind::DecisionLink)
                .then(|| decision_href(context, sources.records, &value))
                .flatten();
            FieldView {
                key: spec.key,
                label: spec.label,
                values: value
                    .lines()
                    .map(str::to_owned)
                    .filter(|line| !line.is_empty())
                    .collect(),
                input: field_input(spec.kind, &value, sources),
                value,
                link_href,
                required: spec.required,
            }
        })
        .collect()
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct FrontmatterReadResult {
    #[serde(default)]
    pub(super) frontmatter: Map<String, Value>,
}

/// One record's frontmatter, raw text, and hash.
struct LoadedRecord {
    frontmatter: Map<String, Value>,
    raw: String,
    hash: Option<String>,
}

async fn read_record_text(
    context: &ManagementContext<'_>,
    path: &VaultRelativePath,
) -> Result<(String, Option<String>), Outcome> {
    match read_attached(context.client, &format!("{}://{path}", context.vault_name)).await {
        Ok(AttachedRead::Text(attached)) => Ok((attached.text, attached.content_hash)),
        Ok(_) => Err(Outcome::from(not_found())),
        Err(_) => Err(Outcome::from(unreachable())),
    }
}

async fn load_record(context: &ManagementContext<'_>, path: &VaultRelativePath) -> Result<LoadedRecord, Outcome> {
    let (raw, hash) = read_record_text(context, path).await?;
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{}://{path}", context.vault_name)),
    );
    let frontmatter = match context.client.call_tool("frontmatter_read".to_owned(), args).await {
        Ok(result) if result.is_error != Some(true) => result
            .into_typed::<FrontmatterReadResult>()
            .map(|read| read.frontmatter)
            .unwrap_or_default(),
        Ok(_) => Map::new(),
        Err(_) => return Err(Outcome::from(unreachable())),
    };
    Ok(LoadedRecord { frontmatter, raw, hash })
}

fn body_of(raw: &str) -> String {
    crate::vault_content::body_of(raw.strip_prefix('\u{feff}').unwrap_or(raw))
        .trim_start_matches(['\r', '\n'])
        .replace("\r\n", "\n")
}

fn facts(kind: Option<RecordType>, frontmatter: &Map<String, Value>, settings: &RegisterSettings) -> Vec<Fact> {
    let declared_type = text(frontmatter, "type");
    let type_value = match (kind, RecordType::try_from(declared_type.as_str())) {
        (Some(kind), Ok(_)) => kind.label().to_owned(),
        (Some(kind), Err(_)) => format!(
            "{} (told by its id prefix {}; it has no type field)",
            kind.label(),
            settings.record_type(kind).prefix
        ),
        (None, _) => "Unclassified: neither its type field nor its id names a decision or a question".to_owned(),
    };
    let mut facts = vec![Fact {
        label: "Type",
        value: type_value,
    }];
    for (key, label) in [("created", "Created"), ("updated", "Updated")] {
        let value = register_records::display_date(&text(frontmatter, key));
        if !value.is_empty() {
            facts.push(Fact { label, value });
        }
    }
    facts
}

/// What a rejected edit puts back on screen: the submitted fields, body, and version, and why it was not saved.
struct Rejected<'a> {
    values: &'a Submitted,
    body: String,
    expected_hash: String,
    error: String,
}

async fn record_view(
    context: &ManagementContext<'_>,
    register: &Register<'_>,
    stem: &str,
    rejected: Option<Rejected<'_>>,
) -> Result<RecordView, Outcome> {
    let Some(path) = register.record_path(stem) else {
        return Err(Outcome::from(not_found()));
    };
    let (record, entities, rows) = tokio::join!(
        load_record(context, &path),
        load_entities(context.client, context.vault_name),
        records(context, register),
    );
    let record = record?;
    let rows = rows?;
    let id = text(&record.frontmatter, "id");
    let kind = classify(&text(&record.frontmatter, "type"), &id, register.settings);
    let body = body_of(&record.raw);
    let title = Some(text(&record.frontmatter, "title"))
        .filter(|title| !title.is_empty())
        .or_else(|| register_records::heading_title(&body, &id))
        .unwrap_or_else(|| stem.to_owned());
    let status = text(&record.frontmatter, "status");
    let sources = FieldSources {
        entities: &entities,
        records: &rows,
        own_id: &id,
    };
    let (fields, body, expected_hash, error) = if let Some(rejected) = rejected {
        (
            field_views(context, kind, rejected.values, &sources),
            rejected.body,
            rejected.expected_hash,
            Some(rejected.error),
        )
    } else {
        let mut shown = record.frontmatter.clone();
        if text(&shown, "title").is_empty() {
            shown.insert("title".to_owned(), Value::String(title.clone()));
        }
        (
            field_views(context, kind, &shown, &sources),
            body,
            record.hash.unwrap_or_default(),
            None,
        )
    };
    let body_html = note_render::render_note(
        context.client,
        context.vault_name,
        NoteView {
            path: path.as_str(),
            raw: Some(&record.raw),
            template: false,
        },
    )
    .await
    .html;
    Ok(RecordView {
        facts: facts(kind, &record.frontmatter, register.settings),
        id,
        type_label: type_label(kind),
        title,
        tone: status_tone(&status),
        status,
        path: path.to_string(),
        fields,
        body_html,
        body,
        expected_hash,
        self_href: href(context, stem),
        edit_href: href(context, &format!("{stem}/edit")),
        list_href: href(context, ""),
        error,
    })
}

/// A template's body for a new record: frontmatter, the leading heading (the new record's own title replaces it), and
/// a trailing authoring comment (shown beside the form instead) removed, and its date commands rendered.
fn seed_from_template(raw: &str, at: time::OffsetDateTime) -> (String, Option<String>) {
    let body = body_of(raw);
    let mut lines: Vec<&str> = body.lines().collect();
    if let Some(index) = lines.iter().position(|line| line.starts_with("# ")) {
        lines.remove(index);
    }
    let mut body = lines.join("\n").trim().to_owned();
    let mut guidance = None;
    if body.ends_with("-->")
        && let Some(start) = body.rfind("<!--")
    {
        let comment = body[start + 4..body.len() - 3].trim().to_owned();
        body.truncate(body[..start].trim_end().len());
        guidance = (!comment.is_empty()).then_some(comment);
    }
    let body = templater::render_templater_expressions(&body, at).unwrap_or(body);
    (if body.is_empty() { body } else { format!("{body}\n") }, guidance)
}

pub(super) struct NewForm<'a> {
    pub(super) values: &'a Submitted,
    pub(super) body: String,
    pub(super) error: String,
}

pub(super) async fn new_view(
    context: &ManagementContext<'_>,
    register: &Register<'_>,
    kind: RecordType,
    submitted: Option<NewForm<'_>>,
) -> Result<MainView, Outcome> {
    let (entities, rows) = tokio::join!(
        load_entities(context.client, context.vault_name),
        records(context, register)
    );
    let rows = rows?;
    let sources = FieldSources {
        entities: &entities,
        records: &rows,
        own_id: "",
    };
    let (seed, guidance, notice) = match &register.settings.record_type(kind).template {
        None => (String::new(), None, None),
        Some(template) => match read_attached(context.client, &format!("{}://{template}", context.vault_name)).await {
            Ok(AttachedRead::Text(attached)) => {
                let (seed, guidance) = seed_from_template(&attached.text, context.now);
                (seed, guidance, None)
            }
            _ => (
                String::new(),
                None,
                Some(format!(
                    "The configured template `{template}` could not be read, so the body starts blank."
                )),
            ),
        },
    };
    let (fields, body, error) = if let Some(form) = submitted {
        (
            field_views(context, Some(kind), form.values, &sources),
            form.body,
            Some(form.error),
        )
    } else {
        let mut defaults = Map::new();
        defaults.insert("status".to_owned(), Value::String(initial_status(kind).to_owned()));
        if kind == RecordType::Decision {
            defaults.insert(
                "date".to_owned(),
                Value::String(templater::format_moment_like(context.now, "YYYY-MM-DD")),
            );
        }
        (field_views(context, Some(kind), &defaults, &sources), seed, None)
    };
    Ok(MainView::New(NewView {
        record_type: kind.value(),
        type_label: kind.label(),
        fields,
        body,
        guidance,
        create_href: href(context, ""),
        list_href: href(context, ""),
        notice,
        error,
    }))
}

/// Splits a submitted record form into its fields, body, and hash, refusing anything that is not a flat object of
/// text values.
pub(super) fn parse_form(body: &Bytes) -> Option<(Submitted, String, String)> {
    let mut form: Submitted = serde_json::from_slice(body).ok()?;
    if form
        .values()
        .any(|value| !matches!(value, Value::String(_) | Value::Null))
    {
        return None;
    }
    let text = |value: Option<Value>| {
        value
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default()
    };
    let record_body = text(form.remove("body"));
    let expected_hash = text(form.remove("expected_hash"));
    Some((form, record_body, expected_hash))
}

/// The decision ids a link may name and the entity slugs an entity may name.
pub(super) async fn validation_sources(
    context: &ManagementContext<'_>,
    register: &Register<'_>,
) -> Result<(Vec<String>, Vec<String>), Outcome> {
    let (entities, rows) = tokio::join!(
        load_entities(context.client, context.vault_name),
        records(context, register)
    );
    let decision_ids = rows?
        .into_iter()
        .filter(|row| row.record_type == Some(RecordType::Decision) && !row.id.is_empty())
        .map(|row| row.id)
        .collect();
    Ok((decision_ids, entities.into_iter().map(|entity| entity.slug).collect()))
}

#[derive(Debug, Deserialize)]
pub(super) struct Refusal {
    #[serde(default)]
    pub(super) code: Option<String>,
}

fn conflict_reason() -> String {
    WriteOutcome::Refused {
        code: Some(CONFLICT.to_owned()),
    }
    .refusal_reason()
    .unwrap_or_default()
}

async fn save_record(context: &ManagementContext<'_>, register: &Register<'_>, stem: &str, body: &Bytes) -> Outcome {
    let Some((form, new_body, expected_hash)) = parse_form(body) else {
        return malformed();
    };
    let Some(path) = register.record_path(stem) else {
        return Outcome::from(not_found());
    };
    let current = match load_record(context, &path).await {
        Ok(record) => record,
        Err(outcome) => return outcome,
    };
    let (decision_ids, entity_slugs) = match validation_sources(context, register).await {
        Ok(sources) => sources,
        Err(outcome) => return outcome,
    };
    let kind = classify(
        &text(&current.frontmatter, "type"),
        &text(&current.frontmatter, "id"),
        register.settings,
    );
    let validation = ValidationContext {
        current: &current.frontmatter,
        decision_ids: &decision_ids,
        entity_slugs: &entity_slugs,
    };
    let error = if expected_hash.is_empty() || current.hash.as_deref() != Some(expected_hash.as_str()) {
        conflict_reason()
    } else {
        match validate_fields(kind, &form, &validation) {
            Err(error) => error,
            Ok(changes) => match Box::pin(write_record(context, &path, &current, changes, &new_body)).await {
                Ok(None) => {
                    return Outcome::Wrote {
                        location: stem.to_owned(),
                    };
                }
                Ok(Some(error)) => error,
                Err(outcome) => return outcome,
            },
        }
    };
    let rejected = Rejected {
        values: &form,
        body: new_body.clone(),
        expected_hash: expected_hash.clone(),
        error,
    };
    match Box::pin(record_view(context, register, stem, Some(rejected))).await {
        Ok(view) => Outcome::Show(render(context, register, MainView::Edit(view), paths_view(context))),
        Err(outcome) => outcome,
    }
}

/// Writes what changed in one conflict-checked `note_update`: the fields patch and, when it differs, the body, so a
/// save is applied whole or not at all, `created` is kept, and `updated` is stamped. Nothing is written when nothing
/// changed. `Ok(Some(reason))` is a refusal to show beside the form.
async fn write_record(
    context: &ManagementContext<'_>,
    path: &VaultRelativePath,
    current: &LoadedRecord,
    fields: Map<String, Value>,
    new_body: &str,
) -> Result<Option<String>, Outcome> {
    let submitted_body = new_body.replace("\r\n", "\n");
    let body_changed = body_of(&current.raw).trim_end() != submitted_body.trim_end();
    if fields.is_empty() && !body_changed {
        return Ok(None);
    }
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{}://{path}", context.vault_name)),
    );
    args.insert(
        "expected_hash".to_owned(),
        Value::String(current.hash.clone().unwrap_or_default()),
    );
    if !fields.is_empty() {
        args.insert("patch".to_owned(), Value::Object(fields));
    }
    if body_changed {
        // The body is everything after the frontmatter's closing line: a blank line, then the text. The server stores
        // it, like all text, with `\n` line endings.
        args.insert(
            "body".to_owned(),
            Value::String(format!("\n{}\n", submitted_body.trim_end())),
        );
    }
    let result = context
        .client
        .call_tool("note_update".to_owned(), args)
        .await
        .map_err(|_| Outcome::from(unreachable()))?;
    if result.is_error == Some(true) {
        let code = result.into_typed::<Refusal>().ok().and_then(|refusal| refusal.code);
        return Ok(WriteOutcome::Refused { code }.refusal_reason());
    }
    Ok(None)
}

async fn update_settings(context: &ManagementContext<'_>, register: &Register<'_>, body: &Bytes) -> Outcome {
    let Ok(submitted) = serde_json::from_slice::<RegisterSubmission>(body) else {
        return malformed();
    };
    match save_register_settings(context, &submitted).await {
        Ok(()) => Outcome::Wrote {
            location: String::new(),
        },
        Err(error) => {
            let rows = match records(context, register).await {
                Ok(rows) => rows,
                Err(outcome) => return outcome,
            };
            let paths = paths_view(context).rejected(&submitted.values(), &submitted.expected_hash, error);
            Outcome::Show(render(context, register, list_view(context, &rows), paths))
        }
    }
}

async fn show(context: &ManagementContext<'_>, register: &Register<'_>, rest: &str) -> Outcome {
    let (stem, action) = match rest.rsplit_once('/') {
        Some((stem, "edit")) => (stem, Some("edit")),
        _ => (rest, None),
    };
    let mut paths = paths_view(context);
    let main = match (stem, action) {
        ("" | "edit", None) => {
            paths.editing = stem == "edit";
            records(context, register).await.map(|rows| list_view(context, &rows))
        }
        ("new", None) => {
            let Some(kind) = selected_type(context) else {
                return Outcome::from(not_found());
            };
            if register.listing.folder().is_none() {
                return Outcome::from(not_found());
            }
            new_view(context, register, kind, None).await
        }
        (stem, None) if register.holds(stem) => Box::pin(record_view(context, register, stem, None))
            .await
            .map(MainView::Record),
        (stem, Some(_)) if register.holds(stem) => Box::pin(record_view(context, register, stem, None))
            .await
            .map(MainView::Edit),
        _ => return Outcome::from(not_found()),
    };
    match main {
        Ok(main) => Outcome::Show(render(context, register, main, paths)),
        Err(outcome) => outcome,
    }
}

/// Answers a request beneath the Decisions category.
pub(super) async fn handle(context: &ManagementContext<'_>, rest: &str, method: &Method, body: &Bytes) -> Outcome {
    let register = Register {
        settings: context.catalogue.loaded.config.register(),
        listing: &context.catalogue.decisions,
    };
    let rest = rest.trim_matches('/');
    match *method {
        Method::GET => Box::pin(show(context, &register, rest)).await,
        Method::PATCH if rest.is_empty() => Box::pin(update_settings(context, &register, body)).await,
        Method::PATCH if register.holds(rest) => Box::pin(save_record(context, &register, rest, body)).await,
        Method::POST if rest.is_empty() && register.listing.folder().is_some() => {
            Box::pin(super::vault_management_register_create::create_record(
                context, &register, body,
            ))
            .await
        }
        Method::POST if rest.is_empty() => Outcome::from(StatusCode::CONFLICT.into_response()),
        _ if !rest.is_empty() && !register.holds(rest) => Outcome::from(not_found()),
        _ => Outcome::from(StatusCode::METHOD_NOT_ALLOWED.into_response()),
    }
}

#[cfg(test)]
#[path = "vault_management_registers_test.rs"]
mod tests;
