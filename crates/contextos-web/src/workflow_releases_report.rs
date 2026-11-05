//! What `workflows releases` prints: lines of text, or one JSON document.

use std::collections::BTreeMap;
use std::fmt;

use contextos_packages::ReleaseOrigin;
use contextos_workflows::{ReleaseKind, RetiredReleaseCause};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::workflow_releases::{ReleaseCondition, ReleaseLine, WorkflowReleases};
use crate::workflow_releases_error::WorkflowReleasesError;

/// A report as text.
pub struct ReleasesText<'a>(pub &'a WorkflowReleases);

/// A report as one JSON document.
pub struct ReleasesJson<'a>(pub &'a WorkflowReleases);

/// A refusal as text, `refused {code}: {message}`.
pub struct ReleasesRefusalText<'a>(pub &'a WorkflowReleasesError);

/// A refusal as the `{"error":{"code":...,"message":...}}` document every command gives.
pub struct ReleasesRefusalJson<'a>(pub &'a WorkflowReleasesError);

/// The words a release's kind is shown with, capitalised for the head of a line.
pub(crate) const fn kind_title(kind: ReleaseKind) -> &'static str {
    match kind {
        ReleaseKind::Current => "Current",
        ReleaseKind::Previous => "Previous",
        ReleaseKind::Future => "Future",
        ReleaseKind::Retired => "Retired",
    }
}

/// The word a release's kind has in the JSON document.
pub(crate) const fn kind_word(kind: ReleaseKind) -> &'static str {
    match kind {
        ReleaseKind::Current => "current",
        ReleaseKind::Previous => "previous",
        ReleaseKind::Future => "future",
        ReleaseKind::Retired => "retired",
    }
}

pub(crate) const fn origin_word(origin: ReleaseOrigin) -> &'static str {
    match origin {
        ReleaseOrigin::Source => "source",
        ReleaseOrigin::Bundle => "bundle",
    }
}

pub(crate) const fn cause_word(cause: RetiredReleaseCause) -> &'static str {
    match cause {
        RetiredReleaseCause::Superseded => "superseded",
    }
}

/// A day as people read it: `3 November 2026`, in UTC.
pub(crate) struct Day(pub(crate) OffsetDateTime);

impl fmt::Display for Day {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let at = self.0.to_offset(time::UtcOffset::UTC);
        write!(formatter, "{} {} {}", at.day(), at.month(), at.year())
    }
}

/// The head of a release's text: its kind, and when it was installed.
pub(crate) fn summary(line: &ReleaseLine) -> String {
    match line.installed_at {
        Some(at) => format!("{}, installed {}", kind_title(line.kind), Day(at)),
        None => format!("{}, install date not recorded", kind_title(line.kind)),
    }
}

impl ReleaseCondition {
    /// The word the JSON document gives the condition.
    pub(crate) const fn word(&self) -> &'static str {
        match self {
            Self::Whole => "whole",
            Self::Removed => "removed",
            Self::Incomplete(_) => "incomplete",
            Self::Damaged(_) => "damaged",
            Self::Unverifiable(_) => "unverifiable",
        }
    }

    /// What is wrong, one line each; none for a whole release.
    pub(crate) fn problems(&self) -> Vec<String> {
        match self {
            Self::Whole => Vec::new(),
            Self::Removed => vec!["the release is no longer in the store".to_owned()],
            Self::Incomplete(findings) | Self::Damaged(findings) => findings.clone(),
            Self::Unverifiable(reason) => vec![reason.clone()],
        }
    }
}

impl fmt::Display for ReleasesText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let report = self.0;
        if report.releases.is_empty() {
            return if report.store == SHARED {
                write!(
                    formatter,
                    "No shared releases of {}/{}.",
                    report.package, report.workflow
                )
            } else {
                write!(
                    formatter,
                    "No releases of {}/{} in {}.",
                    report.package, report.workflow, report.store
                )
            };
        }
        if report.store == SHARED {
            write!(formatter, "{}/{} (shared)", report.package, report.workflow)?;
        } else {
            write!(formatter, "{}/{} in {}", report.package, report.workflow, report.store)?;
        }
        for line in &report.releases {
            write!(formatter, "\n{}\n    version {}", summary(line), line.version)?;
            if let Some(marker) = &line.retired {
                write!(
                    formatter,
                    "\n    retired {}, {}",
                    Day(marker.at),
                    cause_word(marker.cause)
                )?;
            }
            match &line.lock {
                Some(lock) if !lock.is_empty() => {
                    let held: Vec<String> = lock
                        .iter()
                        .map(|(name, locked)| format!("{name} {}", locked.version))
                        .collect();
                    write!(formatter, "\n    locked to {}", held.join(", "))?;
                }
                Some(_) => formatter.write_str("\n    locked to no library")?,
                None => formatter.write_str("\n    no lock yet")?,
            }
            if line.jobs.is_empty() {
                formatter.write_str("\n    no jobs")?;
            } else {
                let counts: Vec<String> = line
                    .jobs
                    .iter()
                    .map(|(stage, count)| format!("{count} {}", stage.as_str()))
                    .collect();
                write!(formatter, "\n    jobs {}", counts.join(", "))?;
            }
            for problem in line.condition.problems() {
                write!(formatter, "\n    {}: {problem}", line.condition.word())?;
            }
            for template in &line.missing_templates {
                write!(formatter, "\n    missing template: {template}")?;
            }
        }
        Ok(())
    }
}

/// The store word a report gives the host's package store.
const SHARED: &str = "shared";

#[derive(Serialize)]
struct LockedView<'a> {
    library: &'a str,
    version: String,
    digest: &'a str,
}

#[derive(Serialize)]
struct RetiredView {
    at: String,
    cause: &'static str,
}

#[derive(Serialize)]
struct ReleaseView<'a> {
    version: String,
    kind: &'static str,
    summary: String,
    installed_at: Option<String>,
    origin: Option<&'static str>,
    condition: &'static str,
    problems: Vec<String>,
    missing_templates: &'a [String],
    retired: Option<RetiredView>,
    lock: Option<Vec<LockedView<'a>>>,
    jobs: BTreeMap<&'static str, usize>,
}

#[derive(Serialize)]
struct ReportView<'a> {
    store: &'a str,
    package: &'a str,
    workflow: &'a str,
    releases: Vec<ReleaseView<'a>>,
}

pub(crate) fn stamp(at: OffsetDateTime) -> Option<String> {
    at.format(&Rfc3339).ok()
}

impl<'a> From<&'a ReleaseLine> for ReleaseView<'a> {
    fn from(line: &'a ReleaseLine) -> Self {
        Self {
            version: line.version.to_string(),
            kind: kind_word(line.kind),
            summary: summary(line),
            installed_at: line.installed_at.and_then(stamp),
            origin: line.origin.map(origin_word),
            condition: line.condition.word(),
            problems: line.condition.problems(),
            missing_templates: &line.missing_templates,
            retired: line.retired.as_ref().map(|marker| RetiredView {
                at: stamp(marker.at).unwrap_or_default(),
                cause: cause_word(marker.cause),
            }),
            lock: line.lock.as_ref().map(|lock| {
                lock.iter()
                    .map(|(library, locked)| LockedView {
                        library,
                        version: locked.version.to_string(),
                        digest: &locked.digest,
                    })
                    .collect()
            }),
            jobs: line
                .jobs
                .iter()
                .map(|(stage, count)| (stage.as_str(), *count))
                .collect(),
        }
    }
}

impl fmt::Display for ReleasesJson<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let report = self.0;
        let view = ReportView {
            store: &report.store,
            package: &report.package,
            workflow: &report.workflow,
            releases: report.releases.iter().map(ReleaseView::from).collect(),
        };
        // The view is plain strings and numbers, which always serialise; the fallback keeps the result a document.
        match serde_json::to_string(&view) {
            Ok(document) => formatter.write_str(&document),
            Err(_) => formatter.write_str(
                r#"{"error":{"code":"workflow/state-unavailable","message":"the result could not be rendered"}}"#,
            ),
        }
    }
}

impl fmt::Display for ReleasesRefusalText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "refused {}: {}", self.0.code(), self.0)
    }
}

impl fmt::Display for ReleasesRefusalJson<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let document = serde_json::json!({
            "error": { "code": self.0.code(), "message": self.0.to_string() }
        });
        write!(formatter, "{document}")
    }
}

#[cfg(test)]
#[path = "workflow_releases_report_test.rs"]
mod tests;
