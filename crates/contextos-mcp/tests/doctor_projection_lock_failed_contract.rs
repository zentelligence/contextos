//! A projection lock that cannot be opened (here, `projection.lock` is a folder) never hides a vault's findings or
//! stops a vault repair: `doctor` and `doctor_resolve` still do all their vault work, and the projection part alone is
//! reported as one failed check named `projection_lock_failed`, whose message carries the stable code
//! `projection/lock-failed`.

mod support;
use serde_json::Value;
use support::projection_doctor::{
    PROJECTION_KINDS, REVIEWER, assert_hides_home, checks_of_kind, doctor, doctor_resolve, projection_outcomes,
};
use support::projection_world::{BoxError, ProjectionWorld, agent};

fn break_the_projection_lock(world: &ProjectionWorld) -> Result<(), BoxError> {
    let lock = world.home().join(".contextos/projection.lock");
    if lock.is_file() {
        std::fs::remove_file(&lock)?;
    }
    std::fs::create_dir_all(&lock)?;
    Ok(())
}

/// The checks that are about the vaults: everything except the projection checks and the two unassessed reports.
fn vault_checks(report: &Value) -> Vec<Value> {
    report["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|check| {
            let kind = check["kind"].as_str().unwrap_or_default();
            kind != "projection_busy" && kind != "projection_lock_failed" && !PROJECTION_KINDS.contains(&kind)
        })
        .cloned()
        .collect()
}

#[tokio::test]
async fn doctor_still_returns_every_vault_check_and_one_failed_projection_lock_failed_check() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    let server = world.projecting_server()?;
    // The first assessment brings the link graph up to date with the vault, which the link graph store check then
    // reports on, so it is made once before the two that are compared.
    doctor(&server).await?;
    let free = doctor(&server).await?;
    assert!(
        !vault_checks(&free).is_empty(),
        "the fixture must have vault checks: {free}"
    );
    break_the_projection_lock(&world)?;

    let broken = doctor(&server).await?;

    let reported = checks_of_kind(&broken, "projection_lock_failed");
    assert_eq!(reported.len(), 1, "{broken}");
    assert_eq!(reported[0]["status"], "fail", "{}", reported[0]);
    assert_eq!(reported[0]["auto_fixable"], false, "{}", reported[0]);
    assert!(
        reported[0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("projection/lock-failed"),
        "{}",
        reported[0]
    );
    assert_eq!(broken["has_failures"], true, "{broken}");
    assert_eq!(
        vault_checks(&broken),
        vault_checks(&free),
        "a host-level problem never hides a vault's findings"
    );
    assert_hides_home(&broken, &world);
    Ok(())
}

#[tokio::test]
async fn doctor_resolve_still_repairs_a_vault_finding_and_resolves_no_projection_finding() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    let server = world.projecting_server()?;
    world.write_vault(0, REVIEWER, &agent("Review it differently."))?;
    break_the_projection_lock(&world)?;
    assert!(
        !world.vault(0).join("index.md").exists(),
        "the fixture needs a stale index"
    );

    let result = doctor_resolve(&server, false).await?;

    let indexes: Vec<&Value> = result["outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|outcome| outcome["kind"] == "managed_indexes")
        .collect();
    assert_eq!(indexes.len(), 1, "{result}");
    assert_eq!(indexes[0]["resolved"], true, "{}", indexes[0]);
    assert!(world.vault(0).join("index.md").exists(), "the index was not rebuilt");
    assert_eq!(projection_outcomes(&result).len(), 0, "{result}");
    assert_eq!(
        checks_of_kind(&result["report"], "projection_lock_failed").len(),
        1,
        "{result}"
    );
    Ok(())
}
