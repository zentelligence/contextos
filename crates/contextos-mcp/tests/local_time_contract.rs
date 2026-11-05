//! Every time the server writes into a vault is the operator's local time: a note's `created` and `updated` stamps
//! and the day its operation log entry is filed under follow `[server] timezone` (or the host's zone when none is
//! set), never UTC, and `vault_info` reports the zone in effect so a client can date its own writes the same way.
//! The tests run on the real clock in Australia/Sydney, which keeps daylight saving, so each stamp is checked against
//! the offset Sydney actually had at the stamped instant (+10:00 or +11:00) rather than a fixed one.

mod support;
use contextos_core::{ConfiguredTimeZone, OperatorTimeZone};
use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Value,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    let arguments: Map<String, Value> = serde_json::from_value(arguments)?;
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let mut client = ().serve(client_transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    client.close().await?;
    server_handle.await??;
    Ok(result?)
}

fn sydney_server(vault: &tempfile::TempDir) -> Result<ContextOsServer, Box<dyn std::error::Error + Send + Sync>> {
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.server.timezone = Some("Australia/Sydney".to_owned());
    Ok(ContextOsServer::try_from(config)?)
}

/// Sydney's offset at `instant`, as `+HH:MM`.
fn sydney_offset_at(instant: OffsetDateTime) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let zone = OperatorTimeZone::from(ConfiguredTimeZone::try_from("Australia/Sydney")?);
    let (hours, minutes, _) = zone.offset_at(instant).as_hms();
    Ok(format!("+{hours:02}:{minutes:02}"))
}

/// Asserts `stamp` is RFC 3339 to the minute, at the offset Sydney had at that instant.
fn assert_sydney_stamp(stamp: &str) -> TestResult {
    let instant = OffsetDateTime::parse(stamp, &Rfc3339)?;
    assert!(
        stamp.ends_with(&format!(":00{}", sydney_offset_at(instant)?)),
        "{stamp}"
    );
    assert!(
        ["+10:00", "+11:00"].iter().any(|offset| stamp.ends_with(offset)),
        "{stamp}"
    );
    Ok(())
}

fn frontmatter_value(note: &str, key: &str) -> Option<String> {
    note.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}: ")))
        .map(|value| value.trim_matches(['\'', '"']).to_owned())
}

#[tokio::test]
async fn a_new_note_is_stamped_in_the_configured_zone_and_logged_under_that_days_date() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = sydney_server(&vault)?;

    // The server reads the real clock, so the call is bracketed: whatever instant it took lies between these two,
    // and the Sydney dates they fall on are the only days its stamp and its log may name.
    let before = OffsetDateTime::now_utc();
    let created = call_tool(
        server,
        "note_create",
        json!({ "path": "note.md", "frontmatter": {}, "content": "Body.\n" }),
    )
    .await?;
    let after = OffsetDateTime::now_utc();

    assert_eq!(created.is_error, Some(false), "{created:?}");
    let note = std::fs::read_to_string(vault.path().join("note.md"))?;
    let stamp = frontmatter_value(&note, "created").ok_or("the note has a created stamp")?;
    assert_sydney_stamp(&stamp)?;
    let stamped = OffsetDateTime::parse(&stamp, &Rfc3339)?;
    assert!(
        stamped <= after && stamped >= before - time::Duration::minutes(1),
        "{stamp} is not the instant of the call"
    );
    let days = [sydney_date(before)?, sydney_date(after)?];
    assert!(
        days.contains(&stamp[0..10].to_owned()),
        "{stamp} is not a Sydney date of the call: {days:?}"
    );
    let logged = daily_logs(&vault.path().join("memory/log"))?;
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert!(
        days.contains(&logged[0]),
        "logged under {logged:?}, not a Sydney date of the call: {days:?}"
    );
    Ok(())
}

/// Sydney's date at `instant`, as `YYYY-MM-DD`.
fn sydney_date(instant: OffsetDateTime) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let zone = OperatorTimeZone::from(ConfiguredTimeZone::try_from("Australia/Sydney")?);
    let local = zone.localise(instant);
    Ok(format!(
        "{:04}-{:02}-{:02}",
        local.year(),
        u8::from(local.month()),
        local.day()
    ))
}

/// The dates the operation log has daily files for, from `memory/log/YYYY/MM/YYYY-MM-DD.md`.
fn daily_logs(root: &std::path::Path) -> std::io::Result<Vec<String>> {
    let mut days = Vec::new();
    for year in std::fs::read_dir(root)? {
        for month in std::fs::read_dir(year?.path())? {
            for day in std::fs::read_dir(month?.path())? {
                if let Some(stem) = day?.path().file_stem().and_then(|stem| stem.to_str()) {
                    days.push(stem.to_owned());
                }
            }
        }
    }
    Ok(days)
}

#[tokio::test]
async fn a_frontmatter_update_restamps_updated_in_the_configured_zone() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "---\ntitle: Note\n---\nBody.\n")?;
    let server = sydney_server(&vault)?;

    let updated = call_tool(
        server,
        "frontmatter_update",
        json!({ "path": "note.md", "patch": { "title": "Renamed" } }),
    )
    .await?;

    assert_eq!(updated.is_error, Some(false), "{updated:?}");
    let note = std::fs::read_to_string(vault.path().join("note.md"))?;
    let stamp = frontmatter_value(&note, "updated").ok_or("the note has an updated stamp")?;
    assert_sydney_stamp(&stamp)?;
    Ok(())
}

#[tokio::test]
async fn vault_info_reports_the_time_zone_in_effect() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;

    let info = call_tool(sydney_server(&vault)?, "vault_info", json!({})).await?;

    assert_eq!(info.is_error, Some(false), "{info:?}");
    let zone = info
        .structured_content
        .as_ref()
        .and_then(|value| value.get("time_zone"))
        .ok_or("vault_info reports a time_zone")?;
    assert_eq!(zone.get("name"), Some(&json!("Australia/Sydney")));
    assert_eq!(zone.get("source"), Some(&json!("configured")));
    assert_eq!(
        zone.get("offset"),
        Some(&json!(sydney_offset_at(OffsetDateTime::now_utc())?))
    );
    Ok(())
}
