use contextos_obsidian::{FrontmatterDocument, FrontmatterError};

/// Filesystem kind represented by a managed index row.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum IndexEntryKind {
    Directory,
    File,
}

/// One currently discovered directory entry, its resolved display title, and
/// its derived summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexEntry {
    pub name: String,
    pub kind: IndexEntryKind,
    pub title: String,
    pub suggested_summary: String,
}

/// Inputs used to derive one entry's display title, independent of its summary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexTitleInput<'a> {
    pub filename: &'a str,
    pub source: Option<&'a str>,
}

/// One entry's resolved display title, for the managed Item column's wikilink text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexTitle(String);

/// Inputs used to derive a new managed-row summary, sourced only from an
/// entry's own frontmatter `description`, `summary`, or `excerpt` property.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexSummaryInput<'a> {
    pub source: Option<&'a str>,
}

/// One single-line summary suitable for a managed index row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexSummary(String);

impl<'a> From<&'a IndexTitle> for &'a str {
    fn from(value: &'a IndexTitle) -> Self {
        &value.0
    }
}

impl TryFrom<IndexTitleInput<'_>> for IndexTitle {
    type Error = FrontmatterError;

    fn try_from(value: IndexTitleInput<'_>) -> Result<Self, Self::Error> {
        if let Some(source) = value.source {
            let document = FrontmatterDocument::try_from(source)?;
            let title = document
                .frontmatter()
                .get("title")
                .and_then(|property| property.as_str())
                .map(str::trim)
                .filter(|title| !title.is_empty());
            if let Some(title) = title {
                return Ok(Self(title.to_owned()));
            }
        }
        let stem = value.filename.strip_suffix(".md").unwrap_or(value.filename);
        Ok(Self(directory_title(stem)))
    }
}

impl<'a> From<&'a IndexSummary> for &'a str {
    fn from(value: &'a IndexSummary) -> Self {
        &value.0
    }
}

impl TryFrom<IndexSummaryInput<'_>> for IndexSummary {
    type Error = FrontmatterError;

    fn try_from(value: IndexSummaryInput<'_>) -> Result<Self, Self::Error> {
        let Some(source) = value.source else {
            return Ok(Self(String::new()));
        };
        let document = FrontmatterDocument::try_from(source)?;
        for field in ["description", "summary", "excerpt"] {
            if let Some(text) = document.frontmatter().get(field).and_then(|property| property.as_str()) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Ok(Self(trimmed.to_owned()));
                }
            }
        }
        Ok(Self(String::new()))
    }
}

pub(crate) fn directory_title(value: &str) -> String {
    value
        .split(['-', '_', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut characters = word.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().chain(characters).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
