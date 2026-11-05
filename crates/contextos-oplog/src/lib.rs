#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::path::{Component, Path};
use std::sync::{Arc, Mutex};

use contextos_core::{
    AppendMutation, AppendsVault, LogsOperations, OpKind, OperationEvent, OperationWarning, Origin, RoutedEvents,
    RoutesOperations, VaultPath, VaultPathInput, VaultRoot, VaultRootId, VaultSet,
};
use thiserror::Error;
use time::OffsetDateTime;

/// Trusted dependencies and relative storage directory for one vault log.
#[derive(Clone, Debug)]
pub struct OperationLogConfig<A> {
    pub root: VaultRoot,
    pub roots: VaultSet,
    pub relative_directory: String,
    pub appender: A,
}

/// Append-only daily operation-log service.
#[derive(Clone, Debug)]
pub struct OperationLog<A> {
    root: VaultRoot,
    root_id: VaultRootId,
    roots: VaultSet,
    relative_directory: String,
    appender: A,
    pending: Arc<Mutex<VecDeque<AppendMutation>>>,
    /// Events for entries already written but not yet handed to a caller. An entry is written before the entries
    /// behind it, so a later failure must not lose the events of those that succeeded: they wait here and go out with
    /// the next call that can deliver them.
    written: Arc<Mutex<Vec<OperationEvent>>>,
}

/// Caller-authored operation-log content with trusted timestamp and paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManualLogInput {
    pub entry: String,
    pub files: Vec<VaultPath>,
    pub at: OffsetDateTime,
    /// The tool that asked for the entry. The write is attributed to it, so version history names the tool.
    pub requested_by: String,
}

impl<A> TryFrom<OperationLogConfig<A>> for OperationLog<A>
where
    A: AppendsVault,
    A::Error: std::error::Error + 'static,
{
    type Error = OperationLogError<A::Error>;

    fn try_from(value: OperationLogConfig<A>) -> Result<Self, Self::Error> {
        let directory = Path::new(&value.relative_directory);
        if value.relative_directory.is_empty()
            || directory.is_absolute()
            || directory.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(OperationLogError::InvalidDirectory);
        }
        let root_index = value
            .roots
            .iter()
            .position(|candidate| candidate == &value.root)
            .ok_or(OperationLogError::RootNotConfigured)?;
        let root_id = VaultRootId::try_from(root_index).map_err(OperationLogError::Path)?;
        Ok(Self {
            root: value.root,
            root_id,
            roots: value.roots,
            relative_directory: value.relative_directory,
            appender: value.appender,
            pending: Arc::new(Mutex::new(VecDeque::new())),
            written: Arc::new(Mutex::new(Vec::new())),
        })
    }
}

impl<A> OperationLog<A>
where
    A: AppendsVault,
    A::Error: std::error::Error + 'static,
{
    fn append_event(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationLogError<A::Error>> {
        if matches!(event.origin, Origin::Internal(_)) {
            return Ok(Vec::new());
        }
        let files = event
            .paths
            .iter()
            .filter(|path| path.root_id() == self.root_id)
            .cloned()
            .collect::<Vec<_>>();
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let origin = match &event.origin {
            Origin::Tool(origin) => single_line(origin),
            Origin::Internal(_) => return Ok(Vec::new()),
        };
        let operation = OperationName::from(event.kind);
        self.append_record(
            event.at,
            &origin,
            <&str>::from(&operation),
            &event.summary,
            &files,
            Origin::Internal("oplog".to_owned()),
        )
    }

    /// Appends one explicit caller-authored log entry with `manual` origin, and routes every write it made (the
    /// entry, and any buffered entries recovered first) through `router`.
    ///
    /// # Errors
    ///
    /// Returns a typed validation, path, append, or retry-buffer error.
    pub fn append_manual(
        &self,
        input: &ManualLogInput,
        router: &dyn RoutesOperations,
    ) -> Result<RoutedEvents, OperationLogError<A::Error>> {
        if input.entry.trim().is_empty() {
            return Err(OperationLogError::EmptyManualEntry);
        }
        if input.files.iter().any(|path| path.root_id() != self.root_id) {
            return Err(OperationLogError::WrongRoot);
        }
        let appended = self.append_record(
            input.at,
            "manual",
            "log",
            &input.entry,
            &input.files,
            Origin::Internal(input.requested_by.clone()),
        );
        self.route_written(appended, router)
    }

    /// Retries every buffered append in original order without creating a new record, and routes every write it
    /// recovered through `router`.
    ///
    /// # Errors
    ///
    /// Returns a typed append or retry-buffer error and leaves the failing and
    /// subsequent records buffered for another recovery attempt.
    pub fn flush(&self, router: &dyn RoutesOperations) -> Result<RoutedEvents, OperationLogError<A::Error>> {
        let flushed = {
            let mut pending = self.pending.lock().map_err(|_| OperationLogError::BufferUnavailable)?;
            self.flush_pending(&mut pending).and_then(|()| self.take_written())
        };
        self.route_written(flushed, router)
    }

    /// Routes what `appended` produced. When it failed part-way, the entries written before the failure are routed
    /// all the same, and the failure is still returned.
    fn route_written(
        &self,
        appended: Result<Vec<OperationEvent>, OperationLogError<A::Error>>,
        router: &dyn RoutesOperations,
    ) -> Result<RoutedEvents, OperationLogError<A::Error>> {
        match appended {
            Ok(events) => Ok(router.route_all(events)),
            Err(error) => {
                if let Ok(stranded) = self.take_written() {
                    let outcome = router.route_all(stranded);
                    for warning in &outcome.warnings {
                        tracing::warn!(
                            code = warning.code,
                            "entries written before a log failure were not fully routed"
                        );
                    }
                }
                Err(error)
            }
        }
    }

    /// Hands over the events of every entry written and not yet delivered.
    fn take_written(&self) -> Result<Vec<OperationEvent>, OperationLogError<A::Error>> {
        let mut written = self.written.lock().map_err(|_| OperationLogError::BufferUnavailable)?;
        Ok(std::mem::take(&mut *written))
    }

    fn append_record(
        &self,
        at: OffsetDateTime,
        origin: &str,
        operation: &str,
        summary: &str,
        files: &[VaultPath],
        written_as: Origin,
    ) -> Result<Vec<OperationEvent>, OperationLogError<A::Error>> {
        let year = at.year();
        let month = u8::from(at.month());
        let day = at.day();
        let relative = format!(
            "{}/{year:04}/{month:02}/{year:04}-{month:02}-{day:02}.md",
            self.relative_directory
        );
        let absolute = self.root.path().join(&relative);
        let raw = absolute
            .to_str()
            .ok_or_else(|| OperationLogError::NonUtf8Path { path: absolute.clone() })?;
        let path = VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw,
        })
        .map_err(OperationLogError::Path)?;
        let origin = single_line(origin);
        let summary = single_line(summary);
        let files = files
            .iter()
            .map(|path| single_line(&path.portable_relative()))
            .collect::<Vec<_>>()
            .join(", ");
        let line = format!(
            "{:02}:{:02}:{:02} | {origin} | {} | {summary} | files: {files}\n",
            at.hour(),
            at.minute(),
            at.second(),
            single_line(operation),
        );
        let preamble = format!("# {year:04}-{month:02}-{day:02}: Operation Log\n\n");
        let mut pending = self.pending.lock().map_err(|_| OperationLogError::BufferUnavailable)?;
        pending.push_back(AppendMutation {
            path,
            preamble,
            content: line,
            origin: written_as,
        });
        self.flush_pending(&mut pending)?;
        drop(pending);
        self.take_written()
    }

    fn flush_pending(&self, pending: &mut VecDeque<AppendMutation>) -> Result<(), OperationLogError<A::Error>> {
        while let Some(request) = pending.front() {
            let result = self.appender.append(request).map_err(OperationLogError::Append)?;
            if let Some(event) = result.event {
                self.written
                    .lock()
                    .map_err(|_| OperationLogError::BufferUnavailable)?
                    .push(event);
            }
            pending.pop_front();
        }
        Ok(())
    }
}

impl<A> LogsOperations for OperationLog<A>
where
    A: AppendsVault,
    A::Error: std::error::Error + 'static,
{
    fn append(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning> {
        self.append_event(event).map_err(OperationWarning::from)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OperationName(&'static str);

impl From<OpKind> for OperationName {
    fn from(value: OpKind) -> Self {
        Self(match value {
            OpKind::Create => "create",
            OpKind::Modify => "modify",
            OpKind::Move => "move",
            OpKind::Delete => "delete",
            OpKind::Restore => "restore",
        })
    }
}

impl<'a> From<&'a OperationName> for &'a str {
    fn from(value: &'a OperationName) -> Self {
        value.0
    }
}

fn single_line(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

/// Typed operation-log failures.
#[derive(Debug, Error)]
pub enum OperationLogError<E>
where
    E: std::error::Error + 'static,
{
    #[error("operation-log directory must be a non-empty relative path without traversal")]
    InvalidDirectory,
    #[error("operation-log path is invalid")]
    Path(#[source] contextos_core::PathError),
    #[error("operation-log root is not present in the configured vault set")]
    RootNotConfigured,
    #[error("operation-log request selected a different vault root")]
    WrongRoot,
    #[error("manual operation-log entry must not be empty")]
    EmptyManualEntry,
    #[error("operation-log path is not valid UTF-8: {path}")]
    NonUtf8Path { path: std::path::PathBuf },
    #[error("operation-log entry could not be appended")]
    Append(#[source] E),
    #[error("operation-log retry buffer is unavailable")]
    BufferUnavailable,
}

impl<E> OperationLogError<E>
where
    E: std::error::Error + 'static,
{
    /// Returns the stable machine-readable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Path(error) => error.code(),
            Self::InvalidDirectory
            | Self::RootNotConfigured
            | Self::WrongRoot
            | Self::EmptyManualEntry
            | Self::NonUtf8Path { .. }
            | Self::Append(_)
            | Self::BufferUnavailable => "log/append",
        }
    }
}

impl<E> From<OperationLogError<E>> for OperationWarning
where
    E: std::error::Error + 'static,
{
    fn from(value: OperationLogError<E>) -> Self {
        Self {
            code: value.code().to_owned(),
            message: value.to_string(),
        }
    }
}
