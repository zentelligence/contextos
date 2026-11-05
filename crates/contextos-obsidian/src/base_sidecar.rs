use serde_json::{Map, Number, Value};
use thiserror::Error;

use crate::base_link_fields::{LinkField, link_fields_from, link_fields_map};
use crate::base_series_templates::SeriesTemplates;

/// The literal `key_format` a sequential (keyed) sidecar carries.
const SEQUENTIAL_KEY_FORMAT: &str = "{key_prefix}-{key_id}";
/// The literal `key_format` of a sidecar with no key scheme.
const FILEPATH_KEY_FORMAT: &str = "FILEPATH";
/// The longest key prefix a sidecar may declare: a prefix becomes part of a
/// filename, so it is bounded as well as restricted to a safe alphabet.
const MAX_KEY_PREFIX_LEN: usize = 32;

/// One independent id series within a keyed sidecar: a prefix and the
/// number of records assigned under it so far.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeySeries {
    prefix: String,
    last_id: u64,
}

impl KeySeries {
    /// Returns this series' prefix.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Returns the number of records assigned under this prefix so far.
    #[must_use]
    pub const fn last_id(&self) -> u64 {
        self.last_id
    }
}

/// A sidecar's key-assignment scheme: one or more independent, prefixed
/// id series, or `FILEPATH` for a `.base` with no declared key scheme.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BaseKeyScheme {
    /// Every declared series keeps its own counter. `default_prefix`, when
    /// present, names the series used when a caller does not choose one;
    /// it is always one of `series`.
    Sequential {
        default_prefix: Option<String>,
        series: Vec<KeySeries>,
    },
    FilePath,
}

/// A `.base` file's paired `<name>.md` sidecar: its key scheme and
/// record-activity timestamps, consumed by `base_record_create`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaseSidecar {
    scheme: BaseKeyScheme,
    link_fields: Vec<LinkField>,
    series_templates: SeriesTemplates,
    last_create: String,
    last_update: String,
}

/// Inputs for a freshly created sidecar with no prior record activity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BaseSidecarInput<'a> {
    /// The default series, used when a record is created without naming one.
    pub key_prefix: Option<&'a str>,
    /// Further series to declare. A prefix equal to `key_prefix` is declared
    /// once, not twice.
    pub key_prefixes: &'a [String],
    pub timestamp: &'a str,
}

impl TryFrom<BaseSidecarInput<'_>> for BaseSidecar {
    type Error = BaseSidecarError;

    fn try_from(value: BaseSidecarInput<'_>) -> Result<Self, Self::Error> {
        let mut prefixes: Vec<&str> = Vec::with_capacity(value.key_prefixes.len().saturating_add(1));
        if let Some(default) = value.key_prefix {
            prefixes.push(default);
        }
        prefixes.extend(
            value
                .key_prefixes
                .iter()
                .map(String::as_str)
                .filter(|prefix| Some(*prefix) != value.key_prefix),
        );

        let scheme = if prefixes.is_empty() {
            BaseKeyScheme::FilePath
        } else {
            let series = declare_series(prefixes.into_iter().map(|prefix| (prefix, 0)))?;
            BaseKeyScheme::Sequential {
                default_prefix: value.key_prefix.map(str::to_owned),
                series,
            }
        };
        Ok(Self {
            scheme,
            link_fields: Vec::new(),
            series_templates: SeriesTemplates::default(),
            last_create: value.timestamp.to_owned(),
            last_update: value.timestamp.to_owned(),
        })
    }
}

impl BaseSidecar {
    /// Returns this sidecar's key-assignment scheme.
    #[must_use]
    pub const fn scheme(&self) -> &BaseKeyScheme {
        &self.scheme
    }

    /// Returns every declared series, in declaration order; empty for a
    /// `FILEPATH`-keyed sidecar.
    #[must_use]
    pub fn series(&self) -> &[KeySeries] {
        match &self.scheme {
            BaseKeyScheme::Sequential { series, .. } => series,
            BaseKeyScheme::FilePath => &[],
        }
    }

    /// Returns the cross-reference link fields this sidecar declares, in
    /// declaration order; empty when it declares none.
    #[must_use]
    pub fn link_fields(&self) -> &[LinkField] {
        &self.link_fields
    }

    /// Returns the template declaration (`template`, `series_templates`, `fixed_fields`) this sidecar carries.
    #[must_use]
    pub const fn series_templates(&self) -> &SeriesTemplates {
        &self.series_templates
    }

    /// Returns the default series' prefix, when this sidecar declares one.
    #[must_use]
    pub fn default_prefix(&self) -> Option<&str> {
        match &self.scheme {
            BaseKeyScheme::Sequential { default_prefix, .. } => default_prefix.as_deref(),
            BaseKeyScheme::FilePath => None,
        }
    }

    /// Returns the timestamp of this sidecar's most recent record creation.
    #[must_use]
    pub fn last_create(&self) -> &str {
        &self.last_create
    }

    /// Returns the timestamp of this sidecar's most recent activity of any kind.
    #[must_use]
    pub fn last_update(&self) -> &str {
        &self.last_update
    }

    /// Returns the index of the series a record is created under: the
    /// `requested` prefix when it is declared, otherwise the default series.
    /// `None` for a `FILEPATH`-keyed sidecar, which assigns no ids.
    fn resolve(&self, requested: Option<&str>) -> Result<Option<usize>, BaseSidecarError> {
        match &self.scheme {
            BaseKeyScheme::FilePath => match requested {
                Some(requested) => Err(BaseSidecarError::KeyPrefixNotApplicable {
                    requested: requested.to_owned(),
                }),
                None => Ok(None),
            },
            BaseKeyScheme::Sequential { default_prefix, series } => {
                let wanted = requested.or(default_prefix.as_deref());
                let Some(wanted) = wanted else {
                    return Err(BaseSidecarError::KeyPrefixRequired {
                        declared: declared_prefixes(series),
                    });
                };
                series
                    .iter()
                    .position(|candidate| candidate.prefix == wanted)
                    .map(Some)
                    .ok_or_else(|| BaseSidecarError::UndeclaredKeyPrefix {
                        requested: wanted.to_owned(),
                        declared: declared_prefixes(series),
                    })
            }
        }
    }

    /// Returns the next zero-padded key the chosen series would assign, or
    /// `None` for a `FILEPATH`-keyed sidecar (no id to assign).
    ///
    /// # Errors
    ///
    /// Returns an error when `requested` is not a declared prefix, when it
    /// is omitted and there is no default series, or when a prefix is
    /// requested of a `FILEPATH`-keyed sidecar.
    pub fn next_key(&self, requested: Option<&str>) -> Result<Option<String>, BaseSidecarError> {
        let Some(index) = self.resolve(requested)? else {
            return Ok(None);
        };
        Ok(self.series().get(index).map(|series| {
            let next_id = series.last_id.saturating_add(1);
            format!("{}-{next_id:03}", series.prefix)
        }))
    }

    /// Returns the prefix of the series a record is created under: the `requested` prefix when it is declared,
    /// otherwise the default series. `None` for a `FILEPATH`-keyed sidecar, which assigns no ids.
    ///
    /// # Errors
    ///
    /// Fails in the same cases as [`Self::next_key`].
    pub fn series_prefix(&self, requested: Option<&str>) -> Result<Option<&str>, BaseSidecarError> {
        Ok(self
            .resolve(requested)?
            .and_then(|index| self.series().get(index))
            .map(KeySeries::prefix))
    }

    /// Returns a copy whose chosen series counts at least `highest_assigned` records as assigned, for a record
    /// whose key was assigned without going through this sidecar (written by hand or by another tool).
    ///
    /// A counter is a floor, never a ceiling: a series already at or beyond `highest_assigned` is returned
    /// unchanged, so the key of a record that has since been deleted is not handed out again. Catching up is not
    /// record activity, so neither timestamp moves. A `FILEPATH`-keyed sidecar has no counter and is returned as it
    /// is.
    ///
    /// # Errors
    ///
    /// Fails in the same cases as [`Self::next_key`].
    pub fn caught_up_to(&self, requested: Option<&str>, highest_assigned: u64) -> Result<Self, BaseSidecarError> {
        let index = self.resolve(requested)?;
        let mut caught_up = self.clone();
        if let (Some(index), BaseKeyScheme::Sequential { series, .. }) = (index, &mut caught_up.scheme)
            && let Some(series) = series.get_mut(index)
        {
            series.last_id = series.last_id.max(highest_assigned);
        }
        Ok(caught_up)
    }

    /// Returns a copy advanced by one record: the chosen series' counter
    /// incremented (a `FILEPATH`-keyed sidecar has no counter to advance),
    /// `last_create` and `last_update` both stamped to `timestamp`.
    ///
    /// # Errors
    ///
    /// Fails in the same cases as [`Self::next_key`].
    pub fn advance(&self, requested: Option<&str>, timestamp: &str) -> Result<Self, BaseSidecarError> {
        let index = self.resolve(requested)?;
        let mut scheme = self.scheme.clone();
        if let (Some(index), BaseKeyScheme::Sequential { series, .. }) = (index, &mut scheme)
            && let Some(series) = series.get_mut(index)
        {
            series.last_id = series.last_id.saturating_add(1);
        }
        Ok(Self {
            scheme,
            link_fields: self.link_fields.clone(),
            series_templates: self.series_templates.clone(),
            last_create: timestamp.to_owned(),
            last_update: timestamp.to_owned(),
        })
    }

    /// Returns a copy with only `last_update` refreshed: a `base_apply`
    /// schema edit is activity on the `.base` file, but not a new record.
    #[must_use]
    pub fn touch(&self, timestamp: &str) -> Self {
        Self {
            scheme: self.scheme.clone(),
            link_fields: self.link_fields.clone(),
            series_templates: self.series_templates.clone(),
            last_create: self.last_create.clone(),
            last_update: timestamp.to_owned(),
        }
    }
}

fn declared_prefixes(series: &[KeySeries]) -> Vec<String> {
    series.iter().map(|series| series.prefix.clone()).collect()
}

/// Whether `prefix` is safe to declare: it becomes part of a generated key
/// and, lower-cased, of a filename, and sidecar frontmatter is untrusted.
fn validate_key_prefix(prefix: &str) -> Result<(), BaseSidecarError> {
    let mut characters = prefix.chars();
    let valid = prefix.len() <= MAX_KEY_PREFIX_LEN
        && characters.next().is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|character| character.is_ascii_alphanumeric());
    if valid {
        Ok(())
    } else {
        Err(BaseSidecarError::InvalidKeyPrefix {
            prefix: prefix.to_owned(),
        })
    }
}

/// Validates every `(prefix, last_id)` pair and rejects two prefixes that
/// differ only by case, since the generated filenames are lower-cased and
/// would collide.
fn declare_series<'a>(pairs: impl Iterator<Item = (&'a str, u64)>) -> Result<Vec<KeySeries>, BaseSidecarError> {
    let mut series: Vec<KeySeries> = Vec::new();
    for (prefix, last_id) in pairs {
        validate_key_prefix(prefix)?;
        if series
            .iter()
            .any(|declared| declared.prefix.eq_ignore_ascii_case(prefix))
        {
            return Err(BaseSidecarError::DuplicateKeyPrefix {
                prefix: prefix.to_owned(),
            });
        }
        series.push(KeySeries {
            prefix: prefix.to_owned(),
            last_id,
        });
    }
    Ok(series)
}

fn counters_map(series: &[KeySeries]) -> Map<String, Value> {
    series
        .iter()
        .map(|series| (series.prefix.clone(), Value::Number(Number::from(series.last_id))))
        .collect()
}

/// A full frontmatter map for a sidecar being created from scratch.
impl From<&BaseSidecar> for Map<String, Value> {
    fn from(value: &BaseSidecar) -> Self {
        let mut map = Self::new();
        match &value.scheme {
            BaseKeyScheme::Sequential { default_prefix, series } => {
                if let Some(default_prefix) = default_prefix {
                    map.insert("key_prefix".to_owned(), Value::String(default_prefix.clone()));
                }
                map.insert("key_format".to_owned(), Value::String(SEQUENTIAL_KEY_FORMAT.to_owned()));
                map.insert("key_prefixes".to_owned(), Value::Object(counters_map(series)));
            }
            BaseKeyScheme::FilePath => {
                map.insert("key_format".to_owned(), Value::String(FILEPATH_KEY_FORMAT.to_owned()));
                map.insert("last_id".to_owned(), Value::Number(Number::from(0_u64)));
            }
        }
        if !value.link_fields.is_empty() {
            map.insert(
                "link_fields".to_owned(),
                Value::Object(link_fields_map(&value.link_fields)),
            );
        }
        map.insert("last_create".to_owned(), Value::String(value.last_create.clone()));
        map.insert("last_update".to_owned(), Value::String(value.last_update.clone()));
        map
    }
}

/// An RFC 7386 merge patch that brings an existing sidecar note's
/// frontmatter up to date with a [`BaseSidecar`], leaving every unrelated
/// key (a `template` link, aliases, and so on) exactly as it was.
///
/// Unlike the full map a new sidecar is created from, it explicitly removes
/// the legacy `last_id` counter (and a stale `key_prefix` when there is no
/// default series), so writing a legacy single-series sidecar migrates it to
/// the `key_prefixes` map form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaseSidecarPatch(pub Map<String, Value>);

impl From<&BaseSidecar> for BaseSidecarPatch {
    fn from(value: &BaseSidecar) -> Self {
        let mut map = Map::from(value);
        // The link-field declaration belongs to whoever wrote the sidecar;
        // a rewrite never touches it.
        map.shift_remove("link_fields");
        match &value.scheme {
            BaseKeyScheme::Sequential { default_prefix, .. } => {
                if default_prefix.is_none() {
                    map.insert("key_prefix".to_owned(), Value::Null);
                }
                map.insert("last_id".to_owned(), Value::Null);
            }
            // The counter of a sidecar with no key scheme is never assigned
            // from, so a patch leaves whatever it holds alone.
            BaseKeyScheme::FilePath => {
                map.shift_remove("last_id");
            }
        }
        Self(map)
    }
}

impl TryFrom<&Map<String, Value>> for BaseSidecar {
    type Error = BaseSidecarError;

    fn try_from(frontmatter: &Map<String, Value>) -> Result<Self, Self::Error> {
        let key_format = required_str(frontmatter, "key_format")?;
        let scheme = if key_format == FILEPATH_KEY_FORMAT {
            if frontmatter.contains_key("key_prefixes") {
                return Err(BaseSidecarError::FilePathWithKeyPrefixes);
            }
            required_u64(frontmatter, "last_id")?;
            BaseKeyScheme::FilePath
        } else if let Some(map) = frontmatter.get("key_prefixes") {
            sequential_from_map(frontmatter, map)?
        } else {
            sequential_from_legacy(frontmatter)?
        };
        let link_fields = link_fields_from(frontmatter)?;
        let series_templates = SeriesTemplates::try_from(frontmatter)?;
        let last_create = required_str(frontmatter, "last_create")?.to_owned();
        let last_update = required_str(frontmatter, "last_update")?.to_owned();
        Ok(Self {
            scheme,
            link_fields,
            series_templates,
            last_create,
            last_update,
        })
    }
}

/// The original single-series form: `key_prefix` plus `last_id`, read as a
/// one-entry map whose only series is the default.
fn sequential_from_legacy(frontmatter: &Map<String, Value>) -> Result<BaseKeyScheme, BaseSidecarError> {
    let key_prefix = required_str(frontmatter, "key_prefix")?;
    let last_id = required_u64(frontmatter, "last_id")?;
    let series = declare_series(std::iter::once((key_prefix, last_id)))?;
    Ok(BaseKeyScheme::Sequential {
        default_prefix: Some(key_prefix.to_owned()),
        series,
    })
}

/// The multi-series form: a `key_prefixes` map of prefix to last id, with
/// an optional `key_prefix` naming the default series.
fn sequential_from_map(frontmatter: &Map<String, Value>, map: &Value) -> Result<BaseKeyScheme, BaseSidecarError> {
    let Value::Object(map) = map else {
        return Err(BaseSidecarError::WrongFieldType { field: "key_prefixes" });
    };
    if frontmatter.contains_key("last_id") {
        return Err(BaseSidecarError::AmbiguousCounters);
    }
    if map.is_empty() {
        return Err(BaseSidecarError::EmptyKeyPrefixes);
    }

    let mut counters = Vec::with_capacity(map.len());
    for (prefix, counter) in map {
        let last_id = counter
            .as_u64()
            .ok_or(BaseSidecarError::WrongFieldType { field: "key_prefixes" })?;
        counters.push((prefix.as_str(), last_id));
    }
    let series = declare_series(counters.into_iter())?;

    let default_prefix = match frontmatter.get("key_prefix") {
        None => None,
        Some(Value::String(default)) => {
            validate_key_prefix(default)?;
            if !series.iter().any(|declared| declared.prefix == *default) {
                return Err(BaseSidecarError::UndeclaredDefaultPrefix {
                    prefix: default.clone(),
                });
            }
            Some(default.clone())
        }
        Some(_) => return Err(BaseSidecarError::WrongFieldType { field: "key_prefix" }),
    };
    Ok(BaseKeyScheme::Sequential { default_prefix, series })
}

fn required_str<'a>(frontmatter: &'a Map<String, Value>, field: &'static str) -> Result<&'a str, BaseSidecarError> {
    frontmatter
        .get(field)
        .ok_or(BaseSidecarError::MissingField { field })?
        .as_str()
        .ok_or(BaseSidecarError::WrongFieldType { field })
}

fn required_u64(frontmatter: &Map<String, Value>, field: &'static str) -> Result<u64, BaseSidecarError> {
    frontmatter
        .get(field)
        .ok_or(BaseSidecarError::MissingField { field })?
        .as_u64()
        .ok_or(BaseSidecarError::WrongFieldType { field })
}

/// Typed sidecar failures: a malformed stored sidecar
/// (`format/base-sidecar`), or a request that its declared key scheme cannot
/// satisfy (`io/invalid-key-prefix`).
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum BaseSidecarError {
    #[error("sidecar frontmatter is missing required field: {field}")]
    MissingField { field: &'static str },
    #[error("sidecar frontmatter field {field} has the wrong type")]
    WrongFieldType { field: &'static str },
    #[error("key prefix {prefix:?} is invalid: use ASCII letters and digits only, starting with a letter")]
    InvalidKeyPrefix { prefix: String },
    #[error("key prefix {prefix:?} is declared more than once (prefixes are compared without regard to case)")]
    DuplicateKeyPrefix { prefix: String },
    #[error("the default key_prefix {prefix:?} is not one of the declared key_prefixes")]
    UndeclaredDefaultPrefix { prefix: String },
    #[error("key_prefixes declares no prefixes")]
    EmptyKeyPrefixes,
    #[error("sidecar has both last_id and key_prefixes; each counter must live in only one place")]
    AmbiguousCounters,
    #[error("key_prefixes cannot be declared on a FILEPATH-keyed sidecar")]
    FilePathWithKeyPrefixes,
    #[error("link field {name:?} is invalid: use ASCII letters, digits, and underscores, starting with a letter")]
    InvalidLinkField { name: String },
    #[error("link field {name:?} is declared more than once (names are compared without regard to case)")]
    DuplicateLinkField { name: String },
    #[error("link field {field:?} names an inverse, {inverse:?}, that is not a declared link field")]
    UndeclaredLinkInverse { field: String, inverse: String },
    #[error("link field {field:?} names {inverse:?} as its inverse, but {inverse:?} does not name it back")]
    AsymmetricLinkInverse { field: String, inverse: String },
    #[error("link field {field:?} has an unknown option {option:?}; the options are single and inverse")]
    UnknownLinkFieldOption { field: String, option: String },
    #[error(
        "series_templates names prefix {prefix:?}, which key_prefixes does not declare (prefixes are case-sensitive)"
    )]
    UndeclaredSeriesTemplate { prefix: String },
    #[error("series_templates cannot be declared on a FILEPATH-keyed sidecar, which has no series")]
    SeriesTemplatesOnFilePath,
    #[error(
        "fixed_fields name {name:?} is invalid: use a field the server and sidecar do not already use, once, as ASCII letters, digits, underscores, and hyphens starting with a letter"
    )]
    InvalidFixedField { name: String },
    #[error("fixed_fields lists {count} names; declare between 1 and 16")]
    FixedFieldsSize { count: usize },
    #[error("prefix {requested:?} is not declared by this base's sidecar (declared: {})", .declared.join(", "))]
    UndeclaredKeyPrefix { requested: String, declared: Vec<String> },
    #[error("this base's sidecar declares no default key prefix, so prefix is required (declared: {})", .declared.join(", "))]
    KeyPrefixRequired { declared: Vec<String> },
    #[error("prefix {requested:?} cannot be used: this base's sidecar is FILEPATH-keyed and assigns no ids")]
    KeyPrefixNotApplicable { requested: String },
}

impl BaseSidecarError {
    /// Returns the stable machine-readable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingField { .. }
            | Self::WrongFieldType { .. }
            | Self::InvalidKeyPrefix { .. }
            | Self::DuplicateKeyPrefix { .. }
            | Self::UndeclaredDefaultPrefix { .. }
            | Self::EmptyKeyPrefixes
            | Self::AmbiguousCounters
            | Self::FilePathWithKeyPrefixes
            | Self::InvalidLinkField { .. }
            | Self::DuplicateLinkField { .. }
            | Self::UndeclaredLinkInverse { .. }
            | Self::AsymmetricLinkInverse { .. }
            | Self::UnknownLinkFieldOption { .. }
            | Self::UndeclaredSeriesTemplate { .. }
            | Self::SeriesTemplatesOnFilePath
            | Self::InvalidFixedField { .. }
            | Self::FixedFieldsSize { .. } => "format/base-sidecar",
            Self::UndeclaredKeyPrefix { .. } | Self::KeyPrefixRequired { .. } | Self::KeyPrefixNotApplicable { .. } => {
                "io/invalid-key-prefix"
            }
        }
    }

    /// Returns an actionable correction for this failure.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::MissingField { .. } | Self::WrongFieldType { .. } => {
                "Correct the sidecar's key_format, key_prefix, key_prefixes, last_id, last_create, last_update, template, series_templates, and fixed_fields fields, and retry."
            }
            Self::InvalidKeyPrefix { .. } | Self::DuplicateKeyPrefix { .. } => {
                "Use distinct key prefixes of ASCII letters and digits, each starting with a letter, and retry."
            }
            Self::UndeclaredDefaultPrefix { .. } | Self::EmptyKeyPrefixes => {
                "Declare every prefix, including the default key_prefix, in the sidecar's key_prefixes map, and retry."
            }
            Self::InvalidLinkField { .. } | Self::DuplicateLinkField { .. } | Self::UnknownLinkFieldOption { .. } => {
                "Declare each link field once, as an identifier of ASCII letters, digits, and underscores with only the single and inverse options, and retry."
            }
            Self::UndeclaredLinkInverse { .. } | Self::AsymmetricLinkInverse { .. } => {
                "Declare the inverse field too, and have each of the pair name the other as its inverse, then retry."
            }
            Self::UndeclaredSeriesTemplate { .. } | Self::SeriesTemplatesOnFilePath => {
                "Key each series_templates entry by a prefix the sidecar's key_prefixes declares, exactly as spelled, and retry."
            }
            Self::InvalidFixedField { .. } | Self::FixedFieldsSize { .. } => {
                "List between 1 and 16 distinct fixed_fields, each a plain field name the server and the sidecar do not already use, and retry."
            }
            Self::AmbiguousCounters => {
                "Remove last_id from the sidecar, keeping each counter in its key_prefixes entry, and retry."
            }
            Self::FilePathWithKeyPrefixes => {
                "Set key_format to \"{key_prefix}-{key_id}\" to use key_prefixes, or remove key_prefixes, and retry."
            }
            Self::UndeclaredKeyPrefix { .. } | Self::KeyPrefixRequired { .. } => {
                "Pass one of the declared prefixes, or declare the new prefix in the sidecar's key_prefixes map first."
            }
            Self::KeyPrefixNotApplicable { .. } => {
                "Omit prefix, or give this base's sidecar a key scheme by declaring key_prefixes, and retry."
            }
        }
    }
}

#[cfg(test)]
#[path = "base_sidecar_test.rs"]
mod tests;
