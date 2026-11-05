//! Ensures `memory/jobs.base` exists once any job is registered
//!. The file's own richer views are the
//! `ContextOS` Obsidian plugin's job to scaffold, not `contextos-web`'s:
//! this module writes only a top-level folder-scoping filter (mirroring
//! `memory/tasks.base`'s own `file.inFolder("memory/tasks")` precedent)
//! plus the single minimal view the Bases schema itself requires to be
//! valid at all, and only the first time. Every call after that is a
//! deliberate no-op against whatever already exists, whether the plugin,
//! an operator, or an earlier call to this same function created it -
//! nothing here can ever clobber it.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::mcp_client::McpClient;

// A sibling of `memory/jobs/`, not nested inside it: the vault-wide job
// dashboard belongs at the same level as `memory/log/`, `memory/coding/`,
// `memory/sessions/`, and `memory/jobs/` itself, not buried inside the
// run-record tree it summarises. It therefore falls outside the
// `memory/jobs` prefix `[vault.index_md] exclude`/`[vault.search]
// exclude`/`[vault.git] restore_exclude` all use - deliberately: unlike
// the run records, this one small file is neither high-volume nor
// append-only, so it has no need of those exclusions.
const JOBS_BASE_PATH: &str = "memory/jobs.base";

#[derive(Debug, Default, Deserialize)]
struct ToolFailureBody {
    #[serde(default)]
    code: String,
}

/// The smallest definition that is both schema-valid and actually scoped
/// to job runs, no richer than that:
///
/// - A top-level `filters` expression restricting Base membership to
///   `memory/jobs/` (`file.inFolder`, the same construct
///   `memory/tasks.base` already uses for `memory/tasks/`), since with no
///   scoping filter this Base would otherwise draw in every note in the
///   vault once the plugin or an operator adds a view with no view-level
///   filter of its own.
/// - One view, since `contextos-obsidian`'s own schema
///   (`BaseDocument::diagnostics`) requires `views` to be non-empty, with
///   nothing beyond the two fields every view requires (`type`, `name`).
///
/// Anything richer, such as per-status columns, a recent-failures view,
/// or sorting, is the plugin's own scaffolding to add once it runs, not
/// this stub's job to anticipate.
fn minimal_definition() -> Map<String, Value> {
    let mut view = Map::new();
    view.insert("type".to_owned(), Value::String("table".to_owned()));
    view.insert("name".to_owned(), Value::String("All".to_owned()));
    let mut definition = Map::new();
    definition.insert(
        "filters".to_owned(),
        Value::String("file.inFolder(\"memory/jobs\")".to_owned()),
    );
    definition.insert("views".to_owned(), Value::Array(vec![Value::Object(view)]));
    definition
}

/// Creates `memory/jobs.base` as [`minimal_definition`] if and only
/// if nothing exists there yet. `base_create` is called with no
/// `expected_hash` and `force: false` (its own default), so
/// `contextos-fs`'s create/overwrite conflict rule
/// (`crates/contextos-fs/src/mutate.rs::validate_expected_hash`) does the
/// actual non-clobbering work: a pre-existing file fails the call with
/// the stable `io/expected-hash-required` code rather than being
/// overwritten, which this function treats as complete success, not a
/// warning. Any other failure is logged and left for the next poll to
/// retry, matching `scheduler.rs`'s own "degrade, never fail the whole
/// pass" discipline.
pub async fn ensure_jobs_base(client: &McpClient, vault_name: &str) {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{JOBS_BASE_PATH}")),
    );
    args.insert("definition".to_owned(), Value::Object(minimal_definition()));
    let Ok(result) = client.call_tool("base_create".to_owned(), args).await else {
        return;
    };
    if result.is_error != Some(true) {
        return;
    }
    let already_exists = result
        .into_typed::<ToolFailureBody>()
        .is_ok_and(|failure| failure.code == "io/expected-hash-required");
    if !already_exists {
        tracing::warn!(vault = vault_name, "failed to create the default jobs.base stub");
    }
}
