use crate::PackagesError;
use crate::ledger::ReleaseOrigin;
use crate::ledger::RequestedRemoval;
use crate::store::PackageStore;
use crate::test_support::Must;
use crate::test_support::{Built, MustFail, TestEngine, engine, key, ledger_of};

const STEP_CODE: &str = "packages/version-step-invalid";

fn release(version: &str, inputs: &[&str]) -> Built {
    let mut built = Built::new("report", version, &[("main.rhai", version.as_bytes())]);
    built.inputs = inputs.iter().map(|input| (*input).to_owned()).collect();
    built
}

async fn install(engine: &TestEngine, built: &Built) -> Result<crate::store::Installed, PackagesError> {
    let lock = engine.lock().await.must("lock");
    engine.install(&lock, built.request(ReleaseOrigin::Bundle)).await
}

#[tokio::test]
async fn an_input_removed_at_a_patch_step_is_refused_naming_the_change() {
    let engine = engine();
    install(&engine, &release("1.0.0", &["day", "week"]))
        .await
        .must("first");

    let error = install(&engine, &release("1.0.1", &["day"]))
        .await
        .must_fail("a major change at a patch step");

    assert_eq!(error.code(), STEP_CODE);
    let message = error.to_string();
    assert!(message.contains("week"), "the change is named: {message}");
    assert!(message.contains("1.0.0"), "the version it follows is named: {message}");
    assert_eq!(ledger_of(&engine).await.entries.len(), 1);
    assert!(!engine.io.exists("report/1.0.1"));
}

#[tokio::test]
async fn an_input_added_at_a_patch_step_is_refused() {
    let engine = engine();
    install(&engine, &release("1.0.0", &["day"])).await.must("first");

    let error = install(&engine, &release("1.0.1", &["day", "week"]))
        .await
        .must_fail("a minor change at a patch step");

    assert_eq!(error.code(), STEP_CODE);
}

#[tokio::test]
async fn a_major_change_at_a_minor_step_is_refused() {
    let engine = engine();
    install(&engine, &release("1.0.0", &["day"])).await.must("first");

    let error = install(&engine, &release("1.1.0", &[]))
        .await
        .must_fail("a major change at a minor step");

    assert_eq!(error.code(), STEP_CODE);
}

#[tokio::test]
async fn the_step_the_change_requires_and_a_larger_one_are_accepted() {
    let engine = engine();
    install(&engine, &release("1.0.0", &["day"])).await.must("first");
    install(&engine, &release("1.1.0", &["day", "week"]))
        .await
        .must("exact minor step");
    install(&engine, &release("1.1.1", &["day", "week"]))
        .await
        .must("exact patch step");
    install(&engine, &release("3.0.0", &["day", "week"]))
        .await
        .must("a larger step than the change needs");

    assert_eq!(ledger_of(&engine).await.entries.len(), 4);
}

#[tokio::test]
async fn the_step_is_judged_against_a_highest_release_that_has_been_removed() {
    let engine = engine();
    install(&engine, &release("1.0.0", &["day"])).await.must("first");
    install(&engine, &release("1.1.0", &["day", "week"]))
        .await
        .must("second");
    {
        let lock = engine.lock().await.must("lock");
        engine
            .remove(&lock, &key("report", "1.1.0"), RequestedRemoval::ByRequest)
            .await
            .must("removes the highest");
    }

    let error = install(&engine, &release("1.1.1", &["day"]))
        .await
        .must_fail("removing week from the removed highest release is major");

    assert_eq!(error.code(), STEP_CODE);
    assert!(error.to_string().contains("week"));
}

#[tokio::test]
async fn the_first_release_of_a_package_is_accepted_at_any_version() {
    let engine = engine();
    install(&engine, &release("3.2.1", &["day"]))
        .await
        .must("first release");
}

#[tokio::test]
async fn a_release_older_than_the_highest_is_accepted_when_its_interface_matches_the_release_it_follows() {
    let engine = engine();
    install(&engine, &release("1.0.0", &["day"])).await.must("first");
    install(&engine, &release("2.0.0", &["day"])).await.must("second");

    install(&engine, &release("1.5.0", &["day"]))
        .await
        .must("a back-filled release");
}

#[tokio::test]
async fn a_release_older_than_the_highest_is_judged_against_the_release_it_follows() {
    let engine = engine();
    install(&engine, &release("1.0.0", &["day"])).await.must("first");
    install(&engine, &release("3.0.0", &["day", "week"]))
        .await
        .must("second");

    let error = install(&engine, &release("1.0.1", &["day", "week"]))
        .await
        .must_fail("a minor change at a patch step from 1.0.0");

    assert_eq!(error.code(), STEP_CODE);
    assert!(error.to_string().contains("1.0.0"), "{error}");
}

#[tokio::test]
async fn another_package_is_judged_only_against_its_own_releases() {
    let engine = engine();
    install(&engine, &release("2.0.0", &["day"])).await.must("report");
    let mut other = Built::new("other", "1.0.0", &[("main.rhai", b"x")]);
    other.inputs = vec!["a".to_owned()];

    install(&engine, &other)
        .await
        .must("a different package starts its own line");
}

#[tokio::test]
async fn a_back_filled_release_is_refused_when_the_release_above_it_would_step_too_little() {
    let engine = engine();
    install(&engine, &release("1.0.0", &["day"])).await.must("first");
    install(&engine, &release("1.2.0", &["day"])).await.must("above");

    let error = install(&engine, &release("1.1.0", &["day", "week"]))
        .await
        .must_fail("1.1.0 to 1.2.0 would remove week at a minor step");

    assert_eq!(error.code(), STEP_CODE);
    let message = error.to_string();
    assert!(message.contains("week"), "the change is named: {message}");
    assert!(
        message.contains("1.2.0") && message.contains("1.1.0"),
        "both versions are named: {message}"
    );
    assert!(!engine.io.exists("report/1.1.0"));
    assert_eq!(ledger_of(&engine).await.entries.len(), 2);
}

#[tokio::test]
async fn a_back_filled_release_that_steps_correctly_to_both_neighbours_is_accepted() {
    let engine = engine();
    install(&engine, &release("1.0.0", &["day"])).await.must("first");
    install(&engine, &release("2.0.0", &["day", "week"]))
        .await
        .must("above");

    install(&engine, &release("1.1.0", &["day", "week"]))
        .await
        .must("minor above 1.0.0, then a major step to 2.0.0");
}

#[tokio::test]
async fn a_removed_release_above_still_constrains_a_back_filled_release() {
    let engine = engine();
    install(&engine, &release("1.0.0", &["day"])).await.must("first");
    install(&engine, &release("1.2.0", &["day"])).await.must("above");
    {
        let lock = engine.lock().await.must("lock");
        engine
            .remove(&lock, &key("report", "1.2.0"), RequestedRemoval::ByRequest)
            .await
            .must("removes the one above");
    }

    let error = install(&engine, &release("1.1.0", &["day", "week"]))
        .await
        .must_fail("the removed release above is still the ledger's neighbour");

    assert_eq!(error.code(), STEP_CODE);
}
