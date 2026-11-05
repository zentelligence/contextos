//! A rebuild completes each managed `index.md`'s own frontmatter from where the directory sits in the vault, keeps
//! every value already there, and leaves a directory that has not changed byte-for-byte alone.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use contextos_core::{
    Clock, MaintainsIndexes, NoCreationReports, NoSubstrateServices, OpKind, OperationEvent, Origin, ReportsCreation,
    VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet,
};
use contextos_fs::{
    Filesystem, FilesystemConfig, FilesystemService, FilesystemServiceConfig, FsLimits, default_hidden_patterns,
};
use contextos_index::{IndexRebuildOptions, IndexRepairAction, IndexService, IndexServiceConfig, IndexServiceError};
use tempfile::tempdir;
use time::OffsetDateTime;
use time::macros::datetime;

type BoxError = Box<dyn std::error::Error>;

const ENTITIES: &str = "[[entities]]\nslug = \"zenith\"\nname = \"Zenith\"\naccent = 1\n\n\
                        [[entities]]\nslug = \"personal\"\nname = \"Personal\"\naccent = 2\n";

/// Reports a later minute on every call, so a rebuild that consults the clock for anything it writes cannot leave a
/// file byte-identical by accident (stamps are kept to the minute), and shows instants in a +10:00 zone, as an
/// operator's local clock does.
#[derive(Clone, Debug)]
struct SteppingClock(Arc<AtomicI64>);

impl SteppingClock {
    fn new() -> Self {
        Self(Arc::new(AtomicI64::new(0)))
    }
}

impl Clock for SteppingClock {
    fn now(&self) -> OffsetDateTime {
        datetime!(2026-09-28 09:00:00 +10:00) + time::Duration::minutes(self.0.fetch_add(1, Ordering::SeqCst))
    }

    fn localise(&self, instant: OffsetDateTime) -> OffsetDateTime {
        instant.to_offset(time::macros::offset!(+10))
    }
}

#[derive(Clone, Debug)]
struct RecordedCreation {
    reported: Option<OffsetDateTime>,
    lookups: Arc<AtomicUsize>,
}

impl RecordedCreation {
    fn new(reported: Option<OffsetDateTime>) -> Self {
        Self {
            reported,
            lookups: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ReportsCreation for RecordedCreation {
    fn created_at(&self, _path: &VaultPath) -> Option<OffsetDateTime> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        self.reported
    }
}

type Service<C> = IndexService<Filesystem, FilesystemService<SteppingClock>, SteppingClock, C>;

struct Fixture<C> {
    service: Service<C>,
    roots: VaultSet,
}

fn fixture<C: ReportsCreation>(vault: &Path, creation: C) -> Result<Fixture<C>, BoxError> {
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
    let clock = SteppingClock::new();
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: filesystem.clone(),
        clock: clock.clone(),
    });
    let service = IndexService::try_from(IndexServiceConfig {
        root,
        roots: roots.clone(),
        reader: filesystem,
        writer,
        excluded: Vec::new(),
        clock,
        creation,
    })?;
    Ok(Fixture { service, roots })
}

impl<C: ReportsCreation> Fixture<C> {
    fn path(&self, raw: &str) -> Result<VaultPath, BoxError> {
        Ok(VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw,
        })?)
    }

    fn rebuild(
        &self,
        subtree: &str,
        options: IndexRebuildOptions,
    ) -> Result<contextos_index::IndexRebuildResult, BoxError> {
        Ok(self.service.rebuild_report_with(
            &self.path(subtree)?,
            Origin::Tool("vault_index_rebuild".to_owned()),
            options,
            &NoSubstrateServices,
        )?)
    }
}

fn write(vault: &Path, relative: &str, content: &str) -> Result<(), BoxError> {
    let path = vault.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

fn property<'a>(source: &'a str, key: &str) -> Option<&'a str> {
    let frontmatter = source.strip_prefix("---\n")?.split("\n---\n").next()?;
    let prefix = format!("{key}: ");
    frontmatter
        .lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()).map(str::trim))
}

fn read(vault: &Path, relative: &str) -> Result<String, BoxError> {
    Ok(std::fs::read_to_string(vault.join(relative))?)
}

fn snapshot(vault: &Path) -> Result<Vec<(String, Vec<u8>)>, BoxError> {
    let mut files = Vec::new();
    let mut pending = vec![vault.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(vault)?.to_string_lossy().replace('\\', "/");
                files.push((relative, std::fs::read(&path)?));
            }
        }
    }
    files.sort();
    Ok(files)
}

fn entity_vault() -> Result<tempfile::TempDir, BoxError> {
    let vault = tempdir()?;
    write(vault.path(), ".contextos/entities.toml", ENTITIES)?;
    write(vault.path(), "zenith/projects/beacon/brief.md", "# Brief\n")?;
    write(vault.path(), "personal/journal/day.md", "# Day\n")?;
    write(vault.path(), "memory/tasks/zenith/t-1.md", "# T1\n")?;
    write(vault.path(), "memory/wiki/analytics/page.md", "# Page\n")?;
    write(vault.path(), "registry/agents/agent.md", "# Agent\n")?;
    Ok(vault)
}

#[test]
fn every_directory_gets_the_entity_its_location_belongs_to() -> Result<(), BoxError> {
    let vault = entity_vault()?;
    let fixture = fixture(vault.path(), NoCreationReports)?;

    fixture.rebuild("vault://.", IndexRebuildOptions::default())?;

    for (index, entity) in [
        ("zenith/index.md", "zenith"),
        ("zenith/projects/beacon/index.md", "zenith"),
        ("personal/journal/index.md", "personal"),
        ("memory/tasks/zenith/index.md", "vault"),
        ("memory/tasks/index.md", "vault"),
        ("memory/wiki/analytics/index.md", "vault"),
        ("memory/index.md", "vault"),
        ("registry/agents/index.md", "vault"),
        ("index.md", "vault"),
    ] {
        let source = read(vault.path(), index)?;
        assert_eq!(property(&source, "entity"), Some(entity), "{index}\n{source}");
        assert_eq!(property(&source, "status"), None, "{index}\n{source}");
    }
    Ok(())
}

#[test]
fn a_vault_without_an_entity_list_gets_no_entity_rather_than_a_guess() -> Result<(), BoxError> {
    let vault = tempdir()?;
    write(vault.path(), "zenith/brief.md", "# Brief\n")?;
    let fixture = fixture(vault.path(), NoCreationReports)?;

    fixture.rebuild("vault://.", IndexRebuildOptions::default())?;

    let source = read(vault.path(), "zenith/index.md")?;
    assert_eq!(property(&source, "entity"), None, "{source}");
    assert!(source.contains("type: index\n"), "{source}");
    Ok(())
}

#[test]
fn a_malformed_entity_list_fails_the_rebuild_before_anything_is_written() -> Result<(), BoxError> {
    let vault = tempdir()?;
    write(vault.path(), ".contextos/entities.toml", "[[entities]\nslug = ")?;
    write(vault.path(), "notes/note.md", "# Note\n")?;
    let before = snapshot(vault.path())?;
    let fixture = fixture(vault.path(), NoCreationReports)?;

    let error = fixture
        .service
        .rebuild_report_with(
            &fixture.path("vault://.")?,
            Origin::Tool("vault_index_rebuild".to_owned()),
            IndexRebuildOptions::default(),
            &NoSubstrateServices,
        )
        .err()
        .ok_or("the rebuild should have failed")?;

    assert!(matches!(error, IndexServiceError::EntityRoster { .. }), "{error:?}");
    assert_eq!(error.code(), "index/entities");
    assert_eq!(snapshot(vault.path())?, before);
    Ok(())
}

#[test]
fn a_second_rebuild_changes_nothing_even_though_the_clock_has_moved_on() -> Result<(), BoxError> {
    let vault = entity_vault()?;
    let fixture = fixture(vault.path(), NoCreationReports)?;
    fixture.rebuild("vault://.", IndexRebuildOptions::default())?;
    let after_first = snapshot(vault.path())?;

    let second = fixture.rebuild("vault://.", IndexRebuildOptions::default())?;

    assert_eq!(second.indexes_created, 0);
    assert_eq!(second.indexes_updated, 0);
    assert!(
        second.events.is_empty(),
        "`second.events` should be empty, got {:?}",
        second.events
    );
    assert_eq!(snapshot(vault.path())?, after_first);
    Ok(())
}

#[test]
fn a_changed_directory_restamps_updated_and_keeps_created() -> Result<(), BoxError> {
    let vault = entity_vault()?;
    let fixture = fixture(vault.path(), NoCreationReports)?;
    fixture.rebuild("vault://.", IndexRebuildOptions::default())?;
    let before = read(vault.path(), "personal/journal/index.md")?;
    write(vault.path(), "personal/journal/another.md", "# Another\n")?;

    fixture.rebuild("vault://.", IndexRebuildOptions::default())?;

    let after = read(vault.path(), "personal/journal/index.md")?;
    assert_ne!(property(&after, "updated"), property(&before, "updated"));
    assert_eq!(property(&after, "created"), property(&before, "created"));
    assert!(after.contains("another.md"), "{after}");
    Ok(())
}

#[test]
fn existing_frontmatter_values_survive_and_only_missing_keys_are_added() -> Result<(), BoxError> {
    let vault = entity_vault()?;
    write(
        vault.path(),
        "zenith/projects/beacon/index.md",
        concat!(
            "---\ntype: index\ntitle: Operator Title\nentity: personal\nstatus: published\n",
            "created: 2020-01-01\nupdated: 2020-02-02\n---\n# Beacon\n\n",
        ),
    )?;
    let fixture = fixture(vault.path(), NoCreationReports)?;

    fixture.rebuild("zenith/projects/beacon", IndexRebuildOptions::default())?;

    let source = read(vault.path(), "zenith/projects/beacon/index.md")?;
    assert_eq!(property(&source, "title"), Some("Operator Title"));
    assert_eq!(property(&source, "entity"), Some("personal"));
    assert_eq!(property(&source, "status"), Some("published"));
    assert_eq!(property(&source, "created"), Some("2020-01-01"));
    assert_ne!(
        property(&source, "updated"),
        Some("2020-02-02"),
        "content changed, so updated moves"
    );
    assert_eq!(property(&source, "tags"), Some("[]"), "{source}");
    Ok(())
}

#[test]
fn a_file_with_no_frontmatter_gains_a_complete_block_dated_from_the_creation_source() -> Result<(), BoxError> {
    let vault = entity_vault()?;
    write(
        vault.path(),
        "personal/journal/index.md",
        "# Journal\n\nHand-written introduction.\n",
    )?;
    let creation = RecordedCreation::new(Some(datetime!(2024-03-04 05:06:07 +10:00)));
    let fixture = fixture(vault.path(), creation)?;

    fixture.rebuild("personal/journal", IndexRebuildOptions::default())?;

    let source = read(vault.path(), "personal/journal/index.md")?;
    assert_eq!(property(&source, "entity"), Some("personal"));
    assert_eq!(property(&source, "created"), Some("2024-03-04T05:06:00+10:00"));
    assert_ne!(property(&source, "updated"), property(&source, "created"));
    assert!(source.contains("Hand-written introduction."), "{source}");
    Ok(())
}

#[test]
fn a_file_no_source_can_date_is_created_at_the_rebuild_instant() -> Result<(), BoxError> {
    let vault = entity_vault()?;
    write(vault.path(), "personal/journal/index.md", "# Journal\n")?;
    let fixture = fixture(vault.path(), RecordedCreation::new(None))?;

    fixture.rebuild("personal/journal", IndexRebuildOptions::default())?;

    let source = read(vault.path(), "personal/journal/index.md")?;
    assert!(property(&source, "created").is_some(), "{source}");
    Ok(())
}

#[test]
fn history_is_consulted_only_for_a_file_that_lacks_a_creation_date() -> Result<(), BoxError> {
    let vault = entity_vault()?;
    let creation = RecordedCreation::new(Some(datetime!(2024-03-04 05:06:07 +10:00)));
    let lookups = Arc::clone(&creation.lookups);
    let fixture = fixture(vault.path(), creation)?;
    fixture.rebuild("vault://.", IndexRebuildOptions::default())?;
    assert_eq!(
        lookups.load(Ordering::SeqCst),
        0,
        "new files are dated by the clock, not history"
    );

    fixture.rebuild("vault://.", IndexRebuildOptions::default())?;

    assert_eq!(
        lookups.load(Ordering::SeqCst),
        0,
        "complete indexes never need a lookup"
    );
    Ok(())
}

fn defective_index(entity: &str, at: &str) -> String {
    format!(
        "---\ntype: index\ntitle: Journal\nentity: {entity}\nstatus: new\ncreated: {at}\nupdated: {at}\ntags: []\n\
         aliases: []\ndescription: ''\nsummary: ''\nexcerpt: ''\n---\n\n# Journal: Index\n\n"
    )
}

#[test]
fn a_new_managed_index_under_a_memory_folder_named_for_an_entity_derives_vault() -> Result<(), BoxError> {
    let vault = entity_vault()?;
    let fixture = fixture(vault.path(), NoCreationReports)?;

    fixture.rebuild("vault://.", IndexRebuildOptions::default())?;

    let source = read(vault.path(), "memory/tasks/zenith/index.md")?;
    assert_eq!(property(&source, "entity"), Some("vault"), "{source}");
    Ok(())
}

/// A managed index written under the previous rule: it holds the entity its `memory/<category>/<entity>/` folder name
/// then derived.
fn old_rule_index() -> String {
    defective_index("zenith", "2026-09-27T10:00:00+10:00").replace("status: new\n", "")
}

#[test]
fn a_rebuild_without_repair_keeps_an_index_entity_from_the_old_rule() -> Result<(), BoxError> {
    // Guard: the new rule must never rewrite an existing index's entity unless the caller asks for a repair.
    let vault = entity_vault()?;
    write(vault.path(), "memory/tasks/zenith/index.md", &old_rule_index())?;
    let fixture = fixture(vault.path(), NoCreationReports)?;

    let report = fixture.rebuild("memory/tasks/zenith", IndexRebuildOptions::default())?;

    let source = read(vault.path(), "memory/tasks/zenith/index.md")?;
    assert_eq!(property(&source, "entity"), Some("zenith"), "{source}");
    assert!(
        report.repairs.is_empty(),
        "an ordinary rebuild reports no repair, got {:?}",
        report.repairs
    );
    Ok(())
}

#[test]
fn a_repair_rebuild_rewrites_an_index_entity_from_the_old_rule_to_vault_and_reports_it_and_a_dry_run_only_reports_it()
-> Result<(), BoxError> {
    let vault = entity_vault()?;
    write(vault.path(), "memory/tasks/zenith/index.md", &old_rule_index())?;
    let fixture = fixture(vault.path(), NoCreationReports)?;
    let before = snapshot(vault.path())?;
    let expected = IndexRepairAction::EntityRederived {
        from: "zenith".to_owned(),
        to: "vault".to_owned(),
    };

    let dry = fixture.rebuild(
        "memory/tasks/zenith",
        IndexRebuildOptions {
            dry_run: true,
            repair: true,
        },
    )?;
    assert_eq!(dry.repairs.len(), 1, "{:?}", dry.repairs);
    assert!(dry.repairs[0].actions.contains(&expected), "{:?}", dry.repairs);
    assert_eq!(snapshot(vault.path())?, before, "a dry run writes nothing");

    let repaired = fixture.rebuild(
        "memory/tasks/zenith",
        IndexRebuildOptions {
            dry_run: false,
            repair: true,
        },
    )?;
    assert_eq!(repaired.repairs.len(), 1, "{:?}", repaired.repairs);
    assert_eq!(repaired.repairs[0].path, "memory/tasks/zenith/index.md");
    assert!(repaired.repairs[0].actions.contains(&expected));
    let source = read(vault.path(), "memory/tasks/zenith/index.md")?;
    assert_eq!(property(&source, "entity"), Some("vault"), "{source}");
    Ok(())
}

#[test]
fn repair_corrects_the_values_a_defective_rebuild_wrote_and_reports_each_one() -> Result<(), BoxError> {
    let vault = entity_vault()?;
    let stamp = "2026-09-27 10:00:00.0 +10:00:00";
    write(
        vault.path(),
        "personal/journal/index.md",
        &defective_index("vault", stamp),
    )?;
    let fixture = fixture(
        vault.path(),
        RecordedCreation::new(Some(datetime!(2024-03-04 05:06:07 +10:00))),
    )?;
    fixture.rebuild("vault://.", IndexRebuildOptions::default())?;
    write(
        vault.path(),
        "personal/journal/index.md",
        &defective_index("vault", stamp),
    )?;

    let report = fixture.rebuild(
        "personal/journal",
        IndexRebuildOptions {
            dry_run: false,
            repair: true,
        },
    )?;

    let source = read(vault.path(), "personal/journal/index.md")?;
    assert_eq!(property(&source, "entity"), Some("personal"));
    assert_eq!(property(&source, "status"), None, "{source}");
    assert_eq!(property(&source, "created"), Some("2024-03-04T05:06:00+10:00"));
    assert_eq!(report.repairs.len(), 1);
    assert_eq!(report.repairs[0].path, "personal/journal/index.md");
    assert!(report.repairs[0].actions.contains(&IndexRepairAction::StatusRemoved));
    assert!(report.repairs[0].actions.contains(&IndexRepairAction::EntityRederived {
        from: "vault".to_owned(),
        to: "personal".to_owned(),
    }));
    Ok(())
}

#[test]
fn a_repairing_dry_run_reports_the_repairs_without_writing_them() -> Result<(), BoxError> {
    let vault = entity_vault()?;
    let stamp = "2026-09-27 10:00:00.0 +10:00:00";
    write(
        vault.path(),
        "personal/journal/index.md",
        &defective_index("vault", stamp),
    )?;
    let before = snapshot(vault.path())?;
    let fixture = fixture(vault.path(), NoCreationReports)?;

    let report = fixture.rebuild(
        "personal/journal",
        IndexRebuildOptions {
            dry_run: true,
            repair: true,
        },
    )?;

    assert_eq!(report.repairs.len(), 1);
    assert_eq!(report.indexes_updated, 1);
    assert_eq!(snapshot(vault.path())?, before);
    Ok(())
}

#[test]
fn repair_is_idempotent_and_an_ordinary_rebuild_never_repairs() -> Result<(), BoxError> {
    let vault = entity_vault()?;
    let stamp = "2026-09-27 10:00:00.0 +10:00:00";
    write(
        vault.path(),
        "personal/journal/index.md",
        &defective_index("vault", stamp),
    )?;
    let fixture = fixture(vault.path(), NoCreationReports)?;

    let ordinary = fixture.rebuild("personal/journal", IndexRebuildOptions::default())?;
    let untouched = read(vault.path(), "personal/journal/index.md")?;
    let first = fixture.rebuild(
        "personal/journal",
        IndexRebuildOptions {
            dry_run: false,
            repair: true,
        },
    )?;
    let repaired = snapshot(vault.path())?;
    let second = fixture.rebuild(
        "personal/journal",
        IndexRebuildOptions {
            dry_run: false,
            repair: true,
        },
    )?;

    assert!(
        ordinary.repairs.is_empty(),
        "`ordinary.repairs` should be empty, got {:?}",
        ordinary.repairs
    );
    assert_eq!(
        property(&untouched, "entity"),
        Some("vault"),
        "an ordinary rebuild keeps the value"
    );
    assert_eq!(first.repairs.len(), 1);
    assert!(
        second.repairs.is_empty(),
        "`second.repairs` should be empty, got {:?}",
        second.repairs
    );
    assert_eq!(second.indexes_updated, 0);
    assert_eq!(snapshot(vault.path())?, repaired);
    Ok(())
}

#[test]
fn an_index_with_uneditable_frontmatter_still_gets_its_links_and_reports_a_warning() -> Result<(), BoxError> {
    let vault = entity_vault()?;
    let flow_style = "---\n{type: index, title: Flow Style}\n---\n# Journal\n\nIntro.\n";
    write(vault.path(), "personal/journal/index.md", flow_style)?;
    let fixture = fixture(vault.path(), NoCreationReports)?;

    let report = fixture.rebuild("vault://.", IndexRebuildOptions::default())?;

    let source = read(vault.path(), "personal/journal/index.md")?;
    assert!(
        source.starts_with(&flow_style[..flow_style.len() - "# Journal\n\nIntro.\n".len()]),
        "{source}"
    );
    assert!(source.contains("[[personal/journal/day.md|Day]]"), "{source}");
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert_eq!(report.warnings[0].path, "personal/journal/index.md");
    assert!(
        read(vault.path(), "zenith/index.md")?.contains("type: index"),
        "the rest of the vault is still rebuilt"
    );
    let again = fixture.rebuild("vault://.", IndexRebuildOptions::default())?;
    assert_eq!(again.indexes_updated, 0);
    Ok(())
}

#[test]
fn a_malformed_entity_list_does_not_stop_ordinary_writes_being_indexed() -> Result<(), BoxError> {
    let vault = tempdir()?;
    write(vault.path(), ".contextos/entities.toml", "[[entities]\nslug = ")?;
    write(vault.path(), "notes/added.md", "# Added\n")?;
    let fixture = fixture(vault.path(), NoCreationReports)?;

    let events = fixture
        .service
        .reconcile(&OperationEvent {
            kind: OpKind::Create,
            paths: vec![fixture.path("notes/added.md")?],
            origin: Origin::Tool("fs_write_file".to_owned()),
            summary: "Created notes/added.md".to_owned(),
            at: datetime!(2026-09-28 09:00:00 +10:00),
        })
        .map_err(|warning| warning.message)?;

    assert!(!events.is_empty(), "`events` should not be empty");
    let source = read(vault.path(), "notes/index.md")?;
    assert!(source.contains("[[notes/added.md|Added]]"), "{source}");
    assert_eq!(
        property(&source, "entity"),
        None,
        "no roster means no entity, not a wrong one: {source}"
    );
    Ok(())
}

#[test]
fn history_is_consulted_once_for_each_existing_file_that_lacks_a_creation_date() -> Result<(), BoxError> {
    let vault = entity_vault()?;
    write(vault.path(), "personal/journal/index.md", "# Journal\n")?;
    write(vault.path(), "zenith/projects/beacon/index.md", "# Beacon\n")?;
    let creation = RecordedCreation::new(Some(datetime!(2024-03-04 05:06:07 +10:00)));
    let lookups = Arc::clone(&creation.lookups);
    let fixture = fixture(vault.path(), creation)?;

    fixture.rebuild("vault://.", IndexRebuildOptions::default())?;

    assert_eq!(lookups.load(Ordering::SeqCst), 2);
    Ok(())
}

#[test]
fn index_stamps_are_rfc_3339_to_the_minute_in_the_operators_zone() -> Result<(), BoxError> {
    let vault = entity_vault()?;
    write(vault.path(), "personal/journal/index.md", "# Journal\n")?;
    // The creation source reports UTC, as Git history and file birth times do; the stamp shows the operator's day.
    let creation = RecordedCreation::new(Some(datetime!(2024-03-03 19:06:07.5 UTC)));
    let fixture = fixture(vault.path(), creation)?;

    fixture.rebuild("personal/journal", IndexRebuildOptions::default())?;

    let source = read(vault.path(), "personal/journal/index.md")?;
    assert_eq!(property(&source, "created"), Some("2024-03-04T05:06:00+10:00"));
    let updated = property(&source, "updated").ok_or("the index has an updated stamp")?;
    assert!(
        updated.starts_with("2026-09-28T09:") && updated.ends_with(":00+10:00") && updated.len() == 25,
        "{updated}"
    );
    Ok(())
}
