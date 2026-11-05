use std::collections::BTreeMap;

use contextos_obsidian::FrontmatterDocument;
use serde_json::{Map, Value};

use crate::{DefinitionRejection, EventsError, FieldName, KindName, ServerKind, Severity};

/// The note type a kind definition carries.
const DEFINITION_TYPE: &str = "event-kind";

/// Whether a recorded event of a kind names the note it is about.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubjectRule {
    /// Every record must give a subject.
    Required,
    /// A record may give a subject.
    Optional,
    /// A record may not give a subject.
    Absent,
}

/// A definition note's name and text, as read from `registry/events/`.
#[derive(Clone, Copy, Debug)]
pub struct KindDefinitionSource<'a> {
    /// The note's file name without its `.md` extension, which must equal the kind it defines.
    pub stem: &'a str,
    pub text: &'a str,
}

/// A vault's declaration of one kind clients may record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KindDefinition {
    pub kind: KindName,
    pub label: String,
    pub description: String,
    /// The severities a record may carry; never empty and never repeating.
    pub severities: Vec<Severity>,
    /// The severity used when a record names none; one of `severities`.
    pub default_severity: Severity,
    pub required: Vec<FieldName>,
    pub optional: Vec<FieldName>,
    pub subject: SubjectRule,
    /// The field that holds a key the server assigns; never also a required or optional field.
    pub id_field: Option<FieldName>,
    /// How long this kind's `info` events are kept, overriding the vault's window.
    pub retention_days: Option<u32>,
}

const fn rejected(field: &'static str, reason: DefinitionRejection) -> EventsError {
    EventsError::InvalidDefinition { field, reason }
}

fn text_field<'a>(frontmatter: &'a Map<String, Value>, field: &'static str) -> Result<&'a str, EventsError> {
    match frontmatter.get(field) {
        None => Err(rejected(field, DefinitionRejection::Missing)),
        Some(Value::String(text)) if text.trim().is_empty() => Err(rejected(field, DefinitionRejection::Empty)),
        Some(Value::String(text)) => Ok(text),
        Some(_) => Err(rejected(field, DefinitionRejection::WrongType)),
    }
}

/// Reads a list of text values; a missing key is an empty list when `optional`.
fn string_list<'a>(value: Option<&'a Value>, field: &'static str, optional: bool) -> Result<Vec<&'a str>, EventsError> {
    match value {
        None if optional => Ok(Vec::new()),
        None => Err(rejected(field, DefinitionRejection::Missing)),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .ok_or_else(|| rejected(field, DefinitionRejection::WrongType))
            })
            .collect(),
        Some(_) => Err(rejected(field, DefinitionRejection::WrongType)),
    }
}

fn field_name(field: &'static str, name: &str) -> Result<FieldName, EventsError> {
    if FieldName::is_event_field(name) || FieldName::is_note_field(name) || FieldName::is_server_stamped(name) {
        return Err(rejected(field, DefinitionRejection::ReservedName));
    }
    FieldName::try_from(name).map_err(|_| rejected(field, DefinitionRejection::InvalidName))
}

fn severities(frontmatter: &Map<String, Value>) -> Result<(Vec<Severity>, Severity), EventsError> {
    let names = string_list(frontmatter.get("severities"), "severities", false)?;
    if names.is_empty() {
        return Err(rejected("severities", DefinitionRejection::Empty));
    }
    let mut allowed = Vec::new();
    for name in names {
        let severity = Severity::try_from(name).map_err(|_| rejected("severities", DefinitionRejection::NotAllowed))?;
        if allowed.contains(&severity) {
            return Err(rejected("severities", DefinitionRejection::Duplicate));
        }
        allowed.push(severity);
    }
    let default = Severity::try_from(text_field(frontmatter, "default_severity")?)
        .ok()
        .filter(|severity| allowed.contains(severity))
        .ok_or_else(|| rejected("default_severity", DefinitionRejection::NotAllowed))?;
    Ok((allowed, default))
}

fn declared_fields(frontmatter: &Map<String, Value>) -> Result<(Vec<FieldName>, Vec<FieldName>), EventsError> {
    let fields = match frontmatter.get("fields") {
        None => None,
        Some(Value::Object(fields)) => Some(fields),
        Some(_) => return Err(rejected("fields", DefinitionRejection::WrongType)),
    };
    let list = |key: &str| string_list(fields.and_then(|fields| fields.get(key)), "fields", true);
    let (required, optional) = (list("required")?, list("optional")?);
    let mut seen: Vec<String> = Vec::new();
    let mut names = |names: Vec<&str>| {
        names
            .into_iter()
            .map(|name| {
                let parsed = field_name("fields", name)?;
                if seen.iter().any(|earlier| earlier == name) {
                    return Err(rejected("fields", DefinitionRejection::Duplicate));
                }
                seen.push(name.to_owned());
                Ok(parsed)
            })
            .collect::<Result<Vec<_>, _>>()
    };
    let required = names(required)?;
    let optional = names(optional)?;
    Ok((required, optional))
}

fn subject_rule(frontmatter: &Map<String, Value>) -> Result<SubjectRule, EventsError> {
    match text_field(frontmatter, "subject")? {
        "required" => Ok(SubjectRule::Required),
        "optional" => Ok(SubjectRule::Optional),
        "none" => Ok(SubjectRule::Absent),
        _ => Err(rejected("subject", DefinitionRejection::NotAllowed)),
    }
}

fn retention_days(frontmatter: &Map<String, Value>) -> Result<Option<u32>, EventsError> {
    match frontmatter.get("retention_days") {
        None => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .filter(|days| *days > 0)
            .and_then(|days| u32::try_from(days).ok())
            .map(Some)
            .ok_or_else(|| rejected("retention_days", DefinitionRejection::NotPositive)),
        Some(_) => Err(rejected("retention_days", DefinitionRejection::WrongType)),
    }
}

fn kind_name(frontmatter: &Map<String, Value>, stem: &str) -> Result<KindName, EventsError> {
    let declared = text_field(frontmatter, "kind")?;
    let kind = KindName::try_from(declared).map_err(|_| rejected("kind", DefinitionRejection::InvalidName))?;
    if declared != stem {
        return Err(rejected("kind", DefinitionRejection::BasenameMismatch));
    }
    if ServerKind::try_from(declared).is_ok() {
        return Err(EventsError::ServerFiredKind {
            kind: declared.to_owned(),
        });
    }
    Ok(kind)
}

impl TryFrom<KindDefinitionSource<'_>> for KindDefinition {
    type Error = EventsError;

    fn try_from(source: KindDefinitionSource<'_>) -> Result<Self, EventsError> {
        let document = FrontmatterDocument::try_from(source.text)
            .map_err(|_| rejected("frontmatter", DefinitionRejection::Malformed))?;
        let frontmatter = document.frontmatter();
        if text_field(frontmatter, "type")? != DEFINITION_TYPE {
            return Err(rejected("type", DefinitionRejection::NotAllowed));
        }
        let kind = kind_name(frontmatter, source.stem)?;
        let label = text_field(frontmatter, "label")?.to_owned();
        let description = text_field(frontmatter, "description")?.to_owned();
        let (severities, default_severity) = severities(frontmatter)?;
        let (required, optional) = declared_fields(frontmatter)?;
        let subject = subject_rule(frontmatter)?;
        let id_field = if frontmatter.contains_key("id_field") {
            let field = field_name("id_field", text_field(frontmatter, "id_field")?)?;
            if required.contains(&field) || optional.contains(&field) {
                return Err(rejected("id_field", DefinitionRejection::DeclaredAsField));
            }
            Some(field)
        } else {
            None
        };
        Ok(Self {
            kind,
            label,
            description,
            severities,
            default_severity,
            required,
            optional,
            subject,
            id_field,
            retention_days: retention_days(frontmatter)?,
        })
    }
}

/// The kinds a vault has declared usably, by name. A definition that could not be read is simply absent, so recording
/// its kind is refused as unknown until it is corrected.
#[derive(Clone, Debug, Default)]
pub struct KindRegistry {
    kinds: BTreeMap<String, KindDefinition>,
}

impl KindRegistry {
    /// Adds a definition.
    ///
    /// # Errors
    ///
    /// Returns [`EventsError::DuplicateDefinition`] when the kind is already defined; the first definition is kept.
    pub fn insert(&mut self, definition: KindDefinition) -> Result<(), EventsError> {
        let name = definition.kind.as_str().to_owned();
        if self.kinds.contains_key(&name) {
            return Err(EventsError::DuplicateDefinition { kind: name });
        }
        self.kinds.insert(name, definition);
        Ok(())
    }

    /// Finds the definition a record of `kind` must follow.
    ///
    /// # Errors
    ///
    /// Returns [`EventsError::ServerFiredKind`] for a kind only the server fires, and [`EventsError::UnknownKind`]
    /// when no kind was given or none usable is defined under that name.
    pub fn lookup(&self, kind: Option<&str>) -> Result<&KindDefinition, EventsError> {
        let name = kind.map(str::trim).filter(|name| !name.is_empty());
        if let Some(name) = name
            && ServerKind::try_from(name).is_ok()
        {
            return Err(EventsError::ServerFiredKind { kind: name.to_owned() });
        }
        name.and_then(|name| self.kinds.get(name))
            .ok_or_else(|| EventsError::UnknownKind {
                kind: name.map(str::to_owned),
            })
    }

    /// Each kind's own retention window, by kind name, for the kinds that declare one.
    #[must_use]
    pub fn retention_windows(&self) -> Vec<(String, u32)> {
        self.kinds
            .iter()
            .filter_map(|(name, definition)| definition.retention_days.map(|days| (name.clone(), days)))
            .collect()
    }

    /// The number of kinds defined.
    #[must_use]
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    /// Reports whether no kind is defined.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }
}

#[cfg(test)]
#[path = "definition_test.rs"]
mod tests;
