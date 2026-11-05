//! The one way a workflow's Rhai script reaches outside itself:
//! issuing a single MCP tool call. Its logging functions only write to the run's own log. `contextos-web` supplies the
//! implementation over its own already-connected `McpClientSet`; this
//! crate depends on neither `contextos-web` nor any MCP transport crate,
//! only this trait and a plain JSON request/response shape, so it stays
//! independently unit-testable against a fake.

use std::future::Future;
use std::pin::Pin;

use serde_json::{Map, Value};
use thiserror::Error;

/// Dispatches one MCP tool call: `server` is a `[[mcp_server]]` name
/// (already validated against `web.toml` at job registration, before any
/// script of this job ever runs), `tool` the MCP tool name, `arguments`
/// its JSON arguments object.
///
/// Boxed and pinned rather than an `async fn` in the trait: this trait is
/// stored as `Arc<dyn McpCaller>` so one implementation is shared across
/// every concurrently-scheduled job run, which requires it to be object
/// safe.
pub trait McpCaller: Send + Sync {
    fn call<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        arguments: Map<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpCallFailure>> + Send + 'a>>;
}

/// Why a workflow run, or one of the MCP calls it made, failed: a closed set, so an
/// owner, a script, and a later run ledger can tell a refusal from an
/// unavailable server from a tool error without parsing prose.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowFailureKind {
    /// The call was outside the effects its workflow declares, and was never issued.
    EffectDenied,
    /// The call named a vault outside the run's scope, and was never issued.
    VaultOutOfScope,
    /// A vault the run named is not (or is no longer) configured on the server.
    VaultUnavailable,
    /// The server was not configured or could not be reached.
    ServerUnavailable,
    /// The tool ran and returned an error result.
    ToolError,
    /// The tool's result could not be read as JSON.
    ResultUnreadable,
    /// The package version a run needs is not installed, so the run ended before any call.
    PackageUnavailable,
    /// A supplied input was undeclared, missing, or invalid, so the run ended before any call.
    InputInvalid,
    /// Another run of the same shared job holds its claim, so this run never started.
    JobBusy,
    /// The run exceeded its timeout and was stopped.
    TimedOut,
    /// The job's lifecycle state does not allow this run, so it never started.
    JobNotRunnable,
    /// A path the script gave for one of its workflow's files is not a plain relative path inside the workflow.
    FilePathInvalid,
    /// A path the script gave is not among, or beneath, the files its workflow declares.
    FileUndeclared,
    /// A declared path holds no regular file.
    FileNotFound,
    /// A declared file is not UTF-8 text.
    FileNotText,
    /// A declared file, or a listing of declared files, is over its limit.
    FileTooLarge,
    /// A declared file could not be read, or is a link.
    FileUnreadable,
    /// The script itself failed: it threw its own error, or hit a runtime error of its own.
    ScriptError,
    /// The workflow's entry script could not be read, so the run ended before it started.
    EntryUnreadable,
    /// A script set an output the workflow does not declare or with the wrong kind, or ended without a required output.
    OutputInvalid,
    /// A task of the host that was running the workflow panicked or was lost, which is no fault of the workflow.
    Internal,
    /// The job state that records what was reviewed could not be read or parsed, so the run ended before it started.
    StateUnavailable,
}

impl WorkflowFailureKind {
    /// The stable machine-readable code for this kind.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::EffectDenied => "workflow/effect-denied",
            Self::VaultOutOfScope => "workflow/vault-out-of-scope",
            Self::VaultUnavailable => "workflow/vault-unavailable",
            Self::ServerUnavailable => "workflow/server-unavailable",
            Self::ToolError => "workflow/tool-error",
            Self::ResultUnreadable => "workflow/result-unreadable",
            Self::PackageUnavailable => "workflow/package-unavailable",
            Self::InputInvalid => "workflow/input-invalid",
            Self::JobBusy => "workflow/job-busy",
            Self::TimedOut => "workflow/timed-out",
            Self::JobNotRunnable => "workflow/job-not-runnable",
            Self::FilePathInvalid => "workflow/file-path-invalid",
            Self::FileUndeclared => "workflow/file-undeclared",
            Self::FileNotFound => "workflow/file-not-found",
            Self::FileNotText => "workflow/file-not-text",
            Self::FileTooLarge => "workflow/file-too-large",
            Self::FileUnreadable => "workflow/file-unreadable",
            Self::ScriptError => "workflow/script-error",
            Self::EntryUnreadable => "workflow/entry-unreadable",
            Self::OutputInvalid => "workflow/output-invalid",
            Self::Internal => "workflow/internal-error",
            Self::StateUnavailable => "workflow/state-unavailable",
        }
    }
}

/// The codes a run can end with before its script has started, so the run says nothing about the job. This is the one
/// place they are listed: the run path emits each of them only while it is still preparing the run, and a record from
/// before runs said whether their script started is judged by it.
pub const PRE_START_CODES: [&str; 6] = [
    WorkflowFailureKind::EntryUnreadable.code(),
    WorkflowFailureKind::ServerUnavailable.code(),
    WorkflowFailureKind::InputInvalid.code(),
    WorkflowFailureKind::JobBusy.code(),
    WorkflowFailureKind::JobNotRunnable.code(),
    WorkflowFailureKind::StateUnavailable.code(),
];

/// Whether a run that failed with `code` ended before its script started. A run with no code is taken to have started.
#[must_use]
pub fn ends_before_the_script(code: Option<&str>) -> bool {
    code.is_some_and(|code| PRE_START_CODES.contains(&code))
}

/// An MCP call a workflow script made did not succeed, whether the transport
/// itself failed, the server returned an MCP-level tool error, or the call
/// was refused before it was issued. Reported to the script as a catchable
/// Rhai runtime error (`script.rs`); an uncaught one fails the run with
/// `status: failed` and an `error` field.
#[derive(Debug, Error)]
#[error("MCP call to {server:?}/{tool:?} failed: {message}")]
pub struct McpCallFailure {
    pub kind: WorkflowFailureKind,
    pub server: String,
    pub tool: String,
    pub message: String,
}

#[cfg(test)]
#[path = "caller_test.rs"]
mod tests;
