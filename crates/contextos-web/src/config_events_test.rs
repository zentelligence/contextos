use std::fs;
use std::time::Duration;

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
fn events_are_polled_every_five_seconds_unless_the_events_table_says_otherwise() -> Result<(), WebConfigError> {
    assert_eq!(load("")?.events.poll_interval(), Duration::from_secs(5));
    assert_eq!(
        load("[events]\npoll_seconds = 1\n")?.events.poll_interval(),
        Duration::from_secs(1)
    );
    assert_eq!(
        load("[events]\npoll_seconds = 30\n")?.events.poll_interval(),
        Duration::from_secs(30)
    );
    Ok(())
}

#[test]
fn a_poll_interval_under_one_second_is_rejected() {
    assert!(matches!(
        load("[events]\npoll_seconds = 0\n"),
        Err(WebConfigError::InvalidPollSeconds)
    ));
}

#[test]
fn an_unknown_or_mistyped_events_key_is_rejected() {
    assert!(matches!(load("[events]\npoll = 5\n"), Err(WebConfigError::Toml { .. })));
    assert!(matches!(
        load("[events]\npoll_seconds = \"5\"\n"),
        Err(WebConfigError::Toml { .. })
    ));
    assert!(matches!(
        load("[events]\npoll_seconds = -1\n"),
        Err(WebConfigError::Toml { .. })
    ));
}

#[test]
fn a_run_is_overdue_after_two_hours_unless_the_agents_table_says_otherwise() -> Result<(), WebConfigError> {
    assert_eq!(load("")?.agents.run_overdue(), time::Duration::minutes(120));
    assert_eq!(
        load("[agents]\nrun_overdue_minutes = 45\n")?.agents.run_overdue(),
        time::Duration::minutes(45)
    );
    Ok(())
}

#[test]
fn an_overdue_window_of_zero_minutes_is_rejected() {
    assert!(matches!(
        load("[agents]\nrun_overdue_minutes = 0\n"),
        Err(WebConfigError::ZeroRunOverdue)
    ));
    assert!(matches!(load("[agents]\nlate = 1\n"), Err(WebConfigError::Toml { .. })));
}

#[test]
fn the_current_settings_fall_back_to_the_defaults_when_the_file_cannot_be_read() {
    let missing = std::path::Path::new("/nonexistent/web.toml");

    assert_eq!(current_events(missing).poll_interval(), Duration::from_secs(5));
    assert_eq!(current_agents(missing).run_overdue(), time::Duration::minutes(120));
}
