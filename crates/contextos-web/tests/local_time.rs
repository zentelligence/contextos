//! Every date contextos-web writes into a vault (a new note's file name and template dates, the journal's "today")
//! is the operator's local date and time in the zone the contextos server reports, never UTC, at the offset that
//! zone had at that instant. Real `contextos-mcp` sessions over real temporary vaults, with the clock fixed at
//! 21:45 UTC: on 8 October 2026 that is 08:45 the next morning in Sydney under daylight saving time (+11:00), and on
//! 8 July 2026 it is 07:45 the next morning under standard time (+10:00).

mod support;

#[path = "notes_common/mod.rs"]
mod notes_common;

use notes_common::{Arc, McpClientSet, Path, Router, VAULT_NAME, extract_create_form_path, get, seed_template, write};
use time::OffsetDateTime;
use time::macros::datetime;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const SUMMER: OffsetDateTime = datetime!(2026-10-08 21:45 UTC);
const WINTER: OffsetDateTime = datetime!(2026-07-08 21:45 UTC);

struct FixedClock(OffsetDateTime);

impl contextos_workflows::Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

/// A router over `vault_dir` whose contextos server is configured for `zone`, with the clock fixed at `at`.
async fn router_in(vault_dir: &Path, config_dir: &Path, zone: &str, at: OffsetDateTime) -> Result<Router, BoxError> {
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let config =
        std::fs::read_to_string(&config_path)?.replace("timezone = \"UTC\"", &format!("timezone = \"{zone}\""));
    std::fs::write(&config_path, config)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router_at(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
        &contextos_web::config::WorkflowsConfig::default(),
        None,
        Arc::new(FixedClock(at)),
    ))
}

async fn location(router: &Router, path: &str) -> Result<Option<String>, BoxError> {
    use notes_common::{Body, Request};
    use tower::ServiceExt as _;

    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    Ok(response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned))
}

#[tokio::test]
async fn the_journal_opens_on_the_operators_local_today_in_summer_and_winter() -> Result<(), BoxError> {
    for (at, today) in [(SUMMER, "2026-10-09"), (WINTER, "2026-07-09")] {
        let vault_dir = tempfile::tempdir()?;
        let config_dir = tempfile::tempdir()?;
        write(vault_dir.path(), "index.md", "# Root\n")?;
        let router = router_in(vault_dir.path(), config_dir.path(), "Australia/Sydney", at).await?;

        let target = location(&router, &format!("/{VAULT_NAME}/journal/")).await?;

        assert_eq!(target, Some(format!("/{VAULT_NAME}/journal/{today}")));
    }
    Ok(())
}

#[tokio::test]
async fn a_new_note_is_named_and_dated_in_the_operators_local_time_across_daylight_saving() -> Result<(), BoxError> {
    for (at, path, heading) in [
        (SUMMER, "2026/10/2026-10-09-0845.md", "# Friday, 9th October 2026"),
        (WINTER, "2026/07/2026-07-09-0745.md", "# Thursday, 9th July 2026"),
    ] {
        let vault_dir = tempfile::tempdir()?;
        let config_dir = tempfile::tempdir()?;
        write(vault_dir.path(), "index.md", "# Root\n")?;
        seed_template(vault_dir.path())?;
        let router = router_in(vault_dir.path(), config_dir.path(), "Australia/Sydney", at).await?;

        let (_, body) = get(&router, &format!("/{VAULT_NAME}/notes/new")).await?;

        assert_eq!(extract_create_form_path(&body).as_deref(), Some(path), "{body}");
        // The template's own date, rendered into the new note's body.
        assert!(body.contains(heading), "{body}");
    }
    Ok(())
}

#[tokio::test]
async fn the_same_instant_is_the_previous_day_where_the_server_keeps_utc() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_in(vault_dir.path(), config_dir.path(), "UTC", SUMMER).await?;

    let target = location(&router, &format!("/{VAULT_NAME}/journal/")).await?;

    assert_eq!(target, Some(format!("/{VAULT_NAME}/journal/2026-10-08")));
    Ok(())
}
