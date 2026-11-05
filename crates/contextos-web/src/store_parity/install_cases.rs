//! Install, read classification, and ledger cases, written once against [`StoreWorld`].

use contextos_packages::{
    DamageFinding, EntryProvenance, IncompleteFinding, InstallOutcome, LedgerProblem, LedgerState, PackageStore,
    PackagesError, ReadCheck, ReadLimits, ReleaseOrigin, UnverifiableReason,
};

use super::store_support::{code_of, condition_of, entries, entry_of, install_versions, tamper, three_releases};
use super::worlds::{
    BoxError, StoreWorld, engine, engine_over, install, key, ledger, listing, read, request_of, top_level, try_install,
    versions,
};
use crate::retention_fixture::{archive, archive_with_script};

// Install.

pub(super) async fn a_new_release_is_installed_recorded_and_left_with_no_staging_folder<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = W::new().await?;

    let installed = install(&world, &archive("1.0.0")?).await?;

    assert_eq!(installed.outcome, InstallOutcome::Installed);
    assert_eq!(installed.release.key, key("1.0.0")?);
    assert_eq!(condition_of(&read(&world, "1.0.0").await?), "verified");
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.0.0"]);
    assert_eq!(top_level(&world.store_dir()), ["crm", "ledger.toml"]);
    let entry = entry_of(&world, "1.0.0").await?;
    assert!(entry.removal.is_none());
    assert!(
        matches!(
            entry.provenance,
            EntryProvenance::Installed {
                origin: ReleaseOrigin::Bundle,
                ..
            }
        ),
        "{:?}",
        entry.provenance
    );
    assert_eq!(entry.release.digest, installed.release.digest);
    Ok(())
}

pub(super) async fn installing_the_same_release_again_changes_nothing<W: StoreWorld>() -> Result<(), BoxError> {
    let world = W::new().await?;
    let first = install(&world, &archive("1.0.0")?).await?;
    let ledger_before = std::fs::read(world.store_dir().join("ledger.toml"))?;
    let script_before = std::fs::read(world.store_dir().join("crm/1.0.0/sync/sync.rhai"))?;

    let again = install(&world, &archive("1.0.0")?).await?;

    assert_eq!(again.outcome, InstallOutcome::Unchanged);
    assert_eq!(again.release, first.release);
    assert_eq!(std::fs::read(world.store_dir().join("ledger.toml"))?, ledger_before);
    assert_eq!(
        std::fs::read(world.store_dir().join("crm/1.0.0/sync/sync.rhai"))?,
        script_before
    );
    assert_eq!(entries(&world).await?.len(), 1);
    Ok(())
}

pub(super) async fn a_version_offered_again_with_another_digest_is_refused_and_the_installed_release_is_kept<
    W: StoreWorld,
>() -> Result<(), BoxError> {
    let world = W::new().await?;
    let first = install(&world, &archive("1.0.0")?).await?;
    let ledger_before = std::fs::read(world.store_dir().join("ledger.toml"))?;

    let refused = try_install(&world, &archive_with_script("1.0.0", "let other = 2;")?).await?;

    match refused {
        Err(PackagesError::DuplicateIdentity {
            release,
            recorded,
            offered,
        }) => {
            assert_eq!(release, key("1.0.0")?);
            assert_eq!(recorded, first.release.digest);
            assert_ne!(offered, recorded);
        }
        other => return Err(format!("expected a duplicate identity, got {other:?}").into()),
    }
    assert_eq!(std::fs::read(world.store_dir().join("ledger.toml"))?, ledger_before);
    assert_eq!(condition_of(&read(&world, "1.0.0").await?), "verified");
    assert_eq!(top_level(&world.store_dir()), ["crm", "ledger.toml"]);
    Ok(())
}

pub(super) async fn a_request_whose_files_the_manifest_does_not_describe_is_refused_and_writes_nothing<
    W: StoreWorld,
>() -> Result<(), BoxError> {
    let world = W::new().await?;
    let mut request = request_of(&archive("1.0.0")?)?;
    request.files.0.insert(
        contextos_packages::ReleaseFilePath::try_from("extra.txt")?,
        b"x".to_vec(),
    );
    let engine = engine(&world).await?;
    let lock = engine.lock().await?;

    let refused = engine.install(&lock, request).await;

    assert_eq!(code_of(refused)?, contextos_packages::codes::INVALID_MANIFEST);
    assert!(!world.store_dir().join("crm").exists());
    assert!(!world.store_dir().join("ledger.toml").exists());
    Ok(())
}

pub(super) async fn installing_a_recorded_release_that_is_removed_incomplete_or_damaged_repairs_it<W: StoreWorld>()
-> Result<(), BoxError> {
    for damage in ["removed", "incomplete", "damaged"] {
        let world = W::new().await?;
        install_versions(&world, &["1.0.0"]).await?;
        match damage {
            "removed" => std::fs::remove_dir_all(world.store_dir().join("crm/1.0.0"))?,
            "incomplete" => std::fs::remove_file(world.store_dir().join("crm/1.0.0/sync/sync.rhai"))?,
            _ => tamper(&world, "1.0.0")?,
        }
        assert_eq!(
            condition_of(&read(&world, "1.0.0").await?),
            damage,
            "the release reads as {damage} before the repair"
        );

        let repaired = install(&world, &archive("1.0.0")?).await?;

        assert_eq!(repaired.outcome, InstallOutcome::Repaired, "{damage}");
        assert_eq!(condition_of(&read(&world, "1.0.0").await?), "verified", "{damage}");
        assert_eq!(entries(&world).await?.len(), 1, "{damage}");
        assert_eq!(top_level(&world.store_dir()), ["crm", "ledger.toml"], "{damage}");
    }
    Ok(())
}

// Read classification.

pub(super) async fn a_release_that_matches_its_manifest_reads_as_verified_and_a_folder_that_is_gone_as_removed<
    W: StoreWorld,
>() -> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    std::fs::remove_dir_all(world.store_dir().join("crm/1.1.0"))?;

    let verified = read(&world, "1.2.0").await?;
    let removed = read(&world, "1.1.0").await?;
    let never_installed = read(&world, "9.0.0").await?;

    match verified {
        ReadCheck::Verified(release) => assert_eq!(release.release.key, key("1.2.0")?),
        other => return Err(format!("expected verified, got {}", condition_of(&other)).into()),
    }
    assert_eq!(condition_of(&removed), "removed");
    assert_eq!(condition_of(&never_installed), "removed");
    Ok(())
}

pub(super) async fn a_release_missing_a_listed_file_an_unlisted_entry_or_its_manifest_reads_as_incomplete<
    W: StoreWorld,
>() -> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    std::fs::remove_file(world.store_dir().join("crm/1.0.0/sync/sync.rhai"))?;
    std::fs::write(world.store_dir().join("crm/1.1.0/stray.txt"), b"stray")?;
    std::fs::remove_file(world.store_dir().join("crm/1.2.0/manifest.toml"))?;

    let absent = read(&world, "1.0.0").await?;
    let unlisted = read(&world, "1.1.0").await?;
    let no_manifest = read(&world, "1.2.0").await?;

    assert!(
        matches!(&absent, ReadCheck::Incomplete(found) if found.iter().any(|f| matches!(f, IncompleteFinding::ListedFileAbsent(_)))),
        "{}",
        condition_of(&absent)
    );
    assert!(
        matches!(&unlisted, ReadCheck::Incomplete(found) if found.iter().any(|f| matches!(f, IncompleteFinding::UnlistedEntry(name) if name.contains("stray.txt")))),
        "{}",
        condition_of(&unlisted)
    );
    assert!(
        matches!(&no_manifest, ReadCheck::Incomplete(found) if found.contains(&IncompleteFinding::ManifestMissing)),
        "{}",
        condition_of(&no_manifest)
    );
    Ok(())
}

pub(super) async fn a_release_whose_file_no_longer_hashes_to_its_manifest_reads_as_damaged<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    tamper(&world, "1.1.0")?;

    let damaged = read(&world, "1.1.0").await?;

    assert!(
        matches!(&damaged, ReadCheck::Damaged(found) if found.iter().any(|f| matches!(f, DamageFinding::FileHashMismatch(_)))),
        "{}",
        condition_of(&damaged)
    );
    assert_eq!(condition_of(&read(&world, "1.0.0").await?), "verified");
    Ok(())
}

pub(super) async fn a_release_that_exceeds_the_read_limits_reads_as_unverifiable_never_as_removed<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    let mut tight = engine_over(world.io().await?);
    tight.limits = ReadLimits {
        max_entries: 1,
        max_bytes: 1 << 20,
    };

    let check = tight.read_release(&key("1.0.0")?).await;

    assert!(
        matches!(&check, ReadCheck::Unverifiable(UnverifiableReason::LimitExceeded(limits)) if limits.max_entries == 1),
        "{}",
        condition_of(&check)
    );
    Ok(())
}

// The ledger.

pub(super) async fn an_empty_store_has_no_ledger_and_lists_nothing<W: StoreWorld>() -> Result<(), BoxError> {
    let world = W::new().await?;

    let reading = ledger(&world).await?;
    let listed = listing(&world).await?;

    assert!(matches!(reading.state, LedgerState::Absent), "{:?}", reading.state);
    assert_eq!(reading.unrecorded_removals.len(), 0);
    assert_eq!(listed.releases.len(), 0);
    assert_eq!(listed.staging.len(), 0);
    assert_eq!(listed.removing.len(), 0);
    assert_eq!(listed.unrecognised.len(), 0);
    Ok(())
}

pub(super) async fn a_store_with_releases_and_no_ledger_is_unusable_and_refuses_every_change<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("[crm.sync]\ncurrent = \"1.2.0\"\nprevious = \"1.1.0\"\n").await?;
    std::fs::remove_file(world.store_dir().join("ledger.toml"))?;

    let reading = ledger(&world).await?;
    let installed = try_install(&world, &archive("1.3.0")?).await?;
    let removed = world.remove("crm", "1.0.0", false).await.err();

    assert!(
        matches!(reading.state, LedgerState::Unusable(LedgerProblem::Missing)),
        "{:?}",
        reading.state
    );
    assert_eq!(code_of(installed)?, "packages/ledger-unreadable");
    assert_eq!(removed.map(|error| error.code()), Some("packages/ledger-unreadable"));
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    Ok(())
}

pub(super) async fn a_ledger_that_cannot_be_read_is_unusable_and_refuses_every_change_and_is_left_as_found<
    W: StoreWorld,
>() -> Result<(), BoxError> {
    let world = three_releases::<W>("[crm.sync]\ncurrent = \"1.2.0\"\nprevious = \"1.1.0\"\n").await?;
    std::fs::write(world.store_dir().join("ledger.toml"), "this is not a ledger")?;

    let reading = ledger(&world).await?;
    let installed = try_install(&world, &archive("1.3.0")?).await?;
    let removed = world.remove("crm", "1.0.0", false).await.err();
    let standing = world.standing().await?;

    assert!(
        matches!(reading.state, LedgerState::Unusable(LedgerProblem::Unreadable(_))),
        "{:?}",
        reading.state
    );
    assert_eq!(code_of(installed)?, "packages/ledger-unreadable");
    assert_eq!(removed.map(|error| error.code()), Some("packages/ledger-unreadable"));
    assert!(matches!(
        standing.ledger,
        crate::package_standing::LedgerStanding::Unreadable(_)
    ));
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    assert_eq!(
        std::fs::read(world.store_dir().join("ledger.toml"))?,
        b"this is not a ledger"
    );
    Ok(())
}

pub(super) async fn a_recorded_release_whose_folder_is_gone_is_read_as_removed_with_no_record<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    std::fs::remove_dir_all(world.store_dir().join("crm/1.0.0"))?;

    let reading = ledger(&world).await?;
    let standing = world.standing().await?;

    assert_eq!(
        reading
            .unrecorded_removals
            .iter()
            .map(|release| release.key.version.to_string())
            .collect::<Vec<_>>(),
        ["1.0.0"]
    );
    assert!(matches!(reading.state, LedgerState::Present(_)));
    let removed = standing
        .removed
        .iter()
        .find(|removed| removed.release.version.to_string() == "1.0.0");
    assert_eq!(
        removed.map(|removed| (removed.cause, removed.removed_at.is_none())),
        Some((contextos_packages::RemovalCause::Unrecorded, true))
    );
    Ok(())
}

pub(super) async fn a_release_folder_the_ledger_does_not_record_blocks_the_install_of_that_version<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    // The same version, put in place beside the ledger as if copied in by hand, under a version the ledger lacks.
    std::fs::create_dir_all(world.store_dir().join("crm/1.3.0"))?;
    std::fs::write(world.store_dir().join("crm/1.3.0/stray.txt"), b"stray")?;

    let refused = try_install(&world, &archive("1.3.0")?).await?;

    assert_eq!(code_of(refused)?, "packages/ledger-unreadable");
    assert_eq!(entries(&world).await?.len(), 3);
    Ok(())
}

pub(super) async fn a_missing_ledger_is_rebuilt_from_the_releases_present_and_changes_work_again<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("[crm.sync]\ncurrent = \"1.2.0\"\nprevious = \"1.1.0\"\n").await?;
    std::fs::remove_file(world.store_dir().join("ledger.toml"))?;
    let refused = world.remove("crm", "1.0.0", false).await.err();

    let rebuilt = world.rebuild().await?;
    world.remove("crm", "1.0.0", false).await?;

    assert_eq!(refused.map(|error| error.code()), Some("packages/ledger-unreadable"));
    assert_eq!(rebuilt.store, W::STORE);
    assert_eq!(rebuilt.recorded, ["crm 1.0.0", "crm 1.1.0", "crm 1.2.0"]);
    assert_eq!(versions(&world.store_dir(), "crm"), ["1.1.0", "1.2.0"]);
    let kept = entry_of(&world, "1.1.0").await?;
    assert_eq!(kept.provenance, EntryProvenance::Rebuilt);
    match ledger(&world).await?.state {
        LedgerState::Present(ledger) => assert!(ledger.rebuilt_at.is_some()),
        other => return Err(format!("expected a ledger, found {other:?}").into()),
    }
    Ok(())
}

pub(super) async fn a_ledger_that_is_present_is_not_rebuilt_whether_or_not_it_can_be_read<W: StoreWorld>()
-> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    let readable = world.rebuild().await.err();
    std::fs::write(world.store_dir().join("ledger.toml"), "this is not a ledger")?;
    let unreadable = world.rebuild().await.err();

    assert_eq!(readable.map(|error| error.code()), Some("packages/ledger-present"));
    assert_eq!(unreadable.map(|error| error.code()), Some("packages/ledger-present"));
    assert_eq!(
        std::fs::read(world.store_dir().join("ledger.toml"))?,
        b"this is not a ledger"
    );
    Ok(())
}

pub(super) async fn a_ledger_is_not_rebuilt_while_a_release_does_not_read_whole<W: StoreWorld>() -> Result<(), BoxError>
{
    let world = three_releases::<W>("").await?;
    std::fs::remove_file(world.store_dir().join("ledger.toml"))?;
    std::fs::remove_file(world.store_dir().join("crm/1.1.0/sync/sync.rhai"))?;

    let refused = world.rebuild().await.err();

    assert_eq!(refused.map(|error| error.code()), Some("packages/release-incomplete"));
    assert!(!world.store_dir().join("ledger.toml").exists());
    Ok(())
}

pub(super) async fn a_ledger_rebuild_records_a_release_whose_content_is_damaged_but_whose_manifest_is_sound<
    W: StoreWorld,
>() -> Result<(), BoxError> {
    let world = three_releases::<W>("").await?;
    std::fs::remove_file(world.store_dir().join("ledger.toml"))?;
    tamper(&world, "1.1.0")?;

    let rebuilt = world.rebuild().await?;

    assert_eq!(rebuilt.recorded, ["crm 1.0.0", "crm 1.1.0", "crm 1.2.0"]);
    assert_eq!(condition_of(&read(&world, "1.1.0").await?), "damaged");
    Ok(())
}

pub(super) async fn a_ledger_entry_records_the_interface_the_release_declares<W: StoreWorld>() -> Result<(), BoxError> {
    let world = W::new().await?;
    install(
        &world,
        &crate::retention_fixture::archive_declaring("1.0.0", "inputs = [{ name = \"region\", kind = \"string\" }]")?,
    )
    .await?;

    let entry = entry_of(&world, "1.0.0").await?;

    assert!(
        format!("{:?}", entry.interface).contains("region"),
        "{:?}",
        entry.interface
    );
    Ok(())
}
