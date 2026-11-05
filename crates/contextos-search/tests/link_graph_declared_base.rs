//! A note's declared base settles a name several files answer to, by the folder that base keeps its records in, and
//! the graph stays right as that base file is changed or removed, in this process or another sharing the store.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use contextos_obsidian::{BaseRecords, LinkCollection};
use contextos_search::{
    FileEntry, GraphBackend, GraphNodeKind, LinkGraph, LinkGraphConfig, NoteEntry, ReadsBaseFolders,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The folder each base file keeps its records in, as the vault holds it now; tests change it as a person edits a base.
#[derive(Default)]
struct Bases(Mutex<BTreeMap<String, String>>);

impl Bases {
    fn keep_records_in(&self, base: &str, folder: &str) {
        if let Ok(mut held) = self.0.lock() {
            held.insert(base.to_owned(), folder.to_owned());
        }
    }
}

impl ReadsBaseFolders for Bases {
    fn records_folder(&self, base: &str) -> Option<BaseRecords> {
        let held = self.0.lock().ok()?;
        held.get(base).map(|folder| BaseRecords {
            path: folder.clone(),
            includes_subfolders: true,
        })
    }
}

const FILES: [&str; 3] = ["people/ann.md", "archive/ann.md", "bases/people.base"];

fn open(dir: &tempfile::TempDir, bases: &Arc<Bases>) -> Result<LinkGraph, Box<dyn std::error::Error>> {
    let mut graph = LinkGraph::try_from(LinkGraphConfig {
        store_directory: PathBuf::from(dir.path()).join(".contextos").join("graph"),
        backend: GraphBackend::Sqlite,
        write_wait: contextos_search::DEFAULT_WRITE_WAIT,
    })?;
    graph.seed_files(FILES.iter().map(|file| (*file).to_owned()));
    graph.read_bases_with(Arc::<Bases>::clone(bases));
    Ok(graph)
}

/// Records the note at `path` as one declaring `people.base` and linking `[[ann]]`.
fn declaring_note(graph: &mut LinkGraph, path: &str) -> TestResult {
    let links = LinkCollection::try_from("[[ann]]\n")?;
    graph.apply_entries(&[FileEntry::Note(NoteEntry {
        path: path.to_owned(),
        title: path.to_owned(),
        links: Ok(links.outgoing().to_vec()),
        base: Some("people.base".to_owned()),
        stamp: None,
    })])?;
    Ok(())
}

fn reached(graph: &mut LinkGraph, from: &str) -> Result<Vec<(String, GraphNodeKind)>, Box<dyn std::error::Error>> {
    Ok(graph
        .full_view()?
        .edges
        .into_iter()
        .filter(|edge| edge.from == from && edge.target == "ann")
        .filter_map(|edge| {
            let node = graph
                .full_view()
                .ok()?
                .nodes
                .into_iter()
                .find(|node| node.path == edge.to)?;
            Some((node.path, node.kind))
        })
        .collect())
}

#[test]
fn a_base_file_changed_in_this_process_re_settles_the_names_of_the_notes_declaring_it() -> TestResult {
    let dir = tempfile::tempdir()?;
    let bases = Arc::new(Bases::default());
    bases.keep_records_in("bases/people.base", "people");
    let mut graph = open(&dir, &bases)?;
    declaring_note(&mut graph, "team.md")?;
    assert_eq!(
        reached(&mut graph, "team.md")?,
        [("people/ann.md".to_owned(), GraphNodeKind::File)]
    );

    bases.keep_records_in("bases/people.base", "archive");
    graph.file_added("bases/people.base")?;

    assert_eq!(
        reached(&mut graph, "team.md")?,
        [("archive/ann.md".to_owned(), GraphNodeKind::File)]
    );
    Ok(())
}

#[test]
fn a_base_file_removed_leaves_the_names_of_the_notes_declaring_it_ambiguous() -> TestResult {
    let dir = tempfile::tempdir()?;
    let bases = Arc::new(Bases::default());
    bases.keep_records_in("bases/people.base", "people");
    let mut graph = open(&dir, &bases)?;
    declaring_note(&mut graph, "team.md")?;

    graph.file_removed("bases/people.base")?;

    assert_eq!(
        reached(&mut graph, "team.md")?,
        [("ann".to_owned(), GraphNodeKind::Ambiguous)]
    );
    Ok(())
}

#[test]
fn a_base_file_changed_by_another_process_is_read_afresh_for_the_next_note_declaring_it() -> TestResult {
    let dir = tempfile::tempdir()?;
    let bases = Arc::new(Bases::default());
    bases.keep_records_in("bases/people.base", "people");
    let mut first = open(&dir, &bases)?;
    let mut second = open(&dir, &bases)?;
    declaring_note(&mut second, "team.md")?;
    assert_eq!(
        reached(&mut second, "team.md")?,
        [("people/ann.md".to_owned(), GraphNodeKind::File)]
    );

    bases.keep_records_in("bases/people.base", "archive");
    first.file_added("bases/people.base")?;
    declaring_note(&mut second, "later.md")?;

    assert_eq!(
        reached(&mut second, "later.md")?,
        [("archive/ann.md".to_owned(), GraphNodeKind::File)]
    );
    Ok(())
}
