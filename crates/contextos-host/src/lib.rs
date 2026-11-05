#![forbid(unsafe_code)]

//! Where the operator's home and platform data folders are, for every binary in this workspace.
//!
//! The operating system answers first, through `directories`. On Windows that answer is refused outright when
//! `USERPROFILE` names anything but the account's own profile, even an existing folder, so a process started with a
//! redirected profile would otherwise have no home at all. There the variable the operator set is taken at its word,
//! as `HOME` already is on Unix. A process started that way keeps its state under that home, as asked.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use directories::{BaseDirs, ProjectDirs};
use thiserror::Error;

/// The name every folder of this workspace's own carries under the platform's data folder.
const PRODUCT: &str = "contextos";

/// The variable `contextos-launcher` sets on the program it starts, naming the launcher's own path.
pub const LAUNCHER_VARIABLE: &str = "CONTEXTOS_LAUNCHER";

/// The path that keeps starting this program across updates, for anything that records how to start it later: a
/// background service or an MCP host's registration.
///
/// A program started by the launcher runs from a versioned folder that a later update leaves behind and cleanup
/// deletes, so the launcher's path is the one to record. A program started any other way has only its own.
///
/// # Errors
///
/// The underlying [`std::io::Error`] when this process's executable cannot be named.
pub fn stable_executable() -> std::io::Result<PathBuf> {
    Ok(launcher_or_running(
        std::env::current_exe()?,
        std::env::var_os(LAUNCHER_VARIABLE),
    ))
}

/// The launcher, when `launcher` names an existing absolute file with the same name as the `running` program. The
/// variable is inherited by everything this process starts, so it may have been set for another program.
fn launcher_or_running(running: PathBuf, launcher: Option<OsString>) -> PathBuf {
    launcher
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_file() && path.file_stem() == running.file_stem())
        .unwrap_or(running)
}

/// Why the environment names no home folder.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum HostFoldersError {
    /// Neither variable holds an absolute path.
    #[error("neither USERPROFILE nor HOME names an absolute folder")]
    NoHomeVariable,
}

/// The environment variables that can name a home folder, taken as values so the rule can be tested on any host.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HomeVariables {
    /// The value of `USERPROFILE`.
    pub user_profile: Option<OsString>,
    /// The value of `HOME`.
    pub home: Option<OsString>,
}

/// The operator's home folder and the platform data folders beneath it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostFolders {
    home: PathBuf,
    config: PathBuf,
    data_local: PathBuf,
}

impl HostFolders {
    /// Asks the operating system, then the environment, and gives `None` when neither can say.
    #[must_use]
    pub fn detect() -> Option<Self> {
        if let Some(folders) = BaseDirs::new() {
            return Some(Self {
                home: folders.home_dir().to_path_buf(),
                config: folders.config_dir().to_path_buf(),
                data_local: folders.data_local_dir().to_path_buf(),
            });
        }
        let folders = Self::try_from(HomeVariables {
            user_profile: std::env::var_os("USERPROFILE"),
            home: std::env::var_os("HOME"),
        })
        .ok()?;
        tracing::debug!("the operating system named no home folder, so the one in USERPROFILE or HOME is used");
        Some(folders)
    }

    /// This workspace's folder for host-local data, which is not in any vault, or `None` when no home can be found.
    #[must_use]
    pub fn application_data() -> Option<PathBuf> {
        ProjectDirs::from("", "", PRODUCT)
            .map(|folders| folders.data_local_dir().to_path_buf())
            .or_else(|| Self::detect().map(|folders| folders.product_data()))
    }

    /// The operator's home folder.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The platform's folder for per-user configuration.
    #[must_use]
    pub fn config(&self) -> &Path {
        &self.config
    }

    /// The platform's folder for per-user data that stays on this machine.
    #[must_use]
    pub fn data_local(&self) -> &Path {
        &self.data_local
    }

    /// Where [`ProjectDirs`] puts this workspace's data under the local data folder.
    fn product_data(&self) -> PathBuf {
        let product = self.data_local.join(PRODUCT);
        if cfg!(windows) { product.join("data") } else { product }
    }
}

impl TryFrom<HomeVariables> for HostFolders {
    type Error = HostFoldersError;

    /// Takes the first variable holding an absolute path, `USERPROFILE` before `HOME`, and lays the platform folders
    /// out beneath it as the platform does. A relative value is refused rather than resolved against wherever the
    /// process happened to start.
    fn try_from(value: HomeVariables) -> Result<Self, Self::Error> {
        let home = [value.user_profile, value.home]
            .into_iter()
            .flatten()
            .map(PathBuf::from)
            .find(|path| path.is_absolute())
            .ok_or(HostFoldersError::NoHomeVariable)?;
        let (config, data_local) = if cfg!(windows) {
            (home.join("AppData").join("Roaming"), home.join("AppData").join("Local"))
        } else {
            (home.join(".config"), home.join(".local").join("share"))
        };
        Ok(Self {
            home,
            config,
            data_local,
        })
    }
}

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
