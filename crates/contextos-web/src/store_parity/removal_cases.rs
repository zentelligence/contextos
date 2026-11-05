//! Remove guard, forced removal, retention, aside-first removal, and recovery cases, written once against
//! [`StoreWorld`].

use contextos_packages::{LedgerState, PackageStore, PackagesError, RecordedCause, RemovalCause, RequestedRemoval};

use super::store_support::{
    code_of, condition_of, copy_tree, entries, entry_of, future_of, install_versions, tamper, three_releases,
};
use super::worlds::{BoxError, StoreWorld, engine, key, ledger, listing, read, recover, top_level, versions};
use crate::retention_fixture::{archive_needing, library_archive};

const CURRENT_1_2: &str = "[crm.sync]\ncurrent = \"1.2.0\"\n";

/// As `CURRENT_1_2`, with `1.1.0` kept as the previous release, so retention after a removal keeps it.
const CURRENT_AND_PREVIOUS: &str = "[crm.sync]\ncurrent = \"1.2.0\"\nprevious = \"1.1.0\"\n";

// The remove guard and a forced removal.

pub(super) async fn the_store_lists_its_releases_and_what_keeps_them<W: StoreWorld>() -> Result<(), BoxError> {
    let world = three_releases::<W>(CURRENT_1_2).await?;
    world.write_job("nightly", "version = \"1.0.0\"\n")?;

    let standing = world.standing().await?;

    assert_eq!(standing.store, W::STORE);
    let versions: Vec<String> = standing
        .releases
        .iter()
        .map(|held| held.release.version.to_string())
        .collect();
    assert_eq!(versions, ["1.0.0", "1.1.0", "1.2.0"]);
    let kept: Vec<_> = standing.releases.iter().map(|held| held.kept_by.len()).collect();
    assert_eq!(
        kept,
        [1, 0, 1],
        "a job file keeps 1.0.0 and the current release keeps 1.2.0"
    );
    assert!(standing.releases.iter().all(|held| matches!(
        held.provenance,
        Some(contextos_packages::EntryProvenance::Installed { .. })
    )));
    Ok(())
}

pub(super) async fn a_release_no_rule_keeps_is_removed_and_recorded_by_request<W: StoreWorld>() -> Result<(), BoxError>
{
    let world = three_releases::<W>(CURRENT_AND_PREVIOUS).await?;

    let report = world.remove("crm", "1.0.0", false).await?;

    assert_eq!(versions(&world.store_dir(), "crm"), ["1.1.0", "1.2.0"]);
    assert_eq!(report.store, W::STORE);
    assert_eq!(report.removal.cause, RecordedCause::ByRequest);
    assert_eq!(report.warnings.len(), 0, "{:?}", report.warnings);
    let recorded = entry_of(&world, "1.0.0").await?.removal.ok_or("no removal recorded")?;
    assert_eq!(recorded.cause, RecordedCause::ByRequest);
    assert_eq!(top_level(&world.store_dir()), ["crm", "ledger.toml"]);
    Ok(())
}

pub(super) async fn a_release_a_job_file_names_is_refused_as_in_use_and_nothing_changes<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>(CURRENT_AND_PREVIOUS).await?;
    world.write_job("nightly", "version = \"1.0.0\"\n")?;
    let ledger_before = std::fs::read(world.store_dir().join("ledger.toml"))?;

    let error = world.remove("crm", "1.0.0", false).await.err();

    assert_eq!(error.map(|error| error.code()), Some("packages/in-use"));
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    assert_eq!(std::fs::read(world.store_dir().join("ledger.toml"))?, ledger_before);
    Ok(())
}

pub(super) async fn the_current_release_is_refused_as_in_use_unless_forced<W: StoreWorld>() -> Result<(), BoxError> {
    let world = three_releases::<W>(CURRENT_AND_PREVIOUS).await?;

    let refused = world.remove("crm", "1.2.0", false).await.err();
    let forced = world.remove("crm", "1.2.0", true).await?;

    assert_eq!(refused.map(|error| error.code()), Some("packages/in-use"));
    assert_eq!(forced.removal.cause, RecordedCause::Forced);
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.1.0"]);
    Ok(())
}

pub(super) async fn a_forced_removal_of_a_release_a_job_file_names_is_recorded_as_forced<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>(CURRENT_AND_PREVIOUS).await?;
    world.write_job("nightly", "version = \"1.0.0\"\n")?;

    let report = world.remove("crm", "1.0.0", true).await?;

    assert_eq!(versions(&world.store_dir(), "crm"), ["1.1.0", "1.2.0"]);
    assert_eq!(report.removal.cause, RecordedCause::Forced);
    let recorded = entry_of(&world, "1.0.0").await?.removal.ok_or("no removal recorded")?;
    assert_eq!(recorded.cause, RecordedCause::Forced);
    let standing = world.standing().await?;
    let removed = standing
        .removed
        .iter()
        .find(|removed| removed.release.version.to_string() == "1.0.0");
    assert_eq!(removed.map(|removed| removed.cause), Some(RemovalCause::Forced));
    Ok(())
}

pub(super) async fn a_state_file_that_is_not_valid_is_refused_with_its_own_code_for_a_listing_and_a_removal<
    W: StoreWorld,
>() -> Result<(), BoxError> {
    let world = three_releases::<W>("this is = not [valid").await?;

    let listed = world.standing().await.err();
    let removed = world.remove("crm", "1.0.0", false).await.err();

    assert_eq!(listed.map(|error| error.code()), Some("packages/state-unreadable"));
    assert_eq!(removed.map(|error| error.code()), Some("packages/state-unreadable"));
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    Ok(())
}

pub(super) async fn a_store_another_operation_holds_refuses_a_removal_as_store_locked<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>(CURRENT_AND_PREVIOUS).await?;
    let held = world.hold_store_lock()?;

    let error = world.remove("crm", "1.0.0", false).await.err();

    drop(held);
    assert_eq!(error.map(|error| error.code()), Some("packages/store-locked"));
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    Ok(())
}

pub(super) async fn a_release_that_is_not_there_or_a_name_or_version_that_is_not_usable_is_refused_by_its_own_code<
    W: StoreWorld,
>() -> Result<(), BoxError> {
    let world = three_releases::<W>(CURRENT_AND_PREVIOUS).await?;

    let missing = world.remove("crm", "9.0.0", false).await.err();
    let bad_name = world.remove("../crm", "1.0.0", false).await.err();
    let bad_version = world.remove("crm", "1.2", false).await.err();

    assert_eq!(missing.map(|error| error.code()), Some("packages/not-found"));
    assert_eq!(bad_name.map(|error| error.code()), Some("packages/invalid-name"));
    assert_eq!(bad_version.map(|error| error.code()), Some("packages/invalid-version"));
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    Ok(())
}

pub(super) async fn removing_the_last_release_of_a_package_removes_its_folder<W: StoreWorld>() -> Result<(), BoxError> {
    let world = W::new().await?;
    install_versions(&world, &["1.0.0"]).await?;
    world.write_state("")?;

    world.remove("crm", "1.0.0", true).await?;

    assert_eq!(top_level(&world.store_dir()), ["ledger.toml"]);
    assert_eq!(
        entries(&world).await?.len(),
        1,
        "the ledger still records the removed release"
    );
    Ok(())
}

pub(super) async fn removing_a_future_release_rewrites_the_recorded_future_to_the_newest_release_left<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("[crm.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.2.0\"\n").await?;

    world.remove("crm", "1.2.0", false).await?;

    assert_eq!(future_of(&world)?.as_deref(), Some("1.1.0"));
    Ok(())
}

pub(super) async fn a_removal_runs_retention_and_reports_the_releases_only_the_removed_one_kept<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = W::new().await?;
    for archive in [
        library_archive("text", "1.0.0")?,
        library_archive("text", "1.1.0")?,
        archive_needing("1.2.0", "text", "^1")?,
    ] {
        super::worlds::install(&world, &archive).await?;
    }
    world.write_state(
        "[crm.sync]\ncurrent = \"1.2.0\"\n\n[crm.sync.\"1.2.0\".lock.text]\nversion = \"1.0.0\"\ndigest = \"sha256:aaaa\"\n",
    )?;

    let report = world.remove("crm", "1.2.0", true).await?;

    assert_eq!(versions(&world.store_dir(), "crm"), Vec::<String>::new());
    assert_eq!(versions(&world.store_dir(), "text"), ["1.1.0"]);
    assert_eq!(
        report
            .retention_removed
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["text 1.0.0"]
    );
    assert_eq!(report.warnings.len(), 0, "{:?}", report.warnings);
    Ok(())
}

// Retention.

pub(super) async fn retention_removes_what_nothing_keeps_and_records_the_cause_as_retention<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>(CURRENT_1_2).await?;

    world.retain().await?;

    assert_eq!(versions(&world.store_dir(), "crm"), ["1.2.0"]);
    for removed in ["1.0.0", "1.1.0"] {
        let recorded = entry_of(&world, removed).await?.removal.ok_or("no removal recorded")?;
        assert_eq!(recorded.cause, RecordedCause::Retention, "{removed}");
    }
    assert!(entry_of(&world, "1.2.0").await?.removal.is_none());
    let standing = world.standing().await?;
    assert_eq!(standing.removing.len(), 0);
    assert_eq!(standing.staging.len(), 0);
    assert!(
        standing
            .removed
            .iter()
            .all(|removed| removed.cause == RemovalCause::Retention),
        "{:?}",
        standing.removed
    );
    Ok(())
}

pub(super) async fn retention_keeps_a_release_a_job_file_names<W: StoreWorld>() -> Result<(), BoxError> {
    let world = three_releases::<W>(CURRENT_1_2).await?;
    world.write_job("nightly", "version = \"1.0.0\"\n")?;

    world.retain().await?;

    assert_eq!(versions(&world.store_dir(), "crm"), ["1.0.0", "1.2.0"]);
    Ok(())
}

pub(super) async fn retention_keeps_every_release_of_a_workflow_whose_job_file_cannot_be_read<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>(CURRENT_1_2).await?;
    world.write_job("broken", "this is = not [toml")?;

    world.retain().await?;

    assert_eq!(versions(&world.store_dir(), "crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    Ok(())
}

pub(super) async fn a_job_file_that_names_no_exact_release_keeps_nothing<W: StoreWorld>() -> Result<(), BoxError> {
    let world = three_releases::<W>(CURRENT_1_2).await?;
    world.write_job("ranged", "version = \"^1.0\"\n")?;

    world.retain().await?;

    assert_eq!(versions(&world.store_dir(), "crm"), ["1.2.0"]);
    Ok(())
}

pub(super) async fn a_store_with_releases_and_no_state_file_runs_no_retention_and_creates_no_state_file<
    W: StoreWorld,
>() -> Result<(), BoxError> {
    let world = W::new().await?;
    install_versions(&world, &["1.0.0", "1.1.0", "1.2.0"]).await?;

    world.retain().await?;

    assert_eq!(versions(&world.store_dir(), "crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    assert!(!world.state_path().exists(), "retention created the state file");
    Ok(())
}

pub(super) async fn retention_never_touches_a_release_that_does_not_read_whole<W: StoreWorld>() -> Result<(), BoxError>
{
    let world = three_releases::<W>(CURRENT_1_2).await?;
    tamper(&world, "1.0.0")?;
    std::fs::remove_file(world.store_dir().join("crm/1.1.0/sync/sync.rhai"))?;

    world.retain().await?;

    assert_eq!(versions(&world.store_dir(), "crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    assert_eq!(condition_of(&read(&world, "1.0.0").await?), "damaged");
    assert_eq!(condition_of(&read(&world, "1.1.0").await?), "incomplete");
    Ok(())
}

pub(super) async fn a_store_another_operation_holds_is_not_changed_by_retention_and_the_run_fails<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>(CURRENT_1_2).await?;
    let held = world.hold_store_lock()?;

    let failure = world.retain().await.err().map(|error| error.code());

    drop(held);
    assert_eq!(failure, Some("packages/retention-failed"));
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    Ok(())
}

pub(super) async fn retention_repairs_a_recorded_future_that_names_no_whole_release_even_when_it_removes_nothing<
    W: StoreWorld,
>() -> Result<(), BoxError> {
    let world = three_releases::<W>("[crm.sync]\ncurrent = \"1.0.0\"\nfuture = \"9.9.9\"\n").await?;
    world.write_job("one", "version = \"1.1.0\"\n")?;
    world.write_job("two", "version = \"1.2.0\"\n")?;

    world.retain().await?;

    assert_eq!(versions(&world.store_dir(), "crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    assert_eq!(future_of(&world)?.as_deref(), Some("1.2.0"));
    Ok(())
}

pub(super) async fn retention_repairs_a_recorded_future_that_names_a_damaged_release<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("[crm.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.2.0\"\n").await?;
    tamper(&world, "1.2.0")?;

    world.retain().await?;

    assert_eq!(future_of(&world)?.as_deref(), Some("1.1.0"));
    Ok(())
}

pub(super) async fn retention_rewrites_the_future_a_removal_changed<W: StoreWorld>() -> Result<(), BoxError> {
    let world = W::new().await?;
    install_versions(&world, &["1.0.0", "1.1.0"]).await?;
    world.write_state("[crm.sync]\nfuture = \"1.0.0\"\n")?;

    world.retain().await?;

    assert_eq!(versions(&world.store_dir(), "crm"), ["1.1.0"]);
    assert_eq!(future_of(&world)?.as_deref(), Some("1.1.0"));
    Ok(())
}

// Aside-first removal.

pub(super) async fn releases_set_aside_leave_their_names_at_once_with_nothing_recorded_until_settled<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    let engine = engine(&world).await?;
    let lock = engine.lock().await?;

    let batch = engine.set_aside(&lock, &[key("1.0.0")?, key("1.1.0")?]).await?;

    assert_eq!(batch.releases.len(), 2);
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.2.0"]);
    let listed = listing(&world).await?;
    assert_eq!(listed.removing.len(), 2);
    assert_eq!(listed.releases.len(), 1);
    assert_eq!(condition_of(&read(&world, "1.0.0").await?), "removed");
    assert!(
        entries(&world).await?.iter().all(|entry| entry.removal.is_none()),
        "nothing is recorded as removed while a release is only set aside"
    );
    for aside in &batch.releases {
        assert!(
            world
                .store_dir()
                .join(format!("_removing-{}", aside.folder.as_str()))
                .join("manifest.toml")
                .is_file()
        );
    }
    Ok(())
}

pub(super) async fn settling_a_batch_renames_back_what_is_kept_and_records_and_deletes_the_rest<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    let engine = engine(&world).await?;
    let lock = engine.lock().await?;
    let batch = engine.set_aside(&lock, &[key("1.0.0")?, key("1.1.0")?]).await?;

    let settled = engine.settle(&lock, batch, &[key("1.1.0")?]).await?;

    assert_eq!(
        settled
            .removed
            .iter()
            .map(|id| id.key.version.to_string())
            .collect::<Vec<_>>(),
        ["1.0.0"]
    );
    assert_eq!(
        settled
            .restored
            .iter()
            .map(|id| id.key.version.to_string())
            .collect::<Vec<_>>(),
        ["1.1.0"]
    );
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.1.0", "1.2.0"]);
    assert_eq!(listing(&world).await?.removing.len(), 0);
    let recorded = entry_of(&world, "1.0.0").await?.removal.ok_or("no removal recorded")?;
    assert_eq!(recorded.cause, RecordedCause::Retention);
    assert!(entry_of(&world, "1.1.0").await?.removal.is_none());
    assert_eq!(condition_of(&read(&world, "1.1.0").await?), "verified");
    Ok(())
}

pub(super) async fn settling_a_batch_for_a_requested_removal_records_the_cause_the_request_has<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    let engine = engine(&world).await?;
    let lock = engine.lock().await?;
    let batch = engine.set_aside(&lock, &[key("1.0.0")?]).await?;

    engine
        .settle_requested(&lock, batch, &[], RequestedRemoval::Forced)
        .await?;

    let recorded = entry_of(&world, "1.0.0").await?.removal.ok_or("no removal recorded")?;
    assert_eq!(recorded.cause, RecordedCause::Forced);
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.1.0", "1.2.0"]);
    Ok(())
}

pub(super) async fn setting_aside_a_release_the_ledger_does_not_record_is_refused_before_any_release_moves<
    W: StoreWorld,
>() -> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    let engine = engine(&world).await?;
    let lock = engine.lock().await?;

    let refused = engine.set_aside(&lock, &[key("1.0.0")?, key("9.0.0")?]).await;

    assert_eq!(code_of(refused)?, "packages/not-found");
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    assert_eq!(listing(&world).await?.removing.len(), 0);
    Ok(())
}

pub(super) async fn a_removal_is_renamed_aside_before_it_is_recorded_and_deleted_after<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>(CURRENT_AND_PREVIOUS).await?;

    world.remove("crm", "1.0.0", false).await?;

    let recorded = entry_of(&world, "1.0.0").await?.removal.ok_or("no removal recorded")?;
    assert_eq!(recorded.cause, RecordedCause::ByRequest);
    assert_eq!(
        listing(&world).await?.removing.len(),
        0,
        "the aside folder is deleted once recorded"
    );
    assert_eq!(condition_of(&read(&world, "1.0.0").await?), "removed");
    Ok(())
}

// Recovery.

pub(super) async fn recovery_deletes_a_staging_folder_and_leaves_every_release_alone<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    std::fs::create_dir_all(world.store_dir().join("_staging-a1/sync"))?;
    std::fs::write(world.store_dir().join("_staging-a1/sync/sync.rhai"), b"half")?;
    let listed = listing(&world).await?;
    assert_eq!(listed.staging.len(), 1);

    let recovered = recover(&world).await?;

    assert_eq!(recovered.staging_deleted.len(), 1);
    assert_eq!(recovered.restored.len(), 0);
    assert_eq!(recovered.removing_deleted.len(), 0);
    assert_eq!(top_level(&world.store_dir()), ["crm", "ledger.toml"]);
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    Ok(())
}

pub(super) async fn recovery_renames_back_a_removal_folder_the_ledger_records_no_removal_for<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    std::fs::rename(
        world.store_dir().join("crm/1.0.0"),
        world.store_dir().join("_removing-r1"),
    )?;
    assert_eq!(condition_of(&read(&world, "1.0.0").await?), "removed");

    let recovered = recover(&world).await?;

    assert_eq!(recovered.restored, [key("1.0.0")?]);
    assert_eq!(condition_of(&read(&world, "1.0.0").await?), "verified");
    assert_eq!(listing(&world).await?.removing.len(), 0);
    assert!(entry_of(&world, "1.0.0").await?.removal.is_none());
    Ok(())
}

pub(super) async fn recovery_deletes_a_removal_folder_the_ledger_records_a_removal_for<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>(CURRENT_AND_PREVIOUS).await?;
    let copy = world.store_dir().join("../copy-of-1.0.0");
    copy_tree(&world.store_dir().join("crm/1.0.0"), &copy)?;
    world.remove("crm", "1.0.0", false).await?;
    copy_tree(&copy, &world.store_dir().join("_removing-r2"))?;

    let recovered = recover(&world).await?;

    assert_eq!(recovered.removing_deleted.len(), 1);
    assert_eq!(recovered.restored.len(), 0);
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.1.0", "1.2.0"]);
    assert_eq!(listing(&world).await?.removing.len(), 0);
    std::fs::remove_dir_all(copy)?;
    Ok(())
}

pub(super) async fn recovery_deletes_a_removal_folder_whose_release_has_been_put_back_in_place<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    copy_tree(
        &world.store_dir().join("crm/1.0.0"),
        &world.store_dir().join("_removing-r3"),
    )?;

    let recovered = recover(&world).await?;

    assert_eq!(recovered.removing_deleted.len(), 1);
    assert_eq!(condition_of(&read(&world, "1.0.0").await?), "verified");
    assert_eq!(listing(&world).await?.removing.len(), 0);
    Ok(())
}

pub(super) async fn recovery_leaves_a_removal_folder_whose_manifest_cannot_be_read_untouched<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    std::fs::create_dir_all(world.store_dir().join("_removing-r4"))?;
    std::fs::write(world.store_dir().join("_removing-r4/junk.txt"), b"unknown")?;

    let recovered = recover(&world).await?;

    assert_eq!(recovered.removing_deleted.len(), 0);
    assert_eq!(recovered.restored.len(), 0);
    assert_eq!(
        std::fs::read(world.store_dir().join("_removing-r4/junk.txt"))?,
        b"unknown"
    );
    Ok(())
}

pub(super) async fn entries_that_are_not_releases_are_reported_as_unrecognised_and_recovery_leaves_them<
    W: StoreWorld,
>() -> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    std::fs::write(world.store_dir().join("notes.txt"), b"mine")?;
    std::fs::write(world.store_dir().join("crm/readme.txt"), b"mine")?;

    let listed = listing(&world).await?;
    recover(&world).await?;

    let mut unrecognised = listed.unrecognised.clone();
    unrecognised.sort();
    assert!(
        unrecognised.iter().any(|name| name.contains("notes.txt")),
        "{unrecognised:?}"
    );
    assert!(
        unrecognised.iter().any(|name| name.contains("readme.txt")),
        "{unrecognised:?}"
    );
    assert_eq!(listed.releases.len(), 3);
    assert_eq!(std::fs::read(world.store_dir().join("notes.txt"))?, b"mine");
    assert_eq!(std::fs::read(world.store_dir().join("crm/readme.txt"))?, b"mine");
    Ok(())
}

/// The ledger and the folders agree after any sequence of the operations above: every release without a removal is a
/// whole folder, and every removal recorded has no folder.
pub(super) async fn the_ledger_and_the_folders_agree_after_an_install_a_removal_and_a_retention<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>(CURRENT_1_2).await?;
    world.remove("crm", "1.2.0", true).await?;
    world.write_state("[crm.sync]\ncurrent = \"1.1.0\"\n")?;
    world.retain().await?;

    let reading = ledger(&world).await?;
    let LedgerState::Present(ledger) = reading.state else {
        return Err("expected a ledger".into());
    };
    assert_eq!(reading.unrecorded_removals.len(), 0);
    for entry in &ledger.entries {
        let folder = world.store_dir().join(format!("crm/{}", entry.release.key.version));
        assert_eq!(folder.exists(), entry.removal.is_none(), "{:?}", entry.release.key);
    }
    let _: Option<PackagesError> = None;
    Ok(())
}
