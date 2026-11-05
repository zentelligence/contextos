//! The Signals screen: real `contextos` sessions against real temporary vaults holding event notes, exercising the
//! three regions, their filters, the rules for what needs attention, and what is never shown.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_web::McpClientSet;
use support::events::{EventSpec, kind_definition, seed_events};
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT: &str = "contract-fixture";

struct Fixture {
    vault: tempfile::TempDir,
    _config: tempfile::TempDir,
    router: Router,
    clients: Arc<McpClientSet>,
}

async fn fixture() -> Result<Fixture, BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    std::fs::write(vault.path().join("index.md"), "# Root\n")?;
    let config_path = support::write_vault_config(config.path(), vault.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    let router = contextos_web::build_router(
        Arc::clone(&clients),
        Some(config.path()),
        &config.path().join("web.toml"),
    );
    Ok(Fixture {
        vault,
        _config: config,
        router,
        clients,
    })
}

async fn get(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    get_as(router, path, false).await
}

async fn get_as(router: &Router, path: &str, htmx: bool) -> Result<(StatusCode, String), BoxError> {
    let mut request = Request::builder().uri(path);
    if htmx {
        request = request.header("hx-request", "true");
    }
    let response = router.clone().oneshot(request.body(Body::empty())?).await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

fn put(root: &Path, spec: &EventSpec<'_>, slug: &str) -> Result<String, BoxError> {
    Ok(spec.write_to(root, slug)?)
}

/// The text of the region `id` in `body`, from its opening tag to the next region or the end.
fn region<'a>(body: &'a str, id: &str) -> &'a str {
    let marker = format!("id=\"{id}\"");
    let Some(start) = body.find(&marker) else {
        return "";
    };
    let rest = &body[start..];
    let end = rest[marker.len()..]
        .find("<section id=\"signals-")
        .map_or(rest.len(), |at| at + marker.len());
    &rest[..end]
}

const TASK_LINK: &str = "[[memory/tasks/a-024|A-024]]";

#[tokio::test]
async fn a_vault_with_no_events_shows_three_empty_states_and_no_error() -> Result<(), BoxError> {
    let fixture = fixture().await?;

    let (status, body) = get(&fixture.router, &format!("/{VAULT}/signals/")).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Nothing needs attention."), "{body}");
    assert!(body.contains("No events"), "{body}");
    assert!(body.contains("No background activity has been recorded."), "{body}");
    assert!(!body.contains("could not be read"), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_page_is_linked_from_the_sidebar_and_a_fragment_request_gets_no_chrome() -> Result<(), BoxError> {
    let fixture = fixture().await?;

    let (_, page) = get(&fixture.router, &format!("/{VAULT}/signals/")).await?;
    let (status, fragment) = get_as(&fixture.router, &format!("/{VAULT}/signals/"), true).await?;

    assert!(
        page.contains(&format!("href=\"/{VAULT}/signals/\"")),
        "the sidebar leads to Signals: {page}"
    );
    assert!(page.contains("<html"), "{page}");
    assert_eq!(status, StatusCode::OK);
    assert!(!fragment.contains("<html"), "{fragment}");
    assert!(fragment.contains("id=\"signals-feed\""), "{fragment}");
    Ok(())
}

#[tokio::test]
async fn one_event_shows_its_label_severity_subject_entity_and_detail() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    seed_events(fixture.vault.path())?;
    let root = fixture.vault.path();
    support::jobs::write(
        root,
        "registry/events/task.state-changed.md",
        &kind_definition("task.state-changed", "info", "info", "from, to", "required")
            .replace("label: task.state-changed", "label: Task state changed here"),
    )?;
    put(
        root,
        &EventSpec {
            subject: Some(TASK_LINK),
            subject_id: Some("A-024"),
            entity: "zenith",
            fields: vec![("from", "next"), ("to", "doing")],
            ..EventSpec::default()
        },
        "a-024",
    )?;

    let (_, body) = get(&fixture.router, &format!("/{VAULT}/signals/")).await?;

    let feed = region(&body, "signals-feed");
    assert!(feed.contains("Task state changed here"), "{feed}");
    assert!(feed.contains("A-024"), "{feed}");
    assert!(
        feed.contains("href=\"/contract-fixture/memory/tasks/a-024.md\""),
        "{feed}"
    );
    assert!(feed.contains("zenith"), "{feed}");
    assert!(feed.contains("next to doing"), "{feed}");
    assert!(feed.contains("datetime=\"2026-10-28T03:15:42Z\""), "{feed}");
    assert!(!feed.contains("No events yet"), "{feed}");
    Ok(())
}

#[tokio::test]
async fn the_feed_lists_fifty_to_a_page_newest_first_and_pages_on() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    for n in 0..60 {
        put(
            root,
            &EventSpec {
                at: &format!("2026-10-28T03:{:02}:{:02}Z", n / 60 + 10, n % 60),
                fields: vec![("to", &format!("state-{n:02}"))],
                ..EventSpec::default()
            },
            &format!("e{n:02}"),
        )?;
    }

    let (_, first) = get(&fixture.router, &format!("/{VAULT}/signals/feed")).await?;
    let (_, second) = get(&fixture.router, &format!("/{VAULT}/signals/feed?page=2")).await?;

    assert_eq!(first.matches("class=\"signal-event ").count(), 50, "{first}");
    assert!(
        first.contains("now state-59") && !first.contains("now state-09"),
        "newest first: {first}"
    );
    assert!(first.contains("Older") && !first.contains("Newer"), "{first}");
    assert_eq!(second.matches("class=\"signal-event ").count(), 10, "{second}");
    assert!(
        second.contains("now state-09") && second.contains("now state-00"),
        "{second}"
    );
    assert!(second.contains("Newer") && !second.contains("Older"), "{second}");
    Ok(())
}

#[tokio::test]
async fn the_feed_filters_by_kind_severity_and_entity() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    let specs = [
        ("one", "task.created", "info", "acme", "10"),
        ("two", "task.claim-lapsed", "warning", "acme", "11"),
        ("three", "task.claim-lapsed", "warning", "other", "12"),
        ("four", "vault.degraded", "warning", "acme", "13"),
    ];
    for (slug, kind, severity, entity, minute) in specs {
        put(
            root,
            &EventSpec {
                kind,
                severity,
                entity,
                at: &format!("2026-10-28T03:{minute}:00Z"),
                ..EventSpec::default()
            },
            slug,
        )?;
    }
    let count = |body: &str| body.matches("class=\"signal-event ").count();

    let (_, by_kind) = get(
        &fixture.router,
        &format!("/{VAULT}/signals/feed?kind=task.claim-lapsed"),
    )
    .await?;
    let (_, by_severity) = get(&fixture.router, &format!("/{VAULT}/signals/feed?severity=warning")).await?;
    let (_, by_entity) = get(&fixture.router, &format!("/{VAULT}/signals/feed?entity=acme")).await?;
    let (_, by_all) = get(
        &fixture.router,
        &format!("/{VAULT}/signals/feed?entity=acme&kind=task.claim-lapsed&severity=warning"),
    )
    .await?;

    assert_eq!(
        (count(&by_kind), count(&by_severity), count(&by_entity), count(&by_all)),
        (2, 3, 3, 1)
    );
    assert!(
        by_kind.contains("sse-swap=\"signal\"") && by_kind.contains("data-filter-kind=\"task.claim-lapsed\""),
        "a filter keeps live prepends, narrowed to what matches: {by_kind}"
    );
    assert!(!by_kind.contains("paused"), "{by_kind}");
    Ok(())
}

#[tokio::test]
async fn an_event_note_that_cannot_be_read_is_skipped_and_counted() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    put(root, &EventSpec::default(), "good")?;
    support::jobs::write(
        root,
        "memory/events/2026/10/2026-10-28-000000-bad.md",
        &EventSpec::default()
            .text()
            .replace("at: 2026-10-28T03:15:42Z", "at: yesterday"),
    )?;
    support::jobs::write(
        root,
        "memory/events/2026/10/2026-10-28-000001-worse.md",
        &EventSpec::default().text().replace("severity: info", "severity: fatal"),
    )?;

    let (_, body) = get(&fixture.router, &format!("/{VAULT}/signals/")).await?;

    let feed = region(&body, "signals-feed");
    assert_eq!(feed.matches("class=\"signal-event ").count(), 1, "{feed}");
    assert!(
        feed.contains("2 event notes could not be read and were left out."),
        "{feed}"
    );
    Ok(())
}

#[tokio::test]
async fn a_warning_stays_in_attention_until_a_later_event_about_its_subject() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    put(
        root,
        &EventSpec {
            kind: "task.claim-lapsed",
            severity: "warning",
            at: "2026-10-28T03:00:00Z",
            subject: Some(TASK_LINK),
            subject_id: Some("A-024"),
            ..EventSpec::default()
        },
        "lapsed",
    )?;

    let (_, open) = get(&fixture.router, &format!("/{VAULT}/signals/attention")).await?;
    put(
        root,
        &EventSpec {
            at: "2026-10-28T04:00:00Z",
            subject: Some(TASK_LINK),
            subject_id: Some("A-024"),
            ..EventSpec::default()
        },
        "later",
    )?;
    let (_, cleared) = get(&fixture.router, &format!("/{VAULT}/signals/attention")).await?;

    assert!(open.contains("Task claim lapsed") && open.contains("A-024"), "{open}");
    assert!(cleared.contains("Nothing needs attention."), "{cleared}");
    Ok(())
}

#[tokio::test]
async fn vault_degraded_is_cleared_by_a_later_recovery_with_the_same_code_only() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    let degraded = |slug: &str, code: &'static str, at: &'static str| {
        put(
            root,
            &EventSpec {
                kind: "vault.degraded",
                severity: "warning",
                at,
                fields: vec![("code", code), ("service", "git")],
                ..EventSpec::default()
            },
            slug,
        )
    };
    degraded("git", "git/commit", "2026-10-28T03:00:00Z")?;
    degraded("index", "index/storage", "2026-10-28T03:01:00Z")?;
    put(
        root,
        &EventSpec {
            kind: "vault.recovered",
            at: "2026-10-28T03:05:00Z",
            fields: vec![("code", "git/commit")],
            ..EventSpec::default()
        },
        "recovered",
    )?;

    let (_, body) = get(&fixture.router, &format!("/{VAULT}/signals/attention")).await?;

    assert!(body.contains("code: index/storage"), "{body}");
    assert!(!body.contains("code: git/commit"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_failed_job_is_cleared_by_a_later_recovery_of_that_job_only() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    seed_events(root)?;
    let failed = |slug: &str, job: &'static str, at: &'static str| {
        put(
            root,
            &EventSpec {
                kind: "job.failed",
                severity: "error",
                at,
                subject: Some("[[memory/jobs/ops/sync/nightly/R-1|R-1]]"),
                fields: vec![
                    ("workflow", "sync"),
                    ("job", job),
                    ("run", "R-1"),
                    ("code", "workflow/script-error"),
                ],
                ..EventSpec::default()
            },
            slug,
        )
    };
    failed("nightly", "nightly", "2026-10-28T03:00:00Z")?;
    failed("weekly", "weekly", "2026-10-28T03:01:00Z")?;
    put(
        root,
        &EventSpec {
            kind: "job.recovered",
            at: "2026-10-28T03:05:00Z",
            fields: vec![("workflow", "sync"), ("job", "nightly"), ("run", "R-2")],
            ..EventSpec::default()
        },
        "recovered",
    )?;

    let (_, body) = get(&fixture.router, &format!("/{VAULT}/signals/attention")).await?;

    assert!(body.contains("job: weekly") && !body.contains("job: nightly"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_warning_with_no_subject_stays_whatever_follows() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    put(
        root,
        &EventSpec {
            kind: "import.failed",
            severity: "error",
            at: "2026-10-28T03:00:00Z",
            ..EventSpec::default()
        },
        "import",
    )?;
    put(
        root,
        &EventSpec {
            at: "2026-10-28T09:00:00Z",
            ..EventSpec::default()
        },
        "later",
    )?;

    let (_, body) = get(&fixture.router, &format!("/{VAULT}/signals/attention")).await?;

    assert!(body.contains("import.failed"), "{body}");
    Ok(())
}

#[tokio::test]
async fn background_activity_groups_the_kinds_the_product_does_not_ship() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    for (slug, kind, at) in [
        ("a", "import.completed", "2026-10-28T03:00:00Z"),
        ("b", "import.completed", "2026-10-28T03:01:00Z"),
        ("c", "trade.executed", "2026-10-28T03:02:00Z"),
        ("d", "task.created", "2026-10-28T03:03:00Z"),
        ("e", "job.failed", "2026-10-28T03:04:00Z"),
    ] {
        put(
            root,
            &EventSpec {
                kind,
                at,
                ..EventSpec::default()
            },
            slug,
        )?;
    }

    let (_, body) = get(&fixture.router, &format!("/{VAULT}/signals/background")).await?;

    assert!(
        body.contains("import.completed") && body.contains("trade.executed"),
        "{body}"
    );
    assert!(!body.contains("task.created") && !body.contains("job.failed"), "{body}");
    let trade = body.find("trade.executed").ok_or("trade")?;
    let import = body.find("import.completed").ok_or("import")?;
    assert!(trade < import, "the most recently active kind comes first: {body}");
    Ok(())
}

#[tokio::test]
async fn the_entity_switcher_narrows_every_region() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    for (slug, entity, kind, severity) in [
        ("a", "acme", "import.completed", "info"),
        ("b", "other", "trade.executed", "info"),
        ("c", "acme", "import.failed", "error"),
        ("d", "other", "batch.failed", "error"),
    ] {
        put(
            root,
            &EventSpec {
                kind,
                severity,
                entity,
                at: "2026-10-28T03:00:00Z",
                ..EventSpec::default()
            },
            slug,
        )?;
    }

    let (_, body) = get(&fixture.router, &format!("/{VAULT}/signals/?entity=acme")).await?;

    assert!(
        body.contains("import.completed") && body.contains("import.failed"),
        "{body}"
    );
    assert!(
        !body.contains("trade.executed") && !body.contains("batch.failed"),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn no_note_body_is_ever_shown_though_every_declared_frontmatter_field_is() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    seed_events(root)?;
    support::jobs::write(
        root,
        "registry/events/import.completed.md",
        &kind_definition("import.completed", "info, warning", "info", "batch", "none"),
    )?;
    put(
        root,
        &EventSpec {
            kind: "import.completed",
            severity: "warning",
            fields: vec![("batch", "FIELD-MARKER-4471")],
            body: "BODY-MARKER-9921 sk-live-secret\n",
            ..EventSpec::default()
        },
        "secret",
    )?;

    let (_, page) = get(&fixture.router, &format!("/{VAULT}/signals/")).await?;

    assert!(page.contains("import.completed"), "{page}");
    assert!(
        !page.contains("BODY-MARKER-9921") && !page.contains("sk-live-secret"),
        "{page}"
    );
    assert!(page.contains("batch: FIELD-MARKER-4471"), "{page}");
    Ok(())
}

#[tokio::test]
async fn an_unknown_vault_is_not_found() -> Result<(), BoxError> {
    let fixture = fixture().await?;

    let (status, _) = get(&fixture.router, "/no-such-vault/signals/").await?;

    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn the_sidebar_counts_what_needs_attention_and_shows_a_dot_while_anything_does() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    put(
        fixture.vault.path(),
        &EventSpec {
            kind: "import.failed",
            severity: "error",
            ..EventSpec::default()
        },
        "bad",
    )?;

    let (_, body) = get(&fixture.router, &format!("/{VAULT}/")).await?;

    let entry = body
        .split("<li>")
        .find(|item| item.contains("<span class=\"nav-link-label\">Signals</span>"))
        .ok_or("no Signals entry")?;
    assert!(entry.contains("<span class=\"nav-link-meta\">1</span>"), "{entry}");
    assert!(entry.contains("status-dot attention"), "{entry}");
    Ok(())
}

#[tokio::test]
async fn a_recorded_kinds_declared_fields_are_shown_whatever_they_are_called_and_are_escaped() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    seed_events(fixture.vault.path())?;
    support::jobs::write(
        fixture.vault.path(),
        "registry/events/booking.received.md",
        &kind_definition(
            "booking.received",
            "info",
            "info",
            "guest, nights, rooms, contact, deposit_paid",
            "none",
        ),
    )?;
    put(
        fixture.vault.path(),
        &EventSpec {
            kind: "booking.received",
            at: "2026-10-28T04:00:00Z",
            fields: vec![
                ("guest", "\"<b>Ada</b>\""),
                ("nights", "3"),
                ("rooms", "[Lake, Hill]"),
                ("contact", "{email: ada@example.test, phone: '555'}"),
                ("deposit_paid", "true"),
                ("stray", "HAND-PLACED-7731"),
            ],
            ..EventSpec::default()
        },
        "booking",
    )?;

    let (_, body) = get(&fixture.router, &format!("/{VAULT}/signals/")).await?;

    let feed = region(&body, "signals-feed");
    assert!(feed.contains("guest: &#60;b&#62;Ada&#60;/b&#62;"), "escaped: {feed}");
    assert!(!feed.contains("<b>Ada</b>"), "{feed}");
    assert!(feed.contains("nights: 3"), "{feed}");
    assert!(feed.contains("rooms: Lake, Hill"), "a list is comma separated: {feed}");
    assert!(
        feed.contains("contact: {email: ada@example.test, phone: 555}"),
        "a map is compact text: {feed}"
    );
    assert!(feed.contains("deposit_paid: true"), "{feed}");
    assert!(
        !feed.contains("kind: booking.received"),
        "common fields are not repeated: {feed}"
    );
    assert!(
        !body.contains("HAND-PLACED-7731"),
        "a field the definition does not declare is never shown, even if a note holds it: {body}"
    );
    let attention_free = region(&body, "signals-background");
    assert!(
        attention_free.contains("guest: &#60;b&#62;Ada&#60;/b&#62;"),
        "{attention_free}"
    );
    Ok(())
}

#[tokio::test]
async fn an_unknown_severity_in_the_query_is_refused_as_malformed_and_never_silently_dropped() -> Result<(), BoxError> {
    let fixture = fixture().await?;

    for path in [
        format!("/{VAULT}/signals/?severity=bogus"),
        format!("/{VAULT}/signals/feed?severity=bogus"),
    ] {
        let (status, body) = get(&fixture.router, &path).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
        assert!(body.contains("route/malformed-query"), "{body}");
    }
    let (status, _) = get(&fixture.router, &format!("/{VAULT}/signals/feed?severity=warning")).await?;
    assert_eq!(status, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn a_cleared_warning_is_not_shown_open_even_once_the_event_that_cleared_it_is_the_only_thing_left()
-> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    put(
        root,
        &EventSpec {
            kind: "task.claim-lapsed",
            severity: "warning",
            at: "2026-10-28T03:00:00Z",
            subject: Some(TASK_LINK),
            ..EventSpec::default()
        },
        "lapsed",
    )?;

    let (_, open) = get(&fixture.router, &format!("/{VAULT}/signals/attention")).await?;
    put(
        root,
        &EventSpec {
            kind: "task.state-changed",
            at: "2026-10-28T04:00:00Z",
            subject: Some(TASK_LINK),
            ..EventSpec::default()
        },
        "later",
    )?;
    let (_, cleared) = get(&fixture.router, &format!("/{VAULT}/signals/attention")).await?;

    assert!(
        open.contains("task.claim-lapsed") || open.contains("Task claim lapsed"),
        "{open}"
    );
    assert!(!cleared.contains("Task claim lapsed"), "{cleared}");
    Ok(())
}

/// The number the sidebar shows beside Signals on the vault's home page.
async fn signals_badge(router: &Router) -> Result<String, BoxError> {
    let (_, body) = get(router, &format!("/{VAULT}/")).await?;
    let entry = body
        .split("<li>")
        .find(|item| item.contains("<span class=\"nav-link-label\">Signals</span>"))
        .ok_or("no Signals entry")?;
    let after = entry.split("<span class=\"nav-link-meta\">").nth(1).ok_or("no count")?;
    Ok(after.split('<').next().unwrap_or_default().to_owned())
}

#[tokio::test]
async fn every_open_warning_is_counted_however_many_there_are() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    for n in 0..450_u32 {
        put(
            fixture.vault.path(),
            &EventSpec {
                kind: "import.failed",
                severity: "warning",
                at: &format!("2026-10-{:02}T{:02}:{:02}:00Z", 1 + n / 100, (n / 60) % 24, n % 60),
                ..EventSpec::default()
            },
            &format!("w{n:03}"),
        )?;
    }

    assert_eq!(signals_badge(&fixture.router).await?, "450");
    Ok(())
}

#[tokio::test]
async fn a_cleared_warning_stays_cleared_however_many_unrelated_events_follow_the_event_that_cleared_it()
-> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    put(
        root,
        &EventSpec {
            kind: "task.claim-lapsed",
            severity: "warning",
            at: "2026-10-01T00:00:00Z",
            subject: Some(TASK_LINK),
            ..EventSpec::default()
        },
        "lapsed",
    )?;
    put(
        root,
        &EventSpec {
            kind: "task.state-changed",
            at: "2026-10-01T01:00:00Z",
            subject: Some(TASK_LINK),
            ..EventSpec::default()
        },
        "cleared",
    )?;
    for n in 0..2_100_u32 {
        put(
            root,
            &EventSpec {
                kind: "vault.recovered",
                at: &format!("2026-10-02T{:02}:{:02}:{:02}Z", n / 3600, (n / 60) % 60, n % 60),
                fields: vec![("code", "elsewhere/recovered")],
                ..EventSpec::default()
            },
            &format!("rec{n:04}"),
        )?;
    }

    assert_eq!(signals_badge(&fixture.router).await?, "0");
    Ok(())
}

#[tokio::test]
async fn the_sidebar_count_is_kept_briefly_and_forgotten_when_the_reader_sees_attention_change() -> Result<(), BoxError>
{
    let fixture = fixture().await?;
    let warn = |slug: &str| {
        put(
            fixture.vault.path(),
            &EventSpec {
                kind: "import.failed",
                severity: "warning",
                ..EventSpec::default()
            },
            slug,
        )
    };
    warn("first")?;
    assert_eq!(signals_badge(&fixture.router).await?, "1");

    warn("second")?;
    assert_eq!(
        signals_badge(&fixture.router).await?,
        "1",
        "a count just worked out is reused"
    );

    fixture
        .clients
        .primary()
        .ok_or("a session")?
        .open_counts()
        .invalidate(VAULT);
    assert_eq!(
        signals_badge(&fixture.router).await?,
        "2",
        "forgetting it reads the vault again"
    );
    Ok(())
}

#[tokio::test]
async fn a_filtered_first_page_keeps_prepending_the_live_events_that_match_its_filters() -> Result<(), BoxError> {
    let fixture = fixture().await?;

    let (_, filtered) = get(
        &fixture.router,
        &format!("/{VAULT}/signals/feed?kind=import.failed&severity=error&entity=acme"),
    )
    .await?;

    assert!(
        filtered.contains("sse-swap=\"signal\"") && filtered.contains("hx-swap=\"afterbegin\""),
        "{filtered}"
    );
    for filter in [
        "data-filter-kind=\"import.failed\"",
        "data-filter-severity=\"error\"",
        "data-filter-entity=\"acme\"",
    ] {
        assert!(filtered.contains(filter), "{filter}: {filtered}");
    }
    assert!(
        filtered.contains("hx-on:htmx:sse-before-message") && filtered.contains("event.preventDefault()"),
        "an event that does not match, or that the list already shows, is not added: {filtered}"
    );
    assert!(
        filtered.contains("hx-on::after-swap") && filtered.contains("remove()"),
        "the list is capped: {filtered}"
    );
    assert!(!filtered.contains("paused"), "{filtered}");
    Ok(())
}

#[tokio::test]
async fn a_feed_page_after_the_first_does_not_take_live_events() -> Result<(), BoxError> {
    let fixture = fixture().await?;

    let (_, second) = get(&fixture.router, &format!("/{VAULT}/signals/feed?page=2")).await?;

    assert!(!second.contains("sse-swap=\"signal\""), "{second}");
    Ok(())
}
