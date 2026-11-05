//! Every auto-fixable `doctor` finding names its stable remediation code in `remediation`, whether a callable MCP tool
//! performs the fix (the code is then the tool's name, which `remediation_tool` repeats) or the server does. A finding
//! that is not auto-fixable names neither.

mod support;
use contextos_mcp::ContextOsServer;
use serde_json::{Value, json};
use support::git_harness::call;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

async fn checks(server: &ContextOsServer) -> Result<Vec<Value>, BoxError> {
    let result = call(server, "doctor", json!({})).await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let report = result
        .structured_content
        .ok_or("doctor omitted its structured result")?;
    Ok(report["checks"].as_array().cloned().unwrap_or_default())
}

fn of_kind<'a>(checks: &'a [Value], kind: &str) -> Result<&'a Value, BoxError> {
    checks
        .iter()
        .find(|check| check["kind"] == kind)
        .ok_or_else(|| format!("no {kind} check reported").into())
}

#[tokio::test]
async fn a_tool_backed_finding_names_its_tool_as_both_remediation_and_remediation_tool() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = true;
    let server = ContextOsServer::try_from(config)?;
    git2::Repository::init(vault.path())?;

    let checks = checks(&server).await?;

    let indexes = of_kind(&checks, "managed_indexes")?;
    assert_eq!(indexes["auto_fixable"], true, "{indexes}");
    assert_eq!(indexes["remediation"], "vault_index_rebuild", "{indexes}");
    assert_eq!(indexes["remediation_tool"], "vault_index_rebuild", "{indexes}");
    let git = of_kind(&checks, "git_recovery")?;
    assert_eq!(git["auto_fixable"], true, "{git}");
    assert_eq!(git["remediation"], "git_init", "{git}");
    assert_eq!(git["remediation_tool"], "git_init", "{git}");
    Ok(())
}

#[tokio::test]
async fn a_finding_that_is_not_auto_fixable_names_no_remediation() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("broken.md"), "---\nkey: [unclosed\n---\n\nBody.\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let checks = checks(&server).await?;

    for check in &checks {
        assert!(check.get("remediation").is_none(), "{check}");
        assert!(check.get("remediation_tool").is_none(), "{check}");
    }
    assert_eq!(of_kind(&checks, "frontmatter")?["status"], "fail");
    Ok(())
}
