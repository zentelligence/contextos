//! The logging questions of the `contextos config` interview: the log file, and, only when one is chosen, its
//! rotation and retention. They mirror `contextos-web config`'s, including the log file offered on a host whose
//! service manager would otherwise lose stderr, so an operator sets logging up the same way for both binaries.

use std::path::{Path, PathBuf};

use contextos_logging::{OFFERED_MAX_SIZE_MB, OFFERED_RETENTION_DAYS};

use super::{InterviewError, Interviewer, parse_manual_path};
use crate::{ConfigDocument, ConfigWriterError, LogSettings};

/// How many times one question group is re-asked after an invalid answer before the interview gives up.
const MAX_ATTEMPTS: u32 = 5;

/// This host's offered log file, `contextos.log`, by the rule `contextos-web` shares
/// ([`contextos_logging::offered_log_file`]).
#[must_use]
pub fn default_log_file() -> Option<PathBuf> {
    contextos_logging::offered_log_file("contextos.log")
}

/// Asks the log file, and, only once one is chosen, the maximum size, daily rotation, and retention, each
/// prefilled from the document. While the document sets no `log_file`, `default_log_file` (when the host has one)
/// is offered with a short rotation. An answer the schema rejects is reported and the group asked again.
///
/// Returns the offered file's folder when that file was chosen, for the caller to create once the whole interview
/// has succeeded, so a cancelled run leaves nothing behind.
pub(super) fn configure_logging(
    interviewer: &mut dyn Interviewer,
    document: &mut ConfigDocument,
    default_log_file: Option<&Path>,
) -> Result<Option<PathBuf>, InterviewError> {
    let current = document.server_settings().logging;
    let offered_file = default_log_file.filter(|_| current.file.is_empty());
    let offered_text = offered_file.map(|path| path.to_string_lossy().into_owned());
    let offered = offered_text.is_some();

    let path_default = if current.file.is_empty() {
        offered_text.clone().unwrap_or_default()
    } else {
        current.file.clone()
    };
    let size_default = number_default(
        current
            .max_size_mb
            .or_else(|| offered.then_some(u64::from(OFFERED_MAX_SIZE_MB))),
    );
    let days_default = number_default(
        current
            .retention_days
            .map(u64::from)
            .or_else(|| offered.then_some(u64::from(OFFERED_RETENTION_DAYS))),
    );
    let kept_default = number_default(current.retention_files.map(u64::from));

    for _attempt in 0..MAX_ATTEMPTS {
        let file = ask_text(interviewer, "Log file path (blank for stderr):", &path_default)?;
        let mut logging = LogSettings {
            file: parse_manual_path(&file).to_string_lossy().into_owned(),
            ..LogSettings::default()
        };
        if !logging.file.is_empty() {
            logging.max_size_mb = ask_optional_number(
                interviewer,
                "Rotate the log file at this size in MB (blank to never):",
                &size_default,
            )?;
            logging.rotate_daily =
                interviewer.confirm("Rotate the log file every calendar day (UTC)?", current.rotate_daily)?;
            if logging.max_size_mb.is_some() || logging.rotate_daily {
                logging.retention_days = ask_optional_count(
                    interviewer,
                    "Delete rotated logs older than this many days (blank to keep all):",
                    &days_default,
                )?;
                logging.retention_files = ask_optional_count(
                    interviewer,
                    "Keep only this many most-recent rotated logs (blank to keep all):",
                    &kept_default,
                )?;
            }
        }
        match document.set_logging(&logging) {
            Ok(()) => {
                return Ok(offered_text
                    .filter(|offered| *offered == logging.file)
                    .and(offered_file.and_then(Path::parent).map(Path::to_path_buf)));
            }
            Err(ConfigWriterError::Invalid { source }) => {
                interviewer.notify(&format!("That was rejected: {source}. Please try again."));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(InterviewError::TooManyAttempts {
        section: "logging settings",
    })
}

/// A free-text question, prefilled only when there is something to prefill.
fn ask_text(interviewer: &mut dyn Interviewer, prompt: &str, default: &str) -> Result<String, InterviewError> {
    if default.is_empty() {
        interviewer.ask(prompt)
    } else {
        interviewer.ask_with_default(prompt, default)
    }
}

/// Asks for an optional whole number, re-asking on a non-numeric answer. A blank answer is `None`.
fn ask_optional_number(
    interviewer: &mut dyn Interviewer,
    prompt: &str,
    default: &str,
) -> Result<Option<u64>, InterviewError> {
    for _attempt in 0..MAX_ATTEMPTS {
        let raw = ask_text(interviewer, prompt, default)?;
        if raw.is_empty() {
            return Ok(None);
        }
        match raw.parse::<u64>() {
            Ok(number) => return Ok(Some(number)),
            Err(_source) => interviewer.notify(&format!("{raw:?} is not a whole number. Please try again.")),
        }
    }
    Err(InterviewError::TooManyAttempts {
        section: "numeric answer",
    })
}

/// As [`ask_optional_number`], for the `u32` retention settings, refusing a larger value rather than truncating it.
fn ask_optional_count(
    interviewer: &mut dyn Interviewer,
    prompt: &str,
    default: &str,
) -> Result<Option<u32>, InterviewError> {
    ask_optional_number(interviewer, prompt, default)?
        .map(|value| u32::try_from(value).map_err(|_source| InterviewError::NumberOutOfRange { value }))
        .transpose()
}

fn number_default(value: Option<u64>) -> String {
    value.map(|number| number.to_string()).unwrap_or_default()
}

#[cfg(test)]
#[path = "logging_test.rs"]
mod tests;
