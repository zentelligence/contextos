//! The one place a vault's health is remembered. Every surface that shows it (the header icon, the footer chip, and
//! the Vault Health pane) reads through this cache, so they cannot disagree and a page full of indicators costs one
//! `doctor` call, not one each. `doctor` scans the whole vault, so it is not cheap to repeat.
//!
//! An assessment is reused until it is older than the time to live, then assessed again on the next read. There is no
//! background polling. A repair stores the report it returns, so the next read reflects it without another scan.
//!
//! Readers share work. A reader that arrives while an assessment is running waits for it and takes its outcome,
//! success or failure, rather than starting another scan behind it. An explicit refresh is different: it exists to
//! see what the vault looks like now, so it is never answered from an assessment that began before it was asked for,
//! and it waits for the one that did. An assessment that runs longer than the timeout is abandoned and reported as a
//! failure, so a stalled `doctor` cannot hold every reader of the vault indefinitely. A failure is never cached, so
//! an outage is not remembered after it ends.
//!
//! Time comes from `tokio::time`, so tests drive it with paused time rather than sleeping.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::vault_health::HealthReport;

/// Why a vault's health could not be assessed.
#[derive(Clone, Debug, thiserror::Error)]
pub(crate) enum HealthError {
    #[error("the MCP server could not be reached")]
    Unreachable,
    #[error("doctor failed ({code}): {message}")]
    Failed { code: String, message: String },
    #[error("doctor returned a result this build cannot read: {0}")]
    Malformed(String),
    #[error("doctor did not answer within {} seconds", .0.as_secs())]
    TimedOut(Duration),
}

/// Where an assessment comes from. The production source calls the `doctor` MCP tool.
pub(crate) trait HealthSource: Send + Sync {
    fn assess(&self, vault: &str) -> impl Future<Output = Result<HealthReport, HealthError>> + Send;
}

/// An assessment and how long ago it was made.
#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    pub report: Arc<HealthReport>,
    pub age: Duration,
}

struct Cached {
    report: Arc<HealthReport>,
    assessed_at: Instant,
    /// The ordinal of the assessment that produced it, from [`SlotState::started`].
    attempt: u64,
}

/// What one vault's slot holds, behind the async lock that is held for the length of an assessment.
#[derive(Default)]
struct Held {
    cached: Option<Cached>,
    /// The most recent assessment that failed, and its ordinal.
    failure: Option<(u64, HealthError)>,
}

/// One vault's slot. `started` and `finished` count assessments begun and ended, so a reader can tell whether one is
/// running as it arrives, and a refresh can tell whether one began after it was asked for.
#[derive(Default)]
struct SlotState {
    held: Mutex<Held>,
    started: AtomicU64,
    finished: AtomicU64,
}

type Slot = Arc<SlotState>;

/// How a read may be answered from work that already happened.
#[derive(Clone, Copy)]
enum Mode {
    /// From an assessment running as the read arrives, else from a fresh enough one.
    Current { running: Option<u64> },
    /// Only from an assessment that began after this many had begun.
    Refresh { asked_after: u64 },
}

pub(crate) struct HealthCache<S> {
    source: S,
    ttl: Duration,
    timeout: Duration,
    slots: StdMutex<HashMap<String, Slot>>,
}

impl<S: HealthSource> HealthCache<S> {
    pub(crate) fn new(source: S, ttl: Duration, timeout: Duration) -> Self {
        Self {
            source,
            ttl,
            timeout,
            slots: StdMutex::new(HashMap::new()),
        }
    }

    fn slot(&self, vault: &str) -> Slot {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(slots.entry(vault.to_owned()).or_default())
    }

    /// The vault's health, assessed only when there is neither one running nor one younger than the time to live.
    pub(crate) async fn current(&self, vault: &str) -> Result<Snapshot, HealthError> {
        let slot = self.slot(vault);
        let started = slot.started.load(Ordering::SeqCst);
        let running = (started > slot.finished.load(Ordering::SeqCst)).then_some(started);
        self.read(&slot, vault, Mode::Current { running }).await
    }

    /// The vault's health, assessed again now.
    pub(crate) async fn refresh(&self, vault: &str) -> Result<Snapshot, HealthError> {
        let slot = self.slot(vault);
        let asked_after = slot.started.load(Ordering::SeqCst);
        self.read(&slot, vault, Mode::Refresh { asked_after }).await
    }

    /// Records an assessment made elsewhere, such as the one a repair returns, as the vault's current one.
    pub(crate) async fn store(&self, vault: &str, report: HealthReport) {
        let slot = self.slot(vault);
        let mut held = slot.held.lock().await;
        let attempt = slot.started.fetch_add(1, Ordering::SeqCst) + 1;
        slot.finished.fetch_add(1, Ordering::SeqCst);
        held.cached = Some(Cached {
            report: Arc::new(report),
            assessed_at: Instant::now(),
            attempt,
        });
        held.failure = None;
    }

    /// Forgets the vault's assessment, so the next read assesses again.
    pub(crate) async fn invalidate(&self, vault: &str) {
        let slot = self.slot(vault);
        let mut held = slot.held.lock().await;
        held.cached = None;
        held.failure = None;
    }

    async fn read(&self, slot: &Slot, vault: &str, mode: Mode) -> Result<Snapshot, HealthError> {
        let mut held = slot.held.lock().await;
        if let Some(answer) = self.reuse(&held, mode) {
            return answer;
        }
        let attempt = slot.started.fetch_add(1, Ordering::SeqCst) + 1;
        let outcome = match tokio::time::timeout(self.timeout, self.source.assess(vault)).await {
            Ok(outcome) => outcome,
            Err(_) => Err(HealthError::TimedOut(self.timeout)),
        };
        slot.finished.fetch_add(1, Ordering::SeqCst);
        match outcome {
            Ok(report) => {
                let cached = Cached {
                    report: Arc::new(report),
                    assessed_at: Instant::now(),
                    attempt,
                };
                let fresh = snapshot(&cached);
                held.cached = Some(cached);
                held.failure = None;
                Ok(fresh)
            }
            Err(error) => {
                // What was there is no longer known to be true.
                held.cached = None;
                held.failure = Some((attempt, error.clone()));
                Err(error)
            }
        }
    }

    /// The answer work that already happened gives this read, if it gives one.
    fn reuse(&self, held: &Held, mode: Mode) -> Option<Result<Snapshot, HealthError>> {
        match mode {
            Mode::Current { running } => {
                if let Some(running) = running {
                    if let Some(cached) = held.cached.as_ref().filter(|cached| cached.attempt == running) {
                        return Some(Ok(snapshot(cached)));
                    }
                    if let Some((_, error)) = held.failure.as_ref().filter(|(attempt, _)| *attempt == running) {
                        return Some(Err(error.clone()));
                    }
                }
                held.cached
                    .as_ref()
                    .filter(|cached| cached.assessed_at.elapsed() < self.ttl)
                    .map(|cached| Ok(snapshot(cached)))
            }
            Mode::Refresh { asked_after } => {
                if let Some(cached) = held.cached.as_ref().filter(|cached| cached.attempt > asked_after) {
                    return Some(Ok(snapshot(cached)));
                }
                held.failure
                    .as_ref()
                    .filter(|(attempt, _)| *attempt > asked_after)
                    .map(|(_, error)| Err(error.clone()))
            }
        }
    }
}

fn snapshot(cached: &Cached) -> Snapshot {
    Snapshot {
        report: Arc::clone(&cached.report),
        age: cached.assessed_at.elapsed(),
    }
}

#[cfg(test)]
#[path = "vault_health_cache_test.rs"]
mod tests;
