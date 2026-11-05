use super::*;
use crate::check::ReadCheck;
use crate::io::StoreFailureKind;
use crate::ledger::{RecordedCause, ReleaseOrigin, RequestedRemoval};
use crate::store::{AsideRelease, PackageStore};
use crate::test_support::{
    Built, START, TestEngine, assert_no_partial_release, engine, key, ledger_of, leftovers, tamper,
};
use crate::test_support::{Must, MustFail};

fn release(version: &str) -> Built {
    Built::new(
        "report",
        version,
        &[("main.rhai", version.as_bytes()), ("t/a.html", b"<p>")],
    )
}

async fn with_releases(versions: &[&str]) -> TestEngine {
    let engine = engine();
    let lock = engine.lock().await.must("lock");
    for version in versions {
        engine
            .install(&lock, release(version).request(ReleaseOrigin::Source))
            .await
            .must("installs");
    }
    engine
}

async fn whole(engine: &TestEngine, version: &str) -> bool {
    matches!(
        engine.read_release(&key("report", version)).await,
        ReadCheck::Verified(_)
    )
}

#[tokio::test]
async fn a_release_is_removed_at_request_recorded_and_deleted() {
    let engine = with_releases(&["1.0.0", "1.1.0"]).await;
    engine.clock.advance(90);
    let lock = engine.lock().await.must("lock");
    let removed = engine
        .remove(&lock, &key("report", "1.0.0"), RequestedRemoval::ByRequest)
        .await
        .must("removes");
    assert_eq!(removed.release, release("1.0.0").id());
    assert_eq!(removed.cause, RecordedCause::ByRequest);
    assert!(matches!(
        engine.read_release(&key("report", "1.0.0")).await,
        ReadCheck::Removed
    ));
    assert!(whole(&engine, "1.1.0").await);
    assert_eq!(leftovers(&engine.io).len(), 0);
    let ledger = ledger_of(&engine).await;
    let removal = ledger.entries[0].removal.must("recorded");
    assert_eq!(removal.cause, RecordedCause::ByRequest);
    assert_eq!(removal.removed_at, START + time::Duration::seconds(90));
    assert_eq!(ledger.entries[1].removal, None);
}

#[tokio::test]
async fn an_overridden_guard_is_recorded_as_forced() {
    let engine = with_releases(&["1.0.0"]).await;
    let lock = engine.lock().await.must("lock");
    let removed = engine
        .remove(&lock, &key("report", "1.0.0"), RequestedRemoval::Forced)
        .await
        .must("removes");
    assert_eq!(removed.cause, RecordedCause::Forced);
    assert_eq!(
        ledger_of(&engine).await.entries[0].removal.must("recorded").cause,
        RecordedCause::Forced
    );
}

#[tokio::test]
async fn a_damaged_or_incomplete_release_can_be_removed() {
    for incomplete in [false, true] {
        let engine = with_releases(&["1.0.0"]).await;
        if incomplete {
            engine.io.delete("report/1.0.0/main.rhai");
        } else {
            tamper(&engine.io, &release("1.0.0"), "main.rhai");
        }
        let lock = engine.lock().await.must("lock");
        engine
            .remove(&lock, &key("report", "1.0.0"), RequestedRemoval::ByRequest)
            .await
            .must("removes");
        assert!(matches!(
            engine.read_release(&key("report", "1.0.0")).await,
            ReadCheck::Removed
        ));
    }
}

#[tokio::test]
async fn removing_a_release_that_is_not_there_is_not_found_and_changes_nothing() {
    let engine = with_releases(&["1.0.0"]).await;
    let before = engine.io.snapshot();
    let lock = engine.lock().await.must("lock");
    let error = engine
        .remove(&lock, &key("report", "9.9.9"), RequestedRemoval::ByRequest)
        .await
        .must_fail("refused");
    assert!(matches!(error, PackagesError::NotFound { .. }));
    assert_eq!(error.code(), "packages/not-found");
    engine
        .remove(&lock, &key("report", "1.0.0"), RequestedRemoval::ByRequest)
        .await
        .must("removes");
    let again = engine
        .remove(&lock, &key("report", "1.0.0"), RequestedRemoval::ByRequest)
        .await
        .must_fail("gone");
    assert_eq!(again.code(), "packages/not-found");
    assert_ne!(engine.io.snapshot(), before);
}

#[tokio::test]
async fn nothing_is_removed_while_the_ledger_is_missing_unreadable_or_silent_about_the_release() {
    let engine = with_releases(&["1.0.0"]).await;
    let lock = engine.lock().await.must("lock");
    Built::new("report", "2.0.0", &[]).put_into(&engine.io);
    let before = engine.io.snapshot();
    let error = engine
        .remove(&lock, &key("report", "2.0.0"), RequestedRemoval::Forced)
        .await
        .must_fail("refused");
    assert_eq!(error.code(), "packages/ledger-unreadable");
    engine.io.delete("ledger.toml");
    assert!(matches!(
        engine
            .remove(&lock, &key("report", "1.0.0"), RequestedRemoval::ByRequest)
            .await,
        Err(PackagesError::LedgerMissing)
    ));
    engine.io.put_file("ledger.toml", b"[");
    assert!(matches!(
        engine
            .remove(&lock, &key("report", "1.0.0"), RequestedRemoval::ByRequest)
            .await,
        Err(PackagesError::LedgerUnreadable { .. })
    ));
    assert!(engine.io.exists("report/1.0.0/manifest.toml"));
    assert_ne!(
        engine.io.snapshot(),
        before,
        "the ledger was replaced by the test itself"
    );
}

#[tokio::test]
async fn a_failed_ledger_write_puts_the_release_back_under_its_name() {
    let engine = with_releases(&["1.0.0"]).await;
    engine.io.reset_mutation_count();
    // Mutations: create the removal folder's rename (0), write the ledger (1).
    engine.io.fail_once(1, StoreFailureKind::Failed);
    let lock = engine.lock().await.must("lock");
    assert!(
        engine
            .remove(&lock, &key("report", "1.0.0"), RequestedRemoval::ByRequest)
            .await
            .is_err()
    );
    assert!(whole(&engine, "1.0.0").await);
    assert_eq!(leftovers(&engine.io).len(), 0);
    assert_eq!(ledger_of(&engine).await.entries[0].removal, None);
}

#[tokio::test]
async fn a_crash_at_any_step_of_a_removal_ends_whole_or_removed_and_recorded() {
    let total = {
        let engine = with_releases(&["1.0.0"]).await;
        engine.io.reset_mutation_count();
        let lock = engine.lock().await.must("lock");
        engine
            .remove(&lock, &key("report", "1.0.0"), RequestedRemoval::ByRequest)
            .await
            .must("removes");
        engine.io.mutations()
    };
    for step in 0..total {
        let engine = with_releases(&["1.0.0"]).await;
        engine.io.reset_mutation_count();
        engine.io.crash_at(step);
        let lock = engine.lock().await.must("lock");
        let _ = engine
            .remove(&lock, &key("report", "1.0.0"), RequestedRemoval::ByRequest)
            .await;
        engine.io.revive();
        assert_no_partial_release(&engine, &key("report", "1.0.0"), false).await;
        engine.recover(&lock).await.must("recovers");
        assert_eq!(leftovers(&engine.io).len(), 0, "step {step}: {:?}", engine.io.paths());
        let entry = ledger_of(&engine).await.entries.remove(0);
        match entry.removal {
            None => assert!(
                whole(&engine, "1.0.0").await,
                "step {step}: unrecorded removal must leave it whole"
            ),
            Some(_) => assert!(
                matches!(engine.read_release(&key("report", "1.0.0")).await, ReadCheck::Removed),
                "step {step}"
            ),
        }
    }
}

#[tokio::test]
async fn releases_set_aside_leave_their_names_and_the_ledger_untouched() {
    let engine = with_releases(&["1.0.0", "1.1.0", "1.2.0"]).await;
    let ledger_before = engine.io.file("ledger.toml");
    let lock = engine.lock().await.must("lock");
    let keys = [key("report", "1.0.0"), key("report", "1.1.0"), key("report", "1.0.0")];
    let batch = engine.set_aside(&lock, &keys).await.must("sets aside");
    assert_eq!(batch.releases.len(), 2, "a repeated release is set aside once");
    assert_eq!(batch.releases[0].release, release("1.0.0").id());
    assert!(matches!(
        engine.read_release(&key("report", "1.0.0")).await,
        ReadCheck::Removed
    ));
    assert!(matches!(
        engine.read_release(&key("report", "1.1.0")).await,
        ReadCheck::Removed
    ));
    assert!(whole(&engine, "1.2.0").await);
    assert_eq!(engine.io.file("ledger.toml"), ledger_before);
    for AsideRelease { folder, .. } in &batch.releases {
        assert!(
            engine
                .io
                .exists(&format!("_removing-{}/manifest.toml", folder.as_str()))
        );
    }
}

#[tokio::test]
async fn setting_aside_an_absent_release_renames_back_what_it_had_moved() {
    let engine = with_releases(&["1.0.0", "1.1.0"]).await;
    let before = engine.io.snapshot();
    let lock = engine.lock().await.must("lock");
    let error = engine
        .set_aside(&lock, &[key("report", "1.0.0"), key("report", "9.9.9")])
        .await
        .must_fail("refused");
    assert_eq!(error.code(), "packages/not-found");
    assert_eq!(engine.io.snapshot(), before);
}

#[tokio::test]
async fn a_failed_rename_while_setting_aside_renames_back_the_earlier_ones() {
    let engine = with_releases(&["1.0.0", "1.1.0"]).await;
    let before = engine.io.snapshot();
    engine.io.reset_mutation_count();
    engine.io.fail_once(1, StoreFailureKind::Failed);
    let lock = engine.lock().await.must("lock");
    let error = engine
        .set_aside(&lock, &[key("report", "1.0.0"), key("report", "1.1.0")])
        .await
        .must_fail("fails");
    assert_eq!(error.code(), "packages/store-unavailable");
    assert_eq!(engine.io.snapshot(), before);
}

#[tokio::test]
async fn settling_restores_what_is_kept_and_records_and_deletes_the_rest() {
    let engine = with_releases(&["1.0.0", "1.1.0", "1.2.0"]).await;
    engine.clock.advance(30);
    let lock = engine.lock().await.must("lock");
    let batch = engine
        .set_aside(&lock, &[key("report", "1.0.0"), key("report", "1.1.0")])
        .await
        .must("aside");
    let settled = engine
        .settle(&lock, batch, &[key("report", "1.1.0"), key("report", "7.7.7")])
        .await
        .must("settles");
    assert_eq!(settled.removed, [release("1.0.0").id()]);
    assert_eq!(settled.restored, [release("1.1.0").id()]);
    assert!(whole(&engine, "1.1.0").await && whole(&engine, "1.2.0").await);
    assert!(matches!(
        engine.read_release(&key("report", "1.0.0")).await,
        ReadCheck::Removed
    ));
    assert_eq!(leftovers(&engine.io).len(), 0);
    let ledger = ledger_of(&engine).await;
    let removal = ledger.entries[0].removal.must("retention recorded");
    assert_eq!(
        (removal.cause, removal.removed_at),
        (RecordedCause::Retention, START + time::Duration::seconds(30))
    );
    assert!(ledger.entries[1].removal.is_none() && ledger.entries[2].removal.is_none());
}

#[tokio::test]
async fn settling_a_requested_removal_records_the_cause_of_the_request_and_restores_what_is_kept() {
    for (reason, expected) in [
        (RequestedRemoval::ByRequest, RecordedCause::ByRequest),
        (RequestedRemoval::Forced, RecordedCause::Forced),
    ] {
        let engine = with_releases(&["1.0.0", "1.1.0", "1.2.0"]).await;
        let lock = engine.lock().await.must("lock");
        let batch = engine
            .set_aside(&lock, &[key("report", "1.0.0"), key("report", "1.1.0")])
            .await
            .must("aside");

        let settled = engine
            .settle_requested(&lock, batch, &[key("report", "1.1.0")], reason)
            .await
            .must("settles");

        assert_eq!(settled.removed, [release("1.0.0").id()]);
        assert_eq!(settled.restored, [release("1.1.0").id()]);
        let ledger = ledger_of(&engine).await;
        assert_eq!(ledger.entries[0].removal.must("recorded").cause, expected);
        assert!(ledger.entries[1].removal.is_none());
        assert_eq!(leftovers(&engine.io).len(), 0);
    }
}

#[tokio::test]
async fn an_unsettled_batch_is_renamed_back_by_the_next_recovery() {
    let engine = with_releases(&["1.0.0", "1.1.0"]).await;
    let lock = engine.lock().await.must("lock");
    engine
        .set_aside(&lock, &[key("report", "1.0.0"), key("report", "1.1.0")])
        .await
        .must("aside");
    let recovered = engine.recover(&lock).await.must("recovers");
    assert_eq!(recovered.restored, [key("report", "1.0.0"), key("report", "1.1.0")]);
    assert!(whole(&engine, "1.0.0").await && whole(&engine, "1.1.0").await);
    assert_eq!(leftovers(&engine.io).len(), 0);
}

#[tokio::test]
async fn a_crash_at_any_step_of_settling_leaves_each_release_whole_or_removed_and_recorded() {
    let setup = || async {
        let engine = with_releases(&["1.0.0", "1.1.0", "1.2.0"]).await;
        let lock = engine.lock().await.must("lock");
        let batch = engine
            .set_aside(&lock, &[key("report", "1.0.0"), key("report", "1.1.0")])
            .await
            .must("aside");
        engine.io.reset_mutation_count();
        (engine, batch)
    };
    let total = {
        let (engine, batch) = setup().await;
        let lock = engine.lock().await.must("lock");
        engine
            .settle(&lock, batch, &[key("report", "1.1.0")])
            .await
            .must("settles");
        engine.io.mutations()
    };
    for step in 0..total {
        let (engine, batch) = setup().await;
        engine.io.crash_at(step);
        let lock = engine.lock().await.must("lock");
        let _ = engine.settle(&lock, batch, &[key("report", "1.1.0")]).await;
        engine.io.revive();
        engine.recover(&lock).await.must("recovers");
        assert_eq!(leftovers(&engine.io).len(), 0, "step {step}: {:?}", engine.io.paths());
        assert!(
            whole(&engine, "1.1.0").await && whole(&engine, "1.2.0").await,
            "step {step}: kept releases stay whole"
        );
        let ledger = ledger_of(&engine).await;
        match ledger.entries[0].removal {
            None => assert!(whole(&engine, "1.0.0").await, "step {step}"),
            Some(_) => assert!(
                matches!(engine.read_release(&key("report", "1.0.0")).await, ReadCheck::Removed),
                "step {step}"
            ),
        }
        assert!(
            ledger.entries[1].removal.is_none(),
            "step {step}: a kept release is never recorded as removed"
        );
    }
}

#[tokio::test]
async fn removing_the_last_release_of_a_package_removes_its_empty_folder() {
    let engine = with_releases(&["1.0.0"]).await;
    let lock = engine.lock().await.must("lock");
    engine
        .remove(&lock, &key("report", "1.0.0"), RequestedRemoval::ByRequest)
        .await
        .must("removes");
    assert!(!engine.io.exists("report"), "{:?}", engine.io.paths());
    assert_eq!(leftovers(&engine.io).len(), 0);
}

#[tokio::test]
async fn removing_one_of_two_releases_keeps_the_package_folder() {
    let engine = with_releases(&["1.0.0", "1.1.0"]).await;
    let lock = engine.lock().await.must("lock");
    engine
        .remove(&lock, &key("report", "1.0.0"), RequestedRemoval::ByRequest)
        .await
        .must("removes");
    assert!(engine.io.exists("report"));
    assert!(whole(&engine, "1.1.0").await);
}

#[tokio::test]
async fn a_package_folder_holding_something_else_is_kept_when_its_last_release_is_removed() {
    let engine = with_releases(&["1.0.0"]).await;
    engine.io.put_file("report/notes.txt", b"mine");
    let lock = engine.lock().await.must("lock");
    engine
        .remove(&lock, &key("report", "1.0.0"), RequestedRemoval::ByRequest)
        .await
        .must("removes");
    assert_eq!(engine.io.file("report/notes.txt"), Some(b"mine".to_vec()));
}

#[tokio::test]
async fn settling_retention_removes_the_folder_of_a_package_left_with_no_release() {
    let engine = with_releases(&["1.0.0"]).await;
    let lock = engine.lock().await.must("lock");
    let batch = engine
        .set_aside(&lock, &[key("report", "1.0.0")])
        .await
        .must("sets aside");
    engine.settle(&lock, batch, &[]).await.must("settles");
    assert!(!engine.io.exists("report"), "{:?}", engine.io.paths());
}

#[tokio::test]
async fn a_release_kept_when_retention_settles_keeps_its_package_folder() {
    let engine = with_releases(&["1.0.0"]).await;
    let lock = engine.lock().await.must("lock");
    let batch = engine
        .set_aside(&lock, &[key("report", "1.0.0")])
        .await
        .must("sets aside");
    engine
        .settle(&lock, batch, &[key("report", "1.0.0")])
        .await
        .must("settles");
    assert!(whole(&engine, "1.0.0").await);
}
