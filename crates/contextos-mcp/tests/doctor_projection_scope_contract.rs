//! Agent host projection is host-level: one set of host folders serves every configured vault. So `doctor` scoped to
//! one vault with `path` lists only the projection findings that name that vault, and `doctor_resolve` scoped the same
//! way repairs none of them, since a projection repair changes the folders every vault shares. Neither `doctor` nor
//! `doctor_resolve` creates a host's agents or skills folder; only serving does.

mod support;
use serde_json::{Value, json};
use support::projection_doctor::{
    GLOBAL_INSTRUCTIONS, REVIEWER, doctor, doctor_resolve, projection_kinds, projection_outcomes, snapshot, structured,
    vault_name,
};
use support::projection_world::{BoxError, ProjectionWorld, agent};

/// Two vaults, each with an agent projected from it, then each agent changed outside the server, so both vaults have
/// a stale projection, and the first vault's global instructions changed, so the orientation block it sources differs.
fn drift_in_both_vaults() -> Result<(ProjectionWorld, contextos_mcp::ContextOsServer), BoxError> {
    let world = ProjectionWorld::new(2)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    world.write_vault(1, "registry/agents/critic.md", &agent("Criticise it."))?;
    world.write_vault(0, GLOBAL_INSTRUCTIONS, "First orientation.\n")?;
    let server = world.projecting_server()?;
    assert!(world.claude_agent("critic").is_file(), "the fixture was not projected");

    world.write_vault(0, REVIEWER, &agent("Review it differently."))?;
    world.write_vault(1, "registry/agents/critic.md", &agent("Criticise it differently."))?;
    world.write_vault(0, GLOBAL_INSTRUCTIONS, "Second orientation.\n")?;
    Ok((world, server))
}

fn projection_checks(report: &Value) -> Vec<&Value> {
    let kinds = projection_kinds(report);
    report["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|check| kinds.contains(check["kind"].as_str().unwrap_or_default()))
        .collect()
}

#[tokio::test]
async fn doctor_scoped_to_one_vault_lists_only_the_projection_findings_that_name_it() -> Result<(), BoxError> {
    let (_world, server) = drift_in_both_vaults()?;
    let first = vault_name(&server, 0).await?;
    let second = vault_name(&server, 1).await?;
    let unscoped = doctor(&server).await?;
    assert!(
        projection_checks(&unscoped)
            .iter()
            .any(|check| check["vault_name"] == first.as_str()),
        "the fixture has no finding naming the first vault: {unscoped}"
    );

    let scoped = structured(&server, "doctor", json!({"path": second})).await?;

    let checks = projection_checks(&scoped);
    assert!(
        !checks.is_empty(),
        "the second vault's stale projection is not listed: {scoped}"
    );
    for check in checks {
        assert_eq!(check["vault_name"], second.as_str(), "{check}");
    }
    Ok(())
}

#[tokio::test]
async fn doctor_resolve_scoped_to_one_vault_repairs_no_projection_finding() -> Result<(), BoxError> {
    let (world, server) = drift_in_both_vaults()?;
    let second = vault_name(&server, 1).await?;
    let before = snapshot(world.home())?;

    let result = structured(&server, "doctor_resolve", json!({"path": second, "dry_run": false})).await?;

    assert!(projection_outcomes(&result).is_empty(), "{result}");
    assert_eq!(
        snapshot(world.home())?,
        before,
        "a scoped resolution changed the host folders"
    );
    let planned = structured(&server, "doctor_resolve", json!({"path": second, "dry_run": true})).await?;
    assert!(projection_outcomes(&planned).is_empty(), "{planned}");
    Ok(())
}

#[tokio::test]
async fn neither_doctor_nor_doctor_resolve_creates_a_missing_agents_or_skills_folder() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    let server = world.projecting_server()?;
    assert!(
        world.claude_agent("reviewer").is_file(),
        "the fixture was not projected"
    );
    let claude_agents = world.home().join(".claude/agents");
    let claude_skills = world.home().join(".claude/skills");
    std::fs::remove_dir_all(&claude_agents)?;
    if claude_skills.exists() {
        std::fs::remove_dir_all(&claude_skills)?;
    }

    let report = doctor(&server).await?;
    let planned = doctor_resolve(&server, true).await?;
    let resolved = doctor_resolve(&server, false).await?;

    assert!(
        !claude_agents.exists(),
        "doctor or doctor_resolve created Claude Code's agents folder"
    );
    assert!(
        !claude_skills.exists(),
        "doctor or doctor_resolve created Claude Code's skills folder"
    );
    for check in projection_checks(&report) {
        assert!(
            !check["subject"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .contains("claude"),
            "an entry in Claude Code's missing agents folder was assessed: {check}"
        );
    }
    for outcome in projection_outcomes(&planned)
        .into_iter()
        .chain(projection_outcomes(&resolved))
    {
        assert!(
            !outcome["subject"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .contains("claude"),
            "an entry in Claude Code's missing agents folder was repaired: {outcome}"
        );
    }
    assert!(
        world.codex_agent("reviewer").is_file(),
        "the installed Codex projection was disturbed"
    );
    Ok(())
}

#[tokio::test]
async fn doctor_scoped_to_a_vault_lists_a_shadowed_finding_from_either_side_of_the_shadowing() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(2)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("From the first."))?;
    world.write_vault(1, REVIEWER, &agent("From the second."))?;
    let server = world.projecting_server()?;
    let first = vault_name(&server, 0).await?;
    let second = vault_name(&server, 1).await?;
    let unscoped = doctor(&server).await?;
    assert!(
        !support::projection_doctor::checks_of_kind(&unscoped, "projection_shadowed").is_empty(),
        "the fixture has no shadowed finding: {unscoped}"
    );

    let winner = structured(&server, "doctor", json!({"path": first})).await?;
    let shadowed = structured(&server, "doctor", json!({"path": second})).await?;

    for (label, report) in [("the winning vault", &winner), ("the shadowed vault", &shadowed)] {
        assert!(
            !support::projection_doctor::checks_of_kind(report, "projection_shadowed").is_empty(),
            "doctor scoped to {label} omits the shadowed finding that names it: {report}"
        );
    }
    Ok(())
}
