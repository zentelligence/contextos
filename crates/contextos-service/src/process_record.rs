//! The running server's process record: its process id and when it wrote the record, kept beside the configuration file it
//! loaded ([`path_for`]) while it serves, so each configuration's server has its own. `<program> service` reads it
//! to stop, restart, and report on that one process where no service manager tracks it (a Windows Startup App), and to
//! tell a crash (a record whose process is gone) from a clean stop (no record). The server writes it once it is
//! listening and removes it when it stops cleanly.
//!
//! The executable and write time are what tie the record to its process: the process must run the executable the
//! server recorded, whichever copy a command is run from, and a process that started after the record was written
//! cannot be the one that wrote it, so a process id Windows reuses after a crash is never mistaken for the server.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::write_atomically;

/// What the record holds.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRecord {
    pub pid: u32,
    /// The executable the server runs.
    pub exe: PathBuf,
    /// When the server wrote the record, in milliseconds since the Unix epoch: after the server process started.
    pub written_at_ms: u64,
}

/// Failures reading or writing the record.
#[derive(Debug, Error)]
pub enum ProcessRecordError {
    #[error("the server's process record at {path} could not be read: {source}", path = path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "the server's process record at {path} is not one {program} wrote ({detail}); stop any running \
         {program} server, delete the file, and try again",
        path = path.display()
    )]
    Malformed {
        program: &'static str,
        path: PathBuf,
        detail: String,
    },
}

/// Where the record for the server loading `config` lives: beside it, named for it (`web.toml.process.json` for
/// `web.toml`, `config.toml.process.json` for `config.toml`), so servers for different configurations never share one.
#[must_use]
pub fn path_for(config: &Path) -> PathBuf {
    let mut name = config.file_name().unwrap_or_default().to_os_string();
    name.push(".process.json");
    config.with_file_name(name)
}

/// The record at `path`, or `None` when there is none.
///
/// # Errors
///
/// [`ProcessRecordError::Read`] when the file exists but cannot be read, and [`ProcessRecordError::Malformed`] when it
/// is not a record: it is reported, never taken to mean "no server".
pub fn read(path: &Path, program: &'static str) -> Result<Option<ProcessRecord>, ProcessRecordError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ProcessRecordError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|error| ProcessRecordError::Malformed {
            program,
            path: path.to_path_buf(),
            detail: error.to_string(),
        })
}

/// Removes the record at `path`; nothing to remove is not an error.
///
/// # Errors
///
/// The underlying [`io::Error`] when the file exists but cannot be removed.
pub fn remove(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// This process's claim on the record: written by [`ProcessRecordGuard::claim`] and removed when dropped, unless
/// another server has written its own since.
#[derive(Debug)]
pub struct ProcessRecordGuard {
    program: &'static str,
    path: PathBuf,
    pid: u32,
}

impl ProcessRecordGuard {
    /// Writes a record naming this process and its executable at `path`, stamped with the current time. `program`
    /// is the binary's name, which a malformed record's message names.
    ///
    /// # Errors
    ///
    /// The underlying [`io::Error`] when the file cannot be written, this process's executable cannot be named, or the
    /// system clock is before the Unix epoch.
    pub fn claim(path: PathBuf, program: &'static str) -> io::Result<Self> {
        let pid = std::process::id();
        let exe = std::env::current_exe()?;
        let since_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?;
        let written_at_ms = u64::try_from(since_epoch.as_millis()).map_err(io::Error::other)?;
        let record = serde_json::to_vec(&ProcessRecord {
            pid,
            exe,
            written_at_ms,
        })
        .map_err(io::Error::other)?;
        write_atomically(&path, &record)?;
        Ok(Self { program, path, pid })
    }
}

impl Drop for ProcessRecordGuard {
    fn drop(&mut self) {
        if matches!(read(&self.path, self.program), Ok(Some(record)) if record.pid == self.pid)
            && let Err(error) = remove(&self.path)
        {
            tracing::warn!(%error, "the server's process record could not be removed");
        }
    }
}

#[cfg(test)]
#[path = "process_record_test.rs"]
mod tests;
