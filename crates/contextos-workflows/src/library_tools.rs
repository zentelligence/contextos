//! Which tools a workflow's libraries call, checked without running anything.
//!
//! A library declares in `mcp_tools` the tools its functions call: each exactly, or every tool on a server with the
//! tool name `*`. The declaration grants nothing: a call is checked at run time against the calling workflow's own
//! rules, so every tool a library the workflow imports from declares must be permitted by the workflow's
//! `[mcp_servers]` and `[mcp_tools]` lists, by the same rule a run applies, and a `*` is permitted only when the
//! workflow permits every tool on that server. Validation says so beforehand, naming the library and the tool, for the
//! libraries the workflow actually imports from, and for the libraries those import from in turn. It also reads every
//! `call_mcp` in a library whose server is a string literal, and reports a tool the library does not declare, exactly
//! or under a `*` for its server. A tool that is computed is accepted only under a `*` for its server, since nothing
//! else says which tool it is. A call whose server is computed is caught at run time.

use std::collections::{BTreeMap, BTreeSet};

use rhai::{ASTNode, Expr};

use crate::call_rules::{ANY_TOOL, CallRules, ToolRef};
use crate::import_rules::ModuleOwner;
use crate::package_name::PackageName;
use crate::script_engine::{on_script_thread, script_engine};
use crate::script_modules::WorkflowScript;
use crate::validation::ScriptSyntaxError;

/// What a check that could not run reports, as the syntax check does: against the entry script, with no position.
fn incomplete(why: &str) -> ScriptSyntaxError {
    ScriptSyntaxError {
        module: None,
        owner: None,
        message: format!("the library server check did not complete: {why}"),
        line: None,
        position: None,
    }
}

/// A tool a library declares that the workflow's rules do not permit, as a failure message.
fn unpermitted_message(library: &str, entry: &ToolRef) -> String {
    if entry.tool == ANY_TOOL {
        return format!(
            "library \"{library}\" declares every tool on server {:?} in `mcp_tools` and the workflow imports from it, \
             but the workflow does not permit every tool on that server (the server is denied or not allowed, or a tool \
             on it is denied or restricted by a tool allow list): a library grants nothing, so allow the whole server in \
             workflow.toml or have the library declare only the tools it calls",
            entry.server
        );
    }
    format!(
        "library \"{library}\" declares the tool {} on server {:?} in `mcp_tools` and the workflow imports from it, but \
         the workflow's `[mcp_servers]` and `[mcp_tools]` rules do not permit that call: a library grants nothing, so \
         allow it in workflow.toml",
        entry.tool, entry.server
    )
}

/// The `call_mcp` calls in `text` whose server is a string literal, each with its tool when that is a literal too.
fn literal_calls(engine: &rhai::Engine, text: &str) -> BTreeSet<(String, Option<String>)> {
    let mut named = BTreeSet::new();
    if let Ok(ast) = engine.compile(text) {
        ast.walk(&mut |nodes: &[ASTNode]| {
            if let Some(ASTNode::Expr(Expr::FnCall(call, _))) = nodes.last()
                && call.name == "call_mcp"
                && let Some(Expr::StringConstant(server, _)) = call.args.first()
            {
                let tool = match call.args.get(1) {
                    Some(Expr::StringConstant(tool, _)) => Some(tool.to_string()),
                    _ => None,
                };
                named.insert((server.to_string(), tool));
            }
            true
        });
    }
    named
}

/// Whether a call the library makes is among the tools it declares: the exact tool when the call names it, or every
/// tool on the server. A computed tool is declared only by every tool on the server.
fn declares(declared: &[ToolRef], server: &str, tool: Option<&str>) -> bool {
    declared
        .iter()
        .any(|entry| entry.server == server && (entry.tool == ANY_TOOL || tool == Some(entry.tool.as_str())))
}

/// Whether the workflow's rules permit what the library declares in `entry`: the tool, or every tool on the server.
fn permitted(rules: &CallRules, entry: &ToolRef) -> bool {
    if entry.tool == ANY_TOOL {
        rules.permits_every_tool(&entry.server)
    } else {
        rules.permits(&entry.server, &entry.tool)
    }
}

fn undeclared_message(module: &str, library: &str, server: &str, tool: Option<&str>) -> String {
    match tool {
        Some(tool) => format!(
            "{module} calls the tool {tool} on server {server:?}, which library \"{library}\" does not declare in its \
             `mcp_tools`"
        ),
        None => format!(
            "{module} calls server {server:?} with a computed tool, and library \"{library}\" does not declare every tool \
             on that server in its `mcp_tools`: declare `{{ server = {server:?}, tool = \"*\" }}`, or call the tool by its \
             name"
        ),
    }
}

/// A tool a library release declares that the workflow does not permit, or a call a library release makes that it
/// does not declare.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseToolFailure {
    pub library: PackageName,
    /// The library file the failure concerns, as `library:path`, or the library's `library.toml` for a server the workflow
    /// does not declare.
    pub artefact: String,
    pub server: String,
    /// The tool, when the failure names one.
    pub tool: Option<String>,
    pub message: String,
}

/// The failures in the library releases `script` reaches, given the servers the workflow release declares and the servers
/// each library release lists in its `library.toml`.
///
/// A library is reached when a file the walk from the entry script visits has an `import` statement that names it,
/// directly or through another reached library. The walk covers the packages of a release: its own files and the library
/// releases its lock names, or the source libraries a run reads.
///
/// # Errors
///
/// Returns an error, never an empty list, when the check could not run: the thread could not be started or the check
/// panicked. A check that did not run says nothing, so it is never taken for a pass.
pub fn check_release_library_tools(
    script: &WorkflowScript,
    library_tools: &BTreeMap<PackageName, Vec<ToolRef>>,
    rules: &CallRules,
) -> Result<Vec<ReleaseToolFailure>, ScriptSyntaxError> {
    check_release_with(script, library_tools, rules, on_script_thread)
}

/// What starts the thread a release check runs on.
type StartRelease = fn(
    Box<dyn FnOnce() -> Vec<ReleaseToolFailure> + Send>,
) -> std::io::Result<std::thread::JoinHandle<Vec<ReleaseToolFailure>>>;

fn check_release_with(
    script: &WorkflowScript,
    library_tools: &BTreeMap<PackageName, Vec<ToolRef>>,
    rules: &CallRules,
    start: StartRelease,
) -> Result<Vec<ReleaseToolFailure>, ScriptSyntaxError> {
    if library_tools.is_empty() {
        return Ok(Vec::new());
    }
    let owned = script.clone();
    let listed = library_tools.clone();
    let declared = rules.clone();
    start(Box::new(move || {
        check_release_on_this_thread(&owned, &listed, &declared)
    }))
    .map_err(|error| incomplete(&error.to_string()))
    .and_then(|thread| thread.join().map_err(|_panic| incomplete("the check panicked")))
}

fn check_release_on_this_thread(
    script: &WorkflowScript,
    library_tools: &BTreeMap<PackageName, Vec<ToolRef>>,
    rules: &CallRules,
) -> Vec<ReleaseToolFailure> {
    let engine = script_engine();
    let mut failures = Vec::new();
    for library in script.packages_imported_from(&engine) {
        for entry in library_tools.get(&library).into_iter().flatten() {
            if !permitted(rules, entry) {
                failures.push(ReleaseToolFailure {
                    artefact: format!("{library}:library.toml"),
                    server: entry.server.clone(),
                    tool: Some(entry.tool.clone()),
                    message: unpermitted_message(library.as_str(), entry),
                    library: library.clone(),
                });
            }
        }
    }
    for (library, declared) in library_tools {
        for module in script
            .modules()
            .filter(|module| *module.owner() == ModuleOwner::Package(library.clone()))
        {
            for (server, tool) in literal_calls(&engine, module.text()) {
                if !declares(declared, &server, tool.as_deref()) {
                    failures.push(ReleaseToolFailure {
                        library: library.clone(),
                        artefact: format!("{library}:library.toml"),
                        message: undeclared_message(
                            &module.path().to_string(),
                            library.as_str(),
                            &server,
                            tool.as_deref(),
                        ),
                        server,
                        tool,
                    });
                }
            }
        }
    }
    failures
}

#[cfg(test)]
#[path = "library_tools_test.rs"]
mod tests;
