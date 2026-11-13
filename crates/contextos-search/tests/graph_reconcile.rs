//! The link graph kept right by the service as files change inside and outside the server: external edits reconcile,
//! hidden files stay out, a note's declared base settles a name, and the first event after the service opens re-resolves
//! what the vault already held.

use std::fs;

use contextos_core::{
    OpKind, OperationEvent, Origin, UpdatesSearch, VaultPath, VaultPathInput, VaultRoot, VaultRootId, VaultRootInput,
    VaultSet,
};
use contextos_search::{
    GraphBackend, GraphDirection, GraphNodeKind, GraphView, SearchError, VaultSearchConfig, VaultSearchService,
};
use time::OffsetDateTime;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Vault {
    dir: tempfile::TempDir,
    roots: VaultSet,
}

fn vault() -> Result<Vault, Box<dyn std::error::Error>> {
    let dir = tempfile::Builder::new().prefix("vault").tempdir()?;
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: dir.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?])?;
    Ok(Vault { dir, roots })
}

impl Vault {
    fn open(&self, hidden: &[&str]) -> Result<VaultSearchService, Box<dyn std::error::Error>> {
        Ok(VaultSearchService::try_from(VaultSearchConfig {
            root_id: VaultRootId::try_from(0_usize)?,
            root: self.dir.path().to_path_buf(),
            excludes: vec![".contextos".to_owned()],
            link_excludes: Vec::new(),
            link_ignore_targets: Vec::new(),
            hidden: hidden.iter().map(|pattern| (*pattern).to_owned()).collect(),
            state_directory: self.dir.path().join(".contextos"),
            text_enabled: false,
            graph_enabled: true,
            graph_backend: GraphBackend::Sqlite,
            semantic: None,
        })?)
    }

    fn put(&self, relative: &str, content: &[u8]) -> TestResult {
        let path = self.dir.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)?;
        Ok(())
    }

    fn event(&self, kind: OpKind, relative: &str) -> Result<OperationEvent, Box<dyn std::error::Error>> {
        Ok(OperationEvent {
            kind,
            paths: vec![VaultPath::try_from(VaultPathInput {
                roots: &self.roots,
                raw: relative,
            })?],
            origin: Origin::Tool("fs_write_file".to_owned()),
            summary: "test".to_owned(),
            at: OffsetDateTime::UNIX_EPOCH,
        })
    }

    /// Writes the file and records the write, as the write pipeline does.
    fn write(&self, service: &VaultSearchService, relative: &str, content: &[u8]) -> TestResult {
        self.put(relative, content)?;
        service
            .update(&self.event(OpKind::Modify, relative)?)
            .map_err(|warning| format!("{warning:?}"))?;
        Ok(())
    }
}

fn reached(
    service: &VaultSearchService,
    note: &str,
) -> Result<Vec<(String, GraphNodeKind)>, Box<dyn std::error::Error>> {
    let view: GraphView = service.graph_neighbours(note, 1, GraphDirection::Out)?;
    let mut reached: Vec<_> = view
        .nodes
        .into_iter()
        .filter(|node| node.path != note)
        .map(|node| (node.path, node.kind))
        .collect();
    reached.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(reached)
}

fn has_node(service: &VaultSearchService, path: &str) -> Result<bool, Box<dyn std::error::Error>> {
    match service.graph_backlinks(path) {
        Ok(_) => Ok(true),
        Err(SearchError::UnknownNote { .. }) => Ok(false),
        Err(other) => Err(other.into()),
    }
}

#[test]
fn a_file_created_outside_the_server_resolves_the_link_that_waited_for_it_once_reconciled() -> TestResult {
    let vault = vault()?;
    let service = vault.open(&[])?;
    vault.write(&service, "a.md", b"![[pic.png]]\n")?;
    vault.put("assets/pic.png", &[1, 2, 3])?;

    let report = service.reconcile_graph()?;

    assert_eq!(report.files_added, 1, "{report:?}");
    assert_eq!(
        reached(&service, "a.md")?,
        vec![("assets/pic.png".to_owned(), GraphNodeKind::File)]
    );
    Ok(())
}

#[test]
fn a_note_edited_outside_the_server_has_its_new_links_recorded_once_reconciled() -> TestResult {
    let vault = vault()?;
    let service = vault.open(&[])?;
    vault.write(&service, "a.md", b"no links\n")?;
    vault.write(&service, "b.md", b"b\n")?;
    vault.put("a.md", b"now [[b]] and more text\n")?;

    let report = service.reconcile_graph()?;

    assert_eq!(report.notes_read, 1, "{report:?}");
    assert_eq!(
        reached(&service, "a.md")?,
        vec![("b.md".to_owned(), GraphNodeKind::Note)]
    );
    Ok(())
}

#[test]
fn a_note_deleted_outside_the_server_is_removed_and_the_links_to_it_break_once_reconciled() -> TestResult {
    let vault = vault()?;
    let service = vault.open(&[])?;
    vault.write(&service, "b.md", b"b\n")?;
    vault.write(&service, "a.md", b"[[b]]\n")?;
    fs::remove_file(vault.dir.path().join("b.md"))?;

    let report = service.reconcile_graph()?;

    assert_eq!(report.files_removed, 1, "{report:?}");
    assert_eq!(
        reached(&service, "a.md")?,
        vec![("b".to_owned(), GraphNodeKind::NotFound)]
    );
    Ok(())
}

#[test]
fn reconciling_a_vault_that_has_not_changed_reads_no_note_and_changes_nothing() -> TestResult {
    let vault = vault()?;
    let service = vault.open(&[])?;
    vault.write(&service, "a.md", b"[[b]]\n")?;
    vault.write(&service, "b.md", b"b\n")?;
    vault.put("pic.png", &[1])?;
    service.reconcile_graph()?;

    let second = service.reconcile_graph()?;

    assert!(second.is_unchanged(), "{second:?}");
    assert_eq!(second.files, 3);
    Ok(())
}

#[test]
fn a_graph_question_sees_an_edit_made_outside_the_server_without_an_explicit_reconcile() -> TestResult {
    let vault = vault()?;
    let service = vault.open(&[])?;
    vault.write(&service, "b.md", b"b\n")?;
    vault.put("a.md", b"[[b]]\n")?;

    let backlinks = service.graph_backlinks("b.md")?;

    assert_eq!(
        backlinks
            .edges
            .iter()
            .map(|edge| edge.from.as_str())
            .collect::<Vec<_>>(),
        ["a.md"]
    );
    Ok(())
}

#[test]
fn neighbours_include_a_link_added_outside_the_server_without_an_explicit_reconcile() -> TestResult {
    let vault = vault()?;
    let service = vault.open(&[])?;
    vault.write(&service, "b.md", b"b\n")?;
    vault.put("a.md", b"[[b]]\n")?;

    let view = service.graph_neighbours("b.md", 1, GraphDirection::In)?;

    assert_eq!(
        view.edges.iter().map(|edge| edge.from.as_str()).collect::<Vec<_>>(),
        ["a.md"]
    );
    Ok(())
}

#[test]
fn a_path_follows_a_link_added_outside_the_server_without_an_explicit_reconcile() -> TestResult {
    let vault = vault()?;
    let service = vault.open(&[])?;
    vault.write(&service, "a.md", b"nothing here\n")?;
    vault.write(&service, "b.md", b"b\n")?;
    vault.put("a.md", b"now it links to [[b]]\n")?;

    let view = service.graph_path("a.md", "b.md", GraphDirection::Out)?;

    assert_eq!(
        view.edges
            .iter()
            .map(|edge| (edge.from.as_str(), edge.to.as_str()))
            .collect::<Vec<_>>(),
        [("a.md", "b.md")]
    );
    Ok(())
}

#[test]
fn a_note_linked_outside_the_server_is_no_longer_an_orphan_without_an_explicit_reconcile() -> TestResult {
    let vault = vault()?;
    let service = vault.open(&[])?;
    vault.write(&service, "a.md", b"nothing here\n")?;
    vault.write(&service, "b.md", b"b\n")?;
    vault.put("a.md", b"now it links to [[b]]\n")?;

    let orphans = service.graph_orphans()?;

    assert!(orphans.nodes.is_empty(), "{:?}", orphans.nodes);
    Ok(())
}

#[test]
fn a_hidden_file_is_no_link_target_and_no_node() -> TestResult {
    let vault = vault()?;
    vault.put("private/secret.md", b"s\n")?;
    vault.put("private/secret.png", &[1])?;
    let service = vault.open(&["private/**", "private/"])?;
    vault.write(&service, "a.md", b"[[secret]] ![[secret.png]]\n")?;
    service.reconcile_graph()?;

    assert_eq!(
        reached(&service, "a.md")?,
        vec![
            ("secret".to_owned(), GraphNodeKind::NotFound),
            ("secret.png".to_owned(), GraphNodeKind::NotFound)
        ]
    );
    assert!(!has_node(&service, "private/secret.md")?);
    Ok(())
}

#[test]
fn a_note_declaring_a_base_settles_an_ambiguous_name_by_the_folder_that_base_keeps_its_records_in() -> TestResult {
    let vault = vault()?;
    vault.put("people/ann.md", b"ann\n")?;
    vault.put("archive/ann.md", b"old ann\n")?;
    vault.put(
        "bases/people.base",
        b"filters:\n  and:\n    - file.inFolder(\"people\")\n",
    )?;
    let service = vault.open(&[])?;
    vault.write(&service, "team.md", b"---\nbase: \"[[people.base]]\"\n---\n[[ann]]\n")?;
    vault.write(&service, "other.md", b"[[ann]]\n")?;

    assert_eq!(
        reached(&service, "team.md")?,
        vec![
            ("bases/people.base".to_owned(), GraphNodeKind::File),
            ("people/ann.md".to_owned(), GraphNodeKind::Note)
        ]
    );
    assert_eq!(
        reached(&service, "other.md")?,
        vec![("ann".to_owned(), GraphNodeKind::Ambiguous)]
    );
    Ok(())
}

#[test]
fn changing_the_base_a_note_declares_re_settles_its_names() -> TestResult {
    let vault = vault()?;
    vault.put("people/ann.md", b"ann\n")?;
    vault.put("archive/ann.md", b"old ann\n")?;
    vault.put(
        "bases/people.base",
        b"filters:\n  and:\n    - file.inFolder(\"people\")\n",
    )?;
    let service = vault.open(&[])?;
    vault.write(&service, "team.md", b"---\nbase: \"[[people.base]]\"\n---\n[[ann]]\n")?;

    vault.write(
        &service,
        "bases/people.base",
        b"filters:\n  and:\n    - file.inFolder(\"archive\")\n",
    )?;

    let destinations: Vec<String> = reached(&service, "team.md")?
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert!(destinations.contains(&"archive/ann.md".to_owned()), "{destinations:?}");
    Ok(())
}

#[test]
fn the_first_event_after_the_service_opens_re_resolves_a_link_to_a_file_the_vault_already_holds() -> TestResult {
    let vault = vault()?;
    {
        let service = vault.open(&[])?;
        vault.write(&service, "a.md", b"![[pic.png]]\n")?;
    }
    vault.put("pic.png", &[1])?;
    let service = vault.open(&[])?;

    service
        .update(&vault.event(OpKind::Create, "pic.png")?)
        .map_err(|warning| format!("{warning:?}"))?;

    assert_eq!(
        reached(&service, "a.md")?,
        vec![("pic.png".to_owned(), GraphNodeKind::File)]
    );
    Ok(())
}

#[test]
fn the_first_event_after_the_service_opens_removes_a_deleted_folders_notes() -> TestResult {
    let vault = vault()?;
    {
        let service = vault.open(&[])?;
        vault.write(&service, "d/n.md", b"n\n")?;
        vault.write(&service, "a.md", b"[[d/n]]\n")?;
    }
    fs::remove_dir_all(vault.dir.path().join("d"))?;
    let service = vault.open(&[])?;

    service
        .update(&vault.event(OpKind::Delete, "d")?)
        .map_err(|warning| format!("{warning:?}"))?;

    assert_eq!(
        reached(&service, "a.md")?,
        vec![("d/n".to_owned(), GraphNodeKind::NotFound)]
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_moved_folder_records_every_file_it_can_and_reports_the_one_it_cannot() -> TestResult {
    let vault = vault()?;
    let service = vault.open(&[])?;
    vault.put("moved/one.md", b"1\n")?;
    vault.put("moved/bad:name.md", b"bad\n")?;
    vault.put("moved/two.md", b"[[one]]\n")?;

    let outcome = service.update(&vault.event(OpKind::Create, "moved")?);

    assert!(outcome.is_err(), "the unrecordable file must be reported");
    assert_eq!(
        reached(&service, "moved/two.md")?,
        vec![("moved/one.md".to_owned(), GraphNodeKind::Note)]
    );
    Ok(())
}
