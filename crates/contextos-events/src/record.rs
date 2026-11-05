use std::fmt::{self, Write as _};

use time::{OffsetDateTime, UtcOffset};

use crate::field::single_quoted;
use crate::{FieldName, FieldValue, KindName, Severity};

/// The longest slug in an event's file name.
const MAX_SLUG_LEN: usize = 48;

/// The note an event is about.
///
/// A record of a keyed base is referenced by its key, never its path: its link is `[[{key}|{KEY}]]`, and what
/// identifies it is its base and key together, since two bases may share a key series. Its path is kept beside them as
/// where it was when the event happened. A note that is no such record is linked and identified by its path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventSubject {
    /// The link text as written in frontmatter, such as `[[a-001|A-001]]`.
    pub link: String,
    /// The subject's own key, when it is a record of a keyed base.
    pub id: Option<String>,
    /// The subject's vault-relative path when the event happened, `.md` included.
    pub path: Option<String>,
    /// The vault-relative path of the base the subject is a record of.
    pub base: Option<String>,
}

impl EventSubject {
    /// A record of the keyed base `base`, at the vault-relative `path`, whose key is `id`: linked by its key, or by its
    /// file name labelled with its key when the file is not named for it, and never by its folder.
    #[must_use]
    pub fn for_record(path: &str, id: &str, base: &str) -> Self {
        let stem = stem_of(path);
        let target = if stem.eq_ignore_ascii_case(id) {
            id.to_lowercase()
        } else {
            stem
        };
        Self {
            link: format!("[[{target}|{id}]]"),
            id: Some(id.to_owned()),
            path: Some(path.to_owned()),
            base: Some(base.to_owned()),
        }
    }

    /// A note at the vault-relative `path` that is no record of a keyed base: linked by its path, labelled with its
    /// name.
    #[must_use]
    pub fn for_note(path: &str) -> Self {
        let target = path.strip_suffix(".md").unwrap_or(path);
        Self {
            link: format!("[[{target}|{}]]", stem_of(target)),
            id: None,
            path: Some(path.to_owned()),
            base: None,
        }
    }

    /// The text a file name uses for this subject: its id, else the name of the note it links.
    fn slug_source(&self) -> String {
        self.id.clone().unwrap_or_else(|| {
            let inner = self.link.trim_start_matches("[[").trim_end_matches("]]");
            let target = inner.split(['|', '#']).next().unwrap_or(inner);
            stem_of(target)
        })
    }
}

/// The last path segment, without its `.md` extension.
fn stem_of(target: &str) -> String {
    let name = target.rsplit('/').next().unwrap_or(target);
    name.strip_suffix(".md").unwrap_or(name).to_owned()
}

/// One event, ready to be written as a note.
///
/// The fields are the event's own, in the order the note carries them; [`fmt::Display`] renders the whole note, with
/// the common event fields first and none of the common note fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventRecord {
    pub kind: KindName,
    pub severity: Severity,
    /// When the event happened, stamped by the server; written in UTC.
    pub at: OffsetDateTime,
    pub entity: String,
    pub subject: Option<EventSubject>,
    /// The claim holder at the time, `{assignee}/{run id}`, when there is one.
    pub actor: Option<String>,
    /// The kind's own fields, in declared order.
    pub fields: Vec<(FieldName, FieldValue)>,
    /// The note's one heading line, without its `#`.
    pub heading: String,
    /// A recorder's own text for the body, for recorded kinds.
    pub content: Option<String>,
}

impl EventRecord {
    /// The vault-relative path of the note: the UTC month folder, then the second-granularity stamp and a slug.
    ///
    /// `attempt` is zero for the first choice and carries no suffix; from one upward it adds `-{attempt}`, which is
    /// how a caller steps past a path another event already holds.
    #[must_use]
    pub fn relative_path(&self, attempt: u32) -> String {
        let at = self.at.to_offset(UtcOffset::UTC);
        let (year, month, day) = (at.year(), u8::from(at.month()), at.day());
        let stamp = format!(
            "{year:04}-{month:02}-{day:02}-{:02}{:02}{:02}",
            at.hour(),
            at.minute(),
            at.second()
        );
        let slug = self.slug();
        if attempt == 0 {
            format!("memory/events/{year:04}/{month:02}/{stamp}-{slug}.md")
        } else {
            format!("memory/events/{year:04}/{month:02}/{stamp}-{slug}-{attempt}.md")
        }
    }

    /// The subject's id or name where there is one, else the kind, lower-cased and hyphenated.
    fn slug(&self) -> String {
        self.subject
            .as_ref()
            .map(|subject| slugify(&subject.slug_source()))
            .filter(|slug| !slug.is_empty())
            .unwrap_or_else(|| slugify(self.kind.as_str()))
    }
}

/// Lower-cases `text`, turns every run of other characters into one hyphen, and bounds the length.
fn slugify(text: &str) -> String {
    let mut slug = String::new();
    for character in text.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.truncate(MAX_SLUG_LEN);
    slug.trim_end_matches('-').to_owned()
}

/// Settles an event's `entity`: the subject's, else the one the recorder gave, else the recorder's own, else `vault`.
/// A blank value counts as absent.
#[must_use]
pub fn resolve_entity(subject: Option<&str>, given: Option<&str>, recorder: Option<&str>) -> String {
    [subject, given, recorder]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|entity| !entity.is_empty())
        .unwrap_or("vault")
        .to_owned()
}

/// `2026-10-28T03:15:42Z`: RFC 3339 in UTC, to the second.
pub(crate) fn rfc3339_utc(at: OffsetDateTime) -> String {
    let at = at.to_offset(UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

impl fmt::Display for EventRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "---")?;
        writeln!(f, "type: event")?;
        writeln!(f, "kind: {}", self.kind)?;
        writeln!(f, "severity: {}", self.severity)?;
        writeln!(f, "at: {}", rfc3339_utc(self.at))?;
        writeln!(f, "entity: {}", single_quoted(&self.entity))?;
        if let Some(subject) = &self.subject {
            writeln!(f, "subject: {}", single_quoted(&subject.link))?;
            if let Some(id) = &subject.id {
                writeln!(f, "subject_id: {}", single_quoted(id))?;
            }
            if let Some(path) = &subject.path {
                writeln!(f, "subject_path: {}", single_quoted(path))?;
            }
            if let Some(base) = &subject.base {
                writeln!(f, "subject_base: {}", single_quoted(base))?;
            }
        }
        if let Some(actor) = &self.actor {
            writeln!(f, "actor: {}", single_quoted(actor))?;
        }
        for (name, value) in &self.fields {
            writeln!(f, "{name}: {value}")?;
        }
        writeln!(f, "---")?;
        let mut body = String::new();
        let heading = self.heading.split_whitespace().collect::<Vec<_>>().join(" ");
        let _ = write!(body, "\n# {heading}\n");
        if let Some(content) = self.content.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
            let _ = write!(body, "\n{content}\n");
        }
        f.write_str(&body)
    }
}

#[cfg(test)]
#[path = "record_test.rs"]
mod tests;
