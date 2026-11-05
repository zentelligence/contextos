//! The Agents screen's liveness: each agent's last finished session run, the tasks claimed for it, an amber note for a
//! run that started and did not finish, and its recently lapsed claims. Real `contextos` sessions over real temporary
//! vaults, with the clock fixed so what is overdue never depends on when the test runs.

mod support;
mod tasks_common;

use support::events::EventSpec;
use tasks_common::*;
use time::macros::datetime;

const TEAM: &str = "[[member]]\nslug = \"ellis\"\nname = \"Ellis\"\ndomain = \"Ops\"\n\n\
                    [[member]]\nslug = \"vela\"\nname = \"Vela\"\ndomain = \"Ops\"\n";

/// "Now" is noon on the 28th.
const NOW: time::OffsetDateTime = datetime!(2026-10-28 12:00:00 UTC);

struct Vault {
    dir: tempfile::TempDir,
    _config: tempfile::TempDir,
    router: Router,
}

async fn vault(web_toml: Option<&str>) -> Result<Vault, BoxError> {
    let dir = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    write(dir.path(), "index.md", "# Root\n")?;
    write(dir.path(), ".contextos/team.toml", TEAM)?;
    if let Some(text) = web_toml {
        write(config.path(), "web.toml", text)?;
    }
    let router = router_over_at(dir.path(), config.path(), "UTC", NOW).await?;
    Ok(Vault {
        dir,
        _config: config,
        router,
    })
}

fn claimed_task(id: &str, assignee: &str, claimed_by: &str, until: &str) -> String {
    format!(
        "---\nid: {id}\ntype: task\ntitle: Title for {id}\nstate: doing\ntask_type: action\nentity: acme\n\
         assignee: {assignee}\nclaimed_by: {claimed_by}\nclaimed_until: {until}\n---\n\n# {id}\n"
    )
}

fn session(
    vault: &Vault,
    slug: &str,
    kind: &str,
    severity: &str,
    at: &str,
    fields: Vec<(&str, &str)>,
) -> Result<(), BoxError> {
    EventSpec {
        kind,
        severity,
        at,
        fields,
        ..EventSpec::default()
    }
    .write_to(vault.dir.path(), slug)?;
    Ok(())
}

/// The card of the member named `name`: from its heading to the next card.
fn card<'a>(body: &'a str, name: &str) -> &'a str {
    let Some(start) = body.find(&format!("<h4>{name}")) else {
        return "";
    };
    let rest = &body[start..];
    let end = rest[4..].find("<h4>").map_or(rest.len(), |at| at + 4);
    &rest[..end]
}

async fn agents(vault: &Vault) -> Result<String, BoxError> {
    Ok(get(&vault.router, &format!("/{VAULT_NAME}/agents/")).await?.1)
}

#[tokio::test]
async fn members_with_no_session_events_show_no_liveness_and_no_amber() -> Result<(), BoxError> {
    let vault = vault(None).await?;

    let body = agents(&vault).await?;

    assert!(body.contains("Ellis") && body.contains("Vela"));
    assert!(
        !body.contains("agent-liveness") && !body.contains("agent-run-overdue"),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn a_member_shows_its_last_finished_run_with_time_and_severity_and_its_live_claims() -> Result<(), BoxError> {
    let vault = vault(None).await?;
    session(
        &vault,
        "s1",
        "session.run-finished",
        "info",
        "2026-10-28T06:00:00Z",
        vec![("run_id", "S-001"), ("members", "ellis")],
    )?;
    session(
        &vault,
        "s2",
        "session.run-finished",
        "warning",
        "2026-10-28T09:30:00Z",
        vec![("run_id", "S-002"), ("members", "ellis, vela")],
    )?;
    write(
        vault.dir.path(),
        "memory/tasks/a-1.md",
        &claimed_task("A-1", "ellis", "ellis/S-002", "2026-10-28T13:00:00Z"),
    )?;
    write(
        vault.dir.path(),
        "memory/tasks/a-2.md",
        &claimed_task("A-2", "ellis", "ellis/S-002", "2026-10-28T08:00:00Z"),
    )?;
    write(
        vault.dir.path(),
        "memory/tasks/a-3.md",
        &claimed_task("A-3", "vela", "vela/S-002", "2026-10-28T13:00:00Z"),
    )?;

    let body = agents(&vault).await?;

    let ellis = card(&body, "Ellis");
    assert!(
        ellis.contains("2026-10-28 09:30 UTC") && ellis.contains("colour-amber"),
        "{ellis}"
    );
    assert!(!ellis.contains("2026-10-28 06:00 UTC"), "only the last run: {ellis}");
    assert!(
        ellis.contains("<dd>1 task</dd>") || ellis.contains(">1 task<"),
        "one live claim: {ellis}"
    );
    assert!(card(&body, "Vela").contains("1 task"), "{body}");
    assert!(!ellis.contains("agent-run-overdue"), "{ellis}");
    Ok(())
}

#[tokio::test]
async fn a_run_that_started_and_did_not_finish_shows_amber_when_every_claim_it_made_has_lapsed() -> Result<(), BoxError>
{
    let vault = vault(None).await?;
    session(
        &vault,
        "s1",
        "session.run-started",
        "info",
        "2026-10-28T11:30:00Z",
        vec![("run_id", "S-007"), ("members", "ellis"), ("scope", "ellis")],
    )?;
    write(
        vault.dir.path(),
        "memory/tasks/a-1.md",
        &claimed_task("A-1", "ellis", "ellis/S-007", "2026-10-28T11:45:00Z"),
    )?;

    let body = agents(&vault).await?;

    let ellis = card(&body, "Ellis");
    assert!(
        ellis.contains("agent-run-overdue") && ellis.contains("S-007"),
        "{ellis}"
    );
    assert!(ellis.contains("2026-10-28 11:30 UTC"), "{ellis}");
    assert!(!card(&body, "Vela").contains("agent-run-overdue"));
    Ok(())
}

#[tokio::test]
async fn a_run_with_a_claim_still_running_is_not_amber() -> Result<(), BoxError> {
    let vault = vault(None).await?;
    session(
        &vault,
        "s1",
        "session.run-started",
        "info",
        "2026-10-28T05:00:00Z",
        vec![("run_id", "S-007"), ("members", "ellis"), ("scope", "ellis")],
    )?;
    write(
        vault.dir.path(),
        "memory/tasks/a-1.md",
        &claimed_task("A-1", "ellis", "ellis/S-007", "2026-10-28T12:30:00Z"),
    )?;

    let body = agents(&vault).await?;

    assert!(!body.contains("agent-run-overdue"), "{body}");
    assert!(
        card(&body, "Ellis").contains("agent-liveness"),
        "a session event still makes liveness worth showing"
    );
    Ok(())
}

#[tokio::test]
async fn a_run_no_task_claims_is_amber_only_after_the_configured_window() -> Result<(), BoxError> {
    let start = "2026-10-28T10:30:00Z";
    let default_window = vault(None).await?;
    session(
        &default_window,
        "s1",
        "session.run-started",
        "info",
        start,
        vec![("run_id", "S-007"), ("members", "ellis"), ("scope", "ellis")],
    )?;
    let short_window = vault(Some("[agents]\nrun_overdue_minutes = 60\n")).await?;
    session(
        &short_window,
        "s1",
        "session.run-started",
        "info",
        start,
        vec![("run_id", "S-007"), ("members", "ellis"), ("scope", "ellis")],
    )?;

    let within = agents(&default_window).await?;
    let past = agents(&short_window).await?;

    assert!(
        !within.contains("agent-run-overdue"),
        "ninety minutes is inside the default two hours"
    );
    assert!(past.contains("agent-run-overdue") && past.contains("S-007"), "{past}");
    Ok(())
}

#[tokio::test]
async fn a_finished_run_is_not_amber_however_old_its_start() -> Result<(), BoxError> {
    let vault = vault(None).await?;
    session(
        &vault,
        "s1",
        "session.run-started",
        "info",
        "2026-10-27T01:00:00Z",
        vec![("run_id", "S-007"), ("members", "ellis"), ("scope", "ellis")],
    )?;
    session(
        &vault,
        "s2",
        "session.run-finished",
        "info",
        "2026-10-27T02:00:00Z",
        vec![("run_id", "S-007"), ("members", "ellis")],
    )?;

    let body = agents(&vault).await?;

    assert!(!body.contains("agent-run-overdue"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_recent_lapsed_claim_on_the_members_task_is_listed_under_it_and_an_old_one_is_not() -> Result<(), BoxError> {
    let vault = vault(None).await?;
    write(
        vault.dir.path(),
        "memory/tasks/a-1.md",
        &claimed_task("A-1", "ellis", "ellis/S-001", "2026-10-29T00:00:00Z"),
    )?;
    write(
        vault.dir.path(),
        "memory/tasks/a-2.md",
        &claimed_task("A-2", "vela", "vela/S-001", "2026-10-29T00:00:00Z"),
    )?;
    let lapse = |slug: &str, at: &str, task: &str| {
        EventSpec {
            kind: "task.claim-lapsed",
            severity: "warning",
            at,
            subject: Some(Box::leak(
                format!("[[{}|{task}]]", task.to_ascii_lowercase()).into_boxed_str(),
            )),
            subject_id: Some(Box::leak(task.to_owned().into_boxed_str())),
            subject_path: Some(Box::leak(
                format!("memory/tasks/{}.md", task.to_ascii_lowercase()).into_boxed_str(),
            )),
            subject_base: Some("memory/tasks.base"),
            ..EventSpec::default()
        }
        .write_to(vault.dir.path(), slug)
    };
    lapse("l1", "2026-10-27T10:00:00Z", "A-1")?;
    lapse("l2", "2026-10-01T10:00:00Z", "A-1")?;
    lapse("l3", "2026-10-27T11:00:00Z", "A-2")?;

    let body = agents(&vault).await?;

    let ellis = card(&body, "Ellis");
    assert!(
        ellis.contains("Claim lapsed") && ellis.contains("A-1") && ellis.contains("2026-10-27"),
        "{ellis}"
    );
    assert!(!ellis.contains("2026-10-01") && !ellis.contains("A-2"), "{ellis}");
    assert!(card(&body, "Vela").contains("A-2"), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_agent_list_reloads_itself_when_the_stream_reports_a_session_or_claim_event() -> Result<(), BoxError> {
    let vault = vault(None).await?;

    let body = agents(&vault).await?;

    assert!(body.contains("hx-trigger=\"sse:attention, sse:reload\""), "{body}");
    Ok(())
}
