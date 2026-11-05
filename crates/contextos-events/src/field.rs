use std::fmt;

use serde_json::{Number, Value};

use crate::{EventsError, FieldRejection};

/// The longest field name accepted.
const MAX_FIELD_NAME_LEN: usize = 64;

/// Fields every event note carries or may carry, ahead of its kind's own.
const EVENT_FIELDS: [&str; 10] = [
    "type",
    "kind",
    "severity",
    "at",
    "entity",
    "subject",
    "subject_id",
    "subject_path",
    "subject_base",
    "actor",
];

/// Fields ordinary notes carry, which an event never does.
const NOTE_FIELDS: [&str; 6] = ["title", "created", "updated", "tags", "aliases", "description"];

/// Names the server stamps itself, so a recorder may not supply them. A subject's key, path, and base are read from
/// the note its link names, never taken from the recorder.
const SERVER_STAMPED: [&str; 6] = ["at", "path", "filename", "subject_id", "subject_path", "subject_base"];

/// A validated name for one of an event kind's own fields.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FieldName(String);

impl FieldName {
    /// The name as written in frontmatter.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Reports whether `name` is one of the fields every event carries.
    #[must_use]
    pub fn is_event_field(name: &str) -> bool {
        EVENT_FIELDS.contains(&name)
    }

    /// Reports whether `name` is a common note field, which events never carry.
    #[must_use]
    pub fn is_note_field(name: &str) -> bool {
        NOTE_FIELDS.contains(&name)
    }

    /// Reports whether `name` is stamped by the server and so cannot be supplied.
    #[must_use]
    pub fn is_server_stamped(name: &str) -> bool {
        SERVER_STAMPED.contains(&name)
    }
}

impl TryFrom<&str> for FieldName {
    type Error = EventsError;

    fn try_from(value: &str) -> Result<Self, EventsError> {
        let mut bytes = value.bytes();
        let valid = value.len() <= MAX_FIELD_NAME_LEN
            && bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
            && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-');
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(EventsError::InvalidFieldName {
                value: value.to_owned(),
            })
        }
    }
}

impl AsRef<str> for FieldName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FieldName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One frontmatter scalar of an event's own field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Scalar {
    Text(String),
    Number(Number),
    Bool(bool),
}

impl From<&str> for Scalar {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<String> for Scalar {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<i64> for Scalar {
    fn from(value: i64) -> Self {
        Self::Number(value.into())
    }
}

impl From<u64> for Scalar {
    fn from(value: u64) -> Self {
        Self::Number(value.into())
    }
}

impl From<bool> for Scalar {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl TryFrom<&Value> for Scalar {
    type Error = FieldRejection;

    fn try_from(value: &Value) -> Result<Self, FieldRejection> {
        match value {
            Value::String(text) if !text.contains(['\n', '\r']) => Ok(Self::Text(text.clone())),
            Value::Number(number) => Ok(Self::Number(number.clone())),
            Value::Bool(flag) => Ok(Self::Bool(*flag)),
            Value::Null | Value::String(_) | Value::Array(_) | Value::Object(_) => Err(FieldRejection::InvalidValue),
        }
    }
}

impl fmt::Display for Scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) => f.write_str(&single_quoted(text)),
            Self::Number(number) => write!(f, "{number}"),
            Self::Bool(flag) => write!(f, "{flag}"),
        }
    }
}

/// The value of one of an event kind's own fields: a scalar, or a flat list of scalars.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldValue {
    Scalar(Scalar),
    List(Vec<Scalar>),
}

impl<T: Into<Scalar>> From<T> for FieldValue {
    fn from(value: T) -> Self {
        Self::Scalar(value.into())
    }
}

impl TryFrom<&Value> for FieldValue {
    type Error = FieldRejection;

    fn try_from(value: &Value) -> Result<Self, FieldRejection> {
        match value {
            Value::Array(items) => items
                .iter()
                .map(Scalar::try_from)
                .collect::<Result<Vec<_>, _>>()
                .map(Self::List),
            other => Scalar::try_from(other).map(Self::Scalar),
        }
    }
}

impl fmt::Display for FieldValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scalar(scalar) => scalar.fmt(f),
            Self::List(items) => {
                f.write_str("[")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    item.fmt(f)?;
                }
                f.write_str("]")
            }
        }
    }
}

/// A YAML single-quoted scalar: quotes are doubled, and whitespace runs (line breaks included) collapse to one space,
/// so the value stays on its frontmatter line whatever a caller supplied.
#[must_use]
pub(crate) fn single_quoted(value: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("'{}'", collapsed.replace('\'', "''"))
}

#[cfg(test)]
#[path = "field_test.rs"]
mod tests;
