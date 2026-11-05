//! Resolution of link targets to vault files: the pure half of link expansion.
//!
//! A target is looked for where Obsidian's shortest-path links point: from the linking note's own folder and each
//! ancestor folder up to the vault root, nearest first, by direct existence checks. Only a target found nowhere in
//! that tree is looked up by file name, and that lookup, like the checks, is the vault's to answer through
//! [`LinkTargets`]. Nothing here enumerates a vault.

use std::collections::HashMap;
use std::fmt;

use crate::RecordsFolder;

/// What a vault can say about a link target, without listing itself.
pub trait LinkTargets {
    /// Why a question could not be answered. A failure fails the expansion; it is never read as "no such file".
    type Error;

    /// Whether `path`, vault-relative with `/` separators, names an existing file.
    ///
    /// # Errors
    ///
    /// Returns the vault's error when the question cannot be answered.
    fn is_file(&self, path: &str) -> Result<bool, Self::Error>;

    /// The files whose trailing path segments equal `suffix`, ignoring case: `foo.md` matches `foo.md` and
    /// `wiki/Foo.md` but not `xfoo.md`.
    ///
    /// # Errors
    ///
    /// Returns the vault's error when the question cannot be answered.
    fn files_ending_with(&self, suffix: &str) -> Result<Vec<String>, Self::Error>;
}

/// Why a note's links could not be expanded.
#[derive(Debug, Eq, PartialEq)]
pub enum ExpansionError<E> {
    /// The vault could not answer a question about a target.
    Lookup(E),
    /// The note names more distinct link targets than one expansion will resolve.
    TooManyTargets { maximum: usize },
    /// The expanded note grew beyond the largest result one expansion may produce. A link can be much shorter than the
    /// path it becomes, so this is checked as the output grows and not once it is built.
    OutputTooLarge { maximum: usize },
    /// A canvas to expand is not a JSON Canvas: not JSON, or its `nodes` is not an array of objects.
    InvalidCanvas { reason: String },
}

impl<E: fmt::Display> fmt::Display for ExpansionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lookup(error) => error.fmt(formatter),
            Self::TooManyTargets { maximum } => {
                write!(formatter, "the note names more than {maximum} distinct link targets")
            }
            Self::OutputTooLarge { maximum } => {
                write!(formatter, "the expanded note is larger than {maximum} bytes")
            }
            Self::InvalidCanvas { reason } => write!(formatter, "the canvas could not be read: {reason}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for ExpansionError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Lookup(error) => Some(error),
            Self::TooManyTargets { .. } | Self::OutputTooLarge { .. } | Self::InvalidCanvas { .. } => None,
        }
    }
}

/// What a link target names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkResolution {
    /// The one file the target names, vault-relative with `/` separators.
    Found(String),
    /// No file answers to the target.
    NotFound,
    /// More than one file answers to the target by name, and none is nearer the linking note than the others.
    Ambiguous {
        /// Every file that answers to it, sorted, vault-relative with `/` separators.
        candidates: Vec<String>,
    },
}

/// Resolves one link `target` written in `note` (vault-relative) exactly as link expansion does. A target ending in `/`
/// names a folder and finds the `index.md` inside it. A rooted target is
/// exact; any other is tried from the note's folder and each ancestor up to the vault root, nearest first, and only
/// then looked up by name, where more than one match is ambiguous. Every reader of a link, whether a body link, a
/// frontmatter link, or a record's `base` property, resolves it through this one rule, so a bare `[[<name>.base]]`, of
/// any name, names the same base wherever it is read.
///
/// # Errors
///
/// Returns the vault's error when a question about the target cannot be answered.
pub fn resolve_link<T: LinkTargets>(targets: &T, note: &str, target: &str) -> Result<LinkResolution, T::Error> {
    resolve_link_narrowed(targets, note, target, None)
}

/// As [`resolve_link`], for a note that declares a base keeping its records in `declared_base`: a name that several
/// files answer to resolves to the only one of them inside that folder, as Obsidian's base-relative links expect.
///
/// # Errors
///
/// Returns the vault's error when a question about the target cannot be answered.
pub fn resolve_link_narrowed<'a, T: LinkTargets>(
    targets: &'a T,
    note: &'a str,
    target: &str,
    declared_base: Option<RecordsFolder<'a>>,
) -> Result<LinkResolution, T::Error> {
    let mut resolver = Resolver::new(targets, note, 1);
    resolver.narrow_to(declared_base);
    resolver.resolve_target(target.trim())
}

/// Resolves the targets of the links in one note, asking the vault each distinct question once.
pub(crate) struct Resolver<'a, T: LinkTargets> {
    targets: &'a T,
    folder: &'a str,
    files: HashMap<String, bool>,
    names: HashMap<String, Vec<String>>,
    resolved: HashMap<String, LinkResolution>,
    maximum: usize,
    /// The records folder of the base the linking note declares, which settles a name several files answer to.
    declared_base: Option<RecordsFolder<'a>>,
}

impl<'a, T: LinkTargets> Resolver<'a, T> {
    /// A resolver for links written in `note`, a vault-relative path, that will resolve at most `maximum` distinct
    /// targets: each costs the vault a handful of questions, so a note cannot be allowed to name an unbounded number.
    pub(crate) fn new(targets: &'a T, note: &'a str, maximum: usize) -> Self {
        Self {
            targets,
            folder: note.rsplit_once('/').map_or("", |(folder, _)| folder),
            files: HashMap::new(),
            names: HashMap::new(),
            resolved: HashMap::new(),
            maximum,
            declared_base: None,
        }
    }

    /// Settles a name several files answer to by the only one of them inside `declared_base`, the records folder of
    /// the base the linking note declares, as Obsidian's base-relative links expect.
    pub(crate) fn narrow_to(&mut self, declared_base: Option<RecordsFolder<'a>>) {
        self.declared_base = declared_base;
    }

    /// The file `target` names.
    ///
    /// A rooted target is exact, and so is a `./` or `../` target taken from the note's folder. Any other target is
    /// tried from the note's folder and each ancestor up to the vault root, nearest first, and then looked up by
    /// name among the notes: one match is found, more than one is ambiguous. Each attempt tries the target as written
    /// and then with `.md` appended, so a name containing a dot still finds its note. A full path must match
    /// exactly; the name lookup ignores case, as Obsidian does.
    pub(crate) fn resolve(&mut self, target: &str) -> Result<LinkResolution, ExpansionError<T::Error>> {
        let target = target.trim();
        if target.is_empty() {
            return Ok(LinkResolution::NotFound);
        }
        if let Some(known) = self.resolved.get(target) {
            return Ok(known.clone());
        }
        if self.resolved.len() >= self.maximum {
            return Err(ExpansionError::TooManyTargets { maximum: self.maximum });
        }
        let resolution = self.resolve_target(target).map_err(ExpansionError::Lookup)?;
        self.resolved.insert(target.to_owned(), resolution.clone());
        Ok(resolution)
    }

    /// [`Self::resolve`]'s rule for one trimmed target, uncached and uncounted.
    fn resolve_target(&mut self, target: &str) -> Result<LinkResolution, T::Error> {
        if target.is_empty() {
            return Ok(LinkResolution::NotFound);
        }
        let names_folder = target.ends_with('/');
        if let Some(rooted) = target.strip_prefix('/') {
            return self.exact(rooted, names_folder);
        }
        if target.starts_with("./") || target.starts_with("../") {
            return self.exact(&format!("{}/{target}", self.folder), names_folder);
        }
        let mut directory = Some(self.folder);
        while let Some(current) = directory {
            let found = self.exact(&format!("{current}/{target}"), names_folder)?;
            if matches!(found, LinkResolution::Found(_)) {
                return Ok(found);
            }
            directory = current
                .rsplit_once('/')
                .map(|(parent, _)| parent)
                .or(if current.is_empty() { None } else { Some("") });
        }
        // A folder is found only by its path: no file is named for the folder it sits in.
        if names_folder {
            return Ok(LinkResolution::NotFound);
        }
        self.by_name(target)
    }

    /// The file at `path`, which names a folder when `names_folder`: the folder's `index.md`.
    fn exact(&mut self, path: &str, names_folder: bool) -> Result<LinkResolution, T::Error> {
        let Some(path) = normalise(path) else {
            return Ok(LinkResolution::NotFound);
        };
        let candidates = if names_folder {
            vec![format!("{path}/index.md")]
        } else {
            with_note_extension(&path)
        };
        for candidate in candidates {
            if self.is_file(&candidate)? {
                return Ok(LinkResolution::Found(candidate));
            }
        }
        Ok(LinkResolution::NotFound)
    }

    fn is_file(&mut self, path: &str) -> Result<bool, T::Error> {
        if let Some(known) = self.files.get(path) {
            return Ok(*known);
        }
        let exists = self.targets.is_file(path)?;
        self.files.insert(path.to_owned(), exists);
        Ok(exists)
    }

    /// The one file named by `target` among all the vault's files, trying the name as written and then with `.md`
    /// appended, so a file with any extension is found and a bare note name finds its note.
    fn by_name(&mut self, target: &str) -> Result<LinkResolution, T::Error> {
        let Some(target) = normalise(target) else {
            return Ok(LinkResolution::NotFound);
        };
        for candidate in with_note_extension(&target) {
            if !self.names.contains_key(&candidate) {
                let matches = self.targets.files_ending_with(&candidate)?;
                self.names.insert(candidate.clone(), matches);
            }
            match self.names.get(&candidate).map(Vec::as_slice) {
                Some([only]) => return Ok(LinkResolution::Found(only.clone())),
                Some(several @ [_, _, ..]) => {
                    let mut in_base = several
                        .iter()
                        .filter(|path| self.declared_base.is_some_and(|base| base.contains(path)));
                    return Ok(match (in_base.next(), in_base.next()) {
                        (Some(only), None) => LinkResolution::Found(only.clone()),
                        _ => LinkResolution::Ambiguous {
                            candidates: several.to_vec(),
                        },
                    });
                }
                _ => {}
            }
        }
        Ok(LinkResolution::NotFound)
    }
}

/// The forms of `path` to try. A name with no extension is a note, so `.md` comes first and the name as written
/// after it. A name that already ends `.md` is tried as it is. A name with another extension is tried as written and
/// then as a note, which covers a dotted note name such as `Release 1.2`.
fn with_note_extension(path: &str) -> Vec<String> {
    let extension = std::path::Path::new(path).extension();
    if extension.is_some_and(|extension| extension.eq_ignore_ascii_case("md")) {
        vec![path.to_owned()]
    } else if extension.is_none() {
        vec![format!("{path}.md"), path.to_owned()]
    } else {
        vec![path.to_owned(), format!("{path}.md")]
    }
}

/// `path` with empty and `.` segments dropped and `..` segments applied, or `None` when it climbs above the root.
fn normalise(path: &str) -> Option<String> {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            other => segments.push(other),
        }
    }
    (!segments.is_empty()).then(|| segments.join("/"))
}

#[cfg(test)]
#[path = "link_resolution_test.rs"]
mod tests;
