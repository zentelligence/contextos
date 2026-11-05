use contextos_packages::{InstallRequest, ReleaseOrigin};
use tempfile::{TempDir, tempdir};

use super::*;
use crate::host_store::HostStore;
use crate::retention_fixture::bundle;
use crate::shared_state::SharedStateStore;
use contextos_workflows::StoreKind;

type BoxError = Box<dyn std::error::Error>;

/// A host with its store, saved jobs, and state in one folder.
struct Host {
    dir: TempDir,
}

impl Host {
    fn with(versions: &[&str]) -> Result<Self, BoxError> {
        let host = Self { dir: tempdir()? };
        let store = HostStore::new(&host.store_root());
        let lock = store.lock()?;
        for version in versions {
            let extracted = bundle(version)?;
            store.install(
                &lock,
                InstallRequest {
                    manifest_text: extracted.manifest_text,
                    files: extracted.files,
                    origin: ReleaseOrigin::Bundle,
                },
            )?;
        }
        Ok(host)
    }

    fn store_root(&self) -> std::path::PathBuf {
        self.dir.path().join("packages")
    }

    fn saved(&self) -> SavedJobs {
        SavedJobs {
            jobs_root: self.dir.path().join("workflows"),
            state: SharedStateStore::new(self.dir.path().join("state")),
        }
    }

    fn record(&self, state: &str) -> Result<(), BoxError> {
        let recorded: contextos_workflows::JobsState = toml::from_str(state)?;
        self.saved().state.update(|held| *held = recorded)?;
        Ok(())
    }

    fn versions(&self) -> Vec<String> {
        let mut found: Vec<String> = std::fs::read_dir(self.store_root().join("crm"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        found.sort();
        found
    }

    fn future(&self) -> Result<Option<String>, BoxError> {
        Ok(self
            .saved()
            .state
            .read()?
            .package_releases("crm")
            .and_then(|releases| releases.workflows.get("sync"))
            .and_then(|versions| versions.future.as_ref())
            .map(ToString::to_string))
    }
}

/// What a tracing subscriber wrote.
#[derive(Clone, Default)]
struct Log(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Log {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Log {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn a_warning_is_logged_once_under_the_store_it_is_about() {
    let log = Log::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(log.clone())
            .with_ansi(false)
            .finish(),
    );
    tracing::callsite::rebuild_interest_cache();

    let warning = RetentionWarning::raised(StoreKind::Vault, &RetentionError::StateKeptChanging);

    let written =
        String::from_utf8_lossy(&log.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)).into_owned();
    assert_eq!(written.lines().count(), 1, "{written}");
    assert!(written.contains("store=\"vault\""), "{written}");
    assert!(written.contains("code=\"packages/retention-failed\""), "{written}");
    assert_eq!(
        warning.to_string(),
        format!("packages/retention-failed: {}", RetentionError::StateKeptChanging)
    );
}

#[test]
fn the_host_repairs_a_recorded_future_that_names_no_whole_release_even_when_it_removes_nothing() -> Result<(), BoxError>
{
    let host = Host::with(&["1.0.0", "1.1.0"])?;
    host.record("[crm.sync]\ncurrent = \"1.0.0\"\nfuture = \"9.9.9\"\n")?;

    retain_host(&host.store_root(), &host.saved())?;

    assert_eq!(host.versions(), ["1.0.0", "1.1.0"]);
    assert_eq!(host.future()?.as_deref(), Some("1.1.0"));
    Ok(())
}

mod vault {
    use contextos_workflows::PackageName;

    use super::*;
    use crate::source_install_world::World;
    use crate::vault_source_install::SourceInstall;
    use crate::vault_source_read::{SourceKind, SourceTarget};

    const SYNC: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\n";

    fn lay_out(world: &World, script: &str) -> Result<(), BoxError> {
        world.write("registry/workflows/ops/workflows.toml", "workflows = [\"sync\"]\n")?;
        world.write("registry/workflows/ops/sync/workflow.toml", SYNC)?;
        world.write("registry/workflows/ops/sync/sync.rhai", script)?;
        Ok(())
    }

    /// Installs the source as a new release without running retention, as an install did before it ran retention.
    async fn install(world: &World) -> Result<(), BoxError> {
        let client = world.session().await?;
        SourceInstall {
            client: &client,
            vault: "vault",
        }
        .install(
            &SourceTarget {
                kind: SourceKind::Workflows,
                name: PackageName::try_from("ops")?,
            },
            None,
        )
        .await?;
        Ok(())
    }

    fn versions(world: &World) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(world.vault().join(".contextos/packages/ops")) else {
            return Vec::new();
        };
        let mut found: Vec<String> = entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        found.sort();
        found
    }

    /// Two releases in the store: a job file names the first while the second is installed, and is removed after.
    async fn two_releases(world: &World) -> Result<(), BoxError> {
        lay_out(world, "let x = 1;")?;
        install(world).await?;
        let first = versions(world).first().cloned().unwrap_or_default();
        let job = "registry/workflows/ops/sync/jobs/holder.toml";
        world.write(job, &format!("version = \"{first}\"\n"))?;
        lay_out(world, "let x = 2;")?;
        install(world).await?;
        std::fs::remove_file(world.vault().join(job))?;
        Ok(())
    }

    #[tokio::test]
    async fn a_vault_removes_the_releases_nothing_keeps_and_records_the_newest_as_future() -> Result<(), BoxError> {
        let world = World::new()?;
        two_releases(&world).await?;
        world.write(".contextos/jobs.toml", "")?;
        let before = versions(&world);
        let client = world.session().await?;

        retain_vault(&client, "vault").await?;

        assert!(before.len() >= 2, "{before:?}");
        assert_eq!(versions(&world).len(), 1, "{:?}", versions(&world));
        assert_eq!(versions(&world), [before.last().cloned().unwrap_or_default()]);
        Ok(())
    }

    #[tokio::test]
    async fn a_vault_repairs_a_recorded_future_that_names_no_whole_release_even_when_it_removes_nothing()
    -> Result<(), BoxError> {
        let world = World::new()?;
        two_releases(&world).await?;
        let before = versions(&world);
        let (oldest, newest) = (
            before.first().cloned().unwrap_or_default(),
            before.last().cloned().unwrap_or_default(),
        );
        // The oldest is the current release, so retention removes nothing, and the recorded future names no release.
        world.write(
            ".contextos/jobs.toml",
            &format!("[ops.sync]\ncurrent = \"{oldest}\"\nfuture = \"9.9.9\"\n"),
        )?;
        let client = world.session().await?;

        retain_vault(&client, "vault").await?;

        assert_eq!(versions(&world), before);
        let state: contextos_workflows::JobsState = toml::from_str(&world.read(".contextos/jobs.toml")?)?;
        let future = state
            .package_releases("ops")
            .and_then(|releases| releases.workflows.get("sync"))
            .and_then(|versions| versions.future.as_ref())
            .map(ToString::to_string);
        assert_eq!(future, Some(newest));
        Ok(())
    }

    #[tokio::test]
    async fn a_vault_whose_state_names_a_stale_future_release_has_it_rewritten() -> Result<(), BoxError> {
        let world = World::new()?;
        two_releases(&world).await?;
        let before = versions(&world);
        let (oldest, newest) = (
            before.first().cloned().unwrap_or_default(),
            before.last().cloned().unwrap_or_default(),
        );
        world.write(".contextos/jobs.toml", &format!("[ops.sync]\nfuture = \"{oldest}\"\n"))?;
        let client = world.session().await?;

        retain_vault(&client, "vault").await?;

        let state: contextos_workflows::JobsState = toml::from_str(&world.read(".contextos/jobs.toml")?)?;
        let future = state
            .package_releases("ops")
            .and_then(|releases| releases.workflows.get("sync"))
            .and_then(|versions| versions.future.as_ref())
            .map(ToString::to_string);
        assert_eq!(future, Some(newest));
        Ok(())
    }
}
