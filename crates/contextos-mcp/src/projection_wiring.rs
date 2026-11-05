//! Composes agent host projection behind the operation router, and runs its start-up refresh.

use std::path::Path;
use std::sync::Arc;

use contextos_core::{OperationEvent, OperationWarning, ProjectsDefinitions, VaultSet};
use contextos_fs::Filesystem;
use contextos_fs::host_file::FolderReplaceObserver;
use contextos_projection::{
    HostFolder, HostKind, HostProjection, InstructionsFile, ManifestLocation, ProjectionError, ProjectionReport,
    ProjectionService, ProjectionTarget, VaultSetSource,
};

use crate::projection_lock::{LockedProjection, LockedProjectionConfig, ProjectionLockError};
use crate::projection_setup::{HostSettings, ProjectionSettings, ProjectionSetup};

/// What the router calls when a definition changes: nothing, or the locked projection service.
///
/// Disabled is a variant of its own, not an absent service, so a configuration that does not project provably never
/// reaches a host folder: there is no code path from `Disabled` to one.
#[derive(Clone, Debug)]
pub(crate) enum Projection {
    /// Projection is off for this configuration.
    Disabled,
    /// Projection runs for at least one host, under the host-level projection lock.
    Enabled(Arc<LockedProjection<ProjectionService>>),
}

/// The inputs to composing projection: the validated settings and the vaults definitions are read from.
pub(crate) struct ProjectionWiringInput {
    pub(crate) setup: ProjectionSetup,
    pub(crate) roots: VaultSet,
    pub(crate) filesystem: Filesystem,
}

/// Whether composing projection may create a host's agent and skill folders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Folders {
    /// Serving: a missing folder is created, so the first projection has somewhere to go.
    Create,
    /// `doctor` and its resolution: a missing folder means nothing was ever projected there, and a host is not given
    /// a folder merely by being assessed, so a host whose folders are missing is left out.
    ExistingOnly,
}

impl From<ProjectionWiringInput> for Projection {
    fn from(value: ProjectionWiringInput) -> Self {
        Self::compose(value, Folders::Create)
    }
}

impl Projection {
    /// Changes how long a projection waits for another process's lock before reporting `projection/busy`. Shared by
    /// every clone; a disabled projection takes no lock, so it has nothing to change.
    pub(crate) fn set_lock_wait(&self, wait: std::time::Duration) {
        if let Self::Enabled(projection) = self {
            projection.set_wait(wait);
        }
    }

    /// Attaches an observer to each step of a skill folder swap, for every clone. A test seam; a disabled projection
    /// swaps nothing.
    pub(crate) fn set_folder_observer(&self, observer: Arc<dyn FolderReplaceObserver + Send + Sync>) {
        if let Self::Enabled(projection) = self {
            projection.inner().set_folder_observer(observer);
        }
    }

    /// Projection as `doctor` and `doctor_resolve` see it: the same settings, hosts, and ownership records as serving,
    /// but composing it never creates a folder under a host's home.
    pub(crate) fn for_doctor(input: ProjectionWiringInput) -> Self {
        Self::compose(input, Folders::ExistingOnly)
    }

    fn compose(value: ProjectionWiringInput, folders: Folders) -> Self {
        let ProjectionWiringInput {
            setup,
            roots,
            filesystem,
        } = value;
        let ProjectionSetup::Enabled(settings) = setup else {
            return Self::Disabled;
        };
        let hosts = settings
            .hosts
            .iter()
            .filter(|host| {
                let installed = host.is_installed();
                if !installed {
                    // Names the host only; the home folder is never created for a host that is not installed.
                    tracing::info!(host = %HostKind::from(&host.render), "agent host not installed, projection skipped");
                }
                installed
            })
            .filter_map(|host| {
                match HostProjection::try_from(HostInput {
                    host,
                    settings: &settings,
                    folders,
                }) {
                    Ok(projection) => Some(projection),
                    Err(error) => {
                        // A home folder that is read-only or in the way must not stop the server serving vaults, so the
                        // host is skipped. Names the host and the stable code only, never a path under the home.
                        tracing::warn!(
                            host = %HostKind::from(&host.render),
                            code = error.code(),
                            "agent host projection skipped: its folders could not be prepared"
                        );
                        None
                    }
                }
            })
            .collect::<Vec<_>>();
        if hosts.is_empty() {
            return Self::Disabled;
        }
        let service = ProjectionService::new(
            Box::new(VaultSetSource::new(roots, filesystem)),
            hosts,
            settings.configuration.clone(),
        );
        Self::Enabled(Arc::new(LockedProjection::from(LockedProjectionConfig::beneath(
            service,
            &settings.state_root,
        ))))
    }
}

/// Creates `folder` if it is missing (when `folders` allows) and wraps it with the ownership record that goes with it.
/// When assessing only, a folder that does not exist is `None`: nothing was ever projected there, and it is not created.
fn host_folder(folder: &Path, state_root: &Path, folders: Folders) -> Result<Option<HostFolder>, ProjectionError> {
    match folders {
        Folders::Create => {
            std::fs::create_dir_all(folder).map_err(|source| ProjectionError::TargetRootUnresolvable { source })?;
        }
        Folders::ExistingOnly if !folder.is_dir() => return Ok(None),
        Folders::ExistingOnly => {}
    }
    let target = ProjectionTarget::try_from(folder)?;
    let manifest = ManifestLocation::for_target(state_root, &target)?;
    Ok(Some(HostFolder::new(target, manifest)))
}

/// One enabled host and the settings it belongs to.
struct HostInput<'a> {
    host: &'a HostSettings,
    settings: &'a ProjectionSettings,
    folders: Folders,
}

impl TryFrom<HostInput<'_>> for HostProjection {
    type Error = ProjectionError;

    fn try_from(
        HostInput {
            host,
            settings,
            folders,
        }: HostInput<'_>,
    ) -> Result<Self, Self::Error> {
        Ok(Self::assessed(
            host_folder(&host.agents, &settings.state_root, folders)?,
            host_folder(&host.skills, &settings.state_root, folders)?,
            host.render.clone(),
        )
        .with_instructions(InstructionsFile::try_from(host.instructions.as_path())?))
    }
}

impl ProjectsDefinitions for Projection {
    fn project(&self, event: &OperationEvent) -> Vec<OperationWarning> {
        match self {
            Self::Disabled => Vec::new(),
            Self::Enabled(projection) => projection.project(event),
        }
    }
}

impl Projection {
    /// What `doctor` should say about every definition and orientation block, read-only, under the projection lock.
    /// A configuration that does not project reports nothing and never takes the lock.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionLockError`] when the lock could not be taken, in which case nothing was read.
    pub(crate) fn assess(&self) -> Result<ProjectionReport, ProjectionLockError> {
        match self {
            Self::Disabled => Ok(ProjectionReport::default()),
            Self::Enabled(projection) => projection.run_locked(ProjectionService::assess),
        }
    }

    /// Repairs every auto-fixable finding under the projection lock. A configuration that does not project repairs
    /// nothing and never takes the lock.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionLockError`] when the lock could not be taken, in which case nothing was changed.
    pub(crate) fn resolve(&self) -> Result<ProjectionReport, ProjectionLockError> {
        match self {
            Self::Disabled => Ok(ProjectionReport::default()),
            Self::Enabled(projection) => projection.run_locked(ProjectionService::resolve),
        }
    }

    /// Brings every host in step with the vaults, as at start-up: each stale, missing, or newly eligible definition
    /// is projected under the host-level lock. Every warning is logged at `WARN` and none stops the caller. Blocking:
    /// run it off an async executor thread.
    pub(crate) fn refresh(&self) {
        let Self::Enabled(projection) = self else {
            return;
        };
        let warnings = match projection.with_lock(ProjectionService::refresh) {
            Ok(warnings) => warnings,
            Err(busy) => vec![busy],
        };
        for warning in warnings {
            tracing::warn!(code = %warning.code, "{}", warning.message);
        }
    }
}

#[cfg(test)]
#[path = "projection_wiring_test.rs"]
mod tests;
