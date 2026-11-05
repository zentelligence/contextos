//! Checked, optimistic snapshots for small vault-side TOML state files.
use crate::mcp_client::McpClient;
use crate::vault_content::{AttachedRead, read_attached};
use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Debug, thiserror::Error)]
pub enum FavouriteError {
    /// The server could not be asked at all, so nothing is known about the state file.
    #[error("Favourite state is unavailable. Check the vault connection and try again.")]
    Unreachable,
    /// The server answered but refused to read the state file.
    #[error("Favourite state is unavailable. Check the vault connection and try again.")]
    Unavailable,
    #[error("Favourite state is malformed. Correct the TOML before changing favourites.")]
    Malformed,
    #[error("Favourite state changed or the write was rejected. Reload and try again.")]
    Rejected,
}

impl FavouriteError {
    /// The stable code a response carries for this failure.
    pub(crate) const fn code(&self) -> &'static str {
        match self {
            Self::Unreachable | Self::Unavailable => "favourites/unavailable",
            Self::Malformed => "favourites/malformed",
            Self::Rejected => "favourites/rejected",
        }
    }
}

/// The status a response carries for a failure: the vault could not be reached, the stored state cannot be trusted,
/// or the write met a newer state.
impl From<&FavouriteError> for axum::http::StatusCode {
    fn from(error: &FavouriteError) -> Self {
        match error {
            FavouriteError::Unreachable | FavouriteError::Unavailable => Self::BAD_GATEWAY,
            FavouriteError::Malformed => Self::INTERNAL_SERVER_ERROR,
            FavouriteError::Rejected => Self::CONFLICT,
        }
    }
}

/// A checked snapshot: malformed, truncated, or unversioned reads are never writable.
#[derive(Default)]
pub(crate) struct Snapshot<T> {
    pub value: T,
    pub(crate) hash: Option<String>,
}
pub(crate) async fn read<T: serde::de::DeserializeOwned + Default>(
    client: &McpClient,
    vault: &str,
    path: &str,
) -> Result<Snapshot<T>, FavouriteError> {
    let read = match read_attached(client, &format!("{vault}://{path}"))
        .await
        .map_err(|_| FavouriteError::Unreachable)?
    {
        // A file that does not exist yet is an empty state to be created; any other refusal means the state cannot
        // be trusted, and must not be written over as though it were empty.
        AttachedRead::Refused { code } if code.as_deref() == Some("path/not-found") => {
            return Ok(Snapshot {
                value: T::default(),
                hash: None,
            });
        }
        AttachedRead::Refused { .. } => return Err(FavouriteError::Unavailable),
        AttachedRead::NotText => return Err(FavouriteError::Malformed),
        AttachedRead::Text(read) => read,
    };
    // A state with no hash could not be written back with a conflict check, so it is never offered for writing.
    let Some(hash) = read.content_hash.filter(|hash| !hash.is_empty()) else {
        return Err(FavouriteError::Malformed);
    };
    Ok(Snapshot {
        value: toml::from_str(&read.text).map_err(|_| FavouriteError::Malformed)?,
        hash: Some(hash),
    })
}

impl<T: Serialize> Snapshot<T> {
    pub(crate) async fn save(&self, client: &McpClient, vault: &str, path: &str) -> Result<(), FavouriteError> {
        let mut args = Map::new();
        args.insert("path".into(), Value::String(format!("{vault}://{path}")));
        args.insert(
            "content".into(),
            Value::String(toml::to_string(&self.value).map_err(|_| FavouriteError::Malformed)?),
        );
        if let Some(hash) = &self.hash {
            args.insert("expected_hash".into(), Value::String(hash.clone()));
        }
        let result = client
            .call_tool("fs_write_file".into(), args)
            .await
            .map_err(|_| FavouriteError::Unavailable)?;
        if result.is_error == Some(true) {
            return Err(FavouriteError::Rejected);
        }
        Ok(())
    }
}

impl<T> std::ops::Deref for Snapshot<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T> std::ops::DerefMut for Snapshot<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

#[cfg(test)]
#[path = "vault_state_test.rs"]
mod tests;
