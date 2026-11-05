//! `%%comments%%`, removed before anything else reads the body.

use std::ops::Range;

/// A body and the byte ranges of code in it, where `%%` is text.
pub(crate) struct CommentInput<'a> {
    pub(crate) text: &'a str,
    pub(crate) code: &'a [Range<usize>],
}

/// The body with every closed comment removed. Each line break inside a comment is kept, so a line of the result is
/// the same line of the note.
pub(crate) struct CommentFree(String);

impl From<CommentInput<'_>> for CommentFree {
    fn from(input: CommentInput<'_>) -> Self {
        let mut out = String::with_capacity(input.text.len());
        let mut copied = 0;
        for comment in closed_comments(&input) {
            out.push_str(input.text.get(copied..comment.start).unwrap_or_default());
            let hidden = input.text.get(comment.clone()).unwrap_or_default();
            out.extend(hidden.chars().filter(|c| *c == '\n'));
            copied = comment.end;
        }
        out.push_str(input.text.get(copied..).unwrap_or_default());
        Self(out)
    }
}

/// The body with every closed comment blanked out byte for byte: each character inside a comment, except a line break,
/// becomes as many spaces as it has bytes, so every byte offset into the result is the same offset into the note.
pub(crate) struct CommentMasked(String);

impl From<CommentInput<'_>> for CommentMasked {
    fn from(input: CommentInput<'_>) -> Self {
        let mut out = String::with_capacity(input.text.len());
        let mut copied = 0;
        for comment in closed_comments(&input) {
            out.push_str(input.text.get(copied..comment.start).unwrap_or_default());
            for character in input.text.get(comment.clone()).unwrap_or_default().chars() {
                if character == '\n' {
                    out.push('\n');
                } else {
                    out.extend(std::iter::repeat_n(' ', character.len_utf8()));
                }
            }
            copied = comment.end;
        }
        out.push_str(input.text.get(copied..).unwrap_or_default());
        Self(out)
    }
}

impl From<CommentMasked> for String {
    fn from(value: CommentMasked) -> Self {
        value.0
    }
}

/// Each closed comment's bytes, `%%` markers included, in order. Markers outside code pair up in order, so a lone last
/// one, an unclosed comment, stays as text.
fn closed_comments(input: &CommentInput<'_>) -> Vec<Range<usize>> {
    let in_code = |at: usize| input.code.iter().any(|code| code.contains(&at));
    let markers: Vec<usize> = input
        .text
        .match_indices("%%")
        .map(|(at, _)| at)
        .filter(|at| !in_code(*at) && !in_code(at.saturating_add(1)))
        .collect();
    let (pairs, _) = markers.as_chunks::<2>();
    pairs
        .iter()
        .map(|&[open, close]| open..close.saturating_add(2))
        .collect()
}

impl From<CommentFree> for String {
    fn from(value: CommentFree) -> Self {
        value.0
    }
}

#[cfg(test)]
#[path = "comments_test.rs"]
mod tests;
