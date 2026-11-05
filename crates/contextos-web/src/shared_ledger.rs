//! The host's record of shared runs.
//!
//! One Markdown note per run at `{runs root}/{package}/{workflow}/{job}/{YYYY}/{MM}/R-{n}.md`. A run's own record is
//! written while the job's claim is held, so nothing races another run of the same job. A scheduled tick that finds the
//! claim held cannot take it, and leaves a finished `skipped` record without it, so run numbers are taken under a short
//! lock of their own that the claim holder and a skipped tick both use. Run numbers are per job and kept in
//! `next-run-id` beside the records, advanced before the record is written so a crash leaves a gap and never a reused
//! number. Nothing is written to a vault.
//!
//! A record that cannot be written never fails the run: the caller logs it and carries on without one.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use contextos_workflows::{
    CallEntry, CallJournal, Clock, MAX_ERROR_BYTES, PackageIdentity, RecordedLibrary, RunHeader, RunOutcome, RunRecord,
    RunStatus, RunTrigger, cut_to_bytes, mark_interrupted, parse_run_id,
};
use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::atomic_write::write_atomically;
use crate::bounded_read::read_text_within;
use crate::shared_claim::{SharedClaim, SharedJobKey, is_plain_segment};
use crate::trailing_path::Trailing;

/// The largest record the ledger will read back. A record lists at most `MAX_RECORDED_CALLS` calls, so this is far
/// above any record it wrote.
const MAX_RECORD_BYTES: u64 = 1024 * 1024;

/// How many trailing components of a record's path a log event shows: the job, year, month, and file.
const RECORD_PATH_TAIL: usize = 4;

const COUNTER_FILE: &str = "next-run-id";

/// The lock a run number is taken under, beside the counter. It is held for the length of one read and write.
const COUNTER_LOCK_FILE: &str = "next-run-id.lock";

/// How much of an unreadable counter an error repeats.
const MAX_COUNTER_ECHO_BYTES: usize = 32;

/// Where the host keeps shared run records, and how long it keeps them.
pub struct SharedLedger {
    runs_root: PathBuf,
    clock: Arc<dyn Clock>,
    retention: Duration,
}

/// What a run being recorded is.
pub struct RunStart<'a> {
    pub identity: &'a PackageIdentity,
    /// The library releases the run resolved, in name order.
    pub libraries: &'a [PackageIdentity],
    /// The names of the run's inputs, never their values.
    pub inputs: Vec<String>,
    /// What started the run.
    pub trigger: RunTrigger,
}

/// A scheduled tick that was not run, because the job's previous run still held its claim.
pub struct SkippedRun<'a> {
    pub identity: &'a PackageIdentity,
    /// The names of the inputs the run would have had, never their values.
    pub inputs: Vec<String>,
    pub reason: &'a str,
}

#[derive(Debug, Error)]
pub enum LedgerError {
    /// The cause is shown by its kind alone: a wrapped filesystem failure names the absolute folder it was raised in.
    #[error("the run ledger could not be written ({kind})", kind = .0.kind())]
    Io(#[from] io::Error),
    #[error("{name:?} cannot name a shared job component: it must be a single plain path segment")]
    InvalidName { name: String },
    #[error(
        "{path} does not hold a run number ({value:?}) and is not repaired; delete it to have the next run number \
         taken from the records already there",
        path = Trailing { path, count: 2 }
    )]
    Counter { path: PathBuf, value: String },
}

/// What a retention sweep did.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct SweepReport {
    pub deleted: usize,
    /// Records that could not be read or deleted, and so were left.
    pub skipped: usize,
}

/// The record of one run in progress. Reports each call to disk as it ends, so a run that dies keeps what it did.
pub struct RunRecorder {
    path: PathBuf,
    record: Mutex<RunRecord>,
    /// Whether each call is written to disk as it ends. A dry run writes its record once, when it ends.
    progress: bool,
}

impl SharedLedger {
    #[must_use]
    pub fn new(runs_root: PathBuf, clock: Arc<dyn Clock>, retention: Duration) -> Self {
        Self {
            runs_root,
            clock,
            retention,
        }
    }

    /// The ledger's time source.
    #[must_use]
    pub fn now(&self) -> OffsetDateTime {
        self.clock.now()
    }

    /// Starts the record of a run of the job `claim` is held on.
    ///
    /// In order: any record of the job still `running` is marked `interrupted`, because holding the claim proves the
    /// process that wrote it is gone; the next run number is taken; and the `running` record is written.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError::Counter`] when the job's counter is not a number, and [`LedgerError::Io`] when a file
    /// cannot be written. Nothing is recorded in either case.
    pub fn begin(&self, claim: &SharedClaim, start: RunStart<'_>) -> Result<RunRecorder, LedgerError> {
        let key = claim.key();
        let job_dir = self.job_dir(key);
        fs::create_dir_all(&job_dir)?;
        let now = self.clock.now();
        Self::interrupt_dead_runs(&job_dir, now);
        let run_id = take_run_number(&job_dir)?;
        let record = RunRecord {
            run_id,
            workflow: key.workflow.clone(),
            job: key.job.clone(),
            package: start.identity.name.to_string(),
            version: start.identity.version.clone(),
            digest: start.identity.digest.as_str().to_owned(),
            libraries: start.libraries.iter().map(RecordedLibrary::from).collect(),
            trigger: start.trigger,
            dry_run: false,
            inputs: start.inputs,
            overrides: Vec::new(),
            outputs: Vec::new(),
            status: RunStatus::Running,
            started: now,
            completed: None,
            error: None,
            calls: Vec::new(),
            calls_omitted: 0,
        };
        let directory = job_dir
            .join(format!("{:04}", now.year()))
            .join(format!("{:02}", u8::from(now.month())));
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("R-{run_id}.md"));
        write_atomically(&path, String::from(&record).as_bytes())?;
        Ok(RunRecorder {
            path,
            record: Mutex::new(record),
            progress: true,
        })
    }

    /// Starts the record of a dry run of the job `key`, which holds no claim, so a live run of the job is never held back.
    ///
    /// It takes a run number under the counter lock, as a run beginning does, so its number is never one another record
    /// has. It reads and changes no other record: a record of the job that is still `running` belongs to a live run it
    /// cannot tell from a dead one without the claim, so it is left. Nothing is written until the record ends, so a dry
    /// run that dies leaves no `running` record for a later run to find, and its calls are kept in memory as they end.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError::InvalidName`] when a component of `key` could leave the runs folder,
    /// [`LedgerError::Counter`] when the job's counter is not a number, and [`LedgerError::Io`] when the job's folder
    /// cannot be created. Nothing is recorded in any of those cases.
    pub fn begin_dry(&self, key: &SharedJobKey, start: RunStart<'_>) -> Result<RunRecorder, LedgerError> {
        for name in [&key.package, &key.workflow, &key.job] {
            if !is_plain_segment(name) {
                return Err(LedgerError::InvalidName { name: name.clone() });
            }
        }
        let job_dir = self.job_dir(key);
        fs::create_dir_all(&job_dir)?;
        let now = self.clock.now();
        let run_id = take_run_number(&job_dir)?;
        let record = RunRecord {
            run_id,
            workflow: key.workflow.clone(),
            job: key.job.clone(),
            package: start.identity.name.to_string(),
            version: start.identity.version.clone(),
            digest: start.identity.digest.as_str().to_owned(),
            libraries: start.libraries.iter().map(RecordedLibrary::from).collect(),
            trigger: start.trigger,
            dry_run: true,
            inputs: start.inputs,
            overrides: Vec::new(),
            outputs: Vec::new(),
            status: RunStatus::Running,
            started: now,
            completed: None,
            error: None,
            calls: Vec::new(),
            calls_omitted: 0,
        };
        let path = job_dir
            .join(format!("{:04}", now.year()))
            .join(format!("{:02}", u8::from(now.month())))
            .join(format!("R-{run_id}.md"));
        Ok(RunRecorder {
            path,
            record: Mutex::new(record),
            progress: false,
        })
    }

    /// Leaves a finished `skipped` record for a scheduled tick that was not run, because another run holds the job's claim.
    ///
    /// It takes no claim: it takes a run number under the counter lock, which a run beginning takes too, so the number is
    /// never one a running record has. It reads and changes no other record, so the run that holds the claim is
    /// unaffected.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError::InvalidName`] when a component of `key` could leave the runs folder,
    /// [`LedgerError::Counter`] when the job's counter is not a number, and [`LedgerError::Io`] when a file cannot be
    /// written. Nothing is recorded in any of those cases.
    pub fn record_skipped(&self, key: &SharedJobKey, skipped: SkippedRun<'_>) -> Result<PathBuf, LedgerError> {
        for name in [&key.package, &key.workflow, &key.job] {
            if !is_plain_segment(name) {
                return Err(LedgerError::InvalidName { name: name.clone() });
            }
        }
        let job_dir = self.job_dir(key);
        fs::create_dir_all(&job_dir)?;
        let now = self.clock.now();
        let run_id = take_run_number(&job_dir)?;
        let record = RunRecord {
            run_id,
            workflow: key.workflow.clone(),
            job: key.job.clone(),
            package: skipped.identity.name.to_string(),
            version: skipped.identity.version.clone(),
            digest: skipped.identity.digest.as_str().to_owned(),
            libraries: Vec::new(),
            trigger: RunTrigger::Scheduled,
            dry_run: false,
            inputs: skipped.inputs,
            overrides: Vec::new(),
            outputs: Vec::new(),
            status: RunStatus::Skipped,
            started: now,
            completed: Some(now),
            error: Some(cut_to_bytes(skipped.reason, MAX_ERROR_BYTES)),
            calls: Vec::new(),
            calls_omitted: 0,
        };
        let directory = job_dir
            .join(format!("{:04}", now.year()))
            .join(format!("{:02}", u8::from(now.month())));
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("R-{run_id}.md"));
        write_atomically(&path, String::from(&record).as_bytes())?;
        Ok(path)
    }

    /// Deletes the records of the job `claim` is held on that ended before the retention window, whatever their
    /// status, except `keep` and any record still `running`.
    ///
    /// A record that cannot be read or deleted is logged and left. Month and year folders the sweep empties are
    /// removed.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError::Io`] only when the job's folder cannot be listed.
    pub fn sweep(&self, claim: &SharedClaim, keep: &Path) -> Result<SweepReport, LedgerError> {
        let job_dir = self.job_dir(claim.key());
        let now = self.clock.now();
        let mut report = SweepReport::default();
        for path in records_of(&job_dir)? {
            if path == keep {
                continue;
            }
            let header = match read_text_within(&path, MAX_RECORD_BYTES)
                .map_err(|error| error.to_string())
                .and_then(|text| RunHeader::try_from(text.as_str()).map_err(|error| error.to_string()))
            {
                Ok(header) => header,
                Err(error) => {
                    tracing::warn!(record = %Trailing { path: &path, count: RECORD_PATH_TAIL }, %error, "a run record could not be read, and is left");
                    report.skipped += 1;
                    continue;
                }
            };
            if header.status == RunStatus::Running || now - header.completed.unwrap_or(header.started) < self.retention
            {
                continue;
            }
            match fs::remove_file(&path) {
                Ok(()) => {
                    report.deleted += 1;
                    remove_if_empty(&path);
                }
                Err(error) => {
                    tracing::warn!(record = %Trailing { path: &path, count: RECORD_PATH_TAIL }, error = %error.kind(), "a run record could not be deleted, and is left");
                    report.skipped += 1;
                }
            }
        }
        Ok(report)
    }

    fn job_dir(&self, key: &SharedJobKey) -> PathBuf {
        self.runs_root.join(&key.package).join(&key.workflow).join(&key.job)
    }

    fn interrupt_dead_runs(job_dir: &Path, now: OffsetDateTime) {
        let Ok(paths) = records_of(job_dir) else {
            return;
        };
        for path in paths {
            let outcome = read_text_within(&path, MAX_RECORD_BYTES)
                .map_err(|error| error.to_string())
                .and_then(|text| mark_interrupted(&text, now).map_err(|error| error.to_string()))
                .and_then(|marked| match marked {
                    Some(text) => write_atomically(&path, text.as_bytes()).map_err(|error| error.kind().to_string()),
                    None => Ok(()),
                });
            if let Err(error) = outcome {
                tracing::warn!(record = %Trailing { path: &path, count: RECORD_PATH_TAIL }, %error, "a run record could not be checked for an interrupted run");
            }
        }
    }
}

impl RunRecorder {
    /// The number this run has, within its job.
    #[must_use]
    pub fn run_id(&self) -> u64 {
        self.lock().run_id
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Names the inputs this run was given on the command line in place of the job's own values, never their values,
    /// and writes the record again when it is written as the run goes.
    pub fn name_overrides(&self, names: Vec<String>) {
        let mut record = self.lock();
        record.overrides = names;
        if self.progress
            && let Err(error) = self.write(&record)
        {
            tracing::warn!(run_id = record.run_id, error = %error.kind(), "the overridden inputs could not be written to the run record");
        }
    }

    /// Names the outputs the script set, never their values, to be written when the record ends.
    pub fn name_outputs(&self, names: Vec<String>) {
        self.lock().outputs = names;
    }

    /// Ends the record with `outcome`.
    ///
    /// # Errors
    ///
    /// Returns the filesystem error when the record could not be written. The run's own outcome is unaffected.
    pub fn finish(&self, outcome: &RunOutcome, completed: OffsetDateTime) -> io::Result<()> {
        let mut record = self.lock();
        record.finish(outcome, completed);
        self.write(&record)
    }

    /// Writes the record, making its month folder first, because a dry run's record is the first thing written there.
    fn write(&self, record: &RunRecord) -> io::Result<()> {
        if let Some(directory) = self.path.parent() {
            fs::create_dir_all(directory)?;
        }
        write_atomically(&self.path, String::from(record).as_bytes())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RunRecord> {
        self.record.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl CallJournal for RunRecorder {
    fn call_finished(&self, entry: CallEntry) {
        let mut record = self.lock();
        record.record_call(entry);
        if !self.progress {
            return;
        }
        if let Err(error) = self.write(&record) {
            tracing::warn!(run_id = record.run_id, error = %error.kind(), "a call could not be written to the run record");
        }
    }
}

/// Reads the job's counter, writes the next number back, and returns the one read.
///
/// A missing counter is not the first run when records exist, because a number must never be given twice: the next
/// number then follows the highest record found. A counter that holds anything but a number is refused, and deleting
/// it is the repair.
fn take_run_number(job_dir: &Path) -> Result<u64, LedgerError> {
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(job_dir.join(COUNTER_LOCK_FILE))?;
    lock.lock()?;
    let taken = read_and_advance_counter(job_dir);
    // Closing the file would release the lock too, so a failure to unlock changes nothing worth reporting.
    let _ = lock.unlock();
    taken
}

fn read_and_advance_counter(job_dir: &Path) -> Result<u64, LedgerError> {
    let path = job_dir.join(COUNTER_FILE);
    let number = match fs::read_to_string(&path) {
        Ok(text) => text.trim().parse::<u64>().map_err(|_| LedgerError::Counter {
            path: path.clone(),
            value: cut_to_bytes(&text, MAX_COUNTER_ECHO_BYTES),
        })?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => highest_recorded(job_dir)?.saturating_add(1),
        Err(error) => return Err(error.into()),
    };
    write_atomically(&path, number.saturating_add(1).to_string().as_bytes())?;
    Ok(number)
}

/// The highest run number among the job's records, or 0 when it has none.
fn highest_recorded(job_dir: &Path) -> io::Result<u64> {
    Ok(records_of(job_dir)?
        .iter()
        .filter_map(|path| path.file_name()?.to_str().and_then(parse_run_id))
        .max()
        .unwrap_or(0))
}

/// Every `R-{n}.md` under the job's `{YYYY}/{MM}` folders.
pub(crate) fn records_of(job_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut records = Vec::new();
    for year in subdirectories(job_dir)? {
        for month in subdirectories(&year)? {
            for entry in fs::read_dir(&month)? {
                let path = entry?.path();
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| parse_run_id(name).is_some())
                {
                    records.push(path);
                }
            }
        }
    }
    Ok(records)
}

/// The folders directly inside `directory`. A folder that does not exist has none: a job that has never been recorded
/// has no records to find.
fn subdirectories(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(found),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            found.push(entry.path());
        }
    }
    Ok(found)
}

/// Removes the folders `deleted` sat in, month then year, when nothing else is in them. A folder that still holds
/// something refuses, which is the answer wanted, so the refusal is ignored.
fn remove_if_empty(deleted: &Path) {
    let month = deleted.parent();
    let year = month.and_then(Path::parent);
    for folder in [month, year].into_iter().flatten() {
        if fs::remove_dir(folder).is_err() {
            return;
        }
    }
}

#[cfg(test)]
#[path = "shared_ledger_test.rs"]
mod tests;
