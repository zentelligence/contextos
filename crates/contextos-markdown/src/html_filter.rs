//! The allowlist author HTML passes through.
//!
//! Nothing an author writes reaches the page as written. The input is tokenised, and the output is rebuilt from the
//! tokens: an allowlisted element with its allowlisted attributes, values decoded and escaped again, and everything
//! else as escaped text. A token this filter misreads therefore shows as text; it can never become markup. A tag the
//! input leaves unterminated is dropped, as an HTML tokeniser drops a tag cut off by the end of the document.

use crate::escape::{escape_attribute, escape_text};

/// HTML an author wrote in a note.
pub(crate) struct AuthorHtml<'a>(pub(crate) &'a str);

/// Author HTML rebuilt from the allowlist.
pub(crate) struct SafeHtml(String);

/// The elements an author may use: those Obsidian renders that cannot run script, load a page, or submit anything.
pub(crate) const ALLOWED_ELEMENTS: &[&str] = &[
    "a",
    "abbr",
    "b",
    "bdi",
    "bdo",
    "blockquote",
    "br",
    "caption",
    "center",
    "cite",
    "code",
    "col",
    "colgroup",
    "dd",
    "del",
    "details",
    "dfn",
    "div",
    "dl",
    "dt",
    "em",
    "figcaption",
    "figure",
    "font",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "i",
    "img",
    "ins",
    "kbd",
    "li",
    "mark",
    "ol",
    "p",
    "pre",
    "q",
    "rp",
    "rt",
    "ruby",
    "s",
    "samp",
    "small",
    "span",
    "strong",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "time",
    "tr",
    "u",
    "ul",
    "var",
    "wbr",
];

/// Elements that have no end tag.
const VOID_ELEMENTS: &[&str] = &["br", "col", "hr", "img", "wbr"];

/// Elements whose content is raw text up to their end tag, removed together with that text.
const RAW_TEXT_ELEMENTS: &[&str] = &[
    "script",
    "style",
    "textarea",
    "title",
    "xmp",
    "iframe",
    "noembed",
    "noframes",
    "noscript",
    "plaintext",
];

/// Elements removed together with all the markup inside them.
const REMOVED_WITH_CONTENT: &[&str] = &["svg", "math", "template", "object", "frameset", "head", "select"];

/// Attributes any allowed element may carry.
const GLOBAL_ATTRIBUTES: &[&str] = &["title", "lang", "dir", "class", "style"];

impl From<AuthorHtml<'_>> for SafeHtml {
    fn from(html: AuthorHtml<'_>) -> Self {
        let source = html.0;
        let mut writer = Writer::default();
        let mut at = 0;
        while let Some(rest) = source.get(at..).filter(|rest| !rest.is_empty()) {
            if !rest.starts_with('<') {
                let end = rest.find('<').unwrap_or(rest.len());
                writer.text(rest.get(..end).unwrap_or_default());
                at = at.saturating_add(end);
                continue;
            }
            if let Some(comment) = rest.strip_prefix("<!--") {
                at = at.saturating_add(comment.find("-->").map_or(rest.len(), |end| end.saturating_add(7)));
                continue;
            }
            if rest.starts_with("<!") || rest.starts_with("<?") {
                at = at.saturating_add(rest.find('>').map_or(rest.len(), |end| end.saturating_add(1)));
                continue;
            }
            match Token::from(rest) {
                Token::Text => {
                    writer.text("<");
                    at = at.saturating_add(1);
                }
                Token::Unterminated => at = source.len(),
                Token::End { name, length } => {
                    writer.end(&name);
                    at = at.saturating_add(length);
                }
                Token::Start {
                    name,
                    attributes,
                    length,
                } => {
                    at = at.saturating_add(length);
                    if RAW_TEXT_ELEMENTS.contains(&name.as_str()) {
                        // Everything up to the matching end tag is text that belongs to the removed element.
                        let content = source.get(at..).unwrap_or_default();
                        at = at.saturating_add(raw_text_length(content, &name));
                    } else {
                        writer.start(&name, attributes);
                    }
                }
            }
        }
        Self(writer.out)
    }
}

/// The name of the element `html`, a single inline tag, opens when that element is removed together with its content.
pub(crate) fn removed_element(html: &str) -> Option<String> {
    match Token::from(html) {
        Token::Start { name, .. }
            if RAW_TEXT_ELEMENTS.contains(&name.as_str()) || REMOVED_WITH_CONTENT.contains(&name.as_str()) =>
        {
            Some(name)
        }
        _ => None,
    }
}

/// Whether `event` is the inline end tag of `name`.
pub(crate) fn closes(event: &pulldown_cmark::Event<'_>, name: &str) -> bool {
    matches!(event, pulldown_cmark::Event::InlineHtml(html)
        if matches!(Token::from(html.as_ref()), Token::End { name: closing, .. } if closing == name))
}

impl From<SafeHtml> for String {
    fn from(value: SafeHtml) -> Self {
        value.0
    }
}

/// One token at the start of the remaining input, which begins with `<`.
enum Token {
    Start {
        name: String,
        attributes: Vec<(String, Option<String>)>,
        length: usize,
    },
    End {
        name: String,
        length: usize,
    },
    /// The `<` does not begin a tag.
    Text,
    /// A tag the input ends inside.
    Unterminated,
}

impl From<&str> for Token {
    fn from(rest: &str) -> Self {
        let mut cursor = Cursor { text: rest, at: 1 };
        let closing = cursor.eat('/');
        if !cursor.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            return Self::Text;
        }
        let name = cursor
            .take_while(|c| !c.is_ascii_whitespace() && c != '/' && c != '>')
            .to_ascii_lowercase();
        if closing {
            return match cursor.text.get(cursor.at..).and_then(|tail| tail.find('>')) {
                Some(end) => Self::End {
                    name,
                    length: cursor.at.saturating_add(end).saturating_add(1),
                },
                None => Self::Unterminated,
            };
        }
        let mut attributes = Vec::new();
        loop {
            cursor.take_while(|c| c.is_ascii_whitespace() || c == '/');
            match cursor.peek() {
                None => return Self::Unterminated,
                Some('>') => {
                    return Self::Start {
                        name,
                        attributes,
                        length: cursor.at.saturating_add(1),
                    };
                }
                Some(_) => {}
            }
            let first = cursor.next_char().unwrap_or_default();
            let mut key = String::from(first);
            key.push_str(&cursor.take_while(|c| !c.is_ascii_whitespace() && c != '/' && c != '>' && c != '='));
            cursor.take_while(|c| c.is_ascii_whitespace());
            let value = if cursor.eat('=') {
                cursor.take_while(|c| c.is_ascii_whitespace());
                match cursor.peek() {
                    None => return Self::Unterminated,
                    Some(quote @ ('"' | '\'')) => {
                        cursor.next_char();
                        let value = cursor.take_while(|c| c != quote);
                        if !cursor.eat(quote) {
                            return Self::Unterminated;
                        }
                        Some(value)
                    }
                    Some(_) => Some(cursor.take_while(|c| !c.is_ascii_whitespace() && c != '>')),
                }
            } else {
                None
            };
            attributes.push((key.to_ascii_lowercase(), value));
        }
    }
}

struct Cursor<'a> {
    text: &'a str,
    at: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<char> {
        self.text.get(self.at..).and_then(|rest| rest.chars().next())
    }

    fn next_char(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at = self.at.saturating_add(c.len_utf8());
        Some(c)
    }

    fn eat(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.at = self.at.saturating_add(expected.len_utf8());
            true
        } else {
            false
        }
    }

    fn take_while(&mut self, keep: impl Fn(char) -> bool) -> String {
        let mut taken = String::new();
        while let Some(c) = self.peek().filter(|c| keep(*c)) {
            taken.push(c);
            self.at = self.at.saturating_add(c.len_utf8());
        }
        taken
    }
}

/// The bytes of raw text before the end tag of `name` in `content`, or all of it when the tag never ends.
fn raw_text_length(content: &str, name: &str) -> usize {
    let lower = content.to_ascii_lowercase();
    let closing = format!("</{name}");
    let mut from = 0;
    while let Some(found) = lower.get(from..).and_then(|rest| rest.find(&closing)) {
        let at = from.saturating_add(found);
        let after = lower
            .get(at.saturating_add(closing.len())..)
            .and_then(|rest| rest.chars().next());
        if after.is_none_or(|c| c.is_ascii_whitespace() || c == '/' || c == '>') {
            return at;
        }
        from = at.saturating_add(1);
    }
    content.len()
}

/// An element being removed with its content, and how deeply it is nested in itself.
struct Removing {
    name: String,
    depth: usize,
}

#[derive(Default)]
struct Writer {
    out: String,
    removing: Option<Removing>,
}

impl Writer {
    fn text(&mut self, text: &str) {
        if self.removing.is_none() {
            self.out.push_str(&escape_text_keeping_references(text));
        }
    }

    fn start(&mut self, name: &str, attributes: Vec<(String, Option<String>)>) {
        if let Some(removing) = &mut self.removing {
            if removing.name == name {
                removing.depth = removing.depth.saturating_add(1);
            }
            return;
        }
        if REMOVED_WITH_CONTENT.contains(&name) {
            self.removing = Some(Removing {
                name: name.to_owned(),
                depth: 1,
            });
            return;
        }
        if !ALLOWED_ELEMENTS.contains(&name) {
            return;
        }
        self.out.push('<');
        self.out.push_str(name);
        let mut seen: Vec<String> = Vec::new();
        for (key, value) in attributes {
            if seen.contains(&key) {
                continue;
            }
            seen.push(key.clone());
            if let Some(kept) = kept_attribute(name, &key, value.as_deref()) {
                self.out.push_str(&kept);
            }
        }
        if name == "a" {
            self.out.push_str(" rel=\"noopener noreferrer\"");
        }
        self.out.push('>');
    }

    fn end(&mut self, name: &str) {
        if let Some(removing) = &mut self.removing {
            if removing.name == name {
                removing.depth = removing.depth.saturating_sub(1);
                if removing.depth == 0 {
                    self.removing = None;
                }
            }
            return;
        }
        if ALLOWED_ELEMENTS.contains(&name) && !VOID_ELEMENTS.contains(&name) {
            self.out.push_str("</");
            self.out.push_str(name);
            self.out.push('>');
        }
    }
}

/// The attribute as written to the page, or `None` when it is not allowed on `element` or its value does not pass.
fn kept_attribute(element: &str, key: &str, raw: Option<&str>) -> Option<String> {
    let allowed_here = match element {
        "a" => ["href"].as_slice(),
        "img" => &["src", "alt", "width", "height"],
        "td" | "th" => &["colspan", "rowspan", "align"],
        "col" | "colgroup" => &["span"],
        "ol" => &["start", "type", "reversed"],
        "li" => &["value"],
        "details" => &["open"],
        "time" => &["datetime"],
        "q" | "blockquote" | "del" | "ins" => &["cite"],
        "font" => &["color"],
        "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => &["align"],
        _ => &[],
    };
    if !GLOBAL_ATTRIBUTES.contains(&key) && !allowed_here.contains(&key) {
        return None;
    }
    if matches!(key, "open" | "reversed") {
        return Some(format!(" {key}"));
    }
    let value = decoded(raw?);
    let passes = match key {
        "href" | "cite" => is_link_address(&value),
        "src" => is_image_address(&value),
        "width" | "height" | "colspan" | "rowspan" | "span" | "start" | "value" => {
            (1..=5).contains(&value.len()) && value.chars().all(|c| c.is_ascii_digit())
        }
        "type" => matches!(value.as_str(), "1" | "a" | "A" | "i" | "I"),
        "align" => matches!(value.as_str(), "left" | "right" | "center" | "justify"),
        "datetime" => value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '+' | '-' | '.' | ' ')),
        "color" => value.chars().all(|c| c.is_ascii_alphanumeric() || c == '#'),
        "style" => is_plain_style(&value),
        _ => true,
    };
    passes.then(|| format!(" {key}=\"{}\"", escape_attribute(&value)))
}

/// `value` as a browser would read it: with whitespace and control characters, which a browser ignores in a scheme,
/// removed and lower-cased, up to its first `:`.
fn scheme_part(value: &str) -> Option<String> {
    let cleaned: String = value
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && !c.is_ascii_control())
        .collect::<String>()
        .to_ascii_lowercase();
    // A reference left undecoded could spell out part of the scheme, so a scheme containing one is never trusted.
    let scheme_end = cleaned.find(['/', '?', '#']).unwrap_or(cleaned.len());
    if cleaned.get(..scheme_end).is_some_and(|scheme| scheme.contains('&')) {
        return None;
    }
    Some(cleaned)
}

fn is_link_address(value: &str) -> bool {
    scheme_part(value).is_some_and(|cleaned| {
        cleaned.starts_with("http://")
            || cleaned.starts_with("https://")
            || cleaned.starts_with("mailto:")
            || cleaned.starts_with('#')
    })
}

fn is_image_address(value: &str) -> bool {
    scheme_part(value).is_some_and(|cleaned| {
        cleaned.starts_with("http://")
            || cleaned.starts_with("https://")
            || ["png", "jpeg", "jpg", "gif", "webp", "avif", "bmp"].iter().any(|kind| {
                cleaned
                    .strip_prefix("data:image/")
                    .and_then(|rest| rest.strip_prefix(kind))
                    .is_some_and(|rest| rest.starts_with([';', ',']))
            })
    })
}

/// Whether a `style` value only sets properties: nothing that loads, imports, runs, or hides behind an escape or a
/// comment.
fn is_plain_style(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    ![
        "url(",
        "expression",
        "behavior",
        "binding",
        "javascript",
        "@",
        "\\",
        "/*",
        "<",
        ">",
    ]
    .iter()
    .any(|danger| lower.contains(danger))
}

/// `raw`, an attribute value, with its character references replaced by the characters they stand for, as a browser
/// reads it. A numeric reference counts without its `;`, as browsers allow. A named reference other than the few
/// below is left as written.
fn decoded(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(rest.get(..at).unwrap_or_default());
        rest = rest.get(at..).unwrap_or_default();
        let (decoded, length) = reference(rest).unwrap_or(('&', 1));
        out.push(decoded);
        rest = rest.get(length..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// The character the reference at the start of `text` (which begins with `&`) stands for, and its length.
fn reference(text: &str) -> Option<(char, usize)> {
    let body = text.get(1..)?;
    if let Some(numeric) = body.strip_prefix('#') {
        let (digits, radix, prefix) = match numeric.strip_prefix(['x', 'X']) {
            Some(hex) => (hex, 16, 2),
            None => (numeric, 10, 1),
        };
        let length = digits.find(|c: char| !c.is_digit(radix)).unwrap_or(digits.len()).min(8);
        if length == 0 {
            return None;
        }
        let value = u32::from_str_radix(digits.get(..length)?, radix).ok()?;
        let semicolon = usize::from(digits.get(length..).is_some_and(|after| after.starts_with(';')));
        let c = char::from_u32(value).filter(|c| *c != '\0').unwrap_or('\u{fffd}');
        return Some((
            c,
            1_usize
                .saturating_add(prefix)
                .saturating_add(length)
                .saturating_add(semicolon),
        ));
    }
    for (name, c) in [
        ("amp;", '&'),
        ("lt;", '<'),
        ("gt;", '>'),
        ("quot;", '"'),
        ("apos;", '\''),
        ("nbsp;", '\u{a0}'),
    ] {
        if body.starts_with(name) {
            return Some((c, name.len().saturating_add(1)));
        }
    }
    None
}

/// `text` escaped for the page, keeping each well-formed character reference as written, since one cannot form
/// markup.
fn escape_text_keeping_references(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&escape_text(rest.get(..at).unwrap_or_default()));
        rest = rest.get(at..).unwrap_or_default();
        let length = well_formed_reference_length(rest);
        if length > 0 {
            out.push_str(rest.get(..length).unwrap_or_default());
            rest = rest.get(length..).unwrap_or_default();
        } else {
            out.push_str("&amp;");
            rest = rest.get(1..).unwrap_or_default();
        }
    }
    out.push_str(&escape_text(rest));
    out
}

/// The length of the reference `&name;`, `&#digits;`, or `&#xhex;` at the start of `text`, or 0 when there is none.
fn well_formed_reference_length(text: &str) -> usize {
    let body = text.get(1..).unwrap_or_default();
    let (name_length, valid) = if let Some(hex) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X")) {
        let digits = hex.chars().take_while(char::is_ascii_hexdigit).count();
        (digits.saturating_add(2), (1..=6).contains(&digits))
    } else if let Some(decimal) = body.strip_prefix('#') {
        let digits = decimal.chars().take_while(char::is_ascii_digit).count();
        (digits.saturating_add(1), (1..=7).contains(&digits))
    } else {
        let letters = body.chars().take_while(char::is_ascii_alphanumeric).count();
        (
            letters,
            (2..=32).contains(&letters) && body.starts_with(|c: char| c.is_ascii_alphabetic()),
        )
    };
    if valid && body.get(name_length..).is_some_and(|after| after.starts_with(';')) {
        name_length.saturating_add(2)
    } else {
        0
    }
}

#[cfg(test)]
#[path = "html_filter_test.rs"]
mod tests;
