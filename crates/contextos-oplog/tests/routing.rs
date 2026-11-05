//! An operation-log entry the caller asked for, and buffered entries retried later, are written by the log on its
//! own account, so each write is handed to the supplied router: none can be left for Git to miss.

use std::sync::{Arc, Mutex};

use contextos_core::{
    AppendMutation, AppendOutcome, AppendsVault, OpKind, OperationEvent, OperationWarning, Origin, PipelineResult,
    RoutesOperations, VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet,
};
use contextos_oplog::{ManualLogInput, OperationLog, OperationLogConfig};
use tempfile::tempdir;
use thiserror::Error;
use time::macros::datetime;

type BoxError = Box<dyn std::error::Error>;

#[derive(Clone, Debug)]
struct FailsThenSucceeds {
    failures_remaining: Arc<Mutex<usize>>,
}

#[derive(Debug, Error)]
#[error("injected append failure")]
struct InjectedFailure;

impl AppendsVault for FailsThenSucceeds {
    type Error = InjectedFailure;

    fn append(&self, request: &AppendMutation) -> Result<PipelineResult<AppendOutcome>, Self::Error> {
        let mut remaining = self.failures_remaining.lock().map_err(|_| InjectedFailure)?;
        if *remaining > 0 {
            *remaining -= 1;
            return Err(InjectedFailure);
        }
        Ok(PipelineResult {
            value: AppendOutcome {
                path: request.path.clone(),
                bytes_appended: request.content.len(),
                created: false,
            },
            event: Some(OperationEvent {
                kind: OpKind::Modify,
                paths: vec![request.path.clone()],
                origin: request.origin.clone(),
                summary: "Appended operation log".to_owned(),
                at: datetime!(2026-07-18 18:30:00 +10:00),
            }),
            warnings: Vec::new(),
        })
    }
}

#[derive(Clone, Debug, Default)]
struct RecordingRouter {
    routed: Arc<Mutex<Vec<OperationEvent>>>,
    warn: bool,
}

impl RoutesOperations for RecordingRouter {
    fn route(&self, event: &OperationEvent) -> Vec<OperationWarning> {
        if let Ok(mut routed) = self.routed.lock() {
            routed.push(event.clone());
        }
        if self.warn {
            vec![OperationWarning {
                code: "git/stage".to_owned(),
                message: "staging is degraded".to_owned(),
            }]
        } else {
            Vec::new()
        }
    }
}

struct Fixture {
    _vault: tempfile::TempDir,
    log: OperationLog<FailsThenSucceeds>,
    note: VaultPath,
}

fn fixture(failures: usize) -> Result<Fixture, BoxError> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "note")?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![root.clone()])?;
    let note = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "note.md",
    })?;
    let log = OperationLog::try_from(OperationLogConfig {
        root,
        roots,
        relative_directory: "memory/log".to_owned(),
        appender: FailsThenSucceeds {
            failures_remaining: Arc::new(Mutex::new(failures)),
        },
    })?;
    Ok(Fixture {
        _vault: vault,
        log,
        note,
    })
}

fn manual(note: &VaultPath) -> ManualLogInput {
    ManualLogInput {
        entry: "Reviewed the note".to_owned(),
        files: vec![note.clone()],
        at: datetime!(2026-07-18 18:30:00 +10:00),
        requested_by: "vault_log_append".to_owned(),
    }
}

#[test]
fn a_manual_entry_is_routed_before_the_call_returns() -> Result<(), BoxError> {
    let fixture = fixture(0)?;
    let router = RecordingRouter::default();

    let outcome = fixture.log.append_manual(&manual(&fixture.note), &router)?;

    let seen = router.routed.lock().map_err(|_| "router poisoned")?.clone();
    assert_eq!(outcome.events.len(), 1);
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].origin,
        Origin::Internal("vault_log_append".to_owned()),
        "the write is attributed to the tool that asked for it, so history names it"
    );
    assert!(
        outcome.warnings.is_empty(),
        "`outcome.warnings` should be empty, got {:?}",
        outcome.warnings
    );
    Ok(())
}

#[test]
fn routing_warnings_for_a_manual_entry_are_returned_to_the_caller() -> Result<(), BoxError> {
    let fixture = fixture(0)?;
    let router = RecordingRouter {
        warn: true,
        ..RecordingRouter::default()
    };

    let outcome = fixture.log.append_manual(&manual(&fixture.note), &router)?;

    assert_eq!(outcome.warnings.len(), 1);
    assert_eq!(outcome.warnings[0].code, "git/stage");
    Ok(())
}

#[test]
fn buffered_entries_recovered_by_a_flush_are_routed() -> Result<(), BoxError> {
    let fixture = fixture(1)?;
    let router = RecordingRouter::default();
    assert!(fixture.log.append_manual(&manual(&fixture.note), &router).is_err());
    assert!(router.routed.lock().map_err(|_| "router poisoned")?.is_empty());

    let outcome = fixture.log.flush(&router)?;

    assert_eq!(outcome.events.len(), 1);
    assert_eq!(router.routed.lock().map_err(|_| "router poisoned")?.len(), 1);
    Ok(())
}

/// Appends succeed or fail as the test scripts, one entry per append call; once the script runs out, appends succeed.
#[derive(Clone, Debug)]
struct ScriptedAppender {
    script: Arc<Mutex<std::collections::VecDeque<bool>>>,
}

impl AppendsVault for ScriptedAppender {
    type Error = InjectedFailure;

    fn append(&self, request: &AppendMutation) -> Result<PipelineResult<AppendOutcome>, Self::Error> {
        let succeeds = self
            .script
            .lock()
            .map_err(|_| InjectedFailure)?
            .pop_front()
            .unwrap_or(true);
        if !succeeds {
            return Err(InjectedFailure);
        }
        Ok(PipelineResult {
            value: AppendOutcome {
                path: request.path.clone(),
                bytes_appended: request.content.len(),
                created: false,
            },
            event: Some(OperationEvent {
                kind: OpKind::Modify,
                paths: vec![request.path.clone()],
                origin: request.origin.clone(),
                summary: request.content.clone(),
                at: datetime!(2026-07-18 18:30:00 +10:00),
            }),
            warnings: Vec::new(),
        })
    }
}

struct ScriptedFixture {
    _vault: tempfile::TempDir,
    log: OperationLog<ScriptedAppender>,
    script: Arc<Mutex<std::collections::VecDeque<bool>>>,
    note: VaultPath,
}

fn scripted_fixture() -> Result<ScriptedFixture, BoxError> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "note")?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![root.clone()])?;
    let note = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "note.md",
    })?;
    let script = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let log = OperationLog::try_from(OperationLogConfig {
        root,
        roots,
        relative_directory: "memory/log".to_owned(),
        appender: ScriptedAppender {
            script: Arc::clone(&script),
        },
    })?;
    Ok(ScriptedFixture {
        _vault: vault,
        log,
        script,
        note,
    })
}

impl ScriptedFixture {
    fn script(&self, steps: &[bool]) -> Result<(), BoxError> {
        *self.script.lock().map_err(|_| "script poisoned")? = steps.iter().copied().collect();
        Ok(())
    }

    fn entry(&self, text: &str) -> ManualLogInput {
        ManualLogInput {
            entry: text.to_owned(),
            files: vec![self.note.clone()],
            at: datetime!(2026-07-18 18:30:00 +10:00),
            requested_by: "vault_log_append".to_owned(),
        }
    }
}

#[test]
fn entries_written_before_a_later_buffered_entry_fails_are_still_routed_exactly_once() -> Result<(), BoxError> {
    let fixture = scripted_fixture()?;
    let router = RecordingRouter::default();
    fixture.script(&[false, false, false])?;
    for text in ["first", "second", "third"] {
        assert!(fixture.log.append_manual(&fixture.entry(text), &router).is_err());
    }
    assert!(router.routed.lock().map_err(|_| "router poisoned")?.is_empty());

    // The first two buffered entries now write; the third fails again.
    fixture.script(&[true, true, false])?;
    assert!(fixture.log.flush(&router).is_err());
    assert_eq!(
        router.routed.lock().map_err(|_| "router poisoned")?.len(),
        2,
        "the two entries that were written are routed even though the flush failed"
    );

    fixture.script(&[])?;
    let recovered = fixture.log.flush(&router)?;
    assert_eq!(
        recovered.events.len(),
        1,
        "only the entry that had not been written before"
    );
    assert_eq!(router.routed.lock().map_err(|_| "router poisoned")?.len(), 3);
    Ok(())
}

#[test]
fn events_stranded_by_a_failed_router_driven_append_are_delivered_with_the_next_one() -> Result<(), BoxError> {
    let fixture = scripted_fixture()?;
    let router = RecordingRouter::default();
    fixture.script(&[false, false])?;
    for text in ["first", "second"] {
        assert!(fixture.log.append_manual(&fixture.entry(text), &router).is_err());
    }
    fixture.script(&[true, false])?;
    let third = OperationEvent {
        kind: OpKind::Modify,
        paths: vec![fixture.note.clone()],
        origin: Origin::Tool("fs_write_file".to_owned()),
        summary: "Third".to_owned(),
        at: datetime!(2026-07-18 18:31:00 +10:00),
    };
    assert!(contextos_core::LogsOperations::append(&fixture.log, &third).is_err());

    fixture.script(&[])?;
    let fourth = OperationEvent {
        summary: "Fourth".to_owned(),
        ..third
    };
    let delivered = contextos_core::LogsOperations::append(&fixture.log, &fourth).map_err(|warning| warning.message)?;

    assert_eq!(
        delivered.len(),
        4,
        "the entry written before the failure, and the three that followed"
    );
    Ok(())
}
