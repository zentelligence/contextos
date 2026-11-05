//! A vault with one saved job, a team, and optionally a task system, behind a real `contextos` session, for the
//! integration tests of the owner gate, the team rules, and the tasks raised for job failures.
//!
//! Everything is a plain string a test can replace: the team, the workflow's script, the job file, the stored state.
//! The defaults describe a valid, ready job owned by `vela`, a team of `atlas` (chief of staff), `vela`, and `piper`,
//! and a vault whose task system accepts the `A` series. Not a test target.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use contextos_web::McpClientSet;
use contextos_web::run_settings::HostContext;
use contextos_workflows::{Clock, JobFingerprint, JobsState};
use time::OffsetDateTime;

use super::{BoxError, host, real_contextos_entry, write_vault_config, write_vault_config_logging_to};

/// The file a logged vault's server writes its log to, inside the config directory.
const SERVER_LOG: &str = "server.log";

pub const VAULT_NAME: &str = "contract-fixture";
pub const WORKFLOW_DIR: &str = "sync";
pub const STATE_PATH: &str = ".contextos/jobs.toml";

/// `atlas` is the chief of staff.
pub const TEAM: &str = "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\nchief_of_staff = true\n\n\
                        [[member]]\nslug = \"vela\"\nname = \"Vela\"\n\n\
                        [[member]]\nslug = \"piper\"\nname = \"Piper\"\ntype = \"human\"\n";
/// A team with no chief of staff.
pub const TEAM_WITHOUT_CHIEF: &str = "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\n\n\
                                      [[member]]\nslug = \"vela\"\nname = \"Vela\"\n";
/// A team with two chiefs of staff.
pub const TEAM_WITH_TWO_CHIEFS: &str = "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\nchief_of_staff = true\n\n\
                                        [[member]]\nslug = \"vela\"\nname = \"Vela\"\nchief_of_staff = true\n";
/// A team without `vela`, the default owner.
pub const TEAM_WITHOUT_VELA: &str = "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\nchief_of_staff = true\n\n\
                                     [[member]]\nslug = \"piper\"\nname = \"Piper\"\ntype = \"human\"\n";
/// A `team.toml` that does not parse.
pub const TEAM_UNPARSABLE: &str = "[[member]\nslug = \n";

/// The package file of the one package, `sync`, listing its one workflow.
pub const PACKAGE: &str = "workflows = [\"sync\"]\n";
pub const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
pub const SCRIPT: &str = "log_info(\"a\");";
/// A script that fails when it runs.
pub const FAILING_SCRIPT: &str = "throw \"the script failed\";";
/// A valid job file owned by `vela`, not yet marked ready.
pub const JOB: &str = "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
/// The same job, marked ready so the validation pass looks at it.
pub const READY_JOB: &str =
    "ready = true\nname = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";

const TASKS_BASE: &str = "filters: file.inFolder(\"memory/tasks\") && file.basename != \"index\"\n\
    views:\n  - type: table\n    name: All\n    order:\n      - file.path\n";
const TASKS_SIDECAR: &str = "---\nkey_prefix: A\nkey_format: '{key_prefix}-{key_id}'\nlast_id: 0\n\
    last_create: 2026-01-01T00:00:00Z\nlast_update: 2026-01-01T00:00:00Z\n---\n";
/// A sidecar that declares no `A` series.
pub const TASKS_SIDECAR_WITHOUT_ACTION_SERIES: &str = "---\nkey_prefix: Q\nkey_format: '{key_prefix}-{key_id}'\nlast_id: 0\n\
    last_create: 2026-01-01T00:00:00Z\nlast_update: 2026-01-01T00:00:00Z\n---\n";

/// What the vault is made of.
#[derive(Clone, Copy, Debug)]
pub struct Spec<'a> {
    /// `.contextos/team.toml`, or none.
    pub team: Option<&'a str>,
    /// `registry/workflows/ops/workflows.toml`.
    pub package: &'a str,
    pub workflow: &'a str,
    pub script: &'a str,
    /// `registry/workflows/ops/sync/jobs/nightly.toml`.
    pub job: &'a str,
    /// `.contextos/jobs.toml`, or none.
    pub state: Option<&'a str>,
    /// Whether the vault has a tasks base and a sidecar declaring the `A` series.
    pub tasks: bool,
}

impl Default for Spec<'_> {
    fn default() -> Self {
        Self {
            team: Some(TEAM),
            package: PACKAGE,
            workflow: WORKFLOW,
            script: SCRIPT,
            job: READY_JOB,
            state: None,
            tasks: true,
        }
    }
}

/// A clock that always reads one instant, in UTC.
#[derive(Clone, Copy, Debug)]
pub struct FixedClock(pub OffsetDateTime);

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

pub fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// The version `spec`'s files have once installed, as validation would store it.
pub fn version_of(spec: &Spec<'_>) -> JobFingerprint {
    let entry = toml::from_str::<toml::Table>(spec.workflow)
        .ok()
        .and_then(|table| table.get("entry").and_then(toml::Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| "sync.rhai".to_owned());
    super::release_fixture::version_of_source(spec.workflow, &entry, spec.script, spec.job)
        // A spec that cannot be installed has the version of nothing, which no stored state matches.
        .unwrap_or_else(|_| {
            JobFingerprint::from(&contextos_workflows::ReleaseFingerprintInput {
                store: contextos_workflows::StoreKind::Vault,
                package: "",
                release: "",
                digest: "",
                job_toml: "",
                release_manifest: "",
                libraries: &[],
            })
        })
}

/// A stored `enabled` job past its trial, scheduled every minute, for the version `spec`'s files have, whose validation
/// passed under the current rules.
pub fn enabled_state(spec: &Spec<'_>) -> String {
    format!(
        "[ops.sync.nightly]\nlifecycle = \"enabled\"\nschedule = \"* * * * *\"\nfingerprint = \"{}\"\n\
         lifecycle_changed = \"2026-09-01T00:00:00Z\"\nlifecycle_actor = \"operator\"\n",
        String::from(version_of(spec))
    )
}

/// A stored `suspended` job.
pub fn suspended_state(spec: &Spec<'_>) -> String {
    format!(
        "[ops.sync.nightly]\nlifecycle = \"suspended\"\nschedule = \"* * * * *\"\nfingerprint = \"{}\"\n\
         lifecycle_changed = \"2026-09-01T00:00:00Z\"\nlifecycle_actor = \"operator\"\n",
        String::from(version_of(spec))
    )
}

/// A stored job in `lifecycle` (`validated` or `registered`) for the version `spec`'s files have.
pub fn lifecycle_state(spec: &Spec<'_>, lifecycle: &str) -> String {
    format!(
        "[ops.sync.nightly]\nlifecycle = \"{lifecycle}\"\nfingerprint = \"{}\"\n",
        String::from(version_of(spec))
    )
}

/// A vault on disk with a connected session, a host, and everything a pass needs.
pub struct JobVault {
    pub vault: tempfile::TempDir,
    pub config: tempfile::TempDir,
    pub clients: Arc<McpClientSet>,
    pub host: HostContext,
}

impl JobVault {
    /// Writes the vault `spec` describes and connects to it.
    pub async fn open(spec: &Spec<'_>) -> Result<Self, BoxError> {
        Self::open_with_log(spec, false).await
    }

    /// As [`JobVault::open`], with the server's log written to [`JobVault::server_log`] instead of stderr, so a test
    /// that makes the server warn on purpose can read the warning back and assert it.
    pub async fn open_logged(spec: &Spec<'_>) -> Result<Self, BoxError> {
        Self::open_with_log(spec, true).await
    }

    async fn open_with_log(spec: &Spec<'_>, logged: bool) -> Result<Self, BoxError> {
        let vault = tempfile::tempdir()?;
        let config = tempfile::tempdir()?;
        let root = vault.path();
        write(root, "index.md", "# Root\n")?;
        write(root, "registry/workflows/ops/workflows.toml", spec.package)?;
        write(root, "registry/workflows/ops/sync/workflow.toml", spec.workflow)?;
        write(root, "registry/workflows/ops/sync/sync.rhai", spec.script)?;
        write(root, "registry/workflows/ops/sync/jobs/nightly.toml", spec.job)?;
        if let Some(team) = spec.team {
            write(root, ".contextos/team.toml", team)?;
        }
        if let Some(state) = spec.state {
            write(root, STATE_PATH, state)?;
        }
        if spec.tasks {
            seed_tasks(root, TASKS_SIDECAR)?;
        }
        super::release_fixture::install_vault_releases(root).map_err(|error| error.to_string())?;
        let this = Self::connect_with_log(vault, config, logged).await?;
        Ok(this)
    }

    /// As [`JobVault::open`], with the workflow also installed as each of `versions` and a ledger kept for the store, so
    /// a test can say which of the releases is current, previous, or future in the state it writes.
    pub async fn open_with_releases(spec: &Spec<'_>, versions: &[&str]) -> Result<Self, BoxError> {
        let vault = Self::open(spec).await?;
        let spare = "registry/workflows/ops/sync/jobs/spare.toml";
        for version in versions {
            vault.write(spare, &READY_JOB.replace("1.0.0", version))?;
            vault.install_releases()?;
        }
        if vault.root().join(spare).exists() {
            std::fs::remove_file(vault.root().join(spare))?;
        }
        vault.rebuild_ledger().await?;
        Ok(vault)
    }

    /// Where the server's log goes when the vault was opened with [`JobVault::open_logged`].
    pub fn server_log(&self) -> PathBuf {
        self.config.path().join(SERVER_LOG)
    }

    /// Connects a new session and host to a vault already on disk: what a restarted scheduler has, with nothing
    /// remembered.
    pub async fn connect(vault: tempfile::TempDir, config: tempfile::TempDir) -> Result<Self, BoxError> {
        Self::connect_with_log(vault, config, false).await
    }

    async fn connect_with_log(
        vault: tempfile::TempDir,
        config: tempfile::TempDir,
        logged: bool,
    ) -> Result<Self, BoxError> {
        let log = config.path().join(SERVER_LOG);
        let config_path = write_vault_config_logging_to(config.path(), vault.path(), logged.then_some(log.as_path()))?;
        let entry = real_contextos_entry("contextos", &config_path)?;
        let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
        let host = host(config.path());
        Ok(Self {
            vault,
            config,
            clients,
            host,
        })
    }

    /// A new session and a new host over the same vault on disk, with the old ones dropped: a restart.
    pub async fn restarted(self) -> Result<Self, BoxError> {
        let Self { vault, config, .. } = self;
        Self::connect(vault, config).await
    }

    pub fn root(&self) -> &Path {
        self.vault.path()
    }

    pub fn write(&self, relative: &str, content: &str) -> std::io::Result<()> {
        write(self.root(), relative, content)
    }

    /// The package engine over the vault's store, with a ledger-aware view of it.
    async fn store_engine(
        client: &contextos_web::McpClient,
    ) -> Result<
        contextos_packages::PackageEngine<
            contextos_web::vault_package_store::VaultStoreIo<&contextos_web::McpClient>,
            contextos_web::package_release_manifest::PackageRelease,
            contextos_core::SystemClock,
            SequentialIds,
        >,
        BoxError,
    > {
        Ok(contextos_packages::PackageEngine {
            io: client.vault_store(VAULT_NAME).await?,
            clock: contextos_core::SystemClock,
            ids: SequentialIds(std::sync::atomic::AtomicU64::new(1)),
            limits: contextos_packages::ReadLimits {
                max_entries: 10_000,
                max_bytes: 256 * 1024 * 1024,
            },
            manifest: std::marker::PhantomData,
        })
    }

    /// Writes the vault store's missing ledger from the releases present, as a store that was installed into has one.
    pub async fn rebuild_ledger(&self) -> Result<(), BoxError> {
        use contextos_packages::PackageStore;
        let client = self.clients.get("contextos").ok_or("no session")?;
        let engine = Self::store_engine(&client).await?;
        let lock = engine.lock().await?;
        engine.rebuild_ledger(&lock).await?;
        Ok(())
    }

    /// Removes the installed release `name` `version` at the operator's request, leaving it recorded as removed.
    pub async fn remove_release(&self, name: &str, version: &str) -> Result<(), BoxError> {
        self.remove_release_as(name, version, contextos_packages::RequestedRemoval::ByRequest)
            .await
    }

    /// Removes the installed release `name` `version` as a forced removal, overriding the guard that a job names it.
    pub async fn remove_release_forced(&self, name: &str, version: &str) -> Result<(), BoxError> {
        self.remove_release_as(name, version, contextos_packages::RequestedRemoval::Forced)
            .await
    }

    async fn remove_release_as(
        &self,
        name: &str,
        version: &str,
        how: contextos_packages::RequestedRemoval,
    ) -> Result<(), BoxError> {
        use contextos_packages::PackageStore;
        let client = self.clients.get("contextos").ok_or("no session")?;
        let engine = Self::store_engine(&client).await?;
        let lock = engine.lock().await?;
        let key = contextos_packages::ReleaseKey {
            name: contextos_packages::ReleaseName::try_from(name)?,
            version: contextos_packages::ReleaseVersion::try_from(version)?,
        };
        engine.remove(&lock, &key, how).await?;
        Ok(())
    }

    /// Installs the vault's source as releases again, so an edit to source made since is a release.
    pub fn install_releases(&self) -> Result<(), BoxError> {
        Ok(super::release_fixture::install_vault_releases(self.root()).map_err(|error| error.to_string())?)
    }

    /// Makes the job file name `version`, so the job follows the release of that version (which may be installed
    /// afterwards with [`Self::install_releases`]).
    pub fn name_version(&self, version: &str) -> Result<(), BoxError> {
        let path = "registry/workflows/ops/sync/jobs/nightly.toml";
        let text = self.read(path)?;
        let replaced: String = text
            .lines()
            .map(|line| {
                if line.starts_with("version = ") {
                    format!("version = \"{version}\"\n")
                } else {
                    format!("{line}\n")
                }
            })
            .collect();
        Ok(self.write(path, &replaced)?)
    }

    /// Installs the vault's source as a new release `version` and makes the job file name it.
    pub fn publish_source_as(&self, version: &str) -> Result<(), BoxError> {
        self.name_version(version)?;
        self.install_releases()
    }

    /// The version the job file now names, as validation works it out from the installed release.
    pub fn current_version(&self) -> Result<JobFingerprint, BoxError> {
        let job = self.read("registry/workflows/ops/sync/jobs/nightly.toml")?;
        Ok(super::release_fixture::vault_version(self.root(), "ops", &job).map_err(|error| error.to_string())?)
    }

    pub fn read(&self, relative: &str) -> std::io::Result<String> {
        std::fs::read_to_string(self.root().join(relative))
    }

    /// One scheduler pass over the vault at `now`.
    pub async fn pass(&self, now: OffsetDateTime) {
        contextos_web::scheduler::poll_once(&self.clients, &self.host, &FixedClock(now)).await;
    }

    /// The stored job state, empty when the file does not exist.
    pub fn state(&self) -> Result<JobsState, BoxError> {
        let path = self.root().join(STATE_PATH);
        if !path.exists() {
            return Ok(JobsState::default());
        }
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }

    /// The lifecycle word of `sync/nightly`.
    pub fn lifecycle(&self) -> Result<&'static str, BoxError> {
        Ok(self.state()?.lifecycle_of("ops", "sync", "nightly").as_str())
    }

    /// Every run record under `memory/jobs`, with its text.
    pub fn run_records(&self) -> Vec<(PathBuf, String)> {
        notes_under(&self.root().join("memory/jobs"), |name| name.starts_with("R-"))
    }

    /// Every task note this crate raised, with its text: any note under `memory/tasks` whose text names an origin
    /// starting `contextos-web:`.
    pub fn raised_tasks(&self) -> Vec<(PathBuf, String)> {
        // Only the task folder: an event about a raised task repeats its origin, and is not a second task.
        notes_under(&self.root().join("memory").join("tasks"), |name| {
            std::path::Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        })
        .into_iter()
        .filter(|(_, text)| text.contains("origin: contextos-web:") || text.contains("origin: 'contextos-web:"))
        .collect()
    }

    /// The tasks raised whose origin has `purpose` (`validation-failed`, `suspended`, or `run-failed`).
    pub fn raised_tasks_for(&self, purpose: &str) -> Vec<(PathBuf, String)> {
        let needle = format!("contextos-web:{purpose}:");
        self.raised_tasks()
            .into_iter()
            .filter(|(_, text)| text.contains(&needle))
            .collect()
    }
}

/// The value of the frontmatter line `key` of a note, without its quotes.
pub fn frontmatter_field(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    text.lines()
        .skip(1)
        .take_while(|line| *line != "---")
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.trim().trim_matches(|c| c == '\'' || c == '"').to_owned())
}

/// Seeds `memory/tasks.base` and its sidecar `sidecar`.
pub fn seed_tasks(root: &Path, sidecar: &str) -> std::io::Result<()> {
    write(root, "memory/tasks.base", TASKS_BASE)?;
    write(root, "memory/tasks.md", sidecar)
}

fn notes_under(directory: &Path, wanted: impl Fn(&str) -> bool + Copy) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().and_then(|name| name.to_str()).is_some_and(wanted)
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                found.push((path, text));
            }
        }
    }
    found.sort();
    found
}

/// A second, independent session and host over the vault of a [`JobVault`], as another scheduler process on the same
/// host would have: nothing is shared with the first but the vault's files, and the vault's folders stay owned by the
/// `JobVault`.
pub struct SecondSession {
    _config: tempfile::TempDir,
    pub clients: Arc<McpClientSet>,
    pub host: HostContext,
}

impl SecondSession {
    /// Connects to the vault `first` wrote.
    pub async fn open_over(first: &JobVault) -> Result<Self, BoxError> {
        let config = tempfile::tempdir()?;
        let config_path = write_vault_config(config.path(), first.root())?;
        let entry = real_contextos_entry("contextos", &config_path)?;
        let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
        let host = host(config.path());
        Ok(Self {
            _config: config,
            clients,
            host,
        })
    }

    /// One scheduler pass over the vault at `now`.
    pub async fn pass(&self, now: OffsetDateTime) {
        contextos_web::scheduler::poll_once(&self.clients, &self.host, &FixedClock(now)).await;
    }
}

/// The stored validation of the version `spec`'s files have, in the shape a release that predates the validator's rules
/// revision and the `by` field wrote: a pass, with no `by` and no `rules`.
pub fn legacy_validation(spec: &Spec<'_>) -> String {
    format!(
        "\n[ops.sync.nightly.validation]\nat = \"2026-09-01T00:00:00Z\"\nfingerprint = \"{}\"\npassed = true\n\
         failures = []\nunverifiable = []\n",
        String::from(version_of(spec))
    )
}

/// An `enabled` job past its trial, as a release that predates the rules revision stored it: its validation passed,
/// and says nothing of who asked for it or under which rules.
pub fn legacy_enabled_state(spec: &Spec<'_>) -> String {
    format!("{}{}", enabled_state(spec), legacy_validation(spec))
}

/// A job in `lifecycle` (`validated` or `registered`) whose stored validation passed under the current rules, made by
/// the validator.
pub fn current_state(spec: &Spec<'_>, lifecycle: &str) -> String {
    format!(
        "{}\n[ops.sync.nightly.validation]\nby = \"validator\"\nrules = {}\nat = \"2026-09-01T00:00:00Z\"\n\
         fingerprint = \"{}\"\npassed = true\nfailures = []\nunverifiable = []\n",
        lifecycle_state(spec, lifecycle),
        contextos_workflows::RULES_REVISION,
        String::from(version_of(spec))
    )
}

/// An `enabled` job past its trial whose stored validation passed under the current rules, made by the validator.
pub fn current_enabled_state(spec: &Spec<'_>) -> String {
    format!(
        "{}\n[ops.sync.nightly.validation]\nby = \"validator\"\nrules = {}\nat = \"2026-09-01T00:00:00Z\"\n\
         fingerprint = \"{}\"\npassed = true\nfailures = []\nunverifiable = []\n",
        enabled_state(spec),
        contextos_workflows::RULES_REVISION,
        String::from(version_of(spec))
    )
}

/// The roster `team` (the text of a `team.toml`) makes, built from its parsed file as the server's own load does.
pub fn roster_of(team: &str) -> Result<contextos_core::TeamRoster, BoxError> {
    let file: contextos_core::TeamFile = toml::from_str(team)?;
    Ok(contextos_core::TeamRoster::try_from(file)?)
}

pub fn summary_of(report: contextos_web::run_report::RunReport) -> contextos_web::run_summary::RunSummary {
    contextos_web::run_summary::RunSummary::finished(
        contextos_web::run_summary::RunInvocation {
            command: contextos_web::run_summary::RunCommand::WorkflowsRun,
            scope: contextos_web::run_summary::RunScope::Vault {
                vault: VAULT_NAME.to_owned(),
                package: "ops".to_owned(),
                workflow: "sync".to_owned(),
                job: None,
                release: None,
            },
            dry_run: false,
            scenario: None,
            overridden_inputs: Vec::new(),
            timings: contextos_web::run_summary::Timings::default(),
        },
        report,
    )
}

/// Operation identifiers in sequence, so a test's engine never repeats one.
pub struct SequentialIds(std::sync::atomic::AtomicU64);

impl contextos_packages::IssuesOperationIds for SequentialIds {
    fn issue(&self) -> contextos_packages::OperationId {
        contextos_packages::OperationId::from(self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}
