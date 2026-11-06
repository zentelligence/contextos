//! Puts the commands on the operator's `PATH` and takes them off it again: the launcher is copied under each public
//! name, and its folder is added to `PATH`, because the people installing the suite should never need to know what
//! an environment variable is.
//!
//! Everything placed is recorded in a ledger in the installation root. That is how a later update knows an older
//! launcher from a file of the operator's with the same name, and how an uninstall removes only what was added.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use contextos_host::HostFolders;
use serde::{Deserialize, Serialize};
use updater_core::ReleaseMetadata;
use updater_engine::compute_sha256;

use crate::LAUNCHER_NAME;
use crate::update_cli::CliResult;

/// The ledger's file name in the installation root.
const LEDGER_FILE: &str = "launchers.json";
const LEDGER_SCHEMA_VERSION: u32 = 1;
/// The line that marks a shell profile as already set up.
const PROFILE_MARKER: &str = "# Added by ContextOS";
/// The line that puts the commands' folder on `PATH`.
const PROFILE_LINE: &str = "export PATH=\"$HOME/.local/bin:$PATH\"";

/// One launcher copy, with the digest it had when placed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlacedLauncher {
    pub(crate) path: PathBuf,
    pub(crate) sha256: String,
}

/// What an install added outside the installation root.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Ledger {
    schema_version: u32,
    pub(crate) launchers: Vec<PlacedLauncher>,
    /// Shell profiles that gained the `PATH` line.
    pub(crate) profiles: Vec<PathBuf>,
    /// The folder added to the user's `Path` on Windows.
    pub(crate) path_folder: Option<PathBuf>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            schema_version: LEDGER_SCHEMA_VERSION,
            launchers: Vec::new(),
            profiles: Vec::new(),
            path_folder: None,
        }
    }
}

impl Ledger {
    /// The ledger in `root`, or an empty one when none has been written.
    pub(crate) fn load(root: &Path) -> CliResult<Self> {
        let path = root.join(LEDGER_FILE);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error.into()),
        };
        let ledger: Self = serde_json::from_slice(&bytes)?;
        if ledger.schema_version != LEDGER_SCHEMA_VERSION {
            return Err(format!(
                "{} has unknown schema version {}",
                path.display(),
                ledger.schema_version
            )
            .into());
        }
        Ok(ledger)
    }

    /// Writes the ledger to `root`, whole or not at all.
    pub(crate) fn save(&self, root: &Path) -> CliResult<()> {
        let staged = root.join(format!("{LEDGER_FILE}.new"));
        fs::write(&staged, serde_json::to_vec_pretty(self)?)?;
        fs::rename(&staged, root.join(LEDGER_FILE))?;
        Ok(())
    }

    fn recorded_digest(&self, target: &Path) -> Option<&str> {
        self.launchers
            .iter()
            .find(|placed| placed.path == target)
            .map(|placed| placed.sha256.as_str())
    }

    fn record(&mut self, path: PathBuf, sha256: String) {
        self.launchers.retain(|placed| placed.path != path);
        self.launchers.push(PlacedLauncher { path, sha256 });
    }
}

/// What became of one public command's launcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Placement {
    /// The launcher was copied there, over nothing or over a launcher placed earlier.
    Placed,
    /// The same launcher was already there.
    AlreadyInPlace,
    /// A file that was never placed here, which is the operator's and is left alone.
    SomethingElseThere,
}

/// The SHA-256 of the file at `path`, as lowercase hex.
pub(crate) fn digest(path: &Path) -> CliResult<String> {
    Ok(compute_sha256(fs::File::open(path)?, path)?.to_string())
}

/// Sets up the commands for the release set `version` under the installation `root` and puts them on `PATH`, returning
/// what it did in words for the person installing.
pub(crate) fn set_up_commands(root: &Path, version: &str) -> CliResult<String> {
    let mut report = refresh_launchers(root, version)?;
    let folders = host_folders()?;
    let folder = command_folder(root, &folders);
    let mut ledger = Ledger::load(root)?;
    report.push(add_folder_to_path(&folder, &folders, &mut ledger)?);
    ledger.save(root)?;
    Ok(report.join("\n"))
}

/// Brings every command's launcher up to the release set `version`, returning anything the operator should know.
pub(crate) fn refresh_launchers(root: &Path, version: &str) -> CliResult<Vec<String>> {
    let folders = host_folders()?;
    let folder = command_folder(root, &folders);
    fs::create_dir_all(&folder)?;
    let release = root.join("versions").join(version);
    let launcher = release
        .join("bin")
        .join(format!("{LAUNCHER_NAME}{}", std::env::consts::EXE_SUFFIX));
    let metadata = ReleaseMetadata::try_from(fs::read(release.join("release.json"))?.as_slice())?;
    let mut names: Vec<String> = metadata.commands.iter().map(|command| command.name.clone()).collect();
    names.push(LAUNCHER_NAME.to_owned());

    let mut ledger = Ledger::load(root)?;
    let launcher_digest = digest(&launcher)?;
    let mut report = Vec::new();
    for name in names {
        let target = folder.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        match place_launcher(&launcher, &target, ledger.recorded_digest(&target))? {
            Placement::SomethingElseThere => report.push(format!(
                "{} already exists and is not a ContextOS launcher, so it was left as it is.",
                target.display()
            )),
            Placement::Placed | Placement::AlreadyInPlace => ledger.record(target, launcher_digest.clone()),
        }
    }
    ledger.save(root)?;
    remove_set_aside(&folder);
    Ok(report)
}

fn host_folders() -> CliResult<HostFolders> {
    Ok(HostFolders::detect().ok_or("no home folder could be found for the ContextOS commands")?)
}

/// Where the commands go: a folder the operator owns, so no administrator rights are needed.
pub(crate) fn command_folder(root: &Path, folders: &HostFolders) -> PathBuf {
    if cfg!(windows) {
        root.join("bin")
    } else {
        folders.home().join(".local").join("bin")
    }
}

/// Copies `launcher` to `target`, over an earlier launcher whose digest is `recorded` but over nothing else.
pub(crate) fn place_launcher(launcher: &Path, target: &Path, recorded: Option<&str>) -> CliResult<Placement> {
    if target.exists() {
        if fs::read(target)? == fs::read(launcher)? {
            return Ok(Placement::AlreadyInPlace);
        }
        if recorded != Some(digest(target)?.as_str()) {
            return Ok(Placement::SomethingElseThere);
        }
    }
    let staged = target.with_extension("installing");
    fs::copy(launcher, &staged)
        .and_then(|_| swap_in(&staged, target))
        .inspect_err(|_| {
            let _ = fs::remove_file(&staged);
        })?;
    Ok(Placement::Placed)
}

/// Replaces `target` with `staged`. Renaming over a file works while the file is running on Unix.
#[cfg(not(windows))]
fn swap_in(staged: &Path, target: &Path) -> io::Result<()> {
    fs::rename(staged, target)
}

/// Replaces `target` with `staged`. Windows refuses to overwrite a running program, which this launcher may well be,
/// but lets it be renamed, so it is moved aside and deleted by a later run.
#[cfg(windows)]
fn swap_in(staged: &Path, target: &Path) -> io::Result<()> {
    if target.exists() {
        fs::rename(target, target.with_extension(format!("old-{}", std::process::id())))?;
    }
    fs::rename(staged, target)
}

/// Deletes launchers moved aside by earlier runs, leaving any still running.
fn remove_set_aside(folder: &Path) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let set_aside = path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| extension.starts_with("old-"));
        if set_aside {
            let _ = fs::remove_file(path);
        }
    }
}

/// Whether `folder` is one of the folders in the `PATH` value `path`.
pub(crate) fn path_holds(path: Option<&OsStr>, folder: &Path) -> bool {
    path.is_some_and(|path| std::env::split_paths(path).any(|entry| entry == folder))
}

/// The profiles that `shell` reads, under `home`. Empty for a shell whose profile this does not know how to edit.
pub(crate) fn profile_files(home: &Path, shell: Option<&OsStr>) -> Vec<PathBuf> {
    match shell.map(Path::new).and_then(Path::file_name).and_then(OsStr::to_str) {
        Some("zsh") => vec![home.join(".zshrc")],
        Some("bash") => {
            let bash_profile = home.join(".bash_profile");
            let login_profile = if bash_profile.exists() {
                bash_profile
            } else {
                home.join(".profile")
            };
            vec![home.join(".bashrc"), login_profile]
        }
        _ => Vec::new(),
    }
}

fn profile_block() -> String {
    format!("\n{PROFILE_MARKER}\n{PROFILE_LINE}\n")
}

/// Adds `~/.local/bin` to `PATH` in `profile`, once, returning whether it changed the file.
pub(crate) fn add_to_profile(profile: &Path) -> io::Result<bool> {
    let existing = match fs::read_to_string(profile) {
        Ok(existing) => existing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    if existing.contains(PROFILE_MARKER) {
        return Ok(false);
    }
    let mut file = fs::OpenOptions::new().create(true).append(true).open(profile)?;
    file.write_all(profile_block().as_bytes())?;
    Ok(true)
}

/// Takes back what [`add_to_profile`] added, deleting a profile that then holds nothing, as it would if adding created
/// it. Anything else in the profile stays exactly as it is.
pub(crate) fn remove_from_profile(profile: &Path) -> io::Result<()> {
    let existing = match fs::read_to_string(profile) {
        Ok(existing) => existing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let remaining = existing.replacen(&profile_block(), "", 1);
    if remaining == existing {
        return Ok(());
    }
    if remaining.is_empty() {
        fs::remove_file(profile)
    } else {
        fs::write(profile, remaining)
    }
}

/// Adds `folder` to `PATH` through the shell's profiles, returning what happened in words.
#[cfg(not(windows))]
fn add_folder_to_path(folder: &Path, folders: &HostFolders, ledger: &mut Ledger) -> io::Result<String> {
    if path_holds(std::env::var_os("PATH").as_deref(), folder) {
        return Ok(format!("The ContextOS commands are in {}.", folder.display()));
    }
    let profiles = profile_files(folders.home(), std::env::var_os("SHELL").as_deref());
    if profiles.is_empty() {
        return Ok(format!(
            "The ContextOS commands are in {}. Add that folder to your PATH to use them.",
            folder.display()
        ));
    }
    for profile in &profiles {
        if add_to_profile(profile)? {
            ledger.profiles.push(profile.clone());
        }
    }
    let names: Vec<String> = profiles.iter().map(|profile| profile.display().to_string()).collect();
    Ok(format!(
        "The ContextOS commands are in {}, which was added to your PATH in {}.",
        folder.display(),
        names.join(" and ")
    ))
}

/// Windows PowerShell, by its full path so no same-named program can stand in for it.
#[cfg(windows)]
fn run_powershell(script: &str, folder: &Path) -> io::Result<()> {
    let system_root = std::env::var_os("SystemRoot").ok_or_else(|| io::Error::other("SystemRoot is not set"))?;
    let powershell = Path::new(&system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let status = std::process::Command::new(powershell)
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("CONTEXTOS_COMMAND_FOLDER", folder)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "your Path could not be changed for {}",
            folder.display()
        )))
    }
}

/// Adds `folder` to the user's `Path` in the registry, through the one call that also tells running programs.
#[cfg(windows)]
fn add_folder_to_path(folder: &Path, _folders: &HostFolders, ledger: &mut Ledger) -> io::Result<String> {
    const SCRIPT: &str = "$folder = $env:CONTEXTOS_COMMAND_FOLDER; \
        $path = [Environment]::GetEnvironmentVariable('Path', 'User'); \
        if (\"$path\" -split ';' -notcontains $folder) { \
        $entries = @(\"$path\".TrimEnd(';'), $folder) | Where-Object { $_ }; \
        [Environment]::SetEnvironmentVariable('Path', ($entries -join ';'), 'User') }";

    run_powershell(SCRIPT, folder)?;
    ledger.path_folder = Some(folder.to_path_buf());
    Ok(format!(
        "The ContextOS commands are in {}, which is on your Path.",
        folder.display()
    ))
}

/// Takes `folder` back off the user's `Path`.
#[cfg(windows)]
pub(crate) fn remove_folder_from_path(folder: &Path) -> io::Result<()> {
    const SCRIPT: &str = "$folder = $env:CONTEXTOS_COMMAND_FOLDER; \
        $path = [Environment]::GetEnvironmentVariable('Path', 'User'); \
        $entries = \"$path\" -split ';' | Where-Object { $_ -and $_ -ne $folder }; \
        [Environment]::SetEnvironmentVariable('Path', ($entries -join ';'), 'User')";

    run_powershell(SCRIPT, folder)
}

#[cfg(test)]
#[path = "commands_test.rs"]
mod tests;
