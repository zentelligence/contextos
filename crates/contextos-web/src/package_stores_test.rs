use tempfile::{TempDir, tempdir};

use super::*;
use crate::retention_fixture::{archive, put_without_retention};
use crate::shared_state::SharedStateStore;

type BoxError = Box<dyn std::error::Error>;

/// A host with a package store, a jobs folder, and a state file in a temporary folder.
struct Host {
    dir: TempDir,
}

impl Host {
    fn with(archives: &[Vec<u8>], state: &str) -> Result<Self, BoxError> {
        let host = Self { dir: tempdir()? };
        for archive in archives {
            put_without_retention(&host.store_root(), archive)?;
        }
        std::fs::create_dir_all(host.dir.path().join("state"))?;
        std::fs::write(host.dir.path().join("state").join("shared-jobs.toml"), state)?;
        Ok(host)
    }

    fn store_root(&self) -> PathBuf {
        self.dir.path().join("packages")
    }

    fn packages(&self) -> SharedPackages {
        SharedPackages {
            store_root: self.store_root(),
            jobs: SavedJobs {
                jobs_root: self.dir.path().join("workflows"),
                state: SharedStateStore::new(self.dir.path().join("state")),
            },
        }
    }

    fn write_job(&self, job: &str, text: &str) -> Result<(), BoxError> {
        let folder = self.dir.path().join("workflows/crm/sync/jobs");
        std::fs::create_dir_all(&folder)?;
        std::fs::write(folder.join(format!("{job}.toml")), text)?;
        Ok(())
    }
}

fn three() -> Result<Vec<Vec<u8>>, BoxError> {
    Ok(vec![archive("1.0.0")?, archive("1.1.0")?, archive("1.2.0")?])
}

const CURRENT_1_2: &str = "[crm.sync]\ncurrent = \"1.2.0\"\n";

#[tokio::test]
async fn the_host_standing_lists_its_releases_and_what_keeps_them() -> Result<(), BoxError> {
    let host = Host::with(&three()?, CURRENT_1_2)?;

    let standing = host.packages().standing().await?;

    assert_eq!(standing.store, "shared");
    assert_eq!(standing.releases.len(), 3);
    let kept: Vec<_> = standing.releases.iter().map(|held| held.kept_by.len()).collect();
    assert_eq!(kept, [0, 0, 1]);
    Ok(())
}

#[tokio::test]
async fn the_host_standing_counts_a_saved_job_file_as_naming_its_release() -> Result<(), BoxError> {
    let host = Host::with(&three()?, CURRENT_1_2)?;
    host.write_job("nightly", "version = \"1.0.0\"\n")?;

    let standing = host.packages().standing().await?;

    let oldest = standing
        .release("crm", &semver::Version::new(1, 0, 0))
        .ok_or("no 1.0.0")?;
    assert_eq!(oldest.kept_by.len(), 1);
    Ok(())
}

#[tokio::test]
async fn one_release_of_the_host_is_reported_and_one_that_is_not_there_is_not_found() -> Result<(), BoxError> {
    let host = Host::with(&three()?, CURRENT_1_2)?;

    let found = host.packages().release("crm", "1.2.0").await?;
    let missing = host.packages().release("crm", "9.0.0").await.err();
    let bad_name = host.packages().release("../crm", "1.2.0").await.err();
    let bad_version = host.packages().release("crm", "1.2").await.err();

    assert_eq!(found.release.to_string(), "crm 1.2.0");
    assert_eq!(missing.map(|error| error.code()), Some("packages/not-found"));
    assert_eq!(bad_name.map(|error| error.code()), Some("packages/invalid-name"));
    assert_eq!(bad_version.map(|error| error.code()), Some("packages/invalid-version"));
    Ok(())
}

mod vault {
    use contextos_workflows::PackageName;

    use super::*;
    use crate::source_install_world::World;
    use crate::vault_source_install::SourceInstall;
    use crate::vault_source_read::{SourceKind, SourceTarget};

    const SYNC: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\n";

    async fn install(world: &World, script: &str) -> Result<(), BoxError> {
        world.write("registry/workflows/ops/workflows.toml", "workflows = [\"sync\"]\n")?;
        world.write("registry/workflows/ops/sync/workflow.toml", SYNC)?;
        world.write("registry/workflows/ops/sync/sync.rhai", script)?;
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

    /// Two releases in the store, with a job file naming the first so the install of the second does not remove it.
    async fn two_releases(world: &World) -> Result<(String, String), BoxError> {
        install(world, "let x = 1;").await?;
        let first = versions(world).first().cloned().unwrap_or_default();
        world.write(
            "registry/workflows/ops/sync/jobs/holder.toml",
            &format!("version = \"{first}\"\n"),
        )?;
        install(world, "let x = 2;").await?;
        let all = versions(world);
        Ok((
            all.first().cloned().unwrap_or_default(),
            all.last().cloned().unwrap_or_default(),
        ))
    }

    #[tokio::test]
    async fn the_standing_of_a_vault_store_shows_where_each_release_came_from_and_what_keeps_it() -> Result<(), BoxError>
    {
        let world = World::new()?;
        let (first, second) = two_releases(&world).await?;
        let client = world.session().await?;

        let standing = VaultPackagesOf {
            client: &client,
            vault: "vault",
        }
        .standing()
        .await?;

        assert_eq!(standing.store, "vault");
        assert_eq!(
            standing
                .releases
                .iter()
                .map(|held| held.release.version.to_string())
                .collect::<Vec<_>>(),
            [first.clone(), second]
        );
        let oldest = standing.releases.first().ok_or("no releases")?;
        assert!(matches!(
            oldest.provenance,
            Some(contextos_packages::EntryProvenance::Installed {
                origin: contextos_packages::ReleaseOrigin::Source,
                ..
            })
        ));
        assert_eq!(oldest.kept_by.len(), 1, "{:?}", oldest.kept_by);
        Ok(())
    }
}
