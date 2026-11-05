//! Executes one workflow run's `entry.rhai` script.
//!
//! A script has one host capability that acts outside itself: `call_mcp(server, tool,
//! args)`, dispatched through the caller's own [`McpCaller`]. It can read, never write, the files its own workflow
//! declares, through `workflow_file(path)` and `workflow_files(prefix)` ([`WorkflowFiles`]). It also has
//! `log_debug`, `log_info`, `log_warn`, `log_error`, and `flag_degraded`, which write
//! only to the run's log sink and its list of reasons, and `set_output`, which keeps a declared output. Rhai's own
//! `print` and `debug` write to the run's log sink too, never to standard output. Nothing else
//! is registered, so a script has no `std::fs`, no `std::net`, and no
//! process access beyond those functions - not a bypassable runtime
//! check, simply a function that does not exist in its scope.
//!
//! Rhai's own evaluation is synchronous and potentially long-running (up
//! to `timeout`), so it never runs on the async executor thread
//! (`rust-quality.md`): [`run_workflow_declaring`] moves it onto a blocking thread via
//! `tokio::task::spawn_blocking`, and the registered `call_mcp` function
//! bridges back into async by blocking that same thread on the captured
//! [`tokio::runtime::Handle`] - never the other way around.

use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use rhai::{Array, Dynamic, Engine, EvalAltResult, Map, Position, Scope};
use time::Duration;
use tokio_util::sync::CancellationToken;

use crate::bridge::{argument_error, arguments_to_json, result_to_dynamic};
use crate::caller::{McpCaller, WorkflowFailureKind};
use crate::clock::Clock;
use crate::inputs::ValidatedInputs;
use crate::logging::{
    DegradedReasons, LogLevel, MAX_MESSAGE_BYTES, RunReport, ScriptLogSink, TracingLogSink, cut_to_bytes,
};
use crate::outputs::{DeclaredOutputs, OutputStore, OutputValues, register_set_output};
use crate::script_engine::{on_script_thread, refusal, script_engine};
use crate::script_modules::{
    DeclaredModuleResolver, ENTRY_IMPORT_NOT_TOP_LEVEL, WorkflowScript, import_inside_a_function,
};
use crate::workflow_files::{WorkflowFile, WorkflowFileFailure, WorkflowFiles};

/// A completed workflow run: `status: succeeded`, `status: failed`, `status: degraded` (succeeded with warnings), or
/// `status: cancelled`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunOutcome {
    Succeeded,
    /// The run completed without failing, but its script counted at least one reason it is degraded.
    Degraded(DegradedReasons),
    Failed(RunFailure),
    /// The operator cancelled the run. Calls already made stay made: nothing is undone.
    Cancelled,
}

/// Why a run failed. `kind` is set when the failure is one of the closed set of `workflow/` kinds, and absent only for
/// a failure raised by a host that has not classified it. `message` is the complete text and already begins with the
/// kind's code when there is one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunFailure {
    pub kind: Option<WorkflowFailureKind>,
    pub message: String,
}

impl From<String> for RunFailure {
    fn from(message: String) -> Self {
        Self { kind: None, message }
    }
}

impl fmt::Display for RunFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// How a workflow run ended, with what the script set and whether the script began.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunResult {
    pub outcome: RunOutcome,
    /// What the script set, once the run `Succeeded` or was `Degraded`. A run that ended any other way returns no
    /// values, because values from a run that did not finish could be acted on as if they were complete.
    pub outputs: Option<OutputValues>,
    /// The names of the outputs the script set, whatever the outcome, so a record can name them without their values.
    pub output_names: Vec<String>,
    /// Whether the script began. A run whose token was already cancelled when it was handed over never did.
    pub started: bool,
}

impl RunResult {
    /// The result of a run that ended before its script began, for a reason such as a scope that could not be built.
    #[must_use]
    pub const fn ended_before_start(outcome: RunOutcome) -> Self {
        Self {
            outcome,
            outputs: None,
            output_names: Vec::new(),
            started: false,
        }
    }
}

impl From<RunOutcome> for RunResult {
    fn from(outcome: RunOutcome) -> Self {
        Self {
            outcome,
            outputs: None,
            output_names: Vec::new(),
            started: true,
        }
    }
}

/// What bounds a run and where its script's log messages go.
#[derive(Clone)]
pub struct RunSettings {
    /// The wall-clock bound, measured through the run's clock.
    pub timeout: Duration,
    /// The minimum level a script's messages are written at.
    pub log_level: LogLevel,
    /// Where kept messages go.
    pub sink: Arc<dyn ScriptLogSink>,
    /// Cancelling this token stops the run: the script ends at its next step, which no script can catch or outlast,
    /// and a call in flight is abandoned.
    pub cancel: CancellationToken,
}

impl RunSettings {
    /// Settings with `timeout`, the default `info` minimum, and messages written through `tracing`.
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            log_level: LogLevel::Info,
            sink: Arc::new(TracingLogSink::default()),
            cancel: CancellationToken::new(),
        }
    }
}

/// Runs `script`, an entry script and the modules it may import, to completion, until `settings.timeout` elapses, or until `settings.cancel` is cancelled, for a
/// workflow that declares `declared` outputs its script sets with `set_output(name, value)`.
///
/// Every `call_mcp(server, tool, args)` the script issues dispatches
/// through `caller`, and every `workflow_file` and `workflow_files` reads through `files`; the wall-clock bound is
/// measured through `clock`
/// (never the system clock read directly, so a test can drive it without
/// a real sleep, `testing.md`).
///
/// Each value is validated against its declared kind as it is set, and each required output must have been set by the
/// time the script completes. A run whose token is already cancelled when it is handed over never starts its script.
pub async fn run_workflow_declaring(
    script: impl Into<WorkflowScript>,
    inputs: ValidatedInputs,
    declared: DeclaredOutputs,
    caller: Arc<dyn McpCaller>,
    files: WorkflowFiles,
    clock: Arc<dyn Clock>,
    settings: RunSettings,
) -> RunResult {
    if settings.cancel.is_cancelled() {
        return RunResult::ended_before_start(RunOutcome::Cancelled);
    }
    let script = script.into();
    let handle = tokio::runtime::Handle::current();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    // A thread of its own, with a stack that holds a script at the engine's limits in every build, rather than one of
    // tokio's blocking threads, whose stack a debug build overflows before it reaches them.
    let started = on_script_thread(move || {
        let evaluation = evaluate(
            script,
            &inputs,
            &declared,
            Host { caller, files },
            clock,
            &settings,
            handle,
        );
        // The receiver is gone only when the run itself was dropped, so there is no one left to tell.
        let _ = sender.send(evaluation);
    });
    let evaluated = match started {
        Ok(_detached) => receiver.await.ok(),
        Err(error) => {
            tracing::warn!(%error, "the thread a workflow script runs on could not be started");
            None
        }
    };
    let Some(evaluation) = evaluated else {
        return RunResult::from(RunOutcome::Failed(RunFailure {
            kind: Some(WorkflowFailureKind::Internal),
            message: format!(
                "{}: the task running the workflow did not complete",
                WorkflowFailureKind::Internal.code()
            ),
        }));
    };
    let Evaluation { result, outputs } = evaluation;
    let output_names = outputs.keys().cloned().collect();
    let (outcome, keep_outputs) = match result {
        Ok(report) => match report.into_reasons() {
            Some(reasons) => (RunOutcome::Degraded(reasons), true),
            None => (RunOutcome::Succeeded, true),
        },
        Err(Stopped::Failed(failure)) => (RunOutcome::Failed(failure), false),
        Err(Stopped::Cancelled) => (RunOutcome::Cancelled, false),
    };
    RunResult {
        outcome,
        outputs: keep_outputs.then(|| OutputValues::from(outputs)),
        output_names,
        started: true,
    }
}

/// What a run's script reaches outside itself through.
struct Host {
    caller: Arc<dyn McpCaller>,
    files: WorkflowFiles,
}

/// What evaluating a script left: how it ended, and the outputs it had set by then.
struct Evaluation {
    result: Result<RunReport, Stopped>,
    outputs: serde_json::Map<String, serde_json::Value>,
}

fn evaluate(
    script: WorkflowScript,
    inputs: &ValidatedInputs,
    declared: &DeclaredOutputs,
    host: Host,
    clock: Arc<dyn Clock>,
    settings: &RunSettings,
    handle: tokio::runtime::Handle,
) -> Evaluation {
    let mut engine = script_engine();
    let entry = script.entry().to_owned();
    engine.set_module_resolver(DeclaredModuleResolver::new(script));
    let last_failure = LastCallFailure::default();
    let report = Arc::new(Mutex::new(RunReport::default()));
    let deadline = Deadline {
        start: clock.now(),
        clock,
        timeout: settings.timeout,
    };
    let waits = Waits {
        handle,
        cancel: settings.cancel.clone(),
        deadline: deadline.clone(),
    };
    register_call_mcp(&mut engine, host.caller, waits.clone(), Arc::clone(&last_failure));
    register_workflow_files(&mut engine, host.files, waits, Arc::clone(&last_failure));
    register_logging(&mut engine, settings, &report);
    let store = OutputStore::default();
    register_set_output(&mut engine, declared, &store, &last_failure);
    register_timeout(&mut engine, deadline, settings.cancel.clone());
    let mut scope = Scope::new();
    scope.push_constant(
        "inputs",
        result_to_dynamic(serde_json::Value::Object(inputs.values().clone())).into_read_only(),
    );
    let result = entry_ast(&engine, &entry)
        .and_then(|ast| engine.run_ast_with_scope(&mut scope, &ast))
        .map_err(|error| stopped_by(*error, &last_failure))
        .and_then(|()| {
            let set = store.lock().unwrap_or_else(PoisonError::into_inner);
            match declared.missing_required(&set) {
                Some(message) => Err(Stopped::Failed(RunFailure {
                    kind: Some(WorkflowFailureKind::OutputInvalid),
                    message,
                })),
                None => Ok(std::mem::take(
                    &mut *report.lock().unwrap_or_else(PoisonError::into_inner),
                )),
            }
        });
    let outputs = declared.in_declaration_order(std::mem::take(
        &mut *store.lock().unwrap_or_else(PoisonError::into_inner),
    ));
    Evaluation { result, outputs }
}

/// The entry script compiled, refused when a function of it imports: a run does not depend on validation having
/// happened, and nothing of the script runs before it is refused.
fn entry_ast(engine: &Engine, entry: &str) -> Result<rhai::AST, Box<EvalAltResult>> {
    let ast = engine
        .compile(entry)
        .map_err(|error| Box::new(EvalAltResult::from(error)))?;
    match import_inside_a_function(&ast) {
        Some(position) => Err(Box::new(EvalAltResult::ErrorRuntime(
            ENTRY_IMPORT_NOT_TOP_LEVEL.into(),
            position,
        ))),
        None => Ok(ast),
    }
}

/// Why a script that did not finish ended, from the error Rhai ended it with.
fn stopped_by(error: EvalAltResult, last_failure: &LastCallFailure) -> Stopped {
    match error {
        // `EvalAltResult`'s own `Display` renders this as the generic "Script terminated", dropping the
        // `on_progress` token this crate set to `register_timeout`'s own message; recovering it here keeps the run's
        // recorded `error` actionable.
        EvalAltResult::ErrorTerminated(token, _) if is_cancellation(&token) => Stopped::Cancelled,
        EvalAltResult::ErrorTerminated(..) => Stopped::Failed(RunFailure {
            kind: Some(WorkflowFailureKind::TimedOut),
            message: format!("{}: {TIMEOUT_MESSAGE}", WorkflowFailureKind::TimedOut.code()),
        }),
        EvalAltResult::ErrorRuntime(payload, position) => {
            Stopped::Failed(match failure_thrown_by_a_call(&payload, last_failure) {
                Some(failure) => failure,
                None => script_error(&EvalAltResult::ErrorRuntime(payload, position).to_string()),
            })
        }
        EvalAltResult::ErrorParsing(kind, position) => Stopped::Failed(script_error(&refusal(&kind).map_or_else(
            || EvalAltResult::ErrorParsing(kind.clone(), position).to_string(),
            |why| format!("Syntax error: {why} ({position})"),
        ))),
        other => Stopped::Failed(script_error(&other.to_string())),
    }
}

/// A failure of the script's own, as opposed to one of a call it made.
fn script_error(text: &str) -> RunFailure {
    RunFailure {
        kind: Some(WorkflowFailureKind::ScriptError),
        message: format!("{}: {text}", WorkflowFailureKind::ScriptError.code()),
    }
}

/// Why evaluation ended before the script finished.
enum Stopped {
    Failed(RunFailure),
    Cancelled,
}

/// What a terminated script carries when its run was cancelled, so it is never mistaken for a timeout.
const CANCELLED_TOKEN: &str = "workflow run cancelled";

const TIMEOUT_MESSAGE: &str = "workflow run exceeded its timeout";

fn is_cancellation(token: &Dynamic) -> bool {
    token
        .clone()
        .try_cast::<String>()
        .is_some_and(|text| text == CANCELLED_TOKEN)
}

/// The text thrown to the script by the most recent failed `call_mcp`, `workflow_file`, or `workflow_files`, with that
/// failure's kind.
pub(crate) type LastCallFailure = Arc<Mutex<Option<(String, WorkflowFailureKind)>>>;

/// The failed call a run ended on, when the runtime error that ended it is exactly the text that call threw: the
/// kind then comes from the call itself and never from reading the text. A script that caught the failure and threw
/// something else, or failed for its own reasons, matches nothing and is a script error.
fn failure_thrown_by_a_call(payload: &Dynamic, last_failure: &LastCallFailure) -> Option<RunFailure> {
    let thrown = payload.clone().try_cast::<String>()?;
    let last = last_failure.lock().unwrap_or_else(PoisonError::into_inner);
    let (text, kind) = last.as_ref()?;
    (*text == thrown).then_some(RunFailure {
        kind: Some(*kind),
        message: thrown,
    })
}

/// Aborts evaluation once `clock.now()` has advanced `timeout` past this
/// call's own start: `Engine::on_progress` fires periodically as the
/// script runs, giving an unbounded loop a bound
/// without relying on any wall-clock sleep in a test driving this through
/// a fake, monotonically-advancing `clock`.
///
/// When a run must end, measured through the run's own clock.
#[derive(Clone)]
struct Deadline {
    clock: Arc<dyn Clock>,
    start: time::OffsetDateTime,
    timeout: Duration,
}

/// The longest one wait for the deadline lasts before the clock is read again, so a clock that is not the runtime's own
/// is still noticed.
const DEADLINE_RECHECK: std::time::Duration = std::time::Duration::from_secs(1);

impl Deadline {
    fn passed(&self) -> bool {
        self.remaining() <= Duration::ZERO
    }

    fn remaining(&self) -> Duration {
        self.timeout - (self.clock.now() - self.start)
    }

    /// Completes once the deadline has passed. A call that never answers is abandoned by racing it against this.
    async fn reached(&self) {
        while !self.passed() {
            let wait = std::time::Duration::try_from(self.remaining()).unwrap_or(DEADLINE_RECHECK);
            tokio::time::sleep(wait.min(DEADLINE_RECHECK)).await;
        }
    }
}

/// The same hook ends the run when `cancel` is cancelled. Termination cannot be caught by a script, so a script can
/// neither outlast its timeout nor its cancellation, and the two are told apart by what the terminated script carries.
fn register_timeout(engine: &mut Engine, deadline: Deadline, cancel: CancellationToken) {
    engine.on_progress(move |_operations| {
        if cancel.is_cancelled() {
            Some(Dynamic::from(CANCELLED_TOKEN.to_owned()))
        } else if deadline.passed() {
            Some(Dynamic::from(TIMEOUT_MESSAGE.to_owned()))
        } else {
            None
        }
    });
}

/// Registers `log_debug`, `log_info`, `log_warn`, `log_error`, and `flag_degraded`.
///
/// A warning or error always counts as a reason the run is degraded, whether or not the minimum level lets the
/// message be written, because what a script reports as a problem must not depend on how verbose the host is.
fn register_logging(engine: &mut Engine, settings: &RunSettings, report: &Arc<Mutex<RunReport>>) {
    for (name, level) in [
        ("log_debug", LogLevel::Debug),
        ("log_info", LogLevel::Info),
        ("log_warn", LogLevel::Warn),
        ("log_error", LogLevel::Error),
    ] {
        let minimum = settings.log_level;
        let sink = Arc::clone(&settings.sink);
        let report = Arc::clone(report);
        engine.register_fn(name, move |message: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let text = text_argument(name, message)?;
            if level >= LogLevel::Warn {
                report.lock().unwrap_or_else(PoisonError::into_inner).note(&text);
            }
            if level.is_kept_at(minimum) {
                sink.emit(level, &cut_to_bytes(&text, MAX_MESSAGE_BYTES));
            }
            Ok(())
        });
    }
    // Rhai's `print` and `debug` are log messages at `info` and `debug`: kept by the same minimum level and cut to the
    // same limit, and never a reason the run is degraded.
    for (level, hook) in [(LogLevel::Info, true), (LogLevel::Debug, false)] {
        let minimum = settings.log_level;
        let sink = Arc::clone(&settings.sink);
        let emit = move |text: &str| {
            if level.is_kept_at(minimum) {
                sink.emit(level, &cut_to_bytes(text, MAX_MESSAGE_BYTES));
            }
        };
        if hook {
            engine.on_print(emit);
        } else {
            // Rhai's own `debug` renders a string in quotes; a script author expects the text as written, as `print`
            // gives it, so the quotes around a whole string are dropped.
            engine.on_debug(move |text, _source, _position| {
                emit(
                    text.strip_prefix('"')
                        .and_then(|inner| inner.strip_suffix('"'))
                        .unwrap_or(text),
                );
            });
        }
    }
    let report = Arc::clone(report);
    engine.register_fn(
        "flag_degraded",
        move |reason: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let text = text_argument("flag_degraded", reason)?;
            report.lock().unwrap_or_else(PoisonError::into_inner).note(&text);
            Ok(())
        },
    );
}

/// The string a logging function was given, or a script error naming the function.
fn text_argument(function: &str, value: Dynamic) -> Result<String, Box<EvalAltResult>> {
    value.into_string().map_err(|found| {
        Box::new(EvalAltResult::ErrorRuntime(
            Dynamic::from(format!("{function} expects a string, found {found}")),
            Position::NONE,
        ))
    })
}

/// How a host function waits for an answer: on the run's blocking thread, for no longer than the run's timeout, and
/// never past its cancellation.
#[derive(Clone)]
struct Waits {
    handle: tokio::runtime::Handle,
    cancel: CancellationToken,
    deadline: Deadline,
}

impl Waits {
    /// Blocks until `work` answers. A run cancelled or out of time first terminates the script, which it cannot
    /// catch, and drops `work`, which abandons it.
    fn answer<T>(&self, work: impl Future<Output = T>) -> Result<T, Box<EvalAltResult>> {
        let cancel = self.cancel.clone();
        let deadline = self.deadline.clone();
        self.handle
            .block_on(async move {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => Err(CANCELLED_TOKEN),
                    () = deadline.reached() => Err(TIMEOUT_MESSAGE),
                    answer = work => Ok(answer),
                }
            })
            .map_err(|reason| {
                Box::new(EvalAltResult::ErrorTerminated(
                    Dynamic::from(reason.to_owned()),
                    Position::NONE,
                ))
            })
    }
}

/// Throws `thrown` to the script as a catchable error, remembering its kind in case it ends the run.
pub(crate) fn throw(thrown: String, kind: WorkflowFailureKind, last_failure: &LastCallFailure) -> EvalAltResult {
    *last_failure.lock().unwrap_or_else(PoisonError::into_inner) = Some((thrown.clone(), kind));
    EvalAltResult::ErrorRuntime(Dynamic::from(thrown), Position::NONE)
}

fn register_call_mcp(engine: &mut Engine, caller: Arc<dyn McpCaller>, waits: Waits, last_failure: LastCallFailure) {
    engine.register_fn(
        "call_mcp",
        move |server: String, tool: String, arguments: Map| -> Result<Dynamic, Box<EvalAltResult>> {
            let json_arguments = arguments_to_json(arguments).map_err(argument_error)?;
            let caller = Arc::clone(&caller);
            // Dropping the call's future abandons it; whether the server acted on it is not known. Either way the
            // script is terminated, which it cannot catch, so a hung call ends the run when its timeout passes.
            let answer = waits.answer(async move { caller.call(&server, &tool, json_arguments).await })?;
            answer.map(result_to_dynamic).map_err(|failure| {
                Box::new(throw(
                    format!("{}: {failure}", failure.kind.code()),
                    failure.kind,
                    &last_failure,
                ))
            })
        },
    );
}

/// Registers `workflow_file(path)`, `workflow_files()`, and `workflow_files(prefix)`: reads of the files the run's
/// workflow declares, never writes. A refusal is thrown as `<code>: <message>`, as a failed call is.
fn register_workflow_files(engine: &mut Engine, files: WorkflowFiles, waits: Waits, last_failure: LastCallFailure) {
    let refused = |last_failure: &LastCallFailure, failure: WorkflowFileFailure| {
        Box::new(throw(failure.to_string(), failure.kind, last_failure))
    };
    {
        let files = files.clone();
        let waits = waits.clone();
        let last_failure = Arc::clone(&last_failure);
        engine.register_fn(
            "workflow_file",
            move |path: &str| -> Result<Dynamic, Box<EvalAltResult>> {
                waits
                    .answer(files.read(path))?
                    .map(Into::<Dynamic>::into)
                    .map_err(|failure| refused(&last_failure, failure))
            },
        );
    }
    let list = move |prefix: &str| -> Result<Array, Box<EvalAltResult>> {
        waits
            .answer(files.list(prefix))?
            .map(|paths| paths.into_iter().map(Dynamic::from).collect())
            .map_err(|failure| refused(&last_failure, failure))
    };
    let list_all = list.clone();
    engine.register_fn("workflow_files", move || list_all(""));
    engine.register_fn("workflow_files", list);
}

impl From<WorkflowFile> for Dynamic {
    /// The shape a script already gets for a file from `fs_attach_file` through `call_mcp`.
    fn from(file: WorkflowFile) -> Self {
        let mut map = Map::new();
        map.insert("content".into(), file.content.into());
        map.insert("content_hash".into(), <&str>::from(&file.content_hash).into());
        Self::from_map(map)
    }
}

/// Runs a script that declares no outputs and returns only how it ended, which is all most script tests assert.
#[cfg(test)]
async fn run_for_outcome(
    script_source: String,
    inputs: ValidatedInputs,
    caller: Arc<dyn McpCaller>,
    files: WorkflowFiles,
    clock: Arc<dyn Clock>,
    settings: RunSettings,
) -> RunOutcome {
    run_workflow_declaring(
        script_source,
        inputs,
        DeclaredOutputs::default(),
        caller,
        files,
        clock,
        settings,
    )
    .await
    .outcome
}

#[cfg(test)]
#[path = "script_test.rs"]
mod tests;

#[cfg(test)]
#[path = "script_logging_test.rs"]
mod logging_tests;

#[cfg(test)]
#[path = "script_files_test.rs"]
mod files_tests;
