use super::*;
use crate::io::StoreFailureKind;
use crate::ledger::ReleaseOrigin;
use crate::store::PackageStore;
use crate::test_support::{Built, FakeIo, Node, engine, engine_over, key};
use crate::test_support::{Must, MustFail};

#[tokio::test]
async fn a_store_whose_root_does_not_exist_lists_nothing() {
    let engine = engine_over(FakeIo::default());
    assert_eq!(engine.list().await.must("lists"), crate::store::StoreListing::default());
}

#[tokio::test]
async fn releases_staging_and_removal_folders_are_listed_by_name_in_order() {
    let engine = engine();
    for (name, version) in [
        ("beta", "1.0.0"),
        ("alpha", "2.0.0"),
        ("alpha", "1.10.0"),
        ("alpha", "1.9.0"),
    ] {
        engine.io.put_dir(&format!("{name}/{version}"));
    }
    engine.io.put_dir("_staging-b2");
    engine.io.put_dir("_staging-a1");
    engine.io.put_dir("_removing-c3");
    let listing = engine.list().await.must("lists");
    assert_eq!(
        listing.releases,
        [
            key("alpha", "1.9.0"),
            key("alpha", "1.10.0"),
            key("alpha", "2.0.0"),
            key("beta", "1.0.0")
        ]
    );
    assert_eq!(
        listing
            .staging
            .iter()
            .map(crate::name::OperationId::as_str)
            .collect::<Vec<_>>(),
        ["a1", "b2"]
    );
    assert_eq!(
        listing
            .removing
            .iter()
            .map(crate::name::OperationId::as_str)
            .collect::<Vec<_>>(),
        ["c3"]
    );
    assert_eq!(listing.unrecognised.len(), 0);
}

#[tokio::test]
async fn the_ledger_is_not_a_release_and_is_not_reported() {
    let engine = engine();
    let lock = engine.lock().await.must("lock");
    engine
        .install(&lock, Built::new("report", "1.0.0", &[]).request(ReleaseOrigin::Source))
        .await
        .must("installs");
    let listing = engine.list().await.must("lists");
    assert_eq!(listing.releases, [key("report", "1.0.0")]);
    assert_eq!(listing.unrecognised.len(), 0);
}

#[tokio::test]
async fn the_stores_own_lock_files_are_part_of_the_store_and_not_reported_as_strays() {
    let engine = engine();
    engine.io.put_file(".lock", b"");
    engine.io.put_dir(".lock.queue");
    engine.io.put_file(".lock.queue.lock", b"");
    engine.io.put_dir("report/1.0.0");
    let listing = engine.list().await.must("lists");
    assert_eq!(listing.releases, [key("report", "1.0.0")]);
    assert_eq!(listing.unrecognised.len(), 0);
}

#[tokio::test]
async fn a_lock_name_that_is_not_the_stores_lock_kind_is_still_reported_as_found() {
    let engine = engine();
    engine.io.put_dir(".lock");
    engine.io.put_file(".lock.queue", b"x");
    engine.io.put_file(".lock.other", b"x");
    let listing = engine.list().await.must("lists");
    assert_eq!(listing.unrecognised, [".lock", ".lock.other", ".lock.queue"]);
}

#[tokio::test]
async fn other_dot_prefixed_entries_are_never_releases() {
    let engine = engine();
    engine.io.put_dir(".hidden/1.0.0");
    engine.io.put_dir("report/.hidden");
    engine.io.put_dir("report/1.0.0");
    let listing = engine.list().await.must("lists");
    assert_eq!(listing.releases, [key("report", "1.0.0")]);
    assert_eq!(listing.unrecognised, [".hidden", "report/.hidden"]);
}

#[tokio::test]
async fn anything_that_does_not_read_as_a_release_is_reported_as_found_and_not_read() {
    let engine = engine();
    engine.io.put_dir("report/1.0.0");
    engine.io.put_dir("report/1.0.0-rc.1");
    engine.io.put_dir("report/01.0.0");
    engine.io.put_dir("report/1.0");
    engine.io.put_file("report/1.1.0", b"a file, not a folder");
    engine.io.put_node("report/1.2.0", Node::Link);
    engine.io.put_file("notes.txt", b"x");
    engine.io.put_file("Upper/1.0.0/x", b"x");
    engine.io.put_file("plainfile", b"x");
    engine.io.put_dir("_staging-UPPER");
    let listing = engine.list().await.must("lists");
    assert_eq!(listing.releases, [key("report", "1.0.0")]);
    assert_eq!(
        listing.unrecognised,
        [
            "Upper",
            "_staging-UPPER",
            "notes.txt",
            "plainfile",
            "report/01.0.0",
            "report/1.0",
            "report/1.0.0-rc.1",
            "report/1.1.0",
            "report/1.2.0",
        ]
    );
}

#[tokio::test]
async fn an_unreachable_store_is_an_error_not_an_empty_listing() {
    let engine = engine();
    engine.io.set_unavailable(true);
    let error = engine.list().await.must_fail("fails");
    assert!(matches!(&error, PackagesError::Store(failure) if failure.kind == StoreFailureKind::Unavailable));
}
