use std::sync::Mutex;

use serde_json::{Map, Value, json};
use time::macros::datetime;

use super::*;
use crate::caller::WorkflowFailureKind;

#[derive(Default)]
struct Entries(Mutex<Vec<CallEntry>>);

impl CallJournal for Entries {
    fn call_finished(&self, entry: CallEntry) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(entry);
    }
}

impl Entries {
    fn taken(&self) -> Vec<CallEntry> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

/// Moves forward by a fixed step on every read, so a duration is exact without a sleep.
struct SteppingClock {
    now: Mutex<time::OffsetDateTime>,
}

impl Clock for SteppingClock {
    fn now(&self) -> time::OffsetDateTime {
        let mut now = self.now.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *now += time::Duration::milliseconds(250);
        *now
    }
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(SteppingClock {
        now: Mutex::new(datetime!(2026-10-01 03:00:00 UTC)),
    })
}

struct Scripted(Result<Value, WorkflowFailureKind>);

impl McpCaller for Scripted {
    fn call<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        _arguments: Map<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpCallFailure>> + Send + 'a>> {
        let answer = self.0.clone().map_err(|kind| McpCallFailure {
            kind,
            server: server.to_owned(),
            tool: tool.to_owned(),
            message: "scripted".to_owned(),
        });
        Box::pin(async move { answer })
    }
}

struct NeverAnswers;

impl McpCaller for NeverAnswers {
    fn call<'a>(
        &'a self,
        _server: &'a str,
        _tool: &'a str,
        _arguments: Map<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpCallFailure>> + Send + 'a>> {
        Box::pin(std::future::pending())
    }
}

fn journalled(inner: impl McpCaller + 'static, entries: &Arc<Entries>) -> JournalledCaller {
    JournalledCaller::new(Arc::new(inner), Arc::clone(entries) as Arc<dyn CallJournal>, clock())
}

#[tokio::test]
async fn a_successful_call_is_returned_unchanged_and_journalled_with_its_duration() {
    let entries = Arc::new(Entries::default());
    let caller = journalled(Scripted(Ok(json!({"vaults": 2}))), &entries);

    let answer = caller.call("contextos", "vault_info", Map::new()).await;

    assert_eq!(answer.ok(), Some(json!({"vaults": 2})));
    assert_eq!(
        entries.taken(),
        vec![CallEntry {
            server: "contextos".to_owned(),
            tool: "vault_info".to_owned(),
            outcome: CallOutcome::Succeeded,
            duration_ms: 250,
        }]
    );
}

#[tokio::test]
async fn a_failed_call_is_journalled_with_its_failure_kind_and_still_fails_the_caller() {
    let entries = Arc::new(Entries::default());
    let caller = journalled(Scripted(Err(WorkflowFailureKind::ServerUnavailable)), &entries);

    let answer = caller.call("contextos", "vault_info", Map::new()).await;

    assert_eq!(
        answer.err().map(|failure| failure.kind),
        Some(WorkflowFailureKind::ServerUnavailable)
    );
    assert_eq!(
        entries.taken().first().map(|entry| entry.outcome),
        Some(CallOutcome::Failed(WorkflowFailureKind::ServerUnavailable))
    );
}

#[tokio::test]
async fn a_call_whose_future_is_dropped_is_journalled_as_abandoned_in_flight() {
    let entries = Arc::new(Entries::default());
    let caller = journalled(NeverAnswers, &entries);

    let abandoned = tokio::time::timeout(
        std::time::Duration::from_millis(20),
        caller.call("contextos", "git_commit", Map::new()),
    )
    .await;

    assert!(abandoned.is_err(), "the call never answers, so the timeout abandons it");
    let taken = entries.taken();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].tool, "git_commit");
    assert_eq!(taken[0].outcome, CallOutcome::AbandonedInFlight);
}

#[tokio::test]
async fn calls_are_journalled_in_the_order_they_finished() {
    let entries = Arc::new(Entries::default());
    let caller = journalled(Scripted(Ok(json!({}))), &entries);

    for tool in ["one", "two", "three"] {
        let _ = caller.call("contextos", tool, Map::new()).await;
    }

    let tools: Vec<_> = entries.taken().into_iter().map(|entry| entry.tool).collect();
    assert_eq!(tools, ["one", "two", "three"]);
}
