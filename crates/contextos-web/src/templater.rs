//! Templater-subset substitution shared by every screen that seeds a
//! brand-new file from a vault-configured, Obsidian Templater-plugin
//! compatible template: only the small set of `tp.date.*` expressions the
//! shipped templates (`templates/daily-journal.md`, `templates/unique-note.md`)
//! actually use, resolved against a real instant with no Templater plugin
//! runtime available server-side. An expression this subset does not
//! recognise is a rejection, never a silent strip: a template this crate
//! cannot fully resolve is surfaced as an actionable error rather than
//! shipping a brand-new file with literal `<% ... %>` text left in it.
//!
//! `journal_template` wraps this module for its own `Date`-only callers
//! (a day's own entry never needs a time-of-day token); `notes_template`
//! calls it directly against a full `OffsetDateTime`, since a unique note's
//! own template (`time: <% tp.date.now("HH:mm") %>`) does.

use time::{Date, Duration, Month, OffsetDateTime, Weekday};

use crate::journal::{month_name, ordinal, weekday_name};

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TemplaterError {
    #[error(
        "unsupported Templater expression `<% {0} %>`; adjust the template, or ask for this expression to be supported"
    )]
    UnsupportedExpression(String),
}

/// Every Templater format token this subset understands, checked longest
/// (and most specific) first so e.g. `MMMM` is never mistaken for two `MM`
/// tokens. Any character this loop does not match is copied through
/// literally, matching moment.js-style format strings (`"-"`, `", "`, `:`
/// and so on need no escaping). `HH`/`mm` (time-of-day, always
/// zero-padded 24-hour/minute) exist only for callers that render against a
/// real instant rather than a bare calendar date; a `Date`-only caller
/// simply never has a template that uses them.
const TOKENS: &[&str] = &["YYYY", "MMMM", "dddd", "Do", "DD", "MM", "HH", "mm", "ww"];

fn token_value(token: &str, at: OffsetDateTime) -> String {
    let date = at.date();
    match token {
        "YYYY" => format!("{:04}", date.year()),
        "MMMM" => month_name(date.month()).to_owned(),
        "dddd" => weekday_name(date.weekday()).to_owned(),
        "Do" => ordinal(date.day()),
        "DD" => format!("{:02}", date.day()),
        "MM" => format!("{:02}", u8::from(date.month())),
        "HH" => format!("{:02}", at.hour()),
        "mm" => format!("{:02}", at.minute()),
        "ww" => format!("{:02}", date.iso_week()),
        _ => String::new(),
    }
}

/// Renders one Templater date-format string (the quoted argument to
/// `tp.date.now`/`tp.date.yesterday`/`tp.date.tomorrow`) against `at`.
#[must_use]
pub(crate) fn format_moment_like(at: OffsetDateTime, format: &str) -> String {
    let mut out = String::with_capacity(format.len());
    let mut rest = format;
    'outer: while !rest.is_empty() {
        for token in TOKENS {
            if let Some(after) = rest.strip_prefix(token) {
                out.push_str(&token_value(token, at));
                rest = after;
                continue 'outer;
            }
        }
        let mut chars = rest.chars();
        if let Some(next) = chars.next() {
            out.push(next);
        }
        rest = chars.as_str();
    }
    out
}

/// The first `width` characters of `candidate`, as long as every one of
/// them is an ASCII digit: `(value, width)`, or `None` when `candidate` is
/// shorter than `width` or contains a non-digit within it. Never accepts
/// more or fewer digits than `width` - a fixed-width token (`YYYY`, `MM`,
/// and so on) must match exactly that many digits, never a shorter or
/// longer run that happens to parse.
fn parse_fixed_digits(candidate: &str, width: usize) -> Option<(i64, usize)> {
    let prefix: String = candidate.chars().take(width).collect();
    if prefix.chars().count() != width || !prefix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    prefix.parse::<i64>().ok().map(|value| (value, prefix.len()))
}

const WEEKDAYS_MONDAY_FIRST: [Weekday; 7] = [
    Weekday::Monday,
    Weekday::Tuesday,
    Weekday::Wednesday,
    Weekday::Thursday,
    Weekday::Friday,
    Weekday::Saturday,
    Weekday::Sunday,
];

/// The longest known month or weekday name `candidate` starts with, from
/// `names` (index-1-based, matching `token_value`'s own 1-12/1-7
/// convention): `(index, matched byte length)`. Longest-first so a locale
/// where one name is a prefix of another is never mismatched.
fn parse_name<'a>(candidate: &str, names: impl Iterator<Item = (i64, &'a str)>) -> Option<(i64, usize)> {
    let mut names: Vec<(i64, &str)> = names.collect();
    names.sort_by_key(|(_, name)| std::cmp::Reverse(name.len()));
    names
        .into_iter()
        .find(|(_, name)| candidate.starts_with(name))
        .map(|(index, name)| (index, name.len()))
}

fn parse_month_name(candidate: &str) -> Option<(i64, usize)> {
    parse_name(
        candidate,
        (1..=12_u8).filter_map(|month| {
            Month::try_from(month)
                .ok()
                .map(|month| (i64::from(u8::from(month)), month_name(month)))
        }),
    )
}

fn parse_weekday_name(candidate: &str) -> Option<(i64, usize)> {
    parse_name(
        candidate,
        WEEKDAYS_MONDAY_FIRST
            .iter()
            .enumerate()
            .map(|(index, weekday)| (i64::try_from(index).unwrap_or(0) + 1, weekday_name(*weekday))),
    )
}

/// A day-with-ordinal-suffix (`"14th"`, `"1st"`): its own leading 1-2-digit
/// day number, plus any one of the four English ordinal suffixes -
/// deliberately not cross-checked against which suffix `ordinal()` would
/// itself produce for that exact number, since a parser's job here is
/// recognising the shape, not re-validating a value [`format_moment_like`]
/// already produced correctly by construction.
fn parse_ordinal_day(candidate: &str) -> Option<(i64, usize)> {
    let digits: String = candidate.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 2 {
        return None;
    }
    let day: i64 = digits.parse().ok()?;
    let after_digits = &candidate[digits.len()..];
    let suffix = ["st", "nd", "rd", "th"]
        .into_iter()
        .find(|suffix| after_digits.starts_with(suffix))?;
    Some((day, digits.len() + suffix.len()))
}

fn parse_token(token: &str, candidate: &str) -> Option<(i64, usize)> {
    match token {
        "YYYY" => parse_fixed_digits(candidate, 4),
        "MM" => parse_fixed_digits(candidate, 2).filter(|(value, _)| (1..=12).contains(value)),
        "DD" => parse_fixed_digits(candidate, 2).filter(|(value, _)| (1..=31).contains(value)),
        "HH" => parse_fixed_digits(candidate, 2).filter(|(value, _)| (0..=23).contains(value)),
        "mm" => parse_fixed_digits(candidate, 2).filter(|(value, _)| (0..=59).contains(value)),
        "ww" => parse_fixed_digits(candidate, 2).filter(|(value, _)| (0..=53).contains(value)),
        "MMMM" => parse_month_name(candidate),
        "dddd" => parse_weekday_name(candidate),
        "Do" => parse_ordinal_day(candidate),
        _ => None,
    }
}

/// One matched token's own name (`"YYYY"`, `"MM"`, and so on - one of
/// [`TOKENS`]) paired with the numeric value [`parse_token`] extracted for
/// it, in the order the format string names them.
pub(crate) type MatchedToken = (&'static str, i64);

/// The inverse of [`format_moment_like`]: matches `candidate` against
/// `format` token-by-token (every literal character - a `-`, a `/`, a
/// space - must match exactly), returning each matched token's own name
/// and extracted value, or `None` the moment either side cannot continue
/// (a token whose expected shape is not there, a literal mismatch, or
/// leftover characters on either side once the other is exhausted).
///
/// The returned order lets a caller either use it directly as a sort key
/// (values in format order, ignoring which token produced which - correct
/// as long as every candidate compared this way was matched against the
/// identical `format`) or look up one field by name
/// ([`matched_field`]) when it needs a specific calendar value regardless
/// of where the configured format places it.
#[must_use]
pub(crate) fn parse_moment_like(format: &str, candidate: &str) -> Option<Vec<MatchedToken>> {
    let mut matched = Vec::new();
    let mut format_rest = format;
    let mut candidate_rest = candidate;
    'outer: while !format_rest.is_empty() {
        for token in TOKENS {
            if let Some(after_format) = format_rest.strip_prefix(token) {
                let (value, consumed) = parse_token(token, candidate_rest)?;
                matched.push((*token, value));
                format_rest = after_format;
                candidate_rest = &candidate_rest[consumed..];
                continue 'outer;
            }
        }
        let mut format_chars = format_rest.chars();
        let literal = format_chars.next()?;
        let mut candidate_chars = candidate_rest.chars();
        if candidate_chars.next()? != literal {
            return None;
        }
        format_rest = format_chars.as_str();
        candidate_rest = candidate_chars.as_str();
    }
    candidate_rest.is_empty().then_some(matched)
}

/// The value [`parse_moment_like`] extracted for `token` (`"YYYY"`,
/// `"MM"`, `"DD"`, and so on), whichever position in `matched` it actually
/// occupies - a configured format may name its tokens in any order.
#[must_use]
pub(crate) fn matched_field(matched: &[MatchedToken], token: &str) -> Option<i64> {
    matched.iter().find(|(name, _)| *name == token).map(|(_, value)| *value)
}

/// Extracts a `function("...")` call's own quoted argument, `None` when
/// `expr` does not call `function` at all (a different `tp.*` expression,
/// or malformed syntax).
fn extract_call_arg<'a>(expr: &'a str, function: &str) -> Option<&'a str> {
    let rest = expr.strip_prefix(function)?.trim_start();
    let inner = rest.strip_prefix('(')?.strip_suffix(')')?;
    Some(inner.trim().trim_matches(['"', '\'']))
}

fn resolve_expression(expr: &str, at: OffsetDateTime) -> Result<String, TemplaterError> {
    if let Some(format) = extract_call_arg(expr, "tp.date.now") {
        return Ok(format_moment_like(at, format));
    }
    if let Some(format) = extract_call_arg(expr, "tp.date.yesterday") {
        let shifted = at.checked_sub(Duration::days(1)).unwrap_or(at);
        return Ok(format_moment_like(shifted, format));
    }
    if let Some(format) = extract_call_arg(expr, "tp.date.tomorrow") {
        let shifted = at.checked_add(Duration::days(1)).unwrap_or(at);
        return Ok(format_moment_like(shifted, format));
    }
    Err(TemplaterError::UnsupportedExpression(expr.to_owned()))
}

/// Resolves every `<% ... %>` Templater expression in `source` against
/// `at`.
///
/// # Errors
///
/// Returns [`TemplaterError::UnsupportedExpression`] on the first
/// expression this subset does not recognise.
pub fn render_templater_expressions(source: &str, at: OffsetDateTime) -> Result<String, TemplaterError> {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("<%") {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find("%>") else {
            // An unterminated tag is copied through verbatim rather than
            // looping forever looking for a close that will never come.
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let expr = after_open[..end].trim();
        out.push_str(&resolve_expression(expr, at)?);
        rest = &after_open[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// [`render_templater_expressions`] against a bare calendar `date`, at
/// midnight: the shape [`crate::journal_template`]'s own `Date`-only
/// callers need, expressed once here rather than duplicated at every call
/// site that has no real time-of-day to render against.
#[must_use]
pub(crate) fn midnight(date: Date) -> OffsetDateTime {
    date.midnight().assume_utc()
}

#[cfg(test)]
#[path = "templater_test.rs"]
mod tests;
