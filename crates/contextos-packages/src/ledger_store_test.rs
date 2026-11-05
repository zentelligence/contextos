use super::*;
use crate::ledger::{LedgerState, ReleaseOrigin, RequestedRemoval};
use crate::store::PackageStore;
use crate::test_support::{Built, FakeIo, engine, engine_over, key};
use crate::test_support::{Must, MustFail};

fn release(version: &str) -> Built {
    Built::new("report", version, &[("main.rhai", version.as_bytes())])
}

#[tokio::test]
async fn a_new_store_has_no_ledger_yet() {
    for engine in [engine_over(FakeIo::default()), engine()] {
        let reading = engine.ledger().await.must("reads");
        assert_eq!(reading.state, LedgerState::Absent);
        assert_eq!(reading.unrecorded_removals.len(), 0);
    }
}

#[tokio::test]
async fn a_store_with_releases_and_no_ledger_is_missing_its_ledger() {
    let engine = engine();
    release("1.0.0").put_into(&engine.io);
    assert_eq!(
        engine.ledger().await.must("reads").state,
        LedgerState::Unusable(LedgerProblem::Missing)
    );
}

#[tokio::test]
async fn a_ledger_that_cannot_be_used_is_a_state_not_an_error() {
    let engine = engine();
    let lock = engine.lock().await.must("lock");
    engine
        .install(&lock, release("1.0.0").request(ReleaseOrigin::Source))
        .await
        .must("installs");
    for text in [&b"["[..], &[0xff, 0xfe][..], b"schema = 2\n"] {
        engine.io.put_file("ledger.toml", text);
        assert!(matches!(
            engine.ledger().await.must("reads").state,
            LedgerState::Unusable(LedgerProblem::Unreadable(_))
        ));
    }
    engine.io.put_file("ledger.toml", &vec![b'#'; 17 * 1024 * 1024]);
    assert!(matches!(
        engine.ledger().await.must("reads").state,
        LedgerState::Unusable(LedgerProblem::Unreadable(_))
    ));
    engine.io.delete("ledger.toml");
    engine.io.put_dir("ledger.toml");
    // A folder where the ledger belongs is not a ledger.
    assert!(matches!(
        engine.ledger().await.must("reads").state,
        LedgerState::Unusable(LedgerProblem::Unreadable(_))
    ));
}

#[tokio::test]
async fn an_unreachable_store_is_an_error() {
    let engine = engine();
    engine.io.set_unavailable(true);
    assert_eq!(
        engine.ledger().await.must_fail("fails").code(),
        "packages/store-unavailable"
    );
}

#[tokio::test]
async fn an_entry_with_no_folder_and_no_removal_is_reported_unrecorded_and_a_removal_is_not() {
    let engine = engine();
    let lock = engine.lock().await.must("lock");
    for version in ["1.0.0", "1.1.0", "1.2.0"] {
        engine
            .install(&lock, release(version).request(ReleaseOrigin::Source))
            .await
            .must("installs");
    }
    engine
        .remove(&lock, &key("report", "1.0.0"), RequestedRemoval::ByRequest)
        .await
        .must("removes");
    engine.io.delete("report/1.1.0");
    let reading = engine.ledger().await.must("reads");
    assert_eq!(reading.unrecorded_removals, [release("1.1.0").id()]);
    assert!(matches!(reading.state, LedgerState::Present(_)));
}
