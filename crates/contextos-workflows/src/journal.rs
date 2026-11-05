//! A journal of the calls a run makes, kept by wrapping the run's caller.
//!
//! Each call is reported when it ends, with its server, tool, outcome, and duration. Arguments and results never reach
//! the journal. A call whose future is dropped before it answers, which is what cancelling a run does, is reported as
//! abandoned in flight, because whether the server acted on it is not known.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::caller::{McpCallFailure, McpCaller};
use crate::clock::Clock;
use crate::run_record::{CallEntry, CallOutcome};

/// Receives each call as it ends. An implementation persists it, so a run that dies leaves the calls it completed.
///
/// It is called on the thread that is running the script, which is a blocking thread, and from the drop of a call that
/// was abandoned. An implementation may therefore do blocking file work, and must not be called from an async task.
pub trait CallJournal: Send + Sync {
    fn call_finished(&self, entry: CallEntry);
}

/// A caller that reports every call it forwards to a [`CallJournal`].
pub struct JournalledCaller {
    inner: Arc<dyn McpCaller>,
    journal: Arc<dyn CallJournal>,
    clock: Arc<dyn Clock>,
}

impl JournalledCaller {
    #[must_use]
    pub fn new(inner: Arc<dyn McpCaller>, journal: Arc<dyn CallJournal>, clock: Arc<dyn Clock>) -> Self {
        Self { inner, journal, clock }
    }
}

impl McpCaller for JournalledCaller {
    fn call<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        arguments: Map<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpCallFailure>> + Send + 'a>> {
        Box::pin(async move {
            let mut pending = Pending {
                journal: Arc::clone(&self.journal),
                clock: Arc::clone(&self.clock),
                server: server.to_owned(),
                tool: tool.to_owned(),
                started: self.clock.now(),
                reported: false,
            };
            let answer = self.inner.call(server, tool, arguments).await;
            pending.report(match &answer {
                Ok(_) => CallOutcome::Succeeded,
                Err(failure) => CallOutcome::Failed(failure.kind),
            });
            answer
        })
    }
}

/// A call that has been issued and not yet reported. Dropping it unreported reports the call as abandoned in flight.
struct Pending {
    journal: Arc<dyn CallJournal>,
    clock: Arc<dyn Clock>,
    server: String,
    tool: String,
    started: time::OffsetDateTime,
    reported: bool,
}

impl Pending {
    fn report(&mut self, outcome: CallOutcome) {
        self.reported = true;
        let elapsed = (self.clock.now() - self.started).whole_milliseconds();
        self.journal.call_finished(CallEntry {
            server: std::mem::take(&mut self.server),
            tool: std::mem::take(&mut self.tool),
            outcome,
            duration_ms: u64::try_from(elapsed).unwrap_or(0),
        });
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.reported {
            self.report(CallOutcome::AbandonedInFlight);
        }
    }
}

#[cfg(test)]
#[path = "journal_test.rs"]
mod tests;
