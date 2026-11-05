//! `jobs.base` scaffolding-stub contract tests: real
//! `contextos-mcp` sessions against real temporary vault fixtures, per
//! `testing.md`.

mod support;

use std::path::Path;
use std::sync::Arc;

use contextos_web::McpClientSet;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

async fn connect(vault_dir: &Path, config_dir: &Path) -> Result<Arc<McpClientSet>, BoxError> {
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    Ok(Arc::new(McpClientSet::connect(&[entry]).await?))
}

#[tokio::test]
async fn ensure_jobs_base_creates_a_bare_valid_stub_when_absent() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;

    let clients = connect(vault_dir.path(), config_dir.path()).await?;
    let client = clients.get("contextos").ok_or("client not configured")?;
    contextos_web::jobs_base::ensure_jobs_base(&client, VAULT_NAME).await;

    let base_path = vault_dir.path().join("memory/jobs.base");
    assert!(base_path.exists(), "jobs.base should be created when absent");
    let content = std::fs::read_to_string(&base_path)?;
    assert!(
        content.contains("file.inFolder(\"memory/jobs\")"),
        "jobs.base must scope itself to memory/jobs/, mirroring memory/tasks.base's own \
         file.inFolder precedent; got:\n{content}"
    );
    Ok(())
}

#[tokio::test]
async fn ensure_jobs_base_never_overwrites_an_existing_file() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;

    // Simulates the ContextOS Obsidian plugin (or an operator) having
    // already scaffolded a real jobs.base with its own views.
    let scaffolded = "views:\n  - type: table\n    name: My Custom View\n";
    write(vault_dir.path(), "memory/jobs.base", scaffolded)?;

    let clients = connect(vault_dir.path(), config_dir.path()).await?;
    let client = clients.get("contextos").ok_or("client not configured")?;
    contextos_web::jobs_base::ensure_jobs_base(&client, VAULT_NAME).await;

    let content = std::fs::read_to_string(vault_dir.path().join("memory/jobs.base"))?;
    assert_eq!(
        content, scaffolded,
        "an existing jobs.base must never be overwritten, plugin-scaffolded or otherwise"
    );
    Ok(())
}

#[tokio::test]
async fn ensure_jobs_base_run_twice_in_a_row_is_a_no_op_the_second_time() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;

    let clients = connect(vault_dir.path(), config_dir.path()).await?;
    let client = clients.get("contextos").ok_or("client not configured")?;
    contextos_web::jobs_base::ensure_jobs_base(&client, VAULT_NAME).await;
    let base_path = vault_dir.path().join("memory/jobs.base");
    let first = std::fs::read_to_string(&base_path)?;

    // A second call (e.g. the next scheduler poll) must not touch the
    // file it just created either.
    contextos_web::jobs_base::ensure_jobs_base(&client, VAULT_NAME).await;
    let second = std::fs::read_to_string(&base_path)?;
    assert_eq!(
        first, second,
        "a second call must not modify the stub it already created"
    );
    Ok(())
}
