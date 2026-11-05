use std::path::PathBuf;

use time::macros::datetime;

use super::*;

fn host(claims_root: Option<&str>) -> HostContext {
    HostContext {
        attention: crate::job_attention::JobAttentionMemory::default(),
        log_level: None,
        claims_root: claims_root.map(PathBuf::from),
        clock: std::sync::Arc::new(crate::vault_time::VaultTime::from(crate::vault_time::VaultTimeInput {
            clock: std::sync::Arc::new(contextos_workflows::SystemClock),
            clients: std::sync::Arc::new(crate::McpClientSet::default()),
        })),
    }
}

#[test]
fn a_manual_only_job_never_needs_daily_partitioning() {
    assert!(!daily_partition_for(
        None,
        datetime!(2026-09-08 00:00:00 UTC),
        &contextos_core::OperatorTimeZone::utc()
    ));
}

#[test]
fn a_high_frequency_scheduled_job_needs_daily_partitioning() {
    assert!(daily_partition_for(
        Some("* * * * *"),
        datetime!(2026-09-08 00:00:00 UTC),
        &contextos_core::OperatorTimeZone::utc()
    ));
}

#[test]
fn a_low_frequency_scheduled_job_does_not_need_daily_partitioning() {
    assert!(!daily_partition_for(
        Some("0 3 * * *"),
        datetime!(2026-09-08 00:00:00 UTC),
        &contextos_core::OperatorTimeZone::utc()
    ));
}

#[test]
fn an_unparsable_schedule_does_not_need_daily_partitioning() {
    assert!(!daily_partition_for(
        Some("whenever"),
        datetime!(2026-09-08 00:00:00 UTC),
        &contextos_core::OperatorTimeZone::utc()
    ));
}

#[test]
fn a_vault_jobs_claim_is_kept_on_the_host_beside_the_shared_ones() -> Result<(), ClaimRunError> {
    let slot = claim_slot(&host(Some("/data/claims")), "work", "ops", "sync", "nightly")?;

    assert_eq!(
        slot.lock_path(),
        PathBuf::from("/data/claims/vaults/work/ops/sync/nightly.lock")
    );
    Ok(())
}

#[test]
fn the_same_workflow_and_job_in_two_packages_have_separate_claims() -> Result<(), ClaimRunError> {
    let host = host(Some("/data/claims"));

    assert_ne!(
        claim_slot(&host, "work", "ops", "sync", "nightly")?,
        claim_slot(&host, "work", "crm", "sync", "nightly")?
    );
    Ok(())
}

#[test]
fn every_spelling_of_a_vault_name_shares_one_claim() -> Result<(), ClaimRunError> {
    let host = host(Some("/data/claims"));

    assert_eq!(
        claim_slot(&host, "Work", "ops", "sync", "nightly")?,
        claim_slot(&host, "work", "ops", "sync", "nightly")?
    );
    Ok(())
}

#[test]
fn a_host_with_no_claims_directory_cannot_claim_a_job() {
    assert!(matches!(
        claim_slot(&host(None), "work", "ops", "sync", "nightly"),
        Err(ClaimRunError::HostUnavailable)
    ));
}

#[test]
fn a_name_that_could_leave_the_claims_directory_cannot_claim_a_job() {
    let host = host(Some("/data/claims"));

    for (vault, package, workflow, job) in [
        ("..", "ops", "sync", "nightly"),
        ("work", "ops", "a/b", "nightly"),
        ("work", "ops", "sync", "..\\up"),
        ("work", "ops", "sync", "c:stream"),
        ("", "ops", "sync", "nightly"),
        ("work", "..", "sync", "nightly"),
    ] {
        assert!(
            matches!(
                claim_slot(&host, vault, package, workflow, job),
                Err(ClaimRunError::HostUnavailable)
            ),
            "{vault:?} {workflow:?} {job:?}"
        );
    }
}
