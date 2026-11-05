//! Rewrites every vault link in a Markdown document to a vault-rooted path, in place.

use crate::RecordsFolder;
use crate::link_resolution::{ExpansionError, LinkResolution, LinkTargets, Resolver};
use crate::link_scan::{
    Destination, Piece, ReferenceDefinition, ReferencePiece, Scanner, StandardLink, is_external_target,
};

/// Marks a link whose target names no file. Everything after it is the original target text.
///
/// No real vault path can begin with this: `VaultPath::try_new` rejects any `:` in a path, so a segment containing one
/// can never collide with a folder.
pub const NOT_FOUND_MARKER: &str = "/:!";

/// Marks a link whose target names more than one file. Everything after it is the original target text.
pub const AMBIGUOUS_MARKER: &str = "/:?";

/// A document to expand and the vault it is expanded against.
#[derive(Debug)]
pub struct LinkExpansionInput<'a, T> {
    /// The document text, frontmatter included.
    pub source: &'a str,
    /// The vault-relative path of the note the text belongs to.
    pub note: &'a str,
    /// What the vault can say about a link target.
    pub targets: &'a T,
    /// The most distinct link targets to resolve. A note naming more fails, and is never partly expanded.
    pub max_distinct_targets: usize,
    /// The most bytes the expanded note may hold. Expansion stops as soon as the output passes it.
    pub max_output_bytes: usize,
    /// The records folder of the base the note declares in its `base` property, when it names one. A name several
    /// files answer to resolves to the only one of them inside it.
    pub declared_base: Option<RecordsFolder<'a>>,
}

/// A document whose links have all been rewritten to vault-rooted paths or marked as unresolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpandedMarkdown(String);

impl<T: LinkTargets> TryFrom<LinkExpansionInput<'_, T>> for ExpandedMarkdown {
    type Error = ExpansionError<T::Error>;

    fn try_from(value: LinkExpansionInput<'_, T>) -> Result<Self, Self::Error> {
        let mut resolver = Resolver::new(value.targets, value.note, value.max_distinct_targets);
        resolver.narrow_to(value.declared_base);
        expand_markdown(value.source, &mut resolver, value.max_output_bytes).map(Self)
    }
}

/// `source` with every link rewritten through `resolver`, failing as soon as the output passes `max_output_bytes`.
pub(crate) fn expand_markdown<T: LinkTargets>(
    source: &str,
    resolver: &mut Resolver<'_, T>,
    max_output_bytes: usize,
) -> Result<String, ExpansionError<T::Error>> {
    let mut out = String::with_capacity(source.len());
    for piece in Scanner::from(source) {
        match piece {
            Piece::Text(text) => out.push_str(text),
            Piece::UnclosedWikilink { covered, .. } | Piece::Reference(ReferencePiece { covered, .. }) => {
                out.push_str(covered);
            }
            Piece::Wikilink(wikilink) => {
                match expand_wikilink(wikilink.embed, wikilink.inner, resolver, wikilink.in_table)? {
                    Some(rewritten) => out.push_str(&rewritten),
                    None => out.push_str(wikilink.raw),
                }
            }
            Piece::Standard(standard) => {
                if standard.embed {
                    out.push('!');
                }
                out.push_str(&standard.link.expanded(resolver)?);
            }
            Piece::Definition(definition) => out.push_str(&definition.expanded(resolver)?),
        }
        if out.len() > max_output_bytes {
            return Err(ExpansionError::OutputTooLarge {
                maximum: max_output_bytes,
            });
        }
    }
    Ok(out)
}

impl From<ExpandedMarkdown> for String {
    fn from(value: ExpandedMarkdown) -> Self {
        value.0
    }
}

impl AsRef<str> for ExpandedMarkdown {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Whether `target` is something other than a vault file: empty, an anchor, protocol-relative, a URI with a scheme,
/// or a link this stage already rewrote.
fn is_left_as_written(target: &str) -> bool {
    is_external_target(target) || target.starts_with(NOT_FOUND_MARKER) || target.starts_with(AMBIGUOUS_MARKER)
}

fn marked(resolution: &LinkResolution, original: &str) -> Option<String> {
    match resolution {
        LinkResolution::Found(_) => None,
        LinkResolution::NotFound => Some(format!("{NOT_FOUND_MARKER}{original}")),
        LinkResolution::Ambiguous { .. } => Some(format!("{AMBIGUOUS_MARKER}{original}")),
    }
}

/// `[[destination|display]]` (or its embed) with the destination rooted, or `None` when it is left as written.
///
/// Inside a table row a literal `|` is written `\|`, so that is what separates the display there, and it is kept.
fn expand_wikilink<T: LinkTargets>(
    embed: bool,
    inner: &str,
    resolver: &mut Resolver<'_, T>,
    in_table: bool,
) -> Result<Option<String>, ExpansionError<T::Error>> {
    let (destination, display, separator) = match inner.split_once('|') {
        Some((before, display)) => match before.strip_suffix('\\') {
            Some(unescaped) => (unescaped.trim(), Some(display), "\\|"),
            None => (before.trim(), Some(display), "|"),
        },
        None => (inner.trim(), None, if in_table { "\\|" } else { "|" }),
    };
    let (target, fragment) = destination.split_once('#').map_or((destination, ""), |(target, _)| {
        (target, destination.strip_prefix(target).unwrap_or(""))
    });
    // A wikilink names a note and never an address, so a target that looks like a URL scheme is looked up like any other.
    if target.is_empty() || target.starts_with(NOT_FOUND_MARKER) || target.starts_with(AMBIGUOUS_MARKER) {
        return Ok(None);
    }
    let resolution = resolver.resolve(target)?;
    let rewritten = match &resolution {
        // A path holding a character that ends or splits a wikilink cannot be written in one, and Obsidian does not
        // allow such names, so the link is treated as naming nothing.
        LinkResolution::Found(path) if !path.contains(['|', '#', '[', ']']) => format!("/{path}{fragment}"),
        LinkResolution::Found(_) => format!("{NOT_FOUND_MARKER}{destination}"),
        other => match marked(other, destination) {
            Some(marked) => marked,
            None => return Ok(None),
        },
    };
    let display = match display {
        Some(display) => format!("{separator}{display}"),
        None if !embed && rewritten != destination => format!("{separator}{destination}"),
        None => String::new(),
    };
    let bang = if embed { "!" } else { "" };
    Ok(Some(format!("{bang}[[{rewritten}{display}]]")))
}

impl StandardLink<'_> {
    fn expanded<T: LinkTargets>(&self, resolver: &mut Resolver<'_, T>) -> Result<String, ExpansionError<T::Error>> {
        Ok(format!(
            "[{}]({}{}{})",
            self.text,
            self.lead,
            self.destination.expanded(resolver)?,
            self.tail
        ))
    }
}

impl Destination<'_> {
    /// The destination rooted, marked, or as written when it is not a vault target, in the author's form.
    fn expanded<T: LinkTargets>(&self, resolver: &mut Resolver<'_, T>) -> Result<String, ExpansionError<T::Error>> {
        Ok(match self.rewritten(resolver)? {
            Some(rewritten) => rewritten,
            None => self.original(),
        })
    }

    fn original(&self) -> String {
        if self.bracketed {
            format!("<{}>", self.text)
        } else {
            self.text.to_owned()
        }
    }

    fn rewritten<T: LinkTargets>(
        &self,
        resolver: &mut Resolver<'_, T>,
    ) -> Result<Option<String>, ExpansionError<T::Error>> {
        if is_left_as_written(self.text) {
            return Ok(None);
        }
        let (path, fragment) = self.path_and_fragment();
        let decoded = self.decoded(path);
        let resolution = resolver.resolve(&decoded)?;
        let rewritten = match &resolution {
            LinkResolution::Found(found) => return Ok(self.rooted(found, path, &decoded, fragment)),
            other => match marked(other, self.text) {
                Some(marked) => marked,
                None => return Ok(None),
            },
        };
        Ok(Some(if self.bracketed {
            format!("<{rewritten}>")
        } else {
            rewritten
        }))
    }

    /// The destination for a link that names `found`. A destination written as a path keeps exactly the text it was
    /// written with and gains only the folder prefix that makes it rooted, in the form (bare or bracketed) the author
    /// used. One that climbs with `..`, or that needed `.md` appended, is replaced by the path it names.
    fn rooted(&self, found: &str, written: &str, decoded: &str, fragment: &str) -> Option<String> {
        let prefix = if written.starts_with('/') {
            return (found != decoded.trim_start_matches('/')).then(|| self.whole(found, fragment));
        } else if found == decoded {
            Some("")
        } else {
            found
                .strip_suffix(decoded)
                .filter(|prefix| prefix.ends_with('/') && !decoded.starts_with("./") && !decoded.starts_with("../"))
        };
        let Some(prefix) = prefix else {
            return Some(self.whole(found, fragment));
        };
        Some(if self.bracketed {
            format!("</{prefix}{written}{fragment}>")
        } else {
            format!("/{}{written}{fragment}", percent_encoded(prefix))
        })
    }

    /// `found` as a whole rooted destination in the author's form.
    fn whole(&self, found: &str, fragment: &str) -> String {
        if self.bracketed {
            format!("</{found}{fragment}>")
        } else {
            format!("/{}{fragment}", percent_encoded(found))
        }
    }
}

impl ReferenceDefinition<'_> {
    fn expanded<T: LinkTargets>(&self, resolver: &mut Resolver<'_, T>) -> Result<String, ExpansionError<T::Error>> {
        Ok(format!(
            "{}{}{}",
            self.lead,
            self.destination.expanded(resolver)?,
            self.tail
        ))
    }
}

/// `text` with the characters that end or delimit a bare link destination percent-encoded.
fn percent_encoded(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_whitespace() || matches!(c, '%' | '#' | '<' | '>' | '(' | ')') {
            let mut buffer = [0_u8; 4];
            for byte in c.encode_utf8(&mut buffer).bytes() {
                encoded.push('%');
                encoded.extend([byte >> 4, byte & 0x0F].into_iter().filter_map(|nibble| {
                    char::from_digit(u32::from(nibble), 16).map(|digit| digit.to_ascii_uppercase())
                }));
            }
        } else {
            encoded.push(c);
        }
    }
    encoded
}

#[cfg(test)]
#[path = "link_expansion_test.rs"]
mod tests;
