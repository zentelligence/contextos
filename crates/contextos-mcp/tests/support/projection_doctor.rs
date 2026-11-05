//! Helpers for the `doctor` and `doctor_resolve` projection tests: calling the tools, reading their findings and
//! outcomes, snapshotting a folder, and one world holding every kind of drift at once.
//!
//! Shared through `tests/support/mod.rs`; a test binary that uses only part of it would otherwise warn, which is
//! expected rather than genuine dead code.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use contextos_mcp::ContextOsServer;
use serde_json::{Value, json};

use super::git_harness::call;
use super::projection_world::{BoxError, ProjectionWorld, agent};

/// Every stable `kind` agent host projection adds to `doctor`.
pub const PROJECTION_KINDS: [&str; 10] = [
    "projection_stale",
    "projection_hand_edited",
    "projection_orphaned",
    "projection_invalid_definition",
    "projection_shadowed",
    "projection_unowned_collision",
    "projection_foreign_owner",
    "orientation_block",
    "codex_read_only_unenforced",
    "codex_override_hides_agents",
];

pub const REVIEWER: &str = "registry/agents/reviewer.md";
pub const GLOBAL_INSTRUCTIONS: &str = "registry/instructions/global.md";
pub const OPERATOR_FILE: &str = "The operator's own agent.\n";
pub const OVERRIDE_FILE: &str = "An override the operator keeps.\n";

/// An agent that Codex can only partly enforce.
pub fn read_only_agent(body: &str) -> String {
    agent(body).replace("access: all", "access: read-only")
}

pub async fn structured(server: &ContextOsServer, tool: &'static str, arguments: Value) -> Result<Value, BoxError> {
    let result = call(server, tool, arguments).await?;
    assert_eq!(result.is_error, Some(false), "{tool}: {result:?}");
    result
        .structured_content
        .ok_or_else(|| format!("{tool} omitted its structured result").into())
}

pub async fn doctor(server: &ContextOsServer) -> Result<Value, BoxError> {
    structured(server, "doctor", json!({})).await
}

pub async fn doctor_resolve(server: &ContextOsServer, dry_run: bool) -> Result<Value, BoxError> {
    structured(server, "doctor_resolve", json!({"dry_run": dry_run})).await
}

pub async fn vault_name(server: &ContextOsServer, index: usize) -> Result<String, BoxError> {
    let info = structured(server, "vault_info", json!({})).await?;
    info["vaults"][index]["name"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "vault_info omitted a vault name".into())
}

fn kind_of(entry: &Value) -> &str {
    entry["kind"].as_str().unwrap_or_default()
}

/// The checks of `report` (a `doctor` result, or the `report` of a `doctor_resolve` result) with the stable `kind`.
pub fn checks_of_kind<'a>(report: &'a Value, kind: &str) -> Vec<&'a Value> {
    report["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|check| kind_of(check) == kind)
        .collect()
}

/// Every projection kind `report` lists, once each.
pub fn projection_kinds(report: &Value) -> BTreeSet<String> {
    report["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|check| PROJECTION_KINDS.contains(&kind_of(check)))
        .map(|check| kind_of(check).to_owned())
        .collect()
}

pub fn kind_set(kinds: &[&str]) -> BTreeSet<String> {
    kinds.iter().map(|kind| (*kind).to_owned()).collect()
}

/// The outcomes of a `doctor_resolve` result for projection kinds, in report order.
pub fn projection_outcomes(result: &Value) -> Vec<&Value> {
    result["outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|outcome| PROJECTION_KINDS.contains(&kind_of(outcome)))
        .collect()
}

pub fn outcomes_of_kind<'a>(result: &'a Value, kind: &str) -> Vec<&'a Value> {
    projection_outcomes(result)
        .into_iter()
        .filter(|outcome| kind_of(outcome) == kind)
        .collect()
}

/// The subject and message of a check, lower-cased, which is where a finding names its host and slug or file.
pub fn words(entry: &Value) -> String {
    format!(
        "{} {}",
        entry["subject"].as_str().unwrap_or_default(),
        entry["message"].as_str().unwrap_or_default()
    )
    .to_lowercase()
}

/// The stable remediation code of a check, or `None` when it names none.
///
/// A projection remediation is performed by the server itself, so it is named in `remediation` and never in
/// `remediation_tool`, which names only a tool a client could call: a client reading `remediation_tool` must never be
/// handed something that is not a tool. A check carrying both would contradict that, so it reads as no remediation.
pub fn remediation(check: &Value) -> Option<&str> {
    if check.get("remediation_tool").is_some_and(|tool| !tool.is_null()) {
        return None;
    }
    check["remediation"].as_str()
}

/// Asserts a report never shows the home folder's path.
pub fn assert_hides_home(report: &Value, world: &ProjectionWorld) {
    let home = world.home().to_string_lossy().into_owned();
    assert!(
        !report.to_string().contains(&home),
        "a finding or outcome showed the home path: {report}"
    );
}

/// Every file under `root` with its bytes, and every folder with none, to compare before and after.
pub fn snapshot(root: &Path) -> Result<BTreeMap<PathBuf, Option<Vec<u8>>>, BoxError> {
    fn walk(path: &Path, into: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) -> Result<(), BoxError> {
        for entry in std::fs::read_dir(path)? {
            let path = entry?.path();
            if path.is_dir() {
                into.insert(path.clone(), None);
                walk(&path, into)?;
            } else {
                into.insert(path.clone(), Some(std::fs::read(&path)?));
            }
        }
        Ok(())
    }
    let mut into = BTreeMap::new();
    walk(root, &mut into)?;
    Ok(into)
}

/// One vault and both hosts, after a server projected, with every kind of drift made outside the server.
pub struct DriftedWorld {
    pub world: ProjectionWorld,
    pub server: ContextOsServer,
    /// What the critic agent's Claude file held when projected, which a reprojection restores.
    pub critic_projected: Vec<u8>,
}

/// Definitions `reviewer`, `critic`, `retired`, `mine`, and global instructions are projected. Then, outside the
/// server: `reviewer` changes (stale), the Claude `critic` file is edited by hand, `retired` is deleted (orphaned),
/// the global instructions change (orientation block differs), an `AGENTS.override.md` appears beside Codex's file,
/// and an invalid `broken` agent is added. The operator's own `mine.md` sat at the Claude slug `mine` before the
/// server first projected, so it is an unowned collision there.
pub fn drifted_world() -> Result<DriftedWorld, BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    world.write_vault(0, "registry/agents/critic.md", &agent("Criticise it."))?;
    world.write_vault(0, "registry/agents/retired.md", &agent("Retire it."))?;
    world.write_vault(0, "registry/agents/mine.md", &agent("Mine."))?;
    world.write_vault(0, GLOBAL_INSTRUCTIONS, "First orientation.\n")?;
    std::fs::create_dir_all(world.home().join(".claude/agents"))?;
    std::fs::write(world.claude_agent("mine"), OPERATOR_FILE)?;
    let server = world.projecting_server()?;
    assert!(world.claude_agent("critic").is_file(), "the fixture was not projected");
    assert!(world.codex_agent("retired").is_file(), "the fixture was not projected");
    let critic_projected = std::fs::read(world.claude_agent("critic"))?;

    world.write_vault(0, REVIEWER, &agent("Review it differently."))?;
    std::fs::write(world.claude_agent("critic"), "edited by hand\n")?;
    std::fs::remove_file(world.vault(0).join("registry/agents/retired.md"))?;
    world.write_vault(0, GLOBAL_INSTRUCTIONS, "Second orientation.\n")?;
    std::fs::write(world.home().join(".codex/AGENTS.override.md"), OVERRIDE_FILE)?;
    world.write_vault(
        0,
        "registry/agents/broken.md",
        "---\ndescription: Broken\ntier: sonnet\naccess: all\n---\n\nBody.\n",
    )?;
    Ok(DriftedWorld {
        world,
        server,
        critic_projected,
    })
}
