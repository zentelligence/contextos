//! What a link destination in an expanded note names.

/// A place within a page: a heading, by its text, or a block, by its id.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Fragment {
    Heading(String),
    Block(String),
}

impl Fragment {
    /// The element id this place has on its rendered page: a heading's anchor, or `^` and a block's id.
    #[must_use]
    pub fn anchor(&self) -> String {
        match self {
            Self::Heading(heading) => crate::slug::slug(heading),
            Self::Block(id) => format!("^{id}"),
        }
    }
}

/// A destination as it appears in an expanded note.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkDestination<'a> {
    /// The destination text.
    pub text: &'a str,
    /// Whether the text is percent-encoded, as a standard link's destination is and a wikilink's is not.
    pub percent_encoded: bool,
}

/// What a destination names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkTarget {
    /// A file in the vault, by its vault-relative path, and an optional place within it.
    Vault { path: String, fragment: Option<Fragment> },
    /// A web page or an email address: `http`, `https`, or `mailto`, or protocol-relative `//host`.
    External(String),
    /// A place on the same page.
    Anchor(Fragment),
    /// The server found no file for this link. The original text, for display only.
    NotFound(String),
    /// The server found several files for this link. The original text, for display only.
    Ambiguous(String),
    /// Anything else: a scheme that is not allowed, or a relative destination the server did not expand. The original
    /// text, for display only.
    Unresolved(String),
}

/// The schemes a link may leave the vault by. Anything else, `javascript:` and `data:` included, is never a link.
const EXTERNAL_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

impl From<LinkDestination<'_>> for LinkTarget {
    fn from(destination: LinkDestination<'_>) -> Self {
        let text = destination.text.trim();
        // The markers come first and their remainder is opaque: it is the author's text, never a path or a URL.
        if let Some(original) = text.strip_prefix(contextos_obsidian::NOT_FOUND_MARKER) {
            return Self::NotFound(original.to_owned());
        }
        if let Some(original) = text.strip_prefix(contextos_obsidian::AMBIGUOUS_MARKER) {
            return Self::Ambiguous(original.to_owned());
        }
        if text.starts_with("//") {
            return Self::External(text.to_owned());
        }
        if let Some(scheme) = scheme_of(text) {
            return if EXTERNAL_SCHEMES
                .iter()
                .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
            {
                Self::External(text.to_owned())
            } else {
                Self::Unresolved(text.to_owned())
            };
        }
        let decode = |part: &str| {
            if destination.percent_encoded {
                percent_decoded(part).unwrap_or_else(|| part.to_owned())
            } else {
                part.to_owned()
            }
        };
        if let Some(anchor) = text.strip_prefix('#') {
            return Self::Anchor(Fragment::from(decode(anchor)));
        }
        let Some(rooted) = text.strip_prefix('/') else {
            return Self::Unresolved(text.to_owned());
        };
        let (path, fragment) = match rooted.split_once('#') {
            Some((path, fragment)) => (path, Some(Fragment::from(decode(fragment)))),
            None => (rooted, None),
        };
        let path = decode(path);
        if path.is_empty() {
            return Self::Unresolved(text.to_owned());
        }
        Self::Vault { path, fragment }
    }
}

impl From<String> for Fragment {
    /// `^id` is a block; anything else is a heading's text.
    fn from(fragment: String) -> Self {
        match fragment.strip_prefix('^') {
            Some(block) => Self::Block(block.to_owned()),
            None => Self::Heading(fragment),
        }
    }
}

/// The scheme of `text`, when it begins with one: a letter, then letters, digits, `+`, `-`, or `.`, then `:`.
fn scheme_of(text: &str) -> Option<&str> {
    let (scheme, _) = text.split_once(':')?;
    let mut chars = scheme.chars();
    (chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then_some(scheme)
}

/// `text` with each `%XX` sequence replaced by its byte, or `None` when the result is not valid UTF-8.
fn percent_decoded(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(byte) = bytes.get(at) {
        let escaped = (*byte == b'%')
            .then(|| bytes.get(at.saturating_add(1)..at.saturating_add(3)))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        if let Some(value) = escaped {
            decoded.push(value);
            at = at.saturating_add(3);
        } else {
            decoded.push(*byte);
            at = at.saturating_add(1);
        }
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
#[path = "link_target_test.rs"]
mod tests;
