#![forbid(unsafe_code)]

//! Rotating, retention-bounded log file destination for `tracing`, shared by
//! every binary in this workspace (`contextos-mcp`, `contextos-web`) that
//! accepts an operator-configured `log_file` plus rotation/retention
//! settings.
//!
//! [`RotatingLog`] is a single-file, `tracing_subscriber`-compatible
//! [`std::io::Write`] destination. It rotates the live file to a timestamped
//! sibling (`{log_file}.{YYYYMMDDTHHMMSSZ}`) when either configured trigger
//! fires (a configured byte size reached, or the UTC calendar day changes
//! under daily rotation), then prunes older rotated siblings against a
//! configured day count or file count. Rotation and retention are both keyed
//! off an injected [`Clock`], never the wall clock directly, so day-boundary
//! and age-based retention behaviour is deterministically testable.
//!
//! The file is safe to share between processes, which is the normal case
//! because every `contextos` process loads the same `config.toml`: the size
//! limit applies to the file's real length, rotation is serialised by an
//! advisory lock on a hidden sibling `.{log_file}.lock` and re-checked under
//! it, and a process whose handle no longer matches the file at the path
//! reopens it in append mode. See [`RotatingLog`] for the details and the
//! failure policy.
//!
//! Deliberately not a general-purpose logging crate integration
//! (`tracing-appender` covers time-based rotation only; nothing on the
//! workspace's existing dependency graph covers size-based rotation or
//! count/age retention together): a small, fully-owned implementation in the
//! same spirit as `contextos-web`'s `atomic_write`; its only dependency beyond
//! `time` is `same-file`, for a portable file-identity comparison. Each binary owns its own
//! `From<&ItsServerConfig> for LogRotationPolicy` and thin `open` wrapper;
//! this crate owns only the config-agnostic mechanism, so the same
//! security- and data-integrity-sensitive rotation/retention logic is never
//! duplicated across binaries.

mod offered;
mod rotation;

pub use offered::{OFFERED_MAX_SIZE_MB, OFFERED_RETENTION_DAYS, offered_log_file, offered_log_file_on};
pub use rotation::{Clock, LogRotationPolicy, RotatingLog, SystemClock, open};
