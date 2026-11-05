//! The `base` property of a record note: a wikilink to the `.base` file that governs the note, written when the
//! note is created through a base and read back as the vault path of that base.

use crate::frontmatter_links::{LinkError, LinkTarget, TARGET_FORBIDDEN};

/// The frontmatter key that names a record note's governing base.
pub const BASE_PROPERTY: &str = "base";

/// Whether `path` names a `.base` file: a non-empty stem before the extension, in a folder or not.
fn is_base_file(path: &str) -> bool {
    path.strip_suffix(".base")
        .is_some_and(|stem| !stem.is_empty() && !stem.ends_with('/'))
}

fn unlinkable(text: &str) -> LinkError {
    LinkError::UnlinkableText { text: text.to_owned() }
}

/// The value stored in the `base` property: `[[memory/tasks.base]]`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaseLink(String);

/// The base to link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BaseLinkInput<'a> {
    /// The base's vault-relative path, including the `.base` extension.
    pub path: &'a str,
}

impl TryFrom<BaseLinkInput<'_>> for BaseLink {
    type Error = LinkError;

    fn try_from(value: BaseLinkInput<'_>) -> Result<Self, Self::Error> {
        if !is_base_file(value.path) || value.path.contains(TARGET_FORBIDDEN) {
            return Err(unlinkable(value.path));
        }
        Ok(Self(format!("[[{}]]", value.path)))
    }
}

impl AsRef<str> for BaseLink {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// The vault path of the base a stored `base` value links to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaseLinkTarget(String);

impl BaseLinkTarget {
    /// Returns the base's vault-relative path, including the `.base` extension.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for BaseLinkTarget {
    type Error = LinkError;

    /// Reads `stored`, which must be exactly one plain wikilink to a `.base` file (a display text or heading is
    /// ignored, and a leading `/` marks the vault root).
    fn try_from(stored: &str) -> Result<Self, Self::Error> {
        let target = LinkTarget::try_from(stored)?;
        let path = target.as_str().trim_start_matches('/');
        if is_base_file(path) {
            Ok(Self(path.to_owned()))
        } else {
            Err(unlinkable(stored))
        }
    }
}

#[cfg(test)]
#[path = "base_link_test.rs"]
mod tests;
