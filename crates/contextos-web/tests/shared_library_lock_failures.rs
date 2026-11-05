//! A first validation of a shared job that cannot resolve its libraries fixes no library lock: the newest matching
//! library release has had its content changed, so it no longer reads whole, and no older release stands in.

mod support;

use contextos_workflows::JobState;
use semver::Version;
use support::BoxError;
use support::shared::{FIRST, SharedFixture as Fixture, write};

const READY: &str = "name = \"Nightly\"\nready = true\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
const SCRIPT: &str = "import \"acme-text:strings\" as s;\ns::shout();";

#[tokio::test]
async fn a_first_validation_whose_newest_library_release_is_damaged_fails_and_fixes_no_lock() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.install_library("acme-text", "1.0.0", "strings", "fn shout() { 1 }")?;
    fixture.install_library("acme-text", "1.1.0", "strings", "fn shout() { 2 }")?;
    fixture.install_depending_on("1.0.0", SCRIPT, &[("acme-text", "^1")])?;
    write(
        &fixture.store_root().join("acme-text/1.1.0/strings.rhai"),
        "fn shout() { 3 }",
    )?;

    fixture.pass(FIRST).await?;

    assert_eq!(fixture.lifecycle()?, "draft");
    let state = fixture.read()?;
    let stored = state
        .job("acme", "sync", "nightly")
        .and_then(JobState::validation)
        .ok_or("the failing result was not kept")?;
    assert!(
        stored
            .failures
            .iter()
            .any(|failure| failure.code == "validation/artefact-invalid"),
        "{stored:?}"
    );
    assert!(
        state.library_lock("acme", "sync", &Version::new(1, 0, 0)).is_none(),
        "no lock is recorded and 1.0.0 does not stand in for the damaged 1.1.0"
    );
    Ok(())
}
