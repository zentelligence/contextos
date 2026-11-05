//! Manual acceptance measurement: query latency on a 10,000-note vault.
//!
//! The non-functional requirement is a text query under 200 ms at the 95th
//! percentile on a 10,000-file vault. This builds a deterministic synthetic
//! vault with realistic score variation (varied term frequency and length,
//! a broad term in half the notes, a rare term in one per cent) and reports
//! p50 and p95 for each query shape, through the same service entry points
//! the tools use, including the per-query index refresh.
//!
//! It measures wall-clock time, so it is ignored in the normal suite. Run it
//! in release mode:
//!
//! ```sh
//! cargo test -p contextos-search --release --test manual_query_latency -- --ignored --nocapture
//! ```

use std::fs;
use std::time::{Duration, Instant};

use contextos_core::VaultRootId;
use contextos_search::{
    BlendedQuery, FakeEmbedder, GraphBackend, MinRelevance, RebuildTarget, ResultControls, SearchMode, SemanticConfig,
    SemanticQuery, SortKey, SortSpec, TextQuery, VaultSearchConfig, VaultSearchService,
};
use serde_json::Map;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const NOTES: usize = 10_000;
const VOCABULARY: usize = 400;
const REPETITIONS: usize = 30;

/// A small deterministic generator, so the vault is identical on every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(self.0 >> 33).unwrap_or(0) % bound
    }
}

fn build_vault(vault: &tempfile::TempDir) -> TestResult {
    let mut random = Lcg(42);
    for number in 0..NOTES {
        let words = 80 + number % 90;
        let mut body: Vec<String> = (0..words)
            .map(|_| format!("term{:03}", random.next(VOCABULARY)))
            .collect();
        if number % 2 == 0 {
            let repeats = 1 + (number * 7) % 5;
            body.extend(std::iter::repeat_n("common".to_owned(), repeats));
        }
        if number % 100 == 0 {
            body.push("rareword".to_owned());
        }
        let folder = number % 20;
        let note_type = if number % 3 == 0 { "task" } else { "note" };
        let path = vault.path().join(format!("area{folder:02}/note-{number:05}.md"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            path,
            format!(
                "---\ntitle: Note {number:05} {}\ntype: {note_type}\ndescription: Synthetic note {number}\n---\n\n{}\n",
                body[0],
                body.join(" ")
            ),
        )?;
    }
    Ok(())
}

fn percentile(samples: &mut [Duration], percent: usize) -> Duration {
    samples.sort_unstable();
    let last = samples.len().saturating_sub(1);
    samples[(last * percent).div_ceil(100).min(last)]
}

fn measure(label: &str, mut run: impl FnMut() -> Result<usize, Box<dyn std::error::Error>>) -> TestResult {
    // One unmeasured call warms the index reader and the file cache.
    run()?;
    let mut samples = Vec::with_capacity(REPETITIONS);
    let mut total = 0;
    for _ in 0..REPETITIONS {
        let started = Instant::now();
        total = run()?;
        samples.push(started.elapsed());
    }
    let p50 = percentile(&mut samples, 50);
    let p95 = percentile(&mut samples, 95);
    println!("{label:<52} matches {total:>6}   p50 {p50:>9.1?}   p95 {p95:>9.1?}");
    Ok(())
}

fn build_service(vault: &tempfile::TempDir) -> Result<VaultSearchService, Box<dyn std::error::Error>> {
    Ok(VaultSearchService::try_from(VaultSearchConfig {
        root_id: VaultRootId::try_from(0_usize)?,
        root: vault.path().to_path_buf(),
        excludes: vec![],
        link_excludes: Vec::new(),
        hidden: Vec::new(),
        state_directory: vault.path().join(".contextos"),
        text_enabled: true,
        graph_enabled: false,
        graph_backend: GraphBackend::Fjall,
        semantic: Some(SemanticConfig {
            embedder: Box::new(FakeEmbedder::default()),
            vector_store_path: vault.path().join(".contextos/vectors.db"),
        }),
    })?)
}

/// Builds both indexes up front and reports how long each took.
fn build_indexes(service: &VaultSearchService) -> TestResult {
    let none = Map::new();
    let started = Instant::now();
    service.query_text_page(
        &TextQuery {
            query: "rareword",
            path_prefix: None,
            exclude_paths: &[],
            tags: &[],
            fields: &none,
            limit: 1,
        },
        &ResultControls::default(),
    )?;
    println!(
        "first query built the {NOTES}-note text index in {:.1?}",
        started.elapsed()
    );
    let started = Instant::now();
    service.rebuild(RebuildTarget::Semantic)?;
    println!("semantic rebuild of {NOTES} notes took {:.1?}", started.elapsed());
    println!();
    Ok(())
}

fn sorted(key: SortKey) -> ResultControls<'static> {
    ResultControls {
        sort: SortSpec::from(key),
        ..ResultControls::default()
    }
}

fn measure_text(service: &VaultSearchService) -> TestResult {
    let none = Map::new();
    let text = |term: &'static str| TextQuery {
        query: term,
        path_prefix: None,
        exclude_paths: &[],
        tags: &[],
        fields: &none,
        limit: 20,
    };

    measure("text: rare term, relevance", || {
        Ok(service
            .query_text_page(&text("rareword"), &ResultControls::default())?
            .total)
    })?;
    measure("text: broad term, relevance (default)", || {
        Ok(service
            .query_text_page(&text("common"), &ResultControls::default())?
            .total)
    })?;
    measure("text: broad term, relevance, page 2 (offset 20)", || {
        let controls = ResultControls {
            offset: 20,
            ..ResultControls::default()
        };
        Ok(service.query_text_page(&text("common"), &controls)?.total)
    })?;
    measure("text: broad term, min relevance 0.5", || {
        let controls = ResultControls {
            min_relevance: MinRelevance::try_from(0.5)?,
            ..ResultControls::default()
        };
        Ok(service.query_text_page(&text("common"), &controls)?.total)
    })?;
    measure("text: broad term, sorted by modified", || {
        Ok(service
            .query_text_page(&text("common"), &sorted(SortKey::Modified))?
            .total)
    })?;
    measure("text: broad term, sorted by title", || {
        Ok(service.query_text_page(&text("common"), &sorted(SortKey::Title))?.total)
    })?;
    measure("text: broad term, note type filter", || {
        let controls = ResultControls {
            note_type: Some("task"),
            ..ResultControls::default()
        };
        Ok(service.query_text_page(&text("common"), &controls)?.total)
    })?;
    measure("text: unpaged query() on the index, broad term", || {
        Ok(service.query_text(&text("common"))?.0.len())
    })
}

fn measure_semantic_and_blended(service: &VaultSearchService) -> TestResult {
    let semantic = SemanticQuery {
        query: "common term042 term117",
        limit: 20,
        path_prefix: None,
        exclude_paths: &[],
    };
    measure("semantic: page of 20", || {
        Ok(service
            .query_semantic_page(&semantic, &ResultControls::default())?
            .total)
    })?;
    let blended = BlendedQuery {
        query: "common",
        limit: 20,
        path_prefix: None,
        exclude_paths: &[],
        mode: SearchMode::Blended,
    };
    measure("blended: fused, relevance", || {
        Ok(service.query_blended(&blended, &ResultControls::default())?.total)
    })?;
    measure("blended: fused, sorted by title", || {
        Ok(service.query_blended(&blended, &sorted(SortKey::Title))?.total)
    })
}

#[test]
#[ignore = "manual acceptance measurement; wall-clock based, run with --ignored --nocapture in release mode"]
fn query_latency_on_a_ten_thousand_note_vault() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    build_vault(&vault)?;
    let service = build_service(&vault)?;
    build_indexes(&service)?;
    measure_text(&service)?;
    measure_semantic_and_blended(&service)
}
