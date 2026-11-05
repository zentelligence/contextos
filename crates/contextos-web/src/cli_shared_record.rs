//! The record a shared command-line run leaves on the host: starting it while the claim is held (or, for a dry run,
//! with none), noting where it is, and ending it, sweeping old records, and releasing the claim.
//!
//! A record that cannot be written never stops a run. It is logged, reported as a host warning, and the run goes ahead.

use std::sync::Arc;

use contextos_workflows::{PackageIdentity, RunResult, RunTrigger};

use crate::cli_shared::{CliSharedError, SharedHost};
use crate::run_summary::{HostRecord, HostWarning, RecordLocation};
use crate::shared_claim::{SharedClaim, SharedJobKey};
use crate::shared_ledger::{RunRecorder, RunStart, SharedLedger};

/// What became of the run's record.
#[derive(Default)]
pub(crate) struct Recording {
    pub(crate) run_id: Option<u64>,
    pub(crate) record: Option<RecordLocation>,
    pub(crate) host_warnings: Vec<HostWarning>,
}

impl Recording {
    pub(crate) fn warn(&mut self, message: &str) {
        self.host_warnings.push(HostWarning {
            code: crate::run_summary::codes::RECORD_UNAVAILABLE.to_owned(),
            message: message.to_owned(),
        });
    }
}

/// What ending a run's record needs: the ledger, the claim to release, and the recorder.
pub(crate) struct Ended<'a> {
    pub(crate) ledger: &'a Arc<SharedLedger>,
    pub(crate) key: &'a SharedJobKey,
    pub(crate) claim: Option<SharedClaim>,
    pub(crate) recorder: Option<Arc<RunRecorder>>,
    pub(crate) recording: Recording,
    pub(crate) dry: bool,
}

/// Ends the run's record and releases its claim, and says in the returned [`Recording`] what became of the record.
pub(crate) async fn end_recording(ended: Ended<'_>, result: &RunResult) -> Recording {
    let Ended {
        ledger,
        key,
        claim,
        recorder,
        mut recording,
        dry,
    } = ended;
    let state = if let Some(claim) = claim {
        let (claim, state) = end_record(ledger, claim, recorder, result).await;
        if let Some(claim) = claim
            && let Err(error) = claim.release().await
        {
            tracing::warn!(job = %key.job, %error, "the claim could not be released; closing its file releases it regardless");
        }
        state
    } else {
        end_dry_record(recorder, result, ledger).await
    };
    if state == Recorded::NotWritten {
        recording.warn("the run's record could not be finished");
        // A dry run's record is written once, when it ends, so there is no file to point at.
        if dry {
            recording.record = None;
        }
    }
    recording
}

/// What starting a run's record leaves: the claim, which comes back with it, the recorder, and what is to be reported.
pub(crate) struct Started {
    pub(crate) claim: Option<SharedClaim>,
    pub(crate) recorder: Option<Arc<RunRecorder>>,
    pub(crate) recording: Recording,
}

/// Starts the run's record, with the claim held for a live run and none for a dry one, and notes where it is.
pub(crate) async fn start_recording(
    host: &SharedHost<'_>,
    key: &SharedJobKey,
    ledger: &Arc<SharedLedger>,
    claim: Option<SharedClaim>,
    begun: Begin,
) -> Result<Started, CliSharedError> {
    let (claim, recorder) = if let Some(claim) = claim {
        let (claim, recorder) = start_record(ledger, claim, begun).await?;
        (Some(claim), recorder)
    } else {
        (None, start_dry_record(ledger, key, begun).await)
    };
    let mut recording = Recording::default();
    match &recorder {
        Some(recorder) => {
            recording.run_id = Some(recorder.run_id());
            recording.record = RecordLocation::try_from(HostRecord {
                runs_root: host.runs_root,
                record: recorder.path(),
            })
            .ok();
        }
        None => recording.warn("the run could not be recorded"),
    }
    Ok(Started {
        claim,
        recorder,
        recording,
    })
}

/// Whether a record ended up written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Recorded {
    Written,
    /// The record was begun and could not be written.
    NotWritten,
    /// No record was begun, which was already reported when it could not be started.
    Absent,
}

/// What the ledger needs to begin a record.
pub(crate) struct Begin {
    pub(crate) identity: PackageIdentity,
    pub(crate) libraries: Vec<PackageIdentity>,
    pub(crate) inputs: Vec<String>,
    pub(crate) overridden: Vec<String>,
    pub(crate) trigger: RunTrigger,
}

/// Starts the run's record while the claim is held. The claim comes back with it, because it must outlive the record
/// work. A record that cannot be started is logged, and the run goes ahead without one.
///
/// # Errors
///
/// Returns [`CliSharedError::Interrupted`] when the blocking task panicked, which also dropped, and so released, the
/// claim.
async fn start_record(
    ledger: &Arc<SharedLedger>,
    claim: SharedClaim,
    begun: Begin,
) -> Result<(SharedClaim, Option<Arc<RunRecorder>>), CliSharedError> {
    let ledger = Arc::clone(ledger);
    let started = tokio::task::spawn_blocking(move || {
        let recorder = ledger.begin(
            &claim,
            RunStart {
                identity: &begun.identity,
                libraries: &begun.libraries,
                inputs: begun.inputs,
                trigger: begun.trigger,
            },
        );
        if let Ok(recorder) = &recorder
            && !begun.overridden.is_empty()
        {
            recorder.name_overrides(begun.overridden);
        }
        (claim, recorder)
    })
    .await;
    match started? {
        (claim, Ok(recorder)) => Ok((claim, Some(Arc::new(recorder)))),
        (claim, Err(error)) => {
            tracing::warn!(%error, "the run could not be recorded, and goes ahead without a record");
            Ok((claim, None))
        }
    }
}

/// Starts the record of a dry run, which holds no claim. A record that cannot be started is logged, and the run goes
/// ahead without one.
async fn start_dry_record(ledger: &Arc<SharedLedger>, key: &SharedJobKey, begun: Begin) -> Option<Arc<RunRecorder>> {
    let ledger = Arc::clone(ledger);
    let key = key.clone();
    let started = tokio::task::spawn_blocking(move || {
        let recorder = ledger.begin_dry(
            &key,
            RunStart {
                identity: &begun.identity,
                libraries: &begun.libraries,
                inputs: begun.inputs,
                trigger: begun.trigger,
            },
        );
        if let Ok(recorder) = &recorder
            && !begun.overridden.is_empty()
        {
            recorder.name_overrides(begun.overridden);
        }
        recorder
    })
    .await;
    match started {
        Ok(Ok(recorder)) => Some(Arc::new(recorder)),
        Ok(Err(error)) => {
            tracing::warn!(%error, "the dry run could not be recorded, and goes ahead without a record");
            None
        }
        Err(error) => {
            tracing::warn!(%error, "the dry run could not be recorded, and goes ahead without a record");
            None
        }
    }
}

/// Ends a dry run's record. It sweeps nothing, because a sweep is the claim holder's, and a failure to write it cannot
/// change the run's outcome.
async fn end_dry_record(
    recorder: Option<Arc<RunRecorder>>,
    result: &RunResult,
    ledger: &Arc<SharedLedger>,
) -> Recorded {
    let Some(recorder) = recorder else {
        return Recorded::Absent;
    };
    let outcome = result.outcome.clone();
    let names = result.output_names.clone();
    let completed = ledger.now();
    let ended = tokio::task::spawn_blocking(move || {
        recorder.name_outputs(names);
        recorder.finish(&outcome, completed)
    })
    .await;
    match ended {
        Ok(Ok(())) => Recorded::Written,
        Ok(Err(error)) => {
            tracing::warn!(error = %error.kind(), "the dry run's record could not be written");
            Recorded::NotWritten
        }
        Err(error) => {
            tracing::warn!(%error, "the dry run's record could not be written");
            Recorded::NotWritten
        }
    }
}

/// Ends the run's record and sweeps the job's old ones, both while the claim is still held, and hands the claim back
/// to be released. Neither can change the run's outcome. Returns no claim when the blocking task panicked, which dropped,
/// and so released, the claim.
async fn end_record(
    ledger: &Arc<SharedLedger>,
    claim: SharedClaim,
    recorder: Option<Arc<RunRecorder>>,
    result: &RunResult,
) -> (Option<SharedClaim>, Recorded) {
    let Some(recorder) = recorder else {
        return (Some(claim), Recorded::Absent);
    };
    let ledger = Arc::clone(ledger);
    let outcome = result.outcome.clone();
    let names = result.output_names.clone();
    let ended = tokio::task::spawn_blocking(move || {
        recorder.name_outputs(names);
        let state = match recorder.finish(&outcome, ledger.now()) {
            Ok(()) => Recorded::Written,
            Err(error) => {
                tracing::warn!(error = %error.kind(), "the run's record could not be finished");
                Recorded::NotWritten
            }
        };
        if let Err(error) = ledger.sweep(&claim, recorder.path()) {
            tracing::warn!(%error, "the job's old run records could not be swept");
        }
        (claim, state)
    })
    .await;
    match ended {
        Ok((claim, state)) => (Some(claim), state),
        Err(_) => (None, Recorded::NotWritten),
    }
}
