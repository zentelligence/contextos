use std::fs;

use tempfile::tempdir;

use super::*;

const ONE_SERVER: &str = "[[mcp_server]]\ntransport = \"stdio\"\nname = \"contextos\"\ncommand = \"contextos-mcp\"\n";

fn load(extra: &str) -> Result<WebConfig, WebConfigError> {
    let dir = tempdir().map_err(|source| WebConfigError::Read {
        path: std::path::PathBuf::new(),
        source,
    })?;
    let path = dir.path().join("web.toml");
    fs::write(&path, format!("{extra}\n{ONE_SERVER}")).map_err(|source| WebConfigError::Read {
        path: path.clone(),
        source,
    })?;
    load_web_config(&path)
}

#[test]
fn without_a_workflows_table_no_host_log_level_is_set() -> Result<(), WebConfigError> {
    assert_eq!(load("")?.workflows.log_level, None);
    Ok(())
}

#[test]
fn the_workflows_table_sets_the_host_default_log_level() -> Result<(), WebConfigError> {
    let config = load("[workflows]\nlog_level = \"warn\"\n")?;

    assert_eq!(config.workflows.log_level, Some(contextos_workflows::LogLevel::Warn));
    Ok(())
}

#[test]
fn an_unknown_level_or_key_in_the_workflows_table_is_rejected() {
    assert!(matches!(
        load("[workflows]\nlog_level = \"loud\"\n"),
        Err(WebConfigError::Toml { .. })
    ));
    assert!(matches!(
        load("[workflows]\nverbosity = 3\n"),
        Err(WebConfigError::Toml { .. })
    ));
}

#[test]
fn run_records_are_kept_thirty_days_unless_the_workflows_table_says_otherwise() -> Result<(), WebConfigError> {
    assert_eq!(load("")?.workflows.run_retention(), time::Duration::days(30));
    assert_eq!(
        load("[workflows]\nrun_retention_days = 7\n")?.workflows.run_retention(),
        time::Duration::days(7)
    );
    Ok(())
}

#[test]
fn a_retention_of_no_days_or_a_negative_number_is_rejected_not_read_as_keep_nothing() {
    assert!(matches!(
        load("[workflows]\nrun_retention_days = 0\n"),
        Err(WebConfigError::ZeroRunRetention)
    ));
    assert!(matches!(
        load("[workflows]\nrun_retention_days = -3\n"),
        Err(WebConfigError::Toml { .. })
    ));
}
