//! Server-side syntax highlighting for a source file shown whole: a registry script, a `.base` definition, a
//! configuration file. A small lexical scanner per language, not a parser: it marks comments, strings, numbers,
//! keywords, and (for the shells) variables, which is what makes a script readable at a glance, and leaves everything
//! else as plain escaped text. Every line is wrapped in its own element so the stylesheet can number lines, and a
//! token that runs over several lines (a block comment, a triple-quoted string) is closed and reopened at each line
//! break so the markup stays well nested.

use std::fmt::Write as _;

use crate::rendering::escape_html;

/// A language this highlighter knows, chosen from a file's extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Language {
    Python,
    Shell,
    PowerShell,
    JavaScript,
    Rust,
    Toml,
    Yaml,
    Json,
    Plain,
}

impl Language {
    #[must_use]
    pub fn for_file_name(name: &str) -> Self {
        let extension = name
            .rsplit_once('.')
            .filter(|(stem, _)| !stem.is_empty())
            .map(|(_, extension)| extension.to_ascii_lowercase());
        match extension.as_deref() {
            Some("py" | "pyw") => Self::Python,
            Some("sh" | "bash" | "zsh") => Self::Shell,
            Some("ps1" | "psm1" | "psd1") => Self::PowerShell,
            Some("js" | "mjs" | "cjs" | "ts" | "mts" | "jsx" | "tsx") => Self::JavaScript,
            Some("rs") => Self::Rust,
            Some("toml") => Self::Toml,
            Some("yml" | "yaml" | "base") => Self::Yaml,
            Some("json" | "canvas") => Self::Json,
            _ => Self::Plain,
        }
    }

    /// The name shown beside the code.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Python => "Python",
            Self::Shell => "Shell",
            Self::PowerShell => "PowerShell",
            Self::JavaScript => "JavaScript",
            Self::Rust => "Rust",
            Self::Toml => "TOML",
            Self::Yaml => "YAML",
            Self::Json => "JSON",
            Self::Plain => "Plain text",
        }
    }

    fn spec(self) -> Option<&'static Spec> {
        match self {
            Self::Python => Some(&PYTHON),
            Self::Shell => Some(&SHELL),
            Self::PowerShell => Some(&POWERSHELL),
            Self::JavaScript => Some(&JAVASCRIPT),
            Self::Rust => Some(&RUST),
            Self::Toml => Some(&TOML),
            Self::Yaml => Some(&YAML),
            Self::Json => Some(&JSON),
            Self::Plain => None,
        }
    }
}

/// One language's lexical rules.
struct Spec {
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    /// String openers, longest first, each closed by the same text.
    strings: &'static [&'static str],
    /// Whether a backslash escapes the next character inside a string opened by `"`. Single-quoted shell and
    /// PowerShell strings are literal, so they never escape.
    escapes_in_single_quotes: bool,
    keywords: &'static [&'static str],
    keywords_ignore_case: bool,
    /// Whether `$name` is a variable.
    dollar_variables: bool,
}

const PYTHON: Spec = Spec {
    line_comments: &["#"],
    block_comment: None,
    strings: &["\"\"\"", "'''", "\"", "'"],
    escapes_in_single_quotes: true,
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except",
        "False", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "None", "nonlocal", "not",
        "or", "pass", "raise", "return", "True", "try", "while", "with", "yield",
    ],
    keywords_ignore_case: false,
    dollar_variables: false,
};

const SHELL: Spec = Spec {
    line_comments: &["#"],
    block_comment: None,
    strings: &["\"", "'"],
    escapes_in_single_quotes: false,
    keywords: &[
        "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in", "local", "return",
        "select", "then", "until", "while",
    ],
    keywords_ignore_case: false,
    dollar_variables: true,
};

const POWERSHELL: Spec = Spec {
    line_comments: &["#"],
    block_comment: Some(("<#", "#>")),
    strings: &["\"", "'"],
    escapes_in_single_quotes: false,
    keywords: &[
        "begin", "break", "catch", "class", "continue", "do", "else", "elseif", "end", "exit", "filter", "finally",
        "for", "foreach", "function", "if", "in", "param", "process", "return", "switch", "throw", "trap", "try",
        "until", "while",
    ],
    keywords_ignore_case: true,
    dollar_variables: true,
};

const JAVASCRIPT: Spec = Spec {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    strings: &["\"", "'", "`"],
    escapes_in_single_quotes: true,
    keywords: &[
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "delete",
        "do",
        "else",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "null",
        "of",
        "return",
        "static",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "type",
        "typeof",
        "undefined",
        "var",
        "void",
        "while",
        "yield",
    ],
    keywords_ignore_case: false,
    dollar_variables: false,
};

const RUST: Spec = Spec {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    strings: &["\""],
    escapes_in_single_quotes: true,
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "else", "enum", "false", "fn", "for", "if",
        "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static",
        "struct", "super", "trait", "true", "type", "use", "where", "while",
    ],
    keywords_ignore_case: false,
    dollar_variables: false,
};

const TOML: Spec = Spec {
    line_comments: &["#"],
    block_comment: None,
    strings: &["\"\"\"", "'''", "\"", "'"],
    escapes_in_single_quotes: false,
    keywords: &["true", "false"],
    keywords_ignore_case: false,
    dollar_variables: false,
};

const YAML: Spec = Spec {
    line_comments: &["#"],
    block_comment: None,
    strings: &["\"", "'"],
    escapes_in_single_quotes: false,
    keywords: &["true", "false", "null"],
    keywords_ignore_case: false,
    dollar_variables: false,
};

const JSON: Spec = Spec {
    line_comments: &[],
    block_comment: None,
    strings: &["\""],
    escapes_in_single_quotes: true,
    keywords: &["true", "false", "null"],
    keywords_ignore_case: false,
    dollar_variables: false,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Comment,
    String,
    Number,
    Keyword,
    Variable,
}

impl Kind {
    const fn class(self) -> Option<&'static str> {
        match self {
            Self::Text => None,
            Self::Comment => Some("tok-comment"),
            Self::String => Some("tok-string"),
            Self::Number => Some("tok-number"),
            Self::Keyword => Some("tok-keyword"),
            Self::Variable => Some("tok-variable"),
        }
    }
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// The byte length of the string starting at `rest` (which begins with `opener`), up to and including its close, or
/// the rest of the source when it is never closed.
fn string_length(rest: &str, opener: &str, escapes: bool) -> usize {
    let mut index = opener.len();
    let single_line = opener.len() == 1 && opener != "`";
    while index < rest.len() {
        let tail = &rest[index..];
        if tail.starts_with(opener) {
            return index + opener.len();
        }
        let Some(ch) = tail.chars().next() else {
            break;
        };
        if single_line && ch == '\n' {
            return index;
        }
        if escapes && ch == '\\' {
            index += 1;
            if let Some(escaped) = rest[index..].chars().next() {
                index += escaped.len_utf8();
            }
            continue;
        }
        index += ch.len_utf8();
    }
    rest.len()
}

fn tokenise<'a>(source: &'a str, spec: &Spec) -> Vec<(Kind, &'a str)> {
    let mut tokens: Vec<(Kind, &'a str)> = Vec::new();
    let mut index = 0;
    let mut previous: Option<char> = None;
    while index < source.len() {
        let rest = &source[index..];
        let at_word_start = previous.is_none_or(|ch| !is_word_char(ch));
        let (kind, length) = if let Some(length) = spec
            .line_comments
            .iter()
            .find(|marker| rest.starts_with(**marker))
            .map(|_| rest.find('\n').unwrap_or(rest.len()))
        {
            (Kind::Comment, length)
        } else if let Some((open, close)) = spec.block_comment.filter(|(open, _)| rest.starts_with(open)) {
            let end = rest[open.len()..]
                .find(close)
                .map_or(rest.len(), |end| open.len() + end + close.len());
            (Kind::Comment, end)
        } else if let Some(opener) = spec.strings.iter().find(|opener| rest.starts_with(**opener)) {
            let escapes = spec.escapes_in_single_quotes || *opener != "'";
            (Kind::String, string_length(rest, opener, escapes))
        } else if spec.dollar_variables && rest.starts_with('$') && rest[1..].starts_with(|ch: char| is_word_char(ch)) {
            let end = rest[1..]
                .find(|ch: char| !is_word_char(ch))
                .map_or(rest.len(), |end| end + 1);
            (Kind::Variable, end)
        } else if at_word_start && rest.starts_with(|ch: char| ch.is_ascii_digit()) {
            let end = rest
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_'))
                .unwrap_or(rest.len());
            (Kind::Number, end)
        } else if at_word_start && rest.starts_with(|ch: char| ch.is_alphabetic() || ch == '_') {
            let end = rest.find(|ch: char| !is_word_char(ch)).unwrap_or(rest.len());
            let word = &rest[..end];
            let is_keyword = spec.keywords.iter().any(|keyword| {
                if spec.keywords_ignore_case {
                    keyword.eq_ignore_ascii_case(word)
                } else {
                    *keyword == word
                }
            });
            (if is_keyword { Kind::Keyword } else { Kind::Text }, end)
        } else {
            let length = rest.chars().next().map_or(1, char::len_utf8);
            (Kind::Text, length)
        };
        let length = length.max(1).min(rest.len());
        let text = &rest[..length];
        previous = text.chars().last();
        match tokens.last_mut() {
            Some((Kind::Text, last)) if kind == Kind::Text => {
                let start = index - last.len();
                *last = &source[start..index + length];
            }
            _ => tokens.push((kind, text)),
        }
        index += length;
    }
    tokens
}

/// Renders `source` as escaped, line-wrapped HTML, marking tokens for `language`.
#[must_use]
pub fn highlight(source: &str, language: Language) -> String {
    let tokens = match language.spec() {
        Some(spec) => tokenise(source, spec),
        None => vec![(Kind::Text, source)],
    };
    let mut lines: Vec<String> = vec![String::new()];
    for (kind, text) in tokens {
        for (part_index, part) in text.split('\n').enumerate() {
            if part_index > 0 {
                lines.push(String::new());
            }
            if part.is_empty() {
                continue;
            }
            if let Some(line) = lines.last_mut() {
                match kind.class() {
                    Some(class) => {
                        // `write!` to a `String` cannot fail.
                        let _ = write!(line, "<span class=\"{class}\">{}</span>", escape_html(part));
                    }
                    None => line.push_str(&escape_html(part)),
                }
            }
        }
    }
    if source.ends_with('\n') {
        lines.pop();
    }
    lines
        .iter()
        .map(|line| format!("<span class=\"code-line\">{line}</span>"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "code_test.rs"]
mod tests;
