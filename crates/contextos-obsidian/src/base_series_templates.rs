//! The template each key series of a base follows, as a base's hand-authored sidecar declares it with `template`,
//! `series_templates`, and `fixed_fields`.

use serde_json::{Map, Value};

use crate::BaseSidecarError;
use crate::base_link_fields::{link_fields_from, reserved_field_name};

/// A template link exactly as the sidecar wrote it, for example `[[templates/task|Task]]`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TemplateLink(String);

impl AsRef<str> for TemplateLink {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// The longest `fixed_fields` list a sidecar may declare.
const MAX_FIXED_FIELDS: usize = 16;

/// The longest field name `fixed_fields` may carry.
const MAX_FIXED_FIELD_NAME_LEN: usize = 64;

/// The template declaration of one sidecar: which link each series follows and which extra fields its templates fix.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SeriesTemplates {
    every_series: Option<TemplateLink>,
    several_templates: bool,
    per_series: Vec<(String, TemplateLink)>,
    fixed_fields: Vec<String>,
}

impl SeriesTemplates {
    /// Returns the template link a series follows: `prefix` is the series, or `None` for a sidecar with no series.
    #[must_use]
    pub fn template_for(&self, prefix: Option<&str>) -> Option<&TemplateLink> {
        prefix
            .and_then(|prefix| {
                self.per_series
                    .iter()
                    .find(|(declared, _)| declared == prefix)
                    .map(|(_, link)| link)
            })
            .or(self.every_series.as_ref())
    }

    /// Returns the field names beyond `type` that this base's templates fix, in declaration order.
    #[must_use]
    pub fn fixed_fields(&self) -> &[String] {
        &self.fixed_fields
    }

    /// Whether the sidecar declares any template: one for every series, or at least one for a series of its own.
    #[must_use]
    pub const fn declares_any(&self) -> bool {
        self.every_series.is_some() || !self.per_series.is_empty()
    }

    /// Whether `template` lists several links, which names no template for a series without its own entry.
    #[must_use]
    pub const fn lists_several_templates(&self) -> bool {
        self.several_templates
    }

    /// Returns each series that declares its own template, with that template, in declaration order.
    #[must_use]
    pub fn per_series_templates(&self) -> &[(String, TemplateLink)] {
        &self.per_series
    }
}

impl TryFrom<&Map<String, Value>> for SeriesTemplates {
    type Error = BaseSidecarError;

    fn try_from(frontmatter: &Map<String, Value>) -> Result<Self, Self::Error> {
        let (every_series, several_templates) = match frontmatter.get("template") {
            None => (None, false),
            Some(Value::String(link)) => (Some(TemplateLink(link.clone())), false),
            Some(Value::Array(links)) => {
                let links = links
                    .iter()
                    .map(|link| link.as_str().map(|link| TemplateLink(link.to_owned())))
                    .collect::<Option<Vec<_>>>()
                    .ok_or(BaseSidecarError::WrongFieldType { field: "template" })?;
                match <[TemplateLink; 1]>::try_from(links) {
                    Ok([only]) => (Some(only), false),
                    Err(links) => (None, links.len() > 1),
                }
            }
            Some(_) => return Err(BaseSidecarError::WrongFieldType { field: "template" }),
        };
        Ok(Self {
            every_series,
            several_templates,
            per_series: series_templates_from(frontmatter)?,
            fixed_fields: fixed_fields_from(frontmatter)?,
        })
    }
}

/// The prefixes the sidecar's key scheme declares, or `None` for a `FILEPATH` sidecar.
fn declared_prefixes(frontmatter: &Map<String, Value>) -> Option<Vec<&str>> {
    if frontmatter.get("key_format").and_then(Value::as_str) == Some("FILEPATH") {
        return None;
    }
    Some(match frontmatter.get("key_prefixes") {
        Some(Value::Object(prefixes)) => prefixes.keys().map(String::as_str).collect(),
        _ => frontmatter
            .get("key_prefix")
            .and_then(Value::as_str)
            .into_iter()
            .collect(),
    })
}

fn series_templates_from(frontmatter: &Map<String, Value>) -> Result<Vec<(String, TemplateLink)>, BaseSidecarError> {
    let Some(declared) = frontmatter.get("series_templates") else {
        return Ok(Vec::new());
    };
    let Value::Object(declared) = declared else {
        return Err(BaseSidecarError::WrongFieldType {
            field: "series_templates",
        });
    };
    let prefixes = declared_prefixes(frontmatter).ok_or(BaseSidecarError::SeriesTemplatesOnFilePath)?;
    declared
        .iter()
        .map(|(prefix, link)| {
            if !prefixes.contains(&prefix.as_str()) {
                return Err(BaseSidecarError::UndeclaredSeriesTemplate { prefix: prefix.clone() });
            }
            let link = link.as_str().ok_or(BaseSidecarError::WrongFieldType {
                field: "series_templates",
            })?;
            Ok((prefix.clone(), TemplateLink(link.to_owned())))
        })
        .collect()
}

/// Whether `name` can be fixed: a plain identifier of ASCII letters, digits, underscores, and hyphens starting with a
/// letter, that is `type` or no key the server or sidecar already uses.
fn valid_fixed_field(name: &str) -> bool {
    let mut characters = name.chars();
    (name == "type" || !reserved_field_name(name))
        && name.len() <= MAX_FIXED_FIELD_NAME_LEN
        && characters.next().is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '-')
}

fn fixed_fields_from(frontmatter: &Map<String, Value>) -> Result<Vec<String>, BaseSidecarError> {
    let Some(declared) = frontmatter.get("fixed_fields") else {
        return Ok(Vec::new());
    };
    let wrong_type = BaseSidecarError::WrongFieldType { field: "fixed_fields" };
    let Value::Array(declared) = declared else {
        return Err(wrong_type);
    };
    if declared.is_empty() || declared.len() > MAX_FIXED_FIELDS {
        return Err(BaseSidecarError::FixedFieldsSize { count: declared.len() });
    }
    let link_fields = link_fields_from(frontmatter)?;
    let mut names: Vec<String> = Vec::with_capacity(declared.len());
    for name in declared {
        let Some(name) = name.as_str() else {
            return Err(wrong_type);
        };
        let clashes = !valid_fixed_field(name)
            || names.iter().any(|seen| seen.eq_ignore_ascii_case(name))
            || link_fields.iter().any(|field| field.name().eq_ignore_ascii_case(name));
        if clashes {
            return Err(BaseSidecarError::InvalidFixedField { name: name.to_owned() });
        }
        names.push(name.to_owned());
    }
    Ok(names)
}

#[cfg(test)]
#[path = "base_series_templates_test.rs"]
mod tests;
