//! Versioned Rhai workflow packages: the distributable, versioned
//! unit a workflow definition ships in, independent of any vault's own `registry/workflows/` discovery.
//!
//! `manifest.toml` describes the release and nothing else: the package's name, version, and digest, and the files it
//! lists. A package lists exactly one of `workflows.toml` ([`crate::workflows_file::WorkflowsFile`]) and `library.toml`
//! ([`crate::library_manifest::LibraryManifest`]), which says what kind of package it is and declares its modules,
//! templates, exports, and libraries. Each workflow is a folder of its own, `{workflow}/`, with a `workflow.toml` that
//! declares it ([`WorkflowDefinition`]). A package's own manifest never embeds a vault name or operator path.
//!
//! This module is pure domain: parsing and validation only, no filesystem, network, or MCP I/O of any kind, matching
//! this crate's own architectural contract. Installing a package into host storage and saving or
//! scheduling a job that references an installed package are deliberately out of this module's scope.

use std::collections::HashSet;
use std::fmt;

use contextos_core::ContentHash;
use semver::{Version, VersionReq};
use serde::Deserialize;
use serde_json::Value;
use time::format_description::FormatItem;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use time::{Date, OffsetDateTime, Time};

pub use crate::package_error::PackageError;
use crate::package_name::PackageName;
use crate::script_modules::DeclaredModules;
pub use crate::workflow_definition::{RequiredToolContract, WorkflowDefinition};
use crate::workflow_files::WorkflowFilePath;

/// The file a library release lists beside `manifest.toml`.
pub const LIBRARY_FILE_NAME: &str = "library.toml";
/// The file a workflow release lists beside `manifest.toml`.
pub const WORKFLOWS_FILE_NAME: &str = "workflows.toml";

const DATE_FORMAT: &[FormatItem<'_>] = format_description!("[year]-[month]-[day]");
const TIME_FORMAT: &[FormatItem<'_>] = format_description!("[hour]:[minute]:[second]");

/// A package's own immutable identity: name, semantic version, and a
/// content digest computed over its full bundle (the package's own
/// concern to compute; this type only validates the digest's shape).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageIdentity {
    pub name: PackageName,
    pub version: Version,
    pub digest: ContentDigest,
}

/// A `sha256:<64 lowercase hex characters>` content digest: the sole
/// construction boundary for a validated digest string, matching
/// `VaultPath::try_new`'s established idiom for a validated newtype in
/// this codebase (`security.md`) rather than a bare `String`.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ContentDigest(String);

impl ContentDigest {
    /// # Errors
    ///
    /// Returns [`PackageError::MalformedDigest`] when `raw` is not
    /// `sha256:` followed by exactly 64 lowercase hexadecimal characters.
    pub fn try_new(raw: impl Into<String>) -> Result<Self, PackageError> {
        let raw = raw.into();
        let Some(hex) = raw.strip_prefix("sha256:") else {
            return Err(PackageError::MalformedDigest(raw));
        };
        if hex.len() == 64 && hex.chars().all(|character| matches!(character, '0'..='9' | 'a'..='f')) {
            Ok(Self(raw))
        } else {
            Err(PackageError::MalformedDigest(raw))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How this package's bundle content arrived at the host. Only
/// `Uploaded` is populated by this pass: fetching a pinned published
/// artefact from a URL is a deliberately deferred distribution source
/// (operator direction, 2026-09-23), not implemented anywhere in this
/// workspace yet. The enum exists so a future `Fetched` source can be
/// added without reshaping this type's existing callers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageProvenance {
    Uploaded { digest: ContentDigest },
}

/// A type tag for one typed input or output field, recursive for `Array`
/// and `Object` so a field can describe an array's own element type or an
/// object's own declared properties, not just that it is "an array" or
/// "an object" with no further shape. The four temporal kinds exist
/// because JSON itself has no native date/time type: a workflow's actual
/// wire data is always a JSON string or number at this boundary
/// (`bridge.rs`'s `arguments_to_json`/`result_to_dynamic`), so declaring
/// one of these kinds is what lets [`validate_and_convert`] parse and
/// re-serialise it to one normalised representation on entry/exit, rather
/// than accepting whatever ad hoc string shape happened to arrive.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldKind {
    String,
    Number,
    Bool,
    Null,
    /// A calendar date with no time component, ISO 8601 `YYYY-MM-DD`.
    Date,
    /// A time of day with no date component, ISO 8601 `HH:MM:SS` (24-hour).
    Time,
    /// A full date and time with an offset, RFC 3339 (e.g.
    /// `2026-09-23T10:00:00Z`).
    DateTime,
    /// Whole seconds since the Unix epoch, as a JSON integer.
    Timestamp,
    /// The name of a vault configured on the primary MCP server: a plain name, never a path.
    Vault,
    /// A homogeneous list; `items` describes every element's own declared
    /// kind.
    Array {
        items: Box<FieldKind>,
    },
    /// A structured value; `properties` names its own declared fields.
    /// A property present in a real JSON object but not declared here
    /// passes through [`validate_and_convert`] unchanged and unvalidated:
    /// this is a "must at least contain" shape, not an exact one.
    Object {
        properties: Vec<TypedField>,
    },
}

impl fmt::Display for FieldKind {
    /// The kind's name as a workflow declares it.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Bool => "bool",
            Self::Null => "null",
            Self::Date => "date",
            Self::Time => "time",
            Self::DateTime => "datetime",
            Self::Timestamp => "timestamp",
            Self::Vault => "vault",
            Self::Array { .. } => "array",
            Self::Object { .. } => "object",
        })
    }
}

/// One typed field a workflow definition's own input or output declares.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypedField {
    pub name: String,
    pub kind: FieldKind,
    /// Whether a run must supply a value. Declared with `required = false` to make an input optional; a missing
    /// optional input is absent from a run's inputs, and no default is ever applied.
    pub required: bool,
}

/// Validates `value` against `kind` and returns its normalised form: an
/// unchanged clone for `String`/`Number`/`Bool`/`Null`, a re-serialised
/// normalised string for the three date/time kinds (so two valid but
/// differently formatted inputs, e.g. an RFC 3339 datetime with `+00:00`
/// versus `Z`, converge on one representation), the validated integer for
/// `Timestamp`, and a recursively converted array/object for `Array`/
/// `Object`.
///
/// This function is pure and standalone. A run calls it for each supplied
/// input value (`SuppliedInputs`) and for each output a script sets with
/// `set_output`, so both reach their consumer in one normalised form.
///
/// # Errors
///
/// Returns [`PackageError::TypeMismatch`] when `value`'s own JSON type
/// does not match `kind`, or [`PackageError::InvalidTemporalValue`] when
/// a date/time/timestamp value does not parse as its declared kind.
pub fn validate_and_convert(value: &Value, kind: &FieldKind) -> Result<Value, PackageError> {
    match kind {
        FieldKind::String => expect_type(value, Value::is_string, "string"),
        FieldKind::Number => expect_type(value, Value::is_number, "number"),
        FieldKind::Bool => expect_type(value, Value::is_boolean, "bool"),
        FieldKind::Null => expect_type(value, Value::is_null, "null"),
        FieldKind::Date => convert_temporal(value, "date", |raw| {
            Date::parse(raw, DATE_FORMAT)
                .map_err(|source| source.to_string())
                .and_then(|date| date.format(DATE_FORMAT).map_err(|source| source.to_string()))
        }),
        FieldKind::Time => convert_temporal(value, "time", |raw| {
            Time::parse(raw, TIME_FORMAT)
                .map_err(|source| source.to_string())
                .and_then(|time| time.format(TIME_FORMAT).map_err(|source| source.to_string()))
        }),
        FieldKind::DateTime => convert_temporal(value, "datetime", |raw| {
            OffsetDateTime::parse(raw, &Rfc3339)
                .map_err(|source| source.to_string())
                .and_then(|instant| instant.format(&Rfc3339).map_err(|source| source.to_string()))
        }),
        FieldKind::Vault => {
            let Some(name) = value.as_str() else {
                return Err(PackageError::TypeMismatch {
                    expected: "vault name (a string)",
                    found: value_type_name(value),
                });
            };
            if name.is_empty() || name.contains([':', '/', '\\']) || name.starts_with('.') {
                return Err(PackageError::InvalidVaultName { value: name.to_owned() });
            }
            Ok(value.clone())
        }
        FieldKind::Timestamp => {
            let Some(seconds) = value.as_i64() else {
                return Err(PackageError::TypeMismatch {
                    expected: "timestamp (a whole number of seconds)",
                    found: value_type_name(value),
                });
            };
            OffsetDateTime::from_unix_timestamp(seconds).map_err(|source| PackageError::InvalidTemporalValue {
                kind: "timestamp",
                value: seconds.to_string(),
                message: source.to_string(),
            })?;
            Ok(Value::Number(seconds.into()))
        }
        FieldKind::Array { items } => {
            let Value::Array(elements) = value else {
                return Err(PackageError::TypeMismatch {
                    expected: "array",
                    found: value_type_name(value),
                });
            };
            let converted = elements
                .iter()
                .map(|element| validate_and_convert(element, items))
                .collect::<Result<_, _>>()?;
            Ok(Value::Array(converted))
        }
        FieldKind::Object { properties } => {
            let Value::Object(map) = value else {
                return Err(PackageError::TypeMismatch {
                    expected: "object",
                    found: value_type_name(value),
                });
            };
            let mut converted = map.clone();
            for property in properties {
                let Some(existing) = map.get(&property.name) else {
                    if property.required {
                        return Err(PackageError::MissingProperty(property.name.clone()));
                    }
                    continue;
                };
                converted.insert(property.name.clone(), validate_and_convert(existing, &property.kind)?);
            }
            Ok(Value::Object(converted))
        }
    }
}

fn expect_type(value: &Value, matches: fn(&Value) -> bool, expected: &'static str) -> Result<Value, PackageError> {
    if matches(value) {
        Ok(value.clone())
    } else {
        Err(PackageError::TypeMismatch {
            expected,
            found: value_type_name(value),
        })
    }
}

fn convert_temporal(
    value: &Value,
    kind: &'static str,
    reformat: impl FnOnce(&str) -> Result<String, String>,
) -> Result<Value, PackageError> {
    let Some(raw) = value.as_str() else {
        return Err(PackageError::TypeMismatch {
            expected: kind,
            found: value_type_name(value),
        });
    };
    reformat(raw)
        .map(Value::String)
        .map_err(|message| PackageError::InvalidTemporalValue {
            kind,
            value: raw.to_owned(),
            message,
        })
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// A requirement on a library: a name and a semver requirement, never a resolved concrete version. Resolution happens
/// in [`crate::dependency::resolve_dependencies`] against a caller-supplied installed set; this crate has no host-level
/// package store of its own to resolve against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageDependency {
    pub name: PackageName,
    pub requirement: VersionReq,
}

/// One file a release holds besides its manifest: where it is, and the SHA-256 of its bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseFile {
    pub path: WorkflowFilePath,
    pub hash: ContentHash,
}

/// A fully parsed and validated `manifest.toml`: the release, and the files it lists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageManifest {
    pub identity: PackageIdentity,
    pub provenance: PackageProvenance,
    /// Every file the release holds besides `manifest.toml`, each once, in the order the manifest lists them, so a
    /// store can tell a complete release from one with a file missing, added, or changed.
    pub files: Vec<ReleaseFile>,
}

impl PackageManifest {
    /// Whether the release is a library: it lists the `library.toml` beside `manifest.toml`.
    #[must_use]
    pub fn is_library(&self) -> bool {
        self.lists(LIBRARY_FILE_NAME)
    }

    /// The name of the package file the release lists beside `manifest.toml`: `library.toml` for a library and
    /// `workflows.toml` for a workflow package. The one definition of which package file a release has.
    #[must_use]
    pub fn package_file_name(&self) -> &'static str {
        if self.is_library() {
            LIBRARY_FILE_NAME
        } else {
            WORKFLOWS_FILE_NAME
        }
    }

    fn lists(&self, path: &str) -> bool {
        self.files.iter().any(|file| file.path.as_str() == path)
    }

    /// Parses and fully validates a `manifest.toml` from its own TOML source.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError`] naming the first validation failure found.
    pub fn parse(source: &str) -> Result<Self, PackageError> {
        let raw: PackageManifestRaw =
            toml::from_str(source).map_err(|error| PackageError::Malformed(error.to_string()))?;
        Self::try_from(raw)
    }
}

impl TryFrom<PackageManifestRaw> for PackageManifest {
    type Error = PackageError;

    fn try_from(raw: PackageManifestRaw) -> Result<Self, PackageError> {
        let version = Version::parse(&raw.version).map_err(|source| PackageError::InvalidVersion {
            version: raw.version.clone(),
            source,
        })?;
        let digest = ContentDigest::try_new(raw.digest)?;
        let identity = PackageIdentity {
            name: PackageName::try_from(raw.name)?,
            version,
            digest: digest.clone(),
        };
        let provenance = PackageProvenance::Uploaded { digest };

        let files = release_files(raw.files)?;
        let holds_library = files.iter().any(|file| file.path.as_str() == LIBRARY_FILE_NAME);
        let holds_workflows = files.iter().any(|file| file.path.as_str() == WORKFLOWS_FILE_NAME);
        match (holds_library, holds_workflows) {
            (true, true) => return Err(PackageError::LibraryAndWorkflow),
            (false, false) => return Err(PackageError::NothingDeclared),
            _ => {}
        }

        Ok(Self {
            identity,
            provenance,
            files,
        })
    }
}

/// The files a manifest lists, each a plain relative path with a SHA-256 hash, none twice and none `manifest.toml`.
fn release_files(raw: Vec<ReleaseFileRaw>) -> Result<Vec<ReleaseFile>, PackageError> {
    let mut seen = HashSet::new();
    let mut files = Vec::with_capacity(raw.len());
    for entry in raw {
        let path = WorkflowFilePath::try_from(entry.path.as_str())
            .map_err(|error| PackageError::InvalidFilePath(error.path))?;
        if path.as_str() == "manifest.toml" {
            return Err(PackageError::ManifestListedAsFile);
        }
        let hash = ContentHash::try_from(entry.hash).map_err(|_| PackageError::InvalidFileHash {
            path: path.as_str().to_owned(),
        })?;
        if !seen.insert(path.clone()) {
            return Err(PackageError::DuplicateFile(path.as_str().to_owned()));
        }
        files.push(ReleaseFile { path, hash });
    }
    Ok(files)
}

/// The `exports` of a manifest as written, with the modules the package declares.
pub(crate) struct RawExports<'a> {
    pub(crate) paths: Vec<String>,
    pub(crate) modules: &'a DeclaredModules,
}

impl TryFrom<RawExports<'_>> for DeclaredModules {
    type Error = PackageError;

    /// Each export a declared module, none twice, so a package offers only code it ships.
    fn try_from(raw: RawExports<'_>) -> Result<Self, PackageError> {
        let invalid = |path: String, reason: String| PackageError::InvalidExport { path, reason };
        let exports = Self::try_from(raw.paths).map_err(|error| invalid(error.path, error.reason))?;
        for export in exports.entries() {
            if !raw.modules.entries().contains(export) {
                return Err(invalid(
                    export.to_string(),
                    "is not in `modules`, so the package ships no such module".to_owned(),
                ));
            }
        }
        Ok(exports)
    }
}

impl TryFrom<DependencyRaw> for PackageDependency {
    type Error = PackageError;

    fn try_from(raw: DependencyRaw) -> Result<Self, PackageError> {
        let Some(stated) = raw.requirement else {
            return Err(PackageError::MissingLibraryRequirement { name: raw.name });
        };
        let requirement = VersionReq::parse(&stated).map_err(|source| PackageError::InvalidVersionRequirement {
            name: raw.name.clone(),
            requirement: stated,
            source,
        })?;
        Ok(Self {
            name: PackageName::try_from(raw.name)?,
            requirement,
        })
    }
}

pub(crate) fn parse_typed_fields(path: &str, raw_fields: Vec<TypedFieldRaw>) -> Result<Vec<TypedField>, PackageError> {
    raw_fields
        .into_iter()
        .map(|field| parse_typed_field(path, field))
        .collect()
}

/// Parses one named field (a workflow's own input/output, or one property
/// of an `object`-kinded field): `path` is the dotted context leading up
/// to it, used only to make a validation error locate the offending field
/// without needing the field's own name threaded back out of a nested
/// `Result`.
fn parse_typed_field(path: &str, raw: TypedFieldRaw) -> Result<TypedField, PackageError> {
    if raw.name.trim().is_empty() {
        return Err(PackageError::MissingFieldName { path: path.to_owned() });
    }
    let field_path = format!("{path}.{}", raw.name);
    let kind = parse_field_kind(&field_path, &raw.kind, raw.items, raw.properties)?;
    Ok(TypedField {
        name: raw.name,
        kind,
        required: raw.required,
    })
}

/// Parses one field's own `kind` (plus, when recursive, its `items` or
/// `properties`), reused both for a named [`TypedField`] and for an
/// array's own unnamed `items` descriptor.
fn parse_field_kind(
    path: &str,
    kind: &str,
    items: Option<Box<TypedFieldRaw>>,
    properties: Option<Vec<TypedFieldRaw>>,
) -> Result<FieldKind, PackageError> {
    if kind != "array" && items.is_some() {
        return Err(PackageError::UnexpectedNestedField {
            path: path.to_owned(),
            field: "items",
        });
    }
    if kind != "object" && properties.is_some() {
        return Err(PackageError::UnexpectedNestedField {
            path: path.to_owned(),
            field: "properties",
        });
    }
    match kind {
        "string" => Ok(FieldKind::String),
        "number" => Ok(FieldKind::Number),
        "bool" => Ok(FieldKind::Bool),
        "null" => Ok(FieldKind::Null),
        "date" => Ok(FieldKind::Date),
        "time" => Ok(FieldKind::Time),
        "datetime" => Ok(FieldKind::DateTime),
        "timestamp" => Ok(FieldKind::Timestamp),
        "vault" => Ok(FieldKind::Vault),
        "array" => {
            let items = items.ok_or_else(|| PackageError::MissingArrayItems { path: path.to_owned() })?;
            let item_path = format!("{path}[]");
            let item_kind = parse_field_kind(&item_path, &items.kind, items.items, items.properties)?;
            Ok(FieldKind::Array {
                items: Box::new(item_kind),
            })
        }
        "object" => {
            let properties = parse_typed_fields(path, properties.unwrap_or_default())?;
            Ok(FieldKind::Object { properties })
        }
        other => Err(PackageError::UnknownFieldKind {
            path: path.to_owned(),
            kind: other.to_owned(),
        }),
    }
}

/// `manifest.toml`'s schema. `deny_unknown_fields`: an unrecognised key is
/// a schema violation, not a silently ignored typo (`rust-quality.md`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageManifestRaw {
    name: String,
    version: String,
    digest: String,
    #[serde(default)]
    files: Vec<ReleaseFileRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseFileRaw {
    path: String,
    hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DependencyRaw {
    pub(crate) name: String,
    pub(crate) requirement: Option<String>,
}

/// A named field (a workflow's own `inputs`/`outputs` entry, or one entry
/// of an `object`-kinded field's own `properties`) and, when `kind` is
/// `"array"` or `"object"`, its own recursive shape. `name` defaults to
/// empty because the identical shape is reused for an array's own
/// `items` descriptor, which needs no name of its own; a genuinely named
/// field with an empty `name` is rejected in [`parse_typed_field`], not
/// here.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TypedFieldRaw {
    #[serde(default)]
    name: String,
    kind: String,
    #[serde(default)]
    items: Option<Box<TypedFieldRaw>>,
    #[serde(default)]
    properties: Option<Vec<TypedFieldRaw>>,
    #[serde(default = "field_is_required_by_default")]
    required: bool,
}

const fn field_is_required_by_default() -> bool {
    true
}

#[cfg(test)]
#[path = "package_test.rs"]
mod tests;

#[cfg(test)]
#[path = "package_shape_test.rs"]
mod shape_tests;

#[cfg(test)]
#[path = "package_vault_test.rs"]
mod vault_tests;

#[cfg(test)]
#[path = "package_exports_test.rs"]
mod exports_tests;

#[cfg(test)]
#[path = "package_release_files_test.rs"]
mod release_files_tests;
