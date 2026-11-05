//! `ledger.toml` as text: [`LedgerText`] from a [`Ledger`], and a [`Ledger`] from text.
//!
//! ```toml
//! schema = 1
//! rebuilt-at = "2026-11-06T00:00:00Z"   # only after a rebuild
//!
//! [[release]]
//! package = "report"
//! version = "1.0.0"
//! digest = "sha256:..."
//! provenance = "installed"              # or "rebuilt"
//! origin = "source"                     # or "bundle"; installed entries only
//! installed-at = "2026-11-03T09:30:15+11:00"
//! removed-at = "2026-11-04T01:00:00Z"   # with removal-cause, once removed
//! removal-cause = "retention"           # or "forced", "by-request"
//!
//! [release.interface]
//! ```
//!
//! Anything the schema does not describe is refused, never skipped, and a reason names a line or an entry but never
//! quotes the file.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::PackagesError;
use crate::io::{StoreFailure, StoreFailureKind};
use crate::ledger::{EntryProvenance, Ledger, LedgerEntry, LedgerProblem, LedgerRemoval, RecordedCause, ReleaseOrigin};
use crate::name::{ReleaseDigest, ReleaseId, ReleaseKey, ReleaseName, ReleaseVersion};

const SCHEMA: u32 = 1;

/// The text of a ledger file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LedgerText(pub(crate) String);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct LedgerFile {
    schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rebuilt_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    release: Vec<ReleaseRecord>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct ReleaseRecord {
    package: String,
    version: String,
    digest: String,
    provenance: ProvenanceRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<OriginRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    installed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    removed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    removal_cause: Option<CauseRecord>,
    interface: toml::Value,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ProvenanceRecord {
    Installed,
    Rebuilt,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum OriginRecord {
    Source,
    Bundle,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CauseRecord {
    Retention,
    Forced,
    ByRequest,
}

impl From<ReleaseOrigin> for OriginRecord {
    fn from(origin: ReleaseOrigin) -> Self {
        match origin {
            ReleaseOrigin::Source => Self::Source,
            ReleaseOrigin::Bundle => Self::Bundle,
        }
    }
}

impl From<OriginRecord> for ReleaseOrigin {
    fn from(origin: OriginRecord) -> Self {
        match origin {
            OriginRecord::Source => Self::Source,
            OriginRecord::Bundle => Self::Bundle,
        }
    }
}

impl From<RecordedCause> for CauseRecord {
    fn from(cause: RecordedCause) -> Self {
        match cause {
            RecordedCause::Retention => Self::Retention,
            RecordedCause::Forced => Self::Forced,
            RecordedCause::ByRequest => Self::ByRequest,
        }
    }
}

impl From<CauseRecord> for RecordedCause {
    fn from(cause: CauseRecord) -> Self {
        match cause {
            CauseRecord::Retention => Self::Retention,
            CauseRecord::Forced => Self::Forced,
            CauseRecord::ByRequest => Self::ByRequest,
        }
    }
}

fn unwritable(detail: &str) -> PackagesError {
    PackagesError::Store(StoreFailure {
        kind: StoreFailureKind::Failed,
        detail: detail.to_owned(),
    })
}

fn written_time(instant: OffsetDateTime) -> Result<String, PackagesError> {
    instant
        .format(&Rfc3339)
        .map_err(|_| unwritable("a time in the ledger cannot be written"))
}

impl<F: Serialize> TryFrom<&LedgerEntry<F>> for ReleaseRecord {
    type Error = PackagesError;

    fn try_from(entry: &LedgerEntry<F>) -> Result<Self, Self::Error> {
        let (provenance, origin, installed_at) = match entry.provenance {
            EntryProvenance::Installed { origin, installed_at } => (
                ProvenanceRecord::Installed,
                Some(OriginRecord::from(origin)),
                Some(written_time(installed_at)?),
            ),
            EntryProvenance::Rebuilt => (ProvenanceRecord::Rebuilt, None, None),
        };
        let (removed_at, removal_cause) = match entry.removal {
            Some(removal) => (
                Some(written_time(removal.removed_at)?),
                Some(CauseRecord::from(removal.cause)),
            ),
            None => (None, None),
        };
        let interface = toml::Value::try_from(&entry.interface)
            .map_err(|_| unwritable("a release's interface cannot be written to the ledger"))?;
        Ok(Self {
            package: entry.release.key.name.as_str().to_owned(),
            version: entry.release.key.version.to_string(),
            digest: entry.release.digest.as_str().to_owned(),
            provenance,
            origin,
            installed_at,
            removed_at,
            removal_cause,
            interface,
        })
    }
}

impl<F: Serialize> TryFrom<&Ledger<F>> for LedgerText {
    type Error = PackagesError;

    fn try_from(ledger: &Ledger<F>) -> Result<Self, Self::Error> {
        let file = LedgerFile {
            schema: SCHEMA,
            rebuilt_at: ledger.rebuilt_at.map(written_time).transpose()?,
            release: ledger
                .entries
                .iter()
                .map(ReleaseRecord::try_from)
                .collect::<Result<_, _>>()?,
        };
        toml::to_string(&file)
            .map(Self)
            .map_err(|_| unwritable("the ledger cannot be written"))
    }
}

fn read_time(raw: &str, field: &str, label: &str) -> Result<OffsetDateTime, LedgerProblem> {
    OffsetDateTime::parse(raw, &Rfc3339)
        .map_err(|_| LedgerProblem::Unreadable(format!("{label}: {field} is not a date and time")))
}

fn entry_error(label: &str, reason: &str) -> LedgerProblem {
    LedgerProblem::Unreadable(format!("{label}: {reason}"))
}

impl<F: DeserializeOwned> TryFrom<ReleaseRecord> for LedgerEntry<F> {
    type Error = LedgerProblem;

    fn try_from(record: ReleaseRecord) -> Result<Self, Self::Error> {
        let label = format!("{} {}", record.package, record.version);
        let name = ReleaseName::try_from(record.package.as_str())
            .map_err(|_| entry_error(&label, "package is not a package name"))?;
        let version = ReleaseVersion::try_from(record.version.as_str())
            .map_err(|_| entry_error(&label, "version is not an exact version"))?;
        let digest = ReleaseDigest::try_from(record.digest.as_str())
            .map_err(|_| entry_error(&label, "digest is not a sha256 digest"))?;
        let provenance = match (record.provenance, record.origin, record.installed_at.as_deref()) {
            (ProvenanceRecord::Installed, Some(origin), Some(at)) => EntryProvenance::Installed {
                origin: origin.into(),
                installed_at: read_time(at, "installed-at", &label)?,
            },
            (ProvenanceRecord::Installed, None, _) => {
                return Err(entry_error(&label, "an installed entry records its origin"));
            }
            (ProvenanceRecord::Installed, Some(_), None) => {
                return Err(entry_error(&label, "an installed entry records installed-at"));
            }
            (ProvenanceRecord::Rebuilt, None, None) => EntryProvenance::Rebuilt,
            (ProvenanceRecord::Rebuilt, Some(_), _) => {
                return Err(entry_error(&label, "a rebuilt entry records no origin"));
            }
            (ProvenanceRecord::Rebuilt, None, Some(_)) => {
                return Err(entry_error(&label, "a rebuilt entry records no installed-at"));
            }
        };
        let removal = match (record.removed_at.as_deref(), record.removal_cause) {
            (Some(at), Some(cause)) => Some(LedgerRemoval {
                removed_at: read_time(at, "removed-at", &label)?,
                cause: cause.into(),
            }),
            (None, None) => None,
            _ => {
                return Err(entry_error(
                    &label,
                    "a removal records both removed-at and removal-cause",
                ));
            }
        };
        let interface = record
            .interface
            .try_into::<F>()
            .map_err(|error| entry_error(&label, &format!("interface: {}", error.message())))?;
        Ok(Self {
            release: ReleaseId {
                key: ReleaseKey { name, version },
                digest,
            },
            interface,
            provenance,
            removal,
        })
    }
}

impl<F: DeserializeOwned> TryFrom<&str> for Ledger<F> {
    type Error = LedgerProblem;

    fn try_from(text: &str) -> Result<Self, Self::Error> {
        let file: LedgerFile = toml::from_str(text).map_err(|error| {
            let line = error
                .span()
                .map_or(0, |span| text[..span.start.min(text.len())].matches('\n').count() + 1);
            LedgerProblem::Unreadable(format!("line {line}: {}", error.message().replace('\n', " ")))
        })?;
        if file.schema != SCHEMA {
            return Err(LedgerProblem::Unreadable(format!(
                "schema {} is not a schema this server reads",
                file.schema
            )));
        }
        let rebuilt_at = file
            .rebuilt_at
            .as_deref()
            .map(|at| read_time(at, "rebuilt-at", "ledger"))
            .transpose()?;
        let mut entries: Vec<LedgerEntry<F>> = Vec::with_capacity(file.release.len());
        for record in file.release {
            let entry = LedgerEntry::<F>::try_from(record)?;
            if let Some(earlier) = entries.iter().find(|earlier| earlier.release.key == entry.release.key) {
                let label = format!("{} {}", entry.release.key.name.as_str(), entry.release.key.version);
                let reason = if earlier.release.digest == entry.release.digest {
                    "is recorded twice"
                } else {
                    "is recorded with two digests"
                };
                return Err(entry_error(&label, reason));
            }
            entries.push(entry);
        }
        Ok(Self { entries, rebuilt_at })
    }
}

#[cfg(test)]
#[path = "ledger_format_test.rs"]
mod tests;
