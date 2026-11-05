//! The triple-colon fence vocabulary a vault declares in `.contextos/fences.toml`.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The manifest format this build reads.
pub const SUPPORTED_FENCE_SCHEMA: u32 = 1;

/// The vocabulary this build carries, the same file the vault scaffold ships.
pub const DEFAULT_FENCES_TOML: &str = include_str!("../default-fences.toml");

/// What a fence is for, which decides how it is drawn.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum FenceRole {
    /// A titled container.
    Callout,
    /// A layout container.
    Layout,
    /// A labelled banner.
    Status,
    /// A break that print styles honour and screens do not show.
    Page,
}

/// The tone a fence carries; the theme decides its colours.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum FenceTone {
    Neutral,
    Positive,
    Caution,
    Critical,
    Emphasis,
    Analysis,
}

/// One fence the vocabulary recognises, after aliases are followed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FenceSpec {
    /// The name the fence is drawn as: its own, or the name it is an alias of.
    pub name: String,
    pub role: FenceRole,
    pub tone: Option<FenceTone>,
    pub self_closing: bool,
}

/// The kind of value an attribute takes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum AttributeType {
    /// Any text.
    Text,
    /// Letters, digits, `-`, and `_`.
    Id,
    /// One word: letters, digits, `-`, and `_`.
    Token,
    /// A whole number of no more than six digits.
    Integer,
    /// `true` or `false`, or the bare key, meaning true.
    Boolean,
}

/// One attribute the vocabulary recognises.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttributeSpec {
    pub kind: AttributeType,
    /// The words a token is limited to, when the manifest lists them.
    pub values: Option<Vec<String>>,
}

/// A vault's fence vocabulary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FenceVocabulary {
    standard: Option<String>,
    fences: BTreeMap<String, FenceSpec>,
    attributes: BTreeMap<String, AttributeSpec>,
}

/// A manifest that cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum VocabularyError {
    #[error("the fence manifest is not valid: {0}")]
    Invalid(String),
    #[error("the fence manifest's schema {found} is newer than this build reads (schema {SUPPORTED_FENCE_SCHEMA})")]
    UnsupportedSchema { found: u32 },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    /// Read first, on its own, so a newer format is named rather than misread; declared here so it is not unknown.
    #[serde(rename = "schema")]
    _schema: u32,
    standard: Option<String>,
    #[serde(default)]
    fences: BTreeMap<String, RawFence>,
    #[serde(default)]
    attributes: BTreeMap<String, RawAttribute>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFence {
    role: Option<FenceRole>,
    tone: Option<FenceTone>,
    #[serde(default)]
    self_closing: bool,
    alias_of: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAttribute {
    #[serde(rename = "type")]
    kind: AttributeType,
    values: Option<Vec<String>>,
}

/// Whether `name` is a fence or attribute name a fence line can carry: lower-case letters, digits, and `-`.
pub(crate) fn is_fence_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl TryFrom<&str> for FenceVocabulary {
    type Error = VocabularyError;

    fn try_from(source: &str) -> Result<Self, Self::Error> {
        let invalid = |reason: String| VocabularyError::Invalid(reason);
        // The schema is read first and on its own, so a newer format is named as such rather than refused for keys
        // this build does not know.
        let table = toml::from_str::<toml::Table>(source).map_err(|error| invalid(error.message().to_owned()))?;
        let schema = table
            .get("schema")
            .and_then(toml::Value::as_integer)
            .ok_or_else(|| invalid("schema is missing or not a whole number".to_owned()))?;
        if schema > i64::from(SUPPORTED_FENCE_SCHEMA) {
            return Err(VocabularyError::UnsupportedSchema {
                found: u32::try_from(schema).unwrap_or(u32::MAX),
            });
        }
        if schema < 1 {
            return Err(invalid(format!("schema {schema} is not a manifest format")));
        }
        let raw = toml::from_str::<RawManifest>(source).map_err(|error| invalid(error.message().to_owned()))?;

        let mut fences = BTreeMap::new();
        for (name, fence) in &raw.fences {
            if !is_fence_name(name) {
                return Err(invalid(format!(
                    "the fence name {name:?} may use only lower-case letters, digits, and -"
                )));
            }
            let spec = match &fence.alias_of {
                Some(target) => {
                    if fence.role.is_some() || fence.tone.is_some() || fence.self_closing {
                        return Err(invalid(format!(
                            "{name} is an alias, so it takes no role, tone, or self_closing of its own"
                        )));
                    }
                    let original = raw
                        .fences
                        .get(target)
                        .ok_or_else(|| invalid(format!("{name} is an alias of {target}, which is not a fence")))?;
                    if original.alias_of.is_some() {
                        return Err(invalid(format!(
                            "{name} is an alias of {target}, which is itself an alias"
                        )));
                    }
                    FenceSpec {
                        name: target.clone(),
                        role: original.role.ok_or_else(|| invalid(format!("{target} has no role")))?,
                        tone: original.tone,
                        self_closing: original.self_closing,
                    }
                }
                None => FenceSpec {
                    name: name.clone(),
                    role: fence.role.ok_or_else(|| invalid(format!("{name} has no role")))?,
                    tone: fence.tone,
                    self_closing: fence.self_closing,
                },
            };
            fences.insert(name.clone(), spec);
        }

        let mut attributes = BTreeMap::new();
        for (name, attribute) in raw.attributes {
            if !is_fence_name(&name) {
                return Err(invalid(format!(
                    "the attribute name {name:?} may use only lower-case letters, digits, and -"
                )));
            }
            if attribute.values.is_some() && attribute.kind != AttributeType::Token {
                return Err(invalid(format!(
                    "{name} lists values, which only a token attribute may"
                )));
            }
            attributes.insert(
                name,
                AttributeSpec {
                    kind: attribute.kind,
                    values: attribute.values,
                },
            );
        }
        Ok(Self {
            standard: raw.standard,
            fences,
            attributes,
        })
    }
}

impl Default for FenceVocabulary {
    /// The vocabulary this build carries. It is parsed from [`DEFAULT_FENCES_TOML`], which a test proves valid, so the
    /// empty vocabulary it falls back to is never reached.
    fn default() -> Self {
        Self::try_from(DEFAULT_FENCES_TOML).unwrap_or_else(|_| Self {
            standard: None,
            fences: BTreeMap::new(),
            attributes: BTreeMap::new(),
        })
    }
}

impl FenceVocabulary {
    /// The fence called `name`, with any alias followed.
    #[must_use]
    pub fn fence(&self, name: &str) -> Option<&FenceSpec> {
        self.fences.get(name)
    }

    /// The attribute called `name`.
    #[must_use]
    pub fn attribute(&self, name: &str) -> Option<&AttributeSpec> {
        self.attributes.get(name)
    }

    /// The label of the standard the manifest says it encodes.
    #[must_use]
    pub fn standard(&self) -> Option<&str> {
        self.standard.as_deref()
    }
}

#[cfg(test)]
#[path = "fence_vocabulary_test.rs"]
mod tests;
