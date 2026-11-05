//! Store I/O cases, written once against [`IoWorld`] and run for the host store and a vault's store alike.

use std::time::Duration;

use contextos_packages::{StoreEntry, StoreEntryKind, StoreFailureKind, StoreIo, StorePath};

use super::io_support::{kind_of, path, put, show};
use super::worlds::{BoxError, IoWorld};

pub(super) async fn a_listing_reports_files_with_their_sizes_and_folders<W: IoWorld>() -> Result<(), BoxError> {
    let world = W::new().await?;
    put(&world.store_dir(), "demo/1.0.0/manifest.toml", b"12345")?;
    put(&world.store_dir(), "ledger.toml", b"ab")?;
    let io = world.io().await?;

    let mut top = io.list(&StorePath::root()).await.map_err(|failure| show(&failure))?;
    top.sort_by(|a, b| a.name.cmp(&b.name));

    assert_eq!(
        top,
        vec![
            StoreEntry {
                name: "demo".to_owned(),
                kind: StoreEntryKind::Directory
            },
            StoreEntry {
                name: "ledger.toml".to_owned(),
                kind: StoreEntryKind::File { size: 2 }
            },
        ]
    );
    let mut below = io.list(&path("demo")?).await.map_err(|failure| show(&failure))?;
    below.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(
        below,
        vec![StoreEntry {
            name: "1.0.0".to_owned(),
            kind: StoreEntryKind::Directory
        }]
    );
    Ok(())
}

pub(super) async fn a_listing_of_a_store_or_folder_that_does_not_exist_is_not_found<W: IoWorld>() -> Result<(), BoxError>
{
    let world = W::new().await?;
    let io = world.io().await?;

    assert_eq!(kind_of(io.list(&StorePath::root()).await)?, StoreFailureKind::NotFound);
    assert_eq!(kind_of(io.list(&path("demo")?).await)?, StoreFailureKind::NotFound);
    assert_eq!(
        kind_of(io.list_tree(&StorePath::root(), 10).await)?,
        StoreFailureKind::NotFound
    );

    std::fs::create_dir_all(world.store_dir())?;
    assert_eq!(kind_of(io.list(&path("demo")?).await)?, StoreFailureKind::NotFound);
    assert_eq!(
        kind_of(io.list_tree(&path("demo")?, 10).await)?,
        StoreFailureKind::NotFound
    );
    Ok(())
}

pub(super) async fn a_tree_listing_reports_every_entry_below_with_paths_relative_to_the_folder<W: IoWorld>()
-> Result<(), BoxError> {
    let world = W::new().await?;
    put(&world.store_dir(), "demo/1.0.0/manifest.toml", b"m")?;
    put(&world.store_dir(), "demo/1.0.0/scripts/main.rhai", b"abc")?;
    let io = world.io().await?;

    let mut tree = io
        .list_tree(&path("demo")?, 100)
        .await
        .map_err(|failure| show(&failure))?;
    tree.sort_by(|a, b| a.path.cmp(&b.path));

    let seen: Vec<(&str, StoreEntryKind)> = tree.iter().map(|entry| (entry.path.as_str(), entry.kind)).collect();
    assert_eq!(
        seen,
        vec![
            ("1.0.0", StoreEntryKind::Directory),
            ("1.0.0/manifest.toml", StoreEntryKind::File { size: 1 }),
            ("1.0.0/scripts", StoreEntryKind::Directory),
            ("1.0.0/scripts/main.rhai", StoreEntryKind::File { size: 3 }),
        ]
    );
    Ok(())
}

pub(super) async fn a_tree_listing_stops_rather_than_return_more_than_the_limit<W: IoWorld>() -> Result<(), BoxError> {
    let world = W::new().await?;
    for name in ["a", "b", "c"] {
        put(&world.store_dir(), &format!("demo/{name}.txt"), b"x")?;
    }
    let io = world.io().await?;

    assert_eq!(
        kind_of(io.list_tree(&path("demo")?, 2).await)?,
        StoreFailureKind::TooLarge
    );
    assert_eq!(
        io.list_tree(&path("demo")?, 3)
            .await
            .map_err(|failure| show(&failure))?
            .len(),
        3
    );
    Ok(())
}

pub(super) async fn a_read_returns_the_exact_bytes_of_text_and_of_binary_files<W: IoWorld>() -> Result<(), BoxError> {
    let world = W::new().await?;
    put(&world.store_dir(), "demo/text.txt", b"line one\r\nline two\r\n")?;
    put(&world.store_dir(), "demo/blob.bin", &[0xff, 0x00, 0xfe, 0x0d])?;
    put(&world.store_dir(), "demo/empty", b"")?;
    let io = world.io().await?;

    assert_eq!(
        io.read(&path("demo/text.txt")?, 1024)
            .await
            .map_err(|failure| show(&failure))?,
        b"line one\r\nline two\r\n"
    );
    assert_eq!(
        io.read(&path("demo/blob.bin")?, 1024)
            .await
            .map_err(|failure| show(&failure))?,
        vec![0xff, 0x00, 0xfe, 0x0d]
    );
    assert_eq!(
        io.read(&path("demo/empty")?, 1024)
            .await
            .map_err(|failure| show(&failure))?,
        Vec::<u8>::new()
    );
    Ok(())
}

pub(super) async fn a_read_past_the_limit_is_refused_before_the_content_is_returned<W: IoWorld>() -> Result<(), BoxError>
{
    let world = W::new().await?;
    put(&world.store_dir(), "demo/big.txt", &[b'x'; 64])?;
    let io = world.io().await?;

    assert_eq!(
        kind_of(io.read(&path("demo/big.txt")?, 63).await)?,
        StoreFailureKind::TooLarge
    );
    assert_eq!(
        io.read(&path("demo/big.txt")?, 64)
            .await
            .map_err(|failure| show(&failure))?
            .len(),
        64
    );
    Ok(())
}

pub(super) async fn a_read_of_a_folder_is_refused_and_of_a_missing_file_is_not_found<W: IoWorld>()
-> Result<(), BoxError> {
    let world = W::new().await?;
    put(&world.store_dir(), "demo/file.txt", b"x")?;
    let io = world.io().await?;

    assert_eq!(kind_of(io.read(&path("demo")?, 1024).await)?, StoreFailureKind::Refused);
    assert_eq!(
        kind_of(io.read(&path("demo/absent.txt")?, 1024).await)?,
        StoreFailureKind::NotFound
    );
    assert_eq!(
        kind_of(io.read(&path("absent/file.txt")?, 1024).await)?,
        StoreFailureKind::NotFound
    );
    Ok(())
}

pub(super) async fn a_new_file_is_written_with_exactly_the_bytes_given_and_leaves_no_temporary_file<W: IoWorld>()
-> Result<(), BoxError> {
    let world = W::new().await?;
    put(&world.store_dir(), "demo/placeholder", b"")?;
    let io = world.io().await?;
    let bytes = [0x00, 0x0d, 0x0a, 0xff, b'\r'];

    io.write_new(&path("demo/data.bin")?, &bytes)
        .await
        .map_err(|failure| show(&failure))?;

    assert_eq!(std::fs::read(world.store_dir().join("demo/data.bin"))?, bytes);
    let mut names: Vec<String> = std::fs::read_dir(world.store_dir().join("demo"))?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()?;
    names.sort();
    assert_eq!(
        names,
        ["data.bin", "placeholder"],
        "no temporary file is left beside the new one"
    );
    Ok(())
}

pub(super) async fn a_new_file_never_replaces_an_existing_entry<W: IoWorld>() -> Result<(), BoxError> {
    let world = W::new().await?;
    put(&world.store_dir(), "demo/data.txt", b"original")?;
    let io = world.io().await?;

    assert_eq!(
        kind_of(io.write_new(&path("demo/data.txt")?, b"other").await)?,
        StoreFailureKind::AlreadyExists
    );
    assert_eq!(
        kind_of(io.write_new(&path("demo")?, b"other").await)?,
        StoreFailureKind::AlreadyExists
    );
    assert_eq!(std::fs::read(world.store_dir().join("demo/data.txt"))?, b"original");
    Ok(())
}

pub(super) async fn a_new_file_in_a_folder_that_does_not_exist_is_not_found_and_creates_nothing<W: IoWorld>()
-> Result<(), BoxError> {
    let world = W::new().await?;
    let io = world.io().await?;

    assert_eq!(
        kind_of(io.write_new(&path("demo/data.txt")?, b"x").await)?,
        StoreFailureKind::NotFound
    );
    assert!(!world.store_dir().join("demo").exists());
    std::fs::create_dir_all(world.store_dir())?;
    assert_eq!(
        kind_of(io.write_new(&path("demo/data.txt")?, b"x").await)?,
        StoreFailureKind::NotFound
    );
    assert!(!world.store_dir().join("demo").exists());
    Ok(())
}

pub(super) async fn replace_creates_an_absent_file_and_swaps_a_present_one_whole<W: IoWorld>() -> Result<(), BoxError> {
    let world = W::new().await?;
    std::fs::create_dir_all(world.store_dir())?;
    let io = world.io().await?;

    io.replace(&StorePath::ledger_file(), b"first")
        .await
        .map_err(|failure| show(&failure))?;
    assert_eq!(std::fs::read(world.store_dir().join("ledger.toml"))?, b"first");
    io.replace(&StorePath::ledger_file(), b"second, longer")
        .await
        .map_err(|failure| show(&failure))?;
    assert_eq!(std::fs::read(world.store_dir().join("ledger.toml"))?, b"second, longer");
    assert_eq!(
        super::worlds::top_level(&world.store_dir()),
        ["ledger.toml"],
        "no temporary file is left beside the replaced one"
    );
    Ok(())
}

pub(super) async fn replace_creates_the_store_folder_when_the_store_does_not_exist_yet<W: IoWorld>()
-> Result<(), BoxError> {
    let world = W::new().await?;
    let io = world.io().await?;

    io.replace(&StorePath::ledger_file(), b"first")
        .await
        .map_err(|failure| show(&failure))?;

    assert_eq!(std::fs::read(world.store_dir().join("ledger.toml"))?, b"first");
    Ok(())
}

pub(super) async fn creating_folders_makes_the_missing_ones_and_accepts_an_existing_one<W: IoWorld>()
-> Result<(), BoxError> {
    let world = W::new().await?;
    let io = world.io().await?;

    io.create_dir_all(&path("demo/1.0.0/scripts")?)
        .await
        .map_err(|failure| show(&failure))?;
    io.create_dir_all(&path("demo/1.0.0/scripts")?)
        .await
        .map_err(|failure| show(&failure))?;

    assert!(world.store_dir().join("demo/1.0.0/scripts").is_dir());
    Ok(())
}

pub(super) async fn creating_a_folder_where_a_file_stands_is_refused_as_already_existing<W: IoWorld>()
-> Result<(), BoxError> {
    let world = W::new().await?;
    put(&world.store_dir(), "demo/file", b"x")?;
    let io = world.io().await?;

    assert_eq!(
        kind_of(io.create_dir_all(&path("demo/file")?).await)?,
        StoreFailureKind::AlreadyExists
    );
    assert_eq!(
        kind_of(io.create_dir_all(&path("demo/file/below")?).await)?,
        StoreFailureKind::AlreadyExists
    );
    assert_eq!(std::fs::read(world.store_dir().join("demo/file"))?, b"x");
    Ok(())
}

pub(super) async fn a_whole_folder_is_renamed_with_its_contents_in_one_step<W: IoWorld>() -> Result<(), BoxError> {
    let world = W::new().await?;
    put(&world.store_dir(), "_staging-a1/manifest.toml", b"m")?;
    put(&world.store_dir(), "_staging-a1/scripts/main.rhai", b"s")?;
    put(&world.store_dir(), "demo/placeholder", b"")?;
    let io = world.io().await?;

    io.rename(&path("_staging-a1")?, &path("demo/1.0.0")?)
        .await
        .map_err(|failure| show(&failure))?;

    assert!(!world.store_dir().join("_staging-a1").exists());
    assert_eq!(std::fs::read(world.store_dir().join("demo/1.0.0/manifest.toml"))?, b"m");
    assert_eq!(
        std::fs::read(world.store_dir().join("demo/1.0.0/scripts/main.rhai"))?,
        b"s"
    );
    Ok(())
}

pub(super) async fn a_rename_never_replaces_the_destination_and_leaves_the_source_in_place<W: IoWorld>()
-> Result<(), BoxError> {
    let world = W::new().await?;
    put(&world.store_dir(), "_staging-a1/manifest.toml", b"new")?;
    put(&world.store_dir(), "demo/1.0.0/manifest.toml", b"installed")?;
    std::fs::create_dir_all(world.store_dir().join("empty"))?;
    let io = world.io().await?;

    assert_eq!(
        kind_of(io.rename(&path("_staging-a1")?, &path("demo/1.0.0")?).await)?,
        StoreFailureKind::AlreadyExists
    );
    assert_eq!(
        kind_of(io.rename(&path("_staging-a1")?, &path("empty")?).await)?,
        StoreFailureKind::AlreadyExists
    );
    assert_eq!(
        std::fs::read(world.store_dir().join("demo/1.0.0/manifest.toml"))?,
        b"installed"
    );
    assert_eq!(
        std::fs::read(world.store_dir().join("_staging-a1/manifest.toml"))?,
        b"new"
    );
    assert!(world.store_dir().join("empty").is_dir());
    Ok(())
}

pub(super) async fn renaming_something_that_is_not_there_is_not_found<W: IoWorld>() -> Result<(), BoxError> {
    let world = W::new().await?;
    std::fs::create_dir_all(world.store_dir())?;
    let io = world.io().await?;

    assert_eq!(
        kind_of(io.rename(&path("_staging-a1")?, &path("demo")?).await)?,
        StoreFailureKind::NotFound
    );
    Ok(())
}

pub(super) async fn removing_a_tree_deletes_it_and_everything_below_and_reports_whether_it_was_there<W: IoWorld>()
-> Result<(), BoxError> {
    let world = W::new().await?;
    put(&world.store_dir(), "_removing-b2/manifest.toml", b"m")?;
    put(&world.store_dir(), "_removing-b2/scripts/main.rhai", b"s")?;
    put(&world.store_dir(), "keep/file", b"k")?;
    let io = world.io().await?;

    assert!(
        io.remove_tree(&path("_removing-b2")?)
            .await
            .map_err(|failure| show(&failure))?
    );
    assert!(
        !io.remove_tree(&path("_removing-b2")?)
            .await
            .map_err(|failure| show(&failure))?
    );

    assert!(!world.store_dir().join("_removing-b2").exists());
    assert_eq!(std::fs::read(world.store_dir().join("keep/file"))?, b"k");
    Ok(())
}

pub(super) async fn removing_a_tree_that_is_not_there_reports_nothing_removed_even_below_a_missing_folder<
    W: IoWorld,
>() -> Result<(), BoxError> {
    let world = W::new().await?;
    std::fs::create_dir_all(world.store_dir())?;
    let io = world.io().await?;

    assert!(
        !io.remove_tree(&path("absent/1.0.0")?)
            .await
            .map_err(|failure| show(&failure))?
    );
    Ok(())
}

pub(super) async fn the_store_lock_is_exclusive_released_when_dropped_and_is_the_lock_file_inside_the_store<
    W: IoWorld,
>() -> Result<(), BoxError> {
    let world = W::new().await?;
    let io = world.io().await?;

    let held = io.lock().await.map_err(|failure| show(&failure))?;
    assert!(world.store_dir().join(".lock").is_file());
    assert_eq!(kind_of(io.lock().await)?, StoreFailureKind::LockBusy);
    drop(held);

    drop(io.lock().await.map_err(|failure| show(&failure))?);
    Ok(())
}

pub(super) async fn two_handles_on_one_store_contend_for_the_one_lock_and_so_does_a_holder_outside_the_adapter<
    W: IoWorld,
>() -> Result<(), BoxError> {
    let world = W::new().await?;
    let first = world.io().await?;
    let second = world.io().await?;

    let held = first.lock().await.map_err(|failure| show(&failure))?;
    assert_eq!(kind_of(second.lock().await)?, StoreFailureKind::LockBusy);
    drop(held);
    drop(second.lock().await.map_err(|failure| show(&failure))?);

    let elsewhere = contextos_fs::lock_host(&world.store_dir().join(".lock"), Duration::ZERO)?;
    assert_eq!(kind_of(first.lock().await)?, StoreFailureKind::LockBusy);
    drop(elsewhere);
    drop(first.lock().await.map_err(|failure| show(&failure))?);
    Ok(())
}

pub(super) async fn the_store_lock_does_not_gate_the_changes_the_lock_holder_makes<W: IoWorld>() -> Result<(), BoxError>
{
    let world = W::new().await?;
    put(&world.store_dir(), "demo/placeholder", b"")?;
    let io = world.io().await?;
    let held = io.lock().await.map_err(|failure| show(&failure))?;

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

    drop(held);
    assert!(!world.store_dir().join("moved").exists());
    Ok(())
}
