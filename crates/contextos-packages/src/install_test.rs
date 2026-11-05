use proptest::prelude::*;

use super::*;
use crate::check::ReadCheck;
use crate::io::StoreFailureKind;
use crate::ledger::{EntryProvenance, LedgerEntry, ReleaseOrigin};
use crate::store::{InstallOutcome, PackageStore};
use crate::test_support::{
    Built, FakeIo, START, TestEngine, assert_no_partial_release, engine, engine_over, key, ledger_of, leftovers, tamper,
};
use crate::test_support::{Must, MustFail};

fn sample() -> Built {
    Built::new(
        "report",
        "1.0.0",
        &[
            ("main.rhai", b"let x = 1;"),
            ("templates/a.html", b"<p>"),
            ("empty", b""),
        ],
    )
}

async fn install(engine: &TestEngine, release: &Built) -> Result<crate::store::Installed, PackagesError> {
    let lock = engine.lock().await.must("lock");
    engine.install(&lock, release.request(ReleaseOrigin::Source)).await
}

#[tokio::test]
async fn a_new_release_is_installed_verified_and_recorded() {
    let engine = engine();
    let release = sample();
    let installed = install(&engine, &release).await.must("installs");
    assert_eq!(installed.outcome, InstallOutcome::Installed);
    assert_eq!(installed.release, release.id());

    let io = &engine.io;
    assert_eq!(
        io.paths(),
        [
            "ledger.toml",
            "report",
            "report/1.0.0",
            "report/1.0.0/empty",
            "report/1.0.0/main.rhai",
            "report/1.0.0/manifest.toml",
            "report/1.0.0/templates",
            "report/1.0.0/templates/a.html",
        ]
    );
    assert_eq!(
        io.file("report/1.0.0/manifest.toml"),
        Some(release.manifest_text().into_bytes())
    );
    assert!(matches!(
        engine.read_release(&key("report", "1.0.0")).await,
        ReadCheck::Verified(_)
    ));

    let ledger = ledger_of(&engine).await;
    assert_eq!(ledger.entries.len(), 1);
    let entry: &LedgerEntry<_> = &ledger.entries[0];
    assert_eq!(entry.release, release.id());
    assert_eq!(entry.interface.inputs, ["day"]);
    assert_eq!(
        entry.provenance,
        EntryProvenance::Installed {
            origin: ReleaseOrigin::Source,
            installed_at: START
        }
    );
    assert_eq!(entry.removal, None);
}

#[tokio::test]
async fn installing_the_same_release_again_changes_nothing() {
    let engine = engine();
    install(&engine, &sample()).await.must("installs");
    let before = engine.io.snapshot();
    engine.io.reset_mutation_count();
    let again = install(&engine, &sample()).await.must("installs again");
    assert_eq!(again.outcome, InstallOutcome::Unchanged);
    assert_eq!(engine.io.snapshot(), before);
    assert_eq!(engine.io.mutations(), 0);
}

#[tokio::test]
async fn a_second_version_and_a_second_package_are_recorded_in_order() {
    let engine = engine();
    install(&engine, &sample()).await.must("first");
    install(&engine, &Built::new("report", "1.0.1", &[("main.rhai", b"let x = 2;")]))
        .await
        .must("second");
    install(&engine, &Built::new("other", "1.0.0", &[])).await.must("third");
    let ledger = ledger_of(&engine).await;
    let recorded: Vec<String> = ledger
        .entries
        .iter()
        .map(|e| format!("{} {}", e.release.key.name.as_str(), e.release.key.version))
        .collect();
    assert_eq!(recorded, ["report 1.0.0", "report 1.0.1", "other 1.0.0"]);
    assert_eq!(engine.list().await.must("lists").releases.len(), 3);
}

#[tokio::test]
async fn a_version_is_never_reused_with_another_digest() {
    let engine = engine();
    install(&engine, &sample()).await.must("installs");
    let before = engine.io.snapshot();
    let other = Built::new("report", "1.0.0", &[("main.rhai", b"different")]);
    let error = install(&engine, &other).await.must_fail("refused");
    assert_eq!(error.code(), "packages/duplicate-identity");
    assert!(
        matches!(&error, PackagesError::DuplicateIdentity { recorded, offered, .. }
        if *recorded == sample().digest() && *offered == other.digest())
    );
    assert_eq!(engine.io.snapshot(), before);
}

#[tokio::test]
async fn a_request_that_does_not_describe_its_files_is_refused_before_anything_is_written() {
    let base = sample();
    let mut extra_file = base.clone();
    extra_file.unlisted.insert("main.rhai".to_owned());
    let mut missing_file = base.request(ReleaseOrigin::Bundle);
    missing_file.files.0.retain(|path, _| path.to_string() != "main.rhai");
    let mut wrong_bytes = base.request(ReleaseOrigin::Bundle);
    for bytes in wrong_bytes.files.0.values_mut() {
        bytes.push(b'!');
    }
    let mut wrong_hash = base.clone();
    wrong_hash
        .listed_hash_overrides
        .insert("main.rhai".to_owned(), "0".repeat(64));
    let mut wrong_digest = base.clone();
    wrong_digest.declared_digest = Some(format!("sha256:{}", "0".repeat(64)));
    let mut not_toml = base.request(ReleaseOrigin::Bundle);
    not_toml.manifest_text = "name = [".to_owned();
    let requests = vec![
        extra_file.request(ReleaseOrigin::Bundle),
        missing_file,
        wrong_bytes,
        wrong_hash.request(ReleaseOrigin::Bundle),
        wrong_digest.request(ReleaseOrigin::Bundle),
        not_toml,
    ];
    for request in requests {
        let engine = engine();
        let lock = engine.lock().await.must("lock");
        let error = engine.install(&lock, request).await.must_fail("refused");
        assert_eq!(error.code(), "packages/invalid-manifest", "{error}");
        assert_eq!(engine.io.mutations(), 0);
        assert_eq!(engine.io.paths(), Vec::<String>::new());
    }
}

#[tokio::test]
async fn a_release_over_the_limits_is_refused_before_anything_is_written() {
    let mut engine = engine();
    engine.limits.max_entries = 3;
    let error = install(&engine, &sample()).await.must_fail("refused");
    assert_eq!(error.code(), "packages/folder-too-large");
    assert_eq!(engine.io.mutations(), 0);
    let mut engine = self::engine();
    engine.limits.max_bytes = 5;
    assert_eq!(
        install(&engine, &sample()).await.must_fail("refused").code(),
        "packages/folder-too-large"
    );
    assert_eq!(engine.io.mutations(), 0);
}

#[tokio::test]
async fn nothing_is_installed_while_the_ledger_is_missing_or_unreadable() {
    let engine = engine();
    install(&engine, &sample()).await.must("installs");
    let next = Built::new("report", "1.0.1", &[]);

    engine.io.delete("ledger.toml");
    let before = engine.io.snapshot();
    assert!(matches!(
        install(&engine, &next).await,
        Err(PackagesError::LedgerMissing)
    ));
    assert_eq!(engine.io.snapshot(), before);

    engine.io.put_file("ledger.toml", b"schema = [");
    let before = engine.io.snapshot();
    let error = install(&engine, &next).await.must_fail("refused");
    assert!(matches!(error, PackagesError::LedgerUnreadable { .. }), "{error}");
    assert_eq!(error.code(), "packages/ledger-unreadable");
    assert_eq!(engine.io.snapshot(), before);
}

#[tokio::test]
async fn a_release_folder_the_ledger_does_not_record_is_not_installed_over() {
    let engine = engine();
    install(&engine, &sample()).await.must("installs");
    Built::new("report", "1.0.1", &[]).put_into(&engine.io);
    let before = engine.io.snapshot();
    let error = install(&engine, &Built::new("report", "1.0.1", &[]))
        .await
        .must_fail("refused");
    assert_eq!(error.code(), "packages/ledger-unreadable");
    assert_eq!(engine.io.snapshot(), before);
}

#[tokio::test]
async fn a_removed_release_is_restored_by_installing_it_again() {
    let engine = engine();
    let release = sample();
    install(&engine, &release).await.must("installs");
    {
        let lock = engine.lock().await.must("lock");
        engine
            .remove(
                &lock,
                &key("report", "1.0.0"),
                crate::ledger::RequestedRemoval::ByRequest,
            )
            .await
            .must("removes");
    }
    assert!(matches!(
        engine.read_release(&key("report", "1.0.0")).await,
        ReadCheck::Removed
    ));
    engine.clock.advance(60);
    let repaired = install(&engine, &release).await.must("repairs");
    assert_eq!(repaired.outcome, InstallOutcome::Repaired);
    assert!(matches!(
        engine.read_release(&key("report", "1.0.0")).await,
        ReadCheck::Verified(_)
    ));
    let ledger = ledger_of(&engine).await;
    assert_eq!(ledger.entries.len(), 1, "the repair adds no entry");
    assert_eq!(ledger.entries[0].removal, None);
    assert_eq!(leftovers(&engine.io).len(), 0);
}

#[tokio::test]
async fn a_damaged_or_incomplete_release_is_replaced_by_installing_it_again() {
    for damage in ["tamper", "delete-file", "extra-file", "delete-manifest"] {
        let engine = engine();
        let release = sample();
        install(&engine, &release).await.must("installs");
        match damage {
            "tamper" => tamper(&engine.io, &release, "main.rhai"),
            "delete-file" => engine.io.delete("report/1.0.0/main.rhai"),
            "extra-file" => engine.io.put_file("report/1.0.0/extra", b"x"),
            _ => engine.io.delete("report/1.0.0/manifest.toml"),
        }
        assert!(
            !matches!(
                engine.read_release(&key("report", "1.0.0")).await,
                ReadCheck::Verified(_)
            ),
            "{damage}"
        );
        let repaired = install(&engine, &release).await.must("repairs");
        assert_eq!(repaired.outcome, InstallOutcome::Repaired, "{damage}");
        assert!(
            matches!(
                engine.read_release(&key("report", "1.0.0")).await,
                ReadCheck::Verified(_)
            ),
            "{damage}"
        );
        assert_eq!(leftovers(&engine.io).len(), 0, "{damage}: {:?}", engine.io.paths());
        assert_eq!(ledger_of(&engine).await.entries.len(), 1);
    }
}

#[tokio::test]
async fn repairing_keeps_the_recorded_origin_and_time() {
    let engine = engine();
    let release = sample();
    let lock = engine.lock().await.must("lock");
    engine
        .install(&lock, release.request(ReleaseOrigin::Bundle))
        .await
        .must("installs");
    engine.clock.advance(100);
    engine.io.delete("report/1.0.0/main.rhai");
    engine
        .install(&lock, release.request(ReleaseOrigin::Source))
        .await
        .must("repairs");
    assert_eq!(
        ledger_of(&engine).await.entries[0].provenance,
        EntryProvenance::Installed {
            origin: ReleaseOrigin::Bundle,
            installed_at: START
        }
    );
}

#[tokio::test]
async fn a_release_that_cannot_be_verified_is_neither_repaired_nor_replaced() {
    let engine = engine();
    install(&engine, &sample()).await.must("installs");
    let before = engine.io.snapshot();
    engine.io.set_unavailable(true);
    let error = install(&engine, &sample()).await.must_fail("refused");
    assert_eq!(error.code(), "packages/store-unavailable");
    engine.io.set_unavailable(false);
    assert_eq!(engine.io.snapshot(), before);
}

#[tokio::test]
async fn a_busy_store_lock_is_reported_as_store_locked() {
    let engine = engine();
    engine.io.set_lock_busy(true);
    assert!(matches!(engine.lock().await, Err(PackagesError::StoreLocked)));
}

#[tokio::test]
async fn an_install_cleans_up_what_an_earlier_one_left() {
    let engine = engine();
    engine.io.put_file("_staging-old/half.txt", b"x");
    install(&engine, &sample()).await.must("installs");
    assert_eq!(leftovers(&engine.io).len(), 0);
}

#[tokio::test]
async fn a_failed_write_leaves_no_partial_release_and_no_staging_folder() {
    let total = {
        let engine = engine();
        install(&engine, &sample()).await.must("installs");
        engine.io.mutations()
    };
    for step in 0..total {
        let engine = engine();
        engine.io.fail_once(step, StoreFailureKind::Failed);
        let outcome = install(&engine, &sample()).await;
        assert!(outcome.is_err(), "step {step} should fail the install");
        assert_no_partial_release(&engine, &key("report", "1.0.0"), false).await;
        assert_eq!(leftovers(&engine.io).len(), 0, "step {step}: {:?}", engine.io.paths());
        let retried = install(&engine, &sample()).await.must("a retry succeeds");
        assert!(
            matches!(retried.outcome, InstallOutcome::Installed | InstallOutcome::Repaired),
            "step {step}"
        );
        assert!(
            matches!(
                engine.read_release(&key("report", "1.0.0")).await,
                ReadCheck::Verified(_)
            ),
            "step {step}"
        );
        assert_eq!(ledger_of(&engine).await.entries.len(), 1, "step {step}");
    }
}

#[tokio::test]
async fn a_crash_at_any_step_of_an_install_leaves_no_incomplete_release_visible() {
    let total = {
        let engine = engine();
        install(&engine, &sample()).await.must("installs");
        engine.io.mutations()
    };
    for step in 0..total {
        let engine = engine();
        engine.io.crash_at(step);
        assert!(install(&engine, &sample()).await.is_err(), "step {step}");
        engine.io.revive();
        assert_no_partial_release(&engine, &key("report", "1.0.0"), false).await;
        install(&engine, &sample())
            .await
            .must("the next operation recovers and installs");
        assert!(
            matches!(
                engine.read_release(&key("report", "1.0.0")).await,
                ReadCheck::Verified(_)
            ),
            "step {step}"
        );
        assert_eq!(leftovers(&engine.io).len(), 0, "step {step}: {:?}", engine.io.paths());
        assert_eq!(ledger_of(&engine).await.entries.len(), 1, "step {step}");
    }
}

#[tokio::test]
async fn a_crash_at_any_step_of_a_repair_leaves_a_whole_release_or_the_one_being_repaired() {
    let damage = |engine: &TestEngine| tamper(&engine.io, &sample(), "main.rhai");
    let total = {
        let engine = engine();
        install(&engine, &sample()).await.must("installs");
        damage(&engine);
        engine.io.reset_mutation_count();
        install(&engine, &sample()).await.must("repairs");
        engine.io.mutations()
    };
    for step in 0..total {
        let engine = engine();
        install(&engine, &sample()).await.must("installs");
        damage(&engine);
        engine.io.reset_mutation_count();
        engine.io.crash_at(step);
        // The last step deletes the old folder, a failure the install only logs.
        let outcome = install(&engine, &sample()).await;
        assert!(outcome.is_err() || step + 1 == total, "step {step}");
        engine.io.revive();
        assert_no_partial_release(&engine, &key("report", "1.0.0"), true).await;
        install(&engine, &sample())
            .await
            .must("the next operation recovers and repairs");
        assert!(
            matches!(
                engine.read_release(&key("report", "1.0.0")).await,
                ReadCheck::Verified(_)
            ),
            "step {step}"
        );
        assert_eq!(leftovers(&engine.io).len(), 0, "step {step}: {:?}", engine.io.paths());
    }
}

#[tokio::test]
async fn a_replacement_that_cannot_be_renamed_in_leaves_the_old_release_and_no_staging_folder() {
    let damage = |engine: &TestEngine| tamper(&engine.io, &sample(), "main.rhai");
    let total = {
        let engine = engine();
        install(&engine, &sample()).await.must("installs");
        damage(&engine);
        engine.io.reset_mutation_count();
        install(&engine, &sample()).await.must("repairs");
        engine.io.mutations()
    };
    let engine = engine();
    install(&engine, &sample()).await.must("installs");
    damage(&engine);
    let damaged = engine.io.file("report/1.0.0/main.rhai");
    engine.io.reset_mutation_count();
    // The mutations end with: rename the old folder aside, rename the replacement in, delete the aside folder.
    engine.io.fail_once(total - 2, StoreFailureKind::Failed);
    assert!(install(&engine, &sample()).await.is_err());
    assert_eq!(
        engine.io.file("report/1.0.0/main.rhai"),
        damaged,
        "the old folder is back under its name"
    );
    assert_eq!(leftovers(&engine.io).len(), 0, "{:?}", engine.io.paths());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn whatever_files_and_wherever_it_crashes_an_install_never_shows_a_partial_release(
        files in prop::collection::btree_map("[a-c]{1,2}(/[a-c]{1,2}){0,2}", prop::collection::vec(any::<u8>(), 0..8), 0..5),
        crash in 0usize..40,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().must("runtime");
        runtime.block_on(async {
            let mut release = Built::new("pkg", "1.2.3", &[]);
            for (path, bytes) in files {
                if crate::path::ReleaseFilePath::try_from(path.as_str()).is_ok() {
                    release.files.insert(path, bytes);
                }
            }
            // A generated path can be a folder of another generated file; such a set cannot be installed.
            let paths: Vec<_> = release.files.keys().cloned().collect();
            if paths.iter().any(|p| paths.iter().any(|q| q.starts_with(&format!("{p}/")))) {
                return;
            }
            let engine = engine_over(FakeIo::with_root());
            engine.io.crash_at(crash);
            let _ = install(&engine, &release).await;
            engine.io.revive();
            assert_no_partial_release(&engine, &key("pkg", "1.2.3"), false).await;
            install(&engine, &release).await.must("recovers");
            assert!(matches!(engine.read_release(&key("pkg", "1.2.3")).await, ReadCheck::Verified(_)));
            assert_eq!(leftovers(&engine.io).len(), 0);
        });
    }
}

#[tokio::test]
async fn the_removal_cause_of_an_unrecorded_entry_is_never_written() {
    let engine = engine();
    install(&engine, &sample()).await.must("installs");
    let written = String::from_utf8(engine.io.file("ledger.toml").must("ledger")).must("text");
    assert!(!written.contains("unrecorded"));
}

#[tokio::test]
async fn the_ledger_records_the_interface_the_model_reads_from_the_files() {
    let engine = engine();
    let release = Built::new("report", "1.0.0", &[("main.rhai", b"x"), ("note.txt", b"from a file")]);
    install(&engine, &release).await.must("installs");

    let ledger = ledger_of(&engine).await;
    assert_eq!(ledger.entries[0].interface.note.as_deref(), Some("from a file"));
}

#[tokio::test]
async fn a_release_the_model_refuses_for_its_files_installs_nothing() {
    let engine = engine();
    let release = Built::new("report", "1.0.0", &[("main.rhai", b"x"), ("forbidden.txt", b"no")]);

    let refused = install(&engine, &release).await;
    assert!(matches!(refused, Err(PackagesError::InvalidManifest(_))), "{refused:?}");
    let Err(PackagesError::InvalidManifest(rejection)) = refused else {
        return;
    };

    assert!(rejection.message.contains("forbidden.txt"), "{}", rejection.message);
    assert_no_partial_release(&engine, &key("report", "1.0.0"), false).await;
}

#[tokio::test]
async fn the_model_is_told_which_store_the_release_goes_into() {
    let release = Built::new("report", "1.0.0", &[("main.rhai", b"x"), ("vault-refused.txt", b"no")]);

    let host = engine();
    install(&host, &release).await.must("a host store accepts it");

    let vault = engine();
    vault.io.state().home = crate::io::StoreHome::Vault;
    let refused = install(&vault, &release).await;
    assert!(matches!(refused, Err(PackagesError::InvalidManifest(_))), "{refused:?}");
    let Err(PackagesError::InvalidManifest(rejection)) = refused else {
        return;
    };
    assert_eq!(rejection.code, "packages/test-vault-refusal");
}
