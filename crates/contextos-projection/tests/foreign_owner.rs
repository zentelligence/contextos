//! An entry another configuration projected is reported as foreign, naming that configuration, and never changed.

mod common;

use std::fs;

use common::{REVIEWER, TestResult, World, reviewer, world};
use contextos_core::OpKind;
use contextos_projection::{Finding, FindingKind, HostKind, Slug};

/// Sixty-four hexadecimal characters, as a real configuration identity is.
const OWNER: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeeff00";
const OTHER: &str = "ffeeddccbbaa99887766554433221100f0e1d2c3b4a5968778695a4b3c2d1e0f";

/// Projects the reviewer with the `OWNER` configuration, leaving the files and records of that run in place.
fn projected_by_owner() -> Result<World, Box<dyn std::error::Error>> {
    let world = world(1)?;
    let owner = world.service(OWNER)?;
    world.write_vault(0, REVIEWER, &reviewer("Review."))?;
    world.project(&owner, OpKind::Create, &[(0, REVIEWER)])?;
    Ok(world)
}

fn foreign_claude(findings: &[Finding]) -> Vec<&Finding> {
    findings
        .iter()
        .filter(|finding| {
            finding.kind == FindingKind::ProjectionForeignOwner
                && finding.host == Some(HostKind::ClaudeCode)
                && finding.name == "reviewer"
        })
        .collect()
}

#[test]
fn a_foreign_entry_whose_slug_this_configuration_defines_is_reported_with_the_owners_short_identity() -> TestResult {
    let world = projected_by_owner()?;
    let other = world.service(OTHER)?;

    let found = other.assess();

    let foreign = foreign_claude(&found.findings);
    assert_eq!(foreign.len(), 1, "{:?}", found.findings);
    assert!(foreign[0].message.contains(&OWNER[..12]), "{}", foreign[0].message);
    assert!(!foreign[0].message.contains(&OWNER[..13]), "{}", foreign[0].message);
    assert!(!foreign[0].message.contains(&OTHER[..12]), "{}", foreign[0].message);
    Ok(())
}

#[test]
fn a_foreign_entry_whose_slug_this_configuration_does_not_define_is_still_reported() -> TestResult {
    let world = projected_by_owner()?;
    let other = world.service_over(&World::no_roots()?, OTHER)?;

    let found = other.assess();

    let foreign = foreign_claude(&found.findings);
    assert_eq!(foreign.len(), 1, "{:?}", found.findings);
    assert!(foreign[0].message.contains(&OWNER[..12]), "{}", foreign[0].message);
    assert!(!foreign[0].message.contains(&OWNER[..13]), "{}", foreign[0].message);
    assert!(
        found
            .findings
            .iter()
            .all(|finding| finding.kind != FindingKind::ProjectionOrphaned || finding.name != "reviewer"),
        "{:?}",
        found.findings
    );
    Ok(())
}

#[test]
fn a_foreign_entry_is_never_changed_by_a_refresh_or_a_resolve_whether_or_not_the_slug_is_defined() -> TestResult {
    let world = projected_by_owner()?;
    let before = fs::read(world.claude_agent("reviewer"))?;
    let defined = world.service(OTHER)?;
    let undefined = world.service_over(&World::no_roots()?, OTHER)?;

    for service in [&defined, &undefined] {
        let _ = service.refresh();
        let _ = service.resolve();
        assert_eq!(fs::read(world.claude_agent("reviewer"))?, before);
        assert!(world.codex_agent("reviewer").exists());
        let manifest = world.claude_agent_manifest()?;
        let entry = manifest
            .get(&Slug::try_from("reviewer")?)
            .ok_or("the record was removed")?;
        assert_eq!(<&str>::from(entry.configuration()), OWNER);
    }
    Ok(())
}

#[test]
fn a_foreign_entry_the_owner_has_since_deleted_from_its_vault_is_still_never_withdrawn_by_another() -> TestResult {
    let world = projected_by_owner()?;
    world.remove_vault(0, REVIEWER)?;
    let other = world.service(OTHER)?;

    let _ = other.resolve();
    let warnings = world.project(&other, OpKind::Delete, &[(0, REVIEWER)])?;

    assert!(world.claude_agent("reviewer").exists(), "{warnings:?}");
    assert_eq!(world.recorded_vault("reviewer")?.as_deref(), Some("alpha"));
    Ok(())
}

#[test]
fn an_entry_recorded_by_this_configuration_is_not_reported_foreign() -> TestResult {
    let world = projected_by_owner()?;
    let owner = world.service(OWNER)?;

    let found = owner.assess();

    assert!(foreign_claude(&found.findings).is_empty(), "{:?}", found.findings);
    Ok(())
}
