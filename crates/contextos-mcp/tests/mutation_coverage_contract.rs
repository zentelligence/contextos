//! Every tool the server advertises is classified as read-only or mutating, and every mutating tool is shown to leave
//! a Git-initialised vault with exactly one new commit and a clean working tree. A tool added without being
//! classified fails here, so no tool can be added that writes to the vault around version control.

mod support;
use contextos_mcp::{ToolEffect, tool_names};
use rmcp::ServiceExt;
use serde_json::{Value, json};
use support::git_harness::{BoxError, Harness, assert_one_commit_and_clean_tree, harness_with};

/// Tools that only read: the vault is left exactly as it was, which the last test also checks by running them. The
/// classification is the server's own (`contextos_mcp::tool_names`), the same one host projection allows a read-only
/// agent, so there is one list to maintain. The `ephemeris_*` tools are left out because they are advertised only
/// under `[server] astro`, which these vaults leave off; `ephemeris_contract.rs` covers them.
fn read_only() -> Vec<&'static str> {
    tool_names(ToolEffect::ReadOnly)
        .into_iter()
        .filter(|name| !name.starts_with("ephemeris_"))
        .collect()
}

/// Tools that manage version control themselves: their own effect is a commit, so "one commit and a clean tree"
/// would be circular for them.
const VERSIONS_ITSELF: &[&str] = &["git_commit", "git_init", "git_restore"];

/// Mutating tools that only rebuild derived stores outside the vault (moving a damaged one aside), so they leave the
/// vault and its history untouched. The last test runs them beside the read-only tools to prove it.
const REBUILDS_DERIVED_STATE: &[&str] = &["query_index_rebuild"];

/// Mutating tools whose commit coverage is demonstrated in `git_coverage_contract.rs`, where the scenario needs its own
/// setup (a stale index to rebuild, a log to append to).
const COVERED_ELSEWHERE: &[&str] = &["doctor_resolve", "vault_index_rebuild", "vault_log_append"];

/// Mutating tools demonstrated by `every_mutating_tool_is_one_commit_and_a_clean_tree` below.
const MUTATING: &[&str] = &[
    "agent_state_change",
    "base_apply",
    "base_create",
    "base_record_create",
    "canvas_apply",
    "canvas_create",
    "frontmatter_link",
    "frontmatter_update",
    "fs_create_directory",
    "fs_delete_file",
    "fs_edit_file",
    "fs_move_file",
    "fs_write_file",
    "note_create",
    "note_update",
];

const SIDECAR: &str = "---\nkey_format: FILEPATH\nlast_id: 0\nlast_create: t0\nlast_update: t0\nlink_fields:\n  depends_on:\n    inverse: blocks\n  blocks:\n    inverse: depends_on\n---\n![[tasks.base|tasks]]\n";
const TASKS_BASE: &str = "filters: file.inFolder(\"tasks\")\nviews:\n- type: table\n  name: All\n";

fn note(id: &str) -> String {
    format!("---\nid: {id}\ntitle: {id} note\n---\nBody of {id}.\n")
}

async fn vault_for_every_tool() -> Result<Harness, BoxError> {
    let harness = harness_with(
        |config| config.vaults[0].git.destructive_delete = true,
        |root| {
            std::fs::create_dir_all(root.join("tasks"))?;
            std::fs::write(root.join("tasks.base"), TASKS_BASE)?;
            std::fs::write(root.join("tasks.md"), SIDECAR)?;
            std::fs::write(root.join("tasks/alpha.md"), note("Alpha"))?;
            std::fs::write(root.join("tasks/beta.md"), note("Beta"))?;
            std::fs::create_dir_all(root.join(".contextos"))?;
            std::fs::write(
                root.join(".contextos/team.toml"),
                "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\ndomain = \"Finance\"\n",
            )?;
            Ok(())
        },
    )
    .await?;
    // Indexes are brought up to date and committed first, so each scenario's index maintenance is part of it.
    harness.structured("vault_index_rebuild", json!({"path": "."})).await?;
    harness.settle()?;
    Ok(harness)
}

async fn advertised_tools(harness: &Harness) -> Result<Vec<String>, BoxError> {
    let (server_transport, client_transport) = tokio::io::duplex(1 << 20);
    let server = harness.server.clone();
    let handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), BoxError>(())
    });
    let mut client = ().serve(client_transport).await?;
    let mut names = client
        .list_all_tools()
        .await?
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect::<Vec<_>>();
    client.close().await?;
    handle.await??;
    names.sort();
    Ok(names)
}

#[tokio::test]
async fn every_advertised_tool_is_classified_exactly_once() -> Result<(), BoxError> {
    let harness = vault_for_every_tool().await?;
    let advertised = advertised_tools(&harness).await?;

    let mut classified = [
        read_only().as_slice(),
        VERSIONS_ITSELF,
        COVERED_ELSEWHERE,
        REBUILDS_DERIVED_STATE,
        MUTATING,
    ]
    .concat()
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    classified.sort();
    let duplicated = classified
        .windows(2)
        .filter(|pair| pair[0] == pair[1])
        .map(|pair| pair[0].clone())
        .collect::<Vec<_>>();
    classified.dedup();

    let unclassified = advertised
        .iter()
        .filter(|name| !classified.contains(name))
        .collect::<Vec<_>>();
    let stale = classified
        .iter()
        .filter(|name| !advertised.contains(name))
        .collect::<Vec<_>>();
    assert!(
        unclassified.is_empty(),
        "classify these new tools as read-only, or as mutating with a commit scenario: {unclassified:?}"
    );
    assert!(
        stale.is_empty(),
        "these classified tools are no longer advertised: {stale:?}"
    );
    assert!(duplicated.is_empty(), "classified more than once: {duplicated:?}");
    Ok(())
}

#[test]
fn the_scenarios_cover_exactly_the_tools_the_server_classifies_as_mutating() {
    let mut scenarios = [VERSIONS_ITSELF, COVERED_ELSEWHERE, REBUILDS_DERIVED_STATE, MUTATING].concat();
    scenarios.sort_unstable();
    assert_eq!(
        scenarios,
        tool_names(ToolEffect::Mutating),
        "a tool classified as mutating needs a commit scenario, and every scenario a mutating classification"
    );
}

/// Runs one mutating call and asserts it left one commit and a clean tree, noting the tool as exercised.
async fn one_operation(
    harness: &Harness,
    exercised: &mut Vec<&'static str>,
    tool: &'static str,
    arguments: Value,
) -> Result<Value, BoxError> {
    let before = harness.commit_count().await?;
    let result = harness.structured(tool, arguments).await?;
    if !exercised.contains(&tool) {
        exercised.push(tool);
    }
    assert_one_commit_and_clean_tree(harness, before, tool).await?;
    Ok(result)
}

async fn hash_of(harness: &Harness, path: &str) -> Result<String, BoxError> {
    let read = harness.structured("fs_read_text_file", json!({"path": path})).await?;
    read.get("content_hash")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{path} was read without a content_hash").into())
}

async fn filesystem_scenarios(harness: &Harness, exercised: &mut Vec<&'static str>) -> Result<(), BoxError> {
    one_operation(
        harness,
        exercised,
        "fs_write_file",
        json!({"path": "alpha/new.md", "content": "# New\n"}),
    )
    .await?;
    one_operation(
        harness,
        exercised,
        "fs_create_directory",
        json!({"path": "alpha/empty-dir"}),
    )
    .await?;
    let hash = hash_of(harness, "alpha/note.md").await?;
    one_operation(
        harness,
        exercised,
        "fs_edit_file",
        json!({"path": "alpha/note.md", "expected_hash": hash, "edits": [{"old_text": "# Note", "new_text": "# Note, edited"}]}),
    )
    .await?;
    one_operation(
        harness,
        exercised,
        "fs_move_file",
        json!({"source": "alpha/beta/leaf.md", "destination": "alpha/beta/leaf-moved.md"}),
    )
    .await?;
    one_operation(
        harness,
        exercised,
        "fs_delete_file",
        json!({"path": "alpha/beta/leaf-moved.md", "hard": true}),
    )
    .await?;
    Ok(())
}

async fn note_scenarios(harness: &Harness, exercised: &mut Vec<&'static str>) -> Result<(), BoxError> {
    one_operation(
        harness,
        exercised,
        "note_create",
        json!({"path": "alpha/created.md", "content": "# Created\n"}),
    )
    .await?;
    let hash = hash_of(harness, "alpha/created.md").await?;
    one_operation(
        harness,
        exercised,
        "frontmatter_update",
        json!({"path": "alpha/created.md", "expected_hash": hash, "patch": {"summary": "A created note"}}),
    )
    .await?;
    let hash = hash_of(harness, "alpha/created.md").await?;
    one_operation(
        harness,
        exercised,
        "note_update",
        json!({"path": "alpha/created.md", "expected_hash": hash, "patch": {"status": "active"}, "body": "# Created, revised\n"}),
    )
    .await?;
    one_operation(
        harness,
        exercised,
        "frontmatter_link",
        json!({"path": "tasks/alpha.md", "operations": [{"op": "add", "field": "depends_on", "target": "tasks/beta.md"}]}),
    )
    .await?;
    Ok(())
}

async fn structured_document_scenarios(harness: &Harness, exercised: &mut Vec<&'static str>) -> Result<(), BoxError> {
    one_operation(
        harness,
        exercised,
        "base_create",
        json!({"path": "notes.base", "definition": {"views": [{"type": "table", "name": "All"}]}}),
    )
    .await?;
    let hash = hash_of(harness, "notes.base").await?;
    one_operation(
        harness,
        exercised,
        "base_apply",
        json!({"path": "notes.base", "expected_hash": hash, "operations": [{"op": "set_property", "name": "status", "property": {"displayName": "Status"}}]}),
    )
    .await?;
    one_operation(
        harness,
        exercised,
        "base_record_create",
        json!({"path": "tasks.base", "filename": "gamma.md"}),
    )
    .await?;
    one_operation(
        harness,
        exercised,
        "canvas_create",
        json!({"path": "map.canvas", "nodes": [{"id": "one", "type": "text", "text": "One", "x": 0, "y": 0, "width": 200, "height": 100}], "edges": []}),
    )
    .await?;
    let hash = hash_of(harness, "map.canvas").await?;
    one_operation(
        harness,
        exercised,
        "canvas_apply",
        json!({"path": "map.canvas", "expected_hash": hash, "operations": [{"op": "remove_node", "id": "one"}]}),
    )
    .await?;
    one_operation(
        harness,
        exercised,
        "agent_state_change",
        json!({"agent": "atlas", "state": "working"}),
    )
    .await?;

    Ok(())
}

#[tokio::test]
async fn every_mutating_tool_is_one_commit_and_a_clean_tree() -> Result<(), BoxError> {
    let harness = vault_for_every_tool().await?;
    let mut exercised = Vec::new();

    filesystem_scenarios(&harness, &mut exercised).await?;
    note_scenarios(&harness, &mut exercised).await?;
    structured_document_scenarios(&harness, &mut exercised).await?;

    let mut expected = MUTATING.to_vec();
    expected.sort_unstable();
    exercised.sort_unstable();
    assert_eq!(
        exercised, expected,
        "every classified mutating tool needs a scenario, and every scenario a classification"
    );
    Ok(())
}

#[tokio::test]
async fn read_only_tools_leave_the_vault_and_its_history_untouched() -> Result<(), BoxError> {
    let harness = vault_for_every_tool().await?;
    let commits = harness.commit_count().await?;
    let calls = [
        ("base_query", json!({"path": "tasks.base"})),
        ("base_read", json!({"path": "tasks.base"})),
        ("canvas_read", json!({"path": "missing.canvas"})),
        ("doctor", json!({})),
        ("frontmatter_read", json!({"path": "tasks/alpha.md"})),
        ("fs_attach_file", json!({"path": "alpha/note.md"})),
        ("fs_directory_tree", json!({"path": "alpha"})),
        ("fs_get_file_info", json!({"path": "alpha/note.md"})),
        ("fs_list_allowed_directories", json!({})),
        ("fs_list_directory", json!({"path": "alpha"})),
        ("fs_read_multiple_files", json!({"paths": ["alpha/note.md"]})),
        ("fs_read_text_file", json!({"path": "alpha/note.md"})),
        ("fs_search_files", json!({"path": "alpha", "pattern": "note"})),
        ("git_diff", json!({})),
        ("git_log", json!({"limit": 5})),
        ("git_status", json!({})),
        ("links_read", json!({"path": "tasks/alpha.md"})),
        ("mermaid_render", json!({"source": "graph TD; A-->B"})),
        ("mermaid_validate", json!({"source": "graph TD; A-->B"})),
        ("query_blended", json!({"query": "note"})),
        (
            "query_graph",
            json!({"operation": "neighbours", "from": "tasks/alpha.md"}),
        ),
        ("query_index_rebuild", json!({})),
        ("query_index_status", json!({})),
        ("query_semantic", json!({"query": "note"})),
        ("query_text", json!({"query": "note"})),
        ("vault_info", json!({})),
    ];
    let mut called = calls.iter().map(|(tool, _)| *tool).collect::<Vec<_>>();
    called.sort_unstable();
    let mut declared = [read_only().as_slice(), REBUILDS_DERIVED_STATE].concat();
    declared.sort_unstable();
    assert_eq!(
        called, declared,
        "every read-only tool and every derived-state rebuild is run here, and every run tool is declared as one"
    );

    for (tool, arguments) in calls {
        // A call may be refused (a missing file, a disabled provider); it must still change nothing.
        harness.call(tool, arguments).await?;
        assert_eq!(
            harness.dirty_paths().await?,
            Vec::<String>::new(),
            "{tool} changed the working tree"
        );
        assert!(
            harness.pending_paths().await?.is_empty(),
            "{tool} staged something for commit"
        );
    }
    assert_eq!(
        harness.commit_count().await?,
        commits,
        "a read-only tool must not create a commit"
    );
    Ok(())
}
