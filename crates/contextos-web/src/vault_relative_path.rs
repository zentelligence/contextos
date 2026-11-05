//! A vault-relative path an operator types into a settings form: the shape check this crate applies before a
//! configured location is stored or used to build a tool call. It is not a security boundary of its own, since
//! `contextos-mcp` confines every path it receives to the vault root regardless, but it rejects the forms that would
//! otherwise reach the server as something other than the plain relative location the operator meant: an absolute
//! path, a Windows drive, separator, or alternate-data-stream form, a traversal, or an empty segment.

use std::fmt;

/// A `/`-separated path relative to the vault root, with no empty, `.`, or `..` segment, no backslash, and no colon.
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct VaultRelativePath(String);

/// Why a submitted string is not a [`VaultRelativePath`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VaultRelativePathError {
    #[error("must not be empty")]
    Empty,
    #[error("must be relative to the vault root, not start with `/`")]
    Absolute,
    #[error("must use `/` between folders, not `\\`")]
    Backslash,
    #[error("must not contain `:`")]
    Colon,
    #[error("must not contain an empty, `.`, or `..` folder name")]
    InvalidSegment,
}

impl VaultRelativePathError {
    /// A stable machine-readable code for this rejection.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "path/empty",
            Self::Absolute => "path/absolute",
            Self::Backslash => "path/backslash",
            Self::Colon => "path/colon",
            Self::InvalidSegment => "path/invalid-segment",
        }
    }
}

impl TryFrom<&str> for VaultRelativePath {
    type Error = VaultRelativePathError;

    /// Validates `raw` after trimming surrounding whitespace. A single trailing `/` is accepted and dropped, so a
    /// folder typed as `memory/decisions/` names the same location as `memory/decisions`.
    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(VaultRelativePathError::Empty);
        }
        if trimmed.starts_with('/') {
            return Err(VaultRelativePathError::Absolute);
        }
        if trimmed.contains('\\') {
            return Err(VaultRelativePathError::Backslash);
        }
        if trimmed.contains(':') {
            return Err(VaultRelativePathError::Colon);
        }
        let without_trailing = trimmed.strip_suffix('/').unwrap_or(trimmed);
        if without_trailing
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return Err(VaultRelativePathError::InvalidSegment);
        }
        Ok(Self(without_trailing.to_owned()))
    }
}

impl TryFrom<String> for VaultRelativePath {
    type Error = VaultRelativePathError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::try_from(raw.as_str())
    }
}

impl VaultRelativePath {
    /// A scaffold default compiled into this crate. Every caller passes a literal that already satisfies
    /// [`TryFrom`]'s rules, which `every_scaffold_default_satisfies_the_validation_rules` checks for each one, so this
    /// skips re-validating a value no operator supplied.
    #[must_use]
    pub(crate) fn scaffold(literal: &'static str) -> Self {
        debug_assert!(Self::try_from(literal).is_ok(), "invalid scaffold path {literal:?}");
        Self(literal.to_owned())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// This path joined beneath `self` with `/`, for a child location the caller has already validated as a
    /// relative path of its own.
    #[must_use]
    pub fn join(&self, child: &Self) -> Self {
        Self(format!("{}/{}", self.0, child.0))
    }

    /// The final segment: a file or folder's own name.
    #[must_use]
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// The containing folder, `None` for a path directly under the vault root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        self.0.rsplit_once('/').map(|(parent, _)| Self(parent.to_owned()))
    }

    /// This path relative to `ancestor`, `None` when `ancestor` is not a real ancestor folder of it (a shared string
    /// prefix such as `memory/dec` against `memory/decisions` is not one).
    #[must_use]
    pub fn strip_ancestor(&self, ancestor: &Self) -> Option<&str> {
        self.0
            .strip_prefix(ancestor.as_str())
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|rest| !rest.is_empty())
    }
}

impl fmt::Display for VaultRelativePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
#[path = "vault_relative_path_test.rs"]
mod tests;
