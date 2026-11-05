//! The text index shared by every `contextos` instance on a vault: its writes keep the index compact, wait a bounded
//! time for another writer rather than failing at once, and its readers see what another instance committed.

mod support;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use contextos_search::{IndexesText, TantivyIndex, TextIndexConfig, TextQuery};
use serde_json::Map;
use support::document;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn index_directory(vault: &tempfile::TempDir) -> PathBuf {
    vault.path().join(".contextos").join("index")
}

fn index_with_wait(
    vault: &tempfile::TempDir,
    writer_lock_wait: Duration,
) -> Result<TantivyIndex, Box<dyn std::error::Error>> {
    Ok(TantivyIndex::try_from(TextIndexConfig {
        directory: index_directory(vault),
        writer_lock_wait,
    })?)
}

fn index_at(vault: &tempfile::TempDir) -> Result<TantivyIndex, Box<dyn std::error::Error>> {
    index_with_wait(vault, TextIndexConfig::DEFAULT_WRITER_LOCK_WAIT)
}

fn hits_for(index: &TantivyIndex, term: &str) -> Result<usize, Box<dyn std::error::Error>> {
    let no_fields = Map::new();
    Ok(index
        .query(&TextQuery {
            query: term,
            path_prefix: None,
            exclude_paths: &[],
            tags: &[],
            fields: &no_fields,
            limit: 200,
        })?
        .len())
}

/// The segments present on disk, counted by their term dictionaries: one `<segment id>.term` file per segment,
/// whether `meta.json` still lists it or not.
fn segment_files(directory: &Path) -> Result<usize, Box<dyn std::error::Error>> {
    let mut count = 0;
    for entry in std::fs::read_dir(directory)? {
        if entry?.path().extension().is_some_and(|extension| extension == "term") {
            count += 1;
        }
    }
    Ok(count)
}

/// Holds tantivy's writer lock on `directory` from another thread, as another process's write would, until told to
/// release it; returns once the lock is held.
fn hold_writer_lock(
    directory: PathBuf,
) -> Result<(mpsc::Sender<()>, std::thread::JoinHandle<()>), Box<dyn std::error::Error>> {
    let (held_tx, held_rx) = mpsc::channel::<Result<(), String>>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let writer = tantivy::Index::open_in_dir(&directory)
            .and_then(|index| index.writer_with_num_threads::<tantivy::TantivyDocument>(1, 15_000_000));
        match writer {
            Ok(writer) => {
                let _ = held_tx.send(Ok(()));
                let _ = release_rx.recv();
                drop(writer);
            }
            Err(error) => {
                let _ = held_tx.send(Err(error.to_string()));
            }
        }
    });
    held_rx.recv()??;
    Ok((release_tx, holder))
}

#[test]
fn repeated_single_note_writes_keep_the_index_compact() -> TestResult {
    let vault = tempfile::tempdir()?;
    let index = index_at(&vault)?;
    for number in 0..40 {
        index.index(&[document(
            &vault,
            &format!("notes/{number}.md"),
            "# Note\n\nGadget prose.\n",
        )?])?;
    }

    assert_eq!(hits_for(&index, "gadget")?, 40);
    let segments = segment_files(&index_directory(&vault))?;
    assert!(
        segments <= 8,
        "40 one-note writes left {segments} segments on disk; merges must complete"
    );
    Ok(())
}

#[test]
fn a_write_waits_for_another_writer_to_release_the_index_rather_than_failing() -> TestResult {
    let vault = tempfile::tempdir()?;
    let index = index_at(&vault)?;
    index.index(&[document(&vault, "notes/first.md", "# First\n\nGadget prose.\n")?])?;
    let (release, holder) = hold_writer_lock(index_directory(&vault))?;

    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        let _ = release.send(());
    });
    index.index(&[document(&vault, "notes/second.md", "# Second\n\nGadget prose.\n")?])?;

    releaser.join().map_err(|_| "releaser thread panicked")?;
    holder.join().map_err(|_| "holder thread panicked")?;
    assert_eq!(hits_for(&index, "gadget")?, 2);
    Ok(())
}

#[test]
fn a_write_still_blocked_when_its_wait_runs_out_fails_as_busy_and_changes_nothing() -> TestResult {
    let vault = tempfile::tempdir()?;
    let index = index_with_wait(&vault, Duration::from_millis(50))?;
    index.index(&[document(&vault, "notes/first.md", "# First\n\nGadget prose.\n")?])?;
    let (release, holder) = hold_writer_lock(index_directory(&vault))?;

    let result = index.index(&[document(&vault, "notes/second.md", "# Second\n\nGadget prose.\n")?]);

    let _ = release.send(());
    holder.join().map_err(|_| "holder thread panicked")?;
    let error = result.err().ok_or("a write blocked past its wait must fail")?;
    assert_eq!(error.code(), "index/busy");
    assert_eq!(hits_for(&index, "gadget")?, 1);
    Ok(())
}

#[test]
fn a_note_another_instance_committed_is_found_by_the_next_query() -> TestResult {
    let vault = tempfile::tempdir()?;
    let reader = index_at(&vault)?;
    assert_eq!(hits_for(&reader, "gadget")?, 0);

    let writer = index_at(&vault)?;
    writer.index(&[document(
        &vault,
        "notes/elsewhere.md",
        "# Elsewhere\n\nGadget prose.\n",
    )?])?;

    assert_eq!(hits_for(&reader, "gadget")?, 1);
    Ok(())
}

/// Writes the files of a segment no commit lists, as a merge dropped part way leaves them, last modified `age` ago.
fn abandoned_segment(directory: &Path, id: &str, age: Duration) -> TestResult {
    let modified = std::time::SystemTime::now()
        .checked_sub(age)
        .ok_or("the fixture's modification time must be representable")?;
    for extension in ["idx", "term", "store", "pos", "fast", "fieldnorm"] {
        let path = directory.join(format!("{id}.{extension}"));
        std::fs::write(&path, b"abandoned")?;
        std::fs::File::options()
            .write(true)
            .open(&path)?
            .set_modified(modified)?;
    }
    Ok(())
}

/// The names of the files in `directory` that belong to segment `id`.
fn files_of(directory: &Path, id: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.starts_with(id) {
            names.push(name);
        }
    }
    Ok(names)
}

/// The id of the one segment `meta.json` lists.
fn only_live_segment(directory: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(directory.join("meta.json"))?)?;
    let segments = meta
        .get("segments")
        .and_then(serde_json::Value::as_array)
        .ok_or("meta.json lists no segments")?;
    let [segment] = segments.as_slice() else {
        return Err(format!("expected one live segment, found {}", segments.len()).into());
    };
    Ok(segment
        .get("segment_id")
        .and_then(serde_json::Value::as_str)
        .ok_or("a segment without an id")?
        .replace('-', ""))
}

#[test]
fn a_write_removes_segment_files_an_abandoned_merge_left_behind_and_keeps_everything_else() -> TestResult {
    let vault = tempfile::tempdir()?;
    let directory = index_directory(&vault);
    {
        let index = index_at(&vault)?;
        index.index(&[document(&vault, "notes/kept.md", "# Kept\n\nGadget prose.\n")?])?;
    }
    let live = only_live_segment(&directory)?;
    let orphan = "0123456789abcdef0123456789abcdef";
    abandoned_segment(&directory, orphan, Duration::from_secs(3_600))?;
    std::fs::write(directory.join("operator-notes.txt"), b"not tantivy's")?;

    {
        let index = index_at(&vault)?;
        index.index(&[document(&vault, "notes/next.md", "# Next\n\nGadget prose.\n")?])?;
    }

    assert_eq!(
        files_of(&directory, orphan)?,
        Vec::<String>::new(),
        "the abandoned segment's files are removed"
    );
    assert!(
        directory.join("operator-notes.txt").exists(),
        "a file tantivy did not create is left alone"
    );
    assert!(
        !files_of(&directory, &live)?.is_empty(),
        "the segment meta.json listed must keep its files"
    );
    // A fresh handle opens every segment from disk, so a deleted live file would show here.
    assert_eq!(hits_for(&index_at(&vault)?, "gadget")?, 2);
    Ok(())
}

#[test]
fn a_recently_written_unlisted_segment_is_kept_in_case_an_older_build_is_still_merging_it() -> TestResult {
    let vault = tempfile::tempdir()?;
    let directory = index_directory(&vault);
    {
        let index = index_at(&vault)?;
        index.index(&[document(&vault, "notes/kept.md", "# Kept\n\nGadget prose.\n")?])?;
    }
    let recent = "fedcba9876543210fedcba9876543210";
    abandoned_segment(&directory, recent, Duration::from_secs(5))?;

    let index = index_at(&vault)?;
    index.index(&[document(&vault, "notes/next.md", "# Next\n\nGadget prose.\n")?])?;

    assert_eq!(
        files_of(&directory, recent)?.len(),
        6,
        "a segment written moments ago is left alone"
    );
    Ok(())
}
