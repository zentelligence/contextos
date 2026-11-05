//! Shared helpers for `contextos-web`'s integration tests, none of which is
//! itself an integration test target (this file lives under `tests/support/`
//! rather than directly under `tests/`, matching the standard Cargo
//! convention for test-only shared code).
//!
//! `contextos-web` never depends on `contextos-mcp` in production, so
//! `CARGO_BIN_EXE_contextos` is not available here: Cargo only sets that
//! variable for a binary target owned by the package currently under test,
//! not a sibling workspace package's binary. [`contextos_mcp_binary`]
//! resolves the real `contextos` binary another way: it asks Cargo to build
//! it (a fast no-op if it is already up to date) and locates it in the
//! shared workspace `target/` directory, so these contract tests exercise a
//! real `contextos-mcp` stdio session, per `testing.md`'s own "no mocking"
//! standard, without a production dependency edge back onto it.
//!
//! Not itself a test target: as a `tests/` subdirectory module it is compiled once per including test crate rather
//! than as its own binary. Some helpers are used by only one sibling test file, so each `use` site imports what it
//! needs; `#![allow(dead_code)]` keeps the unused-in-this-crate items from warning.
#![allow(dead_code)]

pub mod binary_build;
pub mod events;
pub mod jobs;
pub mod logs;
#[path = "../../src/release_fixture.rs"]
pub mod release_fixture;
pub mod shared;

use std::fmt::Write as _;
use std::path::PathBuf;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Whether a folder made read-only can still be written to, which is the case for a process running as root. A test that
/// relies on an unwritable folder returns early when it is, because the failure it needs cannot be made.
#[cfg(unix)]
pub fn directory_permissions_are_ignored() -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt as _;
    let folder = tempfile::tempdir()?;
    std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o555))?;
    let writable = std::fs::File::create(folder.path().join("probe")).is_ok();
    std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o755))?;
    Ok(writable)
}

/// Runs a saved vault job to completion and returns only how it ended, which is what most run tests assert. The
/// command line's own entry point also reports the run's record, outputs, and registration.
pub async fn run_job(
    clients: std::sync::Arc<contextos_web::McpClientSet>,
    host: &contextos_web::run_settings::HostContext,
    vault_name: &str,
    package: &str,
    workflow_slug: &str,
    slug: &str,
    options: contextos_web::cli_run::CliRunOptions,
) -> Result<contextos_workflows::RunOutcome, contextos_web::cli_run::CliRunError> {
    contextos_web::cli_run::run_job_reported(
        clients,
        host,
        vault_name,
        &contextos_workflows::JobKey {
            package: package.to_owned(),
            workflow: workflow_slug.to_owned(),
            job: slug.to_owned(),
        },
        options,
        contextos_web::run_report::RunControls::default(),
    )
    .await
    .map(|report| report.outcome)
}

/// Runs a vault workflow once and returns only how it ended.
pub async fn run_workflow_once(
    clients: std::sync::Arc<contextos_web::McpClientSet>,
    primary_server: &str,
    once: contextos_web::cli_run::OneOffVaultRun,
) -> Result<contextos_workflows::RunOutcome, contextos_web::cli_run::CliRunError> {
    contextos_web::cli_run::run_workflow_once_reported(
        clients,
        primary_server,
        once,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .map(|report| report.outcome)
}

/// The host a run is started with: the first server, no log level set, and job claims in a `claims` directory under
/// `directory`, so two tests never share a claim.
pub fn host(directory: &std::path::Path) -> contextos_web::run_settings::HostContext {
    contextos_web::run_settings::HostContext {
        attention: contextos_web::job_attention::JobAttentionMemory::default(),
        log_level: None,
        claims_root: Some(directory.join("claims")),
        // No server to ask, so this host's zone: no test reading this host's clock depends on the zone.
        clock: std::sync::Arc::new(contextos_web::vault_time::VaultTime::from(
            contextos_web::vault_time::VaultTimeInput {
                clock: std::sync::Arc::new(contextos_workflows::SystemClock),
                clients: std::sync::Arc::new(contextos_web::McpClientSet::default()),
            },
        )),
    }
}

/// Builds (a fast no-op if already up to date) and returns the path to the real `contextos` binary `contextos-mcp`
/// produces. The build is started once per test process, however many tests ask, and a failure says how cargo ended and
/// what it said last.
pub fn contextos_mcp_binary() -> Result<PathBuf, BoxError> {
    static BUILT: binary_build::Built = binary_build::Built::new();
    // Built in the profile these tests were built in, and looked for where that profile puts it.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    Ok(binary_build::built_once(
        &BUILT,
        binary_build::Cargo {
            program: &cargo,
            leading: &[],
        },
        !cfg!(debug_assertions),
        &target_dir(),
    )?)
}

fn target_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return PathBuf::from(dir);
    }
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop(); // crates/contextos-web -> crates
    path.pop(); // crates -> workspace root
    path.push("target");
    path
}

/// A vault root under `parent` whose own basename is a plain, always-scheme-valid name
/// (`"vault"`), never `tempfile::tempdir()`'s own dot-prefixed basename: `contextos-search`'s
/// full-text sync walk builds a throwaway single-root `VaultSet` directly from the vault root's
/// path (`contextos-search/src/sync.rs`'s `resolve_single_root_set`), which derives that
/// throwaway root's own name from the path's basename and fails outright when it starts with
/// `.`, as every `tempfile::tempdir()` path does. Only text-enabled search tests need this;
/// every other test may keep using `tempdir()`'s own path directly.
pub fn named_vault_dir(parent: &std::path::Path) -> std::io::Result<PathBuf> {
    let path = parent.join("vault");
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// Writes a minimal, valid `config.toml` naming zero vaults: just enough for the real `contextos` binary to start
/// (`[server]` alone is a complete, valid document once zero configured vaults is accepted).
pub fn write_zero_vault_config(dir: &std::path::Path) -> std::io::Result<PathBuf> {
    let path = dir.join("config.toml");
    std::fs::write(&path, "[server]\ntransports = [\"stdio\"]\n")?;
    Ok(path)
}

/// Writes a minimal, valid `config.toml` naming one managed vault rooted at
/// `vault_dir`, and returns the config file's path.
pub fn write_vault_config(dir: &std::path::Path, vault_dir: &std::path::Path) -> std::io::Result<PathBuf> {
    write_vault_config_logging_to(dir, vault_dir, None)
}

/// As [`write_vault_config`], with the server's log written to `log_file` instead of stderr, so a test that makes the
/// server warn on purpose can read the warning back and assert it rather than let it reach the shared test output.
pub fn write_vault_config_logging_to(
    dir: &std::path::Path,
    vault_dir: &std::path::Path,
    log_file: Option<&std::path::Path>,
) -> std::io::Result<PathBuf> {
    let path = dir.join("config.toml");
    // `Debug`, not `Display`, is the correct formatting here: it quotes and
    // backslash-escapes the path the way a TOML string value requires,
    // which `.display()` (clippy's suggested alternative) would not do for
    // a path containing a backslash (Windows) or an embedded quote.
    #[allow(clippy::unnecessary_debug_formatting)]
    let vault_toml_value = format!("{vault_dir:?}");
    #[allow(clippy::unnecessary_debug_formatting)]
    let log_line = log_file.map_or_else(String::new, |file| format!("log_file = {file:?}\n"));
    std::fs::write(
        &path,
        // `[server] timezone` is fixed so every date a test reads is UTC's on any host, never the runner's own zone.
        format!(
            "[server]\ntimezone = \"UTC\"\n{log_line}\n[[vault]]\npath = {vault_toml_value}\nname = \"contract-fixture\"\n\
             state_directory = \".contextos\"\n\
             [vault.search]\ntext = false\ngraph = false\n\
             [vault.events]\nretention_days = 36500\n"
        ),
    )?;
    Ok(path)
}

/// Writes a `config.toml` naming one managed vault per `(name, root)` pair, and returns the config file's path.
pub fn write_named_vaults_config(
    dir: &std::path::Path,
    vaults: &[(&str, &std::path::Path)],
) -> std::io::Result<PathBuf> {
    let path = dir.join("config.toml");
    let mut text = String::new();
    for (name, root) in vaults {
        // `Debug` quotes and escapes the path as a TOML string value requires.
        #[allow(clippy::unnecessary_debug_formatting)]
        let root_value = format!("{root:?}");
        write!(
            text,
            "[[vault]]\npath = {root_value}\nname = {name:?}\nstate_directory = \".contextos\"\n\
             [vault.search]\ntext = false\ngraph = false\n\
             [vault.events]\nretention_days = 36500\n"
        )
        .map_err(std::io::Error::other)?;
    }
    std::fs::write(&path, text)?;
    Ok(path)
}

/// A `[[mcp_server]]` entry pointing at a real `contextos-mcp` instance
/// (stdio, the built binary) configured against `config_path`.
pub fn real_contextos_entry(
    name: &str,
    config_path: &std::path::Path,
) -> Result<contextos_web::McpServerConfig, BoxError> {
    Ok(contextos_web::McpServerConfig::Stdio {
        name: name.to_owned(),
        command: contextos_mcp_binary()?.to_string_lossy().into_owned(),
        args: vec!["--config".to_owned(), config_path.to_string_lossy().into_owned()],
    })
}
