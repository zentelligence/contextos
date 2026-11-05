//! Link expansion for `fs_attach_file`'s `expand_links` mode: the MCP-side answers to the two questions the pure
//! expansion in `contextos-obsidian` asks of a vault. Whether a candidate path is a file is one existence check on the
//! filesystem. Which files carry a name is the vault's file name index's answer, read as it stands and rebuilt once,
//! only when a name is not found. The vault is not walked for a link the note's own folder tree can answer.

use std::cell::Cell;
use std::path::Path;

use contextos_core::{ContentHash, ReadsVault, VaultPath, VaultPathInput, VaultSet};
use contextos_fs::{Attachment, FileNameIndex, Filesystem, FsError, MAX_ATTACHMENT_BYTES};
use contextos_obsidian::{
    BaseRecords, CanvasExpansionInput, DeclaredBase, ExpandedCanvas, ExpandedMarkdown, LinkExpansionInput,
    LinkResolution, LinkTargets, resolve_link,
};
use sha2::{Digest, Sha256};

use crate::tool_error::ToolError;

/// The most distinct link targets one expansion resolves. Each costs a few existence checks and an index lookup, and a
/// name found nowhere costs one rebuild of the index, paid at most once per expansion. Beyond the limit the call fails
/// with a stable code and returns nothing, since a note only partly expanded would break the guarantee that every link
/// is.
const MAX_DISTINCT_LINK_TARGETS: usize = 10_000;

/// What `expand_links` can expand: a note's links, or a canvas's text node links and file nodes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Expandable {
    Note,
    Canvas,
}

impl TryFrom<&VaultPath> for Expandable {
    type Error = ToolError;

    /// Reads the kind from the extension, before anything is read, and refuses any other file rather than ignoring
    /// the flag.
    fn try_from(path: &VaultPath) -> Result<Self, Self::Error> {
        let file: &Path = path.into();
        let extension = file.extension().and_then(|extension| extension.to_str());
        match extension {
            Some(extension) if extension.eq_ignore_ascii_case("md") => Ok(Self::Note),
            Some(extension) if extension.eq_ignore_ascii_case("canvas") => Ok(Self::Canvas),
            _ => Err(ToolError::ExpandLinksNotMarkdown {
                path: path.portable_relative(),
                accepted: "Markdown notes (.md) and canvases (.canvas)",
            }),
        }
    }
}

/// Rejects a path that is not a Markdown note (`.md`), before anything is read: frontmatter belongs to notes only.
pub(crate) fn require_markdown_note(path: &VaultPath) -> Result<(), ToolError> {
    match Expandable::try_from(path) {
        Ok(Expandable::Note) => Ok(()),
        Ok(Expandable::Canvas) | Err(_) => Err(ToolError::ExpandLinksNotMarkdown {
            path: path.portable_relative(),
            accepted: "Markdown notes (.md)",
        }),
    }
}

/// Answers a link's questions for one vault.
pub(crate) struct VaultLinkTargets<'a> {
    filesystem: &'a Filesystem,
    roots: &'a VaultSet,
    vault: &'a str,
    names: &'a FileNameIndex,
    rescanned: Cell<bool>,
}

impl<'a> VaultLinkTargets<'a> {
    /// Answers for the vault named `vault` from `names`, an index walked just now, so a name it does not know is
    /// not looked for again with a second walk.
    pub(crate) const fn over_fresh_index(
        filesystem: &'a Filesystem,
        roots: &'a VaultSet,
        vault: &'a str,
        names: &'a FileNameIndex,
    ) -> Self {
        Self {
            filesystem,
            roots,
            vault,
            names,
            rescanned: Cell::new(true),
        }
    }
}

impl<'a> VaultLinkTargets<'a> {
    /// Answers for the vault named `vault` from `names`, a shared index that may be older than the vault, so a name it
    /// does not know is looked for again after one more walk.
    pub(crate) const fn over_index(
        filesystem: &'a Filesystem,
        roots: &'a VaultSet,
        vault: &'a str,
        names: &'a FileNameIndex,
    ) -> Self {
        Self {
            filesystem,
            roots,
            vault,
            names,
            rescanned: Cell::new(false),
        }
    }
}

impl LinkTargets for VaultLinkTargets<'_> {
    type Error = ToolError;

    fn is_file(&self, path: &str) -> Result<bool, Self::Error> {
        // A candidate the path rules reject (traversal, a `:`, a symlink out of the vault) is not a file in the vault.
        let Ok(candidate) = VaultPath::try_from(VaultPathInput {
            roots: self.roots,
            raw: &format!("{}://{path}", self.vault),
        }) else {
            return Ok(false);
        };
        Ok(self.filesystem.is_file(&candidate)?)
    }

    fn files_ending_with(&self, suffix: &str) -> Result<Vec<String>, Self::Error> {
        let found = self.existing_files(suffix)?;
        if !found.is_empty() || self.rescanned.replace(true) {
            return Ok(found);
        }
        // A name the index does not know may be a file created since it was built: rebuild it once, then ask again.
        self.names.rescan()?;
        self.existing_files(suffix)
    }
}

impl VaultLinkTargets<'_> {
    /// The indexed files ending with `suffix` that are still files. An entry for a file deleted or moved since the
    /// index was built is dropped, so a link is never rooted at a path that is not there.
    fn existing_files(&self, suffix: &str) -> Result<Vec<String>, ToolError> {
        let mut existing = Vec::new();
        for path in self.names.files_ending_with(suffix)? {
            if self.is_file(&path)? {
                existing.push(path);
            }
        }
        Ok(existing)
    }
}

/// One note's text (the whole note, or only part of it such as its frontmatter block) to expand, and the vault it
/// is expanded against.
pub(crate) struct NoteExpansion<'a> {
    pub(crate) source: &'a str,
    /// The note the text belongs to: links resolve from its folder.
    pub(crate) note: &'a VaultPath,
    pub(crate) filesystem: &'a Filesystem,
    pub(crate) roots: &'a VaultSet,
    /// The vault's file name index.
    pub(crate) names: &'a FileNameIndex,
}

/// Text with every link rewritten in place to the vault-rooted path it names, or an unresolved marker.
pub(crate) struct ExpandedText(pub(crate) String);

impl TryFrom<NoteExpansion<'_>> for ExpandedText {
    type Error = ToolError;

    fn try_from(input: NoteExpansion<'_>) -> Result<Self, Self::Error> {
        let root = input
            .roots
            .root(input.note.root_id())
            .ok_or(ToolError::AttachmentUriInvalid)?;
        let targets = VaultLinkTargets {
            filesystem: input.filesystem,
            roots: input.roots,
            vault: root.name(),
            names: input.names,
            rescanned: Cell::new(false),
        };
        let note = input.note.portable_relative();
        let declared_base = declared_base(input.source, &note, &targets);
        let expanded = ExpandedMarkdown::try_from(LinkExpansionInput {
            source: input.source,
            note: &note,
            targets: &targets,
            max_distinct_targets: MAX_DISTINCT_LINK_TARGETS,
            // A link can be much shorter than the path it becomes, so the cap on what may be returned is enforced as
            // the output grows and not once it is built.
            max_output_bytes: usize::try_from(MAX_ATTACHMENT_BYTES).unwrap_or(usize::MAX),
            declared_base: declared_base.as_ref().map(BaseRecords::folder),
        })
        .map_err(ToolError::from)?;
        Ok(Self(String::from(expanded)))
    }
}

/// The base the note at `note` declares in its `base` property, resolved as every link is and read, when it names
/// one that can be read. A base that cannot be read or parsed governs nothing here: the note's links are still
/// expanded, and a name several files answer to stays ambiguous.
pub(crate) fn declared_base(source: &str, note: &str, targets: &VaultLinkTargets<'_>) -> Option<BaseRecords> {
    let declared = DeclaredBase::try_from(source).ok()?;
    let LinkResolution::Found(path) = resolve_link(targets, note, declared.target()).ok()? else {
        return None;
    };
    if !DeclaredBase::is_base_file(&path) {
        return None;
    }
    let base = VaultPath::try_from(VaultPathInput {
        roots: targets.roots,
        raw: &format!("{}://{path}", targets.vault),
    })
    .ok()?;
    let text = targets.filesystem.read_optional_text(&base).ok()??;
    BaseRecords::try_from(text.content.as_str()).ok()
}

/// A canvas's text, with its links expanded against the vault.
pub(crate) struct CanvasExpansion<'a> {
    pub(crate) source: &'a str,
    /// The canvas the text belongs to: links in its text nodes resolve from its folder.
    pub(crate) canvas: &'a VaultPath,
    pub(crate) filesystem: &'a Filesystem,
    pub(crate) roots: &'a VaultSet,
    /// The vault's file name index.
    pub(crate) names: &'a FileNameIndex,
}

impl TryFrom<CanvasExpansion<'_>> for ExpandedText {
    type Error = ToolError;

    fn try_from(input: CanvasExpansion<'_>) -> Result<Self, Self::Error> {
        let root = input
            .roots
            .root(input.canvas.root_id())
            .ok_or(ToolError::AttachmentUriInvalid)?;
        let targets = VaultLinkTargets {
            filesystem: input.filesystem,
            roots: input.roots,
            vault: root.name(),
            names: input.names,
            rescanned: Cell::new(false),
        };
        let canvas = input.canvas.portable_relative();
        let expanded = ExpandedCanvas::try_from(CanvasExpansionInput {
            source: input.source,
            canvas: &canvas,
            targets: &targets,
            max_distinct_targets: MAX_DISTINCT_LINK_TARGETS,
            max_output_bytes: usize::try_from(MAX_ATTACHMENT_BYTES).unwrap_or(usize::MAX),
        })
        .map_err(ToolError::from)?;
        Ok(Self(String::from(expanded)))
    }
}

/// An attachment to expand and the vault it is expanded against.
pub(crate) struct ExpansionInput<'a> {
    pub(crate) attachment: Attachment,
    pub(crate) filesystem: &'a Filesystem,
    pub(crate) roots: &'a VaultSet,
    /// The vault's file name index.
    pub(crate) names: &'a FileNameIndex,
}

/// An attachment whose text is the link-expanded form of the note. Its `content_hash` still describes the bytes on
/// disk, so it must never be reported alongside the expanded text.
pub(crate) struct ExpandedAttachment(pub(crate) Attachment);

impl TryFrom<ExpansionInput<'_>> for ExpandedAttachment {
    type Error = ToolError;

    fn try_from(input: ExpansionInput<'_>) -> Result<Self, Self::Error> {
        let ExpansionInput {
            mut attachment,
            filesystem,
            roots,
            names,
        } = input;
        let source = if attachment.text {
            std::str::from_utf8(&attachment.bytes).ok()
        } else {
            None
        }
        .ok_or_else(|| {
            ToolError::Filesystem(FsError::Binary {
                path: <&Path>::from(&attachment.vault_path).to_path_buf(),
            })
        })?;
        let ExpandedText(expanded) = match Expandable::try_from(&attachment.vault_path)? {
            Expandable::Note => ExpandedText::try_from(NoteExpansion {
                source,
                note: &attachment.vault_path,
                filesystem,
                roots,
                names,
            })?,
            Expandable::Canvas => ExpandedText::try_from(CanvasExpansion {
                source,
                canvas: &attachment.vault_path,
                filesystem,
                roots,
                names,
            })?,
        };
        let bytes = expanded.into_bytes();
        // The hash describes exactly the bytes held, which are no longer the file's, so it is never reported.
        attachment.content_hash = ContentHash::from(<[u8; 32]>::from(Sha256::digest(&bytes)));
        attachment.bytes = bytes;
        Ok(Self(attachment))
    }
}
