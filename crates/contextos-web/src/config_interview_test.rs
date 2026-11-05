use std::collections::VecDeque;
use std::fs;

use tempfile::tempdir;

use super::*;
use crate::config::load_web_config;

type BoxError = Box<dyn std::error::Error>;

/// A scripted [`WebInterviewer`] double: hands back fixed answers in call
/// order and records every prompt (with the default it was offered), so
/// [`run_web_config_interview`] is exercised deterministically with no real
/// terminal, and a re-run's prefilled defaults can be asserted.
#[derive(Default)]
struct ScriptedInterviewer {
    confirms: VecDeque<bool>,
    texts: VecDeque<String>,
    choices: VecDeque<usize>,
    asked: Vec<(String, Option<String>)>,
    confirmed: Vec<(String, bool)>,
    chosen: Vec<(String, usize)>,
    notes: Vec<String>,
}

impl ScriptedInterviewer {
    fn new(confirms: Vec<bool>, texts: Vec<&str>, choices: Vec<usize>) -> Self {
        Self {
            confirms: confirms.into(),
            texts: texts.into_iter().map(str::to_owned).collect(),
            choices: choices.into(),
            ..Self::default()
        }
    }

    fn default_for(&self, prompt_fragment: &str) -> Option<String> {
        self.asked
            .iter()
            .find(|(prompt, _)| prompt.contains(prompt_fragment))
            .and_then(|(_, default)| default.clone())
    }
}

impl WebInterviewer for ScriptedInterviewer {
    fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool, WebInterviewError> {
        self.confirmed.push((prompt.to_owned(), default));
        self.confirms.pop_front().ok_or_else(|| exhausted(prompt))
    }

    fn ask(&mut self, prompt: &str, default: Option<&str>) -> Result<String, WebInterviewError> {
        self.asked.push((prompt.to_owned(), default.map(str::to_owned)));
        self.texts.pop_front().ok_or_else(|| exhausted(prompt))
    }

    fn choose(&mut self, prompt: &str, _options: &[&str], default: usize) -> Result<usize, WebInterviewError> {
        self.chosen.push((prompt.to_owned(), default));
        self.choices.pop_front().ok_or_else(|| exhausted(prompt))
    }

    fn notify(&mut self, message: &str) {
        self.notes.push(message.to_owned());
    }
}

fn exhausted(prompt: &str) -> WebInterviewError {
    WebInterviewError::Prompt(inquire::InquireError::Custom(
        format!("scripted interviewer has no answer queued for prompt: {prompt:?}").into(),
    ))
}

#[test]
fn a_fresh_run_writes_a_valid_web_toml_covering_every_section() -> Result<(), BoxError> {
    let directory = tempdir()?;
    let config_path = directory.path().join("web.toml");
    let environment = WebInterviewEnvironment {
        config_path: config_path.clone(),
        default_log_file: None,
    };

    let mut interviewer = ScriptedInterviewer::new(
        vec![
            true,  // Configure the web UI appearance?
            false, // Add another MCP server?
        ],
        vec![
            "127.0.0.1:7500",             // bind
            "",                           // static asset directory
            "",                           // log file
            "midnight",                   // appearance: theme
            "",                           // appearance: font
            "",                           // appearance: size
            "Command Centre",             // appearance: system name
            "contextos",                  // MCP server name
            "contextos",                  // command
            "--config, /tmp/config.toml", // args
        ],
        vec![
            2, // log level = info
            0, // transport = stdio
        ],
    );

    let report = run_web_config_interview(&mut interviewer, &environment)?;

    assert!(report.created);
    assert_eq!(report.added_mcp_servers, vec!["contextos".to_owned()]);

    let config = load_web_config(&config_path)?;
    assert_eq!(config.server.bind, "127.0.0.1:7500");
    assert_eq!(
        config.server.ui.get("theme").and_then(toml::Value::as_str),
        Some("midnight")
    );
    assert_eq!(config.mcp_servers.len(), 1);
    assert!(matches!(
        &config.mcp_servers[0],
        McpServerConfig::Stdio { name, command, args }
            if name == "contextos" && command == "contextos" && args == &["--config".to_owned(), "/tmp/config.toml".to_owned()]
    ));
    Ok(())
}

#[test]
fn an_existing_web_toml_is_offered_back_with_its_current_values_as_defaults() -> Result<(), BoxError> {
    let directory = tempdir()?;
    let config_path = directory.path().join("web.toml");
    fs::write(
        &config_path,
        r#"# a hand-written note that must survive the wizard
[server]
bind = "127.0.0.1:9999"
log_level = "debug"
log_file = ""

[server.ui]
theme = "dark"

[[mcp_server]]
name = "primary"
transport = "http"
endpoint = "http://127.0.0.1:8080"
token_env = "PRIMARY_TOKEN"
"#,
    )?;
    let environment = WebInterviewEnvironment {
        config_path: config_path.clone(),
        default_log_file: None,
    };

    let mut interviewer = ScriptedInterviewer::new(
        vec![
            true,  // Configure the web UI appearance?
            false, // Add a new MCP server?
        ],
        vec![
            "127.0.0.1:9999", // bind (accept current)
            "",               // static asset directory
            "",               // log file
            "dark",           // appearance: theme (accept current)
            "",               // appearance: font
            "",               // appearance: size
            "Command Centre", // appearance: system name
        ],
        vec![
            3, // log level, starting cursor from "debug"
            0, // MCP server "primary": Keep
        ],
    );

    let report = run_web_config_interview(&mut interviewer, &environment)?;

    assert!(!report.created);
    assert_eq!(
        interviewer.default_for("HTTP bind address").as_deref(),
        Some("127.0.0.1:9999")
    );
    assert_eq!(interviewer.default_for("Appearance: theme").as_deref(), Some("dark"));
    let log_level_cursor = interviewer
        .chosen
        .iter()
        .find(|(prompt, _)| prompt == "Log level")
        .map(|(_, cursor)| *cursor);
    assert_eq!(log_level_cursor, Some(3));
    assert_eq!(
        interviewer
            .chosen
            .iter()
            .find(|(prompt, _)| prompt.contains("primary"))
            .map(|(prompt, _)| prompt.clone()),
        Some("MCP server \"primary\"".to_owned())
    );

    let rendered = fs::read_to_string(&config_path)?;
    assert!(rendered.contains("# a hand-written note that must survive the wizard"));

    let config = load_web_config(&config_path)?;
    assert_eq!(config.server.bind, "127.0.0.1:9999");
    assert_eq!(config.mcp_servers.len(), 1);
    assert!(matches!(
        &config.mcp_servers[0],
        McpServerConfig::Http { name, token_env, .. }
            if name == "primary" && token_env.as_deref() == Some("PRIMARY_TOKEN")
    ));
    Ok(())
}

#[test]
fn a_non_loopback_bind_is_rejected_and_re_prompted_without_writing() -> Result<(), BoxError> {
    let directory = tempdir()?;
    let config_path = directory.path().join("web.toml");
    let environment = WebInterviewEnvironment {
        config_path: config_path.clone(),
        default_log_file: None,
    };

    let mut interviewer = ScriptedInterviewer::new(
        vec![false, false],
        vec![
            "0.0.0.0:7373",   // bind: rejected
            "",               // static
            "",               // log file
            "127.0.0.1:7373", // bind: accepted
            "",               // static
            "",               // log file
            "only-server",    // MCP name
            "contextos",      // command
            "",               // args
        ],
        vec![
            2, // log level (attempt 1)
            2, // log level (attempt 2)
            0, // transport
        ],
    );

    let report = run_web_config_interview(&mut interviewer, &environment)?;

    assert!(report.created);
    assert!(
        interviewer.notes.iter().any(|note| note.contains("loopback")),
        "expected a rejection note mentioning loopback, got {:?}",
        interviewer.notes
    );
    assert_eq!(load_web_config(&config_path)?.server.bind, "127.0.0.1:7373");
    Ok(())
}

#[test]
fn stdio_and_http_entries_are_both_written_in_their_documented_shape() -> Result<(), BoxError> {
    let directory = tempdir()?;
    let config_path = directory.path().join("web.toml");
    let environment = WebInterviewEnvironment {
        config_path: config_path.clone(),
        default_log_file: None,
    };

    let mut interviewer = ScriptedInterviewer::new(
        vec![
            false, // appearance
            true,  // add another MCP server
            false, // add another MCP server
        ],
        vec![
            "127.0.0.1:7373",           // bind
            "",                         // static
            "",                         // log file
            "local",                    // MCP 1 name
            "contextos",                // MCP 1 command
            "--config, /p/config.toml", // MCP 1 args
            "remote",                   // MCP 2 name
            "http://127.0.0.1:9000",    // MCP 2 endpoint
            "REMOTE_TOKEN",             // MCP 2 token env
        ],
        vec![
            2, // log level
            0, // MCP 1 transport = stdio
            1, // MCP 2 transport = http
        ],
    );

    run_web_config_interview(&mut interviewer, &environment)?;

    let config = load_web_config(&config_path)?;
    assert!(matches!(
        &config.mcp_servers[0],
        McpServerConfig::Stdio { name, args, .. }
            if name == "local" && args == &["--config".to_owned(), "/p/config.toml".to_owned()]
    ));
    assert!(matches!(
        &config.mcp_servers[1],
        McpServerConfig::Http { name, endpoint, token_env, .. }
            if name == "remote" && endpoint == "http://127.0.0.1:9000" && token_env.as_deref() == Some("REMOTE_TOKEN")
    ));
    Ok(())
}

#[test]
fn an_existing_entry_can_be_edited_and_another_removed() -> Result<(), BoxError> {
    let directory = tempdir()?;
    let config_path = directory.path().join("web.toml");
    fs::write(
        &config_path,
        r#"[server]
bind = "127.0.0.1:7373"
log_file = ""

[[mcp_server]]
name = "a"
transport = "stdio"
command = "contextos"
args = ["--config", "a.toml"]

[[mcp_server]]
name = "b"
transport = "stdio"
command = "contextos"
args = []
"#,
    )?;
    let environment = WebInterviewEnvironment {
        config_path: config_path.clone(),
        default_log_file: None,
    };

    let mut interviewer = ScriptedInterviewer::new(
        vec![
            false, // appearance
            false, // add a new MCP server
        ],
        vec![
            "127.0.0.1:7373",   // bind
            "",                 // static
            "",                 // log file
            "a-renamed",        // edited entry name
            "contextos",        // edited entry command
            "--config, a.toml", // edited entry args
        ],
        vec![
            2, // log level
            1, // MCP server "a": Edit
            0, // edited transport = stdio
            2, // MCP server "b": Remove
        ],
    );

    let report = run_web_config_interview(&mut interviewer, &environment)?;

    assert_eq!(report.edited_mcp_servers, vec!["a-renamed".to_owned()]);
    assert_eq!(report.removed_mcp_servers, vec!["b".to_owned()]);

    let config = load_web_config(&config_path)?;
    assert_eq!(config.mcp_servers.len(), 1);
    assert_eq!(config.mcp_servers[0].name(), "a-renamed");
    Ok(())
}

#[test]
fn a_read_error_on_an_existing_file_stops_the_wizard() -> Result<(), BoxError> {
    let directory = tempdir()?;
    // A directory where the wizard expects a file: an error that is neither
    // "not found" (a first run) nor a parse failure.
    let config_path = directory.path().join("web.toml");
    fs::create_dir(&config_path)?;
    let environment = WebInterviewEnvironment {
        config_path,
        default_log_file: None,
    };

    let mut interviewer = ScriptedInterviewer::new(vec![], vec![], vec![]);
    let result = run_web_config_interview(&mut interviewer, &environment);

    assert!(matches!(result, Err(WebInterviewError::Read { .. })));
    Ok(())
}

#[test]
fn the_report_names_the_file_and_a_next_step() {
    let report = WebInterviewReport {
        config_path: "/etc/contextos/web.toml".into(),
        created: true,
        added_mcp_servers: vec!["contextos".to_owned()],
        edited_mcp_servers: Vec::new(),
        removed_mcp_servers: Vec::new(),
    };

    let rendered = report.to_string();
    assert!(rendered.contains("Created /etc/contextos/web.toml"));
    assert!(rendered.contains("Added MCP server(s): contextos"));
    assert!(rendered.contains("service install"));
}

#[test]
fn parse_manual_path_strips_matching_surrounding_double_quotes() {
    // The real-world case `contextos-mcp::interview::parse_manual_path`
    // already guards against: Windows Explorer's "Copy as path" wraps the
    // clipboard text in double quotes, which then land literally inside the
    // stored value.
    assert_eq!(
        parse_manual_path("\"C:\\Users\\pj\\ContextOS\\contextos.exe\""),
        "C:\\Users\\pj\\ContextOS\\contextos.exe"
    );
}

#[test]
fn parse_manual_path_strips_matching_surrounding_single_quotes() {
    assert_eq!(
        parse_manual_path("'/home/someone/contextos'"),
        "/home/someone/contextos"
    );
}

#[test]
fn parse_manual_path_leaves_an_unquoted_value_unchanged() {
    assert_eq!(parse_manual_path("/home/someone/contextos"), "/home/someone/contextos");
}

#[test]
fn parse_manual_path_leaves_mismatched_quotes_unchanged() {
    assert_eq!(
        parse_manual_path("\"/home/someone/contextos"),
        "\"/home/someone/contextos"
    );
}

#[test]
fn parse_manual_path_leaves_a_blank_answer_blank() {
    assert_eq!(parse_manual_path(""), "");
}

#[test]
fn notify_config_detection_reports_a_found_default_config_toml() -> Result<(), BoxError> {
    let directory = tempdir()?;
    let path = directory.path().join("config.toml");
    fs::write(&path, "")?;
    let mut interviewer = ScriptedInterviewer::default();

    notify_config_detection_at(&mut interviewer, &path);

    assert_eq!(interviewer.notes.len(), 1);
    assert!(interviewer.notes[0].contains("Detected an existing ContextOS configuration"));
    assert!(interviewer.notes[0].contains(&path.display().to_string()));
    Ok(())
}

#[test]
fn notify_config_detection_reports_a_missing_default_config_toml() -> Result<(), BoxError> {
    let directory = tempdir()?;
    let path = directory.path().join("config.toml");
    let mut interviewer = ScriptedInterviewer::default();

    notify_config_detection_at(&mut interviewer, &path);

    assert_eq!(interviewer.notes.len(), 1);
    assert!(interviewer.notes[0].contains("No ContextOS configuration file found"));
    assert!(interviewer.notes[0].contains("contextos config"));
    Ok(())
}

#[test]
fn where_the_host_has_a_default_log_file_the_interview_offers_it_with_short_rotation() -> Result<(), BoxError> {
    let directory = tempdir()?;
    let config_path = directory.path().join("web.toml");
    let default_file = directory
        .path()
        .join("contextos")
        .join("logs")
        .join("contextos-web.log");
    let default_text = default_file.to_string_lossy().into_owned();
    let environment = WebInterviewEnvironment {
        config_path: config_path.clone(),
        default_log_file: Some(default_file.clone()),
    };

    let mut interviewer = ScriptedInterviewer::new(
        vec![
            false, // rotate the log file every calendar day?
            false, // appearance
            false, // add another MCP server
        ],
        vec![
            "127.0.0.1:7373", // bind
            "",               // static asset directory
            &default_text,    // log file: the default offered
            "10",             // rotate at this size in MB
            "7",              // delete rotated logs older than this many days
            "",               // keep only this many rotated logs
            "only-server",    // MCP name
            "contextos",      // command
            "",               // args
        ],
        vec![
            2, // log level
            0, // transport = stdio
        ],
    );

    run_web_config_interview(&mut interviewer, &environment)?;

    assert_eq!(interviewer.default_for("Log file path"), Some(default_text.clone()));
    assert_eq!(
        interviewer.default_for("Rotate the log file at this size"),
        Some("10".to_owned())
    );
    assert_eq!(
        interviewer.default_for("Delete rotated logs older"),
        Some("7".to_owned())
    );
    assert!(
        default_file.parent().is_some_and(std::path::Path::is_dir),
        "the default file's folder is created, so the server can open it"
    );
    let config = load_web_config(&config_path)?;
    assert_eq!(config.server.log_file, default_text);
    assert_eq!(config.server.log_max_size_mb, Some(10));
    assert_eq!(config.server.log_retention_days, Some(7));
    Ok(())
}

#[test]
fn an_earlier_choice_of_stderr_is_offered_again_rather_than_the_default_log_file() -> Result<(), BoxError> {
    let directory = tempdir()?;
    let config_path = directory.path().join("web.toml");
    fs::write(&config_path, "[server]\nlog_file = \"\"\n")?;
    let environment = WebInterviewEnvironment {
        config_path: config_path.clone(),
        default_log_file: Some(directory.path().join("logs").join("contextos-web.log")),
    };

    let mut interviewer = ScriptedInterviewer::new(
        vec![false, false],
        vec!["127.0.0.1:7373", "", "", "only-server", "contextos", ""],
        vec![2, 0],
    );

    run_web_config_interview(&mut interviewer, &environment)?;

    assert_eq!(interviewer.default_for("Log file path"), None);
    assert!(!directory.path().join("logs").exists());
    assert_eq!(load_web_config(&config_path)?.server.log_file, "");
    Ok(())
}

#[test]
fn a_quoted_static_asset_directory_and_log_file_are_stored_without_their_quotes() -> Result<(), BoxError> {
    let directory = tempdir()?;
    let config_path = directory.path().join("web.toml");
    let environment = WebInterviewEnvironment {
        config_path: config_path.clone(),
        default_log_file: None,
    };

    let mut interviewer = ScriptedInterviewer::new(
        vec![
            false, // rotate the log file every calendar day?
            false, // appearance
            false, // add another MCP server
        ],
        vec![
            "127.0.0.1:7373",               // bind
            "\"/opt/contextos/static\"",    // static asset directory, quoted
            "'/var/log/contextos-web.log'", // log file, quoted
            "",                             // rotate at this size in MB
            "only-server",                  // MCP name
            "contextos",                    // command
            "",                             // args
        ],
        vec![
            2, // log level
            0, // transport = stdio
        ],
    );

    run_web_config_interview(&mut interviewer, &environment)?;

    let config = load_web_config(&config_path)?;
    assert_eq!(
        config.server.static_dir.as_deref(),
        Some(std::path::Path::new("/opt/contextos/static"))
    );
    assert_eq!(config.server.log_file, "/var/log/contextos-web.log");
    Ok(())
}

#[test]
fn a_quoted_mcp_command_and_argument_are_stored_without_their_quotes() -> Result<(), BoxError> {
    let directory = tempdir()?;
    let config_path = directory.path().join("web.toml");
    let environment = WebInterviewEnvironment {
        config_path: config_path.clone(),
        default_log_file: None,
    };

    let mut interviewer = ScriptedInterviewer::new(
        vec![
            false, // appearance
            false, // add another MCP server
        ],
        vec![
            "127.0.0.1:7373",                                  // bind
            "",                                                // static
            "",                                                // log file
            "quoted",                                          // MCP name
            "\"C:\\Program Files\\ContextOS\\contextos.exe\"", // command, quoted
            "--config, \"C:\\Users\\pj\\config.toml\"",        // args, one quoted
        ],
        vec![
            2, // log level
            0, // transport = stdio
        ],
    );

    run_web_config_interview(&mut interviewer, &environment)?;

    let config = load_web_config(&config_path)?;
    assert!(matches!(
        &config.mcp_servers[0],
        McpServerConfig::Stdio { command, args, .. }
            if command == "C:\\Program Files\\ContextOS\\contextos.exe"
                && args == &["--config".to_owned(), "C:\\Users\\pj\\config.toml".to_owned()]
    ));
    Ok(())
}

#[test]
fn a_fresh_stdio_mcp_entry_offers_no_arguments_so_every_process_started_from_it_can_run_alongside_another()
-> Result<(), BoxError> {
    // Command-line workflow runs start their own process from the same entry the web server does, so an argument such
    // as `--http`, which only one process on the host can honour, must never be suggested here.
    let directory = tempdir()?;
    let config_path = directory.path().join("web.toml");
    let environment = WebInterviewEnvironment {
        config_path: config_path.clone(),
        default_log_file: None,
    };

    let mut interviewer = ScriptedInterviewer::new(
        vec![
            false, // appearance
            false, // add another MCP server
        ],
        vec![
            "127.0.0.1:7373", // bind
            "",               // static
            "",               // log file
            "contextos",      // MCP name
            "contextos",      // command
            "",               // args, as offered
        ],
        vec![
            2, // log level
            0, // transport = stdio
        ],
    );

    run_web_config_interview(&mut interviewer, &environment)?;

    assert_eq!(interviewer.default_for("Arguments"), None);
    let config = load_web_config(&config_path)?;
    assert!(matches!(
        &config.mcp_servers[0],
        McpServerConfig::Stdio { args, .. } if args.is_empty()
    ));
    Ok(())
}

#[test]
fn an_interview_abandoned_after_the_default_log_file_was_chosen_creates_no_folder() -> Result<(), BoxError> {
    let directory = tempdir()?;
    let config_path = directory.path().join("web.toml");
    let default_file = directory.path().join("logs").join("contextos-web.log");
    let environment = WebInterviewEnvironment {
        config_path: config_path.clone(),
        default_log_file: Some(default_file.clone()),
    };
    // Every server answer, then nothing: the appearance question is never answered.
    let mut interviewer = ScriptedInterviewer::new(
        vec![false],
        vec!["127.0.0.1:7373", "", &default_file.to_string_lossy(), "10", "7", ""],
        vec![2],
    );

    assert!(run_web_config_interview(&mut interviewer, &environment).is_err());

    assert!(
        !directory.path().join("logs").exists(),
        "nothing is created for an unfinished interview"
    );
    assert!(!config_path.exists());
    Ok(())
}
