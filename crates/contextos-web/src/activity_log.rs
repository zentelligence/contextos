//! Reading the vault's two append-only activity records for display: the operation log written by `contextos-mcp`
//! (`memory/log/YYYY/MM/YYYY-MM-DD.md`, one line per operation) and the session summaries written at the end of a
//! working session (`memory/sessions/YYYY/MM/YYYY-MM-DD-HHmm-slug.md`).
//!
//! An operation log line is `HH:MM[:SS] | origin | operation | description | files: a, b`, with `\|` standing for a
//! literal bar inside a field. A line that does not have that shape is kept as written rather than dropped or
//! guessed at, so nothing in the log is ever hidden by its viewer.

use crate::journal::month_name;

/// One operation log entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogEntry {
    pub time: String,
    pub origin: String,
    pub operation: String,
    pub description: String,
    pub files: Vec<String>,
}

/// One line of a day's operation log.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LogLine {
    Entry(LogEntry),
    Heading(String),
    Blank,
    /// A line that is not an entry, kept exactly as written.
    Other(String),
}

fn is_time(value: &str) -> bool {
    let parts: Vec<&str> = value.split(':').collect();
    let in_range = |part: &str, limit: u8| part.len() == 2 && part.parse::<u8>().is_ok_and(|number| number < limit);
    matches!(parts.as_slice(), [hours, minutes] if in_range(hours, 24) && in_range(minutes, 60))
        || matches!(parts.as_slice(), [hours, minutes, seconds]
            if in_range(hours, 24) && in_range(minutes, 60) && in_range(seconds, 60))
}

/// Splits `line` at every ` | ` that is not written as `\|`, unescaping `\|` inside each field.
fn split_fields(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut rest = line;
    while let Some(next) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix("\\|") {
            if let Some(field) = fields.last_mut() {
                field.push('|');
            }
            rest = after;
        } else if let Some(after) = rest.strip_prefix(" | ") {
            fields.push(String::new());
            rest = after;
        } else {
            if let Some(field) = fields.last_mut() {
                field.push(next);
            }
            rest = &rest[next.len_utf8()..];
        }
    }
    fields.into_iter().map(|field| field.trim().to_owned()).collect()
}

fn file_list(raw: &str) -> Vec<String> {
    raw.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(str::trim)
        .filter(|file| !file.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Reads one line of an operation log.
#[must_use]
pub fn parse_line(line: &str) -> LogLine {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return LogLine::Blank;
    }
    if let Some(heading) = trimmed.strip_prefix('#') {
        return LogLine::Heading(heading.trim_start_matches('#').trim().to_owned());
    }
    let mut fields = split_fields(trimmed);
    if fields.len() < 4 || !is_time(&fields[0]) {
        return LogLine::Other(trimmed.to_owned());
    }
    let files = match fields.last().and_then(|last| last.strip_prefix("files:")) {
        Some(list) if fields.len() >= 5 => {
            let files = file_list(list);
            fields.pop();
            files
        }
        _ => Vec::new(),
    };
    let description = fields[3..].join(" | ");
    LogLine::Entry(LogEntry {
        time: fields[0].clone(),
        origin: fields[1].clone(),
        operation: fields[2].clone(),
        description,
        files,
    })
}

/// A day's operation log, line by line.
#[derive(Clone, Debug, Default)]
pub struct LogDay {
    pub lines: Vec<LogLine>,
}

/// Reads a whole day's operation log.
#[must_use]
pub fn parse_day(source: &str) -> LogDay {
    LogDay {
        lines: source.lines().map(parse_line).collect(),
    }
}

impl LogDay {
    pub fn entries(&self) -> impl Iterator<Item = &LogEntry> {
        self.lines.iter().filter_map(|line| match line {
            LogLine::Entry(entry) => Some(entry),
            LogLine::Heading(_) | LogLine::Blank | LogLine::Other(_) => None,
        })
    }

    /// How many distinct files the day's entries touched.
    #[must_use]
    pub fn distinct_files(&self) -> usize {
        let mut files: Vec<&str> = self
            .entries()
            .flat_map(|entry| entry.files.iter().map(String::as_str))
            .collect();
        files.sort_unstable();
        files.dedup();
        files.len()
    }

    /// Each operation and how many entries performed it, most frequent first.
    #[must_use]
    pub fn operation_counts(&self) -> Vec<(String, usize)> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for entry in self.entries() {
            match counts.iter_mut().find(|(operation, _)| *operation == entry.operation) {
                Some((_, count)) => *count += 1,
                None => counts.push((entry.operation.clone(), 1)),
            }
        }
        counts.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        counts
    }
}

/// A session summary's file name read as when it happened and what it was about.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionLabel {
    pub date: Option<String>,
    pub time: Option<String>,
    pub title: String,
}

fn is_digits(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_digit())
}

/// Reads `YYYY-MM-DD[-HHmm]-slug.md` into its date, time, and a readable title.
#[must_use]
pub fn session_label(file_name: &str) -> SessionLabel {
    let stem = file_name.strip_suffix(".md").unwrap_or(file_name);
    let parts: Vec<&str> = stem.splitn(5, '-').collect();
    let dated = parts.len() >= 4 && is_digits(parts[0], 4) && is_digits(parts[1], 2) && is_digits(parts[2], 2);
    let (date, time, slug) = if dated {
        let date = format!("{}-{}-{}", parts[0], parts[1], parts[2]);
        let rest: Vec<&str> = parts[3..].to_vec();
        match rest.split_first() {
            Some((clock, tail)) if is_digits(clock, 4) && !tail.is_empty() => (
                Some(date),
                Some(format!("{}:{}", &clock[..2], &clock[2..])),
                tail.join("-"),
            ),
            _ => (Some(date), None, rest.join("-")),
        }
    } else {
        (None, None, stem.to_owned())
    };
    let words = slug.replace(['-', '_'], " ");
    let mut chars = words.trim().chars();
    let title = chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    SessionLabel { date, time, title }
}

/// A date-partition folder's label: a two-digit month beneath a four-digit year reads as the month's name.
#[must_use]
pub fn folder_label(name: &str, parent: Option<&str>) -> String {
    let month = name
        .parse::<u8>()
        .ok()
        .filter(|_| is_digits(name, 2) && parent.is_some_and(|parent| is_digits(parent, 4)))
        .and_then(|number| time::Month::try_from(number).ok());
    month.map_or_else(|| name.to_owned(), |month| month_name(month).to_owned())
}

#[cfg(test)]
#[path = "activity_log_test.rs"]
mod tests;
