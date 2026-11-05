//! Templater tokens in a template: `<% ... %>` and `{{ ... }}`, shown as written rather than read as Markdown.

/// A template body with each token, outside code and on one line, written as an inline code span, so the parser
/// treats its content as inert text and the renderer can show it as a token.
pub(crate) struct TokensAsCode(String);

impl From<&str> for TokensAsCode {
    fn from(text: &str) -> Self {
        let mut out = String::with_capacity(text.len());
        let mut in_fence = false;
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                in_fence = !in_fence;
                out.push_str(line);
            } else if in_fence {
                out.push_str(line);
            } else {
                tokens_in_line(line, &mut out);
            }
        }
        Self(out)
    }
}

/// The length of the token at the start of `rest`, when one opens there and closes before the line ends.
fn token_length(rest: &str) -> Option<usize> {
    let (open, close) = if rest.starts_with("<%") {
        ("<%", "%>")
    } else if rest.starts_with("{{") {
        ("{{", "}}")
    } else {
        return None;
    };
    let line_end = rest.find('\n').unwrap_or(rest.len());
    let end = rest.get(open.len()..line_end)?.find(close)?;
    Some(open.len().saturating_add(end).saturating_add(close.len()))
}

/// `token` as an inline code span: fenced by one more backtick than its longest run of them, and padded with a space
/// when it starts or ends with one, so the span holds exactly the token.
fn as_code_span(token: &str) -> String {
    let mut longest = 0_usize;
    let mut run = 0_usize;
    for c in token.chars() {
        run = if c == '`' { run.saturating_add(1) } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest.saturating_add(1));
    let pad = if longest > 0 { " " } else { "" };
    format!("{fence}{pad}{token}{pad}{fence}")
}

fn tokens_in_line(line: &str, out: &mut String) {
    let mut rest = line;
    let mut in_code_span = false;
    while let Some(next) = rest.chars().next() {
        if next == '`' {
            in_code_span = !in_code_span;
        } else if !in_code_span && let Some(length) = token_length(rest) {
            out.push_str(&as_code_span(rest.get(..length).unwrap_or_default()));
            rest = rest.get(length..).unwrap_or_default();
            continue;
        }
        out.push(next);
        rest = rest.get(next.len_utf8()..).unwrap_or_default();
    }
}

impl From<TokensAsCode> for String {
    fn from(value: TokensAsCode) -> Self {
        value.0
    }
}

/// Whether `code`, the text of a code span, is a template token.
pub(crate) fn is_token(code: &str) -> bool {
    (code.starts_with("<%") && code.ends_with("%>")) || (code.starts_with("{{") && code.ends_with("}}"))
}

#[cfg(test)]
#[path = "templates_test.rs"]
mod tests;
