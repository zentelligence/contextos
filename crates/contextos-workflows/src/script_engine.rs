//! The one Rhai engine configuration every workflow script is compiled and run with.

use rhai::{Engine, LexError, OptimizationLevel, ParseErrorType};

use crate::script_modules::{DeclaredModuleResolver, WorkflowScript};

// Rhai's own defaults for these limits differ between a debug and a release build of the server, so a script valid in
// one could fail in the other. Each is set to Rhai's release default, so no script that runs in production today stops
// running.

/// The deepest an expression may nest at the top level of a script.
pub(crate) const MAX_EXPR_DEPTH: usize = 64;
/// The deepest an expression may nest inside a function body.
pub(crate) const MAX_FUNCTION_EXPR_DEPTH: usize = 32;
/// The deepest function calls may go.
pub(crate) const MAX_CALL_LEVELS: usize = 64;
/// What every refused `eval` says, and what to do instead.
pub(crate) const EVAL_REFUSED: &str = "a workflow script cannot evaluate text as code: write the code in the script, and \
     read rules a job should change without a new version as data, such as TOML or JSON";
/// The stack a script is compiled and run on. Rhai recurses on the native stack for every level of nesting and every
/// call, and a debug build's frames are several times a release build's, so a 2 MiB thread, tokio's blocking default,
/// overflows and aborts the whole process before a debug build reaches the limits above. This holds them in either
/// build with room to spare; it is reserved address space, committed only as far as a script actually goes.
const SCRIPT_STACK_BYTES: usize = 32 * 1024 * 1024;

/// Starts `work` on a thread of its own with a stack that holds a script at every limit above. Every compile and run of
/// a script goes through it.
///
/// # Errors
///
/// Returns the error the operating system gave when the thread could not be started.
pub(crate) fn on_script_thread<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<T>> {
    std::thread::Builder::new()
        .name("workflow-script".to_owned())
        .stack_size(SCRIPT_STACK_BYTES)
        .spawn(work)
}

/// The engine every script is compiled and run with, so a check made before a run sees what the run will see.
///
/// Rhai's optimiser inlines a constant's value into the script, so `inputs.since = 1` becomes an assignment to a literal,
/// which reaches an `unreachable!` inside Rhai and panics the run. Without the optimiser the same script fails as an
/// ordinary "cannot modify a constant" error, which a script must never be able to avoid.
pub(crate) fn script_engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_optimization_level(OptimizationLevel::None);
    engine.set_max_expr_depths(MAX_EXPR_DEPTH, MAX_FUNCTION_EXPR_DEPTH);
    engine.set_max_call_levels(MAX_CALL_LEVELS);
    // Rhai's default resolver reads any `.rhai` file on the host, by absolute path or relative to the process's working
    // directory, which would let a script reach past its package or vault and run code its version does not cover.
    // This one serves the built-in library alone; a run replaces it with one that also serves its declared modules.
    engine.set_module_resolver(DeclaredModuleResolver::new(WorkflowScript::default()));
    // `eval` runs text the script has read while it runs, such as a workflow file or a note, which neither validation
    // nor the job's version covers. Refusing the keyword refuses every way of reaching it, by name or through `Fn`.
    engine.disable_symbol("eval");
    // Rhai's own `print` and `debug` write to standard output unless told otherwise, which a script must not be able
    // to use. A check compiles with this engine and never runs, so these are inert there.
    engine.on_print(|_| {});
    engine.on_debug(|_, _, _| {});
    engine
}

/// Why the engine refused a parse that failed on a symbol it disables, so the message can say so and what to do
/// instead, or nothing for any other parse failure.
pub(crate) fn refusal(kind: &ParseErrorType) -> Option<&'static str> {
    match kind {
        ParseErrorType::BadInput(LexError::ImproperSymbol(word, _)) if word == "eval" => Some(EVAL_REFUSED),
        _ => None,
    }
}

#[cfg(test)]
#[path = "script_engine_test.rs"]
mod tests;
