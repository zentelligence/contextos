//! The check of the tools a workflow release's locked library releases declare, as validation makes it.

use std::collections::BTreeMap;

use contextos_workflows::call_rules::{CallRules, ToolRef};
use contextos_workflows::{
    PackageName, ValidationCode, ValidationFailure, WorkflowScript, check_release_library_tools,
};

use crate::jobs::failure;

/// The failures in the tools the library releases a workflow release is locked to declare, given the calls the workflow
/// permits: a library the release's files import from, directly or through another library, that declares a tool the
/// workflow does not permit, and a literal `call_mcp` in a library to a tool it does not declare.
///
/// A library is reached by the same analysis of the code's `import` statements that checks a script's imports, applied
/// to the release's own files and the locked library releases, so every import form is found and a name that is only in a
/// comment or a string is not. `entry` is the release's entry script, which a check that could not run is reported
/// against. Blocking: it runs the analysis on a script thread.
pub(crate) fn release_library_tool_failures(
    script: &WorkflowScript,
    entry: &str,
    library_tools: &BTreeMap<PackageName, Vec<ToolRef>>,
    calls: &CallRules,
) -> Vec<ValidationFailure> {
    match check_release_library_tools(script, library_tools, calls) {
        Ok(found) => found
            .into_iter()
            .map(|found| failure(ValidationCode::ArtefactInvalid, &found.artefact, found.message))
            .collect(),
        // A check that did not run says nothing about the libraries, so it is never taken for a pass.
        Err(incomplete) => vec![failure(ValidationCode::ScriptSyntax, entry, incomplete.message)],
    }
}

/// As [`release_library_tool_failures`], found off the async executor, for a release read by either store.
pub(crate) async fn library_tool_failures(
    script: &WorkflowScript,
    entry: &str,
    library_tools: &BTreeMap<PackageName, Vec<ToolRef>>,
    calls: &CallRules,
) -> Vec<ValidationFailure> {
    let script = script.clone();
    let libraries = library_tools.clone();
    let calls = calls.clone();
    let entry_name = entry.to_owned();
    tokio::task::spawn_blocking(move || release_library_tool_failures(&script, &entry_name, &libraries, &calls))
        .await
        .unwrap_or_else(|join_error| {
            vec![failure(
                ValidationCode::ScriptSyntax,
                entry,
                format!("the library tool check did not complete: {join_error}"),
            )]
        })
}

#[cfg(test)]
#[path = "library_checks_test.rs"]
mod tests;
