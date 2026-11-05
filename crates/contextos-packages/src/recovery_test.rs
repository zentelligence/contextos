use super::*;
use crate::check::ReadCheck;
use crate::ledger::{EntryProvenance, LedgerProblem, LedgerState, ReleaseOrigin, RequestedRemoval};
use crate::store::PackageStore;
use crate::test_support::{Built, FakeIo, START, TestEngine, engine, engine_over, key, ledger_of, leftovers, tamper};
use crate::test_support::{Must, MustFail};

fn release(version: &str) -> Built {
    Built::new("report", version, &[("main.rhai", version.as_bytes())])
}

async fn with_releases(versions: &[&str]) -> TestEngine {
    let engine = engine();
    let lock = engine.lock().await.must("lock");
    for version in versions {
        engine
            .install(&lock, release(version).request(ReleaseOrigin::Bundle))
            .await
            .must("installs");
    }
    engine
}

#[tokio::test]
async fn recovering_a_clean_store_changes_nothing() {
    let engine = with_releases(&["1.0.0"]).await;
    engine.io.put_file(".lock", b"");
    engine.io.put_file("stray.txt", b"x");
    let before = engine.io.snapshot();
    engine.io.reset_mutation_count();
    let lock = engine.lock().await.must("lock");
    assert_eq!(engine.recover(&lock).await.must("recovers"), Recovered::default());
    assert_eq!(engine.io.snapshot(), before);
    assert_eq!(engine.io.mutations(), 0);
}

#[tokio::test]
async fn staging_folders_are_deleted() {
    let engine = with_releases(&["1.0.0"]).await;
    engine.io.put_file("_staging-a1/half.txt", b"x");
    engine.io.put_dir("_staging-b2");
    let lock = engine.lock().await.must("lock");
    let recovered = engine.recover(&lock).await.must("recovers");
    assert_eq!(
        recovered
            .staging_deleted
            .iter()
            .map(crate::name::OperationId::as_str)
            .collect::<Vec<_>>(),
        ["a1", "b2"]
    );
    assert_eq!(leftovers(&engine.io).len(), 0);
}

#[tokio::test]
async fn a_removal_folder_with_no_recorded_removal_is_renamed_back() {
    let engine = with_releases(&["1.0.0"]).await;
    release("1.0.0").put_under(&engine.io, "_removing-r1");
    engine.io.delete("report");
    let lock = engine.lock().await.must("lock");
    let recovered = engine.recover(&lock).await.must("recovers");
    assert_eq!(recovered.restored, [key("report", "1.0.0")]);
    assert!(matches!(
        engine.read_release(&key("report", "1.0.0")).await,
        ReadCheck::Verified(_)
    ));
    assert_eq!(leftovers(&engine.io).len(), 0);
}

#[tokio::test]
async fn a_removal_folder_with_a_recorded_removal_is_deleted() {
    let engine = with_releases(&["1.0.0"]).await;
    let lock = engine.lock().await.must("lock");
    engine
        .remove(&lock, &key("report", "1.0.0"), RequestedRemoval::ByRequest)
        .await
        .must("removes");
    release("1.0.0").put_under(&engine.io, "_removing-r1");
    let recovered = engine.recover(&lock).await.must("recovers");
    assert_eq!(
        recovered
            .removing_deleted
            .iter()
            .map(crate::name::OperationId::as_str)
            .collect::<Vec<_>>(),
        ["r1"]
    );
    assert_eq!(recovered.restored.len(), 0);
    assert!(matches!(
        engine.read_release(&key("report", "1.0.0")).await,
        ReadCheck::Removed
    ));
    assert_eq!(leftovers(&engine.io).len(), 0);
}

#[tokio::test]
async fn a_removal_folder_beside_a_refilled_release_folder_is_deleted_not_restored() {
    let engine = with_releases(&["1.0.0"]).await;
    let mut old = release("1.0.0");
    old.files.insert("main.rhai".to_owned(), b"the damaged copy".to_vec());
    old.put_under(&engine.io, "_removing-r1");
    let lock = engine.lock().await.must("lock");
    let recovered = engine.recover(&lock).await.must("recovers");
    assert_eq!(recovered.removing_deleted.len(), 1);
    assert!(matches!(
        engine.read_release(&key("report", "1.0.0")).await,
        ReadCheck::Verified(_)
    ));
}

#[tokio::test]
async fn a_removal_folder_whose_release_cannot_be_named_is_left_untouched() {
    let engine = with_releases(&["1.0.0"]).await;
    engine.io.put_file("_removing-x1/manifest.toml", b"not toml [");
    engine.io.put_file("_removing-x2/main.rhai", b"no manifest");
    let before = engine.io.snapshot();
    let lock = engine.lock().await.must("lock");
    assert_eq!(engine.recover(&lock).await.must("recovers"), Recovered::default());
    assert_eq!(engine.io.snapshot(), before);
}

#[tokio::test]
async fn entries_the_engine_does_not_own_are_never_touched() {
    let engine = with_releases(&["1.0.0"]).await;
    for name in ["notes.txt", "_other"] {
        engine.io.put_file(name, b"x");
    }
    engine.io.put_file("report/readme.txt", b"x");
    engine.io.put_dir("_staging-");
    engine.io.put_dir("Report");
    let before = engine.io.snapshot();
    let lock = engine.lock().await.must("lock");
    engine.recover(&lock).await.must("recovers");
    assert_eq!(engine.io.snapshot(), before);
}

#[tokio::test]
async fn recovery_is_refused_while_the_ledger_is_missing_or_unreadable() {
    let engine = with_releases(&["1.0.0"]).await;
    engine.io.put_file("_staging-a1/x", b"x");
    let lock = engine.lock().await.must("lock");
    engine.io.delete("ledger.toml");
    assert!(matches!(engine.recover(&lock).await, Err(PackagesError::LedgerMissing)));
    engine.io.put_file("ledger.toml", b"[");
    assert!(matches!(
        engine.recover(&lock).await,
        Err(PackagesError::LedgerUnreadable { .. })
    ));
    assert!(engine.io.exists("_staging-a1/x"));
}

fn present_without_ledger(releases: &[Built]) -> TestEngine {
    let io = FakeIo::with_root();
    for release in releases {
        release.put_into(&io);
    }
    engine_over(io)
}

#[tokio::test]
async fn a_missing_ledger_is_rebuilt_from_the_verified_releases_present() {
    let engine = present_without_ledger(&[release("1.1.0"), release("1.0.0"), Built::new("other", "2.0.0", &[])]);
    engine.clock.advance(5);
    let lock = engine.lock().await.must("lock");
    assert!(matches!(
        engine.ledger().await.must("reads").state,
        LedgerState::Unusable(LedgerProblem::Missing)
    ));
    let rebuilt = engine.rebuild_ledger(&lock).await.must("rebuilds");
    let names: Vec<String> = rebuilt
        .recorded
        .iter()
        .map(|id| format!("{} {}", id.key.name.as_str(), id.key.version))
        .collect();
    assert_eq!(names, ["other 2.0.0", "report 1.0.0", "report 1.1.0"]);
    let ledger = ledger_of(&engine).await;
    assert_eq!(ledger.rebuilt_at, Some(START + time::Duration::seconds(5)));
    assert!(
        ledger
            .entries
            .iter()
            .all(|e| e.provenance == EntryProvenance::Rebuilt && e.removal.is_none())
    );
    assert_eq!(ledger.entries[1].interface.inputs, ["day"]);
    // Installing again against the rebuilt ledger is an unchanged install, and a repair dates the entry.
    let again = engine
        .install(&lock, release("1.0.0").request(ReleaseOrigin::Source))
        .await
        .must("installs");
    assert_eq!(again.outcome, crate::store::InstallOutcome::Unchanged);
    tamper(&engine.io, &release("1.0.0"), "main.rhai");
    let repaired = engine
        .install(&lock, release("1.0.0").request(ReleaseOrigin::Source))
        .await
        .must("repairs");
    assert_eq!(repaired.outcome, crate::store::InstallOutcome::Repaired);
    assert_eq!(
        ledger_of(&engine).await.entries[1].provenance,
        EntryProvenance::Installed {
            origin: ReleaseOrigin::Source,
            installed_at: START + time::Duration::seconds(5)
        }
    );
}

#[tokio::test]
async fn an_empty_store_rebuilds_an_empty_marked_ledger() {
    let engine = engine();
    let lock = engine.lock().await.must("lock");
    assert_eq!(engine.rebuild_ledger(&lock).await.must("rebuilds").recorded.len(), 0);
    let ledger = ledger_of(&engine).await;
    assert!(ledger.entries.is_empty() && ledger.rebuilt_at.is_some());
}

#[tokio::test]
async fn a_ledger_that_exists_is_never_rebuilt() {
    let engine = with_releases(&["1.0.0"]).await;
    let lock = engine.lock().await.must("lock");
    for text in [engine.io.file("ledger.toml").must("ledger"), b"[".to_vec(), Vec::new()] {
        engine.io.put_file("ledger.toml", &text);
        let before = engine.io.snapshot();
        let error = engine.rebuild_ledger(&lock).await.must_fail("refused");
        assert_eq!(error.code(), "packages/ledger-present");
        assert_eq!(engine.io.snapshot(), before);
    }
}

#[tokio::test]
async fn a_rebuild_is_refused_while_a_release_is_incomplete_or_unverifiable_and_writes_nothing() {
    let incomplete = present_without_ledger(&[release("1.0.0"), release("1.1.0")]);
    incomplete.io.delete("report/1.1.0/main.rhai");
    let lock = incomplete.lock().await.must("lock");
    let error = incomplete.rebuild_ledger(&lock).await.must_fail("refused");
    assert!(matches!(&error, PackagesError::ReleaseIncomplete { release } if *release == key("report", "1.1.0")));
    assert_eq!(error.code(), "packages/release-incomplete");
    assert!(!incomplete.io.exists("ledger.toml"));

    let unverifiable = present_without_ledger(&[release("1.0.0")]);
    unverifiable.io.before_list_tree(0, |state| state.unavailable = true);
    let lock = unverifiable.lock().await.must("lock");
    assert!(unverifiable.rebuild_ledger(&lock).await.is_err());
    assert!(!unverifiable.io.exists("ledger.toml"));
}

#[tokio::test]
async fn a_damaged_release_with_a_sound_manifest_is_recorded_so_installing_it_again_repairs_it() {
    let engine = present_without_ledger(&[release("1.0.0")]);
    tamper(&engine.io, &release("1.0.0"), "main.rhai");
    let lock = engine.lock().await.must("lock");
    assert_eq!(engine.rebuild_ledger(&lock).await.must("rebuilds").recorded.len(), 1);
    let repaired = engine
        .install(&lock, release("1.0.0").request(ReleaseOrigin::Bundle))
        .await
        .must("repairs");
    assert_eq!(repaired.outcome, crate::store::InstallOutcome::Repaired);
}

#[tokio::test]
async fn a_damaged_release_with_an_unreadable_manifest_blocks_a_rebuild() {
    let engine = present_without_ledger(&[release("1.0.0")]);
    engine.io.put_file("report/1.0.0/manifest.toml", b"not toml [");
    let lock = engine.lock().await.must("lock");
    assert!(matches!(
        engine.rebuild_ledger(&lock).await,
        Err(PackagesError::ReleaseIncomplete { .. })
    ));
}

#[tokio::test]
async fn a_rebuild_first_clears_what_unfinished_operations_left() {
    let engine = present_without_ledger(&[release("1.0.0")]);
    engine.io.put_file("_staging-a1/x", b"x");
    release("1.1.0").put_under(&engine.io, "_removing-r1");
    let lock = engine.lock().await.must("lock");
    let rebuilt = engine.rebuild_ledger(&lock).await.must("rebuilds");
    assert_eq!(
        rebuilt.recorded.len(),
        2,
        "the release that was aside is back and recorded"
    );
    assert_eq!(leftovers(&engine.io).len(), 0);
}

#[tokio::test]
async fn the_stores_lock_files_survive_recovery_and_a_ledger_rebuild_and_are_not_taken_for_releases() {
    let engine = present_without_ledger(&[release("1.0.0")]);
    engine.io.put_file(".lock", b"");
    engine.io.put_dir(".lock.queue");
    engine.io.put_file(".lock.queue.lock", b"");
    let lock = engine.lock().await.must("lock");
    let rebuilt = engine.rebuild_ledger(&lock).await.must("rebuilds");
    assert_eq!(rebuilt.recorded.len(), 1);
    engine.recover(&lock).await.must("recovers");
    let listing = engine.list().await.must("lists");
    assert_eq!(listing.unrecognised.len(), 0);
    assert!(engine.io.snapshot().contains_key(&vec![".lock".to_owned()]));
}
