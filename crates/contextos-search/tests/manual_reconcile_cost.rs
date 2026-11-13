//! Manual acceptance measurement: what bringing the link graph up to date costs on a 10,000-note vault with 2,000
//! attachments, unchanged and after a handful of external edits.
//!
//! It measures wall-clock time, so it is ignored in the normal suite. Run it in release mode:
//!
//! ```sh
//! cargo test -p contextos-search --release --test manual_reconcile_cost -- --ignored --nocapture
//! ```

use std::fs;
use std::time::Instant;

use contextos_core::VaultRootId;
use contextos_search::{GraphBackend, RebuildTarget, VaultSearchConfig, VaultSearchService};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const NOTES: usize = 10_000;
const ATTACHMENTS: usize = 2_000;

#[test]
#[ignore = "wall-clock measurement; run on demand in release mode"]
fn reconciling_a_large_vault_costs_one_walk_and_a_read_of_only_what_changed() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for index in 0..NOTES {
        let folder = vault.path().join(format!("area-{}", index % 50));
        fs::create_dir_all(&folder)?;
        fs::write(
            folder.join(format!("note-{index}.md")),
            format!(
                "# Note {index}\n\n[[note-{}]] and ![[image-{}.png]]\n",
                (index + 1) % NOTES,
                index % ATTACHMENTS
            ),
        )?;
    }
    fs::create_dir_all(vault.path().join("assets"))?;
    for index in 0..ATTACHMENTS {
        fs::write(vault.path().join(format!("assets/image-{index}.png")), [1_u8])?;
    }
    let service = VaultSearchService::try_from(VaultSearchConfig {
        root_id: VaultRootId::try_from(0_usize)?,
        root: vault.path().to_path_buf(),
        excludes: vec![".contextos".to_owned()],
        link_excludes: Vec::new(),
        link_ignore_targets: Vec::new(),
        hidden: Vec::new(),
        state_directory: vault.path().join(".contextos"),
        text_enabled: false,
        graph_enabled: true,
        graph_backend: GraphBackend::Sqlite,
        semantic: None,
    })?;
    let started = Instant::now();
    service.rebuild(RebuildTarget::Graph)?;
    println!("rebuild (reads every note): {:?}", started.elapsed());

    let started = Instant::now();
    let unchanged = service.reconcile_graph()?;
    println!("reconcile, nothing changed: {:?} {unchanged:?}", started.elapsed());

    std::thread::sleep(std::time::Duration::from_millis(20));
    for index in 0..10 {
        fs::write(
            vault.path().join(format!("area-{}/note-{index}.md", index % 50)),
            format!("# Edited {index}\n\n[[note-5]] and some more text\n"),
        )?;
    }
    fs::write(vault.path().join("assets/new.png"), [2_u8])?;
    let started = Instant::now();
    let changed = service.reconcile_graph()?;
    println!(
        "reconcile, 10 edits and a new image: {:?} {changed:?}",
        started.elapsed()
    );
    Ok(())
}
