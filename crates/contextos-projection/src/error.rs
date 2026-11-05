use std::io;

use contextos_fs::FsError;
use contextos_obsidian::FrontmatterError;
use thiserror::Error;

/// Why a name or relative path was rejected. Never carries the rejected text.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum NameRejection {
    /// The name or path is empty.
    #[error("it is empty")]
    Empty,
    /// The name or a path component is longer than the limit.
    #[error("it is longer than {maximum} bytes")]
    TooLong {
        /// The limit in bytes.
        maximum: usize,
    },
    /// A slug contains a character outside lowercase letters, digits, and hyphens.
    #[error("it contains a character outside lowercase letters, digits, and hyphens")]
    InvalidCharacter,
    /// The value is not one of the documented choices.
    #[error("it is not one of the documented values")]
    Unrecognised,
    /// A slug has a leading, trailing, or doubled hyphen.
    #[error("hyphens may only separate lowercase letters and digits")]
    MisplacedHyphen,
    /// A path component is empty (a leading, trailing, or doubled separator).
    #[error("it has an empty path component")]
    EmptyComponent,
    /// A path component is `.` or `..`.
    #[error("it has a `.` or `..` path component")]
    DotComponent,
    /// A path starts with a separator.
    #[error("it is absolute")]
    Absolute,
    /// A path contains a backslash.
    #[error("it contains a backslash")]
    Backslash,
    /// A path contains a colon, which Windows reads as a drive or an alternate data stream.
    #[error("it contains a colon")]
    Colon,
    /// A path contains a NUL or other control character.
    #[error("it contains a control character")]
    ControlCharacter,
    /// A path component is a Windows reserved device name.
    #[error("it has a component that is a Windows reserved device name")]
    ReservedDeviceName,
    /// A path component ends in a dot or a space, which Windows silently rewrites.
    #[error("it has a component ending in a dot or a space")]
    TrailingDotOrSpace,
}

/// Why a target root was rejected.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TargetRootRejection {
    /// The configured root is not an absolute path.
    #[error("it is not absolute")]
    NotAbsolute,
    /// The configured root does not resolve to a folder.
    #[error("it is not a folder")]
    NotFolder,
    /// The configured path names no file (it ends at a root or a `..`).
    #[error("it names no file")]
    NoFileName,
    /// The configured path names something that is not a file.
    #[error("it is not a file")]
    NotFile,
    /// The configured path no longer resolves to where it did when the server started.
    #[error("it no longer resolves to where it did when the server started")]
    Relinked,
}

/// Which limit on a projected entry in a target was exceeded.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TargetLimit {
    /// The entry holds more files than the limit.
    #[error("it holds more files than the limit")]
    Files,
    /// The entry nests folders deeper than the limit.
    #[error("it nests folders deeper than the limit")]
    Depth,
    /// The entry is larger than the limit.
    #[error("it is larger than the limit")]
    Bytes,
}

/// What is wrong with the markers of an orientation block in a host's instructions file.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum BlockRejection {
    /// The file holds more than one begin marker line.
    #[error("it holds more than one begin marker")]
    DuplicateBegin,
    /// The file holds more than one end marker line.
    #[error("it holds more than one end marker")]
    DuplicateEnd,
    /// An end marker line has no begin marker line before it.
    #[error("an end marker has no begin marker before it")]
    EndWithoutBegin,
    /// A begin marker line has no end marker line after it.
    #[error("a begin marker has no end marker after it")]
    BeginWithoutEnd,
}

/// Why an ownership manifest was rejected. Never carries manifest content.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ManifestRejection {
    /// The file is not valid JSON of the manifest shape (an unknown, missing, or mistyped field).
    #[error("it is not a valid manifest document (line {line}, column {column})")]
    InvalidDocument {
        /// One-based line of the first problem.
        line: usize,
        /// One-based column of the first problem.
        column: usize,
    },
    /// The manifest declares a version this build does not read.
    #[error("it declares version {found}, but only version {supported} is supported")]
    UnsupportedVersion {
        /// The declared version.
        found: u32,
        /// The version this build reads.
        supported: u32,
    },
    /// The manifest records a different target root than the one it is read for.
    #[error("it records a different target root")]
    TargetMismatch,
    /// An entry key is not a valid slug.
    #[error("an entry is keyed by an invalid slug")]
    InvalidSlug,
    /// An entry field that must not be empty is empty.
    #[error("an entry has an empty {field}")]
    EmptyField {
        /// The field name.
        field: &'static str,
    },
    /// An entry hash is not a SHA-256 hexadecimal digest.
    #[error("an entry has an invalid {field}")]
    InvalidHash {
        /// The field name.
        field: &'static str,
    },
    /// An entry configuration identifier is invalid.
    #[error("an entry has an invalid configuration identifier")]
    InvalidConfiguration,
}

/// Why a definition field was rejected. Never carries the rejected text.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DefinitionRejection {
    /// The definition is not valid UTF-8.
    #[error("it is not valid UTF-8")]
    NotUtf8,
    /// The YAML frontmatter is unclosed or is not valid YAML of a mapping.
    #[error("it is not valid YAML frontmatter")]
    Malformed,
    /// A required field is absent.
    #[error("it is missing")]
    Missing,
    /// A required field is empty or only whitespace.
    #[error("it is empty")]
    Empty,
    /// A field holds something other than text.
    #[error("it is not text")]
    WrongType,
    /// A field holds text outside its documented values.
    #[error("it holds an unrecognised value")]
    Unrecognised,
    /// The text holds a line that marks the start or end of the orientation block.
    #[error("it holds a line reserved to mark the orientation block")]
    HoldsMarker,
    /// The field names a host's own setting, which a definition must not carry.
    #[error("it is reserved to the hosts and is not read from a definition")]
    Reserved,
}

/// Every failure of the projection substrate, each with a stable machine-readable code.
///
/// Messages describe what was rejected and why, never file content.
#[derive(Debug, Error)]
pub enum ProjectionError {
    /// A slug failed validation.
    #[error("invalid slug: {reason}")]
    InvalidSlug {
        /// The rule it broke.
        reason: NameRejection,
    },
    /// A skill file path failed validation.
    #[error("invalid skill file path: {reason}")]
    InvalidSkillPath {
        /// The rule it broke.
        reason: NameRejection,
    },
    /// A file extension failed validation.
    #[error("invalid file extension: {reason}")]
    InvalidExtension {
        /// The rule it broke.
        reason: NameRejection,
    },
    /// A configuration identifier failed validation.
    #[error("invalid configuration identifier: {reason}")]
    InvalidConfigurationId {
        /// The rule it broke.
        reason: NameRejection,
    },
    /// A skill folder's file set is unusable.
    #[error("invalid skill folder: {reason}")]
    InvalidSkillFolder {
        /// What is wrong with the file set.
        reason: &'static str,
    },
    /// An agent or skill definition failed the projection contract.
    #[error("invalid definition: field `{field}`: {reason}")]
    InvalidDefinition {
        /// The frontmatter field, `frontmatter`, `body`, `definition`, or `SKILL.md` that failed.
        field: &'static str,
        /// The rule it broke.
        reason: DefinitionRejection,
    },
    /// A host setting (a model name, an MCP server name, or a tool name) failed validation.
    #[error("invalid host setting `{field}`: {reason}")]
    InvalidHostSetting {
        /// The setting that failed.
        field: &'static str,
        /// The rule it broke.
        reason: NameRejection,
    },
    /// An agent file's frontmatter could not be serialised as YAML.
    #[error("agent file frontmatter could not be serialised")]
    RenderYaml {
        /// The underlying cause.
        #[source]
        source: FrontmatterError,
    },
    /// An agent file could not be serialised as TOML.
    #[error("agent file could not be serialised")]
    RenderToml {
        /// The underlying cause.
        #[source]
        source: toml::ser::Error,
    },
    /// A host's instructions file holds markers that do not form exactly one block, so it was left unchanged.
    #[error("the orientation block markers are malformed: {reason}")]
    MalformedBlock {
        /// What is wrong with the markers.
        reason: BlockRejection,
    },
    /// A target root or manifest location was rejected.
    #[error("target root rejected: {reason}")]
    TargetRootRejected {
        /// The rule it broke.
        reason: TargetRootRejection,
    },
    /// A target root could not be resolved.
    #[error("target root could not be resolved")]
    TargetRootUnresolvable {
        /// The underlying cause.
        #[source]
        source: io::Error,
    },
    /// An existing component below the target root is a link, so the path could leave the root.
    #[error("a link exists below the target root at {entry}")]
    LinkBelowRoot {
        /// The entry, relative to the target root.
        entry: String,
    },
    /// An existing entry below the target root is a reparse point that is not a link, such as a cloud placeholder.
    #[error("a reparse point exists below the target root at {entry}")]
    ReparsePoint {
        /// The entry, relative to the target root.
        entry: String,
    },
    /// An existing entry is not a regular file or folder.
    #[error("an unsupported kind of entry exists at {entry}")]
    UnsupportedEntry {
        /// The entry, relative to the target root.
        entry: String,
    },
    /// A projected entry could not be read.
    #[error("could not read {entry}")]
    Read {
        /// The entry, relative to the target root.
        entry: String,
        /// The underlying cause.
        #[source]
        source: io::Error,
    },
    /// An existing entry in a target is beyond the file-count, depth, or byte limit, so it was not read.
    #[error("the entry at {entry} is beyond a limit: {limit}")]
    TargetEntryOverLimit {
        /// The entry, relative to the target root.
        entry: String,
        /// The limit it exceeds.
        limit: TargetLimit,
    },
    /// The shared atomic writer failed.
    #[error("{0}")]
    Write(#[from] FsError),
    /// A definition source could not be read.
    #[error("could not read {entry} in vault {vault}")]
    SourceRead {
        /// The vault's configured name.
        vault: String,
        /// The entry, relative to the vault.
        entry: String,
        /// The underlying cause.
        #[source]
        source: Box<FsError>,
    },
    /// A definition source is, or sits below, a link, so reading it could leave the vault.
    #[error("a link exists at {entry} in vault {vault}")]
    SourceLink {
        /// The vault's configured name.
        vault: String,
        /// The entry, relative to the vault.
        entry: String,
    },
    /// The ownership manifest is malformed and was not repaired.
    #[error("ownership manifest is malformed: {reason}")]
    ManifestMalformed {
        /// What is wrong.
        reason: ManifestRejection,
    },
    /// The ownership manifest could not be read.
    #[error("ownership manifest could not be read")]
    ManifestUnreadable {
        /// The underlying cause.
        #[source]
        source: io::Error,
    },
    /// The ownership manifest could not be serialised.
    #[error("ownership manifest could not be serialised")]
    ManifestSerialise {
        /// The underlying cause.
        #[source]
        source: serde_json::Error,
    },
}

impl ProjectionError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidSlug { .. } => "projection/invalid-slug",
            Self::InvalidSkillPath { .. }
            | Self::SourceLink { .. }
            | Self::TargetRootRejected { .. }
            | Self::LinkBelowRoot { .. }
            | Self::UnsupportedEntry { .. }
            | Self::TargetEntryOverLimit { .. } => "projection/path-rejected",
            Self::ReparsePoint { .. } => "projection/reparse-point",
            Self::InvalidSkillFolder { .. } | Self::InvalidDefinition { .. } => "projection/invalid-definition",
            Self::InvalidExtension { .. } | Self::InvalidConfigurationId { .. } | Self::InvalidHostSetting { .. } => {
                "projection/invalid-configuration"
            }
            Self::TargetRootUnresolvable { .. }
            | Self::Read { .. }
            | Self::SourceRead { .. }
            | Self::ManifestUnreadable { .. } => "projection/read-failed",
            Self::Write(_) | Self::ManifestSerialise { .. } | Self::RenderYaml { .. } | Self::RenderToml { .. } => {
                "projection/write-failed"
            }
            Self::ManifestMalformed { .. } => "projection/manifest-malformed",
            Self::MalformedBlock { .. } => "projection/malformed-block",
        }
    }

    /// Actionable remediation suitable for an operator.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::InvalidSlug { .. } => {
                "Rename the definition to lowercase letters and digits separated by single hyphens, at most 64 bytes."
            }
            Self::InvalidSkillPath { .. } | Self::InvalidSkillFolder { .. } => {
                "Rename the skill file to a relative path of plain components, without dot segments, backslashes, \
                 colons, control characters, Windows reserved names, or trailing dots or spaces."
            }
            Self::InvalidExtension { .. } | Self::InvalidConfigurationId { .. } | Self::InvalidHostSetting { .. } => {
                "Correct the projection configuration and restart the server."
            }
            Self::InvalidDefinition {
                reason: DefinitionRejection::HoldsMarker,
                ..
            } => {
                "Remove the lines that read as orientation block markers from registry/instructions/global.md; they \
                 are reserved to ContextOS."
            }
            Self::InvalidDefinition { .. } => {
                "Correct the definition: its frontmatter needs a non-empty `description`, a `tier` of fast, balanced, \
                 or deep, and optionally an `access` of all or read-only (a skill needs `name` and `description`), \
                 it must not name a host model or tool list, and it needs a non-empty body."
            }
            Self::TargetRootRejected {
                reason: TargetRootRejection::Relinked,
            } => {
                "A projection target was re-linked while the server was running, so nothing was written through it. \
                 Restore the link to what it was, or restart the server to adopt the new location."
            }
            Self::TargetRootRejected { .. } | Self::TargetRootUnresolvable { .. } => {
                "Configure an absolute path to an existing folder for the projection target."
            }
            Self::ReparsePoint { .. } => {
                "The entry is a reparse point that is not a link, such as a cloud placeholder (OneDrive files on \
                 demand, for example). Make it available offline, or move it out of the projection target; the \
                 server never follows reparse points below a target root."
            }
            Self::LinkBelowRoot { .. } | Self::UnsupportedEntry { .. } => {
                "Remove the link or unusual entry from the projection target; the server never follows links below a \
                 target root."
            }
            Self::TargetEntryOverLimit { .. } => {
                "Remove or shrink the oversized entry in the projection target; the server does not read entries \
                 beyond the same limits definitions have."
            }
            Self::SourceLink { .. } => {
                "Replace the link in the vault's registry with the real file or folder; definitions are never read \
                 through links."
            }
            Self::Read { .. } | Self::SourceRead { .. } | Self::ManifestUnreadable { .. } => {
                "Check that the file is readable and retry the operation."
            }
            Self::Write(_) | Self::ManifestSerialise { .. } | Self::RenderYaml { .. } | Self::RenderToml { .. } => {
                "Check permissions and available storage for the projection target, then retry the operation."
            }
            Self::MalformedBlock { .. } => {
                "Edit the host's global instructions file so it holds at most one begin marker line followed by one \
                 end marker line, or remove both markers; the file is never repaired automatically."
            }
            Self::ManifestMalformed { .. } => {
                "Inspect the ownership manifest in the state folder; it is never repaired automatically. Remove it \
                 to start afresh: entries in the target that match what would be projected are adopted again on the \
                 next run, and any that differ are left untouched."
            }
        }
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
