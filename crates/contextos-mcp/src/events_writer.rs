//! Writes one event note as an internal write.
//!
//! Every event the server fires goes through here, whether a tool call raised it or the retention sweep's neighbours
//! did: the path comes from [`EventRecord::relative_path`], a path already taken is stepped past, and the write is
//! `Origin::Internal`, so Git follows it and nothing else does.

use contextos_core::{
    OperationEvent, OperationWarning, Origin, VaultPath, VaultSet, WriteMutation, WriteOutcome, WritesVault,
};
use contextos_events::EventRecord;
use contextos_fs::FsError;
use thiserror::Error;

use crate::tool_error::ToolError;
use crate::tools::fs::child_path;

/// Bounded retry for events landing in the same second with the same slug: a genuine correctness concern (an
/// unbounded loop would never be reachable in practice, but "never" is not a proof), not a realistic limit.
pub(crate) const MAX_PATH_ATTEMPTS: u32 = 20;

/// What the origin of every event write is called.
const EVENT_ORIGIN: &str = "events";

/// Why an event note could not be written.
#[derive(Debug, Error)]
pub(crate) enum EventWriteError {
    #[error("no unused event path was found after {MAX_PATH_ATTEMPTS} attempts")]
    PathsExhausted,
    #[error(transparent)]
    Write(#[from] ToolError),
}

/// The stable code of a failure, which is all a log line may carry: the error's own text can hold an absolute path.
impl From<EventWriteError> for String {
    fn from(error: EventWriteError) -> Self {
        match error {
            EventWriteError::PathsExhausted => "events/paths-exhausted".to_owned(),
            EventWriteError::Write(error) => crate::tool_error::ToolFailure::from(error).code,
        }
    }
}

/// An event note that was written.
#[derive(Debug)]
pub(crate) struct WrittenEvent {
    /// The note's vault-relative path.
    pub(crate) path: String,
    /// What the write reported: the note's path and content hash.
    pub(crate) outcome: WriteOutcome,
    /// The operation event the write produced, for a caller that hands it to the router itself.
    pub(crate) event: Option<OperationEvent>,
    /// What the write's own service routing warned about, empty when the write was not routed.
    pub(crate) warnings: Vec<OperationWarning>,
}

/// Where event notes are written and through what.
pub(crate) struct EventWriter<'a, W> {
    pub(crate) writes: &'a W,
    pub(crate) roots: &'a VaultSet,
    /// The vault's root as a path, which event paths are measured from.
    pub(crate) vault: &'a VaultPath,
}

impl<W: WritesVault<Error = FsError>> EventWriter<'_, W> {
    /// Writes `record`, stepping past any path another event already holds.
    ///
    /// # Errors
    ///
    /// Returns [`EventWriteError::PathsExhausted`] when every candidate path is taken, or the write's own error.
    pub(crate) fn write(&self, record: &EventRecord) -> Result<WrittenEvent, EventWriteError> {
        let markdown = record.to_string();
        for attempt in 0..MAX_PATH_ATTEMPTS {
            let relative = record.relative_path(attempt);
            let path = child_path(self.vault, self.roots, &relative)?;
            let request = WriteMutation {
                path,
                content: markdown.clone().into(),
                expected_hash: None,
                force: false,
                origin: Origin::Internal(EVENT_ORIGIN.to_owned()),
            };
            match self.writes.persist(&request) {
                Ok(result) => {
                    return Ok(WrittenEvent {
                        path: relative,
                        outcome: result.value,
                        event: result.event,
                        warnings: result.warnings,
                    });
                }
                Err(FsError::ExpectedHashRequired { .. }) => {}
                Err(other) => return Err(EventWriteError::Write(ToolError::from(other))),
            }
        }
        Err(EventWriteError::PathsExhausted)
    }
}

#[cfg(test)]
#[path = "events_writer_test.rs"]
mod tests;
