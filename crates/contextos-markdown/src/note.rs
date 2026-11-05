//! A note split into its frontmatter and its body.

use contextos_obsidian::{FrontmatterDocument, FrontmatterError};
use serde_json::{Map, Value};

/// A note's properties: the ordered YAML frontmatter, parsed strictly.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frontmatter(Map<String, Value>);

impl Frontmatter {
    /// The properties in the order they are written.
    #[must_use]
    pub const fn properties(&self) -> &Map<String, Value> {
        &self.0
    }
}

/// A note's text after its frontmatter.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Body {
    text: String,
    first_line: usize,
}

impl Body {
    /// The body text, exactly as written.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The line of the note, counting from 1, on which the body starts.
    #[must_use]
    pub const fn first_line(&self) -> usize {
        self.first_line
    }
}

/// Text that is a body and nothing else, such as a section of a note: never read for frontmatter, so a leading `---`
/// is a rule.
impl From<&str> for Body {
    fn from(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            first_line: 1,
        }
    }
}

/// A note.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Note {
    frontmatter: Frontmatter,
    body: Body,
}

impl Note {
    #[must_use]
    pub const fn frontmatter(&self) -> &Frontmatter {
        &self.frontmatter
    }

    #[must_use]
    pub const fn body(&self) -> &Body {
        &self.body
    }
}

/// A note could not be split into frontmatter and body.
#[derive(Debug, thiserror::Error)]
pub enum NoteError {
    #[error(transparent)]
    Frontmatter(#[from] FrontmatterError),
}

impl TryFrom<&str> for Note {
    type Error = NoteError;

    fn try_from(source: &str) -> Result<Self, Self::Error> {
        let document = FrontmatterDocument::try_from(source)?;
        Ok(Self {
            frontmatter: Frontmatter(document.frontmatter().clone()),
            body: Body {
                text: document.body().to_owned(),
                first_line: document.body_start_line(),
            },
        })
    }
}

#[cfg(test)]
#[path = "note_test.rs"]
mod tests;
