//! Retention after the automatic validation pass: a pass that validated a job can have fixed its release's library lock,
//! so it removes the releases nothing keeps any longer, for the host's store and a vault's.

mod support;

use contextos_web::vault_source_install::SourceInstall;
use contextos_web::vault_source_read::{SourceKind, SourceTarget};
use contextos_workflows::PackageName;
use support::BoxError;
use support::jobs::{JobVault, Spec, VAULT_NAME};
use support::shared::{DIGEST, SharedFixture};
use time::macros::datetime;

const READY: &str = "ready = true\nname = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n";
const NOW: time::OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);

fn releases(root: &std::path::Path, package: &str) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(root.join(package))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

#[tokio::test]
async fn a_pass_that_validated_a_shared_job_removes_the_releases_nothing_keeps() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY).await?;
    fixture.install("1.0.1", DIGEST, "let a = 1;")?;
    fixture.install("1.0.2", DIGEST, "let a = 2;")?;
    contextos_web::package_store::rebuild_ledger(&fixture.store_root())?;

    let report = fixture.pass(NOW).await?;

    assert_eq!(report.validated, 1, "{report:?}");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    // The job names 1.0.0, and 1.0.2 is the newest future release.
    assert_eq!(releases(&fixture.store_root(), "acme"), ["1.0.0", "1.0.2"]);
    Ok(())
}

#[tokio::test]
async fn a_pass_that_validated_nothing_removes_nothing() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(&READY.replace("ready = true\n", "")).await?;
    fixture.install("1.0.1", DIGEST, "let a = 1;")?;
    fixture.install("1.0.2", DIGEST, "let a = 2;")?;
    contextos_web::package_store::rebuild_ledger(&fixture.store_root())?;

    let report = fixture.pass(NOW).await?;

    assert_eq!(report.validated, 0);
    assert_eq!(releases(&fixture.store_root(), "acme"), ["1.0.0", "1.0.1", "1.0.2"]);
    Ok(())
}

#[tokio::test]
async fn a_pass_that_validated_a_shared_job_in_a_store_with_no_ledger_reports_the_warning() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY).await?;
    fixture.install("1.0.1", DIGEST, "let a = 1;")?;

    let report = fixture.pass(NOW).await?;

    assert_eq!(report.validated, 1, "{report:?}");
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert_eq!(releases(&fixture.store_root(), "acme"), ["1.0.0", "1.0.1"]);
    Ok(())
}

async fn install_source(vault: &JobVault) -> Result<(), BoxError> {
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    SourceInstall {
        client: &client,
        vault: VAULT_NAME,
    }
    .install(
        &SourceTarget {
            kind: SourceKind::Workflows,
            name: PackageName::try_from("ops")?,
        },
        None,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn a_pass_that_validated_a_vault_job_removes_the_releases_nothing_keeps() -> Result<(), BoxError> {
    let vault = JobVault::open(&Spec::default()).await?;
    std::fs::remove_dir_all(vault.root().join(".contextos/packages"))?;
    install_source(&vault).await?;
    let holder = "registry/workflows/ops/sync/jobs/holder.toml";
    for (script, naming) in [("log_info(\"b\");", "1.0.0"), ("log_info(\"c\");", "1.0.1")] {
        vault.write(holder, &support::jobs::JOB.replace("1.0.0", naming))?;
        vault.write("registry/workflows/ops/sync/sync.rhai", script)?;
        install_source(&vault).await?;
    }
    std::fs::remove_file(vault.root().join(holder))?;
    let client = vault.clients.get("contextos").ok_or("no primary server")?;

    let report =
        contextos_web::job_validation::validation_pass(&client, VAULT_NAME, &["contextos".to_owned()], NOW).await?;

    assert_eq!(report.validated, 1, "{report:?}");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(
        releases(&vault.root().join(".contextos/packages"), "ops"),
        ["1.0.0", "1.0.2"]
    );
    Ok(())
}
