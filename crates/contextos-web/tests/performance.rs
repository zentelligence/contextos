//! Performance test: a note under 50 KB with no
//! Mermaid/Canvas dependency serves in under 200 ms p95 over a repeated-
//! request sample.
//!
//! The proxy route's own overhead (request
//! receipt to tool-call dispatch, excluding the tool's own execution time,
//! under 20 ms p95) is not covered here: isolating that overhead from the
//! underlying tool call's own latency needs internal instrumentation
//! around the `call_tool` await in `proxy.rs` (or a synthetic zero-latency
//! fake tool), neither of which this black-box HTTP suite can measure
//! honestly. Left as a residual follow-up rather than a fabricated pass.
//!
//! The budget is the shipped product's, so it is measured in a release build: `just performance`, which `just ci` and
//! CI's Linux job run. An unoptimised test build serves the same note in about 160 ms, against about 40 ms optimised,
//! so a debug run skips it rather than measure a build nobody ships.
//!
//! It gates on Linux only. GitHub-hosted macOS and Windows runners are shared, oversubscribed hosts whose wall-clock
//! latency for a process-spawning, stdio-heavy workload like this one (a real `contextos-mcp` child per run) has varied
//! far more than any margin a fixed budget can allow. `--ignored` runs it anywhere for a spot check on real hardware.

mod support;

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";
const SAMPLE_COUNT: usize = 40;
const WARMUP_COUNT: usize = 5;

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
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

fn p95(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    // Integer-only ceil(len * 0.95) via ceil(len * 95 / 100), avoiding any
    // float/`usize` cast.
    let index = (samples.len() * 95).div_ceil(100);
    samples[index.saturating_sub(1).min(samples.len() - 1)]
}

#[tokio::test]
#[cfg_attr(
    debug_assertions,
    ignore = "the latency budget is the shipped product's, measured in a release build by `just performance`"
)]
#[cfg_attr(
    all(not(debug_assertions), not(target_os = "linux")),
    ignore = "wall-clock p95 budget is Linux-only in CI; shared macOS/Windows runners are too noisy \
              for this absolute-latency assertion, see this file's module documentation"
)]
async fn a_small_note_with_no_diagram_dependency_serves_under_200ms_p95() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;

    // A representative note under 50 KB: prose, a couple of wikilinks, no
    // Mermaid or Canvas dependency (deliberately excluded from this budget).
    let mut body = String::from("# Performance fixture\n\n");
    for i in 0..200 {
        let _ = writeln!(body, "Paragraph {i} of ordinary prose with a [[target-note]] link.\n");
    }
    assert!(body.len() < 50 * 1024, "fixture note must stay under 50 KB");
    // A wiki note, served by the wiki screen: notes are only ever shown inside their section.
    write(vault_dir.path(), "memory/wiki/target-note.md", "# Target\n")?;
    write(vault_dir.path(), "memory/wiki/note.md", &body)?;

    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let mut samples = Vec::with_capacity(SAMPLE_COUNT);
    for i in 0..(WARMUP_COUNT + SAMPLE_COUNT) {
        let start = Instant::now();
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/{VAULT_NAME}/wiki/note.md"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let _ = to_bytes(response.into_body(), usize::MAX).await?;
        let elapsed = start.elapsed();
        if i >= WARMUP_COUNT {
            samples.push(elapsed);
        }
    }

    let p95_latency = p95(samples);
    assert!(
        p95_latency < Duration::from_millis(200),
        "p95 latency {p95_latency:?} exceeded the 200 ms budget"
    );
    Ok(())
}
