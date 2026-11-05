//! What it costs, in MCP calls, to show a note, held to the redesign's goals rather than measured for a report: a note
//! that refers to nothing in the vault costs no more than the recorded baseline, links cost nothing each, and an
//! embedded note costs one read. Call counts are deterministic for a given vault, so these are exact assertions;
//! timing is reported separately by `render_baseline.rs`.

mod support;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_web::McpClientSet;
use tower::ServiceExt as _;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt as _};
use tracing_subscriber::{Layer, Registry};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT: &str = "contract-fixture";

/// The calls a plain wiki page cost in the baseline recorded before the redesign (a page that links to nothing in the
/// vault). Showing such a note must cost no more: anything above this is a fixed cost the renderer added to every
/// page. A change to the page shell that legitimately costs more moves this number, deliberately, in review.
const PLAIN_PAGE_BASELINE: u64 = 19;

static CALLS: OnceLock<Arc<Mutex<BTreeMap<String, u64>>>> = OnceLock::new();

struct CallCounter(Arc<Mutex<BTreeMap<String, u64>>>);

struct ToolName(Option<String>);

impl Visit for ToolName {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "tool" {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for CallCounter {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != "contextos_web::mcp_call" {
            return;
        }
        let mut tool = ToolName(None);
        event.record(&mut tool);
        if let Some(name) = tool.0 {
            *self
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(name)
                .or_insert(0) += 1;
        }
    }
}

fn counter() -> Arc<Mutex<BTreeMap<String, u64>>> {
    Arc::clone(CALLS.get_or_init(|| {
        let calls = Arc::new(Mutex::new(BTreeMap::new()));
        // This file installs exactly one global subscriber.
        let _ = tracing::subscriber::set_global_default(Registry::default().with(CallCounter(Arc::clone(&calls))));
        calls
    }))
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

fn note_with(title: &str, body: &str) -> String {
    format!("---\ntitle: {title}\n---\n\n# {title}\n\n{body}\n")
}

fn vault() -> Result<tempfile::TempDir, BoxError> {
    let vault = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    write(
        vault.path(),
        "memory/wiki/plain.md",
        &note_with(
            "Plain",
            &"Ordinary prose with **emphasis** and a [web link](https://example.com). ".repeat(20),
        ),
    )?;
    for n in 1..=25 {
        write(
            vault.path(),
            &format!("memory/wiki/topic-{n:02}.md"),
            &note_with(&format!("Topic {n}"), "Text."),
        )?;
    }
    for (name, count) in [("few-links", 5), ("many-links", 25)] {
        let mut body = String::new();
        for n in 1..=count {
            let _ = writeln!(body, "- [[topic-{n:02}]] and [[topic-{n:02}|again]]");
        }
        write(vault.path(), &format!("memory/wiki/{name}.md"), &note_with(name, &body))?;
    }
    for (name, count) in [("few-embeds", 2), ("many-embeds", 6)] {
        let mut body = String::new();
        for n in 1..=count {
            let _ = writeln!(body, "![[topic-{n:02}]]\n");
        }
        write(vault.path(), &format!("memory/wiki/{name}.md"), &note_with(name, &body))?;
    }
    Ok(vault)
}

async fn router_over(vault_dir: &Path, config_dir: &Path) -> Result<Router, BoxError> {
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
    ))
}

/// The MCP calls one request for the wiki page of `note` makes.
async fn calls_for(router: &Router, note: &str) -> Result<u64, BoxError> {
    let calls = counter();
    let path = format!("/{VAULT}/wiki/{note}.md");
    calls.lock().unwrap_or_else(PoisonError::into_inner).clear();
    let response = router
        .clone()
        .oneshot(Request::builder().uri(&path).body(Body::empty())?)
        .await?;
    let status = response.status();
    to_bytes(response.into_body(), usize::MAX).await?;
    if status != StatusCode::OK {
        return Err(format!("{path} answered {status}").into());
    }
    Ok(calls.lock().unwrap_or_else(PoisonError::into_inner).values().sum())
}

// One test, measuring one page at a time: the counter is global to this binary, so pages measured concurrently would
// share their counts.
#[tokio::test]
async fn showing_a_note_costs_nothing_per_link_and_one_read_per_embedded_note() -> Result<(), BoxError> {
    let vault = vault()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;
    // A first request pays start-up costs no page pays again.
    calls_for(&router, "plain").await?;

    let plain = calls_for(&router, "plain").await?;
    assert!(
        plain <= PLAIN_PAGE_BASELINE,
        "a note that refers to nothing in the vault cost {plain} calls, above the baseline of {PLAIN_PAGE_BASELINE}"
    );

    let few_links = calls_for(&router, "few-links").await?;
    let many_links = calls_for(&router, "many-links").await?;
    assert_eq!(few_links, many_links, "links must cost nothing each");

    let few_embeds = calls_for(&router, "few-embeds").await?;
    let many_embeds = calls_for(&router, "many-embeds").await?;
    assert_eq!(many_embeds - few_embeds, 4, "each embedded note costs one read");
    Ok(())
}
