//! The parts of `vault_info`'s result that contextos-web uses, read once, in one place: each vault's name and path,
//! and the time zone the server reports. Every reader of that tool goes through [`McpClient::vault_info`], so a
//! refusal or an answer of the wrong shape is told apart the same way everywhere. Fields this crate does not use are
//! ignored, so the server can add to its result without breaking the web.

use std::path::PathBuf;

use rmcp::model::CallToolResult;
use serde::Deserialize;
use serde_json::Map;
use thiserror::Error;

use crate::mcp_client::{McpCallError, McpClient};

/// Why `vault_info` could not be read.
#[derive(Debug, Error)]
pub enum VaultInfoError {
    /// The server could not be asked, or answered with a protocol error.
    #[error("vault_info could not be asked of the server")]
    Call(#[from] McpCallError),
    /// The server answered the call with a tool error.
    #[error("the server refused vault_info")]
    Refused,
    /// The server's answer is not a report of vaults.
    #[error("the server's answer to vault_info is not a report of vaults")]
    Unparsable,
}

/// One vault as `vault_info` reports it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ReportedVault {
    /// The vault's name.
    pub name: String,
    /// Where the vault is on the server's host, when the server reports it.
    #[serde(default)]
    pub path: Option<PathBuf>,
}

/// The time zone the server reports: an IANA name, or for a host zone with none, POSIX rules.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ReportedZone {
    /// The IANA name, when the zone has one.
    #[serde(default)]
    pub name: Option<String>,
    /// The POSIX rules, when the zone has no name.
    #[serde(default)]
    pub rules: Option<String>,
}

/// What `vault_info` reports, as far as contextos-web uses it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct VaultInfoReport {
    /// Every configured vault, in the server's order.
    pub vaults: Vec<ReportedVault>,
    /// The server's time zone, when it reports one.
    #[serde(default)]
    pub time_zone: Option<ReportedZone>,
}

impl TryFrom<CallToolResult> for VaultInfoReport {
    type Error = VaultInfoError;

    fn try_from(result: CallToolResult) -> Result<Self, Self::Error> {
        if result.is_error == Some(true) {
            return Err(VaultInfoError::Refused);
        }
        result.into_typed::<Self>().map_err(|_| VaultInfoError::Unparsable)
    }
}

impl McpClient {
    /// Asks this session's server for `vault_info` and reads the parts contextos-web uses.
    ///
    /// # Errors
    ///
    /// [`VaultInfoError::Call`] when the server cannot be asked, [`VaultInfoError::Refused`] when it answers with a
    /// tool error, and [`VaultInfoError::Unparsable`] when its answer is not a report of vaults.
    pub async fn vault_info(&self) -> Result<VaultInfoReport, VaultInfoError> {
        let result = self.call_tool("vault_info".to_owned(), Map::new()).await?;
        VaultInfoReport::try_from(result)
    }
}

#[cfg(test)]
#[path = "vault_info_test.rs"]
mod tests;
