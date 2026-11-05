//! The cross-reference link fields a base's sidecar declares in its
//! `link_fields` map: which frontmatter fields hold links to other records,
//! whether each holds one link or a list, and which field on the linked
//! record is its reciprocal.
//!
//! ```yaml
//! link_fields:
//!   depends_on: {inverse: blocks}
//!   blocks: {inverse: depends_on}
//!   related: {inverse: related}
//!   parent: {single: true}
//! ```

use serde_json::{Map, Value};

use crate::BaseSidecarError;

/// The longest field name a sidecar may declare.
const MAX_LINK_FIELD_NAME_LEN: usize = 64;

/// Frontmatter keys a note's own metadata, or the sidecar itself, already
/// uses: declaring one as a link field would let a link edit corrupt it.
const RESERVED_FIELD_NAMES: &[&str] = &[
    "id",
    "type",
    "title",
    "entity",
    "status",
    "state",
    "created",
    "updated",
    "tags",
    "aliases",
    "description",
    "summary",
    "excerpt",
    "template",
    "base",
    "link_fields",
    "series_templates",
    "fixed_fields",
    "key_prefix",
    "key_prefixes",
    "key_format",
    "last_id",
    "last_create",
    "last_update",
];

/// One declared link field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkField {
    name: String,
    single: bool,
    inverse: Option<String>,
}

impl LinkField {
    /// Returns the frontmatter field's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the field holds a single link rather than a list of them.
    #[must_use]
    pub const fn single(&self) -> bool {
        self.single
    }

    /// The field on the linked record that reciprocates this one, if any.
    #[must_use]
    pub fn inverse(&self) -> Option<&str> {
        self.inverse.as_deref()
    }
}

/// Whether `name` is a frontmatter key the server or the sidecar itself already uses, compared without regard to case.
pub(crate) fn reserved_field_name(name: &str) -> bool {
    RESERVED_FIELD_NAMES.contains(&name.to_ascii_lowercase().as_str())
}

/// Whether `name` is a plain frontmatter identifier: ASCII letters, digits,
/// and underscores, starting with a letter, and not a reserved key. Sidecar frontmatter is untrusted
/// and a declared name is later used as a frontmatter key.
fn valid_field_name(name: &str) -> bool {
    let mut characters = name.chars();
    !reserved_field_name(name)
        && name.len() <= MAX_LINK_FIELD_NAME_LEN
        && characters.next().is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn wrong_type() -> BaseSidecarError {
    BaseSidecarError::WrongFieldType { field: "link_fields" }
}

/// Reads the `link_fields` declaration from a sidecar's frontmatter: empty
/// when there is none, an error when it is malformed.
pub(crate) fn link_fields_from(frontmatter: &Map<String, Value>) -> Result<Vec<LinkField>, BaseSidecarError> {
    let Some(declared) = frontmatter.get("link_fields") else {
        return Ok(Vec::new());
    };
    let Value::Object(declared) = declared else {
        return Err(wrong_type());
    };

    let mut fields: Vec<LinkField> = Vec::with_capacity(declared.len());
    for (name, options) in declared {
        if !valid_field_name(name) {
            return Err(BaseSidecarError::InvalidLinkField { name: name.clone() });
        }
        if fields.iter().any(|field| field.name.eq_ignore_ascii_case(name)) {
            return Err(BaseSidecarError::DuplicateLinkField { name: name.clone() });
        }
        let Value::Object(options) = options else {
            return Err(wrong_type());
        };

        let mut single = false;
        let mut inverse = None;
        for (option, value) in options {
            match (option.as_str(), value) {
                ("single", Value::Bool(value)) => single = *value,
                ("inverse", Value::String(value)) => inverse = Some(value.clone()),
                ("single" | "inverse", _) => return Err(wrong_type()),
                _ => {
                    return Err(BaseSidecarError::UnknownLinkFieldOption {
                        field: name.clone(),
                        option: option.clone(),
                    });
                }
            }
        }
        fields.push(LinkField {
            name: name.clone(),
            single,
            inverse,
        });
    }

    for field in &fields {
        let Some(inverse) = field.inverse.as_deref() else {
            continue;
        };
        let Some(other) = fields.iter().find(|candidate| candidate.name == inverse) else {
            return Err(BaseSidecarError::UndeclaredLinkInverse {
                field: field.name.clone(),
                inverse: inverse.to_owned(),
            });
        };
        if other.inverse.as_deref() != Some(field.name.as_str()) {
            return Err(BaseSidecarError::AsymmetricLinkInverse {
                field: field.name.clone(),
                inverse: inverse.to_owned(),
            });
        }
    }
    Ok(fields)
}

/// The `link_fields` frontmatter value for `fields`, for a sidecar created
/// from scratch.
pub(crate) fn link_fields_map(fields: &[LinkField]) -> Map<String, Value> {
    fields
        .iter()
        .map(|field| {
            let mut options = Map::new();
            if field.single {
                options.insert("single".to_owned(), Value::Bool(true));
            }
            if let Some(inverse) = &field.inverse {
                options.insert("inverse".to_owned(), Value::String(inverse.clone()));
            }
            (field.name.clone(), Value::Object(options))
        })
        .collect()
}

#[cfg(test)]
#[path = "base_link_fields_test.rs"]
mod tests;
