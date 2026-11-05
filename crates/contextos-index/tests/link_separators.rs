//! Generated index links are vault-relative wikilinks written with forward slashes at every nesting depth,
//! whichever platform performs the rebuild: a backslash in a link target does not resolve in Obsidian or in
//! the server's own link graph.

use contextos_core::{NoSubstrateServices, Origin, VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet};
use contextos_fs::{
    Filesystem, FilesystemConfig, FilesystemService, FilesystemServiceConfig, FsLimits, default_hidden_patterns,
};
use contextos_index::{IndexService, IndexServiceConfig};
use tempfile::tempdir;
use time::OffsetDateTime;
use time::macros::datetime;

#[derive(Clone, Copy, Debug)]
struct FixedClock;

impl contextos_core::Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        datetime!(2026-09-28 09:00:00 +10:00)
    }
}

type BoxError = Box<dyn std::error::Error>;

fn rebuild_vault(vault: &std::path::Path, subtree: &str) -> Result<(), BoxError> {
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
        creation: contextos_core::NoCreationReports,
    })?;
    let directory = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: subtree,
    })?;
    service.rebuild(
        &directory,
        Origin::Tool("vault_index_rebuild".to_owned()),
        &NoSubstrateServices,
    )?;
    Ok(())
}

#[test]
fn a_directory_row_at_every_depth_links_with_forward_slashes_only() -> Result<(), BoxError> {
    let vault = tempdir()?;
    std::fs::create_dir_all(vault.path().join("memory/wiki/analytics/deep"))?;
    std::fs::write(vault.path().join("memory/wiki/analytics/deep/leaf.md"), "# Leaf\n")?;

    rebuild_vault(vault.path(), "memory")?;

    let memory = std::fs::read_to_string(vault.path().join("memory/index.md"))?;
    let wiki = std::fs::read_to_string(vault.path().join("memory/wiki/index.md"))?;
    let analytics = std::fs::read_to_string(vault.path().join("memory/wiki/analytics/index.md"))?;
    let deep = std::fs::read_to_string(vault.path().join("memory/wiki/analytics/deep/index.md"))?;
    assert!(memory.contains("[[memory/wiki/index.md|Wiki]]"), "{memory}");
    assert!(wiki.contains("[[memory/wiki/analytics/index.md|Analytics]]"), "{wiki}");
    assert!(
        analytics.contains("[[memory/wiki/analytics/deep/index.md|Deep]]"),
        "{analytics}"
    );
    assert!(deep.contains("[[memory/wiki/analytics/deep/leaf.md|Leaf]]"), "{deep}");
    for (name, index) in [
        ("memory", &memory),
        ("wiki", &wiki),
        ("analytics", &analytics),
        ("deep", &deep),
    ] {
        assert!(!index.contains('\\'), "{name} index contains a backslash: {index}");
    }
    Ok(())
}
