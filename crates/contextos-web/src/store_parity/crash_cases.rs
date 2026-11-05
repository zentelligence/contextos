//! Crash consistency over the real adapters: every changing step of an install, a repair, a removal, and a retention
//! batch is failed in turn, as a crash at that step would leave the store. After each, no partial release is visible
//! under a release's name, recovery clears what was left, the ledger agrees with the folders, and running the operation
//! again finishes it.

use contextos_packages::{LedgerState, PackageStore, PackagesError, RequestedRemoval};

use super::faulty_io::{FaultPlan, FaultyIo};
use super::store_support::{condition_of, entries, install_versions, tamper};
use super::worlds::{
    BoxError, Engine, StoreWorld, engine_over, key, ledger, listing, read, recover, request_of, top_level,
};
use crate::package_retention::{RetentionError, retain};
use crate::retention_fixture::{FixedHome, archive};

/// What the operation under test does, and the store it starts from.
#[derive(Clone, Copy, Debug)]
pub(super) enum Scenario {
    /// Installs the first release into a store that does not exist yet.
    InstallIntoEmptyStore,
    /// Installs a new release beside one already installed.
    InstallBesideAnother,
    /// Installs again a recorded release whose folder is gone.
    RepairRemovedRelease,
    /// Installs again a recorded release missing a file.
    RepairIncompleteRelease,
    /// Installs again a recorded release with a file changed.
    RepairDamagedRelease,
    /// Removes a release at an operator's request.
    RemoveByRequest,
    /// Sets two releases aside for retention and settles them, keeping one.
    SetAsideAndSettle,
    /// Runs a whole retention pass that removes the two older of three releases.
    RetentionPass,
}

impl Scenario {
    /// The release the operation is about.
    fn target(self) -> &'static str {
        match self {
            Self::InstallBesideAnother => "1.1.0",
            Self::InstallIntoEmptyStore
            | Self::RepairRemovedRelease
            | Self::RepairIncompleteRelease
            | Self::RepairDamagedRelease
            | Self::RemoveByRequest
            | Self::SetAsideAndSettle
            | Self::RetentionPass => "1.0.0",
        }
    }

    /// How the target reads before the operation starts, which is how it may still read after a crash that did not
    /// reach the step that mends it.
    fn target_before(self) -> &'static str {
        match self {
            Self::InstallIntoEmptyStore | Self::InstallBesideAnother | Self::RepairRemovedRelease => "removed",
            Self::RepairIncompleteRelease => "incomplete",
            Self::RepairDamagedRelease => "damaged",
            Self::RemoveByRequest | Self::SetAsideAndSettle | Self::RetentionPass => "verified",
        }
    }

    /// Whether the operation removes the target, which then ends recorded as removed.
    const fn removes_target(self) -> bool {
        matches!(
            self,
            Self::RemoveByRequest | Self::SetAsideAndSettle | Self::RetentionPass
        )
    }

    /// Lays out the store the operation starts from, with a clean engine.
    async fn prepare<W: StoreWorld>(self, world: &W) -> Result<(), BoxError> {
        match self {
            Self::InstallIntoEmptyStore => {}
            Self::InstallBesideAnother => install_versions(world, &["1.0.0"]).await?,
            Self::RepairRemovedRelease => {
                install_versions(world, &["1.0.0"]).await?;
                std::fs::remove_dir_all(world.store_dir().join("crm/1.0.0"))?;
            }
            Self::RepairIncompleteRelease => {
                install_versions(world, &["1.0.0"]).await?;
                std::fs::remove_file(world.store_dir().join("crm/1.0.0/sync/sync.rhai"))?;
            }
            Self::RepairDamagedRelease => {
                install_versions(world, &["1.0.0"]).await?;
                tamper(world, "1.0.0")?;
            }
            Self::RemoveByRequest | Self::SetAsideAndSettle | Self::RetentionPass => {
                install_versions(world, &["1.0.0", "1.1.0", "1.2.0"]).await?;
            }
        }
        Ok(())
    }

    /// Runs the operation once, to its end or to the failure.
    async fn operate<W: StoreWorld>(self, engine: &Engine<FaultyIo<W::Io>>) -> Result<(), PackagesError> {
        let lock = engine.lock().await?;
        match self {
            Self::InstallIntoEmptyStore
            | Self::InstallBesideAnother
            | Self::RepairRemovedRelease
            | Self::RepairIncompleteRelease
            | Self::RepairDamagedRelease => {
                let request = request_of(&archive(self.target()).map_err(unexpected)?).map_err(unexpected)?;
                engine.install(&lock, request).await.map(|_| ())
            }
            Self::RemoveByRequest => {
                let target = key(self.target()).map_err(unexpected)?;
                engine
                    .remove(&lock, &target, RequestedRemoval::ByRequest)
                    .await
                    .map(|_| ())
            }
            Self::SetAsideAndSettle => {
                let (first, second) = (key("1.0.0").map_err(unexpected)?, key("1.1.0").map_err(unexpected)?);
                let batch = engine.set_aside(&lock, &[first, second.clone()]).await?;
                engine.settle(&lock, batch, &[second]).await.map(|_| ())
            }
            Self::RetentionPass => {
                let home = retention_home().map_err(unexpected)?;
                retain(engine, &lock, &home)
                    .await
                    .map(|_| ())
                    .map_err(|error| match error {
                        RetentionError::Store(error) => error,
                        other => unexpected(other),
                    })
            }
        }
    }

    /// Runs the operation again with a clean engine, as the next operator command would, and says whether it found
    /// nothing left to do.
    async fn finish<W: StoreWorld>(self, world: &W) -> Result<(), BoxError> {
        let engine = engine_over(world.io().await?);
        let lock = engine.lock().await?;
        match self {
            Self::InstallIntoEmptyStore
            | Self::InstallBesideAnother
            | Self::RepairRemovedRelease
            | Self::RepairIncompleteRelease
            | Self::RepairDamagedRelease => {
                engine.install(&lock, request_of(&archive(self.target())?)?).await?;
            }
            Self::RemoveByRequest => {
                match engine
                    .remove(&lock, &key(self.target())?, RequestedRemoval::ByRequest)
                    .await
                {
                    Ok(_) | Err(PackagesError::NotFound { .. }) => {}
                    Err(other) => return Err(other.into()),
                }
            }
            Self::RetentionPass => {
                retain(&engine, &lock, &retention_home()?).await?;
            }
            Self::SetAsideAndSettle => {
                let target = [key("1.0.0")?, key("1.1.0")?];
                let present = {
                    let listed = engine.list().await?;
                    target
                        .iter()
                        .filter(|k| listed.releases.contains(k))
                        .cloned()
                        .collect::<Vec<_>>()
                };
                if !present.is_empty() {
                    let batch = engine.set_aside(&lock, &present).await?;
                    engine.settle(&lock, batch, &[key("1.1.0")?]).await?;
                }
            }
        }
        Ok(())
    }

    /// The versions the operation ends with a removal recorded for.
    fn removed_at_the_end(self) -> &'static [&'static str] {
        match self {
            Self::RemoveByRequest | Self::SetAsideAndSettle => &["1.0.0"],
            Self::RetentionPass => &["1.0.0", "1.1.0"],
            _ => &[],
        }
    }
}

/// A home whose state names `1.2.0` as the only release in use, so retention removes the two older ones.
fn retention_home() -> Result<FixedHome, BoxError> {
    FixedHome::new("[crm.sync]\ncurrent = \"1.2.0\"\n", Vec::new())
}

fn unexpected(error: impl std::fmt::Display) -> PackagesError {
    PackagesError::LedgerUnreadable {
        reason: format!("test setup failed: {error}"),
    }
}

/// What every moment after a crash must satisfy, before anything recovers: whatever stands under a release's name is a
/// whole release, or the target in the state the operation found it in; never a release half written.
async fn assert_no_partial_release<W: StoreWorld>(world: &W, scenario: Scenario, step: usize) -> Result<(), BoxError> {
    let listed = listing(world).await?;
    for release in &listed.releases {
        let version = release.version.to_string();
        let found = condition_of(&read(world, &version).await?);
        let acceptable = found == "verified" || (version == scenario.target() && found == scenario.target_before());
        assert!(
            acceptable,
            "{scenario:?}, crash at step {step}: crm {version} stands under its name reading {found}"
        );
    }
    Ok(())
}

/// What must hold once recovery has run: nothing left over, and the ledger and the folders agree.
async fn assert_agreement<W: StoreWorld>(world: &W, scenario: Scenario, step: usize) -> Result<(), BoxError> {
    let context = format!("{scenario:?}, crash at step {step}");
    let left: Vec<String> = top_level(&world.store_dir())
        .into_iter()
        .filter(|name| name.starts_with("_staging-") || name.starts_with("_removing-"))
        .collect();
    assert_eq!(left.len(), 0, "{context}: recovery left {left:?}");

    let reading = ledger(world).await?;
    let ledger_entries = match reading.state {
        LedgerState::Present(ledger) => ledger.entries,
        LedgerState::Absent => Vec::new(),
        LedgerState::Unusable(problem) => return Err(format!("{context}: the ledger is unusable: {problem:?}").into()),
    };
    let listed = listing(world).await?;
    for entry in &ledger_entries {
        let version = entry.release.key.version.to_string();
        let found = condition_of(&read(world, &version).await?);
        if entry.removal.is_some() {
            assert_eq!(
                found, "removed",
                "{context}: crm {version} is recorded removed but its folder stands"
            );
        } else {
            let healthy = found == "verified";
            let as_found_before = version == scenario.target() && found == scenario.target_before();
            assert!(
                healthy || as_found_before,
                "{context}: crm {version} is recorded but reads {found}"
            );
        }
    }
    for release in &listed.releases {
        let recorded = ledger_entries
            .iter()
            .find(|entry| entry.release.key == *release)
            .ok_or_else(|| format!("{context}: crm {} stands with no ledger entry", release.version))?;
        assert!(
            recorded.removal.is_none(),
            "{context}: crm {} stands though the ledger records it removed",
            release.version
        );
    }
    Ok(())
}

/// After the operation is run again: it is complete.
async fn assert_complete<W: StoreWorld>(world: &W, scenario: Scenario, step: usize) -> Result<(), BoxError> {
    let context = format!("{scenario:?}, crash at step {step}, after running the operation again");
    let reading = ledger(world).await?;
    assert_eq!(
        reading.unrecorded_removals.len(),
        0,
        "{context}: {:?}",
        reading.unrecorded_removals
    );
    let recorded = entries(world).await?;
    for version in scenario.removed_at_the_end() {
        let entry = recorded
            .iter()
            .find(|entry| entry.release.key.version.to_string() == *version)
            .ok_or_else(|| format!("{context}: no ledger entry for crm {version}"))?;
        assert!(
            entry.removal.is_some(),
            "{context}: crm {version} is not recorded removed"
        );
        assert_eq!(condition_of(&read(world, version).await?), "removed", "{context}");
    }
    if !scenario.removes_target() {
        assert_eq!(
            condition_of(&read(world, scenario.target()).await?),
            "verified",
            "{context}"
        );
        let entry = recorded
            .iter()
            .find(|entry| entry.release.key.version.to_string() == scenario.target())
            .ok_or_else(|| format!("{context}: the target has no ledger entry"))?;
        assert!(entry.removal.is_none(), "{context}");
    }
    let left: Vec<String> = top_level(&world.store_dir())
        .into_iter()
        .filter(|name| name.starts_with("_staging-") || name.starts_with("_removing-"))
        .collect();
    assert_eq!(left.len(), 0, "{context}: {left:?}");
    Ok(())
}

/// Fails the operation of `scenario` at every changing step in turn, over the real adapter of `W`.
///
/// Returns the number of changing steps the operation makes when nothing fails.
pub(super) async fn crash_at_every_step<W: StoreWorld>(scenario: Scenario) -> Result<usize, BoxError> {
    // A run that fails nothing finds how many changing steps there are.
    let steps = {
        let world = W::new().await?;
        scenario.prepare(&world).await?;
        let plan = FaultPlan::never_failing();
        let engine = engine_over(FaultyIo {
            inner: world.io().await?,
            plan: plan.clone(),
        });
        scenario.operate::<W>(&engine).await?;
        plan.attempted()
    };
    assert!(
        steps >= 3,
        "{scenario:?} made only {steps} changing steps, so the loop would prove little"
    );

    for step in 0..steps {
        let world = W::new().await?;
        scenario.prepare(&world).await?;
        let plan = FaultPlan::failing_from(step);
        let engine = engine_over(FaultyIo {
            inner: world.io().await?,
            plan: plan.clone(),
        });

        let outcome = scenario.operate::<W>(&engine).await;

        assert!(plan.attempted() > step, "{scenario:?}: step {step} was never reached");
        if outcome.is_ok() {
            // Only cleanup failed: the operation's own steps all happened.
            assert_complete_or_cleanup_only(&world, scenario, step).await?;
        }
        assert_no_partial_release(&world, scenario, step).await?;
        recover(&world).await?;
        assert_agreement(&world, scenario, step).await?;
        scenario.finish(&world).await?;
        assert_complete(&world, scenario, step).await?;
    }
    Ok(steps)
}

/// An operation that reported success despite a failed step failed only a step that deletes, which the next
/// operation repeats: the target is already in its final state.
async fn assert_complete_or_cleanup_only<W: StoreWorld>(
    world: &W,
    scenario: Scenario,
    step: usize,
) -> Result<(), BoxError> {
    let found = condition_of(&read(world, scenario.target()).await?);
    let expected = if scenario.removes_target() {
        "removed"
    } else {
        "verified"
    };
    assert_eq!(
        found, expected,
        "{scenario:?}, step {step}: the operation reported success but the target reads {found}"
    );
    Ok(())
}
