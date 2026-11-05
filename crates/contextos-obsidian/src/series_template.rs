//! The fixed fields a series template contributes: `type` always, plus each field a sidecar's `fixed_fields` names
//! that the template declares with a literal value.

use serde_json::{Map, Value};
use thiserror::Error;

/// The field every series template fixes.
const TYPE_FIELD: &str = "type";

/// Substrings that mark a template value as filled in or chosen at use rather than literal.
const NON_LITERAL_MARKERS: [&str; 5] = ["{{", "}}", "<%", "%>", "|"];

/// The inputs to read a template's fixed fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SeriesTemplateInput<'a> {
    /// The template note's parsed frontmatter.
    pub template: &'a Map<String, Value>,
    /// The sidecar's `fixed_fields`, beyond `type`.
    pub fixed_fields: &'a [String],
}

/// The fields a record of a series must carry unchanged, with the template's values and YAML types.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FixedFields(Map<String, Value>);

impl FixedFields {
    /// Returns every fixed field with its template value.
    #[must_use]
    pub const fn fields(&self) -> &Map<String, Value> {
        &self.0
    }
}

impl TryFrom<SeriesTemplateInput<'_>> for FixedFields {
    type Error = SeriesTemplateError;

    fn try_from(value: SeriesTemplateInput<'_>) -> Result<Self, Self::Error> {
        let mut fixed = Map::new();
        match value.template.get(TYPE_FIELD) {
            None => {
                return Err(SeriesTemplateError::Missing {
                    field: TYPE_FIELD.to_owned(),
                });
            }
            Some(declared) if literal(declared) => {
                fixed.insert(TYPE_FIELD.to_owned(), declared.clone());
            }
            Some(_) => {
                return Err(SeriesTemplateError::NotLiteral {
                    field: TYPE_FIELD.to_owned(),
                });
            }
        }
        for name in value.fixed_fields.iter().filter(|name| name.as_str() != TYPE_FIELD) {
            match value.template.get(name) {
                None => {}
                Some(declared) if literal(declared) => {
                    fixed.insert(name.clone(), declared.clone());
                }
                Some(_) => return Err(SeriesTemplateError::NotLiteral { field: name.clone() }),
            }
        }
        Ok(Self(fixed))
    }
}

/// Whether a template value can be stamped unchanged: a number, a boolean, or a non-empty string free of
/// placeholder (`{{ }}`), Templater (`<% %>`), and menu (`|`) markers. Anything else is filled in or chosen by whoever
/// uses the template, so it can never be a fixed value.
fn literal(value: &Value) -> bool {
    match value {
        Value::Number(_) | Value::Bool(_) => true,
        Value::String(text) => {
            !text.trim().is_empty() && !NON_LITERAL_MARKERS.iter().any(|marker| text.contains(marker))
        }
        Value::Null | Value::Array(_) | Value::Object(_) => false,
    }
}

/// Why a template cannot supply fixed fields (`format/series-template`).
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SeriesTemplateError {
    /// A fixed field's template value is a placeholder, expression, menu, empty value, list, or map.
    #[error("the template's {field} is not a literal value")]
    NotLiteral { field: String },
    /// The template declares no `type`.
    #[error("the template declares no {field}")]
    Missing { field: String },
}

#[cfg(test)]
#[path = "series_template_test.rs"]
mod tests;
