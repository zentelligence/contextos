//! What a note's body holds besides its text, derived as it is rendered and never stored.

use crate::LinkTarget;

/// A heading in the note's outline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlineHeading {
    /// 1 to 6.
    pub level: u8,
    /// The heading's text, without markup.
    pub text: String,
    /// The element id the rendered heading carries.
    pub anchor: String,
    /// The line of the note it is on, counting from 1.
    pub line: usize,
}

/// A `#tag` written in the body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BodyTag {
    /// The tag without its `#`.
    pub name: String,
    pub line: usize,
}

/// A `^block-id` written in the body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockId {
    /// The id without its `^`.
    pub id: String,
    pub line: usize,
}

/// How a link was written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkKind {
    /// `[[target]]`
    Wikilink,
    /// `![[target]]`
    Embed,
    /// `[text](target)` or a reference link.
    Link,
    /// `![alt](target)`
    Image,
}

/// A link or embed in the body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkRecord {
    pub kind: LinkKind,
    pub target: LinkTarget,
    pub line: usize,
}

/// Everything in the body besides its text.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BodyMetadata {
    pub outline: Vec<OutlineHeading>,
    pub tags: Vec<BodyTag>,
    pub block_ids: Vec<BlockId>,
    pub links: Vec<LinkRecord>,
}
