//! The vault files that make `memory/events.base` usable, as the plugin scaffold ships them, and readers for the event
//! notes a vault holds. Not a test target.
#![allow(dead_code)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::jobs::{frontmatter_field, write};

pub const EVENTS_BASE: &str = "filters:\n  and:\n    - file.inFolder(\"memory/events\")\n    - note.type == \"event\"\n\
    views:\n  - type: table\n    name: Recent\n    order:\n      - note.at\n";

/// The sidecar, declaring the kind registry and the `S` key series.
pub const EVENTS_SIDECAR: &str = "---\nbase: '[[memory/events.base|Events]]'\nlast_create: 2026-01-01T00:00:00Z\n\
    last_update: 2026-01-01T00:00:00Z\nkey_prefixes:\n  S: 0\nkey_format: '{key_prefix}-{key_id}'\n\
    template: '[[templates/event|Event]]'\nfixed_fields: [type]\nevent_kinds: registry/events\n---\n";

/// The same sidecar without a kind registry.
pub const EVENTS_SIDECAR_WITHOUT_KINDS: &str = "---\nbase: '[[memory/events.base|Events]]'\n\
    last_create: 2026-01-01T00:00:00Z\nlast_update: 2026-01-01T00:00:00Z\nkey_prefixes:\n  S: 0\n\
    key_format: '{key_prefix}-{key_id}'\ntemplate: '[[templates/event|Event]]'\nfixed_fields: [type]\n---\n";

pub const EVENT_TEMPLATE: &str = "---\ntype: event\nkind: \"{{kind}}\"\nseverity: \"{{info | warning | error}}\"\n\
    at: \"{{RFC 3339 time in UTC}}\"\nentity: \"{{entity}}\"\nsubject: \"{{link to the note the event is about}}\"\n\
    subject_id: \"{{id of the subject}}\"\nsubject_path: \"{{path of the subject}}\"\n\
    subject_base: \"{{base of the subject}}\"\nactor: \"{{assignee/run id}}\"\n---\n\n# {{What happened}}\n";

/// A kind definition with the given fields.
pub fn kind_definition(kind: &str, severities: &str, default: &str, required: &str, subject: &str) -> String {
    format!(
        "---\ntype: event-kind\nkind: {kind}\nlabel: {kind}\ndescription: A test kind.\nseverities: [{severities}]\n\
         default_severity: {default}\nfields:\n  required: [{required}]\n  optional: []\nsubject: {subject}\n---\n\n# {kind}\n"
    )
}

/// The kinds the scaffold ships for the Web scheduler.
pub fn job_kinds() -> Vec<(&'static str, String)> {
    vec![
        (
            "registry/events/job.failed.md",
            kind_definition(
                "job.failed",
                "error",
                "error",
                "package, workflow, job, run, code",
                "required",
            ),
        ),
        (
            "registry/events/job.suspended.md",
            kind_definition(
                "job.suspended",
                "warning",
                "warning",
                "package, workflow, job, code, suspended_at",
                "required",
            ),
        ),
        (
            "registry/events/job.recovered.md",
            kind_definition(
                "job.recovered",
                "info",
                "info",
                "package, workflow, job, run",
                "required",
            ),
        ),
        (
            "registry/events/job.retired.md",
            kind_definition(
                "job.retired",
                "info",
                "info",
                "package, workflow, job, replaced_by, retired_at",
                "required",
            ),
        ),
    ]
}

/// Writes the events base, its sidecar, the template, and the Web scheduler's three kinds under `root`.
pub fn seed_events(root: &Path) -> std::io::Result<()> {
    write(root, "memory/events.base", EVENTS_BASE)?;
    write(root, "memory/events.md", EVENTS_SIDECAR)?;
    write(root, "templates/event.md", EVENT_TEMPLATE)?;
    for (path, text) in job_kinds() {
        write(root, path, &text)?;
    }
    Ok(())
}

/// Every event note under `root`, with its text, oldest path first.
pub fn event_notes(root: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut pending = vec![root.join("memory").join("events")];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(text) = std::fs::read_to_string(&path)
                && text.starts_with("---\ntype: event")
            {
                found.push((path, text));
            }
        }
    }
    found.sort();
    found
}

/// The event notes of `kind`.
pub fn events_of_kind(root: &Path, kind: &str) -> Vec<(PathBuf, String)> {
    event_notes(root)
        .into_iter()
        .filter(|(_, text)| frontmatter_field(text, "kind").as_deref() == Some(kind))
        .collect()
}

/// The event notes of the kinds the Web scheduler records, which are the only ones a job pass writes itself: the server
/// also fires `task.created` for each task a pass raises.
pub fn job_event_notes(root: &Path) -> Vec<(PathBuf, String)> {
    event_notes(root)
        .into_iter()
        .filter(|(_, text)| frontmatter_field(text, "kind").is_some_and(|kind| kind.starts_with("job.")))
        .collect()
}

/// An event note to put in a vault by hand, as the server would have written it.
#[derive(Clone, Debug)]
pub struct EventSpec<'a> {
    pub kind: &'a str,
    pub severity: &'a str,
    /// RFC 3339 in UTC, `YYYY-MM-DDTHH:MM:SSZ`.
    pub at: &'a str,
    pub entity: &'a str,
    /// A `[[link]]`, when the event has a subject.
    pub subject: Option<&'a str>,
    pub subject_id: Option<&'a str>,
    /// Where the subject was when the event happened.
    pub subject_path: Option<&'a str>,
    /// The base the subject is a record of.
    pub subject_base: Option<&'a str>,
    /// The kind's own fields, in order.
    pub fields: Vec<(&'a str, &'a str)>,
    /// The note's body after its heading.
    pub body: &'a str,
}

impl Default for EventSpec<'_> {
    fn default() -> Self {
        Self {
            kind: "task.state-changed",
            severity: "info",
            at: "2026-10-28T03:15:42Z",
            entity: "vault",
            subject: None,
            subject_id: None,
            subject_path: None,
            subject_base: None,
            fields: Vec::new(),
            body: "",
        }
    }
}

impl EventSpec<'_> {
    /// The vault path the server would give the note: UTC month folder, stamp, and a slug from `slug`.
    pub fn path(&self, slug: &str) -> String {
        let (date, time) = self.at.split_once('T').unwrap_or((self.at, "000000Z"));
        let mut parts = date.split('-');
        let (year, month) = (parts.next().unwrap_or("0000"), parts.next().unwrap_or("00"));
        let stamp = time.trim_end_matches('Z').replace(':', "");
        format!("memory/events/{year}/{month}/{date}-{stamp}-{slug}.md")
    }

    /// The note's text.
    pub fn text(&self) -> String {
        let mut text = format!(
            "---\ntype: event\nkind: {}\nseverity: {}\nat: {}\nentity: {}\n",
            self.kind, self.severity, self.at, self.entity
        );
        if let Some(subject) = self.subject {
            let _ = writeln!(text, "subject: '{subject}'");
        }
        if let Some(id) = self.subject_id {
            let _ = writeln!(text, "subject_id: {id}");
        }
        if let Some(path) = self.subject_path {
            let _ = writeln!(text, "subject_path: '{path}'");
        }
        if let Some(base) = self.subject_base {
            let _ = writeln!(text, "subject_base: '{base}'");
        }
        for (name, value) in &self.fields {
            let _ = writeln!(text, "{name}: {value}");
        }
        let _ = write!(text, "---\n\n# {}\n{}", self.kind, self.body);
        text
    }

    /// Writes the note under `root` and returns its vault path.
    pub fn write_to(&self, root: &Path, slug: &str) -> std::io::Result<String> {
        let path = self.path(slug);
        write(root, &path, &self.text())?;
        Ok(path)
    }
}
