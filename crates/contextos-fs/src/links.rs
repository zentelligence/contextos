//! Recognising a link or reparse point, so a write never follows one.

use std::fs::Metadata;

/// Whether `metadata`, taken without following links, describes a symbolic link, a Windows junction, or any other
/// Windows reparse point (a cloud placeholder, say), none of which a host write may pass through.
#[must_use]
pub fn is_link_or_reparse_point(metadata: &Metadata) -> bool {
    EntryClass::from(metadata) != EntryClass::Plain
}

/// The Windows `FILE_ATTRIBUTE_REPARSE_POINT` bit.
pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// The two facts about a directory entry that decide how a link check treats it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntryFlags {
    /// Whether the entry is a symbolic link or a junction.
    pub is_link: bool,
    /// The Windows file attributes, or zero on a platform that has none.
    pub file_attributes: u32,
}

/// How a directory entry is treated by a link check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryClass {
    /// Neither a link nor a reparse point.
    Plain,
    /// A symbolic link or junction.
    Link,
    /// A reparse point that is not a link, such as a cloud placeholder.
    ReparsePoint,
}

impl From<EntryFlags> for EntryClass {
    fn from(value: EntryFlags) -> Self {
        if value.is_link {
            Self::Link
        } else if value.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            Self::ReparsePoint
        } else {
            Self::Plain
        }
    }
}

impl From<&Metadata> for EntryFlags {
    fn from(value: &Metadata) -> Self {
        // On Windows the standard library reports a junction as a symbolic link too, so a link is either of them and a
        // reparse point that is neither (a cloud placeholder, say) is told apart by its attributes.
        Self {
            is_link: value.file_type().is_symlink(),
            file_attributes: windows_attributes(value),
        }
    }
}

impl From<&Metadata> for EntryClass {
    fn from(value: &Metadata) -> Self {
        Self::from(EntryFlags::from(value))
    }
}

#[cfg(windows)]
fn windows_attributes(metadata: &Metadata) -> u32 {
    std::os::windows::fs::MetadataExt::file_attributes(metadata)
}

#[cfg(not(windows))]
fn windows_attributes(_metadata: &Metadata) -> u32 {
    0
}

#[cfg(test)]
#[path = "links_test.rs"]
mod tests;
