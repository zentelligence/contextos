//! Reading the series template a base's sidecar declares: resolving its link as every link is resolved, reading only
//! its frontmatter within the vault's read limit, and turning it into the fields a record of the series must carry.
//!
//! `base_record_create` stamps those fields and refuses a conflicting value; the `Record conformance` doctor check
//! compares existing records with them. Both read a template through [`TemplateReader`], so the two agree on which
//! templates can be used and why one cannot.

use std::fmt::{self, Display, Formatter};

use contextos_core::{PathError, VaultPath, VaultPathInput, VaultSet};
use contextos_fs::{FileNameIndex, Filesystem, FsError, ReadTextRequest};
use contextos_obsidian::{
    FixedFields, FrontmatterDocument, LinkResolution, LinkTarget, LinkTargets, SeriesTemplateError,
    SeriesTemplateInput, resolve_link,
};
use serde_json::{Map, Value};

use crate::server::MAX_INDEXED_FILES;
use crate::tools::fs_expand::VaultLinkTargets;

/// The name `format/series-template` and the doctor give a template that applies to every series of a base.
pub(crate) const EVERY_SERIES: &str = "every series";

/// A vault whose templates are to be read.
pub(crate) struct TemplateVault<'a> {
    pub(crate) filesystem: &'a Filesystem,
    pub(crate) roots: &'a VaultSet,
    /// The vault's configured name.
    pub(crate) name: &'a str,
}

/// Reads templates from one vault, asking for the vault's file names at most once however many are read.
pub(crate) struct TemplateReader<'a> {
    vault: TemplateVault<'a>,
    names: FileNameIndex,
}

impl<'a> TryFrom<TemplateVault<'a>> for TemplateReader<'a> {
    type Error = PathError;

    fn try_from(vault: TemplateVault<'a>) -> Result<Self, Self::Error> {
        let root = VaultPath::try_from(VaultPathInput {
            roots: vault.roots,
            raw: &format!("{}://.", vault.name),
        })?;
        let names = FileNameIndex::new(vault.filesystem.clone(), root, MAX_INDEXED_FILES);
        Ok(Self { vault, names })
    }
}

/// Which template to read and which of its fields to fix.
#[derive(Clone, Copy)]
pub(crate) struct TemplateSpec<'a> {
    /// The sidecar that declares the template, vault-relative: a bare link resolves from its folder.
    pub(crate) sidecar: &'a str,
    /// The link as the sidecar wrote it.
    pub(crate) link: &'a str,
    /// The sidecar's `fixed_fields`, beyond `type`.
    pub(crate) fixed_fields: &'a [String],
}

/// A template that was read and can be used.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedTemplate {
    /// The template note's vault-relative path.
    pub(crate) path: String,
    pub(crate) fixed: FixedFields,
}

/// Why a declared template cannot be used.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TemplateProblem {
    /// The declaration is not a wikilink.
    NotALink,
    /// The link names no note.
    NotFound,
    /// The link names several notes and nothing nearer settles which.
    Ambiguous,
    /// The link names a file that is not a Markdown note.
    NotMarkdown { path: String },
    /// The note is a base's sidecar (it sits beside a `.base` file of the same name), which carries the base's
    /// counters and declarations rather than the fields of a record.
    BaseSidecar { path: String },
    /// The note is larger than the vault's read limit.
    TooLarge { path: String },
    /// The note could not be read.
    Unreadable { path: String },
    /// The note's frontmatter is not valid.
    MalformedFrontmatter { path: String },
    /// The note's frontmatter lacks a literal value for a field it must fix.
    Unusable { path: String, error: SeriesTemplateError },
}

impl Display for TemplateProblem {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotALink => formatter.write_str("it is not a link to a note"),
            Self::NotFound => formatter.write_str("not found"),
            Self::Ambiguous => formatter.write_str("more than one note matches it"),
            Self::NotMarkdown { path } => write!(formatter, "{path} is not a Markdown note"),
            Self::BaseSidecar { path } => {
                write!(formatter, "{path} resolves to a base sidecar, which is not a template")
            }
            Self::TooLarge { path } => write!(formatter, "{path} is larger than the vault's read limit"),
            Self::Unreadable { path } => write!(formatter, "{path} could not be read"),
            Self::MalformedFrontmatter { path } => write!(formatter, "the frontmatter of {path} cannot be parsed"),
            Self::Unusable { path, error } => write!(formatter, "{path}: {error}"),
        }
    }
}

impl TemplateProblem {
    /// What to change so the template can be used.
    pub(crate) fn remediation(&self) -> String {
        match self {
            Self::NotALink => "Declare the template as one wikilink such as [[templates/task]], and retry.".to_owned(),
            Self::NotFound => "Create the template note, or correct the link in the sidecar's template or series_templates, and retry."
                .to_owned(),
            Self::Ambiguous => {
                "Write the link with enough of its path to name one note, for example [[templates/task]], and retry."
                    .to_owned()
            }
            Self::NotMarkdown { .. } => {
                "Point the link at a Markdown note (.md) whose frontmatter holds the record's fixed fields, and retry."
                    .to_owned()
            }
            Self::BaseSidecar { .. } => {
                "Point the link at a template note, not at a base's sidecar, and retry.".to_owned()
            }
            Self::TooLarge { .. } => {
                "Shorten the template note below the vault's read limit (its frontmatter is all that is used), and retry."
                    .to_owned()
            }
            Self::Unreadable { .. } => "Check that the template note is readable text, and retry.".to_owned(),
            Self::MalformedFrontmatter { .. } => {
                "Correct the template note's YAML frontmatter, and retry.".to_owned()
            }
            Self::Unusable { path, error } => {
                let (SeriesTemplateError::NotLiteral { field } | SeriesTemplateError::Missing { field }) = error;
                format!(
                    "Give {path} a single literal {field}, or remove {field} from the sidecar's fixed_fields, and retry."
                )
            }
        }
    }
}

impl TemplateReader<'_> {
    /// Resolves and reads the template `spec` names.
    ///
    /// # Errors
    ///
    /// Returns why the template cannot be used. A template is never skipped or guessed.
    pub(crate) fn read(&self, spec: TemplateSpec<'_>) -> Result<ResolvedTemplate, TemplateProblem> {
        let target = LinkTarget::try_from(spec.link).map_err(|_| TemplateProblem::NotALink)?;
        let targets =
            VaultLinkTargets::over_fresh_index(self.vault.filesystem, self.vault.roots, self.vault.name, &self.names);
        let path = match resolve_link(&targets, spec.sidecar, target.as_str()) {
            Ok(LinkResolution::Found(path)) => path,
            Ok(LinkResolution::Ambiguous { .. }) => return Err(TemplateProblem::Ambiguous),
            Ok(LinkResolution::NotFound) => return Err(TemplateProblem::NotFound),
            Err(_) => {
                return Err(TemplateProblem::Unreadable {
                    path: target.as_str().to_owned(),
                });
            }
        };
        if !std::path::Path::new(&path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        {
            return Err(TemplateProblem::NotMarkdown { path });
        }
        let paired_base = std::path::Path::new(&path).with_extension("base");
        if targets
            .is_file(&paired_base.to_string_lossy())
            .map_err(|_| TemplateProblem::Unreadable { path: path.clone() })?
        {
            return Err(TemplateProblem::BaseSidecar { path });
        }
        let note = VaultPath::try_from(VaultPathInput {
            roots: self.vault.roots,
            raw: &format!("{}://{path}", self.vault.name),
        })
        .map_err(|_| TemplateProblem::Unreadable { path: path.clone() })?;
        let source = self
            .vault
            .filesystem
            .read_text(&ReadTextRequest {
                path: note,
                limit: None,
            })
            .map_err(|error| match error {
                FsError::TooLarge { .. } => TemplateProblem::TooLarge { path: path.clone() },
                _ => TemplateProblem::Unreadable { path: path.clone() },
            })?;
        let document = FrontmatterDocument::try_from(source.content.as_str())
            .map_err(|_| TemplateProblem::MalformedFrontmatter { path: path.clone() })?;
        let fixed = FixedFields::try_from(SeriesTemplateInput {
            template: document.frontmatter(),
            fixed_fields: spec.fixed_fields,
        })
        .map_err(|error| TemplateProblem::Unusable {
            path: path.clone(),
            error,
        })?;
        Ok(ResolvedTemplate { path, fixed })
    }
}

impl ResolvedTemplate {
    /// The fixed fields `supplied` gives a value that differs from the template's by type or by value, in the
    /// template's order. A field `supplied` leaves out is not a conflict.
    pub(crate) fn conflicting_fields(&self, supplied: &Map<String, Value>) -> Vec<String> {
        self.fixed
            .fields()
            .iter()
            .filter(|(name, value)| supplied.get(name.as_str()).is_some_and(|given| given != *value))
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// `supplied` with every fixed field it leaves out added after its own fields.
    pub(crate) fn stamped(&self, supplied: &Map<String, Value>) -> Map<String, Value> {
        let mut stamped = supplied.clone();
        for (name, value) in self.fixed.fields() {
            if !stamped.contains_key(name) {
                stamped.insert(name.clone(), value.clone());
            }
        }
        stamped
    }
}

#[cfg(test)]
#[path = "base_record_template_test.rs"]
mod tests;
