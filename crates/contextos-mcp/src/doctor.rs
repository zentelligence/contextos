use std::fmt::{self, Display, Formatter};

use contextos_core::{
    Clock, LocalClock, NoCreationReports, NoSubstrateServices, OperatorTimeZone, Origin, SystemClock, TimeZoneSource,
    VaultPath, VaultPathInput, VaultSet,
};
use contextos_fs::{
    Filesystem, FilesystemConfig, FilesystemService, FilesystemServiceConfig, FsError, FsLimits, ReadTextRequest,
    SearchFilesRequest,
};
use contextos_git::{Git2Vault, Git2VaultConfig};
use contextos_index::{IndexService, IndexServiceConfig};
use contextos_obsidian::FrontmatterDocument;
use contextos_search::{SearchError, SemanticHealth, VectorStoreState};
use thiserror::Error;
use time::UtcOffset;

use crate::doctor_kind::DoctorKind;
use crate::doctor_projection::ProjectionChecks;
use crate::projection_setup::ProjectionSetup;
use crate::projection_wiring::{Projection, ProjectionWiringInput};
use crate::server::semantic_config;
use crate::{Config, ConfigError};
pub(crate) use links::LinkSources;
use links::link_integrity_check;

/// A read-only, actionable assessment of one effective server configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorReport {
    pub(crate) checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    /// Reports whether any check requires operator action before normal service.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.checks.iter().any(|check| check.status == DoctorStatus::Fail)
    }
}

impl TryFrom<&Config> for DoctorReport {
    type Error = DoctorError;

    fn try_from(value: &Config) -> Result<Self, Self::Error> {
        Self::assess(value, None, None, LinkSources::Open)
    }
}

impl DoctorReport {
    /// Assesses `value` without opening any derived state, so a caller that promised to write nothing keeps its word.
    /// The link integrity check, which asks the link graph, reports that it was not made.
    ///
    /// # Errors
    ///
    /// Returns [`DoctorError`] when a check cannot be assessed.
    pub fn without_derived_state(value: &Config) -> Result<Self, DoctorError> {
        Self::assess(value, None, None, LinkSources::Skip)
    }

    /// Assesses the global configuration and, when `target_vault_index` is set, only that configured vault; every
    /// vault otherwise. Scoping avoids the costly per-vault scans for vaults the caller did not ask about.
    ///
    /// Agent host projection is assessed through `projection` when the caller has composed it (a server does), and
    /// otherwise composed here from `value`, in either case without creating any folder.
    pub(crate) fn assess(
        value: &Config,
        target_vault_index: Option<usize>,
        projection: Option<&Projection>,
        links: LinkSources<'_>,
    ) -> Result<Self, DoctorError> {
        let roots = VaultSet::try_from(value)?;
        let filesystem = filesystem_for(value, &roots)?;
        // The same local clock the server writes with, so an index preview stamps exactly as a real rebuild would.
        let clock = LocalClock::from(OperatorTimeZone::try_from(&value.server)?);
        let writer = FilesystemService::from(FilesystemServiceConfig {
            filesystem: filesystem.clone(),
            clock: clock.clone(),
        });
        let zone = clock.zone();
        let setup = ProjectionSetup::try_from(value);
        let configuration = with_projection(
            configuration_check(zone.source(), zone.name().or(zone.rules()), clock.now().offset()),
            &setup,
        );
        let mut checks = vec![configuration];

        let context = VaultContext {
            roots: &roots,
            filesystem: &filesystem,
            writer: &writer,
            clock: &clock,
            links,
        };
        for (index, (vault, root)) in value.vaults.iter().zip(roots.iter()).enumerate() {
            if target_vault_index.is_some_and(|target| target != index) {
                continue;
            }
            checks.extend(vault_checks(&context, index, vault, root)?);
        }
        let composed;
        let projection = if let Some(projection) = projection {
            projection
        } else {
            composed = Projection::for_doctor(ProjectionWiringInput {
                setup: setup.unwrap_or(ProjectionSetup::Disabled),
                roots: roots.clone(),
                filesystem: filesystem.clone(),
            });
            &composed
        };
        checks.extend(Vec::<DoctorCheck>::try_from(ProjectionChecks {
            projection,
            roots: &roots,
            target_vault_index,
        })?);
        Ok(Self { checks })
    }
}

/// What every vault's checks read through.
struct VaultContext<'a> {
    roots: &'a VaultSet,
    filesystem: &'a Filesystem,
    writer: &'a FilesystemService<LocalClock>,
    clock: &'a LocalClock,
    links: LinkSources<'a>,
}

/// The filesystem over every configured vault, with each vault's read limit and hidden folders.
fn filesystem_for(value: &Config, roots: &VaultSet) -> Result<Filesystem, DoctorError> {
    let limits = value
        .vaults
        .iter()
        .map(|vault| {
            Ok(FsLimits {
                max_read_bytes: vault
                    .limits
                    .max_read_mb
                    .checked_mul(1024 * 1024)
                    .ok_or(DoctorError::ReadLimitOverflow)?,
                max_batch_files: vault.limits.max_batch_files,
            })
        })
        .collect::<Result<Vec<_>, DoctorError>>()?;
    let hidden = value
        .vaults
        .iter()
        .map(|vault| vault.hidden.clone())
        .collect::<Vec<_>>();
    Ok(Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits,
        hidden,
        atomic_write_guard: None,
    })?)
}

/// Every check of the vault at `index`: its access, then, for a managed vault, its indexes, Git, stores, and records.
fn vault_checks(
    context: &VaultContext<'_>,
    index: usize,
    vault: &crate::VaultConfig,
    root: &contextos_core::VaultRoot,
) -> Result<Vec<DoctorCheck>, DoctorError> {
    let VaultContext {
        roots,
        filesystem,
        writer,
        clock,
        links,
    } = context;
    let name = root.name();
    let mut checks = vec![
        DoctorCheck::pass(
            DoctorKind::VaultAccess,
            if vault.managed {
                "accessible managed vault"
            } else {
                "accessible filesystem-only vault"
            },
        )
        .with_subject(format!("Vault {}", root.path().display()))
        .for_vault(index, name),
    ];
    if !vault.managed {
        checks.push(DoctorCheck::pass(DoctorKind::ManagedIndexes, "disabled").for_vault(index, name));
        checks.push(DoctorCheck::pass(DoctorKind::GitRecovery, "disabled").for_vault(index, name));
        return Ok(checks);
    }
    checks.push(index_check(vault, root, roots, filesystem, writer, clock)?.for_vault(index, name));
    checks.push(git_check(vault, root, roots).for_vault(index, name));
    checks.push(uncommitted_check(vault, root, roots).for_vault(index, name));
    checks.push(semantic_check(vault, root)?.for_vault(index, name));
    checks.extend(
        stores::store_checks(vault, root)?
            .into_iter()
            .map(|check| check.for_vault(index, name)),
    );
    checks.push(frontmatter_check(root, roots, filesystem)?.for_vault(index, name));
    checks.push(duplicate_ids::duplicate_id_check(root, roots, filesystem)?.for_vault(index, name));
    checks.push(record_conformance::record_conformance_check(root, roots, filesystem)?.for_vault(index, name));
    checks.push(crate::doctor_team::team_config_check(root, roots, filesystem)?.for_vault(index, name));
    checks.push(crate::doctor_fences::fence_vocabulary_check(root, roots, filesystem)?.for_vault(index, name));
    checks.push(link_integrity_check(*links, vault, root, index).for_vault(index, name));
    checks.extend(event_checks_of(index, root, roots, filesystem)?);
    Ok(checks)
}

/// The event checks of one vault, tagged with the vault they apply to.
fn event_checks_of(
    index: usize,
    root: &contextos_core::VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
) -> Result<Vec<DoctorCheck>, DoctorError> {
    Ok(crate::doctor_events::event_checks(root, roots, filesystem)?
        .into_iter()
        .map(|check| check.for_vault(index, root.name()))
        .collect())
}

/// `check` with what the projection setup adds to it: the identity of this configuration when it projects, or the
/// reason it cannot. A projection setting that cannot be applied is a finding about the configuration, not a reason to
/// stop: the vault checks say nothing about it and are still wanted.
fn with_projection(check: DoctorCheck, setup: &Result<ProjectionSetup, ConfigError>) -> DoctorCheck {
    match setup {
        Ok(ProjectionSetup::Enabled(settings)) => {
            let identity: String = <&str>::from(&settings.configuration)
                .chars()
                .take(IDENTITY_CHARACTERS)
                .collect();
            DoctorCheck {
                message: format!("{}; agent host projection identity {identity}", check.message),
                ..check
            }
        }
        Ok(ProjectionSetup::Disabled) => check,
        Err(error) => DoctorCheck::fail(
            DoctorKind::Configuration,
            format!("{}; but {error}", check.message),
            "Correct the [projection] setting the message names, or switch projection off, then restart the server \
             and rerun the doctor.",
        ),
    }
}

/// How many leading hexadecimal characters of a projection identity are shown to the operator.
const IDENTITY_CHARACTERS: usize = 12;

/// The one global check: the configuration is valid (it could not have been loaded otherwise), and the time zone
/// every time written into a vault is shown in. A zone that could not be determined fails it, since UTC then stands
/// in and dates fall on UTC's day rather than the operator's.
pub(crate) fn configuration_check(source: TimeZoneSource, name: Option<&str>, offset: UtcOffset) -> DoctorCheck {
    let (hours, minutes, _) = offset.as_hms();
    let sign = if offset.is_negative() { '-' } else { '+' };
    let zone = format!(
        "time zone {} ({}, {sign}{:02}:{:02})",
        name.unwrap_or("without an IANA name"),
        source.label(),
        hours.unsigned_abs(),
        minutes.unsigned_abs()
    );
    match source {
        TimeZoneSource::Configured | TimeZoneSource::System => DoctorCheck::pass(
            DoctorKind::Configuration,
            format!("valid effective configuration; {zone}"),
        ),
        TimeZoneSource::Undetected => DoctorCheck::fail(
            DoctorKind::Configuration,
            format!(
                "valid effective configuration, but the host's time zone could not be determined, so {zone} stands in"
            ),
            "Set [server] timezone to your IANA time zone, such as \"Australia/Sydney\", and restart the server.",
        ),
    }
}

#[path = "doctor_duplicate_ids.rs"]
mod duplicate_ids;
#[path = "doctor_links.rs"]
mod links;
#[path = "doctor_record_conformance.rs"]
mod record_conformance;
#[path = "doctor_stores.rs"]
mod stores;

#[cfg(test)]
#[path = "doctor_fixture_test.rs"]
mod fixture;

type DoctorIndexService = IndexService<Filesystem, FilesystemService<LocalClock>, LocalClock>;

fn index_check(
    vault: &crate::VaultConfig,
    root: &contextos_core::VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
    writer: &FilesystemService<LocalClock>,
    clock: &LocalClock,
) -> Result<DoctorCheck, DoctorError> {
    if !vault.index_md.enabled {
        return Ok(DoctorCheck::pass(DoctorKind::ManagedIndexes, "disabled"));
    }
    let path = VaultPath::try_from(VaultPathInput {
        roots,
        raw: root
            .path()
            .to_str()
            .ok_or_else(|| DoctorError::NonUtf8VaultPath(root.path().to_path_buf()))?,
    })?;
    let service = DoctorIndexService::try_from(IndexServiceConfig {
        root: root.clone(),
        roots: roots.clone(),
        reader: filesystem.clone(),
        writer: writer.clone(),
        excluded: vault.index_md.exclude.clone(),
        clock: clock.clone(),
        // A dry run writes nothing, so no creation date is ever needed.
        creation: NoCreationReports,
    });
    let report = match service {
        Ok(service) => service.rebuild_report(&path, Origin::Tool("doctor".to_owned()), true, &NoSubstrateServices),
        Err(error) => {
            return Ok(DoctorCheck::fail(
                DoctorKind::ManagedIndexes,
                error.to_string(),
                "Correct the index configuration, and call vault_index_rebuild again.",
            ));
        }
    };
    match report {
        Ok(report) if report.indexes_created == 0 && report.indexes_updated == 0 => Ok(DoctorCheck::pass(
            DoctorKind::ManagedIndexes,
            format!("{} directories are current", report.directories_scanned),
        )),
        Ok(report) => Ok(DoctorCheck::fail_auto_fixable(
            DoctorKind::ManagedIndexes,
            format!(
                "{} indexes are missing and {} are stale",
                report.indexes_created, report.indexes_updated
            ),
            "Call vault_index_rebuild for this vault, then run contextos doctor again.",
            Remediation::RebuildIndexes,
        )),
        Err(error) => Ok(DoctorCheck::fail(
            DoctorKind::ManagedIndexes,
            error.to_string(),
            "Resolve the reported index conflict, call vault_index_rebuild, and rerun the doctor.",
        )),
    }
}

fn git_check(vault: &crate::VaultConfig, root: &contextos_core::VaultRoot, roots: &VaultSet) -> DoctorCheck {
    if !vault.git.enabled {
        return DoctorCheck::pass(DoctorKind::GitRecovery, "disabled");
    }
    let mut protected_restore_paths = vault.git.restore_exclude.clone();
    if vault.oplog.enabled && !protected_restore_paths.contains(&vault.oplog.path) {
        protected_restore_paths.push(vault.oplog.path.clone());
    }
    let protected_restore_paths = crate::server_owned_folders::with_server_owned_restore_paths(protected_restore_paths);
    let service = Git2Vault::try_from(Git2VaultConfig {
        root: root.clone(),
        roots: roots.clone(),
        clock: SystemClock,
        author_name: vault.git.author_name.clone(),
        author_email: vault.git.author_email.clone(),
        allow_destructive_restore: vault.git.destructive_delete,
        protected_restore_paths,
    });
    let service = match service {
        Ok(service) => service,
        Err(error) => {
            return DoctorCheck::fail(
                DoctorKind::GitRecovery,
                error.to_string(),
                "Correct the vault.git configuration, and rerun the doctor.",
            );
        }
    };
    if !service.is_repository() {
        return DoctorCheck::fail_auto_fixable(
            DoctorKind::GitRecovery,
            "enabled, but the vault is not a Git repository",
            "Call git_init for this vault, then rerun the doctor. This starts \
             automatic commits on every future mutation to this vault, a \
             standing behavioural change, not just a one-off fix.",
            Remediation::InitialiseGit,
        );
    }
    match service.has_commits() {
        Ok(true) => {}
        Ok(false) => {
            return DoctorCheck::fail_auto_fixable(
                DoctorKind::GitRecovery,
                "enabled, but the vault's Git repository has no commits yet",
                "Call git_init for this vault to give the repository its first commit, then rerun the doctor. Until \
                 then no change made through MCP can be committed.",
                Remediation::InitialiseGit,
            );
        }
        Err(error) => {
            return DoctorCheck::fail(
                DoctorKind::GitRecovery,
                error.to_string(),
                "Inspect the local Git repository, repair it without rewriting shared history, and rerun the doctor.",
            );
        }
    }
    match service.status() {
        Ok(status) => DoctorCheck::pass(
            DoctorKind::GitRecovery,
            format!(
                "repository is readable on {} with {} pending paths",
                status.branch,
                status.pending_paths.len()
            ),
        ),
        Err(error) => DoctorCheck::fail(
            DoctorKind::GitRecovery,
            error.to_string(),
            "Inspect the local Git repository, repair it without rewriting shared history, and rerun the doctor.",
        ),
    }
}

/// Reports MCP-made changes still uncommitted after the debounce window, which can only mean a commit failed or
/// never ran, since with every mutation arriving through MCP nothing should outlive the window. Edits made outside
/// MCP (in Obsidian, say) are only counted: they are the operator's to commit, never a failure.
fn uncommitted_check(vault: &crate::VaultConfig, root: &contextos_core::VaultRoot, roots: &VaultSet) -> DoctorCheck {
    if !vault.git.enabled {
        return DoctorCheck::pass(DoctorKind::UncommittedChanges, "Git recovery is disabled");
    }
    let service = Git2Vault::try_from(Git2VaultConfig {
        root: root.clone(),
        roots: roots.clone(),
        clock: SystemClock,
        author_name: vault.git.author_name.clone(),
        author_email: vault.git.author_email.clone(),
        allow_destructive_restore: vault.git.destructive_delete,
        protected_restore_paths: Vec::new(),
    });
    let Ok(service) = service else {
        return DoctorCheck::pass(
            DoctorKind::UncommittedChanges,
            "Git recovery is misconfigured; see the Git recovery check",
        );
    };
    if !service.is_repository() {
        return DoctorCheck::pass(
            DoctorKind::UncommittedChanges,
            "the vault is not a Git repository; see the Git recovery check",
        );
    }
    if matches!(service.has_commits(), Ok(false)) {
        return DoctorCheck::pass(
            DoctorKind::UncommittedChanges,
            "the vault's Git repository has no commits yet; see the Git recovery check",
        );
    }
    let window = std::time::Duration::from_secs(vault.git.commit_debounce_s);
    match service.uncommitted(window) {
        Ok(report) if report.overdue_mcp_paths.is_empty() => DoctorCheck::pass(
            DoctorKind::UncommittedChanges,
            format!(
                "no MCP-made changes are awaiting commit ({} changes made outside MCP)",
                report.operator_changes
            ),
        ),
        Ok(report) => DoctorCheck::fail_auto_fixable(
            DoctorKind::UncommittedChanges,
            format!(
                "{} MCP-made changes have been uncommitted for over {} seconds ({} more were made outside MCP and are left alone)",
                report.overdue_mcp_paths.len(),
                vault.git.commit_debounce_s,
                report.operator_changes
            ),
            "Call doctor_resolve to commit exactly the MCP-made changes, then run the doctor again. If it recurs, \
             git_status reports why commits are failing.",
            Remediation::CommitMcpChanges,
        ),
        Err(error) => DoctorCheck::fail(
            DoctorKind::UncommittedChanges,
            error.to_string(),
            "Inspect the local Git repository, repair it without rewriting shared history, and rerun the doctor.",
        ),
    }
}

/// Reports the semantic index's provider configuration, model presence, and vector store contents, without writing:
/// the provider is built from the same configuration the server uses and asked for its dimension (one probe embedding
/// when its configuration does not say), and the vector store is read through a read-only connection, so a vault
/// whose store was never built is reported as such rather than having one created. Model acquisition itself (the
/// pre-fetch tool) is separate; this check only reports whether the configured directory currently holds a usable
/// model.
fn semantic_check(vault: &crate::VaultConfig, root: &contextos_core::VaultRoot) -> Result<DoctorCheck, DoctorError> {
    if !vault.search.semantic {
        return Ok(DoctorCheck::pass(DoctorKind::SemanticSearch, "disabled"));
    }

    let state_directory = crate::state_dir::resolve_state_directory(vault.state_directory.as_deref(), root.path())?;
    let outcome = semantic_config(vault, &state_directory).and_then(|semantic| match semantic {
        Some(semantic) => SemanticHealth::try_from(semantic).map(Some),
        None => Ok(None),
    });

    Ok(match outcome {
        Ok(Some(SemanticHealth {
            store: VectorStoreState::Present(stats),
            ..
        })) => DoctorCheck::pass(
            DoctorKind::SemanticSearch,
            format!(
                "provider and model are available; the vector store holds {} documents across {} chunks",
                stats.documents, stats.chunks
            ),
        ),
        Ok(Some(SemanticHealth {
            store: VectorStoreState::Absent,
            ..
        })) => DoctorCheck::pass(
            DoctorKind::SemanticSearch,
            "provider and model are available; the vector store has not been built yet",
        ),
        Ok(None) => DoctorCheck::pass(DoctorKind::SemanticSearch, "disabled"),
        Err(error) => DoctorCheck::fail(
            DoctorKind::SemanticSearch,
            error.to_string(),
            "Correct [vault.search.embedding] (provider, model_directory, endpoint, and the key: api_key holding \
             it, or api_key_env naming its environment variable), or check this vault's vectors.db permissions \
             under its derived state directory, and rerun the doctor.",
        ),
    })
}

/// Vault-wide YAML frontmatter validity scan. Reuses the same
/// strict parser `frontmatter_read`/`frontmatter_update`
/// (`contextos_obsidian::FrontmatterDocument`) already enforce, so a file
/// this check passes is guaranteed parseable by those tools too. Always
/// `auto_fixable: false`: reformatting or repairing a caller's YAML is out
/// of scope, per the reject-never-repair non-negotiable.
///
/// Reuses `Filesystem::search_files` (the same traversal `fs_search_files`
/// uses, already honouring the vault's `hidden` configuration) rather than
/// a bespoke walk, for consistent behaviour across every enumeration
/// surface. A file that cannot be read as UTF-8 text
/// at all (binary, oversized, or a genuine I/O race) is skipped rather
/// than reported: that is a different problem outside this check's scope,
/// and every other doctor check already leaves file-level read failures to
/// the purpose-built reading tools.
fn frontmatter_check(
    root: &contextos_core::VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
) -> Result<DoctorCheck, DoctorError> {
    const MAX_REPORTED: usize = 10;

    let root_path = VaultPath::try_from(VaultPathInput {
        roots,
        raw: root
            .path()
            .to_str()
            .ok_or_else(|| DoctorError::NonUtf8VaultPath(root.path().to_path_buf()))?,
    })?;
    let matches = filesystem.search_files(&SearchFilesRequest {
        path: root_path,
        pattern: "**/*.md".to_owned(),
        exclude_patterns: Vec::new(),
        max_results: usize::MAX,
    })?;

    let mut invalid = Vec::new();
    for relative in &matches {
        let absolute = root.path().join(relative);
        let Some(raw) = absolute.to_str() else {
            continue;
        };
        let Ok(file_path) = VaultPath::try_from(VaultPathInput { roots, raw }) else {
            continue;
        };
        let Ok(read) = filesystem.read_text(&ReadTextRequest {
            path: file_path,
            limit: None,
        }) else {
            continue;
        };
        if let Err(error) = FrontmatterDocument::try_from(read.content.as_str())
            && !is_valid_once_placeholders_are_substituted(&read.content)
        {
            invalid.push(format!("{relative}: {error}"));
        }
    }

    if invalid.is_empty() {
        return Ok(DoctorCheck::pass(
            DoctorKind::Frontmatter,
            format!("{} markdown files scanned", matches.len()),
        ));
    }
    let mut listed = invalid
        .iter()
        .take(MAX_REPORTED)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    if invalid.len() > MAX_REPORTED {
        use std::fmt::Write as _;
        let _ = write!(listed, ", and {} more", invalid.len() - MAX_REPORTED);
    }
    Ok(DoctorCheck::fail(
        DoctorKind::Frontmatter,
        format!(
            "{} of {} markdown files have invalid YAML frontmatter: {listed}",
            invalid.len(),
            matches.len()
        ),
        "Correct the YAML frontmatter in the listed file(s) without changing the note body, \
         then rerun the doctor.",
    ))
}

/// Whether `relative` exists as a readable file under `root`, used only for a presence check - never returns the
/// file's own content. A read error other than "not found" (a permissions problem, a non-UTF-8 path) is reported
/// as a doctor error rather than silently treated as absent, the same as every other check in this module.
pub(crate) fn file_exists(
    root: &contextos_core::VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
    relative: &str,
) -> Result<bool, DoctorError> {
    let absolute = root.path().join(relative);
    let path = VaultPath::try_from(VaultPathInput {
        roots,
        raw: absolute
            .to_str()
            .ok_or_else(|| DoctorError::NonUtf8VaultPath(absolute.clone()))?,
    })?;
    match filesystem.read_text(&ReadTextRequest { path, limit: None }) {
        Ok(_) => Ok(true),
        Err(FsError::NotFound { .. }) => Ok(false),
        Err(other) => Err(DoctorError::from(other)),
    }
}

/// True when `content`'s frontmatter parses once every `{{...}}` token is
/// replaced by a YAML-safe placeholder scalar. `{{...}}` is the vault's
/// operator-facing "unresolved placeholder" convention (the same one
/// the plugin's `vault_lint.py` `UNRESOLVED_PLACEHOLDER_RE`
/// recognises), used generally, not only under a `templates/` directory.
/// An unquoted `{` opens a YAML flow mapping, so a plain scalar value
/// containing this convention fails strict parsing on syntax alone. This
/// re-check exists to tell that apart from a genuine structural error: the
/// substitution only ever changes placeholder spans, so an unrelated
/// mistake elsewhere in the same frontmatter block still fails both
/// attempts and is still reported.
fn is_valid_once_placeholders_are_substituted(content: &str) -> bool {
    if !content.contains("{{") {
        return false;
    }
    FrontmatterDocument::try_from(substitute_placeholders(content).as_str()).is_ok()
}

/// Replaces every `{{...}}` span with a fixed, unquoted placeholder word.
/// Non-nesting: the first `}}` after a `{{` closes it. A `{{` with no
/// closing `}}` is left untouched (not a well-formed placeholder).
///
/// Deliberately unquoted rather than a quoted scalar (`"placeholder"`):
/// real placeholders are not always the entire value. A quoted
/// replacement breaks in two cases found live against an operator vault:
/// trailing literal text after the placeholder on the same line (`{{Hat
/// Name}} Hat` becomes `"placeholder" Hat`, invalid: text after a closed
/// quoted scalar), and a placeholder already nested inside a quoted
/// string (`"[[{{slug}}]]"` becomes `"[["placeholder"]]"`, injecting `"`
/// characters mid-string). A single unquoted word is a valid YAML plain
/// scalar fragment in both positions.
fn substitute_placeholders(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find("{{") {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        if let Some(end) = after_open.find("}}") {
            result.push_str("PLACEHOLDER");
            rest = &after_open[end + 2..];
        } else {
            result.push_str(&rest[start..]);
            rest = "";
        }
    }
    result.push_str(rest);
    result
}

impl Display for DoctorReport {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "ContextOS MCP doctor")?;
        for check in &self.checks {
            writeln!(formatter, "{} | {} | {}", check.subject, check.status, check.message)?;
            if let Some(action) = &check.action {
                writeln!(formatter, "  Action: {action}")?;
            }
        }
        Ok(())
    }
}

/// What `doctor_resolve` does about an auto-fixable finding: a call to an existing MCP tool, or a repair the server
/// performs itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Remediation {
    /// `vault_index_rebuild`: rebuild the vault's stale or missing managed indexes.
    RebuildIndexes,
    /// `git_init`: create the vault's repository, or give one with no commits its first.
    InitialiseGit,
    /// `git_commit`: commit exactly the changes MCP tools recorded as pending.
    CommitMcpChanges,
    /// `query_index_rebuild`: move a damaged derived store aside, keeping it, and rebuild the store from the vault.
    RecoverStore(contextos_search::DerivedStore),
    /// `query_index_rebuild`: rebuild a derived store from the vault in place.
    RebuildStore(contextos_search::DerivedStore),
    /// Server-performed: project a stale, missing, or hand-edited agent or skill again, replacing a hand edit.
    Reproject,
    /// Server-performed: remove a projected entry whose source definition no longer exists.
    WithdrawProjection,
    /// Server-performed: rewrite only the orientation block in a host's global instructions.
    RestoreOrientation,
}

impl Remediation {
    /// The stable code `doctor` names in `remediation` for every auto-fixable finding. A remediation that calls an MCP
    /// tool uses that tool's name; one the server performs itself has a code of its own.
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::RebuildIndexes => "vault_index_rebuild",
            Self::InitialiseGit => "git_init",
            Self::CommitMcpChanges => "git_commit",
            Self::RecoverStore(_) | Self::RebuildStore(_) => "query_index_rebuild",
            Self::Reproject => "reproject",
            Self::WithdrawProjection => "withdraw_projection",
            Self::RestoreOrientation => "restore_orientation",
        }
    }

    /// Whether the server performs this remediation itself, rather than a client calling a tool for it.
    pub(crate) const fn performed_by_server(self) -> bool {
        self.tool().is_none()
    }

    /// The MCP tool a client could call to perform this remediation, as `doctor` advertises it in `remediation_tool`;
    /// `None` when the server performs it itself, since a client must never be handed something that is not a tool.
    pub(crate) const fn tool(self) -> Option<&'static str> {
        match self {
            Self::RebuildIndexes
            | Self::InitialiseGit
            | Self::CommitMcpChanges
            | Self::RecoverStore(_)
            | Self::RebuildStore(_) => Some(self.code()),
            Self::Reproject | Self::WithdrawProjection | Self::RestoreOrientation => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DoctorCheck {
    pub(crate) subject: String,
    pub(crate) status: DoctorStatus,
    pub(crate) message: String,
    pub(crate) action: Option<String>,
    /// Whether `doctor_resolve` may act on this finding without
    /// operator confirmation. A type-level classification set explicitly
    /// per check, not derived from `action`'s free text.
    pub(crate) auto_fixable: bool,
    /// What `doctor_resolve` does about this finding when `auto_fixable` is `true`. Always `None` when
    /// `auto_fixable` is `false`.
    pub(crate) remediation: Option<Remediation>,
    /// The configured vault this check applies to, as an index into
    /// `Config::vaults` (and every other per-vault `Vec` `ContextOsServer`
    /// holds in lockstep, per `server.rs`). `None` for the one global
    /// `Configuration` check. Required by `doctor_resolve` to
    /// dispatch a remediation to the correct vault in a multi-vault
    /// configuration: `subject` alone is ambiguous, since every vault's
    /// "Managed indexes"/"Git recovery" checks share the same text.
    pub(crate) vault_index: Option<usize>,
    /// The configured name of that vault, as `{name}://` addresses it. `None` exactly when `vault_index` is.
    pub(crate) vault_name: Option<String>,
    /// The stable category of this finding.
    pub(crate) kind: DoctorKind,
}

impl DoctorCheck {
    pub(crate) fn pass(kind: DoctorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            vault_name: None,
            subject: kind.to_string(),
            status: DoctorStatus::Pass,
            message: message.into(),
            action: None,
            auto_fixable: false,
            remediation: None,
            vault_index: None,
        }
    }

    /// A pass that carries advice, for something the operator will need before long but that nothing depends on yet.
    pub(crate) fn pass_advising(kind: DoctorKind, message: impl Into<String>, advice: impl Into<String>) -> Self {
        Self {
            action: Some(advice.into()),
            ..Self::pass(kind, message)
        }
    }

    /// A failure requiring operator judgement: never auto-fixable.
    pub(crate) fn fail(kind: DoctorKind, message: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            kind,
            vault_name: None,
            subject: kind.to_string(),
            status: DoctorStatus::Fail,
            message: message.into(),
            action: Some(action.into()),
            auto_fixable: false,
            remediation: None,
            vault_index: None,
        }
    }

    /// A failure `doctor_resolve` may act on without operator confirmation
    /// by calling `remediation_tool`.
    fn fail_auto_fixable(
        kind: DoctorKind,
        message: impl Into<String>,
        action: impl Into<String>,
        remediation: Remediation,
    ) -> Self {
        Self {
            kind,
            vault_name: None,
            subject: kind.to_string(),
            status: DoctorStatus::Fail,
            message: message.into(),
            action: Some(action.into()),
            auto_fixable: true,
            remediation: Some(remediation),
            vault_index: None,
        }
    }

    /// Tags this check with the configured-vault index it applies to.
    /// Applied by the per-vault loop in `TryFrom<&Config>`; every
    /// constructor above defaults to `None` (global), since a check
    /// function has no reason to know its own position in `Config::vaults`.
    fn for_vault(mut self, index: usize, name: &str) -> Self {
        self.vault_index = Some(index);
        self.vault_name = Some(name.to_owned());
        self
    }

    /// Replaces the subject the kind implies, for the one check whose subject also names the vault path.
    fn with_subject(mut self, subject: String) -> Self {
        self.subject = subject;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DoctorStatus {
    Pass,
    Fail,
}

impl Display for DoctorStatus {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass => formatter.write_str("PASS"),
            Self::Fail => formatter.write_str("FAIL"),
        }
    }
}

/// Typed failures that prevent the read-only doctor from assessing all vaults.
#[derive(Debug, Error)]
pub enum DoctorError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(transparent)]
    Path(#[from] contextos_core::PathError),
    #[error(transparent)]
    Search(#[from] SearchError),
    #[error(transparent)]
    StateDir(#[from] crate::state_dir::StateDirError),
    #[error("configured maximum read size overflows bytes")]
    ReadLimitOverflow,
    #[error("vault path is not valid UTF-8: {0}")]
    NonUtf8VaultPath(std::path::PathBuf),
}

#[cfg(test)]
#[path = "doctor_test.rs"]
mod tests;
