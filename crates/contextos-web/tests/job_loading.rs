//! How a vault job file loads through `contextos_web::discover_jobs` over a real vault: a job states its own timeout
//! and never takes its workflow's, a missing owner does not stop a job loading, and an owner who is not a member, or is
//! not even slug-shaped, does not stop it either, so a page and a dry run can still see it.

mod support;

use contextos_core::{DeclaredOwner, OwnerStanding};
use contextos_web::discover_jobs;
use support::BoxError;
use support::jobs::{JobVault, Spec, TEAM, VAULT_NAME, roster_of};

const WORKFLOW_WITH_TIMEOUT: &str =
    "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\ntimeout_secs = 120\n";

async fn discovered(vault: &JobVault) -> Result<Vec<contextos_web::RegisteredJob>, BoxError> {
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    Ok(discover_jobs(&client, VAULT_NAME, &["contextos".to_owned()]).await?)
}

#[tokio::test]
async fn a_job_never_takes_its_workflows_timeout() -> Result<(), BoxError> {
    let without = Spec {
        workflow: WORKFLOW_WITH_TIMEOUT,
        job: "name = \"Nightly\"\nowner = \"vela\"\n",
        ..Spec::default()
    };
    let vault = JobVault::open(&without).await?;
    assert!(
        discovered(&vault).await?.is_empty(),
        "a job file with no timeout_secs does not load, whatever its workflow says"
    );

    let with = Spec {
        workflow: WORKFLOW_WITH_TIMEOUT,
        job: "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 45\nversion = \"1.0.0\"\n",
        ..Spec::default()
    };
    let vault = JobVault::open(&with).await?;
    let jobs = discovered(&vault).await?;
    assert_eq!(jobs.iter().map(|job| job.timeout_secs).collect::<Vec<_>>(), vec![45]);
    Ok(())
}

#[tokio::test]
async fn a_job_file_without_an_owner_loads_and_its_owner_is_undeclared() -> Result<(), BoxError> {
    let spec = Spec {
        job: "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        ..Spec::default()
    };
    let vault = JobVault::open(&spec).await?;

    let jobs = discovered(&vault).await?;

    let [job] = jobs.as_slice() else {
        return Err(format!("expected the job to load, found {} jobs", jobs.len()).into());
    };
    assert_eq!(job.declared_owner, None);
    let roster = roster_of(TEAM)?;
    assert_eq!(job.owner_standing(&roster), OwnerStanding::Undeclared);
    assert!(
        TEAM.contains("vela"),
        "the team is valid, so only the missing owner is at issue"
    );
    Ok(())
}

#[tokio::test]
async fn a_job_whose_owner_is_not_a_member_is_still_discovered() -> Result<(), BoxError> {
    let spec = Spec {
        job: "name = \"Nightly\"\nowner = \"ghost\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        ..Spec::default()
    };
    let vault = JobVault::open(&spec).await?;

    let jobs = discovered(&vault).await?;

    let [job] = jobs.as_slice() else {
        return Err(format!("expected the job to be discovered, found {} jobs", jobs.len()).into());
    };
    assert_eq!(job.declared_owner.as_ref().map(DeclaredOwner::as_str), Some("ghost"));
    let roster = roster_of(TEAM)?;
    assert!(
        matches!(job.owner_standing(&roster), OwnerStanding::NotAMember(ref slug) if slug.as_str() == "ghost"),
        "the owner is declared and no member"
    );
    Ok(())
}

#[tokio::test]
async fn a_job_whose_owner_is_not_slug_shaped_is_still_discovered_with_the_text_it_declared() -> Result<(), BoxError> {
    let spec = Spec {
        job: "name = \"Nightly\"\nowner = \"Vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        ..Spec::default()
    };
    let vault = JobVault::open(&spec).await?;

    let jobs = discovered(&vault).await?;

    let [job] = jobs.as_slice() else {
        return Err(format!("expected the job to be discovered, found {} jobs", jobs.len()).into());
    };
    assert_eq!(
        job.declared_owner,
        Some(DeclaredOwner::Invalid("Vela".to_owned())),
        "the declared text is kept as written, not lowercased into the member `vela`"
    );
    let roster = roster_of(TEAM)?;
    assert_eq!(
        job.owner_standing(&roster),
        OwnerStanding::InvalidSlug("Vela".to_owned())
    );
    Ok(())
}

fn job_with_version(version: &str) -> String {
    format!("name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = {version:?}\n")
}

#[tokio::test]
async fn a_job_file_without_a_version_does_not_load() -> Result<(), BoxError> {
    let spec = Spec {
        job: "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\n",
        ..Spec::default()
    };
    let vault = JobVault::open(&spec).await?;

    assert!(discovered(&vault).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn a_job_file_whose_version_is_not_an_exact_release_version_does_not_load() -> Result<(), BoxError> {
    for version in ["^1.0.0", "1.0", "1.0.0+build.1", "1.0.0-rc.1", "latest"] {
        let job = job_with_version(version);
        let spec = Spec {
            job: &job,
            ..Spec::default()
        };
        let vault = JobVault::open(&spec).await?;

        assert!(discovered(&vault).await?.is_empty(), "{version}");
    }
    Ok(())
}

#[tokio::test]
async fn a_loaded_job_carries_the_release_and_predecessor_its_file_names() -> Result<(), BoxError> {
    let job = format!("{}predecessor = \"nightly-v1\"\n", job_with_version("1.2.0"));
    let spec = Spec {
        job: &job,
        ..Spec::default()
    };
    let vault = JobVault::open(&spec).await?;

    let jobs = discovered(&vault).await?;

    let [loaded] = jobs.as_slice() else {
        return Err(format!("expected the job to load, found {} jobs", jobs.len()).into());
    };
    assert_eq!(loaded.release.version.as_semver().to_string(), "1.2.0");
    assert_eq!(
        loaded
            .release
            .predecessor
            .as_ref()
            .map(contextos_workflows::PredecessorKey::as_str),
        Some("nightly-v1")
    );
    Ok(())
}
