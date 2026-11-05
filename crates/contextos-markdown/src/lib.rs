//! Pure rendering of vault notes.
//!
//! A note arrives with every link already expanded by the server (`fs_attach_file` with `expand_links`), so this
//! crate never resolves a link: it reads each destination as a [`LinkTarget`] and asks a [`LinkRouter`] for the URL.
//! Nothing here performs I/O. A caller prepares a note, fetches what the preparation says it needs (embedded notes,
//! diagrams, base views), and renders it; the result is a pure function of the note, those resources, and the router.

#![forbid(unsafe_code)]

mod comments;
mod escape;
mod fence_attributes;
mod fence_blocks;
mod fence_vocabulary;
mod html_filter;
mod link_target;
mod metadata;
mod note;
mod note_sections;
mod prepare;
mod render;
mod router;
mod section;
mod slug;
mod templates;

pub use escape::{escape_attribute, escape_text};
pub use fence_attributes::{AttributeInput, AttributeValue, FenceAttributes};
pub use fence_vocabulary::{
    AttributeSpec, AttributeType, DEFAULT_FENCES_TOML, FenceRole, FenceSpec, FenceTone, FenceVocabulary,
    SUPPORTED_FENCE_SCHEMA, VocabularyError,
};
pub use link_target::{Fragment, LinkDestination, LinkTarget};
pub use metadata::{BlockId, BodyMetadata, BodyTag, LinkKind, LinkRecord, OutlineHeading};
pub use note::{Body, Frontmatter, Note, NoteError};
pub use note_sections::{NoteSections, SectionSpan};
pub use prepare::{BaseViewKey, EmbedInput, EmbedKey, Needs, PrepareInput, PreparedNote};
pub use render::{AMBIGUOUS_HINT, Diagram, DiagramProblem, RenderInput, RenderedNote, Resources, UNROUTED_HINT};
pub use router::{FileKind, LinkRouter};
