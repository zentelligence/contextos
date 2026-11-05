//! The link graph's listing of the vault's files stays right when the graph is opened from a store that already holds
//! links, when the file a change names is already in the listing, and when another process shares the store.

use std::path::PathBuf;

use contextos_obsidian::LinkCollection;
use contextos_search::{FileEntry, GraphBackend, GraphNodeKind, LinkGraph, LinkGraphConfig, NoteEntry, VaultScan};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BACKENDS: [GraphBackend; 3] = [GraphBackend::Serde, GraphBackend::Fjall, GraphBackend::Sqlite];

fn open(dir: &tempfile::TempDir, backend: GraphBackend) -> Result<LinkGraph, Box<dyn std::error::Error>> {
    Ok(LinkGraph::try_from(LinkGraphConfig {
        store_directory: PathBuf::from(dir.path()).join(".contextos").join("graph"),
        backend,
        write_wait: contextos_search::DEFAULT_WRITE_WAIT,
    })?)
}

fn note(graph: &mut LinkGraph, path: &str, source: &str) -> TestResult {
    let links = LinkCollection::try_from(source)?;
    graph.upsert_note(path, path, links.outgoing())?;
    Ok(())
}

fn kind_of(graph: &mut LinkGraph, path: &str) -> Result<Option<GraphNodeKind>, Box<dyn std::error::Error>> {
    Ok(graph
        .full_view()?
        .nodes
        .into_iter()
        .find(|node| node.path == path)
        .map(|node| node.kind))
}

fn destinations(graph: &mut LinkGraph, from: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(graph
        .full_view()?
        .edges
        .into_iter()
        .filter(|edge| edge.from == from)
        .map(|edge| edge.to)
        .collect())
}

fn files(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| (*path).to_owned()).collect()
}

#[test]
fn an_image_added_before_the_first_event_still_resolves_the_phantom_that_waited_for_it() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        {
            let mut graph = open(&dir, backend)?;
            graph.seed_files(files(&["a.md"]));
            note(&mut graph, "a.md", "![[pic.png]]\n")?;
            assert_eq!(kind_of(&mut graph, "pic.png")?, Some(GraphNodeKind::NotFound));
        }
        let mut reopened = open(&dir, backend)?;
        // The vault was listed after the image landed, as a service that opens late does.
        reopened.seed_files(files(&["a.md", "pic.png"]));

        reopened.file_added("pic.png")?;

        assert_eq!(destinations(&mut reopened, "a.md")?, ["pic.png"], "backend {backend:?}");
        assert_eq!(
            kind_of(&mut reopened, "pic.png")?,
            Some(GraphNodeKind::File),
            "{backend:?}"
        );
    }
    Ok(())
}

#[test]
fn deleting_one_of_two_candidates_after_the_listing_already_lost_it_settles_the_ambiguous_link() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        {
            let mut graph = open(&dir, backend)?;
            graph.seed_files(files(&["a.md", "x/t.md", "y/t.md"]));
            note(&mut graph, "x/t.md", "x\n")?;
            note(&mut graph, "y/t.md", "y\n")?;
            note(&mut graph, "a.md", "[[t]]\n")?;
            assert_eq!(kind_of(&mut graph, "t")?, Some(GraphNodeKind::Ambiguous));
        }
        let mut reopened = open(&dir, backend)?;
        reopened.seed_files(files(&["a.md", "x/t.md"]));

        reopened.remove_note("y/t.md")?;

        assert_eq!(destinations(&mut reopened, "a.md")?, ["x/t.md"], "backend {backend:?}");
    }
    Ok(())
}

#[test]
fn deleting_a_folder_after_the_listing_already_lost_it_removes_the_notes_it_held() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        {
            let mut graph = open(&dir, backend)?;
            graph.seed_files(files(&["a.md", "d/n.md"]));
            note(&mut graph, "d/n.md", "n\n")?;
            note(&mut graph, "a.md", "[[d/n]]\n")?;
        }
        let mut reopened = open(&dir, backend)?;
        reopened.seed_files(files(&["a.md"]));

        reopened.file_removed("d")?;

        assert_eq!(kind_of(&mut reopened, "d/n.md")?, None, "backend {backend:?}");
        assert_eq!(destinations(&mut reopened, "a.md")?, ["d/n"], "backend {backend:?}");
        assert_eq!(
            kind_of(&mut reopened, "d/n")?,
            Some(GraphNodeKind::NotFound),
            "{backend:?}"
        );
    }
    Ok(())
}

#[test]
fn a_file_added_by_one_process_is_a_link_target_for_another_process_sharing_the_store() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut first = open(&dir, GraphBackend::Sqlite)?;
    let mut second = open(&dir, GraphBackend::Sqlite)?;
    first.seed_files(files(&["a.md"]));
    second.seed_files(files(&["a.md"]));

    first.file_added("pic.png")?;
    note(&mut second, "b.md", "![[pic.png]]\n")?;

    assert_eq!(destinations(&mut second, "b.md")?, ["pic.png"]);
    assert_eq!(kind_of(&mut second, "pic.png")?, Some(GraphNodeKind::File));
    Ok(())
}

#[test]
fn a_note_written_by_one_process_is_a_link_target_for_another_process_sharing_the_store() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut first = open(&dir, GraphBackend::Sqlite)?;
    let mut second = open(&dir, GraphBackend::Sqlite)?;
    first.seed_files(files(&[]));
    second.seed_files(files(&[]));

    note(&mut first, "wiki/topic.md", "t\n")?;
    note(&mut second, "b.md", "[[topic]]\n")?;

    assert_eq!(destinations(&mut second, "b.md")?, ["wiki/topic.md"]);
    Ok(())
}

#[test]
fn a_file_removed_by_one_process_stops_being_a_link_target_for_another_process() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut first = open(&dir, GraphBackend::Sqlite)?;
    let mut second = open(&dir, GraphBackend::Sqlite)?;
    first.seed_files(files(&["pic.png"]));
    second.seed_files(files(&["pic.png"]));

    first.file_removed("pic.png")?;
    note(&mut second, "b.md", "![[pic.png]]\n")?;

    assert_eq!(kind_of(&mut second, "pic.png")?, Some(GraphNodeKind::NotFound));
    Ok(())
}

#[test]
fn a_graph_lists_the_vault_again_after_another_process_rebuilds_the_store_under_it() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut first = open(&dir, GraphBackend::Sqlite)?;
    let mut second = open(&dir, GraphBackend::Sqlite)?;
    let on_disk = std::sync::Arc::new(std::sync::Mutex::new(files(&["a.md"])));
    let lister = {
        let on_disk = std::sync::Arc::clone(&on_disk);
        std::sync::Arc::new(move || Ok(on_disk.lock().map(|listed| listed.clone()).unwrap_or_default()))
    };
    second.list_files_with(lister)?;
    first.rebuild_vault(VaultScan {
        files: files(&["a.md"]),
        ..VaultScan::default()
    })?;
    // The image arrives while the second process is idle, and the first process's rebuild replaces the stored graph.
    on_disk
        .lock()
        .map_err(|_| "listing lock poisoned")?
        .push("pic.png".to_owned());
    first.rebuild_vault(VaultScan {
        files: files(&["a.md", "pic.png"]),
        ..VaultScan::default()
    })?;

    note(&mut second, "b.md", "![[pic.png]]\n")?;

    assert_eq!(destinations(&mut second, "b.md")?, ["pic.png"]);
    Ok(())
}

#[test]
fn a_batch_of_files_is_one_write_to_the_store() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut graph = open(&dir, GraphBackend::Sqlite)?;
    graph.seed_files(files(&[]));
    let before = graph.store_generation();
    let entries: Vec<FileEntry> = (0..20)
        .map(|index| FileEntry::File(format!("assets/{index}.png")))
        .chain((0..20).map(|index| {
            FileEntry::Note(NoteEntry {
                path: format!("notes/{index}.md"),
                title: format!("{index}"),
                links: Ok(Vec::new()),
                base: None,
                stamp: None,
            })
        }))
        .collect();

    graph.apply_entries(&entries)?;

    assert_eq!(
        graph.store_generation(),
        before.map_or(Some(1), |generation| Some(generation + 1))
    );
    Ok(())
}
