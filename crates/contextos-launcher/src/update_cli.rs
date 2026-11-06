//! `contextos-launcher` `install`, `update`, and `rollback`: the self-update composition root for this product, using
//! `updater-core`/`updater-launcher`/`updater-engine` directly with `contextos`-specific defaults baked in (product
//! name, declared commands, default update URL, and the production trusted signing key).

use std::collections::HashSet;
use std::error::Error;
use std::io::Write;

use crate::commands::{refresh_launchers, set_up_commands};
use updater_core::{ActivationPort, ActiveVersionReader, ProductVersion};
use updater_engine::{
    Ed25519ManifestVerifier, FilesystemActivationPort, InstallCollaborators, SpawnVersionProbe, SystemClock,
    TrustedKey, UpdateContext, UreqTransport, check_for_update, cleanup_obsolete_versions, detect_target,
    install_update, read_state, rollback as engine_rollback,
};
use updater_launcher::{ProcessEnvironment, resolve_installation_root};

pub(crate) type CliResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

pub(crate) const PRODUCT: &str = "contextos";
pub(crate) const ENV_PREFIX: &str = "CONTEXTOS";
const DECLARED_COMMANDS: [&str; 2] = ["contextos", "contextos-web"];
const DEFAULT_UPDATE_URL: &str = "https://github.com/zentelligence/contextos/releases/latest/download/latest.json";

/// The production release-signing key generated for this repository. Rotate by adding a second
/// `TrustedKey` here (or via `CONTEXTOS_TRUSTED_KEYS`) alongside this one, never by replacing it
/// outright, so releases signed under the old key remain verifiable during the rotation window.
const PRODUCTION_KEY_ID: &str = "release-2026";
const PRODUCTION_PUBLIC_KEY_HEX: &str = "4261f02366aa56d06e176e5de45a325926c4d8745b60b0b3d32179197fd06606";

fn manifest_url() -> String {
    std::env::var(format!("{ENV_PREFIX}_UPDATE_URL")).unwrap_or_else(|_| DEFAULT_UPDATE_URL.to_string())
}

fn trusted_keys() -> CliResult<Vec<TrustedKey>> {
    let production_bytes =
        decode_hex_32(PRODUCTION_PUBLIC_KEY_HEX).ok_or("the embedded production public key is malformed")?;
    let mut keys = vec![TrustedKey::new(PRODUCTION_KEY_ID, &production_bytes)?];

    if let Ok(raw) = std::env::var(format!("{ENV_PREFIX}_TRUSTED_KEYS")) {
        for entry in raw.split(',').map(str::trim).filter(|entry| !entry.is_empty()) {
            let (key_id, hex) = entry
                .split_once(':')
                .ok_or_else(|| format!("malformed {ENV_PREFIX}_TRUSTED_KEYS entry: {entry:?}"))?;
            let bytes =
                decode_hex_32(hex).ok_or_else(|| format!("malformed {ENV_PREFIX}_TRUSTED_KEYS entry: {entry:?}"))?;
            keys.push(TrustedKey::new(key_id, &bytes)?);
        }
    }
    Ok(keys)
}

fn decode_hex_32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

fn declared_commands() -> Vec<String> {
    DECLARED_COMMANDS.iter().map(ToString::to_string).collect()
}

fn display_optional(version: Option<&ProductVersion>) -> String {
    version.map_or_else(|| "none".to_string(), ToString::to_string)
}

/// `contextos-launcher update check`: reports whether a newer, verified release set is available.
///
/// # Errors
///
/// Returns an error if the installation root cannot be resolved, the manifest cannot be retrieved
/// or verified, or no asset matches the detected target.
pub(crate) fn run_check() -> CliResult<String> {
    let root = resolve_installation_root(&ProcessEnvironment, ENV_PREFIX, PRODUCT)?;
    let target = detect_target()?;
    let url = manifest_url();
    let commands = declared_commands();
    let context = UpdateContext {
        product: PRODUCT,
        installation_root: root.as_path(),
        manifest_url: &url,
        target: &target,
        declared_public_commands: &commands,
        retain_versions: 2,
    };

    let transport = UreqTransport::default();
    let verifier = Ed25519ManifestVerifier::new(trusted_keys()?);
    let activation = FilesystemActivationPort::new(root.as_path().to_path_buf());

    let outcome = check_for_update(&transport, &verifier, &activation, &context)?;
    Ok(match (&outcome.active_version, outcome.update_available) {
        (None, _) => format!(
            "No release set is installed yet. Run 'contextos-launcher install' to install {}.",
            outcome.manifest.version
        ),
        (Some(active), true) => format!(
            "Update available: {}, current release set: {active}.\nRun 'contextos-launcher update' to install release set {}.",
            outcome.manifest.version, outcome.manifest.version
        ),
        (Some(active), false) => format!("Release set {active} is already up to date."),
    })
}

/// `contextos-launcher update` / `contextos-launcher update --yes`: performs the full install transaction.
///
/// # Errors
///
/// Returns an error if the manifest cannot be retrieved or verified, the download or checksum
/// fails, the release set fails validation, or activation fails.
pub(crate) fn run_update(yes: bool) -> CliResult<String> {
    if !yes && !confirm("Install this update?") {
        return Ok("Update cancelled.".to_string());
    }

    let root = resolve_installation_root(&ProcessEnvironment, ENV_PREFIX, PRODUCT)?;
    let target = detect_target()?;
    let url = manifest_url();
    let commands = declared_commands();
    let context = UpdateContext {
        product: PRODUCT,
        installation_root: root.as_path(),
        manifest_url: &url,
        target: &target,
        declared_public_commands: &commands,
        retain_versions: 2,
    };

    let transport = UreqTransport::default();
    let verifier = Ed25519ManifestVerifier::new(trusted_keys()?);
    let probe = SpawnVersionProbe;
    let activation = FilesystemActivationPort::new(root.as_path().to_path_buf());
    let clock = SystemClock;
    let collaborators = InstallCollaborators {
        transport: &transport,
        verifier: &verifier,
        probe: &probe,
        activation: &activation,
        clock: &clock,
    };

    let installed = install_update(&collaborators, &context, false)?;
    let mut lines = vec![
        format!("Installed release set {installed}."),
        format!("All new command invocations will now use {installed}."),
    ];
    lines.extend(refresh_launchers(root.as_path(), &installed.to_string())?);
    Ok(lines.join("\n"))
}

/// `contextos-launcher install`: installs the channel's current release set and sets up the `contextos` and `contextos-web`
/// commands. `--version` pins the release for CI and automation; it is only satisfiable when it matches the channel's
/// current manifest, since this product publishes one "latest" manifest per channel. Running it again sets the
/// commands up again without reinstalling.
///
/// # Errors
///
/// Returns an error if the requested version is malformed, does not match the channel's current
/// manifest, the install transaction itself fails, or the commands cannot be set up.
pub(crate) fn run_install(requested_version: Option<String>) -> CliResult<String> {
    let requested = requested_version.map(ProductVersion::try_from).transpose()?;

    let root = resolve_installation_root(&ProcessEnvironment, ENV_PREFIX, PRODUCT)?;
    let target = detect_target()?;
    let url = manifest_url();
    let commands = declared_commands();
    let context = UpdateContext {
        product: PRODUCT,
        installation_root: root.as_path(),
        manifest_url: &url,
        target: &target,
        declared_public_commands: &commands,
        retain_versions: 2,
    };

    let transport = UreqTransport::default();
    let verifier = Ed25519ManifestVerifier::new(trusted_keys()?);
    let probe = SpawnVersionProbe;
    let activation = FilesystemActivationPort::new(root.as_path().to_path_buf());
    let clock = SystemClock;

    let outcome = check_for_update(&transport, &verifier, &activation, &context)?;
    if let Some(requested) = requested
        && outcome.manifest.version != requested
    {
        return Err(format!(
            "pinned version {requested} is not available; the channel currently offers {}",
            outcome.manifest.version
        )
        .into());
    }

    let installed = if outcome.active_version.as_ref() == Some(&outcome.manifest.version) {
        outcome.manifest.version
    } else {
        let collaborators = InstallCollaborators {
            transport: &transport,
            verifier: &verifier,
            probe: &probe,
            activation: &activation,
            clock: &clock,
        };
        install_update(&collaborators, &context, true)?
    };

    let commands_report = set_up_commands(root.as_path(), &installed.to_string())?;
    Ok(format!(
        "Installed ContextOS {installed}.\n{commands_report}\nOpen a new terminal, then run 'contextos config' to set up your vault."
    ))
}

/// `contextos-launcher rollback`: switches the whole release set back to the previous one.
///
/// # Errors
///
/// Returns an error if there is no previous release set, or its release metadata is missing.
pub(crate) fn run_rollback() -> CliResult<String> {
    let root = resolve_installation_root(&ProcessEnvironment, ENV_PREFIX, PRODUCT)?;
    let activation = FilesystemActivationPort::new(root.as_path().to_path_buf());
    let versions_dir = root.as_path().join("versions");

    let from = activation.active_version()?;
    let to = engine_rollback(&activation, &versions_dir)?;
    Ok(match from {
        Some(from) => {
            format!("Rolled back release set from {from} to {to}.\nAll new command invocations will now use {to}.")
        }
        None => format!("Rolled back to release set {to}.\nAll new command invocations will now use {to}."),
    })
}

/// `contextos-launcher update cleanup`: best-effort deletion of obsolete release sets.
///
/// # Errors
///
/// Returns an error if the installation root or its active/previous pointers cannot be read.
pub(crate) fn run_cleanup() -> CliResult<String> {
    let root = resolve_installation_root(&ProcessEnvironment, ENV_PREFIX, PRODUCT)?;
    let activation = FilesystemActivationPort::new(root.as_path().to_path_buf());
    let versions_dir = root.as_path().join("versions");

    let mut keep = HashSet::new();
    if let Some(active) = activation.active_version()? {
        keep.insert(active);
    }
    if let Some(previous) = activation.previous_version()? {
        keep.insert(previous);
    }

    let deferred = cleanup_obsolete_versions(&versions_dir, &keep);
    if deferred.is_empty() {
        return Ok("Cleanup complete; no obsolete release sets remain.".to_string());
    }
    let mut lines =
        vec!["Cleanup completed; some release sets are still in use and will be retried later:".to_string()];
    for item in deferred {
        lines.push(format!("  {} ({})", item.version, item.reason));
    }
    Ok(lines.join("\n"))
}

/// `contextos-launcher update diagnostics`: a structured, human-readable summary of the installation.
///
/// # Errors
///
/// Returns an error if the installation root, its pointers, or `state.json` cannot be read.
pub(crate) fn run_diagnostics() -> CliResult<String> {
    let root = resolve_installation_root(&ProcessEnvironment, ENV_PREFIX, PRODUCT)?;
    let target = detect_target()?;
    let activation = FilesystemActivationPort::new(root.as_path().to_path_buf());

    let active = activation.active_version()?;
    let previous = activation.previous_version()?;
    let state = read_state(root.as_path())?;

    let mut lines = vec![
        format!("product: {PRODUCT}"),
        format!("installation_root: {}", root.as_path().display()),
        format!("detected_target: {target}"),
        format!("active_version: {}", display_optional(active.as_ref())),
        format!("previous_version: {}", display_optional(previous.as_ref())),
    ];
    match state {
        Some(state) => {
            lines.push(format!("channel: {}", state.channel));
            let installed = state
                .installed_versions
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("installed_versions: {installed}"));
            lines.push(format!(
                "last_check_at: {}",
                state.last_check_at.as_deref().unwrap_or("never")
            ));
            lines.push(format!(
                "last_successful_update_at: {}",
                state.last_successful_update_at.as_deref().unwrap_or("never")
            ));
            lines.push(format!(
                "last_update_error: {}",
                state.last_update_error.as_deref().unwrap_or("none")
            ));
        }
        None => lines.push("state: no state.json recorded yet".to_string()),
    }
    Ok(lines.join("\n"))
}

pub(crate) fn confirm(prompt: &str) -> bool {
    print!("{prompt} [y/N]: ");
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
#[path = "update_cli_test.rs"]
mod tests;
