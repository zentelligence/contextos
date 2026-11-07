//! On-disk credential store for `/server/settings/`.
//!
//! Every secret `/server/settings/` will ever accept or produce — a pasted bearer token, an `OAuth2` client secret, or an
//! `OAuth2` access/refresh token pair — is written here, one JSON file per `credential_ref`, and never into
//! `web.toml` itself, for the two entry paths an environment variable does not cover: a non-technical operator has no environment variable to point at, and an
//! `OAuth2` token is a value `contextos-web` itself produces).
//!
//! This module is a pure storage boundary: it has no injected clock and never reads system time itself, so every
//! timestamp field is a plain `String` (RFC 3339) supplied by the caller. Nothing in this crate calls it yet; a
//! later increment wires `OAuth2` routes and `/server/settings/` validation against it.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::atomic_write::write_atomically;

/// The `credentials/` directory sibling to `web_config_path`: the single
/// derivation rule every caller that needs a [`FsCredentialStore`] rooted next to a particular
/// `web.toml` shares, rather than each one repeating `web_config_path.parent().join("credentials")`
/// independently. `routes::settings::SettingsRoutesState::new` and `app::connect` (the live MCP
/// sessions' own token-refresh credential access) are the two current callers.
#[must_use]
pub(crate) fn credentials_directory(web_config_path: &Path) -> PathBuf {
    web_config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
        .join("credentials")
}

/// Whether `credential_ref` can name a record: not empty, with no `/` or `\\` separator and no `..` segment, so it
/// always names one file directly inside the store. Checked before any filesystem operation, and by `/server/settings/`
/// before it saves a reference a pasted secret would be stored under.
#[must_use]
pub(crate) fn can_hold(credential_ref: &str) -> bool {
    !(credential_ref.is_empty()
        || credential_ref.contains('/')
        || credential_ref.contains('\\')
        || credential_ref.contains(".."))
}

/// One stored secret. Internally tagged on `kind` so the three
/// record shapes serialise as distinct, self-describing JSON documents rather than a single all-fields-optional
/// shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CredentialRecord {
    /// A pasted bearer token (`/server/settings/`'s `PATCH`/`POST` on a `bearer`-mode entry).
    Bearer { value: String, updated_at: String },
    /// An `OAuth2` client secret (`/server/settings/`'s `PATCH`/`POST` on an `oauth2`-mode entry).
    Oauth2ClientSecret { value: String, updated_at: String },
    /// An `OAuth2` access/refresh token pair (the callback route and the refresh path). `refresh_token` is `None`
    /// when the provider never issued one.
    Oauth2Token {
        access_token: String,
        refresh_token: Option<String>,
        expires_at: String,
        obtained_at: String,
    },
}

/// A small capability trait over the credential store, so a later increment's `OAuth2` routes and `/server/settings/`
/// validation depend on this trait rather than the concrete filesystem implementation.
///
/// # Windows scope boundary
///
/// The Unix `0700`/`0600` permission enforcement this trait's filesystem implementation applies has no equivalent
/// call in this module on other platforms. On Windows, the store instead relies entirely on the enclosing
/// user-profile directory's own ACL, the same directory `web.toml` itself already lives under. This is a
/// deliberate, documented scope boundary, not an oversight.
pub trait CredentialStore {
    /// Returns the stored record for `credential_ref`, or `Ok(None)` when nothing has ever been stored under that
    /// reference.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialStoreError::InvalidCredentialRef`] when `credential_ref` fails the path-confinement
    /// guard, or [`CredentialStoreError::Read`]/[`CredentialStoreError::Deserialize`] when a stored record exists
    /// but cannot be read or parsed.
    fn get(&self, credential_ref: &str) -> Result<Option<CredentialRecord>, CredentialStoreError>;

    /// Writes `record` for `credential_ref`, creating the store's root directory (and any missing parent) first
    /// if it does not already exist.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialStoreError::InvalidCredentialRef`] when `credential_ref` fails the path-confinement
    /// guard, or [`CredentialStoreError::CreateDirectory`]/[`CredentialStoreError::Serialize`]/
    /// [`CredentialStoreError::Write`] when the directory cannot be created, `record` cannot be serialised, or
    /// the write fails.
    fn set(&self, credential_ref: &str, record: &CredentialRecord) -> Result<(), CredentialStoreError>;

    /// Removes the stored record for `credential_ref`. Idempotent: deleting a `credential_ref` that was never
    /// set, or was already deleted, is `Ok(())`, not an error.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialStoreError::InvalidCredentialRef`] when `credential_ref` fails the path-confinement
    /// guard, or [`CredentialStoreError::Delete`] when the file exists but cannot be removed.
    fn delete(&self, credential_ref: &str) -> Result<(), CredentialStoreError>;
}

/// Filesystem-backed [`CredentialStore`]: one JSON file per `credential_ref` under `root`.
///
/// `root` is the `credentials/` directory itself; the caller is responsible for deriving it (typically a
/// `credentials` directory sibling to the loaded `web.toml`) since this module has no knowledge of where
/// `web.toml` lives.
#[derive(Clone, Debug)]
pub struct FsCredentialStore {
    root: PathBuf,
}

impl FsCredentialStore {
    /// Creates a store rooted at `root`. `root` is not created until the first [`CredentialStore::set`] call.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Validates `credential_ref` against the path-confinement guard and returns the JSON file path it maps to
    /// under `root`.
    ///
    /// `credential_ref` ultimately comes from an operator-editable `web.toml`, so it is untrusted input the
    /// moment it is used to build a filesystem path (`security.md`'s path-confinement rules). An empty
    /// reference, one containing a `/` or `\` separator, or one containing a `..` segment is rejected before any
    /// filesystem operation is attempted.
    fn record_path(&self, credential_ref: &str) -> Result<PathBuf, CredentialStoreError> {
        if !can_hold(credential_ref) {
            return Err(CredentialStoreError::InvalidCredentialRef {
                credential_ref: credential_ref.to_owned(),
            });
        }
        Ok(self.root.join(format!("{credential_ref}.json")))
    }

    #[cfg(unix)]
    fn restrict_directory_permissions(root: &std::path::Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
    }

    #[cfg(unix)]
    fn restrict_file_permissions(path: &std::path::Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
}

impl CredentialStore for FsCredentialStore {
    fn get(&self, credential_ref: &str) -> Result<Option<CredentialRecord>, CredentialStoreError> {
        let path = self.record_path(credential_ref)?;
        if !path.exists() {
            return Ok(None);
        }
        let contents = fs::read_to_string(&path).map_err(|source| CredentialStoreError::Read {
            credential_ref: credential_ref.to_owned(),
            source,
        })?;
        let record = serde_json::from_str(&contents).map_err(|source| CredentialStoreError::Deserialize {
            credential_ref: credential_ref.to_owned(),
            source,
        })?;
        Ok(Some(record))
    }

    fn set(&self, credential_ref: &str, record: &CredentialRecord) -> Result<(), CredentialStoreError> {
        let path = self.record_path(credential_ref)?;
        if !self.root.exists() {
            fs::create_dir_all(&self.root).map_err(|source| CredentialStoreError::CreateDirectory {
                path: self.root.clone(),
                source,
            })?;
            #[cfg(unix)]
            Self::restrict_directory_permissions(&self.root).map_err(|source| {
                CredentialStoreError::CreateDirectory {
                    path: self.root.clone(),
                    source,
                }
            })?;
        }
        let contents = serde_json::to_vec_pretty(record).map_err(|source| CredentialStoreError::Serialize {
            credential_ref: credential_ref.to_owned(),
            source,
        })?;
        write_atomically(&path, &contents).map_err(|source| CredentialStoreError::Write {
            credential_ref: credential_ref.to_owned(),
            source,
        })?;
        #[cfg(unix)]
        Self::restrict_file_permissions(&path).map_err(|source| CredentialStoreError::Write {
            credential_ref: credential_ref.to_owned(),
            source,
        })?;
        Ok(())
    }

    fn delete(&self, credential_ref: &str) -> Result<(), CredentialStoreError> {
        let path = self.record_path(credential_ref)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(CredentialStoreError::Delete {
                credential_ref: credential_ref.to_owned(),
                source,
            }),
        }
    }
}

/// Errors from [`CredentialStore`] operations. Never carries a secret value: only the `credential_ref` involved
/// and, for I/O failures, the underlying [`std::io::Error`].
#[derive(Debug, Error)]
pub enum CredentialStoreError {
    #[error(
        "credential_ref {credential_ref:?} is invalid: it must be non-empty and must not contain \
         a path separator or a `..` segment"
    )]
    InvalidCredentialRef { credential_ref: String },
    #[error("could not create credential store directory: {path}", path = path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not read credential record for {credential_ref}")]
    Read {
        credential_ref: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write credential record for {credential_ref}")]
    Write {
        credential_ref: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not delete credential record for {credential_ref}")]
    Delete {
        credential_ref: String,
        #[source]
        source: std::io::Error,
    },
    #[error("stored credential record for {credential_ref} is not valid JSON")]
    Deserialize {
        credential_ref: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("could not serialise credential record for {credential_ref}")]
    Serialize {
        credential_ref: String,
        #[source]
        source: serde_json::Error,
    },
}

#[cfg(test)]
#[path = "credential_store_test.rs"]
mod tests;
