//! A real package store, shared job files, host state file, and `contextos-mcp` session, in temporary directories, for
//! the integration tests of shared jobs.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contextos_web::McpClientSet;
use contextos_web::job_validation::PassReport;
use contextos_web::shared_claim::SharedJobKey;
use contextos_web::shared_lifecycle::{
    SharedJobChange, SharedPassHost, change_shared_lifecycle_in_release, shared_validation_pass,
};
use contextos_web::shared_state::SharedStateStore;
use contextos_web::shared_validation::SharedValidationHost;
use contextos_workflows::{Actor, JobSchedule, JobsState, ReleaseContext, Transition};
use time::OffsetDateTime;
use time::macros::datetime;

use super::{BoxError, real_contextos_entry, write_vault_config, write_zero_vault_config};

pub const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const OTHER_DIGEST: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const FIRST: OffsetDateTime = datetime!(2026-10-04 03:00:00 UTC);

/// The one job the fixture holds: `acme`'s workflow `sync`, job `nightly`.
pub fn key() -> SharedJobKey {
    SharedJobKey {
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    }
}

pub fn write(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)
}

pub struct SharedFixture {
    pub root: tempfile::TempDir,
    _config: tempfile::TempDir,
    vault_dir: tempfile::TempDir,
    pub clients: Arc<McpClientSet>,
}

impl SharedFixture {
    /// A package `acme` 1.0.0 with one workflow `sync`, the job `nightly` with `job` as its file, and a server with one
    /// vault.
    ///
    /// # Errors
    ///
    /// Returns an error when the directories or the server cannot be set up.
    pub async fn new(job: &str) -> Result<Self, BoxError> {
        Self::connect(job, false).await
    }

    /// As [`SharedFixture::new`], with a server that has no vault at all.
    ///
    /// # Errors
    ///
    /// Returns an error when the directories or the server cannot be set up.
    pub async fn zero_vaults(job: &str) -> Result<Self, BoxError> {
        Self::connect(job, true).await
    }

    async fn connect(job: &str, zero_vaults: bool) -> Result<Self, BoxError> {
        let root = tempfile::tempdir()?;
        let vault = tempfile::tempdir()?;
        std::fs::write(vault.path().join("index.md"), "# Root\n")?;
        let config = tempfile::tempdir()?;
        let config_path = if zero_vaults {
            write_zero_vault_config(config.path())?
        } else {
            write_vault_config(config.path(), vault.path())?
        };
        let entry = real_contextos_entry("contextos", &config_path)?;
        let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
        let fixture = Self {
            root,
            _config: config,
            vault_dir: vault,
            clients,
        };
        fixture.install("1.0.0", DIGEST, "log_info(\"a\");")?;
        write(&fixture.job_path(), job)?;
        Ok(fixture)
    }

    /// Edits the job file to name `release`, as an operator changing the job to run another release would.
    ///
    /// # Errors
    ///
    /// Returns an error when the job file cannot be read or written.
    pub fn name_release(&self, release: &str) -> std::io::Result<()> {
        let text = std::fs::read_to_string(self.job_path())?;
        // Whatever release the file names now is replaced, so a file can be edited away and restored.
        let renamed: Vec<String> = text
            .lines()
            .map(|line| {
                if line.starts_with("version = ") {
                    format!("version = \"{release}\"")
                } else {
                    line.to_owned()
                }
            })
            .collect();
        std::fs::write(self.job_path(), renamed.join("\n") + "\n")
    }

    pub fn store_root(&self) -> PathBuf {
        self.root.path().join("packages")
    }

    pub fn jobs_root(&self) -> PathBuf {
        self.root.path().join("jobs")
    }

    pub fn claims_root(&self) -> PathBuf {
        self.root.path().join("claims")
    }

    pub fn runs_root(&self) -> PathBuf {
        self.root.path().join("runs")
    }

    pub fn job_path(&self) -> PathBuf {
        self.jobs_root().join("acme/sync/jobs/nightly.toml")
    }

    pub fn state(&self) -> SharedStateStore {
        SharedStateStore::new(self.root.path().join("state"))
    }

    /// Installs release `version` of `acme`, whose workflow `sync` has `script` as its entry, with `extra` appended to
    /// the workflow's manifest entry.
    pub fn install_with(&self, version: &str, digest: &str, script: &str, extra: &str) -> std::io::Result<()> {
        let dir = self.store_root().join("acme").join(version);
        write(
            &dir.join("manifest.toml"),
            &format!(
                "name = \"acme\"\nversion = \"{version}\"\ndigest = \"{digest}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n{extra}"
            ),
        )?;
        write(&dir.join("sync.rhai"), script)?;
        super::release_fixture::seal_io(&dir)
    }

    /// Installs release `version` of `acme` whose package declares `modules`, each written beside the entry `script`.
    pub fn install_with_modules(
        &self,
        version: &str,
        digest: &str,
        script: &str,
        modules: &[(&str, &str)],
    ) -> std::io::Result<()> {
        let dir = self.store_root().join("acme").join(version);
        let declared: Vec<String> = modules.iter().map(|(path, _)| format!("{path:?}")).collect();
        write(
            &dir.join("manifest.toml"),
            &format!(
                "name = \"acme\"\nversion = \"{version}\"\ndigest = \"{digest}\"\nmodules = [{}]\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n",
                declared.join(", ")
            ),
        )?;
        for (path, text) in modules {
            write(&dir.join(path), text)?;
        }
        write(&dir.join("sync.rhai"), script)?;
        super::release_fixture::seal_io(&dir)
    }

    /// Installs library `name` `version`, which exports the one module `module` holding `text`.
    pub fn install_library(&self, name: &str, version: &str, module: &str, text: &str) -> std::io::Result<()> {
        let dir = self.store_root().join(name).join(version);
        write(
            &dir.join("manifest.toml"),
            &format!(
                "name = \"{name}\"\nversion = \"{version}\"\ndigest = \"{DIGEST}\"\nmodules = [\"{module}.rhai\"]\nexports = [\"{module}.rhai\"]\n"
            ),
        )?;
        write(&dir.join(format!("{module}.rhai")), text)?;
        super::release_fixture::seal_io(&dir)
    }

    /// Installs release `version` of `acme` with `script` as its entry, depending on each of `dependencies` as `(name,
    /// requirement)`.
    pub fn install_depending_on(
        &self,
        version: &str,
        script: &str,
        dependencies: &[(&str, &str)],
    ) -> std::io::Result<()> {
        let mut extra = String::new();
        for (name, requirement) in dependencies {
            let _ = write!(
                extra,
                "\n[[dependencies]]\nname = \"{name}\"\nrequirement = \"{requirement}\"\n"
            );
        }
        self.install_with(version, DIGEST, script, &extra)
    }

    pub fn install(&self, version: &str, digest: &str, script: &str) -> std::io::Result<()> {
        self.install_with(version, digest, script, "")
    }

    pub async fn pass(&self, now: OffsetDateTime) -> Result<PassReport, BoxError> {
        self.pass_with(now, &["contextos".to_owned()]).await
    }

    pub async fn pass_with(&self, now: OffsetDateTime, known: &[String]) -> Result<PassReport, BoxError> {
        let store = self.state();
        let jobs_root = self.jobs_root();
        let store_root = self.store_root();
        let host = SharedPassHost {
            jobs_root: &jobs_root,
            state: &store,
            validation: SharedValidationHost {
                store_root: &store_root,
                clients: &self.clients,
                primary_server: "contextos",
                known_servers: known,
            },
        };
        Ok(shared_validation_pass(&host, now).await)
    }

    pub async fn change(&self, actor: Actor, transition: Transition, now: OffsetDateTime) -> Result<(), BoxError> {
        self.change_job(&key(), actor, transition, now).await
    }

    /// As [`SharedFixture::change`], for the job `job` of the fixture's package.
    pub async fn change_job(
        &self,
        job: &SharedJobKey,
        actor: Actor,
        transition: Transition,
        now: OffsetDateTime,
    ) -> Result<(), BoxError> {
        // A job whose state records its release is enabled, resumed, and re-activated by whether that release is current, previous, or future, which
        // needs the releases the host holds of its package.
        let installed = std::fs::read_dir(self.store_root().join("acme"))?
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
            .collect();
        let context = ReleaseContext {
            package: "acme".to_owned(),
            predecessor: None,
            installed,
            predecessor_running: false,
            named_release: None,
        };
        change_shared_lifecycle_in_release(
            &self.state(),
            SharedJobChange {
                key: job,
                actor,
                transition,
                now,
            },
            Some(&context),
        )
        .await?;
        Ok(())
    }

    pub fn read(&self) -> Result<JobsState, BoxError> {
        Ok(self.state().read()?)
    }

    pub fn lifecycle(&self) -> Result<&'static str, BoxError> {
        Ok(self.read()?.lifecycle_of("acme", "sync", "nightly").as_str())
    }

    /// Rewrites the host state file with `edit` applied to what it holds, as the operator's tools would leave it.
    pub fn edit_state(&self, edit: impl FnOnce(&mut toml::Table)) -> Result<(), BoxError> {
        let path = self.state().state_path();
        let mut table: toml::Table =
            std::fs::read_to_string(&path).map_or_else(|_| Ok(toml::Table::new()), |text| toml::from_str(&text))?;
        edit(&mut table);
        write(&path, &toml::to_string(&table)?)?;
        Ok(())
    }

    /// Records `releases` (`current`, `previous`, or `future`, each with a version) for `acme`'s `sync` in the host
    /// state file, keeping what the file already holds.
    pub fn record_releases(&self, releases: &[(&str, &str)]) -> Result<(), BoxError> {
        self.edit_state(|table| {
            edit_sync_table(table, |workflow| {
                for (name, version) in releases {
                    workflow.insert((*name).to_owned(), toml::Value::String((*version).to_owned()));
                }
            });
        })
    }

    /// Marks release `version` of `acme`'s `sync` as a retired release in the host state file.
    pub fn retire_release(&self, version: &str) -> Result<(), BoxError> {
        self.edit_state(|table| {
            let mut marker = toml::Table::new();
            marker.insert("cause".to_owned(), toml::Value::String("superseded".to_owned()));
            marker.insert("at".to_owned(), toml::Value::String("2026-09-01T00:00:00Z".to_owned()));
            let mut release = toml::Table::new();
            release.insert("retired".to_owned(), toml::Value::Table(marker));
            edit_sync_table(table, |workflow| {
                workflow.insert(version.to_owned(), toml::Value::Table(release));
            });
        })
    }

    pub fn state_text(&self) -> std::io::Result<String> {
        std::fs::read_to_string(self.state().state_path())
    }

    /// The job's recorded version, once it has been validated.
    pub fn recorded_version(&self) -> Result<contextos_workflows::JobFingerprint, BoxError> {
        self.read()?
            .job("acme", "sync", "nightly")
            .and_then(|job| job.fingerprint().cloned())
            .ok_or_else(|| "the job was not validated".into())
    }

    /// Takes the job to `registered` for the version it has now, through the real pass and transitions.
    pub async fn register(&self) -> Result<(), BoxError> {
        self.pass(FIRST).await?;
        let version = self.recorded_version()?;
        self.change(Actor::DryRun, Transition::DryRunCompleted(version), FIRST)
            .await
    }

    /// Takes the job to `enabled` on `schedule` for the version it has now, through the real pass and transitions. Its
    /// first live run is on trial.
    pub async fn enable_on(&self, schedule: &str, now: OffsetDateTime) -> Result<(), BoxError> {
        self.register().await?;
        let version = self.recorded_version()?;
        self.change(
            Actor::Operator,
            Transition::Enable(JobSchedule::try_from(schedule)?, version),
            now,
        )
        .await
    }

    /// As [`SharedFixture::enable_on`], then passes the trial, as a successful first live run would.
    pub async fn enable(&self) -> Result<(), BoxError> {
        self.enable_on("0 3 * * *", FIRST).await?;
        let version = self.recorded_version()?;
        self.change(Actor::System, Transition::ProbationPassed(version), FIRST)
            .await
    }

    pub fn on_trial(&self) -> Result<bool, BoxError> {
        Ok(self
            .read()?
            .job("acme", "sync", "nightly")
            .is_some_and(contextos_workflows::JobState::first_run_pending))
    }
}

impl SharedFixture {
    /// The root of the one vault the fixture's server has, which no shared job belongs to.
    pub fn vault_root(&self) -> &Path {
        self.vault_dir.path()
    }
}

/// Applies `edit` to the table of `acme`'s workflow `sync` in a host state file's contents, made when it is not there.
fn edit_sync_table(table: &mut toml::Table, edit: impl FnOnce(&mut toml::Table)) {
    let empty = || toml::Value::Table(toml::Table::new());
    if let Some(package) = table.entry("acme").or_insert_with(empty).as_table_mut()
        && let Some(workflow) = package.entry("sync").or_insert_with(empty).as_table_mut()
    {
        edit(workflow);
    }
}
