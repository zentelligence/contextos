//! The vault's Decisions register: one Markdown note per record in one configured folder, holding two types of
//! record, decisions and questions, told apart by the record's `type` frontmatter or, when it has none, by its id's
//! prefix. Its state is carried in frontmatter and listed through the register's companion `.base`. This module owns
//! each record type's field schema (which fields a record has, how each is edited, and which values a closed field
//! accepts), classifying and listing records, and turning a submitted record form into a validated frontmatter patch
//! of only what changed.
//!
//! The vocabularies follow the register's own templates: a decision is `adopted` or `superseded` (and names its
//! successor in `superseded_by`); a question is `open` or `resolved` (and may name the decision it led to in
//! `decision`). A record already carrying a value outside its vocabulary keeps showing it, and keeps it on save,
//! rather than being silently rewritten. `id`, `type`, `created`, and `updated` are never edited here: the id and
//! type are fixed when a record is created, and the server stamps the timestamps.

use std::cmp::Ordering;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::mcp_client::{McpCallError, McpClient};
use crate::vault_management_config::{RecordType, RegisterSettings};
use crate::vault_relative_path::VaultRelativePath;

/// How a record field is edited.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldKind {
    Text,
    /// A calendar date, `YYYY-MM-DD`.
    Date,
    /// One of a fixed set of values.
    Choice(&'static [&'static str]),
    /// One of the vault's configured entities.
    Entity,
    /// A list of short values, one per line.
    List,
    /// The id of a decision in the register.
    DecisionLink,
}

/// One frontmatter field a record type carries.
#[derive(Clone, Copy, Debug)]
pub struct FieldSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub required: bool,
}

pub const DECISION_STATUSES: &[&str] = &["adopted", "superseded"];
pub const QUESTION_STATUSES: &[&str] = &["open", "resolved"];

const fn field(key: &'static str, label: &'static str, kind: FieldKind, required: bool) -> FieldSpec {
    FieldSpec {
        key,
        label,
        kind,
        required,
    }
}

const DECISION_FIELDS: &[FieldSpec] = &[
    field("title", "Title", FieldKind::Text, true),
    field("status", "Status", FieldKind::Choice(DECISION_STATUSES), true),
    field("date", "Decided", FieldKind::Date, false),
    field("superseded_by", "Superseded by", FieldKind::DecisionLink, false),
    field("applies_to", "Applies to", FieldKind::List, false),
    field("description", "Description", FieldKind::Text, false),
];

const QUESTION_FIELDS: &[FieldSpec] = &[
    field("title", "Title", FieldKind::Text, true),
    field("status", "Status", FieldKind::Choice(QUESTION_STATUSES), true),
    field("entity", "Entity", FieldKind::Entity, false),
    field("resolved", "Resolved", FieldKind::Date, false),
    field("decision", "Decision", FieldKind::DecisionLink, false),
    field("blocks", "Blocks", FieldKind::Text, false),
    field("resolution_path", "Resolution path", FieldKind::Text, false),
    field("description", "Description", FieldKind::Text, false),
];

/// A record whose type cannot be told: neither its `type` nor its id names one. Shown and edited with only the
/// fields every record shares, so nothing type-specific is guessed at.
const UNCLASSIFIED_FIELDS: &[FieldSpec] = &[
    field("title", "Title", FieldKind::Text, true),
    field("status", "Status", FieldKind::Text, false),
    field("description", "Description", FieldKind::Text, false),
];

/// The fields a record of `kind` carries, in display order; `None` is an unclassified record.
#[must_use]
pub const fn fields(kind: Option<RecordType>) -> &'static [FieldSpec] {
    match kind {
        Some(RecordType::Decision) => DECISION_FIELDS,
        Some(RecordType::Question) => QUESTION_FIELDS,
        None => UNCLASSIFIED_FIELDS,
    }
}

/// The status values `kind` offers.
#[must_use]
pub const fn statuses(kind: RecordType) -> &'static [&'static str] {
    match kind {
        RecordType::Decision => DECISION_STATUSES,
        RecordType::Question => QUESTION_STATUSES,
    }
}

/// The status a new record of `kind` starts with.
#[must_use]
pub const fn initial_status(kind: RecordType) -> &'static str {
    match kind {
        RecordType::Decision => "adopted",
        RecordType::Question => "open",
    }
}

/// The field that links a record onward: the decision that superseded it, or the decision a question led to.
#[must_use]
pub const fn related_field(kind: RecordType) -> &'static str {
    match kind {
        RecordType::Decision => "superseded_by",
        RecordType::Question => "decision",
    }
}

/// A status's badge tone: settled records read as done, open questions as needing attention.
#[must_use]
pub fn status_tone(status: &str) -> &'static str {
    match status {
        "adopted" | "resolved" => "done",
        "open" => "attention",
        _ => "neutral",
    }
}

/// What type `record_type` (the record's `type` frontmatter) and `id` make a record: the `type` when it names one,
/// otherwise the type whose configured id prefix the id carries, otherwise unclassified.
#[must_use]
pub fn classify(record_type: &str, id: &str, settings: &RegisterSettings) -> Option<RecordType> {
    if let Ok(kind) = RecordType::try_from(record_type.trim()) {
        return Some(kind);
    }
    RecordType::ALL.into_iter().find(|kind| {
        let prefix = &settings.record_type(*kind).prefix;
        id.len() > prefix.len()
            && id.is_char_boundary(prefix.len())
            && id[..prefix.len()].eq_ignore_ascii_case(prefix)
            && id.as_bytes()[prefix.len()] == b'-'
    })
}

/// One record as the register's list shows it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecordRow {
    /// The note's path relative to the register folder, without `.md`: the record's address beneath the register.
    pub stem: String,
    pub id: String,
    pub record_type: Option<RecordType>,
    pub title: String,
    pub status: String,
    pub updated: String,
    pub related: String,
}

/// The `YYYY-MM-DD` date at the start of a frontmatter timestamp, or the value as written when it has none.
#[must_use]
pub fn display_date(raw: &str) -> String {
    let candidate = raw.trim().trim_matches('"');
    if starts_with_iso_date(candidate) {
        candidate[..10].to_owned()
    } else {
        candidate.to_owned()
    }
}

fn starts_with_iso_date(candidate: &str) -> bool {
    candidate.len() >= 10
        && candidate.as_bytes()[..10]
            .iter()
            .enumerate()
            .all(|(index, byte)| match index {
                4 | 7 => *byte == b'-',
                _ => byte.is_ascii_digit(),
            })
}

/// Whether `value` is exactly a real calendar date, `YYYY-MM-DD`.
#[must_use]
pub fn is_iso_date(value: &str) -> bool {
    value.len() == 10
        && starts_with_iso_date(value)
        && time::Date::parse(value, time::macros::format_description!("[year]-[month]-[day]")).is_ok()
}

/// A record's title from the first level-one heading of its body, without a leading `ID:` the heading may repeat.
#[must_use]
pub fn heading_title(body: &str, id: &str) -> Option<String> {
    let heading = body.lines().find_map(|line| line.strip_prefix("# "))?.trim();
    let without_id = (!id.is_empty())
        .then(|| heading.strip_prefix(id))
        .flatten()
        .map(|rest| rest.trim_start_matches([':', ' ', '-']).trim())
        .filter(|rest| !rest.is_empty());
    Some(without_id.unwrap_or(heading).to_owned())
}

/// Orders ids such as `VD-10` after `VD-9`: by their trailing number when both have one, otherwise as text.
#[must_use]
pub fn compare_ids(left: &str, right: &str) -> Ordering {
    fn number(id: &str) -> Option<(&str, u64)> {
        let split = id.trim_end_matches(|ch: char| ch.is_ascii_digit()).len();
        id[split..].parse().ok().map(|number| (&id[..split], number))
    }
    match (number(left), number(right)) {
        (Some((left_prefix, left_number)), Some((right_prefix, right_number))) => {
            left_prefix.cmp(right_prefix).then(left_number.cmp(&right_number))
        }
        _ => left.cmp(right),
    }
}

/// A frontmatter value as plain text: a list one item per line.
#[must_use]
pub fn text(row: &Map<String, Value>, key: &str) -> String {
    match row.get(key) {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map_or_else(|| item.to_string(), str::to_owned))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// `row` from a `base_query` over the register folder as a list row, `None` when it is not beneath the folder.
#[must_use]
pub fn record_row(settings: &RegisterSettings, row: &Map<String, Value>) -> Option<RecordRow> {
    let path = VaultRelativePath::try_from(text(row, "file.path")).ok()?;
    let relative = path.strip_ancestor(&settings.paths.dir)?;
    let stem = relative.strip_suffix(".md")?.to_owned();
    let id = text(row, "id");
    let record_type = classify(&text(row, "type"), &id, settings);
    let related = record_type
        .map(|kind| text(row, related_field(kind)))
        .unwrap_or_default();
    let updated = text(row, "updated");
    let dated = if updated.is_empty() {
        let fallback = match record_type {
            Some(RecordType::Decision) => "date",
            Some(RecordType::Question) | None => "created",
        };
        text(row, fallback)
    } else {
        updated
    };
    Some(RecordRow {
        record_type,
        title: text(row, "title"),
        status: text(row, "status"),
        updated: display_date(&dated),
        related,
        stem,
        id,
    })
}

#[derive(Debug, Deserialize)]
struct BaseQueryResult {
    content: String,
}

#[derive(Debug, Deserialize)]
struct Refusal {
    #[serde(default)]
    code: Option<String>,
}

/// Why a register could not be listed.
#[derive(Debug, thiserror::Error)]
pub enum ListRecordsError {
    #[error("the register could not be listed")]
    Refused { code: Option<String> },
    #[error(transparent)]
    Unreachable(#[from] McpCallError),
}

/// Every record note in the register folder, newest id first. A record with no `title` in its frontmatter takes its
/// first heading.
///
/// # Errors
///
/// Returns [`ListRecordsError::Refused`] when the query is refused or its result cannot be read, and
/// [`ListRecordsError::Unreachable`] when the transport fails.
pub async fn list_records(
    client: &McpClient,
    vault_name: &str,
    settings: &RegisterSettings,
) -> Result<Vec<RecordRow>, ListRecordsError> {
    let folder = serde_json::to_string(settings.paths.dir.as_str()).unwrap_or_default();
    let mut args = Map::new();
    args.insert("vault".to_owned(), Value::String(format!("{vault_name}://.")));
    args.insert("format".to_owned(), Value::String("json".to_owned()));
    args.insert(
        "definition".to_owned(),
        json!({
            "filters": format!("file.inFolder({folder}) && file.ext == \"md\" && file.basename != \"index\""),
            "properties": {
                "file.path": {}, "id": {}, "type": {}, "title": {}, "status": {}, "updated": {},
                "date": {}, "created": {}, "superseded_by": {}, "decision": {},
            },
            "views": [{
                "type": "table",
                "name": "records",
                "order": [
                    "file.path", "id", "type", "title", "status", "updated", "date", "created", "superseded_by",
                    "decision"
                ],
            }],
        }),
    );
    let result = client.call_tool("base_query".to_owned(), args).await?;
    if result.is_error == Some(true) {
        let code = result.into_typed::<Refusal>().ok().and_then(|refusal| refusal.code);
        return Err(ListRecordsError::Refused { code });
    }
    let rows: Vec<Map<String, Value>> = result
        .into_typed::<BaseQueryResult>()
        .ok()
        .and_then(|parsed| serde_json::from_str(&parsed.content).ok())
        .ok_or(ListRecordsError::Refused { code: None })?;
    let mut records: Vec<RecordRow> = rows.iter().filter_map(|row| record_row(settings, row)).collect();
    for record in records.iter_mut().filter(|record| record.title.is_empty()) {
        let path = format!("{vault_name}://{}/{}.md", settings.paths.dir, record.stem);
        let body = crate::vault_content::read_full_text(client, &path)
            .await?
            .unwrap_or_default();
        record.title =
            heading_title(crate::vault_content::body_of(&body), &record.id).unwrap_or_else(|| record.stem.clone());
    }
    records.sort_by(|left, right| compare_ids(&right.id, &left.id).then_with(|| left.stem.cmp(&right.stem)));
    Ok(records)
}

/// A submitted record form, field by field, as text.
pub type Submitted = Map<String, Value>;

/// Whether `new` would leave `key` as it already is in `current`: an absent key, `null`, an empty string, and an
/// empty list all mean "no value".
fn unchanged(current: &Map<String, Value>, key: &str, new: &Value) -> bool {
    let is_empty = |value: Option<&Value>| match value {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.is_empty(),
        Some(Value::Array(items)) => items.is_empty(),
        Some(_) => false,
    };
    let held = current.get(key);
    if is_empty(held) && is_empty(Some(new)) {
        return true;
    }
    held == Some(new)
}

/// What a submitted form needs to be checked against.
pub struct ValidationContext<'a> {
    /// The record's existing frontmatter, empty for a new record: a closed field may keep a value it already held,
    /// and a field whose value is unchanged is left out of the patch.
    pub current: &'a Map<String, Value>,
    /// Every decision id a link field may name.
    pub decision_ids: &'a [String],
    /// Every entity slug an entity field may name; empty when the vault configures none.
    pub entity_slugs: &'a [String],
}

/// Validates every field of `kind`'s schema in a submitted record form into a frontmatter merge patch holding only
/// the fields whose value changes (`null` clearing an optional field).
///
/// # Errors
///
/// Returns a sentence naming the first field that is not acceptable.
pub fn validate_fields(
    kind: Option<RecordType>,
    submitted: &Submitted,
    context: &ValidationContext<'_>,
) -> Result<Map<String, Value>, String> {
    if let Some(unknown) = submitted
        .keys()
        .find(|key| !fields(kind).iter().any(|spec| spec.key == key.as_str()))
    {
        return Err(format!("`{unknown}` is not a field of this record."));
    }
    let mut patch = Map::new();
    for spec in fields(kind) {
        let raw = match submitted.get(spec.key) {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(value)) => value.trim().to_owned(),
            Some(_) => return Err(format!("{} must be text.", spec.label)),
        };
        let held = text(context.current, spec.key);
        let value = if raw.is_empty() {
            if spec.required {
                return Err(format!("{} is required.", spec.label));
            }
            Value::Null
        } else {
            field_value(spec, raw, &held, context)?
        };
        if !unchanged(context.current, spec.key, &value) {
            patch.insert(spec.key.to_owned(), value);
        }
    }
    Ok(patch)
}

fn field_value(spec: &FieldSpec, raw: String, held: &str, context: &ValidationContext<'_>) -> Result<Value, String> {
    // A value the record already held is kept as it is, even outside today's vocabulary or format: an edit that
    // leaves a field alone must never rewrite it.
    let kept = raw == held;
    Ok(match spec.kind {
        FieldKind::Text => Value::String(raw),
        FieldKind::Date if kept || is_iso_date(&raw) => Value::String(raw),
        FieldKind::Date => return Err(format!("{} must be a date (YYYY-MM-DD).", spec.label)),
        FieldKind::Choice(allowed) if kept || allowed.contains(&raw.as_str()) => Value::String(raw),
        FieldKind::Choice(allowed) => {
            return Err(format!("{} must be one of: {}.", spec.label, allowed.join(", ")));
        }
        FieldKind::Entity if kept || context.entity_slugs.is_empty() || context.entity_slugs.contains(&raw) => {
            Value::String(raw)
        }
        FieldKind::Entity => return Err(format!("{} must be one of this vault's entities.", spec.label)),
        FieldKind::DecisionLink if kept || context.decision_ids.contains(&raw) => Value::String(raw),
        FieldKind::DecisionLink => return Err(format!("{} must name a decision in the register.", spec.label)),
        FieldKind::List => Value::Array(
            raw.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| Value::String(line.to_owned()))
                .collect(),
        ),
    })
}

#[cfg(test)]
#[path = "register_records_test.rs"]
mod tests;
