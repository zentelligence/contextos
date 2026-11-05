//! The retention sweep of the events the server writes, and the background task that runs it.
//!
//! The sweep runs at start-up and once a day while serving, under the vault's write lock. The date of the last
//! completed sweep lives in the vault's state directory, so of several processes serving one vault only the first to
//! see a new UTC day sweeps. Deletions are internal hard deletes, which Git follows; nothing else does, and an event
//! the sweep deletes raises no event.

use std::path::PathBuf;
use std::time::Duration;

use contextos_core::{Clock, DeleteMode, DeleteMutation, Origin, VaultPath, VaultPathInput, VaultRootId};
use contextos_events::{RetentionPolicy, SweepDay, SweepNote, Verdict};
use contextos_fs::{EntryKind, ListDirectoryRequest, ReadTextRequest};
use contextos_obsidian::FrontmatterDocument;
use thiserror::Error;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::events_folder::{EVENTS_BASE, EVENTS_FOLDER, EVENTS_INDEX, is_markdown};
use crate::server::ContextOsServer;
use crate::tools::fs::child_path;
use crate::{event_kinds, events_record};

/// The file in a vault's state directory that records the last sweep.
const SWEEP_FILE: &str = "events-sweep.json";

/// What the sweep did for one vault.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EventSweepReport {
    /// The vault's name.
    pub vault: String,
    /// Whether a sweep ran: `false` when this UTC day was already swept, or the vault is not managed.
    pub swept: bool,
    /// How many event notes were deleted.
    pub deleted: usize,
    /// How many deletions failed, to be retried at the next sweep.
    pub failed: usize,
    /// How many emptied folders were removed.
    pub removed_folders: usize,
    /// Event notes kept because their time cannot be read, by vault-relative path.
    pub unreadable: Vec<String>,
}

/// Why the sweep could not run for a vault.
#[derive(Debug, Error)]
pub enum EventsSweepError {
    /// A vault's path could not be built.
    #[error("the events sweep could not address a vault: {0}")]
    Path(#[from] contextos_core::PathError),
    /// A vault's write lock could not be taken.
    #[error("the events sweep could not take a vault's write lock ({cause})")]
    Lock {
        /// The stable code of the failure that stopped the lock.
        cause: String,
    },
    /// The task running the sweep failed or was cancelled.
    #[error("the events sweep task did not finish: {0}")]
    Task(#[from] tokio::task::JoinError),
    /// A vault's derived-state directory could not be resolved.
    #[error("the events sweep could not find the vault's state directory: {0}")]
    StateDirectory(#[from] crate::state_dir::StateDirError),
    /// A configured vault has no root.
    #[error("the events sweep found a configured vault with no root")]
    UnknownVault,
}

impl From<crate::tool_error::ToolError> for EventsSweepError {
    fn from(error: crate::tool_error::ToolError) -> Self {
        Self::Lock {
            cause: crate::tool_error::ToolFailure::from(error).code,
        }
    }
}

impl EventsSweepError {
    /// A stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Path(_) => "events/sweep-path",
            Self::Lock { .. } => "events/sweep-lock",
            Self::Task(_) => "events/sweep-task",
            Self::StateDirectory(_) => "events/sweep-state-directory",
            Self::UnknownVault => "events/sweep-unknown-vault",
        }
    }
}

impl ContextOsServer {
    /// Sweeps every managed vault whose events have not been swept on the current UTC day, under each vault's write
    /// lock.
    ///
    /// # Errors
    ///
    /// Returns [`EventsSweepError`] when a vault's write lock cannot be taken or the sweep task fails.
    pub async fn sweep_events(&self) -> Result<Vec<EventSweepReport>, EventsSweepError> {
        let mut reports = Vec::new();
        for index in 0..self.config.vaults.len() {
            let root_id = VaultRootId::try_from(index)?;
            let guards = self.writes.lock_roots(&[root_id]).await?;
            let server = self.clone();
            let report = tokio::task::spawn_blocking(move || {
                let _guards = guards;
                server.sweep_vault(index)
            })
            .await??;
            reports.push(report);
        }
        Ok(reports)
    }

    /// Spawns the task that sweeps at start-up and again each UTC day until `shutdown`, returning its handle for the
    /// caller to await when shutting down.
    #[must_use]
    pub fn spawn_events_maintenance(&self, shutdown: &CancellationToken) -> tokio::task::JoinHandle<()> {
        let server = self.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            loop {
                match server.sweep_events().await {
                    Ok(reports) => {
                        for report in reports.iter().filter(|report| report.swept) {
                            tracing::info!(
                                vault = %report.vault,
                                deleted = report.deleted,
                                failed = report.failed,
                                unreadable = report.unreadable.len(),
                                "events swept"
                            );
                        }
                    }
                    Err(error) => tracing::warn!(code = error.code(), "the events sweep did not run"),
                }
                let wait = until_next_utc_day(server.clock.now());
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    () = tokio::time::sleep(wait) => {}
                }
            }
        })
    }

    fn sweep_vault(&self, index: usize) -> Result<EventSweepReport, EventsSweepError> {
        let vault_config = &self.config.vaults[index];
        let root_id = VaultRootId::try_from(index)?;
        let root = self.roots.root(root_id).ok_or(EventsSweepError::UnknownVault)?;
        let mut report = EventSweepReport {
            vault: root.name().to_owned(),
            ..EventSweepReport::default()
        };
        if !vault_config.managed {
            return Ok(report);
        }
        let state_directory =
            crate::state_dir::resolve_state_directory(vault_config.state_directory.as_deref(), root.path())?;
        let marker: PathBuf = state_directory.join(SWEEP_FILE);
        let now = self.clock.now();
        let today = SweepDay::utc_date(now);
        if let Ok(Some(last)) = SweepDay::load(&marker)
            && !last.is_due(today)
        {
            return Ok(report);
        }
        let vault = VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw: &format!("{}://.", root.name()),
        })?;
        let policy = RetentionPolicy {
            default_days: vault_config.events.retention_days,
            keep_attention: vault_config.events.keep_attention,
        };
        let kind_days = self.kind_windows(&vault);
        let events = child_path(&vault, &self.roots, EVENTS_FOLDER)?;
        let mut walk = Walk {
            server: self,
            vault: &vault,
            policy,
            kind_days: &kind_days,
            now,
            report: &mut report,
        };
        walk.run(&events);
        report.swept = true;
        if let Err(error) = SweepDay::from(today).store(&marker) {
            tracing::warn!(code = error.code(), "the events sweep date could not be stored");
        }
        Ok(report)
    }

    /// Each declared kind's own retention window, by kind name.
    fn kind_windows(&self, vault: &VaultPath) -> Vec<(String, u32)> {
        let Ok(base) = child_path(vault, &self.roots, EVENTS_BASE) else {
            return Vec::new();
        };
        let Ok(Some(folder)) = events_record::kind_registry_folder(&self.filesystem, &self.roots, &base) else {
            return Vec::new();
        };
        let series = crate::tools::base_sidecar::sidecar_path(&self.roots, &base)
            .and_then(|sidecar| crate::tools::base_record::read_sidecar(&self.filesystem, &sidecar))
            .map(|state| events_record::sidecar_series(&state))
            .unwrap_or_default();
        let loaded = event_kinds::load(&self.filesystem, &self.roots, vault, &folder, &series);
        loaded.registry.retention_windows()
    }
}

/// How long until the next UTC midnight, with a minute of margin so the new day has certainly begun.
fn until_next_utc_day(now: OffsetDateTime) -> Duration {
    let today = SweepDay::utc_date(now);
    let Some(tomorrow) = today.next_day() else {
        return Duration::from_hours(24);
    };
    let midnight = tomorrow.midnight().assume_utc() + time::Duration::minutes(1);
    let wait = midnight - now;
    Duration::try_from(wait).unwrap_or(Duration::from_secs(60))
}

/// One note found under the events folder that the sweep may judge.
struct Found {
    path: VaultPath,
    relative: String,
    note: SweepNote,
}

/// One pass over the events folder: read every event note, judge them all together by the shared rules for what is
/// still open, delete the expired, and remove the folders that are left empty.
struct Walk<'a> {
    server: &'a ContextOsServer,
    vault: &'a VaultPath,
    policy: RetentionPolicy,
    kind_days: &'a [(String, u32)],
    now: OffsetDateTime,
    report: &'a mut EventSweepReport,
}

impl Walk<'_> {
    fn run(&mut self, events: &VaultPath) {
        let mut found = Vec::new();
        let mut folders = Vec::new();
        self.collect(events, EVENTS_FOLDER, &mut found, &mut folders);
        let notes: Vec<SweepNote> = found.iter().map(|found| found.note.clone()).collect();
        let verdicts = Verdict::plan(
            &notes,
            self.policy,
            |kind| {
                self.kind_days
                    .iter()
                    .find_map(|(name, days)| (name == kind).then_some(*days))
            },
            self.now,
        );
        for (found, verdict) in found.iter().zip(verdicts) {
            match verdict {
                Verdict::Kept => {}
                Verdict::Unreadable => self.report.unreadable.push(found.relative.clone()),
                Verdict::Expired => self.delete(found),
            }
        }
        // Deepest folders first, so a month folder emptied by its notes is removed before its year is judged.
        for (path, relative) in folders.iter().rev() {
            self.remove_if_empty(path, relative);
        }
    }

    /// Reads the notes in `folder` and below it into `found`, recording each folder below the events root in
    /// `folders` before the folders inside it.
    fn collect(
        &mut self,
        folder: &VaultPath,
        relative: &str,
        found: &mut Vec<Found>,
        folders: &mut Vec<(VaultPath, String)>,
    ) {
        let Ok(listing) = self
            .server
            .filesystem
            .list_directory(&ListDirectoryRequest { path: folder.clone() })
        else {
            return;
        };
        for entry in listing.entries {
            let child = format!("{relative}/{}", entry.name);
            let Some(child_path) = self.entry_path(&child) else {
                continue;
            };
            match entry.kind {
                EntryKind::Directory => {
                    folders.push((child_path.clone(), child.clone()));
                    self.collect(&child_path, &child, found, folders);
                }
                EntryKind::File if child != EVENTS_INDEX && is_markdown(&entry.name) => {
                    self.read(child_path, child, found);
                }
                EntryKind::File => {}
            }
        }
    }

    /// The path of the entry at `relative`, or `None` when it is not that entry: a symbolic link or other reparse
    /// point resolves to wherever it points, so a path that comes back as anything but the one asked for is skipped
    /// and never read, followed, or deleted.
    fn entry_path(&self, relative: &str) -> Option<VaultPath> {
        let path = child_path(self.vault, &self.server.roots, relative).ok()?;
        (path.portable_relative() == relative).then_some(path)
    }

    fn is_empty(&self, path: &VaultPath) -> bool {
        self.server
            .filesystem
            .list_directory(&ListDirectoryRequest { path: path.clone() })
            .is_ok_and(|listing| listing.entries.is_empty())
    }

    /// Adds the event note at `path` to `found`. A note that cannot be read is reported unreadable; one that is not an
    /// event is left alone and reported by `doctor`, since only what the server wrote as an event is ever deleted.
    fn read(&mut self, path: VaultPath, relative: String, found: &mut Vec<Found>) {
        let document = self
            .server
            .filesystem
            .read_text(&ReadTextRequest {
                path: path.clone(),
                limit: None,
            })
            .ok()
            .and_then(|file| FrontmatterDocument::try_from(file.content.as_str()).ok());
        let Some(document) = document else {
            self.report.unreadable.push(relative);
            return;
        };
        if document.frontmatter().get("type").and_then(serde_json::Value::as_str) != Some("event") {
            return;
        }
        let note = SweepNote {
            path: relative.clone(),
            frontmatter: document.frontmatter().clone(),
        };
        found.push(Found { path, relative, note });
    }

    fn delete(&mut self, found: &Found) {
        let deleted = self.server.mutations.delete_path(&DeleteMutation {
            path: found.path.clone(),
            mode: DeleteMode::Hard,
            origin: Origin::Internal("events".to_owned()),
        });
        match deleted {
            Ok(_) => self.report.deleted += 1,
            Err(error) => {
                self.report.failed += 1;
                tracing::warn!(path = %found.relative, code = error.code(), "an expired event could not be deleted; it is retried at the next sweep");
            }
        }
    }

    fn remove_if_empty(&mut self, path: &VaultPath, relative: &str) {
        if !self.is_empty(path) {
            return;
        }
        let removed = self.server.mutations.delete_path(&DeleteMutation {
            path: path.clone(),
            mode: DeleteMode::Hard,
            origin: Origin::Internal("events".to_owned()),
        });
        match removed {
            Ok(_) => self.report.removed_folders += 1,
            Err(error) => {
                tracing::warn!(path = %relative, code = error.code(), "an empty events folder could not be removed");
            }
        }
    }
}

#[cfg(test)]
#[path = "server_events_test.rs"]
mod tests;
