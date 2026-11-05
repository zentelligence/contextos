//! The per-concern steps of composing a server: each builds one family of per-vault services from the configuration,
//! so [`ContextOsServer::compose`](crate::server::ContextOsServer) reads as the order they are wired in.

use std::path::PathBuf;
use std::sync::Arc;

use contextos_core::{
    FirstReportedCreation, OperationRouter, OperationRouterConfig, VaultPath, VaultPathInput, VaultRootId, VaultSet,
};
use contextos_fs::{FileNameIndex, Filesystem, FilesystemConfig, FilesystemService, FsLimits};
use contextos_git::{Git2Vault, Git2VaultConfig};
use contextos_index::{IndexService, IndexServiceConfig};
use contextos_oplog::{OperationLog, OperationLogConfig};
use contextos_search::VaultSearchService;

use crate::commit_schedule::{TokioScheduler, VersionCollection};
use crate::projection_setup::ProjectionSetup;
use crate::projection_wiring::{Projection, ProjectionWiringInput};
use crate::server::{
    IndexCollection, MAX_INDEXED_FILES, ManagedGit, ManagedIndexService, ManagedOperationLog, OperationLogCollection,
    Purpose, SearchCollection, SearchUnavailable, SubstrateRouter, VaultClock, open_vault_search,
};
use crate::vault_events::{VaultEvents, VaultEventsConfig, VaultEventsSetting};
use crate::write_coordinator::{WriteCoordinator, WriteCoordinatorConfig};
use crate::{Config, ServerBuildError};

/// The filesystem over every vault, with each vault's read limits and hidden patterns.
pub(crate) fn filesystem(config: &Config, roots: &VaultSet) -> Result<Filesystem, ServerBuildError> {
    let limits = config
        .vaults
        .iter()
        .map(|vault| {
            Ok(FsLimits {
                max_read_bytes: vault
                    .limits
                    .max_read_mb
                    .checked_mul(1024 * 1024)
                    .ok_or(ServerBuildError::ReadLimitOverflow)?,
                max_batch_files: vault.limits.max_batch_files,
            })
        })
        .collect::<Result<Vec<_>, ServerBuildError>>()?;
    Ok(Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits,
        hidden: config.vaults.iter().map(|vault| vault.hidden.clone()).collect(),
        atomic_write_guard: None,
    })?)
}

/// Settings the server keeps per vault, in configuration order, for tools to consult.
pub(crate) struct VaultSettings {
    pub(crate) destructive_delete: Vec<bool>,
    pub(crate) rebuild_budget_seconds: Vec<u64>,
    pub(crate) resources_list_include: Vec<Vec<String>>,
}

impl From<&Config> for VaultSettings {
    fn from(config: &Config) -> Self {
        Self {
            destructive_delete: config.vaults.iter().map(|vault| vault.git.destructive_delete).collect(),
            rebuild_budget_seconds: config
                .vaults
                .iter()
                .map(|vault| vault.search.rebuild_budget_seconds)
                .collect(),
            resources_list_include: config
                .vaults
                .iter()
                .map(|vault| vault.resources_list_include.clone())
                .collect(),
        }
    }
}

/// Each managed vault's Git service, `None` for a vault that is unmanaged or has Git off.
pub(crate) fn git_services(
    config: &Config,
    roots: &VaultSet,
    clock: &VaultClock,
) -> Result<Vec<Option<ManagedGit>>, ServerBuildError> {
    config
        .vaults
        .iter()
        .zip(roots.iter())
        .map(|(vault, root)| {
            if !vault.managed || !vault.git.enabled {
                return Ok(None);
            }
            let mut protected_restore_paths = vault.git.restore_exclude.clone();
            if vault.oplog.enabled && !protected_restore_paths.contains(&vault.oplog.path) {
                protected_restore_paths.push(vault.oplog.path.clone());
            }
            Git2Vault::try_from(Git2VaultConfig {
                root: root.clone(),
                roots: roots.clone(),
                clock: clock.clone(),
                author_name: vault.git.author_name.clone(),
                author_email: vault.git.author_email.clone(),
                allow_destructive_restore: vault.git.destructive_delete,
                protected_restore_paths: crate::server_owned_folders::with_server_owned_restore_paths(
                    protected_restore_paths,
                ),
            })
            .map(Some)
            .map_err(ServerBuildError::Git)
        })
        .collect()
}

/// What composing the managed `index.md` services reads and writes through, and the Git services that date files.
pub(crate) struct IndexWiring<'a> {
    pub(crate) config: &'a Config,
    pub(crate) roots: &'a VaultSet,
    pub(crate) filesystem: &'a Filesystem,
    pub(crate) writer: &'a FilesystemService<VaultClock>,
    pub(crate) clock: &'a VaultClock,
    pub(crate) git: &'a [Option<ManagedGit>],
}

/// Each managed vault's `index.md` service, `None` for a vault that is unmanaged or has it off.
pub(crate) fn index_services(wiring: &IndexWiring<'_>) -> Result<Vec<Option<ManagedIndexService>>, ServerBuildError> {
    wiring
        .config
        .vaults
        .iter()
        .zip(wiring.roots.iter())
        .zip(wiring.git.iter())
        .map(|((vault, root), vault_git)| {
            if !vault.managed || !vault.index_md.enabled {
                return Ok(None);
            }
            IndexService::try_from(IndexServiceConfig {
                root: root.clone(),
                roots: wiring.roots.clone(),
                reader: wiring.filesystem.clone(),
                writer: wiring.writer.clone(),
                excluded: vault.index_md.exclude.clone(),
                clock: wiring.clock.clone(),
                // A file with no `created` is dated from its first commit where the vault is versioned, else from
                // the filesystem's own birth time; never from the rebuild that happens to find it.
                creation: FirstReportedCreation {
                    primary: vault_git.clone(),
                    fallback: wiring.filesystem.clone(),
                },
            })
            .map(Some)
            .map_err(ServerBuildError::Index)
        })
        .collect()
}

/// Each managed vault's operation log, `None` for a vault that is unmanaged or has it off.
pub(crate) fn operation_log_services(
    config: &Config,
    roots: &VaultSet,
    writer: &FilesystemService<VaultClock>,
) -> Result<Vec<Option<ManagedOperationLog>>, ServerBuildError> {
    config
        .vaults
        .iter()
        .zip(roots.iter())
        .map(|(vault, root)| {
            if !vault.managed || !vault.oplog.enabled {
                return Ok(None);
            }
            let relative_directory = vault
                .oplog
                .path
                .to_str()
                .ok_or_else(|| ServerBuildError::NonUtf8OperationLogPath {
                    path: vault.oplog.path.clone(),
                })?
                .to_owned();
            OperationLog::try_from(OperationLogConfig {
                root: root.clone(),
                roots: roots.clone(),
                relative_directory,
                appender: writer.clone(),
            })
            .map(Some)
            .map_err(ServerBuildError::OperationLog)
        })
        .collect()
}

/// Git's start-up work for a server that goes on to serve: maintaining the ignore rules, giving a repository with no
/// commits its first commit, and committing changes an earlier process left pending.
pub(crate) fn start_git(
    git: &[Option<ManagedGit>],
    writer: &FilesystemService<VaultClock>,
) -> Result<(), ServerBuildError> {
    for service in git.iter().flatten() {
        if service.is_repository() {
            service.initialise(writer).map_err(ServerBuildError::GitWrite)?;
        }
        service.recover_staged().map_err(ServerBuildError::Git)?;
    }
    Ok(())
}

/// Each managed vault's search service, `None` for a vault that is unmanaged or has no search enabled.
pub(crate) type SearchServices = Vec<Option<Arc<VaultSearchService>>>;

/// Each managed vault's search service with the vaults whose search could not be opened. A server that goes on to
/// serve fails over such a vault instead.
pub(crate) fn search_services(
    config: &Config,
    roots: &VaultSet,
    purpose: Purpose,
) -> Result<(SearchServices, Vec<SearchUnavailable>), ServerBuildError> {
    let mut unavailable = Vec::new();
    let services = config
        .vaults
        .iter()
        .zip(roots.iter())
        .enumerate()
        .map(|(index, (vault, root))| {
            let enabled = vault.search.text || vault.search.graph || vault.search.semantic;
            if !vault.managed || !enabled {
                return Ok(None);
            }
            let root_id = VaultRootId::try_from(index)?;
            match (open_vault_search(vault, root, root_id, purpose), purpose) {
                (Ok(service), _) => Ok(Some(Arc::new(service))),
                (Err(error), Purpose::Serve) => Err(error),
                (Err(error), Purpose::Resolve) => {
                    unavailable.push(SearchUnavailable {
                        vault_index: index,
                        reason: error.to_string(),
                    });
                    Ok(None)
                }
            }
        })
        .collect::<Result<Vec<_>, ServerBuildError>>()?;
    Ok((services, unavailable))
}

/// The projection a server writes through and the one `doctor` assesses.
///
/// A server that is going to serve refuses a bad `[projection]` section, so it never starts over one. The one-shot
/// doctor resolution does not: it still repairs every vault finding, projects nothing, and the doctor report it ends
/// with names the projection setting as a failed configuration check. Only a server that goes on to serve projects:
/// the one-shot doctor resolution reports every write it makes, so no vault write it makes is routed to a host
/// folder, and it reaches one only through the repairs it lists. Either way `doctor` and `doctor_resolve` see the same
/// projection, composed without creating any folder when the server is not serving.
pub(crate) fn projections(
    config: &Config,
    roots: &VaultSet,
    filesystem: &Filesystem,
    purpose: Purpose,
) -> Result<(Projection, Projection), ServerBuildError> {
    let setup = match (ProjectionSetup::try_from(config), purpose) {
        (Ok(setup), _) => setup,
        (Err(error), Purpose::Serve) => return Err(error.into()),
        (Err(_), Purpose::Resolve) => ProjectionSetup::Disabled,
    };
    let wiring = |setup| ProjectionWiringInput {
        setup,
        roots: roots.clone(),
        filesystem: filesystem.clone(),
    };
    Ok(match (setup, purpose) {
        (setup @ ProjectionSetup::Enabled(_), Purpose::Serve) => {
            let projection = Projection::from(wiring(setup));
            (projection.clone(), projection)
        }
        (setup @ ProjectionSetup::Enabled(_), Purpose::Resolve) => {
            (Projection::for_doctor(wiring(setup)), Projection::Disabled)
        }
        _ => (Projection::Disabled, Projection::Disabled),
    })
}

/// Each vault's derived-state directory.
pub(crate) fn state_directories(config: &Config, roots: &VaultSet) -> Result<Vec<PathBuf>, ServerBuildError> {
    Ok(config
        .vaults
        .iter()
        .zip(roots.iter())
        .map(|(vault, root)| crate::state_dir::resolve_state_directory(vault.state_directory.as_deref(), root.path()))
        .collect::<Result<Vec<_>, _>>()?)
}

/// The consumer that fires events from completed writes, for every vault.
pub(crate) fn vault_events(
    config: &Config,
    state_directories: &[PathBuf],
    parts: VaultEventsParts,
) -> Result<VaultEvents, ServerBuildError> {
    Ok(VaultEvents::try_from(VaultEventsConfig {
        roots: parts.roots,
        filesystem: parts.filesystem,
        writer: parts.writer,
        clock: parts.clock,
        vaults: config
            .vaults
            .iter()
            .zip(state_directories)
            .map(|(vault, state_directory)| VaultEventsSetting {
                enabled: vault.managed,
                state_directory: state_directory.clone(),
            })
            .collect(),
    })?)
}

/// What the events consumer reads and writes through.
pub(crate) struct VaultEventsParts {
    pub(crate) roots: VaultSet,
    pub(crate) filesystem: Filesystem,
    pub(crate) writer: FilesystemService<VaultClock>,
    pub(crate) clock: VaultClock,
}

/// Each vault's index of file names.
pub(crate) fn file_name_indexes(
    roots: &VaultSet,
    filesystem: &Filesystem,
) -> Result<Vec<FileNameIndex>, ServerBuildError> {
    roots
        .iter()
        .map(|root| {
            let vault_root = VaultPath::try_from(VaultPathInput {
                roots,
                raw: &format!("{}://.", root.name()),
            })?;
            Ok(FileNameIndex::new(filesystem.clone(), vault_root, MAX_INDEXED_FILES))
        })
        .collect()
}

/// The services the write pipeline hands a completed mutation to.
pub(crate) struct RouterParts {
    pub(crate) indexes: Arc<Vec<Option<ManagedIndexService>>>,
    pub(crate) operation_logs: Arc<Vec<Option<ManagedOperationLog>>>,
    pub(crate) git: Arc<Vec<Option<ManagedGit>>>,
    pub(crate) debounce_seconds: Arc<Vec<u64>>,
    pub(crate) search: Arc<SearchServices>,
    pub(crate) projection: Projection,
    pub(crate) events: VaultEvents,
}

/// The router that delivers each completed mutation to the index, operation log, Git, search, projection, and events.
pub(crate) fn router(parts: RouterParts) -> SubstrateRouter {
    OperationRouter::from(OperationRouterConfig {
        indexes: IndexCollection {
            services: parts.indexes,
        },
        operation_log: OperationLogCollection {
            services: parts.operation_logs,
        },
        versions: VersionCollection::new(parts.git, parts.debounce_seconds, Arc::new(TokioScheduler)),
        search: SearchCollection { services: parts.search },
        projection: parts.projection,
        events: parts.events,
    })
}

/// The write locks of every vault, whose host lock files live in the vaults' state directories.
pub(crate) fn write_coordinator(state_directories: &[PathBuf]) -> WriteCoordinator {
    WriteCoordinator::from(WriteCoordinatorConfig::from(
        state_directories
            .iter()
            .map(|directory| directory.join(contextos_fs::HOST_WRITE_LOCK))
            .collect::<Vec<_>>(),
    ))
}
