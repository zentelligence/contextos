//! One `[{package}.{workflow}]` table of the state file, which holds three kinds of entry under one name and tells them
//! apart by the key alone: `current` and `previous` (with the time each took effect) and `future` name versions, a key holding dots
//! is a quoted version whose table is that release's library lock and retired marker, and any other key is a job.
//!
//! A version always holds dots and a job slug never does, and `current`, `previous`, and `future` are never job slugs,
//! so the three cannot meet.

use std::collections::BTreeMap;
use std::fmt;

use semver::Version;
use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::OffsetDateTime;

use crate::release_state::{WorkflowReleaseState, WorkflowVersions};
use crate::state::JobState;

/// A time as the state file writes it: an RFC 3339 string.
#[derive(Deserialize, Serialize)]
struct At(#[serde(with = "time::serde::rfc3339")] OffsetDateTime);

/// `[{package}.{workflow}]`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct WorkflowTable {
    /// The workflow's `current`, `previous`, and `future` versions.
    pub(crate) versions: WorkflowVersions,
    /// Each release's lock and retired marker, by version.
    pub(crate) releases: BTreeMap<Version, WorkflowReleaseState>,
    /// The workflow's jobs, by slug.
    pub(crate) jobs: BTreeMap<String, JobState>,
}

impl Serialize for WorkflowTable {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        if let Some(current) = &self.versions.current {
            map.serialize_entry("current", current)?;
        }
        if let Some(at) = self.versions.current_at {
            map.serialize_entry("current_at", &At(at))?;
        }
        if let Some(previous) = &self.versions.previous {
            map.serialize_entry("previous", previous)?;
        }
        if let Some(at) = self.versions.previous_at {
            map.serialize_entry("previous_at", &At(at))?;
        }
        if let Some(future) = &self.versions.future {
            map.serialize_entry("future", future)?;
        }
        for (version, release) in &self.releases {
            map.serialize_entry(&version.to_string(), release)?;
        }
        for (slug, job) in &self.jobs {
            map.serialize_entry(slug, job)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for WorkflowTable {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(TableVisitor)
    }
}

struct TableVisitor;

impl<'de> Visitor<'de> for TableVisitor {
    type Value = WorkflowTable;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a workflow table of versions, releases, and jobs")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<WorkflowTable, A::Error> {
        let mut table = WorkflowTable::default();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "current" => table.versions.current = Some(map.next_value()?),
                "current_at" => table.versions.current_at = Some(map.next_value::<At>()?.0),
                "previous" => table.versions.previous = Some(map.next_value()?),
                "previous_at" => table.versions.previous_at = Some(map.next_value::<At>()?.0),
                "future" => table.versions.future = Some(map.next_value()?),
                other if other.contains('.') => {
                    let version = Version::parse(other).map_err(serde::de::Error::custom)?;
                    table.releases.insert(version, map.next_value()?);
                }
                _ => {
                    table.jobs.insert(key, map.next_value()?);
                }
            }
        }
        Ok(table)
    }
}

#[cfg(test)]
#[path = "workflow_table_test.rs"]
mod tests;
