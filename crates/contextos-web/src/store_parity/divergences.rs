//! Where the host store and a vault's store are documented to answer differently, each pinned by a test of its own so a
//! change to either side shows as a failure here and not as a silent drift. Each pair states the difference once in
//! its doc comment and asserts both sides.

#[cfg(unix)]
use contextos_packages::{DamageFinding, StoreEntryKind};
use contextos_packages::{PackageStore, ReadCheck, StoreFailureKind, StoreIo, UnverifiableReason};

use super::host_world::HostWorld;
#[cfg(unix)]
use super::io_support::link;
use super::io_support::{kind_of, path, put, show};
use super::store_support::{condition_of, install_versions};
use super::vault_world::VaultWorld;
#[cfg(unix)]
use super::worlds::StoreWorld;
use super::worlds::{BoxError, IoWorld, engine_over, key, read};

/// A store holding a link to a folder outside it, a link to a file outside it, and a real file.
#[cfg(unix)]
async fn world_with_links<W: IoWorld>() -> Result<W, BoxError> {
    let world = W::new().await?;
    std::fs::write(world.outside_dir().join("secret"), b"secret")?;
    put(&world.store_dir(), "pkg/1.0.0/file", b"x")?;
    link(&world.outside_dir(), &world.store_dir().join("pkg/1.0.0/escape"))?;
    link(
        &world.outside_dir().join("secret"),
        &world.store_dir().join("pkg/1.0.0/file-link"),
    )?;
    link(&world.outside_dir(), &world.store_dir().join("dirlink"))?;
    Ok(world)
}

#[cfg(unix)]
fn kinds(entries: impl IntoIterator<Item = (String, StoreEntryKind)>) -> Vec<(String, StoreEntryKind)> {
    let mut found: Vec<_> = entries.into_iter().collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// The host reports a link as a link, which the engine reads as damage wherever it stands in a release.
#[cfg(unix)]
#[tokio::test]
async fn the_host_lists_a_link_as_a_link_and_does_not_follow_it() -> Result<(), BoxError> {
    let world = world_with_links::<HostWorld>().await?;
    let io = world.io().await?;

    let listed = io.list(&path("pkg/1.0.0")?).await.map_err(|failure| show(&failure))?;
    let tree = io
        .list_tree(&path("pkg/1.0.0")?, 10)
        .await
        .map_err(|failure| show(&failure))?;

    let expected = vec![
        ("escape".to_owned(), StoreEntryKind::Link),
        ("file".to_owned(), StoreEntryKind::File { size: 1 }),
        ("file-link".to_owned(), StoreEntryKind::Link),
    ];
    assert_eq!(kinds(listed.into_iter().map(|e| (e.name, e.kind))), expected);
    assert_eq!(kinds(tree.into_iter().map(|e| (e.path, e.kind))), expected);
    Ok(())
}

/// A vault's server resolves a link before it answers, so the store reports a link as the file it leads to and never
/// as a [`StoreEntryKind::Link`]; reading the entry is then refused as a link escape.
#[cfg(unix)]
#[tokio::test]
async fn a_vault_lists_a_link_as_the_file_the_server_reports_and_refuses_to_read_it() -> Result<(), BoxError> {
    let world = world_with_links::<VaultWorld>().await?;
    let io = world.io().await?;

    let listed = io.list(&path("pkg/1.0.0")?).await.map_err(|failure| show(&failure))?;
    let tree = io
        .list_tree(&path("pkg/1.0.0")?, 10)
        .await
        .map_err(|failure| show(&failure))?;

    for entries in [
        kinds(listed.into_iter().map(|e| (e.name, e.kind))),
        kinds(tree.into_iter().map(|e| (e.path, e.kind))),
    ] {
        let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["escape", "file", "file-link"]);
        assert!(
            entries
                .iter()
                .all(|(_, kind)| matches!(kind, StoreEntryKind::File { .. })),
            "{entries:?}"
        );
    }
    assert_eq!(
        kind_of(io.read(&path("pkg/1.0.0/file-link")?, 100).await)?,
        StoreFailureKind::Refused
    );
    Ok(())
}

/// The host unlinks a link that stands where a tree is removed, and renames a link as itself, never following it.
#[cfg(unix)]
#[tokio::test]
async fn the_host_removes_and_renames_a_link_itself_without_following_it() -> Result<(), BoxError> {
    let world = world_with_links::<HostWorld>().await?;
    let io = world.io().await?;

    assert!(
        io.remove_tree(&path("dirlink")?)
            .await
            .map_err(|failure| show(&failure))?
    );
    link(&world.outside_dir(), &world.store_dir().join("dirlink"))?;
    io.rename(&path("dirlink")?, &path("moved")?)
        .await
        .map_err(|failure| show(&failure))?;

    assert!(std::fs::symlink_metadata(world.store_dir().join("moved"))?.is_symlink());
    assert!(!world.store_dir().join("dirlink").exists());
    assert_eq!(std::fs::read(world.outside_dir().join("secret"))?, b"secret");
    Ok(())
}

/// A vault's store refuses to remove or rename a link, whether the link is the entry itself or on the way to it, and
/// leaves it in place.
#[cfg(unix)]
#[tokio::test]
async fn a_vault_refuses_to_remove_or_rename_a_link_itself() -> Result<(), BoxError> {
    let world = world_with_links::<VaultWorld>().await?;
    let io = world.io().await?;

    assert_eq!(
        kind_of(io.remove_tree(&path("dirlink")?).await)?,
        StoreFailureKind::Refused
    );
    assert_eq!(
        kind_of(io.rename(&path("dirlink")?, &path("moved")?).await)?,
        StoreFailureKind::Refused
    );

    assert!(std::fs::symlink_metadata(world.store_dir().join("dirlink"))?.is_symlink());
    assert_eq!(std::fs::read(world.outside_dir().join("secret"))?, b"secret");
    Ok(())
}

/// The host lists every entry of its store folder. A vault hides what the vault's hidden patterns name (`.git` and
/// `.contextos`), even inside the store, which sits in `.contextos`.
#[tokio::test]
async fn the_host_lists_an_entry_a_vault_hides_by_its_hidden_patterns() -> Result<(), BoxError> {
    let world = HostWorld::new().await?;
    put(&world.store_dir(), ".git/config", b"x")?;
    put(&world.store_dir(), "demo/file", b"x")?;
    let io = world.io().await?;

    let mut names: Vec<_> = io
        .list(&path("")?)
        .await
        .map_err(|failure| show(&failure))?
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    names.sort();

    assert_eq!(names, [".git", "demo"]);
    Ok(())
}

#[tokio::test]
async fn a_vault_does_not_list_an_entry_its_hidden_patterns_name_though_it_is_on_disk() -> Result<(), BoxError> {
    let world = VaultWorld::new().await?;
    put(&world.store_dir(), ".git/config", b"x")?;
    put(&world.store_dir(), "demo/file", b"x")?;
    let io = world.io().await?;

    let names: Vec<_> = io
        .list(&path("")?)
        .await
        .map_err(|failure| show(&failure))?
        .into_iter()
        .map(|entry| entry.name)
        .collect();

    assert_eq!(names, ["demo"]);
    assert!(world.store_dir().join(".git/config").exists());
    Ok(())
}

/// The host store is a folder on the local disk, so it has no outage: the engine's unverifiable-as-unavailable reading
/// is reachable only through a vault, whose reads go through a session to the primary server.
#[tokio::test]
async fn the_host_store_has_no_outage_to_report() -> Result<(), BoxError> {
    let world = HostWorld::new().await?;

    assert!(world.unreachable_io().await?.is_none());
    Ok(())
}

/// A vault's reads, all of them, fail as unavailable when the server cannot answer, and never as a missing entry.
#[tokio::test]
async fn a_vault_whose_server_cannot_answer_reports_every_read_as_unavailable_never_as_missing() -> Result<(), BoxError>
{
    let world = VaultWorld::new().await?;
    let io = world.unreachable_io().await?.ok_or("a vault can be unreachable")?;

    assert_eq!(kind_of(io.list(&path("")?).await)?, StoreFailureKind::Unavailable);
    assert_eq!(
        kind_of(io.list_tree(&path("demo")?, 10).await)?,
        StoreFailureKind::Unavailable
    );
    assert_eq!(
        kind_of(io.read(&path("demo/file.txt")?, 10).await)?,
        StoreFailureKind::Unavailable
    );
    Ok(())
}

/// A store folder that is a regular file is refused by the host with its own kind on a listing, and by a vault's
/// server as a failure; both refuse the store lock.
#[tokio::test]
async fn a_store_folder_that_is_a_file_is_refused_by_the_lock_in_both_and_by_a_listing_with_each_stores_own_kind()
-> Result<(), BoxError> {
    let host = HostWorld::new().await?;
    let vault = VaultWorld::new().await?;
    for world_store in [host.store_dir(), vault.store_dir()] {
        if let Some(parent) = world_store.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(world_store, b"a file")?;
    }
    let (host_io, vault_io) = (host.io().await?, vault.io().await?);

    assert_eq!(kind_of(host_io.lock().await)?, StoreFailureKind::Refused);
    assert_eq!(kind_of(vault_io.lock().await)?, StoreFailureKind::Refused);
    assert_eq!(kind_of(host_io.list(&path("")?).await)?, StoreFailureKind::Refused);
    assert_eq!(kind_of(vault_io.list(&path("")?).await)?, StoreFailureKind::Failed);
    Ok(())
}

/// A vault's writes go through the vault's own write path, which does not take the server's write lock, so a holder of
/// that lock does not stall the store.
#[tokio::test]
async fn a_vault_write_does_not_wait_for_the_write_lock_the_server_takes_for_its_own_writes() -> Result<(), BoxError> {
    let world = VaultWorld::new().await?;
    put(&world.store_dir(), "demo/placeholder", b"")?;
    let io = world.io().await?;
    let _held = contextos_fs::lock_vault_writes(&world.server_state_dir(), std::time::Duration::ZERO)?;

    io.write_new(&path("demo/data.txt")?, b"x")
        .await
        .map_err(|failure| show(&failure))?;
    io.rename(&path("demo")?, &path("moved")?)
        .await
        .map_err(|failure| show(&failure))?;
    assert!(
        io.remove_tree(&path("moved")?)
            .await
            .map_err(|failure| show(&failure))?
    );

    assert!(!world.store_dir().join("moved").exists());
    Ok(())
}

/// A release folder holding a link, put there by hand: one in a folder of a release, one in place of a listed file, and
/// one in place of an unlisted entry.
#[cfg(unix)]
async fn releases_holding_links<W: StoreWorld>() -> Result<W, BoxError> {
    let world = W::new().await?;
    install_versions(&world, &["1.0.0", "1.1.0", "1.2.0"]).await?;
    std::fs::write(world.outside_dir().join("secret"), b"secret")?;
    link(&world.outside_dir(), &world.store_dir().join("crm/1.0.0/escape"))?;
    let listed = world.store_dir().join("crm/1.1.0/sync/sync.rhai");
    std::fs::remove_file(&listed)?;
    link(&world.outside_dir().join("secret"), &listed)?;
    link(
        &world.outside_dir().join("secret"),
        &world.store_dir().join("crm/1.2.0/sync/file-link"),
    )?;
    Ok(world)
}

/// The host sees a link as a link, so one anywhere in a release, in place of a listed file or not, is damage.
#[cfg(unix)]
#[tokio::test]
async fn the_host_reads_a_release_holding_a_link_anywhere_as_damaged() -> Result<(), BoxError> {
    let world = releases_holding_links::<HostWorld>().await?;

    for version in ["1.0.0", "1.1.0", "1.2.0"] {
        let check = read(&world, version).await?;
        assert!(
            matches!(&check, ReadCheck::Damaged(found) if found.iter().any(|f| matches!(f, DamageFinding::LinkInRelease(_)))),
            "{version}: {}",
            condition_of(&check)
        );
    }
    assert_eq!(std::fs::read(world.outside_dir().join("secret"))?, b"secret");
    Ok(())
}

/// A vault's server reports a link as the entry it leads to, so a link the manifest does not list reads as an unlisted
/// entry (the release is incomplete), and one standing in place of a listed file cannot be read through (the release is
/// unverifiable, where the host reads it as damaged). A vault's store never follows the link either way.
#[cfg(unix)]
#[tokio::test]
async fn a_vault_reads_a_release_holding_an_unlisted_link_as_incomplete_and_never_follows_it() -> Result<(), BoxError> {
    let world = releases_holding_links::<VaultWorld>().await?;

    let folder_link = read(&world, "1.0.0").await?;
    let replaced_file = read(&world, "1.1.0").await?;
    let unlisted_file_link = read(&world, "1.2.0").await?;

    assert_eq!(condition_of(&folder_link), "incomplete");
    assert_eq!(condition_of(&unlisted_file_link), "incomplete");
    assert_eq!(condition_of(&replaced_file), "unverifiable");
    assert_eq!(std::fs::read(world.outside_dir().join("secret"))?, b"secret");
    Ok(())
}

/// Only a vault can be unreachable, and a release it cannot read through is then unverifiable, never removed.
#[tokio::test]
async fn a_release_in_a_vault_whose_server_cannot_answer_reads_as_unverifiable_never_as_removed() -> Result<(), BoxError>
{
    let world = VaultWorld::new().await?;
    install_versions(&world, &["1.0.0"]).await?;
    let unreachable = engine_over(world.unreachable_io().await?.ok_or("a vault can be unreachable")?);

    let check = unreachable.read_release(&key("1.0.0")?).await;

    assert!(
        matches!(&check, ReadCheck::Unverifiable(UnverifiableReason::StoreUnavailable(failure)) if failure.kind == StoreFailureKind::Unavailable),
        "{}",
        condition_of(&check)
    );
    assert_eq!(condition_of(&read(&world, "1.0.0").await?), "verified");
    Ok(())
}
