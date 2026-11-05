//! Puts the `contextos` and `contextos-web` commands on the operator's `PATH` after an install, so that installing
//! is one command: the launcher is copied under each public name, and its folder is added to `PATH`.

use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use contextos_host::HostFolders;

/// The commands a release set puts on `PATH`, each a copy of the launcher.
const COMMANDS: [&str; 2] = ["contextos", "contextos-web"];

/// The line that marks a shell profile as already set up.
const PROFILE_MARKER: &str = "# Added by ContextOS";

/// What became of one public command's launcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Placement {
    /// The launcher was copied there.
    Placed,
    /// The same launcher was already there.
    AlreadyInPlace,
    /// A different file is there, which is the operator's and is left alone.
    SomethingElseThere,
}

/// Sets up the commands for the release set `version` under the installation `root`, returning what it did in
/// words for the person installing.
pub(crate) fn set_up_commands(root: &Path, version: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
    let folders = HostFolders::detect().ok_or("no home folder could be found for the contextos commands")?;
    let launcher = root
        .join("versions")
        .join(version)
        .join("bin")
        .join(format!("contextos-launcher{}", std::env::consts::EXE_SUFFIX));
    let folder = command_folder(&folders);
    fs::create_dir_all(&folder)?;

    let mut report = Vec::new();
    for command in COMMANDS {
        let target = folder.join(format!("{command}{}", std::env::consts::EXE_SUFFIX));
        if place_launcher(&launcher, &target)? == Placement::SomethingElseThere {
            report.push(format!(
                "{} already exists and is not this launcher, so it was left as it is.",
                target.display()
            ));
        }
    }
    report.push(add_folder_to_path(&folder, &folders)?);
    Ok(report.join("\n"))
}

/// Where the commands go: a folder the operator owns, so no administrator rights are needed.
pub(crate) fn command_folder(folders: &HostFolders) -> PathBuf {
    if cfg!(windows) {
        folders.data_local().join("Programs").join("ContextOS")
    } else {
        folders.home().join(".local").join("bin")
    }
}

/// Copies `launcher` to `target`, unless something is already there.
pub(crate) fn place_launcher(launcher: &Path, target: &Path) -> io::Result<Placement> {
    match fs::read(target) {
        Ok(existing) if existing == fs::read(launcher)? => return Ok(Placement::AlreadyInPlace),
        Ok(_) => return Ok(Placement::SomethingElseThere),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let staged = target.with_extension("installing");
    fs::copy(launcher, &staged)
        .and_then(|_| fs::rename(&staged, target))
        .inspect_err(|_| {
            let _ = fs::remove_file(&staged);
        })?;
    Ok(Placement::Placed)
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
    writeln!(file, "\n{PROFILE_MARKER}\nexport PATH=\"$HOME/.local/bin:$PATH\"")?;
    Ok(true)
}

/// Adds `folder` to `PATH` through the shell's profiles, returning what happened in words.
#[cfg(not(windows))]
fn add_folder_to_path(folder: &Path, folders: &HostFolders) -> io::Result<String> {
    if path_holds(std::env::var_os("PATH").as_deref(), folder) {
        return Ok(format!(
            "The contextos and contextos-web commands are in {}.",
            folder.display()
        ));
    }
    let profiles = profile_files(folders.home(), std::env::var_os("SHELL").as_deref());
    if profiles.is_empty() {
        return Ok(format!(
            "The contextos and contextos-web commands are in {}. Add that folder to your PATH to use them.",
            folder.display()
        ));
    }
    for profile in &profiles {
        add_to_profile(profile)?;
    }
    let names: Vec<String> = profiles.iter().map(|profile| profile.display().to_string()).collect();
    Ok(format!(
        "The contextos and contextos-web commands are in {}, which was added to your PATH in {}.",
        folder.display(),
        names.join(" and ")
    ))
}

/// Adds `folder` to the user's `Path` in the registry, through the one call that also tells running programs.
#[cfg(windows)]
fn add_folder_to_path(folder: &Path, _folders: &HostFolders) -> io::Result<String> {
    const SCRIPT: &str = "$folder = $env:CONTEXTOS_COMMAND_FOLDER; \
        $path = [Environment]::GetEnvironmentVariable('Path', 'User'); \
        if (\"$path\" -split ';' -notcontains $folder) { \
        $entries = @(\"$path\".TrimEnd(';'), $folder) | Where-Object { $_ }; \
        [Environment]::SetEnvironmentVariable('Path', ($entries -join ';'), 'User') }";

    let system_root = std::env::var_os("SystemRoot").ok_or_else(|| io::Error::other("SystemRoot is not set"))?;
    let powershell = Path::new(&system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let status = std::process::Command::new(powershell)
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("CONTEXTOS_COMMAND_FOLDER", folder)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "{} could not be added to your Path",
            folder.display()
        )));
    }
    Ok(format!(
        "The contextos and contextos-web commands are in {}, which is on your Path.",
        folder.display()
    ))
}

#[cfg(test)]
#[path = "command_setup_test.rs"]
mod tests;
