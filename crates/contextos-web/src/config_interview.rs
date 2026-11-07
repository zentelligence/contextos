//! `contextos-web config`: an interactive wizard that creates or updates
//! `web.toml` from the terminal, mirroring `contextos config`'s guided
//! interview on the MCP-server binary so an operator never has to hand-edit
//! the file.
//!
//! `contextos-web` must not depend on `contextos-mcp` in production
//!, and that binary is where the guided-setup
//! interview loop and its `TerminalInterviewer` live, so this is a
//! deliberately smaller re-implementation over `web.toml`'s much smaller
//! field set rather than a shared abstraction lifted into `contextos-core`
//! (which holds no interactive-CLI concerns). Every operator interaction
//! goes through the [`WebInterviewer`] trait, so [`run_web_config_interview`]
//! is exercised in tests against a scripted double with no real terminal
//! I/O.
//!
//! Writing goes through [`WebConfigDocument`], the same comment-preserving,
//! schema-revalidating `web.toml` writer `/server/settings/` uses, so a wizard run
//! against an existing file keeps its hand-written commentary and can never
//! persist a document that would fail [`crate::config::load_web_config`] to
//! re-load. Nothing is written until every question has been answered: an
//! abandoned run (operator cancellation, or a step that stays invalid after
//! [`MAX_ATTEMPTS`] corrections) leaves any existing file untouched.

use std::fmt::{self, Display, Formatter};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use inquire::{Confirm, Select, Text};
use serde_json::{Map as JsonMap, Value as JsonValue, json};
use thiserror::Error;

use crate::atomic_write::write_atomically;
use crate::config::{DEFAULT_SYSTEM_NAME, McpServerConfig, WebServerConfig};
use crate::config_writer::{WebConfigDocument, WebConfigWriterError, WebServerSettingsInput};

/// How many times one wizard step re-prompts after the edited configuration
/// is rejected as invalid before it gives up and surfaces the error. It
/// exists only so a genuinely un-fixable pre-existing file (a hand
/// corruption the wizard's own questions do not cover) cannot loop forever,
/// not to limit ordinary correction.
pub const MAX_ATTEMPTS: u32 = 5;

/// `[server].log_level`'s allowed values, lowest to highest verbosity,
/// matching [`crate::config::WebLogLevel`].
const LOG_LEVELS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];

/// The two `[[mcp_server]]` transports, in the order they are offered.
const TRANSPORTS: [&str; 2] = ["stdio", "http"];

/// The `[server.ui]` keys with real operator-visible effect (the Appearance
/// pane, `contextos-web.css`, and the shell header); every other key in that
/// table is persisted but inert, so the wizard does not ask about it.
const APPEARANCE_KEYS: [&str; 4] = ["theme", "font", "size", "system_name"];

/// One operator interaction the wizard needs: a yes/no question with a
/// default, a free-text answer optionally prefilled with a default, a choice
/// from a fixed list, or a one-line status message. Abstracted so
/// [`run_web_config_interview`] is testable against a scripted double
/// instead of a real terminal.
pub trait WebInterviewer {
    /// Asks a yes/no question, returning `default` when the operator enters
    /// nothing.
    ///
    /// # Errors
    ///
    /// Returns [`WebInterviewError::Prompt`] when the operator cancels the
    /// prompt (for example Ctrl-C) or terminal I/O fails.
    fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool, WebInterviewError>;

    /// Asks a free-text question, prefilled with `default` when it is
    /// `Some`, returning the operator's trimmed answer. The prefill is
    /// editable text, so deleting it answers blank.
    ///
    /// # Errors
    ///
    /// Returns [`WebInterviewError::Prompt`] when the operator cancels the
    /// prompt or terminal I/O fails.
    fn ask(&mut self, prompt: &str, default: Option<&str>) -> Result<String, WebInterviewError>;

    /// Presents `options` as a fixed list with `default` pre-selected and
    /// returns the index of the operator's choice.
    ///
    /// # Errors
    ///
    /// Returns [`WebInterviewError::Prompt`] when the operator cancels the
    /// prompt, terminal I/O fails, or (structurally unreachable in the
    /// terminal implementation, but typed rather than a panic regardless)
    /// the selection cannot be matched back to one of `options`.
    fn choose(&mut self, prompt: &str, options: &[&str], default: usize) -> Result<usize, WebInterviewError>;

    /// Shows a one-line status message (a rejected-configuration reason, or
    /// "not a whole number, try again"), between prompts.
    fn notify(&mut self, message: &str);
}

/// The real [`WebInterviewer`], backed by `inquire`'s terminal prompts.
#[derive(Clone, Copy, Debug, Default)]
pub struct TerminalWebInterviewer;

impl WebInterviewer for TerminalWebInterviewer {
    fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool, WebInterviewError> {
        Ok(Confirm::new(prompt).with_default(default).prompt()?)
    }

    fn ask(&mut self, prompt: &str, default: Option<&str>) -> Result<String, WebInterviewError> {
        let mut text = Text::new(prompt);
        // An initial value, not a default: the operator sees it typed in and can edit or delete it, so a question
        // whose blank answer means something ("blank for stderr") can still be answered blank once prefilled.
        if let Some(value) = default {
            text = text.with_initial_value(value);
        }
        Ok(text.prompt()?.trim().to_owned())
    }

    fn choose(&mut self, prompt: &str, options: &[&str], default: usize) -> Result<usize, WebInterviewError> {
        let cursor = default.min(options.len().saturating_sub(1));
        let selection = Select::new(prompt, options.to_vec())
            .with_starting_cursor(cursor)
            .prompt()?;
        options.iter().position(|option| *option == selection).ok_or_else(|| {
            WebInterviewError::Prompt(inquire::InquireError::Custom(
                "selected option not found among the offered choices".into(),
            ))
        })
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "the wizard speaks to the operator on standard output, an interactive stream that is not a run result"
    )]
    fn notify(&mut self, message: &str) {
        // Deliberate operator-facing CLI output on stdout, the same channel
        // (and `io::Write`, not the `println!` macro `rust-quality.md`
        // forbids for diagnostics) the wizard's closing report uses.
        let _ignored = writeln!(std::io::stdout().lock(), "{message}");
    }
}

/// This host's [`WebInterviewEnvironment::default_log_file`], `contextos-web.log`, by the rule `contextos` shares
/// ([`contextos_logging::offered_log_file`]).
#[must_use]
pub fn default_log_file() -> Option<PathBuf> {
    contextos_logging::offered_log_file("contextos-web.log")
}

/// Everything [`run_web_config_interview`] needs beyond operator
/// interaction: the `web.toml` path it reads and writes, and the log file it
/// offers when that file sets none.
#[derive(Clone, Debug)]
pub struct WebInterviewEnvironment {
    pub config_path: PathBuf,
    /// The log file offered when `web.toml` sets none, with a short rotation
    /// ([`contextos_logging::OFFERED_MAX_SIZE_MB`], [`contextos_logging::OFFERED_RETENTION_DAYS`]): [`default_log_file`]
    /// in production, on a host whose service would otherwise lose stderr. `None` offers stderr.
    pub default_log_file: Option<PathBuf>,
}

/// A short, human-readable summary of one `contextos-web config` run,
/// written to stdout by the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebInterviewReport {
    pub config_path: PathBuf,
    /// Whether the run created `web.toml` (no file existed) rather than
    /// updating an existing one.
    pub created: bool,
    pub added_mcp_servers: Vec<String>,
    pub edited_mcp_servers: Vec<String>,
    pub removed_mcp_servers: Vec<String>,
}

impl Display for WebInterviewReport {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "ContextOS web configuration")?;
        let verb = if self.created { "Created" } else { "Updated" };
        writeln!(formatter, "{verb} {}", self.config_path.display())?;
        if !self.added_mcp_servers.is_empty() {
            writeln!(formatter, "Added MCP server(s): {}", self.added_mcp_servers.join(", "))?;
        }
        if !self.edited_mcp_servers.is_empty() {
            writeln!(
                formatter,
                "Edited MCP server(s): {}",
                self.edited_mcp_servers.join(", ")
            )?;
        }
        if !self.removed_mcp_servers.is_empty() {
            writeln!(
                formatter,
                "Removed MCP server(s): {}",
                self.removed_mcp_servers.join(", ")
            )?;
        }
        writeln!(
            formatter,
            "Run `contextos-web service install` to run this instance as a background service."
        )
    }
}

/// Runs the full `contextos-web config` wizard: walks `[server]`,
/// `[server.ui]`, and every `[[mcp_server]]` entry, prefilling an existing
/// file's current values as editable defaults, then writes the result
/// atomically.
///
/// # Errors
///
/// Returns [`WebInterviewError::Read`] when an existing `web.toml` cannot be
/// read, [`WebInterviewError::Prompt`] on operator cancellation,
/// [`WebInterviewError::TooManyAttempts`] when a step stays invalid after
/// [`MAX_ATTEMPTS`] corrections, [`WebInterviewError::Writer`] for any other
/// rejected edit, or [`WebInterviewError::Write`] when the finished file
/// cannot be persisted.
pub fn run_web_config_interview(
    interviewer: &mut dyn WebInterviewer,
    environment: &WebInterviewEnvironment,
) -> Result<WebInterviewReport, WebInterviewError> {
    let existing = match fs::read_to_string(&environment.config_path) {
        Ok(text) => Some(text),
        Err(source) if source.kind() == ErrorKind::NotFound => None,
        Err(source) => {
            return Err(WebInterviewError::Read {
                path: environment.config_path.clone(),
                source,
            });
        }
    };
    let created = existing.is_none();
    let mut document = WebConfigDocument::parse(existing.as_deref().unwrap_or_default())?;

    let log_directory = configure_server(interviewer, &mut document, environment.default_log_file.as_deref())?;
    configure_appearance(interviewer, &mut document)?;
    let changes = configure_mcp_servers(interviewer, &mut document)?;

    // Created only once every answer is in, so a cancelled interview leaves nothing behind.
    if let Some(directory) = log_directory {
        fs::create_dir_all(&directory).map_err(|source| WebInterviewError::LogDirectory {
            path: directory,
            source,
        })?;
    }
    let rendered = document.render();
    write_atomically(&environment.config_path, rendered.as_bytes()).map_err(|source| WebInterviewError::Write {
        path: environment.config_path.clone(),
        source,
    })?;

    Ok(WebInterviewReport {
        config_path: environment.config_path.clone(),
        created,
        added_mcp_servers: changes.added,
        edited_mcp_servers: changes.edited,
        removed_mcp_servers: changes.removed,
    })
}

/// Asks the `[server]` scalar questions and writes them, re-prompting on a
/// rejected result (a non-loopback `bind`, an unsatisfiable log-rotation
/// combination). The rotation and retention questions are only asked once
/// `log_file` is non-empty, since the schema rejects them otherwise.
///
/// While `web.toml` sets no `log_file`, `default_log_file` (when there is one) is offered with a short rotation;
/// `log_file = ""` is the operator's earlier choice of stderr and is offered again. Returns the default file's folder
/// when that file is chosen, for the caller to create.
fn configure_server(
    interviewer: &mut dyn WebInterviewer,
    document: &mut WebConfigDocument,
    default_log_file: Option<&Path>,
) -> Result<Option<PathBuf>, WebInterviewError> {
    let current = document.server_settings();
    let fallback = WebServerConfig::default();
    let offered_log_file = default_log_file.filter(|_| current.log_file.is_none());
    let offered_text = offered_log_file.map(|path| path.to_string_lossy().into_owned());
    let offered = offered_text.is_some();

    let bind_default = current.bind.unwrap_or(fallback.bind);
    let static_default = current.static_dir.unwrap_or_default();
    let level_default = current.log_level.unwrap_or_else(|| "info".to_owned());
    let log_file_default = current.log_file.or_else(|| offered_text.clone()).unwrap_or_default();
    let size_default = optional_number_default(
        current
            .log_max_size_mb
            .or_else(|| offered.then_some(i64::from(contextos_logging::OFFERED_MAX_SIZE_MB))),
    );
    let daily_default = current.log_rotate_daily.unwrap_or(false);
    let retain_days_default = optional_number_default(
        current
            .log_retention_days
            .or_else(|| offered.then_some(i64::from(contextos_logging::OFFERED_RETENTION_DAYS))),
    );
    let retain_files_default = optional_number_default(current.log_retention_files);

    let mut chosen_log_file = String::new();
    retry_until_valid(interviewer, "server settings", |interviewer| {
        let bind = interviewer.ask("HTTP bind address (loopback only)", Some(&bind_default))?;
        let static_dir = parse_manual_path(&interviewer.ask(
            "Extra static asset directory (blank for the bundled assets only)",
            optional(&static_default),
        )?);
        let level_index = interviewer.choose("Log level", &LOG_LEVELS, level_position(&level_default))?;
        let log_file =
            parse_manual_path(&interviewer.ask("Log file path (blank for stderr)", optional(&log_file_default))?);
        chosen_log_file.clone_from(&log_file);

        let mut log_max_size_mb = None;
        let mut log_rotate_daily = false;
        let mut log_retention_days = None;
        let mut log_retention_files = None;
        if !log_file.is_empty() {
            log_max_size_mb = ask_optional_number(
                interviewer,
                "Rotate the log file at this size in MB (blank to never)",
                &size_default,
            )?;
            log_rotate_daily = interviewer.confirm("Rotate the log file every calendar day (UTC)?", daily_default)?;
            if log_max_size_mb.is_some() || log_rotate_daily {
                log_retention_days = ask_optional_number(
                    interviewer,
                    "Delete rotated logs older than this many days (blank to keep all)",
                    &retain_days_default,
                )?
                .map(narrow_u32)
                .transpose()?;
                log_retention_files = ask_optional_number(
                    interviewer,
                    "Keep only this many most-recent rotated logs (blank to keep all)",
                    &retain_files_default,
                )?
                .map(narrow_u32)
                .transpose()?;
            }
        }

        let input = WebServerSettingsInput {
            bind,
            static_dir: non_empty(static_dir),
            log_level: LOG_LEVELS[level_index].to_owned(),
            log_file,
            log_max_size_mb,
            log_rotate_daily,
            log_retention_days,
            log_retention_files,
        };
        apply_step(document.set_server_settings(&input))
    })?;
    Ok(offered_text
        .filter(|offered| *offered == chosen_log_file)
        .and(offered_log_file.and_then(Path::parent).map(Path::to_path_buf)))
}

/// Asks for each meaningful `[server.ui]` appearance key, prefilling the
/// current value; a blank answer clears the key (falling back to the
/// rendered default).
fn configure_appearance(
    interviewer: &mut dyn WebInterviewer,
    document: &mut WebConfigDocument,
) -> Result<(), WebInterviewError> {
    if !interviewer.confirm(
        "Configure the web UI appearance (theme, font, size, system name)?",
        false,
    )? {
        return Ok(());
    }

    let mut answers: Vec<(&str, String)> = Vec::with_capacity(APPEARANCE_KEYS.len());
    for key in APPEARANCE_KEYS {
        let current = document.ui_string(key);
        let default = match (key, current.as_deref()) {
            ("system_name", None) => Some(DEFAULT_SYSTEM_NAME.to_owned()),
            (_, other) => other.map(str::to_owned),
        };
        let prompt = match key {
            "system_name" => "System name shown in the shell header".to_owned(),
            other => format!("Appearance: {other} (blank to leave unset)"),
        };
        answers.push((key, interviewer.ask(&prompt, default.as_deref())?));
    }

    let values: Vec<(&str, Option<&str>)> = answers.iter().map(|(key, answer)| (*key, optional(answer))).collect();
    document.set_ui_strings(&values)?;
    Ok(())
}

/// The `[[mcp_server]]` section: offers each existing entry back to keep,
/// edit, or remove, then adds new entries. At least one entry is required
/// when the file has none (a web instance with no MCP server can serve
/// nothing); adding more is an optional follow-up otherwise, matching
/// `contextos config`'s own vault logic.
fn configure_mcp_servers(
    interviewer: &mut dyn WebInterviewer,
    document: &mut WebConfigDocument,
) -> Result<McpChanges, WebInterviewError> {
    let mut changes = McpChanges::default();
    let existing = document.mcp_server_entries();

    for entry in &existing {
        let name = entry.name().to_owned();
        match interviewer.choose(&format!("MCP server {name:?}"), &["Keep", "Edit", "Remove"], 0)? {
            1 => {
                let updated = edit_one_mcp_server(interviewer, document, entry)?;
                changes.edited.push(updated);
            }
            2 => {
                document.remove_mcp_server(&name)?;
                changes.removed.push(name);
            }
            // `0` is Keep; any out-of-range index (structurally impossible
            // from the terminal implementation) also keeps the entry rather
            // than falling through to a destructive default.
            _ => {}
        }
    }

    if existing.is_empty() {
        loop {
            changes.added.push(add_one_mcp_server(interviewer, document, None)?);
            if !interviewer.confirm("Add another MCP server?", false)? {
                break;
            }
        }
    } else {
        while interviewer.confirm("Add a new MCP server?", false)? {
            changes.added.push(add_one_mcp_server(interviewer, document, None)?);
        }
    }

    Ok(changes)
}

/// Asks for one new `[[mcp_server]]` entry and appends it, re-prompting on a
/// rejected result (a duplicate name, a malformed endpoint). `prefill`
/// carries an existing entry's values when this is an edit rather than a
/// fresh add.
fn add_one_mcp_server(
    interviewer: &mut dyn WebInterviewer,
    document: &mut WebConfigDocument,
    prefill: Option<&McpServerConfig>,
) -> Result<String, WebInterviewError> {
    let mut chosen_name: Option<String> = None;
    retry_until_valid(interviewer, "MCP server entry", |interviewer| {
        let entry = ask_one_mcp_server(interviewer, prefill)?;
        let name = mcp_entry_name(&entry).to_owned();
        let result = match prefill {
            Some(existing) => document.replace_mcp_server(existing.name(), &entry),
            None => document.add_mcp_server(&entry),
        };
        apply_step(result)?;
        chosen_name = Some(name);
        Ok(())
    })?;
    chosen_name.ok_or_else(|| {
        WebInterviewError::Prompt(inquire::InquireError::Custom(
            "the MCP server step reported success without capturing a name".into(),
        ))
    })
}

/// The edit path for an existing `[[mcp_server]]` entry: same questions as
/// [`add_one_mcp_server`], every field prefilled from `current`, written
/// back with [`WebConfigDocument::replace_mcp_server`] so a rename is
/// handled.
fn edit_one_mcp_server(
    interviewer: &mut dyn WebInterviewer,
    document: &mut WebConfigDocument,
    current: &McpServerConfig,
) -> Result<String, WebInterviewError> {
    add_one_mcp_server(interviewer, document, Some(current))
}

/// Asks the transport-specific questions for one `[[mcp_server]]` entry and
/// returns the JSON object [`WebConfigDocument`] expects.
fn ask_one_mcp_server(
    interviewer: &mut dyn WebInterviewer,
    prefill: Option<&McpServerConfig>,
) -> Result<JsonMap<String, JsonValue>, WebInterviewError> {
    let default_transport = match prefill {
        Some(McpServerConfig::Http { .. }) => 1,
        _ => 0,
    };
    let transport = interviewer.choose("Transport", &TRANSPORTS, default_transport)?;

    let name_default = prefill.map(McpServerConfig::name).map(str::to_owned);
    let name = interviewer.ask("Name", name_default.as_deref())?;

    let entry = if transport == 1 {
        let endpoint_default = match prefill {
            Some(McpServerConfig::Http { endpoint, .. }) => Some(endpoint.clone()),
            _ => None,
        };
        let token_default = match prefill {
            Some(McpServerConfig::Http { token_env, .. }) => token_env.clone(),
            _ => None,
        };
        let endpoint = interviewer.ask("HTTP endpoint URL", endpoint_default.as_deref())?;
        let token_env = interviewer.ask(
            "Bearer-token environment variable name (blank for none)",
            token_default.as_deref(),
        )?;
        let mut object = json!({ "transport": "http", "name": name, "endpoint": endpoint });
        if !token_env.is_empty() {
            insert_string(&mut object, "token_env", &token_env);
        }
        object
    } else {
        let command_default = if let Some(McpServerConfig::Stdio { command, .. }) = prefill {
            Some(command.clone())
        } else {
            notify_default_config_detection(interviewer);
            Some("contextos".to_owned())
        };
        let args_default = match prefill {
            Some(McpServerConfig::Stdio { args, .. }) => args.join(", "),
            _ => String::new(),
        };
        let command = parse_manual_path(&interviewer.ask("Command to run", command_default.as_deref())?);
        let args_raw = interviewer.ask("Arguments (comma-separated)", optional(&args_default))?;
        let args: Vec<JsonValue> = args_raw
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| JsonValue::String(parse_manual_path(part)))
            .collect();
        json!({ "transport": "stdio", "name": name, "command": command, "args": args })
    };

    entry
        .as_object()
        .cloned()
        .ok_or_else(|| WebInterviewError::Prompt(inquire::InquireError::Custom("built a non-object MCP entry".into())))
}

/// Which vaults/servers the `[[mcp_server]]` walk touched, for the report.
#[derive(Debug, Default)]
struct McpChanges {
    added: Vec<String>,
    edited: Vec<String>,
    removed: Vec<String>,
}

/// A step failure inside [`retry_until_valid`]: either a validation
/// rejection the operator can correct by answering again, or a fatal error
/// (operator cancellation, a value out of range, an unexpected writer
/// error) that ends the wizard.
enum StepError {
    Rejected(String),
    Fatal(WebInterviewError),
}

impl From<WebInterviewError> for StepError {
    fn from(error: WebInterviewError) -> Self {
        Self::Fatal(error)
    }
}

/// Maps a [`WebConfigDocument`] mutation result into a [`StepError`]: a
/// [`WebConfigWriterError::Invalid`] is a correctable rejection (its reason
/// is shown and the step re-runs); anything else is fatal.
fn apply_step(result: Result<(), WebConfigWriterError>) -> Result<(), StepError> {
    match result {
        Ok(()) => Ok(()),
        Err(WebConfigWriterError::Invalid { source }) => Err(StepError::Rejected(source.to_string())),
        Err(other) => Err(StepError::Fatal(WebInterviewError::Writer(other))),
    }
}

/// Runs `step` until it succeeds, re-prompting (with the rejection reason
/// shown) after a correctable failure, up to [`MAX_ATTEMPTS`] times. A fatal
/// step error, or exhausting the attempts, ends the wizard.
fn retry_until_valid(
    interviewer: &mut dyn WebInterviewer,
    section: &'static str,
    mut step: impl FnMut(&mut dyn WebInterviewer) -> Result<(), StepError>,
) -> Result<(), WebInterviewError> {
    for _attempt in 0..MAX_ATTEMPTS {
        match step(interviewer) {
            Ok(()) => return Ok(()),
            Err(StepError::Rejected(reason)) => {
                interviewer.notify(&format!("That was rejected: {reason}. Please try again."));
            }
            Err(StepError::Fatal(error)) => return Err(error),
        }
    }
    Err(WebInterviewError::TooManyAttempts { section })
}

/// Asks for an optional whole number, re-prompting on a non-numeric answer.
/// A blank answer is `None`.
fn ask_optional_number(
    interviewer: &mut dyn WebInterviewer,
    prompt: &str,
    default: &str,
) -> Result<Option<u64>, WebInterviewError> {
    for _attempt in 0..MAX_ATTEMPTS {
        let raw = interviewer.ask(prompt, optional(default))?;
        if raw.is_empty() {
            return Ok(None);
        }
        match raw.parse::<u64>() {
            Ok(number) => return Ok(Some(number)),
            Err(_source) => interviewer.notify(&format!("{raw:?} is not a whole number. Please try again.")),
        }
    }
    Err(WebInterviewError::TooManyAttempts {
        section: "numeric answer",
    })
}

/// Narrows a `u64` answer to the `u32` the retention keys take, rejecting an
/// out-of-range value rather than truncating it.
fn narrow_u32(value: u64) -> Result<u32, WebInterviewError> {
    u32::try_from(value).map_err(|_source| WebInterviewError::NumberOutOfRange { value })
}

/// `Some(text)` unless `text` is empty, in which case `None` (an unset
/// optional key).
fn non_empty(text: String) -> Option<String> {
    if text.is_empty() { None } else { Some(text) }
}

/// A prompt default: `None` (no prefill) when `text` is empty, so `inquire`
/// does not show an empty default.
fn optional(text: &str) -> Option<&str> {
    if text.is_empty() { None } else { Some(text) }
}

/// Strips one layer of matching surrounding quotes (`"..."` or `'...'`) from
/// a manually-entered path-like answer (a static asset directory, a log
/// file, or an MCP server `command`/argument), mirroring `contextos
/// config`'s own `parse_manual_path` (`contextos-mcp/src/interview.rs`): a
/// path pasted from Windows Explorer's "Copy as path" arrives wrapped in
/// double quotes, which otherwise become part of the stored value and later
/// get quoted a second time wherever that value is embedded in a generated
/// command line (`contextos-service`'s Windows quoting) or passed straight
/// to `Command::new` (`mcp_client.rs`), so the literal quote characters make
/// the path resolve as nonexistent.
fn parse_manual_path(raw: &str) -> String {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| trimmed.strip_prefix('\'').and_then(|rest| rest.strip_suffix('\'')))
        .unwrap_or(trimmed)
        .to_owned()
}

/// Tells the operator, immediately before defaulting a fresh MCP server
/// entry's command to bare `"contextos"`, whether `contextos config`'s own
/// default `config.toml` location already has a file: a `contextos` command
/// run with no `--config` argument resolves that identical default location
/// itself (`contextos-mcp::default_config_path`), so an operator who has
/// already run `contextos config` does not need to find and pass its path
/// by hand here.
fn notify_default_config_detection(interviewer: &mut dyn WebInterviewer) {
    let Some(path) = default_mcp_config_toml_path() else {
        return;
    };
    notify_config_detection_at(interviewer, &path);
}

/// The message-wording half of [`notify_default_config_detection`], taking
/// the candidate path as a parameter so it is unit-testable against a
/// temporary file rather than the operator's real home directory.
fn notify_config_detection_at(interviewer: &mut dyn WebInterviewer, path: &std::path::Path) {
    if path.is_file() {
        interviewer.notify(&format!(
            "Detected an existing ContextOS configuration at {}; a bare \"contextos\" command with no \
             arguments will use it automatically.",
            path.display()
        ));
    } else {
        interviewer.notify(&format!(
            "No ContextOS configuration file found at the default location ({}); run `contextos config` \
             first, or pass --config <path> as an argument below.",
            path.display()
        ));
    }
}

/// Mirrors `contextos-mcp`'s own `default_config_path` (`main.rs`): the
/// fallback `config.toml` location `contextos config` writes to when no
/// `--config` or `CONTEXTOS_MCP_CONFIG` override is given. Reimplemented
/// here rather than shared, since `contextos-web` must not depend on
/// `contextos-mcp` in production; this only
/// checks for the file's existence to word an informational prompt, so
/// drift between the two copies can under- or over-report the detection
/// message but can never corrupt configuration.
fn default_mcp_config_toml_path() -> Option<PathBuf> {
    let folders = contextos_host::HostFolders::detect()?;
    Some(folders.home().join(".config").join("contextos").join("config.toml"))
}

/// The string form of an optional integer setting, for a prompt default: an
/// empty string when unset.
fn optional_number_default(value: Option<i64>) -> String {
    value.map(|number| number.to_string()).unwrap_or_default()
}

/// The index of `level` in [`LOG_LEVELS`], defaulting to `info` for an
/// unrecognised value.
fn level_position(level: &str) -> usize {
    LOG_LEVELS.iter().position(|candidate| *candidate == level).unwrap_or(2)
}

/// The `name` field of a JSON MCP entry, for the report.
fn mcp_entry_name(entry: &JsonMap<String, JsonValue>) -> &str {
    entry.get("name").and_then(JsonValue::as_str).unwrap_or_default()
}

/// Inserts a string key into a JSON object value, a no-op when `object` is
/// not an object (structurally unreachable here: the caller built it with
/// [`json!`]).
fn insert_string(object: &mut JsonValue, key: &str, value: &str) {
    if let Some(map) = object.as_object_mut() {
        map.insert(key.to_owned(), JsonValue::String(value.to_owned()));
    }
}

/// Typed failures running the `contextos-web config` wizard.
#[derive(Debug, Error)]
pub enum WebInterviewError {
    #[error("the operator cancelled the setup wizard: {0}")]
    Prompt(#[from] inquire::InquireError),
    #[error("web configuration file could not be read: {path}", path = path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the log file's folder could not be created: {path}", path = path.display())]
    LogDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("web configuration file could not be written: {path}", path = path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Writer(#[from] WebConfigWriterError),
    #[error("the {section} were still invalid after {MAX_ATTEMPTS} attempts; nothing was written")]
    TooManyAttempts { section: &'static str },
    #[error("{value} is too large for this setting")]
    NumberOutOfRange { value: u64 },
}

#[cfg(test)]
#[path = "config_interview_test.rs"]
mod tests;
