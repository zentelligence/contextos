//! A projection lock held by another process beyond the bound never hides a vault's findings or stops a vault repair:
//! `doctor` and `doctor_resolve` still do all their vault work, and the projection part alone is reported as one failed
//! check named `projection_busy`, whose message carries the stable code `projection/busy`.

mod support;
use std::fs::{self, File, OpenOptions};
use std::time::Duration;

use serde_json::Value;
use support::projection_doctor::{
    PROJECTION_KINDS, REVIEWER, assert_hides_home, checks_of_kind, doctor, doctor_resolve, projection_outcomes,
};
use support::projection_world::{BoxError, ProjectionWorld, agent};

/// Short enough to keep the test quick, long enough that a free lock is always taken.
const BOUND: Duration = Duration::from_millis(200);

/// Holds the projection lock the way another `contextos` process on the host would, until dropped.
fn hold_the_projection_lock(world: &ProjectionWorld) -> Result<File, BoxError> {
    let state_root = world.home().join(".contextos");
    fs::create_dir_all(&state_root)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state_root.join("projection.lock"))?;
    assert!(contextos_fs::try_lock_exclusive(&file)?, "the lock was already held");
    Ok(file)
}

/// The checks that are about the vaults: everything except the projection checks and the busy report.
fn vault_checks(report: &Value) -> Vec<Value> {
    report["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|check| {
            let kind = check["kind"].as_str().unwrap_or_default();
            kind != "projection_busy" && !PROJECTION_KINDS.contains(&kind)
        })
        .cloned()
        .collect()
}

#[tokio::test]
async fn doctor_still_returns_every_vault_check_and_one_failed_projection_busy_check() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    let server = world.projecting_server()?.with_projection_lock_wait(BOUND);
    // The first assessment brings the link graph up to date with the vault, which the link graph store check then
    // reports on, so it is made once before the two that are compared.
    doctor(&server).await?;
    let free = doctor(&server).await?;
    assert_eq!(checks_of_kind(&free, "projection_busy").len(), 0, "{free}");
    assert!(
        !vault_checks(&free).is_empty(),
        "the fixture must have vault checks: {free}"
    );
    let _held = hold_the_projection_lock(&world)?;

    let busy = doctor(&server).await?;

    let reported = checks_of_kind(&busy, "projection_busy");
    assert_eq!(reported.len(), 1, "{busy}");
    assert_eq!(reported[0]["status"], "fail", "{}", reported[0]);
    assert_eq!(reported[0]["auto_fixable"], false, "{}", reported[0]);
    assert!(
        reported[0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("projection/busy"),
        "{}",
        reported[0]
    );
    assert_eq!(busy["has_failures"], true, "{busy}");
    assert_eq!(
        vault_checks(&busy),
        vault_checks(&free),
        "a host-level problem never hides a vault's findings"
    );
    assert_hides_home(&busy, &world);
    Ok(())
}

#[tokio::test]
async fn doctor_resolve_still_repairs_a_vault_finding_and_reports_projection_as_busy() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    let server = world.projecting_server()?.with_projection_lock_wait(BOUND);
    assert!(
        !world.vault(0).join("index.md").exists(),
        "the fixture needs a stale index"
    );
    let _held = hold_the_projection_lock(&world)?;

    let result = doctor_resolve(&server, false).await?;

    let indexes = outcomes_of_any_kind(&result, "managed_indexes");
    assert_eq!(indexes.len(), 1, "{result}");
    assert_eq!(indexes[0]["resolved"], true, "{}", indexes[0]);
    assert!(world.vault(0).join("index.md").exists(), "the index was not rebuilt");
    assert_eq!(projection_outcomes(&result).len(), 0, "{result}");
    let reported = checks_of_kind(&result["report"], "projection_busy");
    assert_eq!(reported.len(), 1, "{result}");
    assert_eq!(reported[0]["status"], "fail", "{}", reported[0]);
    assert!(
        reported[0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("projection/busy"),
        "{}",
        reported[0]
    );
    Ok(())
}

/// The outcomes of one kind, whether or not it is a projection kind.
fn outcomes_of_any_kind<'a>(result: &'a Value, kind: &str) -> Vec<&'a Value> {
    result["outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|outcome| outcome["kind"] == kind)
        .collect()
}
