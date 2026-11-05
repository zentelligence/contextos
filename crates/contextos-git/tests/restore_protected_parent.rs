//! A restore that would delete a folder holding a protected path leaves the folder, and the protected path, alone.
use contextos_core::{
    MoveMutation, NoEvents, NoProjection, NoSearchUpdates, OperationRouter, OperationRouterConfig, Origin, VaultRoot,
    VaultRootInput, VaultSet,
};
use contextos_fs::{FilesystemService, FilesystemServiceConfig, RoutedFilesystemServiceConfig};
use contextos_git::{Git2Vault, Git2VaultConfig, GitRestoreRequest};
use contextos_index::{IndexService, IndexServiceConfig};
use contextos_oplog::{OperationLog, OperationLogConfig};
use tempfile::tempdir;

mod support;

#[test]
fn a_restore_does_not_delete_a_parent_folder_that_holds_a_protected_path() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("staging"))?;
    std::fs::write(vault.path().join("staging/a.md"), "A\n")?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![root.clone()])?;
    let filesystem = support::filesystem(&root)?;
    let plain_writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: filesystem.clone(),
        clock: support::FixedClock,
    });
    let indexes = IndexService::try_from(IndexServiceConfig {
        root: root.clone(),
        roots: roots.clone(),
        reader: filesystem.clone(),
        writer: plain_writer.clone(),
        excluded: vec![".git".to_owned(), ".contextos".to_owned(), "memory".to_owned()],
        clock: support::FixedClock,
        creation: contextos_core::NoCreationReports,
    })?;
    let operation_log = OperationLog::try_from(OperationLogConfig {
        root: root.clone(),
        roots: roots.clone(),
        relative_directory: "ops".to_owned(),
        appender: plain_writer.clone(),
    })?;
    let git = Git2Vault::try_from(Git2VaultConfig {
        root: root.clone(),
        roots,
        clock: support::FixedClock,
        author_name: "Context OS MCP".to_owned(),
        author_email: "mcp@contextos.local".to_owned(),
        allow_destructive_restore: true,
        protected_restore_paths: vec!["memory/protected".into()],
    })?;
    let baseline = git
        .initialise(&plain_writer)?
        .commit_id
        .ok_or("initial commit missing")?;
    let routed_writer = FilesystemService::from(RoutedFilesystemServiceConfig {
        filesystem,
        clock: support::FixedClock,
        services: OperationRouter::from(OperationRouterConfig {
            indexes,
            operation_log,
            versions: git.clone(),
            search: NoSearchUpdates,
            projection: NoProjection,
            events: NoEvents,
        }),
    });
    routed_writer.move_file(&MoveMutation {
        source: support::vault_path(&root, "staging")?,
        destination: support::vault_path(&root, "memory")?,
        origin: Origin::Tool("fs_move_file".to_owned()),
    })?;
    std::fs::create_dir_all(vault.path().join("memory/protected"))?;
    std::fs::write(vault.path().join("memory/protected/keep.md"), "keep\n")?;

    git.restore(
        &GitRestoreRequest {
            path: support::vault_path(&root, ".")?,
            reference: baseline,
            dry_run: false,
        },
        &routed_writer,
    )?;

    assert_eq!(
        std::fs::read_to_string(vault.path().join("memory/protected/keep.md"))?,
        "keep\n"
    );
    Ok(())
}
