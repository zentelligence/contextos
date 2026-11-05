//! Which registered apps still name an `[[mcp_server]]` entry, so a removal or rename that would break one is refused.

use crate::apps::{self, RegisteredApp};
use crate::mcp_client::{McpCallError, McpClient};
use crate::vault_info::VaultInfoError;

/// An app that names a server being removed.
pub(super) struct DependencyConflict {
    pub(super) vault: String,
    pub(super) slug: String,
    pub(super) server: String,
}

/// Why the dependent apps of a removed server could not be found.
#[derive(Debug, thiserror::Error)]
pub(super) enum DependencyCheckError {
    /// An MCP call the check makes failed.
    #[error(transparent)]
    Call(#[from] McpCallError),
    /// The server refused `vault_info` or answered it with something that is not a vault list, so which vaults to
    /// search is unknown.
    #[error("the vault list could not be read, so dependent apps could not be checked")]
    VaultListUnreadable,
}

/// Lists every configured vault's `name` via `vault_info`, the same MCP
/// tool call an operator would use, rather than `contextos-web` maintaining
/// a second, locally pre-loaded vault registry (the same established
/// precedent the vault content routes already follow, extended here to
/// this check).
async fn configured_vault_names(client: &McpClient) -> Result<Vec<String>, DependencyCheckError> {
    match client.vault_info().await {
        Ok(report) => Ok(report.vaults.into_iter().map(|vault| vault.name).collect()),
        Err(VaultInfoError::Call(error)) => Err(DependencyCheckError::Call(error)),
        Err(VaultInfoError::Refused | VaultInfoError::Unparsable) => Err(DependencyCheckError::VaultListUnreadable),
    }
}

/// For every configured vault, discovers its registered apps (using
/// `known_before`, the server list as it stood *before* this edit, so a
/// currently-valid app's registration is not spuriously invalidated by the
/// very edit under validation) and reports every `(vault, app, server)`
/// triple where the app's manifest names one of `removed`.
pub(super) async fn dependent_apps(
    client: &McpClient,
    known_before: &[String],
    removed: &[String],
) -> Result<Vec<DependencyConflict>, DependencyCheckError> {
    let vault_names = configured_vault_names(client).await?;
    let mut conflicts = Vec::new();
    for vault_name in vault_names {
        let apps: Vec<RegisteredApp> = apps::discover_apps(client, &vault_name, known_before).await?;
        for app in apps {
            for server in &app.mcp_servers {
                if removed.iter().any(|name| name == server) {
                    conflicts.push(DependencyConflict {
                        vault: vault_name.clone(),
                        slug: app.slug.clone(),
                        server: server.clone(),
                    });
                }
            }
        }
    }
    Ok(conflicts)
}

#[cfg(test)]
#[path = "settings_dependents_test.rs"]
mod tests;
