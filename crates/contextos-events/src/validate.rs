use serde_json::{Map, Value};
use time::OffsetDateTime;

use crate::{
    EventRecord, EventSubject, EventsError, FieldName, FieldRejection, FieldValue, KindDefinition, KindRegistry,
    SeriesRejection, Severity, SubjectRejection, SubjectRule, link_target, resolve_entity,
};

/// A client's request to record an event, as it arrived and before any of it is trusted.
#[derive(Clone, Debug, Default)]
pub struct RecordRequest {
    pub kind: Option<String>,
    pub severity: Option<String>,
    /// The link to the note the event is about.
    pub subject: Option<String>,
    pub entity: Option<String>,
    /// Text for the note's body, after its heading.
    pub content: Option<String>,
    /// Every other frontmatter key the caller supplied, the kind's own fields among them.
    pub fields: Map<String, Value>,
}

/// What the server knows when it records an event, and the caller cannot say.
#[derive(Clone, Debug)]
pub struct RecordContext {
    /// When the event is recorded, from an injected clock.
    pub at: OffsetDateTime,
    /// The entity of the subject note, when the subject has one.
    pub subject_entity: Option<String>,
    /// The note the subject link names, when it names exactly one.
    pub subject_note: Option<SubjectNote>,
    /// The recorder's own resolved entity, when it has one.
    pub recorder_entity: Option<String>,
    /// The key the server assigned from the kind's series. Required exactly when the kind declares a key field.
    pub assigned_key: Option<String>,
}

/// The note an event's subject link names, as the server found it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubjectNote {
    /// Its vault-relative path, `.md` included.
    pub path: String,
    /// Its key and base, when it is a record of a keyed base.
    pub record: Option<SubjectRecord>,
}

/// A record of a keyed base: its key and the vault path of its base.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubjectRecord {
    pub key: String,
    pub base: String,
}

fn refused_field(definition: &KindDefinition, field: &str, reason: FieldRejection) -> EventsError {
    EventsError::InvalidFields {
        kind: definition.kind.to_string(),
        field: field.to_owned(),
        reason,
    }
}

fn refused_subject(definition: &KindDefinition, reason: SubjectRejection) -> EventsError {
    EventsError::InvalidSubject {
        kind: definition.kind.to_string(),
        reason,
    }
}

/// A single, non-blank line, trimmed.
fn single_line(text: &str) -> Option<&str> {
    let text = text.trim();
    (!text.is_empty() && !text.contains(['\n', '\r'])).then_some(text)
}

impl KindDefinition {
    /// Validates a request to record an event of this kind and builds the record to write.
    ///
    /// Nothing is written and no key is consumed by a refusal: the caller assigns the key only once this has
    /// succeeded in principle, and passes it in `context`.
    ///
    /// # Errors
    ///
    /// Returns the refusal that applies first: a server-stamped name supplied, a severity the kind does not allow,
    /// a subject that breaks the kind's rule, a field that is missing, undeclared, a common note field, or not
    /// representable in frontmatter, or a key assigned (or not) against the kind's declaration.
    pub fn validate(&self, request: &RecordRequest, context: &RecordContext) -> Result<EventRecord, EventsError> {
        self.refuse_stamped_names(&request.fields)?;
        let severity = self.severity(request.severity.as_deref())?;
        let subject = self.subject(request, context.subject_note.as_ref())?;
        let fields = self.fields(&request.fields, context.assigned_key.as_deref())?;
        Ok(EventRecord {
            kind: self.kind.clone(),
            severity,
            at: context.at,
            entity: resolve_entity(
                context.subject_entity.as_deref(),
                request.entity.as_deref(),
                context.recorder_entity.as_deref(),
            ),
            subject,
            actor: None,
            fields,
            heading: self.label.clone(),
            content: request.content.clone(),
        })
    }

    fn refuse_stamped_names(&self, supplied: &Map<String, Value>) -> Result<(), EventsError> {
        let stamped = |name: &str| {
            FieldName::is_server_stamped(name) || self.id_field.as_ref().is_some_and(|field| field.as_str() == name)
        };
        match supplied.keys().find(|name| stamped(name)) {
            Some(field) => Err(EventsError::ServerStamped { field: field.clone() }),
            None => Ok(()),
        }
    }

    fn severity(&self, given: Option<&str>) -> Result<Severity, EventsError> {
        let Some(given) = given else {
            return Ok(self.default_severity);
        };
        let severity = Severity::try_from(given)?;
        if self.severities.contains(&severity) {
            Ok(severity)
        } else {
            Err(EventsError::SeverityNotAllowed {
                kind: self.kind.to_string(),
                severity: given.to_owned(),
            })
        }
    }

    /// The subject the request names, stamped with what `note` (the note its link names, when the server found one)
    /// says it is. A record of a keyed base must be linked by its key, as every reference to it is.
    fn subject(
        &self,
        request: &RecordRequest,
        note: Option<&SubjectNote>,
    ) -> Result<Option<EventSubject>, EventsError> {
        let given = request
            .subject
            .as_deref()
            .map(|text| {
                single_line(text)
                    .map(str::to_owned)
                    .ok_or_else(|| refused_subject(self, SubjectRejection::Malformed))
            })
            .transpose()?;
        let link = match (self.subject, given) {
            (SubjectRule::Absent | SubjectRule::Optional, None) => return Ok(None),
            (SubjectRule::Required, None) => return Err(refused_subject(self, SubjectRejection::Required)),
            (SubjectRule::Absent, Some(_)) => return Err(refused_subject(self, SubjectRejection::NotAccepted)),
            (SubjectRule::Required | SubjectRule::Optional, Some(link)) => link,
        };
        // A subject is one whole wikilink: a bare path, an embed, or a link inside other text names no subject the
        // clearing rules can compare, and would let a record be named by its path unchecked.
        if link_target(&link).is_none() {
            return Err(refused_subject(self, SubjectRejection::Malformed));
        }
        let Some(note) = note else {
            return Ok(Some(EventSubject {
                link,
                id: None,
                path: None,
                base: None,
            }));
        };
        let Some(record) = &note.record else {
            return Ok(Some(EventSubject {
                link,
                id: None,
                path: Some(note.path.clone()),
                base: None,
            }));
        };
        let by_key = EventSubject::for_record(&note.path, &record.key, &record.base);
        let same_target = match (link_target(&link), link_target(&by_key.link)) {
            (Some(given), Some(expected)) => given.eq_ignore_ascii_case(&expected),
            _ => false,
        };
        if !same_target {
            return Err(refused_subject(self, SubjectRejection::NotByKey));
        }
        Ok(Some(EventSubject { link, ..by_key }))
    }

    fn fields(
        &self,
        supplied: &Map<String, Value>,
        assigned_key: Option<&str>,
    ) -> Result<Vec<(FieldName, FieldValue)>, EventsError> {
        let declared = |name: &str| {
            self.required
                .iter()
                .chain(&self.optional)
                .any(|field| field.as_str() == name)
        };
        for name in supplied.keys() {
            if FieldName::is_note_field(name) {
                return Err(refused_field(self, name, FieldRejection::CommonNoteField));
            }
            if !declared(name) {
                return Err(refused_field(self, name, FieldRejection::Undeclared));
            }
        }
        if let Some(missing) = self
            .required
            .iter()
            .find(|field| !supplied.contains_key(field.as_str()))
        {
            return Err(refused_field(self, missing.as_str(), FieldRejection::Missing));
        }
        let mut fields = Vec::new();
        match (&self.id_field, assigned_key) {
            (Some(field), Some(key)) => fields.push((field.clone(), FieldValue::from(key))),
            (Some(_), None) => {
                return Err(EventsError::KeyNotAssigned {
                    kind: self.kind.to_string(),
                });
            }
            (None, Some(_)) => {
                return Err(EventsError::KeyNotExpected {
                    kind: self.kind.to_string(),
                });
            }
            (None, None) => {}
        }
        for field in self.required.iter().chain(&self.optional) {
            if let Some(value) = supplied.get(field.as_str()) {
                let value =
                    FieldValue::try_from(value).map_err(|reason| refused_field(self, field.as_str(), reason))?;
                fields.push((field.clone(), value));
            }
        }
        Ok(fields)
    }
}

impl KindDefinition {
    /// Decides the key series a record is assigned its key from.
    ///
    /// `mapped` holds the series the events sidecar maps to this kind. A kind with a key field needs the request to
    /// name one of them; a kind without one accepts no series. Nothing is reserved here, so a refusal costs no key.
    ///
    /// # Errors
    ///
    /// Returns [`EventsError::InvalidSeries`] when the request's series is missing, unexpected, or not mapped here.
    pub fn select_series(&self, requested: Option<&str>, mapped: &[&str]) -> Result<Option<String>, EventsError> {
        let refuse = |reason| EventsError::InvalidSeries {
            kind: self.kind.to_string(),
            prefix: requested.map(str::to_owned),
            reason,
        };
        match (&self.id_field, requested) {
            (None, None) => Ok(None),
            (None, Some(_)) => Err(refuse(SeriesRejection::NotAccepted)),
            (Some(_), None) => Err(refuse(SeriesRejection::Missing)),
            (Some(_), Some(series)) if mapped.contains(&series) => Ok(Some(series.to_owned())),
            (Some(_), Some(_)) => Err(refuse(SeriesRejection::NotMapped)),
        }
    }
}

impl KindRegistry {
    /// Finds the definition for the request's kind and validates the request against it.
    ///
    /// # Errors
    ///
    /// Returns the lookup refusal ([`EventsError::UnknownKind`], [`EventsError::ServerFiredKind`]) or the first
    /// refusal [`KindDefinition::validate`] gives.
    pub fn validate(&self, request: &RecordRequest, context: &RecordContext) -> Result<EventRecord, EventsError> {
        self.lookup(request.kind.as_deref())?.validate(request, context)
    }
}

#[cfg(test)]
#[path = "validate_test.rs"]
mod tests;
