//! Store I/O cases about links, written once against [`IoWorld`]. A link in a store is a security boundary: nothing is
//! read, written, renamed, or listed through one, and deleting a tree never reaches what a link inside it points at.
//! Where the two stores answer a link differently, the difference is pinned in `divergences` instead.

use contextos_packages::{StoreFailureKind, StoreIo};

use super::io_support::{kind_of, link, path, put, show};
use super::worlds::{BoxError, IoWorld};

/// A store holding `dirlink`, a link to a folder outside it that holds `secret`.
async fn world_with_a_link_to_a_folder_outside_the_store<W: IoWorld>() -> Result<W, BoxError> {
    let world = W::new().await?;
    std::fs::write(world.outside_dir().join("secret"), b"secret")?;
    std::fs::create_dir_all(world.store_dir())?;
    link(&world.outside_dir(), &world.store_dir().join("dirlink"))?;
    Ok(world)
}

pub(super) async fn nothing_is_read_written_renamed_removed_or_created_through_a_link_on_the_way<W: IoWorld>()
-> Result<(), BoxError> {
    let world = world_with_a_link_to_a_folder_outside_the_store::<W>().await?;
    let io = world.io().await?;

    assert_eq!(
        kind_of(io.read(&path("dirlink/secret")?, 100).await)?,
        StoreFailureKind::Refused
    );
    assert_eq!(kind_of(io.list(&path("dirlink")?).await)?, StoreFailureKind::Refused);
    assert_eq!(
        kind_of(io.write_new(&path("dirlink/new")?, b"x").await)?,
        StoreFailureKind::Refused
    );
    assert_eq!(
        kind_of(io.replace(&path("dirlink/new")?, b"x").await)?,
        StoreFailureKind::Refused
    );
    assert_eq!(
        kind_of(io.create_dir_all(&path("dirlink/sub")?).await)?,
        StoreFailureKind::Refused
    );
    assert_eq!(
        kind_of(io.rename(&path("dirlink/secret")?, &path("moved")?).await)?,
        StoreFailureKind::Refused
    );
    assert_eq!(
        kind_of(io.remove_tree(&path("dirlink/secret")?).await)?,
        StoreFailureKind::Refused
    );

    assert_eq!(std::fs::read(world.outside_dir().join("secret"))?, b"secret");
    let mut outside: Vec<_> = std::fs::read_dir(world.outside_dir())?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<_, _>>()?;
    outside.sort();
    assert_eq!(outside, ["secret"], "nothing was created outside the store");
    assert!(!world.store_dir().join("moved").exists());
    Ok(())
}

pub(super) async fn a_file_that_is_a_link_is_neither_read_nor_replaced<W: IoWorld>() -> Result<(), BoxError> {
    let world = W::new().await?;
    let target = world.outside_dir().join("secret");
    std::fs::write(&target, b"secret")?;
    std::fs::create_dir_all(world.store_dir())?;
    link(&target, &world.store_dir().join("ledger.toml"))?;
    let io = world.io().await?;

    assert_eq!(
        kind_of(io.read(&path("ledger.toml")?, 100).await)?,
        StoreFailureKind::Refused
    );
    assert_eq!(
        kind_of(io.replace(&path("ledger.toml")?, b"x").await)?,
        StoreFailureKind::Refused
    );

    assert_eq!(std::fs::read(&target)?, b"secret");
    assert!(std::fs::symlink_metadata(world.store_dir().join("ledger.toml"))?.is_symlink());
    Ok(())
}

pub(super) async fn a_link_inside_a_tree_being_removed_is_deleted_without_touching_what_it_points_to<W: IoWorld>()
-> Result<(), BoxError> {
    let world = W::new().await?;
    std::fs::write(world.outside_dir().join("keep"), b"keep")?;
    put(&world.store_dir(), "pkg/1.0.0/file", b"x")?;
    link(&world.outside_dir(), &world.store_dir().join("pkg/1.0.0/escape"))?;
    link(
        &world.outside_dir().join("keep"),
        &world.store_dir().join("pkg/1.0.0/file-link"),
    )?;
    let io = world.io().await?;

    assert!(
        io.remove_tree(&path("pkg/1.0.0")?)
            .await
            .map_err(|failure| show(&failure))?
    );

    assert!(!world.store_dir().join("pkg/1.0.0").exists());
    assert_eq!(std::fs::read(world.outside_dir().join("keep"))?, b"keep");
    Ok(())
}
