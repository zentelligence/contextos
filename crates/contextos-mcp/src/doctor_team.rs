//! The doctor's `team_config` check: the `agents.toml` migration finding, and whether `.contextos/team.toml` is a team
//! a vault's jobs can use (slugs unique and well formed, exactly one chief of staff).

use contextos_core::{TEAM_PATH, TeamFile, TeamRoster, TeamRosterError, TeamRule, VaultPath, VaultPathInput, VaultSet};
use contextos_fs::{Filesystem, FsError, ListDirectoryRequest, ReadTextRequest};

use crate::doctor::{DoctorCheck, DoctorError, file_exists};
use crate::doctor_kind::DoctorKind;

/// Flags a vault still holding `.contextos/agents.toml` with no `.contextos/team.toml` alongside it: the file was
/// renamed (`agents.toml` gained human team members and became `team.toml`), and both `contextos-mcp` and
/// `contextos-web` now read only the new name, so a lingering old file would otherwise leave the Agents and Tasks
/// screens silently empty with no indication why. Deliberately not `auto_fixable`: renaming an operator-owned
/// config file out from under them is not a structural repair `doctor_resolve` can prove safe the way rebuilding a
/// derived index or initialising a Git repository is.
pub(crate) fn team_config_check(
    root: &contextos_core::VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
) -> Result<DoctorCheck, DoctorError> {
    let old_present = file_exists(root, roots, filesystem, ".contextos/agents.toml")?;
    let new_present = file_exists(root, roots, filesystem, ".contextos/team.toml")?;
    if old_present && !new_present {
        return Ok(DoctorCheck::fail(
            DoctorKind::TeamConfig,
            "`.contextos/agents.toml` is present but `.contextos/team.toml` is not",
            "Rename .contextos/agents.toml to .contextos/team.toml (and add `type = \"agent\"` or \
             `type = \"human\"` per entry if you want to name any human team members) - neither \
             contextos-mcp nor contextos-web reads the old filename any more.",
        ));
    }
    match roster_of(root, roots, filesystem)? {
        Ok(roster) => Ok(DoctorCheck::pass(
            DoctorKind::TeamConfig,
            format!("{} members, one chief of staff", roster.members().len()),
        )),
        Err(error) if error.rule() == TeamRule::FileAbsent => {
            if vault_has_jobs(root, roots, filesystem)? {
                Ok(invalid(&error))
            } else {
                Ok(DoctorCheck::pass_advising(
                    DoctorKind::TeamConfig,
                    "no .contextos/team.toml; none is needed until the vault has jobs",
                    "Add .contextos/team.toml with exactly one member who has `chief_of_staff = true` before saving a job: \
                     a job needs an owner who is on the team, and failures are raised to the chief of staff.",
                ))
            }
        }
        Err(error) => Ok(invalid(&error)),
    }
}

/// The failure a team that breaks a rule is reported as. Names the rule and never a member's content.
fn invalid(error: &TeamRosterError) -> DoctorCheck {
    DoctorCheck::fail(
        DoctorKind::TeamConfig,
        format!(
            "{TEAM_PATH} cannot be used by the vault's jobs ({})",
            error.rule().as_str()
        ),
        format!(
            "Correct {TEAM_PATH}: every member needs a lowercase slug (letters, digits, and hyphens) that no other \
             member has, the team needs at least one member, and exactly one member needs `chief_of_staff = true`. Scheduled jobs in this vault are \
             paused until it is valid."
        ),
    )
}

/// The vault's team as a job reads it. The inner error is a team that cannot be used, and the outer one a read that
/// could not be made at all.
fn roster_of(
    root: &contextos_core::VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
) -> Result<Result<TeamRoster, TeamRosterError>, DoctorError> {
    let path = vault_path(root, roots, TEAM_PATH)?;
    let text = match filesystem.read_text(&ReadTextRequest { path, limit: None }) {
        Ok(read) => read.content,
        Err(FsError::NotFound { .. }) => return Ok(Err(TeamRule::FileAbsent.into())),
        Err(FsError::Binary { .. }) => return Ok(Err(TeamRule::FileNotText.into())),
        Err(other) => return Err(DoctorError::from(other)),
    };
    Ok(toml::from_str::<TeamFile>(&text)
        .map_err(|_| TeamRosterError::from(TeamRule::FileUnparsable))
        .and_then(TeamRoster::try_from))
}

/// Whether the vault has any saved job: a file under `registry/workflows/<package>/<workflow>/jobs/`.
fn vault_has_jobs(
    root: &contextos_core::VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
) -> Result<bool, DoctorError> {
    for package in folders_under(root, roots, filesystem, "registry/workflows")? {
        for workflow in folders_under(root, roots, filesystem, &format!("registry/workflows/{package}"))? {
            let folder = vault_path(root, roots, &format!("registry/workflows/{package}/{workflow}/jobs"))?;
            match filesystem.list_directory(&ListDirectoryRequest { path: folder }) {
                Ok(listing) if listing.entries.iter().any(|entry| is_toml(&entry.name)) => return Ok(true),
                Ok(_) | Err(FsError::NotFound { .. } | FsError::NotDirectory { .. }) => {}
                Err(other) => return Err(DoctorError::from(other)),
            }
        }
    }
    Ok(false)
}

/// The names of the folders directly under `relative`; a folder that is not there holds none.
fn folders_under(
    root: &contextos_core::VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
    relative: &str,
) -> Result<Vec<String>, DoctorError> {
    let listing = match filesystem.list_directory(&ListDirectoryRequest {
        path: vault_path(root, roots, relative)?,
    }) {
        Ok(listing) => listing,
        Err(FsError::NotFound { .. } | FsError::NotDirectory { .. }) => return Ok(Vec::new()),
        Err(other) => return Err(DoctorError::from(other)),
    };
    Ok(listing
        .entries
        .into_iter()
        .filter(|entry| entry.kind == contextos_fs::EntryKind::Directory)
        .map(|entry| entry.name)
        .collect())
}

/// Whether `name` is a TOML file's name, whatever the case of its extension.
fn is_toml(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
}

fn vault_path(root: &contextos_core::VaultRoot, roots: &VaultSet, relative: &str) -> Result<VaultPath, DoctorError> {
    let absolute = root.path().join(relative);
    Ok(VaultPath::try_from(VaultPathInput {
        roots,
        raw: absolute
            .to_str()
            .ok_or_else(|| DoctorError::NonUtf8VaultPath(absolute.clone()))?,
    })?)
}

#[cfg(test)]
#[path = "doctor_team_test.rs"]
mod tests;
