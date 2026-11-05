//! An in-memory task system for tests of the raising rules: it records every call and answers with what the test sets.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use crate::task_sink::{ExistingTask, RaiseOutcome, RaisedTask, TaskSink};

/// A task system that answers every create with one outcome and lists the tasks it was given.
pub(crate) struct FakeTaskSink {
    pub outcome: Mutex<RaiseOutcome>,
    pub existing: Mutex<Vec<ExistingTask>>,
    pub created: Mutex<Vec<RaisedTask>>,
    /// The codes the next listings fail with, one per listing, oldest first. Once spent, listings succeed again.
    pub listing_failures: Mutex<Vec<String>>,
}

impl FakeTaskSink {
    pub(crate) fn answering(outcome: RaiseOutcome) -> Self {
        Self {
            outcome: Mutex::new(outcome),
            existing: Mutex::new(Vec::new()),
            created: Mutex::new(Vec::new()),
            listing_failures: Mutex::new(Vec::new()),
        }
    }

    /// Makes the next listing fail with `code`, and the ones after it succeed.
    pub(crate) fn fail_next_listing(&self, code: &str) {
        if let Ok(mut failures) = self.listing_failures.lock() {
            failures.push(code.to_owned());
        }
    }

    /// How many creates were attempted.
    pub(crate) fn calls(&self) -> usize {
        self.created.lock().map_or(0, |created| created.len())
    }

    pub(crate) fn answer(&self, outcome: RaiseOutcome) {
        if let Ok(mut current) = self.outcome.lock() {
            *current = outcome;
        }
    }
}

impl TaskSink for FakeTaskSink {
    fn create<'a>(&'a self, task: &'a RaisedTask) -> Pin<Box<dyn Future<Output = RaiseOutcome> + Send + 'a>> {
        Box::pin(async move {
            if let Ok(mut created) = self.created.lock() {
                created.push(task.clone());
            }
            self.outcome.lock().map_or_else(
                |_| RaiseOutcome::Failed("poisoned".to_owned()),
                |outcome| outcome.clone(),
            )
        })
    }

    fn tasks_with_origin_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ExistingTask>, String>> + Send + 'a>> {
        Box::pin(async move {
            let failure = self
                .listing_failures
                .lock()
                .ok()
                .and_then(|mut failures| (!failures.is_empty()).then(|| failures.remove(0)));
            if let Some(code) = failure {
                return Err(code);
            }
            Ok(self.existing.lock().map_or_else(
                |_| Vec::new(),
                |existing| {
                    existing
                        .iter()
                        .filter(|task| task.origin.starts_with(prefix))
                        .cloned()
                        .collect()
                },
            ))
        })
    }
}
