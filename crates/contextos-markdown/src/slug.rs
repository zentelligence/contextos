//! Heading anchors.

use std::collections::HashSet;

/// The anchor a heading's text gives: lower-cased, every run of other characters one `-`, with none at either end.
/// A link to `Note#Some Heading` and the heading `Some Heading` agree because both pass through here.
#[must_use]
pub(crate) fn slug(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    let mut hyphen_due = false;
    for c in text.chars() {
        if c.is_alphanumeric() {
            if hyphen_due && !slug.is_empty() {
                slug.push('-');
            }
            hyphen_due = false;
            slug.extend(c.to_lowercase());
        } else {
            hyphen_due = true;
        }
    }
    slug
}

/// Hands out element ids unique across one page, so a page and the notes embedded in it never share one.
#[derive(Debug, Default)]
pub(crate) struct Anchors {
    taken: HashSet<String>,
}

impl Anchors {
    /// A unique id for a heading with `text`: its slug, or failing that the slug with the first free `-1`, `-2`, and
    /// so on.
    pub(crate) fn heading(&mut self, text: &str) -> String {
        let base = slug(text);
        let base = if base.is_empty() { "section".to_owned() } else { base };
        let mut candidate = base.clone();
        let mut suffix = 0_usize;
        while self.taken.contains(&candidate) {
            suffix = suffix.saturating_add(1);
            candidate = format!("{base}-{suffix}");
        }
        self.taken.insert(candidate.clone());
        candidate
    }
}

#[cfg(test)]
#[path = "slug_test.rs"]
mod tests;
