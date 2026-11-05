use super::*;

fn world_path(roots: &VaultSet, raw: &str) -> Result<VaultPath, Box<dyn std::error::Error>> {
    Ok(VaultPath::try_from(VaultPathInput { roots, raw })?)
}

fn one_root() -> Result<(tempfile::TempDir, VaultSet), Box<dyn std::error::Error>> {
    use contextos_core::{VaultRoot, VaultRootInput};
    let directory = tempfile::tempdir()?;
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: directory.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?])?;
    Ok((directory, roots))
}

fn event(kind: OpKind, paths: Vec<VaultPath>) -> OperationEvent {
    OperationEvent {
        kind,
        paths,
        origin: contextos_core::Origin::Tool("fs_delete_file".to_owned()),
        summary: String::new(),
        at: OffsetDateTime::UNIX_EPOCH,
    }
}

fn tasks_scope() -> TaskScope {
    TaskScope::try_from("filters: file.inFolder(\"memory/tasks\")\n").unwrap_or_else(|_| unreachable!())
}

#[test]
fn a_deleted_path_the_base_cannot_reach_needs_no_look_at_the_table() -> Result<(), Box<dyn std::error::Error>> {
    let (_directory, roots) = one_root()?;
    let scope = tasks_scope();
    let events = [
        event(OpKind::Delete, vec![world_path(&roots, "vault://notes/old.md")?]),
        event(
            OpKind::Delete,
            vec![world_path(&roots, "vault://notes/folder.with.dots")?],
        ),
    ];

    assert!(
        Operations::from(TaskBatch {
            scope: &scope,
            events: &events
        })
        .is_empty()
    );
    Ok(())
}

#[test]
fn a_deleted_path_beside_the_tasks_may_have_held_tasks_whatever_its_name_looks_like()
-> Result<(), Box<dyn std::error::Error>> {
    let (_directory, roots) = one_root()?;
    let scope = tasks_scope();
    for raw in [
        "vault://memory/tasks/2026.Q1",
        "vault://memory/tasks/archive",
        "vault://memory",
    ] {
        let events = [event(OpKind::Delete, vec![world_path(&roots, raw)?])];
        let operations = Operations::from(TaskBatch {
            scope: &scope,
            events: &events,
        });
        assert!(!operations.is_empty(), "{raw}");
        assert!(operations.list.is_empty(), "{raw}: a folder is not a note");
    }
    Ok(())
}

#[test]
fn a_deleted_task_note_is_one_removal_and_not_a_folder() -> Result<(), Box<dyn std::error::Error>> {
    let (_directory, roots) = one_root()?;
    let scope = tasks_scope();
    let events = [event(
        OpKind::Delete,
        vec![world_path(&roots, "vault://memory/tasks/a-001.md")?],
    )];

    let operations = Operations::from(TaskBatch {
        scope: &scope,
        events: &events,
    });

    assert_eq!(operations.list.len(), 1);
    assert_eq!(operations.below, Vec::<String>::new());
    Ok(())
}

#[test]
fn a_folder_moved_into_the_tasks_may_bring_tasks_with_it() -> Result<(), Box<dyn std::error::Error>> {
    let (_directory, roots) = one_root()?;
    let scope = tasks_scope();
    let moved = event(
        OpKind::Move,
        vec![
            world_path(&roots, "vault://staging.v2")?,
            world_path(&roots, "vault://memory/tasks/2026.Q1")?,
        ],
    );

    let operations = Operations::from(TaskBatch {
        scope: &scope,
        events: &[moved],
    });

    assert!(!operations.is_empty());
    assert_eq!(operations.arrived.len(), 1);
    assert!(
        operations.below.is_empty(),
        "nothing tracked lies below a folder outside the tasks"
    );
    Ok(())
}

#[test]
fn a_move_between_vaults_is_a_removal_where_it_left_and_a_creation_where_it_arrived()
-> Result<(), Box<dyn std::error::Error>> {
    use contextos_core::{Origin, VaultRoot, VaultRootInput};

    let (first, second) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let root = |directory: &tempfile::TempDir, name: &str| {
        VaultRoot::try_from(VaultRootInput {
            path: directory.path().to_path_buf(),
            managed: true,
            name: Some(name.to_owned()),
        })
    };
    let roots = VaultSet::try_from(vec![root(&first, "first")?, root(&second, "second")?])?;
    let path = |raw: &str| VaultPath::try_from(VaultPathInput { roots: &roots, raw });
    let moved = OperationEvent {
        kind: OpKind::Move,
        paths: vec![path("first://a.md")?, path("second://b.md")?],
        origin: Origin::Tool("fs_move_file".to_owned()),
        summary: "move".to_owned(),
        at: OffsetDateTime::UNIX_EPOCH,
    };

    let left = scoped_to(&moved, VaultRootId::try_from(0_usize)?).ok_or("first vault")?;
    let arrived = scoped_to(&moved, VaultRootId::try_from(1_usize)?).ok_or("second vault")?;

    assert_eq!((left.kind, left.paths.len()), (OpKind::Delete, 1));
    assert_eq!((arrived.kind, arrived.paths.len()), (OpKind::Create, 1));
    Ok(())
}

/// A vault with one task, behind a `VaultEvents` whose state directory is a second temporary directory.
struct SeedWorld {
    vault: tempfile::TempDir,
    state: tempfile::TempDir,
    events: VaultEvents,
}

fn seed_world(managed: bool) -> Result<SeedWorld, Box<dyn std::error::Error>> {
    use contextos_core::{ConfiguredTimeZone, LocalClockInput, OperatorTimeZone, VaultRoot, VaultRootInput};
    use contextos_fs::{
        Filesystem, FilesystemConfig, FilesystemService, FilesystemServiceConfig, FsLimits, default_hidden_patterns,
    };

    let vault = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    std::fs::create_dir_all(vault.path().join("memory/tasks"))?;
    std::fs::write(
        vault.path().join("memory/tasks.base"),
        "filters: file.inFolder(\"memory/tasks\")\n",
    )?;
    std::fs::write(
        vault.path().join("memory/tasks/a-001.md"),
        "---\ntype: task\nid: A-001\nstate: next\n---\n\n# A\n",
    )?;
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed,
        name: Some("vault".to_owned()),
    })?])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![FsLimits {
            max_read_bytes: 1024 * 1024,
            max_batch_files: 50,
        }],
        hidden: vec![default_hidden_patterns()],
        atomic_write_guard: None,
    })?;
    let clock = VaultClock::from(LocalClockInput {
        clock: crate::ServerClock::default(),
        zone: OperatorTimeZone::from(ConfiguredTimeZone::try_from("UTC")?),
    });
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: filesystem.clone(),
        clock: clock.clone(),
    });
    let events = VaultEvents::try_from(VaultEventsConfig {
        roots,
        filesystem,
        writer,
        clock,
        vaults: vec![VaultEventsSetting {
            enabled: managed,
            state_directory: state.path().to_path_buf(),
        }],
    })?;
    Ok(SeedWorld { vault, state, events })
}

#[test]
fn the_task_table_is_seeded_only_while_the_vaults_write_lock_is_held() -> Result<(), Box<dyn std::error::Error>> {
    let world = seed_world(true)?;
    let held = std::cell::Cell::new(false);
    let seeded_while_held = std::cell::Cell::new(false);
    let table = world.state.path().join(TASK_TABLE_FILE);

    world.events.seed_missing_tables(|index| {
        assert_eq!(index, 0);
        held.set(true);
        Ok(Guard {
            held: &held,
            table: &table,
            seeded_while_held: &seeded_while_held,
        })
    });

    assert!(table.exists(), "the table is seeded");
    assert!(
        seeded_while_held.get(),
        "the table existed only once the lock was held, and the lock was released after"
    );
    assert!(!held.get(), "the lock is released when seeding is done");
    drop(world.vault);
    Ok(())
}

/// A lock that notes, as it is released, that the table was written while it was held.
struct Guard<'a> {
    held: &'a std::cell::Cell<bool>,
    table: &'a std::path::Path,
    seeded_while_held: &'a std::cell::Cell<bool>,
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.seeded_while_held.set(self.held.get() && self.table.exists());
        self.held.set(false);
    }
}

#[test]
fn a_vault_whose_write_lock_cannot_be_taken_is_left_for_its_first_locked_batch_to_seed()
-> Result<(), Box<dyn std::error::Error>> {
    let world = seed_world(true)?;

    world.events.seed_missing_tables(|_| {
        Err::<(), _>(crate::tool_error::ToolError::VaultBusy {
            lock: std::path::PathBuf::new(),
            seconds: 0,
        })
    });

    assert!(!world.state.path().join(TASK_TABLE_FILE).exists());
    Ok(())
}

#[test]
fn an_unmanaged_vault_is_never_locked_or_seeded() -> Result<(), Box<dyn std::error::Error>> {
    let world = seed_world(false)?;
    let asked = std::cell::Cell::new(false);

    world.events.seed_missing_tables(|_| {
        asked.set(true);
        Ok(())
    });

    assert!(!asked.get());
    assert!(!world.state.path().join(TASK_TABLE_FILE).exists());
    Ok(())
}
