//! Why a package manifest, a dependency, or the resolution of one failed.

use semver::{Version, VersionReq};
use thiserror::Error;

use crate::library_resolution::Requirement;
use crate::package_name::PackageName;
use crate::script_modules::DeclaredModulesError;

fn requirements_phrase(requirements: &[Requirement]) -> String {
    let named: Vec<String> = requirements.iter().map(ToString::to_string).collect();
    named.join("; ")
}

fn versions_phrase(versions: &[Version]) -> String {
    let named: Vec<String> = versions.iter().map(ToString::to_string).collect();
    named.join(", ")
}

fn loop_phrase(path: &[PackageName]) -> String {
    let named: Vec<&str> = path.iter().map(PackageName::as_str).collect();
    named.join(" -> ")
}

/// One focused error enum for every package-manifest and
/// dependency-resolution failure this crate's own `package`/`dependency`
/// modules can produce (`rust-quality.md`).
#[derive(Debug, Error)]
pub enum PackageError {
    #[error("package manifest does not match the required schema: {0}")]
    Malformed(String),
    #[error("version {version:?} is not a valid semantic version: {source}")]
    InvalidVersion {
        version: String,
        #[source]
        source: semver::Error,
    },
    #[error("digest {0:?} is not a valid sha256 content digest")]
    MalformedDigest(String),
    #[error("field at {path:?} names an unrecognised type {kind:?}")]
    UnknownFieldKind { path: String, kind: String },
    #[error("field at {path:?} has no name")]
    MissingFieldName { path: String },
    #[error("field at {path:?} is kinded \"array\" but declares no items")]
    MissingArrayItems { path: String },
    #[error(
        "field at {path:?} declares {field:?}, which only applies to an \"array\" or \"object\" kind as appropriate"
    )]
    UnexpectedNestedField { path: String, field: &'static str },
    #[error("expected a {expected} value, found a {found} value")]
    TypeMismatch {
        expected: &'static str,
        found: &'static str,
    },
    // The offending value is held in the variant but never rendered: a supplied value may be sensitive.
    #[error("the value is not a valid {kind}: {message}")]
    InvalidTemporalValue {
        kind: &'static str,
        value: String,
        message: String,
    },
    #[error("object is missing its declared property {0:?}")]
    MissingProperty(String),
    #[error("input {path:?} is a vault input, which only a workflow from the host package store may declare")]
    VaultInputNotPermitted { path: String },
    #[error("{requested} is not installed")]
    ReleaseUnavailable { requested: String },
    #[error("input argument at position {position} has no `=`; write it as name=value")]
    MalformedInputArgument { position: usize },
    #[error("the value of input {name:?} is not valid JSON: {message}")]
    UnreadableInputValue { name: String, message: String },
    #[error("input {0:?} is declared more than once")]
    DuplicateInput(String),
    #[error("output {0:?} is declared more than once")]
    DuplicateOutput(String),
    #[error("output {path:?} is a vault output, which only a workflow from the host package store may declare")]
    VaultOutputNotPermitted { path: String },
    #[error("input {0:?} is not declared by this workflow")]
    UndeclaredInput(String),
    #[error("input {0:?} is required and was not supplied")]
    MissingInput(String),
    // As above, the value is held and never rendered.
    #[error("the value is not a vault name: a vault is named, never given as a path")]
    InvalidVaultName { value: String },
    #[error("{reason}")]
    InvalidWorkflowFile { reason: String },
    #[error("the call rules contradict each other: {reason}")]
    CallRuleContradiction { reason: String },
    #[error("dependency requirement {requirement:?} for package {name:?} is not a valid version requirement: {source}")]
    InvalidVersionRequirement {
        name: String,
        requirement: String,
        #[source]
        source: semver::Error,
    },
    #[error(
        "path {0:?} must be a plain relative path within the package: `/`-separated, with no empty, `.`, or `..` segment, and no backslash, colon, or NUL"
    )]
    InvalidTemplatePath(String),
    #[error(
        "files entry {0:?} must be a plain relative path within the package: `/`-separated, with no empty, `.`, or `..` segment, and no backslash, colon, or NUL"
    )]
    InvalidFilePath(String),
    #[error("files entry {path:?} has a hash that is not 64 hexadecimal characters")]
    InvalidFileHash { path: String },
    #[error("files lists {0:?} more than once")]
    DuplicateFile(String),
    #[error("files lists manifest.toml, which describes the release and is never one of its files")]
    ManifestListedAsFile,
    #[error("{0}")]
    InvalidModule(DeclaredModulesError),
    #[error("exports entry {path:?} {reason}")]
    InvalidExport { path: String, reason: String },
    #[error(
        "package lists neither a `workflows.toml` nor a `library.toml` at its top level: a package lists exactly one of them, which says what kind of package it is"
    )]
    NothingDeclared,
    #[error(
        "package lists both a `workflows.toml` and a `library.toml`: a package lists exactly one of them, which says what kind of package it is"
    )]
    LibraryAndWorkflow,
    #[error("the release's {file} is absent or is not UTF-8 text")]
    PackageFileUnreadable { file: &'static str },
    #[error("workflows.toml is not valid: {reason}")]
    InvalidWorkflowsFile { reason: String },
    #[error("`libraries` names {0:?} twice")]
    DuplicateLibrary(String),
    #[error("library \"{name}\" is not installed, and {}", requirements_phrase(.requirements))]
    LibraryNotInstalled {
        name: PackageName,
        requirements: Vec<Requirement>,
    },
    #[error(
        "no installed release of library \"{name}\" satisfies every requirement on it ({}); installed: {}",
        requirements_phrase(.requirements),
        versions_phrase(.installed)
    )]
    NoReleaseSatisfies {
        name: PackageName,
        requirements: Vec<Requirement>,
        installed: Vec<Version>,
    },
    #[error(
        "library \"{name}\" was already chosen at {chosen}, which a later requirement excludes ({})",
        requirements_phrase(.requirements)
    )]
    RequirementExcludesChosen {
        name: PackageName,
        chosen: Version,
        requirements: Vec<Requirement>,
    },
    #[error("packages depend on each other in a loop: {}", loop_phrase(.path))]
    DependencyCycle { path: Vec<PackageName> },
    #[error("the job reaches more than {limit} library packages")]
    TooManyDependencies { limit: usize },
    #[error("library \"{name}\" is more than {limit} levels of dependencies deep")]
    DependencyTooDeep { name: PackageName, limit: usize },
    #[error("module {path:?} would take the job's code past its limit of {limit}")]
    SnapshotOverLimit { path: String, limit: &'static str },
    #[error("installed packages could not be read for {name:?}: {reason}")]
    CatalogueUnreadable { name: String, reason: String },
    #[error(
        "installed release {name} {version} has a manifest that does not parse or does not match its schema: {reason}"
    )]
    InstalledManifestInvalid {
        name: PackageName,
        version: Version,
        reason: String,
    },
    #[error(
        "`import \"{package}:...\"` names the importing package itself: a package imports its own modules by their path alone"
    )]
    ImportOfOwnPackage { package: PackageName },
    #[error(
        "`import \"{package}:...\"` names a package that is not one of the importing package's own `libraries`: only a direct library's exports can be imported"
    )]
    ImportNotDependency { package: PackageName },
    #[error("package {package:?} does not export module {module:?}: only the modules in its `exports` can be imported")]
    ImportNotExported { package: PackageName, module: String },
    #[error("package name {name:?} is not valid: {reason}")]
    InvalidPackageName { name: String, reason: &'static str },
    #[error("library {name:?} states no requirement: say which versions it may be, such as requirement = \"^1.2\"")]
    MissingLibraryRequirement { name: String },
    #[error("workflow name {name:?} is not valid: {reason}")]
    InvalidWorkflowName { name: String, reason: &'static str },
    #[error("library.toml is not valid: {reason}")]
    InvalidLibraryManifest { reason: String },
    #[error("library name {name:?} is not valid: {reason}")]
    InvalidLibraryName { name: String, reason: &'static str },
    #[error("dependency on package {0:?} could not be resolved: no installed version was supplied")]
    UnresolvedDependency(String),
    #[error("installed version {installed} of package {name:?} does not satisfy the required {requirement}")]
    IncompatibleVersion {
        name: String,
        installed: Version,
        requirement: VersionReq,
    },
    #[error("duplicate package identity: {name:?} version {version} is registered more than once")]
    DuplicateIdentity { name: String, version: Version },
}
