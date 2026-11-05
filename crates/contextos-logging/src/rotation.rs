use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use same_file::Handle;
use time::macros::format_description;
use time::{Duration, OffsetDateTime, PrimitiveDateTime, UtcOffset};

/// Injectable time source: rotation and retention are both keyed off this,
/// never the wall clock directly, so day-boundary and age-based retention
/// behaviour is deterministically testable.
pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// Rotation and retention triggers for one [`RotatingLog`]. Each binary
/// resolves this once from its own `ServerConfig` at startup (its
/// megabyte-unit size field converted to the bytes this type actually
/// compares against) via its own `From<&ItsServerConfig>` implementation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LogRotationPolicy {
    pub max_size_bytes: Option<u64>,
    pub rotate_daily: bool,
    pub retention_days: Option<u32>,
    pub retention_files: Option<u32>,
}

/// Opens `log_file` as a [`RotatingLog`], or `None` when `log_file` is
/// empty (logging goes to stderr; each binary's own config validation
/// already rejects a rotation/retention setting paired with an empty
/// `log_file` before this is ever called, so a non-default `policy` here
/// always has somewhere to apply).
///
/// # Errors
///
/// Returns the underlying [`io::Error`] when `log_file` cannot be opened
/// (for example, its parent directory does not exist): a startup error, not
/// a lazily-discovered one.
pub fn open(log_file: &str, policy: LogRotationPolicy) -> io::Result<Option<RotatingLog<SystemClock>>> {
    if log_file.is_empty() {
        return Ok(None);
    }
    RotatingLog::open(PathBuf::from(log_file), policy, SystemClock).map(Some)
}

const ROTATED_SUFFIX: &[time::format_description::FormatItem<'_>] =
    format_description!("[year][month][day]T[hour][minute][second]Z");

/// A rotating log file that is safe to share between processes.
///
/// Many `contextos` processes (every stdio client, plus the web host) load one
/// `config.toml` and so append to one `log_file`. Each holds its own
/// `RotatingLog`, with no channel between them other than the filesystem, so
/// every decision is made from the filesystem rather than from process memory:
///
/// - **Size** is the length of the file currently at the path, never a
///   per-process byte count, so the limit applies to the combined output.
/// - **Identity**: before every write the path is compared with the handle this
///   process holds (device and inode on Unix, volume serial number and file
///   index on Windows, through `same-file`). When another process has renamed
///   the file away, or removed it, this process reopens the path in append
///   mode (create plus append, never truncate) and carries on in the new file.
///   Cost per write: one `open`, one `fstat`, and one `close` of the path
///   beside the write itself, which is negligible at log volumes and keeps the
///   decision correct without any background thread or notification channel.
/// - **Rotation** runs under an exclusive advisory lock on a hidden sibling
///   lock file (`.{file_name}.lock`, through `std::fs::File` as everywhere else in the
///   workspace: `flock` on Unix, `LockFileEx` on Windows, released by the OS
///   if the holder dies). The
///   triggers are evaluated again under the lock against the file now at the
///   path: when another process has already rotated, this one reopens the
///   path and does not rotate a second time. The lock file is never deleted,
///   since removing it would let two processes lock different files.
/// - **Failure policy**: logging must outlive its own housekeeping. A lock
///   that cannot be opened or taken, or an identity check that cannot be made,
///   skips the rotation attempt (or reopen) for that write, the line is still
///   written, and the next write tries again; the file may overshoot its size
///   limit until the lock works. A rename or reopen failure while rotating, and
///   a failure to recreate the path after another process removed it, still
///   propagate to the writer as before (tracing discards them, and the next
///   write retries). Pruning remains best-effort.
///
/// A line written through a stale handle in the instant between another
/// process's check and rename lands in the rotated file rather than the live
/// one; no line is lost or split, and the live file can overshoot the limit by
/// at most one line per concurrent writer. When the day boundary and another
/// process's size rotation fall in the same window, the next daily rotation
/// waits for the next boundary rather than rotating the fresh file.
pub struct RotatingLog<C: Clock> {
    path: PathBuf,
    lock_path: PathBuf,
    file: File,
    identity: Handle,
    /// Length of the file at `path` as of the last [`refresh`](Self::refresh).
    observed_len: u64,
    current_day: time::Date,
    policy: LogRotationPolicy,
    clock: C,
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

impl<C: Clock> RotatingLog<C> {
    /// Opens (creating if absent) `path` in append mode. The rotation
    /// clock starts from `clock.now()`'s own day at open time, not any day
    /// implied by the file's existing content: a restart never triggers an
    /// immediate rotation purely because the file predates today, only the
    /// next real day-boundary crossing this process itself observes does.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when `path` cannot be opened or
    /// its metadata cannot be read.
    pub fn open(path: PathBuf, policy: LogRotationPolicy, clock: C) -> io::Result<Self> {
        let file = open_append(&path)?;
        let observed_len = file.metadata()?.len();
        let identity = Handle::from_file(file.try_clone()?)?;
        let current_day = utc_now(&clock).date();
        let lock_path = lock_file_path(&path);
        Ok(Self {
            path,
            lock_path,
            file,
            identity,
            observed_len,
            current_day,
            policy,
            clock,
        })
    }

    fn should_rotate(&self, incoming: usize) -> bool {
        if self.policy.rotate_daily && utc_now(&self.clock).date() != self.current_day {
            return true;
        }
        match self.policy.max_size_bytes {
            Some(max) => self.observed_len.saturating_add(incoming as u64) > max,
            None => false,
        }
    }

    /// Brings this writer in line with the file now at `path`.
    ///
    /// When the path still names the file this writer holds, records its
    /// current length. When it names another file, or none (another process
    /// rotated or removed it), reopens the path in append mode and adopts the
    /// new file, restarting the daily clock from today because the new file
    /// was created by a rotation that already accounted for any day change.
    /// An identity check that cannot be made for another reason (for example
    /// the path is momentarily unreadable) leaves the current handle in use.
    fn refresh(&mut self) -> io::Result<()> {
        match Handle::from_path(&self.path) {
            Ok(at_path) if at_path == self.identity => {
                if let Ok(metadata) = at_path.as_file().metadata() {
                    self.observed_len = metadata.len();
                }
                Ok(())
            }
            Ok(_) => self.reopen(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => self.reopen(),
            Err(_) => {
                if let Ok(metadata) = self.file.metadata() {
                    self.observed_len = metadata.len();
                }
                Ok(())
            }
        }
    }

    /// Opens whatever is at `path` (creating it if absent) for appending.
    /// Never truncates: the file may already hold another process's lines.
    fn reopen(&mut self) -> io::Result<()> {
        let file = open_append(&self.path)?;
        self.observed_len = file.metadata()?.len();
        self.identity = Handle::from_file(file.try_clone()?)?;
        self.file = file;
        self.current_day = utc_now(&self.clock).date();
        Ok(())
    }

    /// Rotates if `incoming` more bytes would breach a trigger, once, across
    /// every process sharing the path. A lock that cannot be had skips the
    /// rotation (see the type docs); a rename or reopen failure propagates.
    fn rotate_if_due(&mut self, incoming: usize) -> io::Result<()> {
        if !self.should_rotate(incoming) {
            return Ok(());
        }
        let Ok(lock) = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.lock_path)
        else {
            return Ok(());
        };
        if lock.lock().is_err() {
            return Ok(());
        }
        // The lock is released when `lock` is dropped, on every exit path.
        self.refresh()?;
        if !self.should_rotate(incoming) {
            return Ok(());
        }
        self.rotate()
    }

    /// Renames the current file to a timestamped sibling, reopens `path` as
    /// a fresh empty file, then prunes older siblings against `policy`'s
    /// retention bounds. Called only with the rotation lock held. A pruning
    /// failure is swallowed (best-effort housekeeping, never a reason to fail
    /// the write that triggered rotation, matching the write pipeline's
    /// "downstream failures become warnings, never a rolled-back write"
    /// precedent); a rotation failure itself (the rename or reopen) is not,
    /// and propagates to the caller.
    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let rotated = self.rotated_sibling_path();
        std::fs::rename(&self.path, &rotated)?;
        self.reopen()?;
        let _ = self.prune_rotated_siblings();
        Ok(())
    }

    /// A not-yet-existing `{file_name}.{timestamp}` sibling path: a numeric
    /// suffix is appended only in the (test-only in practice) case of two
    /// rotations landing on the same clock second, so concurrent rotations
    /// never clobber each other's rotated file. The timestamp is always the
    /// injected clock's own value, never the real wall clock, so retention
    /// age comparisons ([`prune_rotated_siblings`](Self::prune_rotated_siblings))
    /// stay deterministic under a fake clock in tests exactly as they do
    /// against [`SystemClock`] in production, rather than silently falling
    /// back to the filesystem's own real mtime.
    fn rotated_sibling_path(&self) -> PathBuf {
        let stamp = utc_now(&self.clock)
            .format(ROTATED_SUFFIX)
            .unwrap_or_else(|_| "0".to_owned());
        let mut candidate = sibling_with_suffix(&self.path, &stamp);
        let mut disambiguator: u32 = 1;
        while candidate.exists() {
            candidate = sibling_with_suffix(&self.path, &format!("{stamp}-{disambiguator}"));
            disambiguator += 1;
        }
        candidate
    }

    /// Every existing `{file_name}.*` sibling of `path` in its own
    /// directory that this log itself rotated (its name parses back as a
    /// [`rotated_sibling_path`](Self::rotated_sibling_path)-shaped
    /// timestamp), paired with that rotation's own clock timestamp. A
    /// sibling whose name does not parse (a foreign file, or one whose
    /// naming this log did not produce) is left alone entirely: retention
    /// only ever prunes files this log itself created, never guesses at an
    /// unrecognised one's age.
    fn rotated_siblings(&self) -> io::Result<Vec<(PathBuf, OffsetDateTime)>> {
        let Some(file_name) = self.path.file_name().map(|name| name.to_string_lossy().into_owned()) else {
            return Ok(Vec::new());
        };
        let directory = self.path.parent().unwrap_or_else(|| Path::new("."));
        let prefix = format!("{file_name}.");
        let mut siblings = Vec::new();
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(rotated_at) = name.strip_prefix(&prefix).and_then(parse_rotation_timestamp) {
                siblings.push((entry.path(), rotated_at));
            }
        }
        Ok(siblings)
    }

    /// Deletes rotated siblings that violate either configured retention
    /// bound: beyond the `retention_files` most recently rotated, or
    /// rotated more than `retention_days` before the current (injected)
    /// clock time. A bound left unset never deletes on that basis (each
    /// binary's own config validation already requires at least one
    /// rotation trigger whenever retention is configured at all, so this
    /// only runs when it can have an effect). An individual file's delete
    /// failure is skipped, not propagated: one permission-denied sibling
    /// must not stop every other sibling's pruning.
    fn prune_rotated_siblings(&self) -> io::Result<()> {
        let mut siblings = self.rotated_siblings()?;
        if siblings.is_empty() {
            return Ok(());
        }
        siblings.sort_by_key(|(_, rotated_at)| std::cmp::Reverse(*rotated_at));

        let mut doomed: Vec<&Path> = Vec::new();
        if let Some(keep) = self.policy.retention_files {
            doomed.extend(siblings.iter().skip(keep as usize).map(|(path, _)| path.as_path()));
        }
        if let Some(days) = self.policy.retention_days {
            let cutoff = utc_now(&self.clock) - Duration::days(i64::from(days));
            doomed.extend(
                siblings
                    .iter()
                    .filter(|(_, rotated_at)| *rotated_at < cutoff)
                    .map(|(path, _)| path.as_path()),
            );
        }
        for path in doomed {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }
}

/// `.{file_name}.lock` beside the log: hidden, and not shaped like a rotated
/// sibling (`{file_name}.{timestamp}`), so retention never considers it.
fn lock_file_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{file_name}.lock"))
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{file_name}.{suffix}"))
}

/// Length, in bytes, of a [`ROTATED_SUFFIX`]-formatted timestamp
/// (`YYYYMMDDTHHMMSSZ`): every component is fixed-width, so this is a
/// compile-time constant, not something computed from a sample value.
const ROTATED_SUFFIX_LEN: usize = 16;

/// Recovers the clock timestamp embedded in one rotated file's own
/// dotted-suffix (the part of its name after `{file_name}.`), tolerating a
/// trailing `-N` same-second disambiguator
/// ([`rotated_sibling_path`](RotatingLog::rotated_sibling_path)) after the
/// fixed-width timestamp. `None` for anything that does not parse: a
/// foreign file sharing the prefix by coincidence, or a truncated/corrupted
/// name.
fn parse_rotation_timestamp(suffix: &str) -> Option<OffsetDateTime> {
    let stamp = suffix.get(..ROTATED_SUFFIX_LEN)?;
    let naive = PrimitiveDateTime::parse(stamp, ROTATED_SUFFIX).ok()?;
    Some(naive.assume_utc())
}

/// `clock.now()`, normalised to UTC: every rotation decision, filename
/// timestamp, and retention comparison is expressed in UTC regardless of
/// what offset a particular [`Clock`] implementation's `now()` happens to
/// return, so `"UTC calendar day"` and the retained/parsed filename
/// timestamps stay unambiguous.
fn utc_now<C: Clock>(clock: &C) -> OffsetDateTime {
    clock.now().to_offset(UtcOffset::UTC)
}

impl<C: Clock> Write for RotatingLog<C> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.refresh()?;
        self.rotate_if_due(buf.len())?;
        let written = self.file.write(buf)?;
        self.observed_len = self.observed_len.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
#[path = "rotation_test.rs"]
mod tests;
