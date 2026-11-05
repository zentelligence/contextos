//! The base a note declares, and the folder that base keeps its records in: what settles a link name that several
//! files answer to. Link expansion and the link graph read both here, so they settle the same name the same way.

use std::path::Path;

use crate::{BASE_PROPERTY, BaseDocument, FrontmatterDocument, LinkTarget, RecordsFolder, records_folder};

/// The base a note names in its `base` property, as the link target it was written with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclaredBase(String);

/// The note declares no base, or one that is not a link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NoDeclaredBase;

impl TryFrom<&str> for DeclaredBase {
    type Error = NoDeclaredBase;

    /// Reads the `base` property of the note `source`: a wikilink to the base file.
    fn try_from(source: &str) -> Result<Self, Self::Error> {
        let document = FrontmatterDocument::try_from(source).map_err(|_| NoDeclaredBase)?;
        let stored = document
            .frontmatter()
            .get(BASE_PROPERTY)
            .and_then(serde_json::Value::as_str)
            .ok_or(NoDeclaredBase)?;
        let target = LinkTarget::try_from(stored).map_err(|_| NoDeclaredBase)?;
        Ok(Self(target.as_str().to_owned()))
    }
}

impl DeclaredBase {
    /// The link target the note wrote, to be resolved like any other link.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.0
    }

    /// Whether the file a declared base resolved to, `path`, is a base file: a link in the `base` property to anything
    /// else declares nothing.
    #[must_use]
    pub fn is_base_file(path: &str) -> bool {
        Path::new(path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("base"))
    }
}

/// The folder a base keeps its records in, owned so it can be held between questions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaseRecords {
    /// Vault-relative, with no leading or trailing `/`.
    pub path: String,
    /// Whether records may sit in folders beneath it.
    pub includes_subfolders: bool,
}

/// The base file confines its records to no single folder, or cannot be read as a base.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NoRecordsFolder;

impl TryFrom<&str> for BaseRecords {
    type Error = NoRecordsFolder;

    /// Reads the folder the base file `text` confines its records to.
    fn try_from(text: &str) -> Result<Self, Self::Error> {
        let document = BaseDocument::try_from(text).map_err(|_| NoRecordsFolder)?;
        let folder = records_folder(document.definition().get("filters")).ok_or(NoRecordsFolder)?;
        Ok(Self {
            path: folder.path.to_owned(),
            includes_subfolders: folder.includes_subfolders,
        })
    }
}

impl BaseRecords {
    /// The folder as link resolution takes it.
    #[must_use]
    pub fn folder(&self) -> RecordsFolder<'_> {
        RecordsFolder {
            path: &self.path,
            includes_subfolders: self.includes_subfolders,
        }
    }
}

#[cfg(test)]
#[path = "declared_base_test.rs"]
mod tests;
