use std::sync::{Arc, Mutex};

use time::macros::datetime;

use super::*;
use crate::vault_runs::{claim_slot, take_claim};

const NOW: OffsetDateTime = datetime!(2026-10-28 03:00:00 UTC);

fn host(claims: &std::path::Path) -> HostContext {
    HostContext {
        attention: crate::job_attention::JobAttentionMemory::default(),
        log_level: None,
        claims_root: Some(claims.to_path_buf()),
        clock: Arc::new(crate::vault_time::VaultTime::from(crate::vault_time::VaultTimeInput {
            clock: Arc::new(contextos_workflows::SystemClock),
            clients: Arc::new(crate::McpClientSet::default()),
        })),
    }
}

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Log {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Log {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Log {
    fn warnings(&self) -> usize {
        let bytes = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        String::from_utf8_lossy(&bytes)
            .lines()
            .filter(|line| line.contains(" WARN ") && line.contains("reconcile"))
            .count()
    }
}

fn capture() -> (Log, tracing::subscriber::DefaultGuard) {
    let log = Log::default();
    let guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(log.clone())
            .with_ansi(false)
            .finish(),
    );
    (log, guard)
}

#[tokio::test]
async fn a_note_written_for_a_vault_is_read_back_as_when_it_was_first_reconciled() -> Result<(), std::io::Error> {
    let claims = tempfile::tempdir()?;
    let host = host(claims.path());
    assert_eq!(read(&host, "work").await, Reconciled::Never);

    write(&host, "work", NOW).await;

    assert_eq!(read(&host, "work").await, Reconciled::Since(NOW));
    Ok(())
}

#[tokio::test]
async fn a_workflow_whose_slug_matches_the_note_can_still_take_its_job_claim_and_keep_the_note()
-> Result<(), Box<dyn std::error::Error>> {
    let claims = tempfile::tempdir()?;
    let host = host(claims.path());
    write(&host, "work", NOW).await;

    let slot = claim_slot(&host, "work", "ops", "job-events-reconciled", "nightly")?;
    let _claim = take_claim(&slot).await.map_err(|error| format!("{error:?}"))?;

    assert_eq!(read(&host, "work").await, Reconciled::Since(NOW));
    Ok(())
}

#[tokio::test]
async fn a_note_can_be_written_after_a_workflow_with_the_same_slug_took_its_claim()
-> Result<(), Box<dyn std::error::Error>> {
    let claims = tempfile::tempdir()?;
    let host = host(claims.path());
    let slot = claim_slot(&host, "work", "ops", "job-events-reconciled", "nightly")?;
    let _claim = take_claim(&slot).await.map_err(|error| format!("{error:?}"))?;

    write(&host, "work", NOW).await;

    assert_eq!(read(&host, "work").await, Reconciled::Since(NOW));
    Ok(())
}

#[tokio::test]
async fn a_note_that_is_not_a_time_is_unusable_and_reported_once_however_many_passes_read_it()
-> Result<(), Box<dyn std::error::Error>> {
    let claims = tempfile::tempdir()?;
    let host = host(claims.path());
    write(&host, "work", NOW).await;
    let path = marker_path(&host, "work").ok_or("no path")?;
    std::fs::write(&path, "garbled\n")?;
    let (log, _guard) = capture();

    for _ in 0..3 {
        assert_eq!(read(&host, "work").await, Reconciled::Unusable);
    }

    assert_eq!(log.warnings(), 1, "reported once, not on every pass");
    Ok(())
}

#[tokio::test]
async fn a_note_that_cannot_be_read_is_unusable_and_reported_once() -> Result<(), Box<dyn std::error::Error>> {
    let claims = tempfile::tempdir()?;
    let host = host(claims.path());
    let path = marker_path(&host, "work").ok_or("no path")?;
    std::fs::create_dir_all(&path)?;
    let (log, _guard) = capture();

    for _ in 0..3 {
        assert_eq!(read(&host, "work").await, Reconciled::Unusable);
    }

    assert_eq!(log.warnings(), 1);
    Ok(())
}

#[tokio::test]
async fn a_note_that_cannot_be_written_is_reported_once_and_the_vault_stays_unreconciled()
-> Result<(), Box<dyn std::error::Error>> {
    let claims = tempfile::tempdir()?;
    let host = host(claims.path());
    let path = marker_path(&host, "work").ok_or("no path")?;
    std::fs::create_dir_all(&path)?;
    let (log, _guard) = capture();

    for _ in 0..3 {
        write(&host, "work", NOW).await;
    }

    assert_eq!(log.warnings(), 1);
    Ok(())
}

#[tokio::test]
async fn a_note_that_reads_again_clears_its_report_so_a_later_fault_is_reported_afresh()
-> Result<(), Box<dyn std::error::Error>> {
    let claims = tempfile::tempdir()?;
    let host = host(claims.path());
    write(&host, "work", NOW).await;
    let path = marker_path(&host, "work").ok_or("no path")?;
    let (log, _guard) = capture();

    std::fs::write(&path, "garbled\n")?;
    assert_eq!(read(&host, "work").await, Reconciled::Unusable);
    std::fs::write(&path, "2026-10-28T03:00:00Z\n")?;
    assert_eq!(read(&host, "work").await, Reconciled::Since(NOW));
    std::fs::write(&path, "garbled again\n")?;
    assert_eq!(read(&host, "work").await, Reconciled::Unusable);

    assert_eq!(log.warnings(), 2);
    Ok(())
}
