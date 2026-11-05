//! Measures what it costs, in MCP round trips and wall time, to serve a rendered note, so a change to how content is
//! rendered can be judged against a recorded baseline rather than an impression.
//!
//! Ignored by default because it prints a report and asserts nothing about timing, which varies by machine. Run it with
//! `cargo test -p contextos-web --test render_baseline -- --ignored --nocapture`. The round trip counts are
//! deterministic for a given vault and are the number to compare; the latencies are indicative only, and come from a
//! debug build like every other test in this crate.

mod support;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

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
const RUNS: usize = 40;

/// Tool calls seen so far, by tool name.
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
        // A second global subscriber is not possible, and this file installs exactly one.
        let _ = tracing::subscriber::set_global_default(Registry::default().with(CallCounter(Arc::clone(&calls))));
        calls
    }))
}

/// Appends formatted text to a string, which cannot fail.
fn writeln_to(target: &mut String, arguments: std::fmt::Arguments<'_>) {
    use std::fmt::Write as _;
    // Writing to a `String` never returns an error.
    let _ = target.write_fmt(arguments);
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// A vault in which every note lives under the wiki folder, so links resolve to a screen, alongside a tasks base so
/// that a page also pays for base scope discovery, as a real vault's would.
fn vault() -> Result<tempfile::TempDir, BoxError> {
    let vault = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    write(
        vault.path(),
        "memory/tasks.base",
        "filters: file.inFolder(\"memory/tasks\")\nviews:\n  - type: table\n    name: All\n    order:\n      - file.path\n",
    )?;
    for id in 1..=5 {
        write(
            vault.path(),
            &format!("memory/tasks/t-{id:03}.md"),
            &format!("---\nid: T-{id:03}\ntitle: Task {id}\n---\n\nBody.\n"),
        )?;
    }
    write(
        vault.path(),
        "memory/wiki/plain.md",
        &format!(
            "---\ntitle: Plain\n---\n\n# Plain\n\n{}\n\n```rust\nfn main() {{}}\n```\n",
            "A paragraph of ordinary prose with **emphasis** and a [standard link](https://example.com). ".repeat(20)
        ),
    )?;
    for n in 1..=25 {
        write(
            vault.path(),
            &format!("memory/wiki/topic-{n:02}.md"),
            &format!("---\ntitle: Topic {n}\n---\n\n# Topic {n}\n\nSee [[topic-01]].\n"),
        )?;
    }
    let mut links = String::from("---\ntitle: Links\n---\n\n# Links\n\n");
    for n in 1..=25 {
        writeln_to(
            &mut links,
            format_args!("- [[topic-{n:02}]] and [[topic-{n:02}|again]]\n"),
        );
    }
    for n in 1..=5 {
        writeln_to(&mut links, format_args!("- [[missing-{n}]]\n"));
    }
    write(vault.path(), "memory/wiki/links.md", &links)?;
    for n in 1..=3 {
        write(vault.path(), &format!("memory/wiki/pic-{n}.png"), "not really a png")?;
    }
    let mut embeds = String::from("---\ntitle: Embeds\n---\n\n# Embeds\n\n");
    for n in 1..=8 {
        writeln_to(&mut embeds, format_args!("![[topic-{n:02}]]\n\n"));
    }
    for n in 1..=3 {
        writeln_to(&mut embeds, format_args!("![[pic-{n}.png|300]]\n\n"));
    }
    write(vault.path(), "memory/wiki/embeds.md", &embeds)?;
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

async fn fetch(router: &Router, path: &str) -> Result<(), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    to_bytes(response.into_body(), usize::MAX).await?;
    if status != StatusCode::OK {
        return Err(format!("{path} answered {status}").into());
    }
    Ok(())
}

struct Measured {
    name: &'static str,
    calls_per_page: BTreeMap<String, f64>,
    p50: Duration,
    p95: Duration,
}

/// Calls per page, from the total over every run. Counts here are far below the range where a float loses precision.
fn average(total: u64) -> f64 {
    f64::from(u32::try_from(total).unwrap_or(u32::MAX)) / f64::from(u32::try_from(RUNS).unwrap_or(u32::MAX))
}

async fn measure(router: &Router, name: &'static str, note: &str) -> Result<Measured, BoxError> {
    let calls = counter();
    let path = format!("/{VAULT}/wiki/{note}.md");
    fetch(router, &path).await?; // a warm-up, so the first run does not carry start-up cost
    calls.lock().unwrap_or_else(PoisonError::into_inner).clear();
    let mut times = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let started = Instant::now();
        fetch(router, &path).await?;
        times.push(started.elapsed());
    }
    times.sort();
    let calls_per_page = calls
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|(tool, count)| (tool.trim_matches('"').to_owned(), average(*count)))
        .collect();
    Ok(Measured {
        name,
        calls_per_page,
        p50: times[RUNS / 2],
        p95: times[RUNS * 95 / 100],
    })
}

#[tokio::test]
#[ignore = "prints a measurement report; run explicitly"]
async fn report_the_cost_of_rendering_a_note() -> Result<(), BoxError> {
    let vault = vault()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    let results = [
        measure(&router, "plain (no links)", "plain").await?,
        measure(&router, "links (25 distinct, each twice, 5 dead)", "links").await?,
        measure(&router, "embeds (8 notes, 3 images)", "embeds").await?,
    ];

    println!("\nRendering cost per served note ({RUNS} runs each, debug build)\n");
    for result in &results {
        let total: f64 = result.calls_per_page.values().sum();
        println!(
            "{}: {total:.1} MCP calls, p50 {:?}, p95 {:?}",
            result.name, result.p50, result.p95
        );
        for (tool, count) in &result.calls_per_page {
            println!("    {tool}: {count:.1}");
        }
    }
    Ok(())
}
