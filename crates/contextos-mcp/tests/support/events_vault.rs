//! A Git-initialised vault with a task base and a few tasks behind a real server, and readers for the events it
//! writes, for the contract tests of vault events.
//!
//! Shared through `tests/support/mod.rs`; a test binary that uses only part of it would otherwise warn, which is
//! expected rather than genuine dead code.
#![allow(dead_code)]

use std::path::Path;

use serde_json::Value;

use super::git_harness::{BoxError, Harness, harness_with_clock};

/// The filters of the scaffold's task base, which is what decides whether a note is a task.
pub const TASKS_BASE: &str = "filters: file.inFolder(\"memory/tasks\") && file.basename != \"index\"\n\
views:\n  - type: table\n    name: All\n";

pub const TASK_PATH: &str = "memory/tasks/a-001.md";

/// A task note as the scaffold writes one.
pub fn task_note(id: &str, state: &str, extra: &str) -> String {
    format!(
        "---\ntype: task\ntask_type: action\nid: {id}\nentity: zenith\nstate: {state}\nassignee: ellis\n{extra}---\n\n# Task {id}\n"
    )
}

/// A managed, Git-initialised vault with the task base and one action `A-001` in `next`.
pub async fn events_harness() -> Result<Harness, BoxError> {
    events_harness_with(|_| {}, |_| Ok(())).await
}

pub async fn events_harness_with(
    configure: impl FnOnce(&mut contextos_mcp::Config),
    seed: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<Harness, BoxError> {
    events_harness_with_clock(configure, seed, contextos_mcp::ServerClock::default()).await
}

/// As [`events_harness_with`], with the server reading the time from `clock`.
pub async fn events_harness_with_clock(
    configure: impl FnOnce(&mut contextos_mcp::Config),
    seed: impl FnOnce(&Path) -> std::io::Result<()>,
    clock: contextos_mcp::ServerClock,
) -> Result<Harness, BoxError> {
    harness_with_clock(
        configure,
        |root| {
            std::fs::create_dir_all(root.join("memory/tasks"))?;
            std::fs::write(root.join("memory/tasks.base"), TASKS_BASE)?;
            std::fs::write(root.join(TASK_PATH), task_note("A-001", "next", ""))?;
            seed(root)
        },
        clock,
    )
    .await
}

/// Every event note under the vault's events folder, as `(vault-relative path, text)`, oldest path first.
pub fn event_notes(vault: &Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    collect(vault, &vault.join("memory/events"), &mut found);
    found.sort();
    found
}

fn collect(vault: &Path, directory: &Path, found: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(vault, &path, found);
        } else if path.extension().is_some_and(|extension| extension == "md")
            && path.file_name().is_some_and(|name| name != "index.md")
        {
            let relative = path
                .strip_prefix(vault)
                .map(|relative| relative.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            found.push((relative, std::fs::read_to_string(&path).unwrap_or_default()));
        }
    }
}

/// The event notes of one kind.
pub fn events_of_kind(vault: &Path, kind: &str) -> Vec<(String, String)> {
    let line = format!("\nkind: {kind}\n");
    event_notes(vault)
        .into_iter()
        .filter(|(_, text)| text.contains(&line))
        .collect()
}

/// The value of a frontmatter line in an event note, unquoted.
pub fn field(text: &str, name: &str) -> Option<String> {
    let prefix = format!("{name}: ");
    text.lines()
        .skip(1)
        .take_while(|line| *line != "---")
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.trim_matches('\'').to_owned())
}

/// The warnings of a tool result, as the strings the tool returned.
pub fn warnings(result: &Value) -> Vec<String> {
    result
        .get("warnings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|warning| warning.as_str().map(str::to_owned))
        .collect()
}

/// The scaffold's events base, scoped to the events folder.
pub const EVENTS_BASE: &str = concat!(
    "filters:\n",
    "  and:\n",
    "    - file.inFolder(\"memory/events\")\n",
    "    - note.type == \"event\"\n",
    "views:\n",
    "  - type: table\n",
    "    name: Recent\n",
);

/// The scaffold's seeded events sidecar: the kind registry, the `S` series mapped to the session start kind, and no default series.
pub const EVENTS_SIDECAR: &str = concat!(
    "---\n",
    "base: '[[memory/events.base|Events]]'\n",
    "last_create: 2026-10-28T00:00:00Z\n",
    "last_update: 2026-10-28T00:00:00Z\n",
    "key_prefixes:\n",
    "  S: 0\n",
    "key_format: '{key_prefix}-{key_id}'\n",
    "template: '[[templates/event|Event]]'\n",
    "fixed_fields: [type]\n",
    "event_kinds: registry/events\n",
    "series_kinds:\n",
    "  S: session.run-started\n",
    "---\n",
    "\n",
    "![[memory/events.base|Events]]\n",
);

pub const EVENT_TEMPLATE: &str = concat!(
    "---\n",
    "type: event\n",
    "kind: \"{{kind}}\"\n",
    "severity: \"{{info | warning | error}}\"\n",
    "at: \"{{RFC 3339 time in UTC}}\"\n",
    "entity: \"{{entity}}\"\n",
    "---\n",
    "\n",
    "# {{What happened}}\n",
);

/// The kind definition the plugin ships as `file`, when the marketplace repository is beside this one or
/// `CONTEXTOS_PLUGIN_DIR` names the plugin folder. The scaffold's own definitions are what a real vault holds, so tests prefer them to a copy
/// that could drift; `.claude/scripts/check-plugin-skill.sh` checks the shipped definitions against the server's rules.
fn shipped_kind(file: &str) -> Option<String> {
    let plugin = std::env::var_os("CONTEXTOS_PLUGIN_DIR").map_or_else(
        || std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../marketplace/plugins/context-os"),
        std::path::PathBuf::from,
    );
    std::fs::read_to_string(
        plugin
            .join("workflows/vault/templates/shipped/registry/events")
            .join(file),
    )
    .ok()
}

/// `session.run-started` as the plugin ships it.
pub fn run_started() -> String {
    shipped_kind("session.run-started.md").unwrap_or_else(|| RUN_STARTED_SHIPPED_SHAPE.to_owned())
}

/// `session.run-finished` as the plugin ships it.
pub fn run_finished() -> String {
    shipped_kind("session.run-finished.md").unwrap_or_else(|| RUN_FINISHED_SHIPPED_SHAPE.to_owned())
}

const RUN_STARTED_SHIPPED_SHAPE: &str = concat!(
    "---\n",
    "type: event-kind\n",
    "kind: session.run-started\n",
    "label: Session run started\n",
    "description: A scheduled agent session began working the task list.\n",
    "severities: [info]\n",
    "default_severity: info\n",
    "fields:\n",
    "  required: [members, scope]\n",
    "  optional: []\n",
    "subject: none\n",
    "id_field: run_id\n",
    "---\n",
    "\n",
    "# Event kind: Session run started\n",
);

const RUN_FINISHED_SHIPPED_SHAPE: &str = concat!(
    "---\n",
    "type: event-kind\n",
    "kind: session.run-finished\n",
    "label: Session run finished\n",
    "description: A scheduled agent session finished working the task list, or stopped early.\n",
    "severities: [info, warning, error]\n",
    "default_severity: info\n",
    "fields:\n",
    "  required: [run_id, members, dispatched, review, waiting]\n",
    "  optional: []\n",
    "subject: none\n",
    "---\n",
    "\n",
    "# Event kind: Session run finished\n",
);

pub const IMPORT_COMPLETED: &str = concat!(
    "---\n",
    "type: event-kind\n",
    "kind: import.completed\n",
    "label: Import completed\n",
    "description: A job loaded data from an external source.\n",
    "severities: [info, warning]\n",
    "default_severity: info\n",
    "fields:\n",
    "  required: [source]\n",
    "  optional: [rows]\n",
    "subject: optional\n",
    "retention_days: 400\n",
    "---\n",
    "\n",
    "# Event kind: Import completed\n",
);

/// Seeds the events base, sidecar, template, and the three kinds the contract tests record.
pub fn seed_events_scaffold(root: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(root.join("registry/events"))?;
    std::fs::create_dir_all(root.join("templates"))?;
    std::fs::write(root.join("memory/events.base"), EVENTS_BASE)?;
    std::fs::write(root.join("memory/events.md"), EVENTS_SIDECAR)?;
    std::fs::write(root.join("templates/event.md"), EVENT_TEMPLATE)?;
    std::fs::write(root.join("registry/events/session.run-started.md"), run_started())?;
    std::fs::write(root.join("registry/events/session.run-finished.md"), run_finished())?;
    std::fs::write(root.join("registry/events/import.completed.md"), IMPORT_COMPLETED)
}

/// A harness whose vault carries the events scaffold.
pub async fn scaffolded_harness() -> Result<Harness, BoxError> {
    events_harness_with(|_| {}, seed_events_scaffold).await
}
