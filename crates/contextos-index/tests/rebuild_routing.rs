//! A rebuild writes on its own account, so every index it writes is handed to the supplied router as it is written:
//! nothing it changes can be left for Git and search to miss, and a routing fault is reported, not lost.

use std::path::Path;
use std::sync::{Arc, Mutex};

use contextos_core::{
    NoCreationReports, OpKind, OperationEvent, OperationWarning, Origin, RoutesOperations, VaultPath, VaultPathInput,
    VaultRoot, VaultRootInput, VaultSet,
};
use contextos_fs::{
    Filesystem, FilesystemConfig, FilesystemService, FilesystemServiceConfig, FsLimits, default_hidden_patterns,
};
use contextos_index::{IndexRebuildOptions, IndexService, IndexServiceConfig};
use tempfile::tempdir;
use time::OffsetDateTime;
use time::macros::datetime;

type BoxError = Box<dyn std::error::Error>;

#[derive(Clone, Copy, Debug)]
struct FixedClock;

impl contextos_core::Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        datetime!(2026-09-28 09:00:00 +10:00)
    }
}

#[derive(Clone, Debug, Default)]
struct RecordingRouter {
    routed: Arc<Mutex<Vec<OperationEvent>>>,
    /// The size of each batch handed over, in order.
    batches: Arc<Mutex<Vec<usize>>>,
    warn: bool,
}

impl RecordingRouter {
    fn seen(&self) -> Result<Vec<OperationEvent>, BoxError> {
        Ok(self.routed.lock().map_err(|_| "router poisoned")?.clone())
    }
}

impl RoutesOperations for RecordingRouter {
    fn route_all(&self, events: Vec<OperationEvent>) -> contextos_core::RoutedEvents {
        if let Ok(mut batches) = self.batches.lock() {
            batches.push(events.len());
        }
        let warnings = events.iter().flat_map(|event| self.route(event)).collect();
        contextos_core::RoutedEvents { events, warnings }
    }

    fn route(&self, event: &OperationEvent) -> Vec<OperationWarning> {
        if let Ok(mut routed) = self.routed.lock() {
            routed.push(event.clone());
        }
        if self.warn {
            vec![OperationWarning {
                code: "git/stage".to_owned(),
                message: "staging is degraded".to_owned(),
            }]
        } else {
            Vec::new()
        }
    }
}

struct Fixture {
    service: IndexService<Filesystem, FilesystemService<FixedClock>, FixedClock, NoCreationReports>,
    roots: VaultSet,
}

fn fixture(vault: &Path) -> Result<Fixture, BoxError> {
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![root.clone()])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![FsLimits {
            max_read_bytes: 1024 * 1024,
            max_batch_files: 50,
        }],
        hidden: vec![default_hidden_patterns()],
        atomic_write_guard: None,
    })?;
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: filesystem.clone(),
        clock: FixedClock,
    });
    let service = IndexService::try_from(IndexServiceConfig {
        root,
        roots: roots.clone(),
        reader: filesystem,
        writer,
        excluded: Vec::new(),
        clock: FixedClock,
        creation: NoCreationReports,
    })?;
    Ok(Fixture { service, roots })
}

impl Fixture {
    fn whole_vault(&self) -> Result<VaultPath, BoxError> {
        Ok(VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw: "vault://.",
        })?)
    }
}

fn two_directory_vault() -> Result<tempfile::TempDir, BoxError> {
    let vault = tempdir()?;
    std::fs::create_dir_all(vault.path().join("alpha/beta"))?;
    std::fs::write(vault.path().join("alpha/note.md"), "# Note\n")?;
    std::fs::write(vault.path().join("alpha/beta/leaf.md"), "# Leaf\n")?;
    Ok(vault)
}

#[test]
fn every_index_a_rebuild_writes_is_routed_as_an_internal_write() -> Result<(), BoxError> {
    let vault = two_directory_vault()?;
    let fixture = fixture(vault.path())?;
    let router = RecordingRouter::default();

    let report = fixture.service.rebuild_report_with(
        &fixture.whole_vault()?,
        Origin::Tool("vault_index_rebuild".to_owned()),
        IndexRebuildOptions::default(),
        &router,
    )?;

    let seen = router.seen()?;
    assert_eq!(report.indexes_created, 3);
    assert_eq!(seen.len(), 3, "one routed event per index written: {seen:?}");
    assert!(
        seen.iter()
            .all(|event| event.origin == Origin::Internal("vault_index_rebuild".to_owned()))
    );
    let mut paths = seen
        .iter()
        .flat_map(|event| event.paths.iter().map(VaultPath::portable_relative))
        .collect::<Vec<_>>();
    paths.sort();
    assert_eq!(paths, ["alpha/beta/index.md", "alpha/index.md", "index.md"]);
    assert_eq!(report.events.len(), 3);
    Ok(())
}

#[test]
fn a_rebuild_that_changes_nothing_or_only_previews_routes_nothing() -> Result<(), BoxError> {
    let vault = two_directory_vault()?;
    let fixture = fixture(vault.path())?;
    let router = RecordingRouter::default();
    let origin = || Origin::Tool("vault_index_rebuild".to_owned());

    fixture.service.rebuild_report_with(
        &fixture.whole_vault()?,
        origin(),
        IndexRebuildOptions {
            dry_run: true,
            repair: false,
        },
        &router,
    )?;
    assert!(router.seen()?.is_empty(), "a preview writes nothing, so routes nothing");

    fixture.service.rebuild_report_with(
        &fixture.whole_vault()?,
        origin(),
        IndexRebuildOptions::default(),
        &RecordingRouter::default(),
    )?;
    fixture.service.rebuild_report_with(
        &fixture.whole_vault()?,
        origin(),
        IndexRebuildOptions::default(),
        &router,
    )?;
    assert!(
        router.seen()?.is_empty(),
        "a no-op rebuild writes nothing, so routes nothing"
    );
    Ok(())
}

#[test]
fn a_legacy_index_move_is_routed_with_the_rewrite() -> Result<(), BoxError> {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("notes"))?;
    std::fs::write(vault.path().join("notes/_index.md"), "# Notes\n")?;
    std::fs::write(vault.path().join("notes/note.md"), "# Note\n")?;
    let fixture = fixture(vault.path())?;
    let router = RecordingRouter::default();

    fixture.service.rebuild_report_with(
        &fixture.whole_vault()?,
        Origin::Tool("vault_index_rebuild".to_owned()),
        IndexRebuildOptions::default(),
        &router,
    )?;

    let kinds = router.seen()?.iter().map(|event| event.kind).collect::<Vec<_>>();
    assert!(kinds.contains(&OpKind::Move), "{kinds:?}");
    Ok(())
}

#[test]
fn routing_warnings_are_reported_against_the_first_index_of_the_batch_that_raised_them() -> Result<(), BoxError> {
    let vault = two_directory_vault()?;
    let fixture = fixture(vault.path())?;
    let router = RecordingRouter {
        warn: true,
        ..RecordingRouter::default()
    };

    let report = fixture.service.rebuild_report_with(
        &fixture.whole_vault()?,
        Origin::Tool("vault_index_rebuild".to_owned()),
        IndexRebuildOptions::default(),
        &router,
    )?;

    assert_eq!(report.warnings.len(), 3, "one warning per routed write");
    assert!(
        report
            .warnings
            .iter()
            .all(|warning| warning.message.contains("git/stage"))
    );
    let first = report
        .events
        .first()
        .and_then(|event| event.paths.first())
        .map(VaultPath::portable_relative);
    assert_eq!(
        report.warnings.first().map(|warning| Some(warning.path.clone())),
        Some(first)
    );
    Ok(())
}

#[test]
fn writes_are_routed_in_bounded_batches_and_the_last_partial_batch_is_not_lost() -> Result<(), BoxError> {
    let vault = tempdir()?;
    for number in 0..120 {
        std::fs::create_dir_all(vault.path().join(format!("dir-{number:03}")))?;
    }
    let fixture = fixture(vault.path())?;
    let router = RecordingRouter::default();

    let report = fixture.service.rebuild_report_with(
        &fixture.whole_vault()?,
        Origin::Tool("vault_index_rebuild".to_owned()),
        IndexRebuildOptions::default(),
        &router,
    )?;

    let batches = router.batches.lock().map_err(|_| "router poisoned")?.clone();
    assert_eq!(report.indexes_created, 121, "the root and 120 directories");
    assert_eq!(batches.iter().sum::<usize>(), 121, "every write is routed exactly once");
    assert!(
        batches.len() < 121,
        "writes are batched, not routed one at a time: {batches:?}"
    );
    assert!(
        batches.iter().all(|size| *size <= 64),
        "a batch stays bounded: {batches:?}"
    );
    assert_eq!(router.seen()?.len(), 121);
    Ok(())
}

#[test]
fn writes_made_before_a_rebuild_fails_part_way_are_still_routed() -> Result<(), BoxError> {
    let vault = tempdir()?;
    // The root is written first; the failure comes one level down, when `alpha` reads `broken`'s own index.
    std::fs::create_dir_all(vault.path().join("alpha/broken"))?;
    std::fs::write(
        vault.path().join("alpha/broken/index.md"),
        "---\ntype: [unclosed\n---\n",
    )?;
    std::fs::write(vault.path().join("alpha/broken/note.md"), "# Note\n")?;
    let fixture = fixture(vault.path())?;
    let router = RecordingRouter::default();

    let outcome = fixture.service.rebuild_report_with(
        &fixture.whole_vault()?,
        Origin::Tool("vault_index_rebuild".to_owned()),
        IndexRebuildOptions::default(),
        &router,
    );

    assert!(outcome.is_err(), "the malformed frontmatter fails the rebuild");
    let paths = router
        .seen()?
        .iter()
        .flat_map(|event| event.paths.iter().map(VaultPath::portable_relative))
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        ["index.md"],
        "the root index was written before the failure, so it must not be left unrouted"
    );
    Ok(())
}
