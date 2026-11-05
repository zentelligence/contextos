use time::macros::datetime;

use super::*;
use crate::release_fixture::install_vault_releases;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TEAM: &str = "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\nchief_of_staff = true\n\n\
                    [[member]]\nslug = \"vela\"\nname = \"Vela\"\n";
const JOB: &str = "ready = true\nname = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
const LIBRARY: &str = "modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n";

/// A vault on disk with a server over it, whose package store holds the releases of the source written below.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn write(&self, path: &str, text: &str) -> TestResult {
        let target = self.dir.path().join("vault").join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, text)?;
        Ok(())
    }

    /// Two workflows of one package that names the library `crm`, each with one ready job, installed as releases.
    fn with_two_workflows_sharing_a_library() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("vault"))?;
        std::fs::write(
            dir.path().join("config.toml"),
            format!(
                "[[vault]]\npath = {:?}\nname = \"vault\"\nstate_directory = \"state\"\n",
                dir.path().join("vault")
            ),
        )?;
        let world = Self { dir };
        let workflow = |entry: &str| format!("name = \"Sync\"\nentry = \"{entry}\"\nmcp_servers.allow = []\n");
        world.write(".contextos/team.toml", TEAM)?;
        world.write(
            "registry/workflows/pair/workflows.toml",
            "workflows = [\"first\", \"second\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \"*\"\n",
        )?;
        for (folder, entry) in [("first", "first.rhai"), ("second", "second.rhai")] {
            world.write(
                &format!("registry/workflows/pair/{folder}/workflow.toml"),
                &workflow(entry),
            )?;
            world.write(
                &format!("registry/workflows/pair/{folder}/{entry}"),
                "import \"crm:client\" as crm;",
            )?;
            world.write(&format!("registry/workflows/pair/{folder}/jobs/nightly.toml"), JOB)?;
        }
        world.write("registry/libraries/crm/library.toml", LIBRARY)?;
        world.write("registry/libraries/crm/client.rhai", "fn id() { 1 }")?;
        install_vault_releases(&world.dir.path().join("vault"))?;
        Ok(world)
    }

    async fn session(&self) -> Result<McpClient, Box<dyn std::error::Error>> {
        use rmcp::ServiceExt;
        let config = self.dir.path().join("config.toml");
        let server = contextos_mcp::ContextOsServer::try_from(contextos_mcp::Config::try_from(
            std::fs::read_to_string(&config)?.as_str(),
        )?)?;
        let (client_io, server_io) = tokio::io::duplex(1 << 20);
        tokio::spawn(async move {
            if let Ok(running) = server.serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        Ok(McpClient::over("contextos", ().serve(client_io).await?))
    }
}

#[tokio::test]
async fn two_jobs_that_reach_one_library_are_validated_against_one_locked_release_of_it() -> TestResult {
    let world = World::with_two_workflows_sharing_a_library()?;
    let client = world.session().await?;
    let file: contextos_core::TeamFile = toml::from_str(TEAM)?;
    let roster = contextos_core::TeamRoster::try_from(file)?;

    let outcome = validation_pass_for(&client, "vault", &[], &roster, datetime!(2026-10-31 03:00:00 UTC)).await?;

    assert_eq!(outcome.observed.len(), 2, "{:?}", outcome.observed);
    assert_eq!(outcome.report.validated, 2, "{:?}", outcome.report);
    assert_eq!(outcome.report.failed, 0, "{:?}", outcome.report);
    let state = std::fs::read_to_string(world.dir.path().join("vault/.contextos/jobs.toml"))?;
    let state: toml::Table = toml::from_str(&state)?;
    let locked = |workflow: &str| {
        state
            .get("pair")
            .and_then(|package| package.get(workflow))
            .and_then(|workflow| workflow.get("nightly"))
            .and_then(|job| job.get("lock"))
            .and_then(|lock| lock.get("crm"))
            .cloned()
    };
    assert!(locked("first").is_some(), "{state:?}");
    assert_eq!(locked("first"), locked("second"));
    Ok(())
}
