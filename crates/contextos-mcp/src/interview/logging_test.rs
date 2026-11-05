use std::fs;

use tempfile::tempdir;

use super::*;
use crate::interview::tests::{
    NeverRunning, ScriptedInterviewer, StateDirectoryCleanup, no_network_environment, write_existing_config,
};
use crate::{Config, HostPathError, HostPathResolution, ModelCliError, run_interview};

const FILE_PROMPT: &str = "Log file path (blank for stderr):";
const SIZE_PROMPT: &str = "Rotate the log file at this size in MB (blank to never):";
const DAYS_PROMPT: &str = "Delete rotated logs older than this many days (blank to keep all):";
const FILES_PROMPT: &str = "Keep only this many most-recent rotated logs (blank to keep all):";
const DAILY_PROMPT: &str = "Rotate the log file every calendar day (UTC)?";

fn text(value: &str) -> String {
    value.to_owned()
}

/// What `prompt` was prefilled with: `Offered(None)` for a question asked with no default, and `NotAsked` when it
/// never came up.
#[derive(Debug, Eq, PartialEq)]
enum Prefill {
    NotAsked,
    Blank,
    Value(String),
}

fn default_of(interviewer: &ScriptedInterviewer, prompt: &str) -> Prefill {
    interviewer
        .asked
        .iter()
        .find(|(asked, _)| asked == prompt)
        .map_or(Prefill::NotAsked, |(_, default)| {
            default.clone().map_or(Prefill::Blank, Prefill::Value)
        })
}

/// Runs the interview against `config_path` with no network, host, or real home directory involved.
fn run(
    interviewer: &mut ScriptedInterviewer,
    config_path: &Path,
    default_log_file: Option<PathBuf>,
) -> Result<(), InterviewError> {
    let download_model = |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> { Err(ModelCliError::NoRequiredFiles) };
    let resolve_host_path =
        || -> Result<HostPathResolution, HostPathError> { Err(HostPathError::HomeDirectoryUnavailable) };
    let detector = NeverRunning;
    let mut environment = no_network_environment(
        config_path.to_path_buf(),
        &detector,
        &download_model,
        &resolve_host_path,
    );
    environment.default_log_file = default_log_file;
    run_interview(interviewer, &environment).map(|_report| ())
}

#[test]
fn a_fresh_setup_offers_the_default_log_file_with_its_rotation_and_creates_its_folder()
-> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let _state_cleanup = StateDirectoryCleanup::for_vault(vault_dir.path());
    let config_path = config_dir.path().join("config.toml");
    let log_file = config_dir.path().join("logs").join("contextos.log");
    let log_text = log_file.display().to_string();
    let mut interviewer = ScriptedInterviewer::new(
        vec![
            false, // Add another vault?
            false, // Rotate daily?
            false, // Enable semantic search?
            false, // Register with Claude Desktop?
        ],
        vec![
            text("mine"),
            vault_dir.path().display().to_string(),
            log_text.clone(),
            text("10"),
            text("7"),
            String::new(),
        ],
    );

    run(&mut interviewer, &config_path, Some(log_file.clone()))?;

    assert_eq!(default_of(&interviewer, FILE_PROMPT), Prefill::Value(log_text.clone()));
    assert_eq!(default_of(&interviewer, SIZE_PROMPT), Prefill::Value(text("10")));
    assert_eq!(default_of(&interviewer, DAYS_PROMPT), Prefill::Value(text("7")));
    assert_eq!(default_of(&interviewer, FILES_PROMPT), Prefill::Blank);
    let config = Config::try_from(fs::read_to_string(&config_path)?.as_str())?;
    assert_eq!(config.server.log_file, log_text);
    assert_eq!(config.server.log_max_size_mb, Some(10));
    assert!(!config.server.log_rotate_daily);
    assert_eq!(config.server.log_retention_days, Some(7));
    assert_eq!(config.server.log_retention_files, None);
    assert!(log_file.parent().is_some_and(Path::is_dir));
    Ok(())
}

#[test]
fn a_fresh_setup_that_declines_the_offered_log_file_logs_to_stderr_and_creates_no_folder()
-> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let _state_cleanup = StateDirectoryCleanup::for_vault(vault_dir.path());
    let config_path = config_dir.path().join("config.toml");
    let log_file = config_dir.path().join("logs").join("contextos.log");
    let mut interviewer = ScriptedInterviewer::new(
        vec![false, false, false],
        vec![text("mine"), vault_dir.path().display().to_string(), String::new()],
    );

    run(&mut interviewer, &config_path, Some(log_file.clone()))?;

    let rendered = fs::read_to_string(&config_path)?;
    assert!(!rendered.contains("log_"), "{rendered}");
    assert_eq!(Config::try_from(rendered.as_str())?.server.log_file, "");
    assert!(!config_dir.path().join("logs").exists());
    assert_eq!(default_of(&interviewer, SIZE_PROMPT), Prefill::NotAsked);
    Ok(())
}

#[test]
fn a_host_with_no_default_log_file_offers_stderr() -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let _state_cleanup = StateDirectoryCleanup::for_vault(vault_dir.path());
    let config_path = config_dir.path().join("config.toml");
    let mut interviewer = ScriptedInterviewer::new(
        vec![false, false, false],
        vec![text("mine"), vault_dir.path().display().to_string(), String::new()],
    );

    run(&mut interviewer, &config_path, None)?;

    assert_eq!(default_of(&interviewer, FILE_PROMPT), Prefill::Blank);
    Ok(())
}

#[test]
fn editing_an_existing_config_prefills_every_logging_value() -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let _state_cleanup = StateDirectoryCleanup::for_vault(vault_dir.path());
    let config_path = config_dir.path().join("config.toml");
    write_existing_config(
        &config_path,
        &format!(
            "[[vault]]\npath = {:?}\nname = \"mine\"\n\n[server]\nlog_file = \"/var/log/contextos.log\"\nlog_max_size_mb = 25\n\
             log_rotate_daily = true\nlog_retention_days = 14\nlog_retention_files = 5\n",
            vault_dir.path()
        ),
    )?;
    let offered = config_dir.path().join("logs").join("contextos.log");
    let mut interviewer = ScriptedInterviewer::new(
        vec![
            true,  // Keep mine managed?
            false, // Enable semantic search for mine?
            false, // Add a new vault?
            true,  // Rotate daily?
            false, // Register with Claude Desktop?
        ],
        vec![
            text("mine"),
            vault_dir.path().display().to_string(),
            text("/var/log/contextos.log"),
            text("25"),
            text("14"),
            text("5"),
        ],
    );

    run(&mut interviewer, &config_path, Some(offered))?;

    assert_eq!(
        default_of(&interviewer, FILE_PROMPT),
        Prefill::Value(text("/var/log/contextos.log"))
    );
    assert_eq!(default_of(&interviewer, SIZE_PROMPT), Prefill::Value(text("25")));
    assert_eq!(default_of(&interviewer, DAYS_PROMPT), Prefill::Value(text("14")));
    assert_eq!(default_of(&interviewer, FILES_PROMPT), Prefill::Value(text("5")));
    assert!(interviewer.confirm_defaults.contains(&(DAILY_PROMPT.to_owned(), true)));
    let config = Config::try_from(fs::read_to_string(&config_path)?.as_str())?;
    assert_eq!(config.server.log_file, "/var/log/contextos.log");
    assert_eq!(config.server.log_max_size_mb, Some(25));
    assert!(config.server.log_rotate_daily);
    assert_eq!(config.server.log_retention_days, Some(14));
    assert_eq!(config.server.log_retention_files, Some(5));
    assert!(
        !config_dir.path().join("logs").exists(),
        "the offered default is not for a config that sets a file"
    );
    Ok(())
}

#[test]
fn clearing_the_log_file_drops_the_rotation_and_retention_keys_so_the_config_stays_valid()
-> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let _state_cleanup = StateDirectoryCleanup::for_vault(vault_dir.path());
    let config_path = config_dir.path().join("config.toml");
    write_existing_config(
        &config_path,
        &format!(
            "# my notes\n[[vault]]\npath = {:?}\nname = \"mine\"\n\n[server]\nlog_level = \"debug\"\nlog_file = \"/x.log\"\n\
             log_max_size_mb = 5\nlog_rotate_daily = true\nlog_retention_days = 2\nlog_retention_files = 4\n",
            vault_dir.path()
        ),
    )?;
    let mut interviewer = ScriptedInterviewer::new(
        vec![true, false, false, false],
        vec![text("mine"), vault_dir.path().display().to_string(), String::new()],
    );

    run(&mut interviewer, &config_path, None)?;

    let rendered = fs::read_to_string(&config_path)?;
    for key in [
        "log_file",
        "log_max_size_mb",
        "log_rotate_daily",
        "log_retention_days",
        "log_retention_files",
    ] {
        assert!(!rendered.contains(key), "{key} survived: {rendered}");
    }
    assert!(rendered.contains("# my notes") && rendered.contains("log_level = \"debug\""));
    Config::try_from(rendered.as_str())?;
    Ok(())
}

#[test]
fn an_interview_abandoned_after_the_default_log_file_was_chosen_creates_no_folder()
-> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let _state_cleanup = StateDirectoryCleanup::for_vault(vault_dir.path());
    let config_path = config_dir.path().join("config.toml");
    let log_file = config_dir.path().join("logs").join("contextos.log");
    // The script ends at the semantic search question, after the log file was accepted and before the write.
    let mut interviewer = ScriptedInterviewer::new(
        vec![false, false],
        vec![
            text("mine"),
            vault_dir.path().display().to_string(),
            log_file.display().to_string(),
            text("10"),
            text("7"),
            String::new(),
        ],
    );

    let result = run(&mut interviewer, &config_path, Some(log_file));

    assert!(matches!(result, Err(InterviewError::Prompt(_))), "{result:?}");
    assert!(!config_dir.path().join("logs").exists());
    assert!(!config_path.exists());
    Ok(())
}

#[test]
fn a_non_numeric_size_is_reported_and_asked_again() -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let _state_cleanup = StateDirectoryCleanup::for_vault(vault_dir.path());
    let config_path = config_dir.path().join("config.toml");
    let mut interviewer = ScriptedInterviewer::new(
        vec![false, false, false, false],
        vec![
            text("mine"),
            vault_dir.path().display().to_string(),
            text("/x.log"),
            text("big"),
            text("5"),
            String::new(),
            String::new(),
        ],
    );

    run(&mut interviewer, &config_path, None)?;

    assert_eq!(interviewer.notices.len(), 1);
    assert!(
        interviewer.notices[0].contains("not a whole number"),
        "{:?}",
        interviewer.notices
    );
    let config = Config::try_from(fs::read_to_string(&config_path)?.as_str())?;
    assert_eq!(config.server.log_max_size_mb, Some(5));
    Ok(())
}

#[test]
fn repeated_non_numeric_answers_end_the_interview_without_writing() -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let _state_cleanup = StateDirectoryCleanup::for_vault(vault_dir.path());
    let config_path = config_dir.path().join("config.toml");
    let mut texts = vec![text("mine"), vault_dir.path().display().to_string(), text("/x.log")];
    texts.extend(std::iter::repeat_n(text("big"), 5));
    let mut interviewer = ScriptedInterviewer::new(vec![false], texts);

    let result = run(&mut interviewer, &config_path, None);

    assert!(
        matches!(result, Err(InterviewError::TooManyAttempts { .. })),
        "{result:?}"
    );
    assert!(!config_path.exists());
    Ok(())
}

#[test]
fn a_rejected_combination_is_reported_and_the_logging_questions_asked_again() -> Result<(), Box<dyn std::error::Error>>
{
    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let _state_cleanup = StateDirectoryCleanup::for_vault(vault_dir.path());
    let config_path = config_dir.path().join("config.toml");
    let mut interviewer = ScriptedInterviewer::new(
        vec![false, false, false, false, false],
        vec![
            text("mine"),
            vault_dir.path().display().to_string(),
            // A zero retention is refused by the schema.
            text("/x.log"),
            text("5"),
            text("0"),
            String::new(),
            // The second pass is valid.
            text("/x.log"),
            text("5"),
            text("3"),
            String::new(),
        ],
    );

    run(&mut interviewer, &config_path, None)?;

    assert_eq!(interviewer.notices.len(), 1);
    assert!(
        interviewer.notices[0].starts_with("That was rejected"),
        "{:?}",
        interviewer.notices
    );
    let config = Config::try_from(fs::read_to_string(&config_path)?.as_str())?;
    assert_eq!(config.server.log_retention_days, Some(3));
    Ok(())
}

#[test]
fn a_retention_count_beyond_u32_is_refused_rather_than_truncated() -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let _state_cleanup = StateDirectoryCleanup::for_vault(vault_dir.path());
    let config_path = config_dir.path().join("config.toml");
    let mut interviewer = ScriptedInterviewer::new(
        vec![false, false],
        vec![
            text("mine"),
            vault_dir.path().display().to_string(),
            text("/x.log"),
            text("5"),
            text("4294967296"),
        ],
    );

    let result = run(&mut interviewer, &config_path, None);

    assert!(
        matches!(result, Err(InterviewError::NumberOutOfRange { value: 4_294_967_296 })),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn the_offered_default_log_file_is_named_for_this_binary() {
    assert_eq!(default_log_file(), contextos_logging::offered_log_file("contextos.log"));
}
