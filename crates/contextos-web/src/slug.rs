//! A single small, shared "title to filename-safe slug" conversion, used by
//! every Knowledge screen that creates a brand-new page from an
//! operator-typed title (`wiki::create_page`, `insights::create_insight`):
//! kept in one place rather than duplicated per screen, since the two
//! already need byte-for-byte identical behaviour.

/// `title`, lower-cased and reduced to `[a-z0-9]` runs joined by single
/// hyphens, with no leading, trailing, or doubled hyphen - a filename-safe
/// slug. Empty when `title` has no sluggable character at all.
#[must_use]
pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut last_was_hyphen = true;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            slug.push('-');
            last_was_hyphen = true;
        }
    }
    slug.trim_end_matches('-').to_owned()
}

#[cfg(test)]
#[path = "slug_test.rs"]
mod tests;
