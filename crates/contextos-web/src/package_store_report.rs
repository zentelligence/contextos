//! What `packages list`, `inspect`, and `remove` print and the package API answers: lines of text, or one JSON document,
//! for a vault's store and the host's alike.

use std::fmt;

use contextos_packages::{EntryProvenance, RecordedCause, RemovalCause};
use contextos_workflows::{JobLifecycle, ReleaseRef};
use serde::Serialize;

use crate::package_keep::{ReasonText, ReasonView};
use crate::package_op_error::{PackageOpError, Reasons};
use crate::package_removal::Removal;
use crate::package_standing::{LedgerStanding, ReleaseStanding, StoreStanding};
use crate::store_retention::RetentionWarning;
use crate::workflow_releases::ReleaseCondition;
use crate::workflow_releases_report::{Day, kind_word, origin_word, stamp};

/// A store's standing as text.
pub struct StandingText<'a>(pub &'a StoreStanding);

/// A store's standing as one JSON document.
pub struct StandingJson<'a>(pub &'a StoreStanding);

/// One release as text.
pub struct ReleaseText<'a>(pub &'a ReleaseStanding);

/// One release as one JSON document.
pub struct ReleaseJson<'a>(pub &'a ReleaseStanding);

/// A removal that was made, and what could not be finished after it.
#[derive(Clone, Debug)]
pub struct RemovalReport {
    /// The word the store is named by: `shared`, or the vault's name.
    pub store: String,
    pub removal: Removal,
    /// What went wrong after the release was removed, which does not undo it.
    pub warnings: Vec<RetentionWarning>,
    /// The releases retention removed afterwards, because only the release removed kept them.
    pub retention_removed: Vec<ReleaseRef>,
}

/// A removal as text.
pub struct RemovalText<'a>(pub &'a RemovalReport);

/// A removal as one JSON document.
pub struct RemovalJson<'a>(pub &'a RemovalReport);

/// A rebuilt ledger: the releases it now records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RebuiltReport {
    /// The word the store is named by: `shared`, or the vault's name.
    pub store: String,
    pub recorded: Vec<String>,
}

/// A rebuilt ledger as text.
pub struct RebuiltText<'a>(pub &'a RebuiltReport);

/// A rebuilt ledger as one JSON document.
pub struct RebuiltJson<'a>(pub &'a RebuiltReport);

/// A refusal as text, `refused {code}: {message}`.
pub struct RefusalText<'a>(pub &'a PackageOpError);

/// A refusal as the `{"error":{"code":...,"message":...}}` document every command gives.
pub struct RefusalJson<'a>(pub &'a PackageOpError);

/// How a removal's cause reads in a sentence.
struct CauseText(RemovalCause);

impl fmt::Display for CauseText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.0 {
            RemovalCause::Retention => "removed by retention",
            RemovalCause::Forced => "removed by request, forced",
            RemovalCause::ByRequest => "removed by request",
            RemovalCause::Unrecorded => "gone, with no removal recorded",
        })
    }
}

/// How a removal's cause is written in a JSON document.
struct CauseWord(RemovalCause);

impl From<CauseWord> for &'static str {
    fn from(cause: CauseWord) -> Self {
        match cause.0 {
            RemovalCause::Retention => "retention",
            RemovalCause::Forced => "forced",
            RemovalCause::ByRequest => "by-request",
            RemovalCause::Unrecorded => "unrecorded",
        }
    }
}

/// Where a release came from, as the sentence that follows its condition.
struct ProvenanceText(Option<EntryProvenance>);

impl fmt::Display for ProvenanceText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(EntryProvenance::Installed { origin, installed_at }) => {
                write!(
                    formatter,
                    "from a {}, installed {}",
                    origin_word(origin),
                    Day(installed_at)
                )
            }
            Some(EntryProvenance::Rebuilt) => formatter.write_str("recorded by a ledger rebuild"),
            None => formatter.write_str("not in the ledger"),
        }
    }
}

/// A count of releases, with the noun in the number it needs.
struct ReleaseCount(usize);

impl fmt::Display for ReleaseCount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {}",
            self.0,
            if self.0 == 1 { "release" } else { "releases" }
        )
    }
}

fn write_release(formatter: &mut fmt::Formatter<'_>, held: &ReleaseStanding) -> fmt::Result {
    writeln!(formatter, "{}", held.release)?;
    writeln!(
        formatter,
        "    {}, {}",
        held.condition.word(),
        ProvenanceText(held.provenance)
    )?;
    for problem in held.condition.problems() {
        if !matches!(held.condition, ReleaseCondition::Whole) {
            writeln!(formatter, "    {}: {problem}", held.condition.word())?;
        }
    }
    let Some(contents) = &held.contents else {
        // A release that does not read whole has no contents to describe, but what the state says holds it is still said.
        return write_kept(formatter, held, false);
    };
    for workflow in &contents.workflows {
        write!(formatter, "    {}: {}", workflow.workflow, kind_word(workflow.kind))?;
        match &workflow.lock {
            Some(lock) if !lock.is_empty() => {
                let locked: Vec<String> = lock
                    .iter()
                    .map(|(name, locked)| format!("{name} {}", locked.version))
                    .collect();
                writeln!(formatter, ", locked to {}", locked.join(", "))?;
            }
            Some(_) => writeln!(formatter, ", locked to no library")?,
            None => writeln!(formatter, ", no lock yet")?,
        }
    }
    if !contents.libraries.is_empty() {
        let required: Vec<String> = contents
            .libraries
            .iter()
            .map(|(name, requirement)| format!("{name} {requirement}"))
            .collect();
        writeln!(formatter, "    requires {}", required.join(", "))?;
    }
    if !contents.exports.is_empty() {
        writeln!(formatter, "    exports {}", contents.exports.join(", "))?;
    }
    write_kept(formatter, held, true)
}

/// The reasons a release is kept, one line each; `say_none` adds the line that says nothing keeps it.
fn write_kept(formatter: &mut fmt::Formatter<'_>, held: &ReleaseStanding, say_none: bool) -> fmt::Result {
    if held.kept_by.is_empty() && say_none {
        writeln!(formatter, "    kept: nothing keeps it")?;
    }
    for reason in &held.kept_by {
        writeln!(formatter, "    kept: {}", ReasonText(reason))?;
    }
    Ok(())
}

fn write_section(formatter: &mut fmt::Formatter<'_>, title: &str, lines: &[String]) -> fmt::Result {
    if lines.is_empty() {
        return Ok(());
    }
    writeln!(formatter, "{title}")?;
    for line in lines {
        writeln!(formatter, "    {line}")?;
    }
    Ok(())
}

impl fmt::Display for StandingText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let standing = self.0;
        writeln!(formatter, "Package store {}", standing.store)?;
        match &standing.ledger {
            LedgerStanding::Absent => writeln!(formatter, "Ledger: none yet")?,
            LedgerStanding::Present { recorded, rebuilt_at } => {
                write!(formatter, "Ledger: {} recorded", ReleaseCount(*recorded))?;
                if let Some(at) = rebuilt_at {
                    write!(formatter, ", rebuilt {}", Day(*at))?;
                }
                writeln!(formatter)?;
            }
            LedgerStanding::Missing => writeln!(
                formatter,
                "Ledger: missing; installs and removals are refused until it is restored from a copy or rebuilt with \
                 `packages inspect --rebuild-ledger`"
            )?,
            LedgerStanding::Unreadable(reason) => writeln!(
                formatter,
                "Ledger: unreadable ({reason}); installs and removals are refused until it is restored from a copy"
            )?,
        }
        if standing.releases.is_empty() && standing.removed.is_empty() {
            return writeln!(formatter, "No packages installed.");
        }
        for held in &standing.releases {
            write_release(formatter, held)?;
        }
        let removed: Vec<String> = standing
            .removed
            .iter()
            .map(|gone| match gone.removed_at {
                Some(at) => format!("{}, {}, {}", gone.release, CauseText(gone.cause), Day(at)),
                None => format!("{}, {}", gone.release, CauseText(gone.cause)),
            })
            .collect();
        write_section(formatter, "Removed", &removed)?;
        let left: Vec<String> = standing
            .staging
            .iter()
            .chain(&standing.removing)
            .chain(&standing.unrecognised)
            .cloned()
            .collect();
        write_section(formatter, "Left by unfinished operations", &left)
    }
}

impl fmt::Display for ReleaseText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_release(formatter, self.0)?;
        // The listing leaves the digest out; inspecting one release is where it is asked for.
        if let Some(digest) = &self.0.digest {
            writeln!(formatter, "    digest: {digest}")?;
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct LockView<'a> {
    library: &'a str,
    version: String,
    digest: &'a str,
}

#[derive(Serialize)]
struct WorkflowView<'a> {
    workflow: &'a str,
    kind: &'static str,
    lock: Option<Vec<LockView<'a>>>,
}

#[derive(Serialize)]
struct LibraryView<'a> {
    name: &'a str,
    requirement: &'a str,
}

/// One release in a JSON document.
#[derive(Serialize)]
pub struct ReleaseView<'a> {
    name: &'a str,
    version: String,
    digest: Option<&'a str>,
    condition: &'static str,
    problems: Vec<String>,
    origin: Option<&'static str>,
    installed_at: Option<String>,
    workflows: Vec<WorkflowView<'a>>,
    libraries: Vec<LibraryView<'a>>,
    exports: &'a [String],
    kept_by: Vec<ReasonView>,
}

impl<'a> From<&'a ReleaseStanding> for ReleaseView<'a> {
    fn from(held: &'a ReleaseStanding) -> Self {
        let (origin, installed_at) = match held.provenance {
            Some(EntryProvenance::Installed { origin, installed_at }) => {
                (Some(origin_word(origin)), stamp(installed_at))
            }
            Some(EntryProvenance::Rebuilt) => (Some("rebuilt"), None),
            None => (None, None),
        };
        let contents = held.contents.as_ref();
        Self {
            name: held.release.name.as_str(),
            version: held.release.version.to_string(),
            digest: held.digest.as_deref(),
            condition: held.condition.word(),
            problems: held.condition.problems(),
            origin,
            installed_at,
            workflows: contents
                .map(|contents| {
                    contents
                        .workflows
                        .iter()
                        .map(|workflow| WorkflowView {
                            workflow: &workflow.workflow,
                            kind: kind_word(workflow.kind),
                            lock: workflow.lock.as_ref().map(|lock| {
                                lock.iter()
                                    .map(|(library, locked)| LockView {
                                        library,
                                        version: locked.version.to_string(),
                                        digest: &locked.digest,
                                    })
                                    .collect()
                            }),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            libraries: contents
                .map(|contents| {
                    contents
                        .libraries
                        .iter()
                        .map(|(name, requirement)| LibraryView { name, requirement })
                        .collect()
                })
                .unwrap_or_default(),
            exports: contents.map_or(&[], |contents| contents.exports.as_slice()),
            kept_by: held.kept_by.iter().map(ReasonView::from).collect(),
        }
    }
}

#[derive(Serialize)]
struct LedgerView<'a> {
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    recorded: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rebuilt_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
}

impl<'a> From<&'a LedgerStanding> for LedgerView<'a> {
    fn from(ledger: &'a LedgerStanding) -> Self {
        let bare = |state| Self {
            state,
            recorded: None,
            rebuilt_at: None,
            reason: None,
        };
        match ledger {
            LedgerStanding::Absent => bare("absent"),
            LedgerStanding::Present { recorded, rebuilt_at } => Self {
                recorded: Some(*recorded),
                rebuilt_at: rebuilt_at.and_then(stamp),
                ..bare("present")
            },
            LedgerStanding::Missing => bare("missing"),
            LedgerStanding::Unreadable(reason) => Self {
                reason: Some(reason),
                ..bare("unreadable")
            },
        }
    }
}

#[derive(Serialize)]
struct RemovedView<'a> {
    name: &'a str,
    version: String,
    cause: &'static str,
    removed_at: Option<String>,
}

#[derive(Serialize)]
struct StandingView<'a> {
    store: &'a str,
    ledger: LedgerView<'a>,
    releases: Vec<ReleaseView<'a>>,
    removed: Vec<RemovedView<'a>>,
    staging: &'a [String],
    removing: &'a [String],
    unrecognised: &'a [String],
}

impl<'a> From<&'a StoreStanding> for StandingView<'a> {
    fn from(standing: &'a StoreStanding) -> Self {
        Self {
            store: &standing.store,
            ledger: LedgerView::from(&standing.ledger),
            releases: standing.releases.iter().map(ReleaseView::from).collect(),
            removed: standing
                .removed
                .iter()
                .map(|gone| RemovedView {
                    name: gone.release.name.as_str(),
                    version: gone.release.version.to_string(),
                    cause: CauseWord(gone.cause).into(),
                    removed_at: gone.removed_at.and_then(stamp),
                })
                .collect(),
            staging: &standing.staging,
            removing: &standing.removing,
            unrecognised: &standing.unrecognised,
        }
    }
}

#[derive(Serialize)]
struct AffectedJobView<'a> {
    package: &'a str,
    workflow: &'a str,
    job: &'a str,
    lifecycle: &'static str,
}

#[derive(Serialize)]
struct WarningView<'a> {
    code: &'static str,
    message: &'a str,
}

/// A removal in a JSON document.
#[derive(Serialize)]
pub struct RemovalView<'a> {
    store: &'a str,
    name: &'a str,
    version: String,
    cause: &'static str,
    overridden: Vec<ReasonView>,
    affected_jobs: Vec<AffectedJobView<'a>>,
    retention_removed: Vec<ReleaseIdView<'a>>,
    warnings: Vec<WarningView<'a>>,
}

#[derive(Serialize)]
struct ReleaseIdView<'a> {
    name: &'a str,
    version: String,
}

impl<'a> From<&'a RemovalReport> for RemovalView<'a> {
    fn from(report: &'a RemovalReport) -> Self {
        let removal = &report.removal;
        Self {
            store: &report.store,
            name: removal.release.name.as_str(),
            version: removal.release.version.to_string(),
            cause: match removal.cause {
                RecordedCause::Retention => "retention",
                RecordedCause::Forced => "forced",
                RecordedCause::ByRequest => "by-request",
            },
            overridden: removal.overridden.iter().map(ReasonView::from).collect(),
            affected_jobs: removal
                .affected_jobs
                .iter()
                .map(|job| AffectedJobView {
                    package: &job.package,
                    workflow: &job.workflow,
                    job: &job.job,
                    lifecycle: job.lifecycle.as_str(),
                })
                .collect(),
            retention_removed: report
                .retention_removed
                .iter()
                .map(|gone| ReleaseIdView {
                    name: gone.name.as_str(),
                    version: gone.version.to_string(),
                })
                .collect(),
            warnings: report
                .warnings
                .iter()
                .map(|warning| WarningView {
                    code: RetentionWarning::CODE,
                    message: warning.message(),
                })
                .collect(),
        }
    }
}

/// The document, or a fixed error document in the unlikely event the view cannot be written.
fn document(view: &impl Serialize, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    // The views are plain strings, numbers, and lists, which always serialise; the fallback keeps the result a document.
    match serde_json::to_string(view) {
        Ok(written) => formatter.write_str(&written),
        Err(_) => formatter.write_str(
            r#"{"error":{"code":"packages/store-unavailable","message":"the result could not be rendered"}}"#,
        ),
    }
}

impl fmt::Display for StandingJson<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        document(&StandingView::from(self.0), formatter)
    }
}

impl fmt::Display for ReleaseJson<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        document(&ReleaseView::from(self.0), formatter)
    }
}

impl fmt::Display for RemovalText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let report = self.0;
        let removal = &report.removal;
        write!(formatter, "Removed {} from {}", removal.release, report.store)?;
        writeln!(
            formatter,
            "{}",
            if removal.cause == RecordedCause::Forced {
                ", forced."
            } else {
                "."
            }
        )?;
        if !removal.overridden.is_empty() {
            writeln!(formatter, "Overridden: {}", Reasons(&removal.overridden))?;
        }
        if !removal.affected_jobs.is_empty() {
            writeln!(
                formatter,
                "The next validation pass moves these jobs, because their release is gone:"
            )?;
            for job in &removal.affected_jobs {
                let moved_to = if job.lifecycle == JobLifecycle::Enabled {
                    "suspended"
                } else {
                    "draft"
                };
                writeln!(
                    formatter,
                    "    {}/{}/{} ({}, to {moved_to})",
                    job.package,
                    job.workflow,
                    job.job,
                    job.lifecycle.as_str()
                )?;
            }
        }
        if !report.retention_removed.is_empty() {
            let gone: Vec<String> = report.retention_removed.iter().map(ToString::to_string).collect();
            writeln!(
                formatter,
                "Retention then removed {}, which nothing else kept.",
                gone.join(", ")
            )?;
        }
        for warning in &report.warnings {
            writeln!(formatter, "Warning: {warning}")?;
        }
        Ok(())
    }
}

impl fmt::Display for RemovalJson<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        document(&RemovalView::from(self.0), formatter)
    }
}

impl fmt::Display for RebuiltText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let report = self.0;
        writeln!(
            formatter,
            "Rebuilt the ledger of {} from {}.",
            report.store,
            ReleaseCount(report.recorded.len())
        )?;
        for release in &report.recorded {
            writeln!(formatter, "    {release}")?;
        }
        Ok(())
    }
}

impl fmt::Display for RebuiltJson<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let report = self.0;
        document(
            &serde_json::json!({ "store": report.store, "recorded": report.recorded }),
            formatter,
        )
    }
}

impl fmt::Display for RefusalText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "refused {}: {}", self.0.code(), self.0)
    }
}

impl fmt::Display for RefusalJson<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reasons: Vec<ReasonView> = match self.0 {
            PackageOpError::InUse { reasons, .. } => reasons.iter().map(ReasonView::from).collect(),
            _ => Vec::new(),
        };
        let mut error = serde_json::json!({ "code": self.0.code(), "message": self.0.to_string() });
        if !reasons.is_empty() {
            error["reasons"] = serde_json::json!(reasons);
        }
        write!(formatter, "{}", serde_json::json!({ "error": error }))
    }
}

#[cfg(test)]
#[path = "package_store_report_test.rs"]
mod tests;
