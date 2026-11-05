//! Shared helpers for `contextos-mcp`'s own integration tests, none of
//! which is itself an integration test target (this file lives under
//! `tests/support/` rather than directly under `tests/`, matching the
//! standard Cargo convention for test-only shared code, and
//! `contextos-web`'s own identical `tests/support/mod.rs`).

pub mod doctor_output;
pub mod embedding_stub;
pub mod events_vault;
pub mod git_harness;
pub mod projection_doctor;
pub mod projection_world;
pub mod templated_vault;

use std::path::PathBuf;

use contextos_mcp::{Config, ConfigError};

/// As [`Config::try_from`]'s own `Vec<PathBuf>` conversion, with every
/// vault's `state_directory` pinned to `.contextos` under the vault root
/// itself rather than left at that conversion's own default. That default
/// (`state_dir.rs`'s own doc covers the production rationale: a
/// directory under the operator's home, `~/.contextos/vaults`, keyed by a
/// hash of the vault's resolved root, so index segments and locks never
/// sit somewhere a third-party sync tool could observe) is exactly right
/// for a real vault,
/// but wrong for a contract test's own throwaway `tempfile::tempdir()`
/// vault: every test run hashes to a distinct, permanent directory under
/// the operator's real home directory that nothing ever cleans up, since
/// the temp vault's own `Drop` has no reach into that separate location.
/// Scoping it back inside the vault's own tempdir instead means it is
/// deleted for free the same way the rest of the fixture is.
pub fn scoped_config(paths: Vec<PathBuf>) -> Result<Config, ConfigError> {
    let mut config = Config::try_from(paths)?;
    for vault in &mut config.vaults {
        vault.state_directory = Some(PathBuf::from(".contextos"));
    }
    Ok(config)
}
