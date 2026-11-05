//! `contextos-web workflows run` for a workflow registered in a vault: real `contextos-mcp` sessions over real
//! temporary vaults. A one-off run validates its inputs, is confined to its own vault and its workflow's servers, and
//! takes no claim and writes no run record.

mod support;

use std::path::Path;
use std::sync::Arc;

use contextos_web::McpClientSet;
use contextos_web::cli_run::{CliRunError, OneOffVaultRun};
use contextos_workflows::{RunOutcome, WorkflowFailureKind};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

fn write_workflow(vault: &Path, slug: &str, extra_manifest: &str, script: &str) -> std::io::Result<()> {
    write(
        vault,
        &format!("registry/workflows/{slug}/workflows.toml"),
        &format!("workflows = [{slug:?}]\n"),
    )?;
    write(
        vault,
        &format!("registry/workflows/{slug}/{slug}/workflow.toml"),
        &format!(
            "name = \"Test workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n{extra_manifest}"
        ),
    )?;
    write(
        vault,
        &format!("registry/workflows/{slug}/{slug}/workflow.rhai"),
        script,
    )
}

struct Fixture {
    _config: tempfile::TempDir,
    home: tempfile::TempDir,
    other: tempfile::TempDir,
    clients: Arc<McpClientSet>,
}

async fn fixture() -> Result<Fixture, BoxError> {
    let config = tempfile::tempdir()?;
    let home = tempfile::tempdir()?;
    let other = tempfile::tempdir()?;
    write(home.path(), "index.md", "# Home\n")?;
    write(other.path(), "index.md", "# Other\n")?;
    let config_path =
        support::write_named_vaults_config(config.path(), &[("client-a", home.path()), ("client-b", other.path())])?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(Fixture {
        _config: config,
        home,
        other,
        clients,
    })
}

async fn run(fixture: &Fixture, workflow: &str, arguments: &[&str]) -> Result<RunOutcome, CliRunError> {
    support::run_workflow_once(
        Arc::clone(&fixture.clients),
        "contextos",
        OneOffVaultRun {
            code: contextos_web::cli_run::OneOffCode::Source,
            vault: "client-a".to_owned(),
            package: workflow.to_owned(),
            workflow_slug: workflow.to_owned(),
            arguments: arguments.iter().map(|text| (*text).to_owned()).collect(),
            dry_run_scenario: None,
            log_level: None,
        },
    )
    .await
}

fn failed_with(outcome: &RunOutcome, kind: WorkflowFailureKind) -> bool {
    matches!(outcome, RunOutcome::Failed(failure) if failure.kind == Some(kind))
}

#[tokio::test]
async fn a_one_off_run_gives_the_script_its_validated_input_and_writes_no_record() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    write_workflow(
        fixture.home.path(),
        "greet",
        "\n[[input]]\nname = \"label\"\nkind = \"string\"\n",
        "if inputs.label != \"hello\" { throw \"wrong input\"; }\ncall_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"client-a://index.md\" });",
    )?;

    let outcome = run(&fixture, "greet", &["label=hello"]).await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    assert!(!fixture.home.path().join("memory/jobs").exists());
    Ok(())
}

#[tokio::test]
async fn an_undeclared_or_missing_input_ends_the_run_before_any_call() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    write_workflow(
        fixture.home.path(),
        "greet",
        "\n[[input]]\nname = \"label\"\nkind = \"string\"\n",
        "call_mcp(\"contextos\", \"vault_info\", #{});",
    )?;

    let undeclared = run(&fixture, "greet", &["label=a", "extra=b"]).await?;
    let missing = run(&fixture, "greet", &[]).await?;

    assert!(
        failed_with(&undeclared, WorkflowFailureKind::InputInvalid),
        "{undeclared:?}"
    );
    assert!(failed_with(&missing, WorkflowFailureKind::InputInvalid), "{missing:?}");
    Ok(())
}

#[tokio::test]
async fn a_run_may_not_name_another_configured_vault_in_any_letter_case() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    write_workflow(
        fixture.home.path(),
        "escape",
        "",
        "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"CLIENT-B://index.md\" });",
    )?;

    let outcome = run(&fixture, "escape", &[]).await?;

    assert!(
        failed_with(&outcome, WorkflowFailureKind::VaultOutOfScope),
        "{outcome:?}"
    );
    assert!(fixture.other.path().join("index.md").is_file());
    Ok(())
}

#[tokio::test]
async fn a_call_to_a_server_outside_the_workflows_list_is_denied() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    write_workflow(
        fixture.home.path(),
        "wander",
        "",
        "call_mcp(\"billing\", \"charge\", #{});",
    )?;

    let outcome = run(&fixture, "wander", &[]).await?;

    assert!(failed_with(&outcome, WorkflowFailureKind::EffectDenied), "{outcome:?}");
    Ok(())
}

#[tokio::test]
async fn a_vault_kind_input_is_refused_for_a_vault_registered_workflow() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    write_workflow(
        fixture.home.path(),
        "cross",
        "\n[[input]]\nname = \"target\"\nkind = \"vault\"\n",
        "",
    )?;

    let result = run(&fixture, "cross", &["target=client-b"]).await;

    assert!(
        matches!(result, Err(CliRunError::WorkflowNotFound { .. })),
        "{result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_workflow_that_is_not_registered_is_reported_as_not_found() -> Result<(), BoxError> {
    let fixture = fixture().await?;

    let result = run(&fixture, "missing", &[]).await;

    assert!(
        matches!(result, Err(CliRunError::WorkflowNotFound { .. })),
        "{result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_script_receives_fs_attach_file_as_content_and_hash_like_fs_read_text_file() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    write(fixture.home.path(), "notes/a.md", "# Title\n\nSee [other](b.md).\n")?;
    write(fixture.home.path(), "notes/b.md", "# Other\n")?;
    write_workflow(
        fixture.home.path(),
        "attach",
        "",
        r#"
        let attached = call_mcp("contextos", "fs_attach_file", #{ path: "client-a://notes/a.md" });
        let read = call_mcp("contextos", "fs_read_text_file", #{ path: "client-a://notes/a.md" });
        if type_of(attached) != "map" { throw "attached is " + type_of(attached); }
        if attached.content != read.content { throw "content differs: " + attached.content; }
        if attached.content_hash != read.content_hash { throw "hash differs"; }
        if attached.contains("uri") { throw "the resource uri reached the script"; }

        let expanded = call_mcp("contextos", "fs_attach_file", #{ path: "client-a://notes/a.md", expand_links: true });
        if expanded.links_expanded != true { throw "links were not reported as expanded"; }
        if expanded.contains("content_hash") { throw "an expanded reading must carry no hash"; }
        if !expanded.content.contains("notes/b.md") { throw "link was not rewritten: " + expanded.content; }
        "#,
    )?;

    let outcome = run(&fixture, "attach", &[]).await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    Ok(())
}

#[tokio::test]
async fn a_call_naming_a_vault_the_server_does_not_know_fails_as_vault_unavailable_and_ends_the_run()
-> Result<(), BoxError> {
    // The later call would fail with a different kind if the run carried on past the unknown vault.
    let fixture = fixture().await?;
    write_workflow(
        fixture.home.path(),
        "unknown",
        "",
        "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"client-a://index.md\" });\n\
         call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"unknown-vault://index.md\" });\n\
         call_mcp(\"billing\", \"charge\", #{});",
    )?;

    let outcome = run(&fixture, "unknown", &[]).await?;

    assert!(
        failed_with(&outcome, WorkflowFailureKind::VaultUnavailable),
        "{outcome:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_tool_that_returns_an_error_fails_the_run_as_a_tool_error_and_the_script_may_catch_it() -> Result<(), BoxError>
{
    let fixture = fixture().await?;
    write_workflow(
        fixture.home.path(),
        "missing-note",
        "",
        "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"client-a://absent.md\" });",
    )?;
    write_workflow(
        fixture.home.path(),
        "caught",
        "",
        "let seen = \"\";\n\
         try { call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"client-a://absent.md\" }); } catch (e) { seen = e; }\n\
         if !seen.starts_with(\"workflow/tool-error: \") { throw \"saw \" + seen; }",
    )?;

    let uncaught = run(&fixture, "missing-note", &[]).await?;
    let caught = run(&fixture, "caught", &[]).await?;

    assert!(failed_with(&uncaught, WorkflowFailureKind::ToolError), "{uncaught:?}");
    assert_eq!(caught, RunOutcome::Succeeded);
    Ok(())
}
