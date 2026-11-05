//! `doctor` and `doctor_resolve`: a schema-valid
//! MCP equivalent of `contextos doctor`, reusing
//! [`crate::doctor::DoctorReport`] unchanged so diagnostic content is
//! identical to the CLI, plus a narrowly scoped remediation tool that acts
//! only on findings `doctor` itself classifies `auto_fixable`.

use std::sync::Arc;

use contextos_core::{
    Origin, RoutesOperations, SystemClock, VaultPath, VaultPathInput, VaultRoot, VaultRootId, VaultSet,
};
use contextos_fs::{Filesystem, FilesystemService, FilesystemServiceConfig};
use contextos_search::{DerivedStore, RebuildTarget, VaultSearchService};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{schemars, tool};
use serde::{Deserialize, Serialize};

use super::doctor_projection;
use crate::Config;
use crate::doctor::{DoctorCheck, DoctorError, DoctorReport, DoctorStatus, LinkSources, Remediation};
use crate::doctor_kind::DoctorKind;
use crate::module::ModuleRegistry;
use crate::projection_wiring::Projection;
use crate::resource_support::fallible_output_schema_for;
use crate::server::{
    ContextOsServer, ManagedGit, ManagedIndexService, Purpose, SearchUnavailable, ServerBuildConfig, ServerBuildError,
};
use crate::tool_error::{ToolError, ToolFailure, evaluate, execute};

#[rmcp::tool_router(router = doctor_tool_router, vis = "pub(crate)")]
impl ContextOsServer {
    #[tool(
        name = "doctor",
        description = "Report actionable, read-only health checks for the effective configuration: configuration validity, per-vault reachability, managed index staleness, Git recovery state, semantic search health, whether every link in the vault resolves to exactly one file (link integrity), and whether each derived store (text index, link graph store, vector store) opens and is intact, naming any other contextos process holding a damaged one. Never writes to a vault; each check reports whether doctor_resolve may act on it automatically. Identical diagnostic content to the contextos doctor CLI command. Every check names its stable kind and, unless it is global, the vault it concerns; set path to one configured vault to assess only that vault (plus the global configuration check).",
        output_schema = fallible_output_schema_for::<DoctorToolResult>()
    )]
    async fn doctor(&self, Parameters(input): Parameters<DoctorInput>) -> Result<Json<DoctorToolResult>, ToolFailure> {
        let target_vault_index = target_vault_index(&self.roots, input.path).await?;
        let config = Arc::clone(&self.config);
        let projection = self.projection.clone();
        let search = Arc::clone(&self.search);
        execute(move || {
            DoctorReport::assess(
                config.as_ref(),
                target_vault_index,
                Some(&projection),
                LinkSources::Live(search.as_ref()),
            )
            .map(DoctorToolResult::from)
            .map_err(ToolError::from)
        })
        .await
    }

    #[tool(
        name = "doctor_resolve",
        description = "Resolve every currently auto-fixable doctor finding (a stale or missing managed index, MCP-made changes left uncommitted, a Git repository that is absent or has no commits yet, a derived store that is damaged, holds a record this build cannot decode, or was written by another version, or an agent host projection that has drifted) by doing what doctor names for it in remediation. Four are tools: vault_index_rebuild, git_commit, git_init, and query_index_rebuild (which moves a damaged store aside, keeping it, before rebuilding it). Three are repairs the server performs itself on the agent host folders under the operator's home, and doctor names no remediation_tool for them: reproject projects a stale, missing, or hand-edited agent or skill again from the vault, and replaces a hand edit of a projected file with what the vault defines; withdraw_projection removes a projected entry the server owns whose definition no longer exists; restore_orientation rewrites only the orientation block between its markers in a host's global instructions, leaving every other line alone. Never acts on a finding doctor classifies as not auto-fixable (an invalid or shadowed definition, a file the server did not project at a projected slug, an entry another configuration owns, an AGENTS.override.md, a malformed orientation block); those always require an operator decision. Projection repairs are host-level, so they run only when path is omitted. Scoped to one configured vault via path, or every configured vault when path is omitted. Initialising Git for a vault that previously had none is a standing behavioural change, not a one-off fix: it starts automatic commits on every future mutation. Set dry_run to preview which findings would be acted on without writing anything.",
        output_schema = fallible_output_schema_for::<DoctorResolveToolResult>()
    )]
    async fn doctor_resolve(
        &self,
        Parameters(input): Parameters<DoctorResolveInput>,
    ) -> Result<Json<DoctorResolveToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let target_vault_index = target_vault_index(&roots, input.path).await?;

        let lock_targets = match target_vault_index {
            Some(index) => vec![index],
            None => (0..roots.len()).collect(),
        };
        let root_ids = lock_targets
            .iter()
            .copied()
            .map(|index| VaultRootId::try_from(index).map_err(ToolError::from))
            .collect::<Result<Vec<_>, _>>()?;
        let guards = self.writes.lock_roots(&root_ids).await?;

        let config = Arc::clone(&self.config);
        let indexes = Arc::clone(&self.indexes);
        let git = Arc::clone(&self.git);
        let search = Arc::clone(&self.search);
        let filesystem = Arc::clone(&self.filesystem);
        let router = Arc::clone(&self.router);
        let projection = self.projection.clone();
        let dry_run = input.dry_run;

        execute(move || {
            if dry_run {
                drop(guards);
                return preview_doctor_findings(config.as_ref(), roots.as_ref(), target_vault_index)
                    .map_err(ToolError::from);
            }
            resolve_doctor_findings(
                &ResolveContext {
                    config: config.as_ref(),
                    roots: roots.as_ref(),
                    indexes: indexes.as_ref(),
                    git: git.as_ref(),
                    search: search.as_ref(),
                    filesystem: filesystem.as_ref(),
                    router: router.as_ref(),
                    projection: &projection,
                    target_vault_index,
                },
                move || drop(guards),
            )
            .map_err(ToolError::from)
        })
        .await
    }
}

/// Resolves an optional vault selector to that vault's index, or `None` when every vault is meant.
async fn target_vault_index(roots: &Arc<VaultSet>, raw: Option<String>) -> Result<Option<usize>, ToolFailure> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let roots = Arc::clone(roots);
    let path = evaluate(move || VaultPath::try_from_vault_selector(&roots, &raw).map_err(ToolError::from)).await?;
    Ok(Some(usize::try_from(path.root_id()).map_err(ToolError::from)?))
}

struct ResolveContext<'a> {
    config: &'a Config,
    roots: &'a VaultSet,
    indexes: &'a [Option<ManagedIndexService>],
    git: &'a [Option<ManagedGit>],
    /// Each vault's live search service, whose own handle on a store is released before the store is moved aside.
    search: &'a [Option<Arc<VaultSearchService>>],
    filesystem: &'a Filesystem,
    /// Where every write the resolution makes is handed, so Git and search learn of it.
    router: &'a dyn RoutesOperations,
    /// Agent host projection, repaired under its own host-level lock, taken after the vault locks the caller holds.
    projection: &'a Projection,
    target_vault_index: Option<usize>,
}

/// How a remediation's outcome is reported, whether the dry run plans it or the resolution performs it.
impl Remediation {
    fn outcome(self, root: &VaultRoot, resolved: bool, message: String) -> DoctorResolveOutcome {
        let kind = match self {
            Self::RebuildIndexes => DoctorKind::ManagedIndexes,
            Self::InitialiseGit => DoctorKind::GitRecovery,
            Self::CommitMcpChanges => DoctorKind::UncommittedChanges,
            Self::RecoverStore(store) | Self::RebuildStore(store) => match store {
                DerivedStore::Text => DoctorKind::TextIndexStore,
                DerivedStore::Graph => DoctorKind::LinkGraphStore,
                DerivedStore::Vectors => DoctorKind::VectorStore,
            },
            // A projection outcome takes its kind and subject from the finding it resolves, which names the host and
            // the definition; see `doctor_projection`.
            Self::Reproject | Self::WithdrawProjection | Self::RestoreOrientation => {
                return DoctorResolveOutcome {
                    subject: self.code().to_owned(),
                    kind: DoctorKind::ProjectionStale,
                    vault_path: Some(root.path().display().to_string()),
                    vault_name: Some(root.name().to_owned()),
                    resolved,
                    message: "not a tool-backed finding".to_owned(),
                };
            }
        };
        DoctorResolveOutcome {
            subject: kind.to_string(),
            kind,
            vault_path: Some(root.path().display().to_string()),
            vault_name: Some(root.name().to_owned()),
            resolved,
            message,
        }
    }

    /// What a dry run reports for this tool-backed remediation: the action the real resolution would take.
    fn planned(self, root: &VaultRoot) -> DoctorResolveOutcome {
        let message = match self {
            Self::RebuildIndexes => "would call vault_index_rebuild".to_owned(),
            Self::InitialiseGit => "would call git_init".to_owned(),
            Self::CommitMcpChanges => "would commit the MCP-made changes".to_owned(),
            Self::RecoverStore(store) => format!(
                "would move the damaged {} aside, keeping it, and rebuild it from the vault",
                store.label()
            ),
            Self::RebuildStore(store) => format!("would rebuild the {} from the vault", store.label()),
            Self::Reproject | Self::WithdrawProjection | Self::RestoreOrientation => {
                "would perform a repair the server makes itself".to_owned()
            }
        };
        self.outcome(root, false, message)
    }
}

/// Who performs a remedy, and so which vault it needs.
pub(super) enum RemedyScope<'a> {
    /// A tool repairs it, so it always has a configured vault.
    Tool {
        /// The vault's index into `Config::vaults`.
        vault_index: usize,
        /// The configured vault.
        root: &'a VaultRoot,
    },
    /// The server repairs the agent host folders itself; the finding names a configured vault only when it concerns
    /// one.
    Host {
        /// The configured vault, when the finding names one.
        root: Option<&'a VaultRoot>,
    },
}

/// One auto-fixable finding to act on: the check that reported it, how it is fixed, and the vault it concerns, if any.
pub(super) struct Remedy<'a> {
    pub(super) check: &'a DoctorCheck,
    pub(super) remediation: Remediation,
    pub(super) scope: RemedyScope<'a>,
}

impl<'a> Remedy<'a> {
    /// The configured vault the finding concerns.
    pub(super) const fn root(&self) -> Option<&'a VaultRoot> {
        match self.scope {
            RemedyScope::Tool { root, .. } => Some(root),
            RemedyScope::Host { root, .. } => root,
        }
    }
}

/// Every auto-fixable finding in `report` within the targeted vaults, in report order. The real resolution and the dry
/// run both select through this, so a preview names exactly the findings the resolution acts on.
///
/// Agent host projection is host-level, not per vault, so its findings are repaired only when no vault is targeted. A
/// tool-backed finding that names no configured vault is never selected.
fn remedies<'a>(report: &'a DoctorReport, roots: &'a VaultSet, target_vault_index: Option<usize>) -> Vec<Remedy<'a>> {
    report
        .checks
        .iter()
        .filter(|check| check.auto_fixable)
        .filter_map(|check| {
            let remediation = check.remediation?;
            if remediation.performed_by_server() {
                if target_vault_index.is_some() {
                    return None;
                }
                return Some(Remedy {
                    check,
                    remediation,
                    scope: RemedyScope::Host {
                        root: check.vault_index.and_then(|index| roots.iter().nth(index)),
                    },
                });
            }
            let vault_index = check.vault_index?;
            if target_vault_index.is_some_and(|target| target != vault_index) {
                return None;
            }
            let root = roots.iter().nth(vault_index)?;
            Some(Remedy {
                check,
                remediation,
                scope: RemedyScope::Tool { vault_index, root },
            })
        })
        .collect()
}

/// Reports what [`resolve_doctor_findings`] would do, from the configuration alone. It needs no service, so it
/// writes nothing, not even the derived state a composed server opens or the Git setup it performs on start.
fn preview_doctor_findings(
    config: &Config,
    roots: &VaultSet,
    target_vault_index: Option<usize>,
) -> Result<DoctorResolveToolResult, DoctorError> {
    let report = DoctorReport::assess(config, None, None, LinkSources::Skip)?;
    let outcomes = remedies(&report, roots, target_vault_index)
        .into_iter()
        .map(|remedy| {
            let RemedyScope::Tool { root, .. } = remedy.scope else {
                return Ok(doctor_projection::planned(&remedy));
            };
            if matches!(remedy.remediation, Remediation::RebuildIndexes) {
                // The real rebuild addresses the vault by its root path, so a root that cannot be addressed fails
                // the preview just as it would fail the resolution.
                vault_path_for(roots, root)?;
            }
            Ok(remedy.remediation.planned(root))
        })
        .collect::<Result<Vec<_>, DoctorError>>()?;
    Ok(DoctorResolveToolResult {
        outcomes,
        report: DoctorToolResult::from(report),
    })
}

/// Resolves every auto-fixable finding. The remediations that write vault content run first, while the caller still
/// holds the vaults' write locks, and the agent host projection repair runs with them (taking its own lock after the
/// vault locks, never before); `release` then gives the vault locks up before any derived store is repaired, since a
/// store's recovery or rebuild writes no vault content and can take long enough to hold every other session's writes
/// past their wait.
fn resolve_doctor_findings(
    ctx: &ResolveContext<'_>,
    release: impl FnOnce(),
) -> Result<DoctorResolveToolResult, DoctorError> {
    let before = DoctorReport::assess(ctx.config, None, Some(ctx.projection), LinkSources::Live(ctx.search));
    let before = match before {
        Ok(before) => before,
        Err(error) => {
            release();
            return Err(error);
        }
    };
    let mut outcomes = Vec::new();
    let mut store_repairs = Vec::new();
    let mut projection_repairs = Vec::new();

    for remedy in remedies(&before, ctx.roots, ctx.target_vault_index) {
        let remediation = remedy.remediation;
        let RemedyScope::Tool { vault_index, root } = remedy.scope else {
            projection_repairs.push(remedy);
            continue;
        };
        let outcome = match remediation {
            Remediation::RebuildIndexes => match resolve_stale_index(ctx, vault_index, root) {
                Ok(outcome) => outcome,
                Err(error) => {
                    release();
                    return Err(error);
                }
            },
            Remediation::InitialiseGit => resolve_git_initialisation(ctx, vault_index, root),
            Remediation::CommitMcpChanges => resolve_uncommitted_mcp_changes(ctx, vault_index, root),
            Remediation::RecoverStore(_) | Remediation::RebuildStore(_) => {
                store_repairs.push((vault_index, root, remediation));
                continue;
            }
            Remediation::Reproject | Remediation::WithdrawProjection | Remediation::RestoreOrientation => {
                projection_repairs.push(remedy);
                continue;
            }
        };
        outcomes.push(outcome);
    }
    // One pass repairs every projection finding; its outcome per finding is judged below against a fresh assessment.
    let projection_pass = (!projection_repairs.is_empty()).then(|| ctx.projection.resolve());
    release();
    for (vault_index, root, remediation) in store_repairs {
        outcomes.push(resolve_store(ctx, vault_index, root, remediation));
    }

    // Reassessed after any attempt, not only a successful one: a remediation that failed part-way may still have
    // written some of its work.
    let report = if outcomes.is_empty() && projection_pass.is_none() {
        before.clone()
    } else {
        DoctorReport::assess(ctx.config, None, Some(ctx.projection), LinkSources::Live(ctx.search))?
    };
    if let Some(pass) = &projection_pass {
        outcomes.extend(doctor_projection::performed(&projection_repairs, pass, &report));
    }
    Ok(DoctorResolveToolResult {
        outcomes,
        report: DoctorToolResult::from(report),
    })
}

fn resolve_stale_index(
    ctx: &ResolveContext<'_>,
    vault_index: usize,
    root: &VaultRoot,
) -> Result<DoctorResolveOutcome, DoctorError> {
    let remediation = Remediation::RebuildIndexes;
    let vault_path = vault_path_for(ctx.roots, root)?;
    let Some(service) = ctx.indexes.get(vault_index).cloned().flatten() else {
        return Ok(remediation.outcome(root, false, "managed indexes are unavailable for this vault".to_owned()));
    };
    let report = service.rebuild_report(
        &vault_path,
        Origin::Tool("doctor_resolve".to_owned()),
        false,
        ctx.router,
    );
    Ok(match report {
        Ok(report) => remediation.outcome(
            root,
            true,
            format!(
                "vault_index_rebuild created {} and updated {} indexes",
                report.indexes_created, report.indexes_updated
            ),
        ),
        Err(error) => remediation.outcome(root, false, error.to_string()),
    })
}

/// Repairs one derived store through the vault's live search service: a damaged store is moved aside, kept, and
/// rebuilt from the vault (the vault queued to be embedded again, for the vector store); any other store needing it is
/// rebuilt in place.
fn resolve_store(
    ctx: &ResolveContext<'_>,
    vault_index: usize,
    root: &VaultRoot,
    remediation: Remediation,
) -> DoctorResolveOutcome {
    let Some(service) = ctx.search.get(vault_index).cloned().flatten() else {
        return remediation.outcome(root, false, "search is unavailable for this vault".to_owned());
    };
    let outcome = match remediation {
        Remediation::RecoverStore(store) | Remediation::RebuildStore(store @ DerivedStore::Vectors) => {
            service.recover_store(store).and_then(|recovery| {
                if store == DerivedStore::Text {
                    service.rebuild(RebuildTarget::Text)?;
                }
                let repopulated = if store == DerivedStore::Vectors {
                    "queued the vault to be embedded again"
                } else {
                    "rebuilt it from the vault"
                };
                Ok(format!(
                    "moved the damaged {} to {} and {repopulated}",
                    store.label(),
                    recovery.moved_to.display()
                ))
            })
        }
        Remediation::RebuildStore(store) => {
            let target = if store == DerivedStore::Text {
                RebuildTarget::Text
            } else {
                RebuildTarget::Graph
            };
            service
                .rebuild(target)
                .map(|_| format!("rebuilt the {} from the vault", store.label()))
        }
        Remediation::RebuildIndexes
        | Remediation::InitialiseGit
        | Remediation::CommitMcpChanges
        | Remediation::Reproject
        | Remediation::WithdrawProjection
        | Remediation::RestoreOrientation => {
            return remediation.outcome(root, false, "not a derived store finding".to_owned());
        }
    };
    match outcome {
        Ok(message) => remediation.outcome(root, true, message),
        Err(error) => remediation.outcome(root, false, format!("{error} ({})", error.code())),
    }
}

/// Commits exactly the paths MCP tools recorded as pending, the same recovery a restarting server performs, and
/// nothing an operator changed outside MCP.
fn resolve_uncommitted_mcp_changes(
    ctx: &ResolveContext<'_>,
    vault_index: usize,
    root: &VaultRoot,
) -> DoctorResolveOutcome {
    let remediation = Remediation::CommitMcpChanges;
    let Some(service) = ctx.git.get(vault_index).cloned().flatten() else {
        return remediation.outcome(root, false, "Git recovery is unavailable for this vault".to_owned());
    };
    match service.recover_staged() {
        Ok(committed) if committed.committed_paths.is_empty() => {
            remediation.outcome(root, false, "no MCP-made changes were left to commit".to_owned())
        }
        Ok(committed) => remediation.outcome(
            root,
            true,
            format!("committed {} MCP-made changes", committed.committed_paths.len()),
        ),
        Err(error) => remediation.outcome(root, false, error.to_string()),
    }
}

fn resolve_git_initialisation(ctx: &ResolveContext<'_>, vault_index: usize, root: &VaultRoot) -> DoctorResolveOutcome {
    let remediation = Remediation::InitialiseGit;
    let Some(service) = ctx.git.get(vault_index).cloned().flatten() else {
        return remediation.outcome(root, false, "Git recovery is unavailable for this vault".to_owned());
    };
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: ctx.filesystem.clone(),
        clock: SystemClock,
    });
    match service.initialise(&writer) {
        Ok(_) => remediation.outcome(root, true, "git_init initialised the repository".to_owned()),
        Err(error) => remediation.outcome(root, false, error.to_string()),
    }
}

/// CLI entry point for `contextos doctor --resolve`: composes the server for `config`, then resolves every
/// auto-fixable finding across every configured vault through the same dispatch `doctor_resolve` uses over MCP, so CLI
/// and MCP stay behaviourally identical for the auto-fixable set.
///
/// The server is composed with Git's start-up work deferred, so changes an earlier process left pending, and a first
/// commit for a repository with none, are made by the resolution and listed among its outcomes, exactly as
/// [`preview_resolution_for_cli`] previews them. A vault whose search state cannot be opened is resolved without
/// search and named in [`DoctorResolveCliReport::search_unavailable`], since search is downstream of the
/// resolution's writes. Every vault's write lock is held for the whole resolution, as the MCP tool holds it, so a
/// server serving the same vaults cannot write alongside it.
///
/// # Errors
///
/// Returns [`DoctorResolveCliError`] when the server cannot be composed, a vault's write lock cannot be taken, or the
/// effective configuration cannot be assessed at all. A remediation failing for one finding is not an error here: it
/// is reported as an unresolved [`DoctorResolveOutcome`] alongside every other finding.
pub async fn resolve_for_cli(config: Config) -> Result<DoctorResolveCliReport, DoctorResolveCliError> {
    let (server, unavailable) = tokio::task::spawn_blocking(move || {
        ContextOsServer::compose(
            ServerBuildConfig {
                config,
                modules: ModuleRegistry::new(),
            },
            Purpose::Resolve,
            crate::ServerClock::default(),
        )
    })
    .await??;
    let search_unavailable = unavailable
        .into_iter()
        .filter_map(|SearchUnavailable { vault_index, reason }| {
            server.roots.iter().nth(vault_index).map(|root| SearchSkipped {
                vault_name: root.name().to_owned(),
                vault_path: root.path().display().to_string(),
                reason,
            })
        })
        .collect();
    let root_ids = (0..server.roots.len())
        .map(|index| VaultRootId::try_from(index).map_err(DoctorError::from))
        .collect::<Result<Vec<_>, _>>()?;
    let guards = server.writes.lock_roots(&root_ids).await?;
    let outcomes = tokio::task::spawn_blocking(move || resolve_composed(&server, guards)).await??;
    Ok(DoctorResolveCliReport {
        outcomes,
        search_unavailable,
    })
}

/// What `contextos doctor --resolve` did.
#[derive(Debug)]
pub struct DoctorResolveCliReport {
    /// One outcome per auto-fixable finding, as the dry run previewed them.
    pub outcomes: Vec<DoctorResolveOutcome>,
    /// The vaults resolved without updating their search indexes, because their search state could not be opened.
    pub search_unavailable: Vec<SearchSkipped>,
}

/// A vault `contextos doctor --resolve` resolved without updating its search indexes.
#[derive(Debug)]
pub struct SearchSkipped {
    pub vault_name: String,
    pub vault_path: String,
    /// Why its search state could not be opened.
    pub reason: String,
}

fn resolve_composed(
    server: &ContextOsServer,
    guards: Vec<crate::write_coordinator::RootGuard>,
) -> Result<Vec<DoctorResolveOutcome>, DoctorResolveCliError> {
    // A one-shot process exits as soon as it has resolved, long before a debounce timer could fire, so what the
    // resolution staged is committed while the write locks are still held, rather than left for a timer that will
    // never run. This happens whatever the resolution's own result: an error part-way through still leaves the writes
    // made before it staged.
    let mut flushed = Ok(());
    let release = || {
        flushed = server.flush_substrates();
        drop(guards);
    };
    let resolved = resolve_doctor_findings(
        &ResolveContext {
            config: server.config.as_ref(),
            roots: server.roots.as_ref(),
            indexes: server.indexes.as_ref(),
            git: server.git.as_ref(),
            search: server.search.as_ref(),
            filesystem: server.filesystem.as_ref(),
            router: server.router.as_ref(),
            projection: &server.projection,
            target_vault_index: None,
        },
        release,
    )
    .map(|result| result.outcomes);
    let mut outcomes = resolved?;
    if let Err(error) = flushed {
        outcomes.push(DoctorResolveOutcome {
            subject: "Git commit".to_owned(),
            kind: DoctorKind::UncommittedChanges,
            vault_path: None,
            vault_name: None,
            resolved: false,
            message: error.to_string(),
        });
    }
    Ok(outcomes)
}

/// Why `contextos doctor --resolve` could not run its resolution at all.
#[derive(Debug, thiserror::Error)]
pub enum DoctorResolveCliError {
    #[error(transparent)]
    Build(#[from] ServerBuildError),
    #[error(transparent)]
    Doctor(#[from] DoctorError),
    #[error("the resolution's blocking task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("another contextos process held a vault's write lock for more than {seconds} seconds")]
    VaultBusy { lock: std::path::PathBuf, seconds: u64 },
    #[error("could not open or take a vault's host write lock {}: {source}", lock.display())]
    HostWriteLock {
        lock: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("could not coordinate writes to the configured vaults: {reason}")]
    WriteCoordination { reason: String },
}

/// Taking every vault's write lock fails only in the ways [`crate::write_coordinator`] documents; the two an
/// operator can act on keep their own variants, and anything else (a coordinator missing a configured root) is an
/// internal fault reported with its description.
impl From<ToolError> for DoctorResolveCliError {
    fn from(value: ToolError) -> Self {
        match value {
            ToolError::VaultBusy { lock, seconds } => Self::VaultBusy { lock, seconds },
            ToolError::HostWriteLock { lock, source } => Self::HostWriteLock { lock, source },
            other => Self::WriteCoordination {
                reason: other.to_string(),
            },
        }
    }
}

/// CLI entry point for `contextos doctor --resolve --dry-run`: reports what [`resolve_for_cli`] would do, through the
/// same selection the `doctor_resolve` tool's dry run uses, from the configuration alone. It composes no server, so
/// nothing is written: no vault file, no Git commit, and no derived state.
///
/// # Errors
///
/// Returns [`DoctorError`] when the configuration's vaults cannot be resolved or assessed.
pub fn preview_resolution_for_cli(config: &Config) -> Result<Vec<DoctorResolveOutcome>, DoctorError> {
    let roots = VaultSet::try_from(config)?;
    preview_doctor_findings(config, &roots, None).map(|result| result.outcomes)
}

fn vault_path_for(roots: &VaultSet, root: &VaultRoot) -> Result<VaultPath, DoctorError> {
    let raw = root
        .path()
        .to_str()
        .ok_or_else(|| DoctorError::NonUtf8VaultPath(root.path().to_path_buf()))?;
    Ok(VaultPath::try_from(VaultPathInput { roots, raw })?)
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DoctorInput {
    /// Assess only one configured vault (plus the global configuration check): a configured vault's bare name, or
    /// a path/`{name}://{relative-path}`; omit to assess every configured vault.
    path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DoctorResolveInput {
    /// Scope resolution to one configured vault: a configured vault's
    /// bare name, or a path/`{name}://{relative-path}`; omit to resolve
    /// every auto-fixable finding across every configured vault.
    path: Option<String>,
    /// Report what would be resolved without writing anything.
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct DoctorResolveToolResult {
    outcomes: Vec<DoctorResolveOutcome>,
    report: DoctorToolResult,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DoctorResolveOutcome {
    pub subject: String,
    /// The stable category of the finding this outcome resolves.
    pub kind: DoctorKind,
    /// The filesystem path of the vault acted on; omitted when the outcome concerns no single configured vault (every
    /// vault, or a projected entry whose source vault is no longer configured).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    pub vault_path: Option<String>,
    /// The configured name of that vault; omitted exactly when `vault_path` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    pub vault_name: Option<String>,
    pub resolved: bool,
    pub message: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct DoctorToolResult {
    checks: Vec<DoctorCheckToolResult>,
    has_failures: bool,
}

impl From<DoctorReport> for DoctorToolResult {
    fn from(value: DoctorReport) -> Self {
        let has_failures = value.has_failures();
        Self {
            checks: value.checks.iter().map(DoctorCheckToolResult::from).collect(),
            has_failures,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct DoctorCheckToolResult {
    /// Human-readable label. Not a stable identifier: group and route by `kind`.
    subject: String,
    /// The stable machine-readable category of this finding.
    kind: DoctorKind,
    /// The configured name of the vault this check concerns; omitted for the global configuration check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    vault_name: Option<String>,
    status: String,
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    action: Option<String>,
    /// Whether `doctor_resolve` may act on this finding without
    /// operator confirmation.
    auto_fixable: bool,
    /// The stable code of what `doctor_resolve` does when `auto_fixable` is `true`: the tool's name when a callable
    /// MCP tool performs the fix, otherwise the code of a repair the server performs itself (`reproject`,
    /// `withdraw_projection`, or `restore_orientation`). Omitted (never `null`) when `auto_fixable` is `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    remediation: Option<String>,
    /// The MCP tool `doctor_resolve` dispatches to when `auto_fixable` is `true` and a callable tool performs the
    /// fix. Omitted (never `null`) when `auto_fixable` is `false` and for a repair the server performs itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    remediation_tool: Option<String>,
}

impl From<&DoctorCheck> for DoctorCheckToolResult {
    fn from(value: &DoctorCheck) -> Self {
        Self {
            subject: value.subject.clone(),
            kind: value.kind,
            vault_name: value.vault_name.clone(),
            status: match value.status {
                DoctorStatus::Pass => "pass".to_owned(),
                DoctorStatus::Fail => "fail".to_owned(),
            },
            message: value.message.clone(),
            action: value.action.clone(),
            auto_fixable: value.auto_fixable,
            remediation: value.remediation.map(|remediation| remediation.code().to_owned()),
            remediation_tool: value.remediation.and_then(Remediation::tool).map(str::to_owned),
        }
    }
}

#[cfg(test)]
#[path = "doctor_test.rs"]
mod tests;
