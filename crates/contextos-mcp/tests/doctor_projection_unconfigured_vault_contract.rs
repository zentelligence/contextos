//! A projection finding or outcome names a vault only when that vault is configured. A recorded entry whose source
//! vault is no longer configured is an orphan that concerns no configured vault, so `vault_name` and `vault_path` are
//! omitted for it rather than naming a vault the operator cannot address; one whose source vault is still configured
//! names it. Checks are covered beside the code that words them, in `doctor_projection_test.rs`.

mod support;
use contextos_mcp::{ConfigLocation, preview_resolution_for_cli};
use support::projection_doctor::{REVIEWER, checks_of_kind, doctor, doctor_resolve, outcomes_of_kind, vault_name};
use support::projection_world::{BoxError, ProjectionWorld, agent};

/// Two vaults, with `reviewer` defined in the first and projected from there, then a configuration that no longer
/// lists the first. A server over it would withdraw the orphans as it starts, so the entries are read through the
/// configuration alone, as `contextos doctor --resolve --dry-run` does.
async fn preview_with_first_vault_unconfigured() -> Result<(Vec<serde_json::Value>, String), BoxError> {
    let world = ProjectionWorld::new(2)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    let first = world.projecting_server()?;
    let first_name = vault_name(&first, 0).await?;
    drop(first);
    let mut config = world.config(ConfigLocation::PlatformDefault, None)?;
    config.vaults.remove(0);

    let outcomes = preview_resolution_for_cli(&config)?;

    Ok((
        outcomes.iter().map(serde_json::to_value).collect::<Result<_, _>>()?,
        first_name,
    ))
}

#[tokio::test]
async fn previewing_an_orphan_of_a_vault_that_is_no_longer_configured_names_no_vault() -> Result<(), BoxError> {
    let (outcomes, first_name) = preview_with_first_vault_unconfigured().await?;

    let orphaned = outcomes
        .iter()
        .filter(|outcome| outcome["kind"] == "projection_orphaned")
        .collect::<Vec<_>>();
    assert_eq!(orphaned.len(), 2, "{outcomes:?}");
    for outcome in orphaned {
        assert!(outcome.get("vault_name").is_none(), "{outcome}");
        assert!(outcome.get("vault_path").is_none(), "{outcome}");
        assert_eq!(outcome["resolved"], false, "{outcome}");
    }
    assert!(
        !serde_json::to_string(&outcomes)?.contains(&first_name),
        "the unconfigured vault is named"
    );
    Ok(())
}

#[tokio::test]
async fn an_orphan_whose_source_vault_is_still_configured_names_it() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(2)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    let server = world.projecting_server()?;
    std::fs::remove_file(world.vault(0).join(REVIEWER))?;
    let name = vault_name(&server, 0).await?;

    let report = doctor(&server).await?;
    let planned = doctor_resolve(&server, true).await?;

    let orphaned = checks_of_kind(&report, "projection_orphaned");
    assert_eq!(orphaned.len(), 2, "{report}");
    for check in orphaned {
        assert_eq!(check["vault_name"], name.as_str(), "{check}");
    }
    for outcome in outcomes_of_kind(&planned, "projection_orphaned") {
        assert_eq!(outcome["vault_name"], name.as_str(), "{outcome}");
        assert!(outcome["vault_path"].is_string(), "{outcome}");
    }
    Ok(())
}
