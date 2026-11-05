use super::*;

fn plan(timeout_secs: u64, level: LogLevel) -> RunPlan {
    RunPlan {
        timeout_secs,
        log_level: level,
        label: RunLabel {
            workflow: "sync".to_owned(),
            job: Some("nightly".to_owned()),
            run_id: Some(7),
        },
        cancel: CancellationToken::new(),
    }
}

#[test]
fn a_plan_becomes_settings_with_its_timeout_and_level() {
    let settings = RunSettings::from(plan(90, LogLevel::Warn));

    assert_eq!(settings.timeout, time::Duration::seconds(90));
    assert_eq!(settings.log_level, LogLevel::Warn);
}

#[test]
fn a_timeout_too_large_for_the_clock_is_capped_not_wrapped() {
    let settings = RunSettings::from(plan(u64::MAX, LogLevel::Info));

    assert_eq!(settings.timeout, time::Duration::seconds(i64::MAX));
}

#[test]
fn the_level_is_the_jobs_then_the_workflows_then_the_hosts() {
    let host = HostContext {
        attention: crate::job_attention::JobAttentionMemory::default(),
        log_level: Some(LogLevel::Warn),
        claims_root: None,
        clock: std::sync::Arc::new(crate::vault_time::VaultTime::from(crate::vault_time::VaultTimeInput {
            clock: std::sync::Arc::new(contextos_workflows::SystemClock),
            clients: std::sync::Arc::new(crate::McpClientSet::default()),
        })),
    };

    assert_eq!(
        host.level_for(Some(LogLevel::Debug), Some(LogLevel::Error)),
        LogLevel::Error
    );
    assert_eq!(host.level_for(Some(LogLevel::Debug), None), LogLevel::Debug);
    assert_eq!(host.level_for(None, None), LogLevel::Warn);
}

#[test]
fn with_nothing_set_anywhere_the_level_is_info() {
    let host = HostContext {
        attention: crate::job_attention::JobAttentionMemory::default(),
        log_level: None,
        claims_root: None,
        clock: std::sync::Arc::new(crate::vault_time::VaultTime::from(crate::vault_time::VaultTimeInput {
            clock: std::sync::Arc::new(contextos_workflows::SystemClock),
            clients: std::sync::Arc::new(crate::McpClientSet::default()),
        })),
    };

    assert_eq!(host.level_for(None, None), LogLevel::Info);
}
