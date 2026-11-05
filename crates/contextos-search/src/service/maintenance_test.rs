use contextos_core::{Origin, UpdatesSearch, VaultRoot, VaultRootId, VaultRootInput};

use super::*;
use crate::{GraphBackend, GraphDirection, GraphNodeKind, GraphView, RebuildTarget, VaultSearchConfig};

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
    /// One instance's graph-only search service over this vault, sharing its state directory with every other.
    fn instance(&self) -> Result<VaultSearchService, Box<dyn std::error::Error>> {
        self.instance_on(GraphBackend::Sqlite)
    }

    /// As [`Self::instance`], with the link graph kept by `backend`.
    fn instance_on(&self, backend: GraphBackend) -> Result<VaultSearchService, Box<dyn std::error::Error>> {
        self.instance_excluding(backend, &[".contextos"])
    }

    /// As [`Self::instance_on`], with the folders in `excludes` kept out of the indexes.
    fn instance_excluding(
        &self,
        backend: GraphBackend,
        excludes: &[&str],
    ) -> Result<VaultSearchService, Box<dyn std::error::Error>> {
        Ok(VaultSearchService::try_from(VaultSearchConfig {
            root_id: VaultRootId::try_from(0_usize)?,
            root: self.dir.path().to_path_buf(),
            excludes: excludes.iter().map(|folder| (*folder).to_owned()).collect(),
            link_excludes: Vec::new(),
            hidden: Vec::new(),
            state_directory: self.dir.path().join(".contextos"),
            text_enabled: false,
            graph_enabled: true,
            graph_backend: backend,
            semantic: None,
        })?)
    }

    /// As [`Self::instance`], with every path matching a pattern in `hidden` kept out of the vault's view.
    fn instance_hiding(&self, hidden: &[&str]) -> Result<VaultSearchService, Box<dyn std::error::Error>> {
        Ok(VaultSearchService::try_from(VaultSearchConfig {
            root_id: VaultRootId::try_from(0_usize)?,
            root: self.dir.path().to_path_buf(),
            excludes: vec![".contextos".to_owned()],
            link_excludes: Vec::new(),
            hidden: hidden.iter().map(|pattern| (*pattern).to_owned()).collect(),
            state_directory: self.dir.path().join(".contextos"),
            text_enabled: false,
            graph_enabled: true,
            graph_backend: GraphBackend::Sqlite,
            semantic: None,
        })?)
    }

    fn event(&self, kind: OpKind, relative: &str) -> Result<OperationEvent, Box<dyn std::error::Error>> {
        let path = VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw: relative,
        })?;
        Ok(OperationEvent {
            kind,
            paths: vec![path],
            origin: Origin::Tool("fs_write_file".to_owned()),
            summary: "test".to_owned(),
            at: OffsetDateTime::UNIX_EPOCH,
        })
    }

    /// Writes `content` to `relative` and records it in `instance`'s graph the way the write pipeline does.
    fn write(&self, instance: &VaultSearchService, relative: &str, content: &str) -> TestResult {
        fs::write(self.dir.path().join(relative), content)?;
        instance
            .update(&self.event(OpKind::Modify, relative)?)
            .map_err(|warning| format!("recording the write failed: {warning:?}"))?;
        Ok(())
    }

    /// Deletes `relative` and records the deletion in `instance`'s graph.
    fn delete(&self, instance: &VaultSearchService, relative: &str) -> TestResult {
        fs::remove_file(self.dir.path().join(relative))?;
        instance
            .update(&self.event(OpKind::Delete, relative)?)
            .map_err(|warning| format!("recording the deletion failed: {warning:?}"))?;
        Ok(())
    }
}

fn backlinks(instance: &VaultSearchService, to: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let view = instance.graph_backlinks(to)?;
    Ok(view
        .edges
        .into_iter()
        .map(|edge| edge.from)
        .filter(|from| from != to)
        .collect())
}

fn rebuilding(
    instance: &VaultSearchService,
) -> Result<std::sync::Arc<super::super::slot::Held<Mutex<LinkGraph>>>, Box<dyn std::error::Error>> {
    Ok(instance.graph_store()?)
}

#[test]
fn a_graph_rebuild_never_puts_back_links_older_than_a_write_made_while_it_ran() -> TestResult {
    let vault = vault()?;
    let instance = vault.instance()?;
    vault.write(&instance, "a.md", "# A\n\n[[b]]\n")?;
    vault.write(&instance, "b.md", "# B\n")?;
    vault.write(&instance, "c.md", "# C\n")?;
    let graph = rebuilding(&instance)?;
    let scanned = instance.scan_graph_notes(graph.as_ref())?;

    vault.write(&instance, "a.md", "# A\n\nNow links elsewhere: [[c]]\n")?;
    instance.commit_graph_rebuild(graph.as_ref(), scanned)?;

    assert_eq!(
        backlinks(&instance, "c.md")?,
        vec!["a.md".to_owned()],
        "the newer link must survive"
    );
    assert!(
        backlinks(&instance, "b.md")?.is_empty(),
        "the rebuild must not put back the older link"
    );
    Ok(())
}

#[test]
fn a_graph_rebuild_never_overwrites_a_write_another_instance_made_while_it_ran() -> TestResult {
    let vault = vault()?;
    let rebuilder = vault.instance()?;
    let writer = vault.instance()?;
    vault.write(&writer, "a.md", "# A\n\n[[b]]\n")?;
    vault.write(&writer, "b.md", "# B\n")?;
    vault.write(&writer, "c.md", "# C\n")?;
    let graph = rebuilding(&rebuilder)?;
    let scanned = rebuilder.scan_graph_notes(graph.as_ref())?;

    vault.write(&writer, "a.md", "# A\n\nNow links elsewhere: [[c]]\n")?;
    rebuilder.commit_graph_rebuild(graph.as_ref(), scanned)?;

    for (name, instance) in [
        ("rebuilder", &rebuilder),
        ("writer", &writer),
        ("fresh", &vault.instance()?),
    ] {
        assert_eq!(
            backlinks(instance, "c.md")?,
            vec!["a.md".to_owned()],
            "{name}: the newer link must survive"
        );
        assert!(
            backlinks(instance, "b.md")?.is_empty(),
            "{name}: the older link must not come back"
        );
    }
    Ok(())
}

#[test]
fn a_graph_rebuild_keeps_a_note_created_while_it_ran() -> TestResult {
    let vault = vault()?;
    let instance = vault.instance()?;
    vault.write(&instance, "c.md", "# C\n")?;
    let graph = rebuilding(&instance)?;
    let scanned = instance.scan_graph_notes(graph.as_ref())?;

    vault.write(&instance, "d.md", "# D\n\n[[c]]\n")?;
    instance.commit_graph_rebuild(graph.as_ref(), scanned)?;

    assert_eq!(backlinks(&instance, "c.md")?, vec!["d.md".to_owned()]);
    Ok(())
}

#[test]
fn a_graph_rebuild_drops_a_note_deleted_while_it_ran() -> TestResult {
    let vault = vault()?;
    let instance = vault.instance()?;
    vault.write(&instance, "a.md", "# A\n\n[[c]]\n")?;
    vault.write(&instance, "c.md", "# C\n")?;
    let graph = rebuilding(&instance)?;
    let scanned = instance.scan_graph_notes(graph.as_ref())?;

    vault.delete(&instance, "a.md")?;
    instance.commit_graph_rebuild(graph.as_ref(), scanned)?;

    assert!(
        backlinks(&instance, "c.md")?.is_empty(),
        "a deleted note's links must not come back"
    );
    assert!(
        instance.graph_neighbours("a.md", 1, GraphDirection::Both).is_err(),
        "a deleted note must not come back"
    );
    Ok(())
}

#[test]
fn a_graph_rebuild_on_a_backend_with_no_history_rechecks_every_note() -> TestResult {
    let vault = vault()?;
    let instance = vault.instance_on(GraphBackend::Serde)?;
    vault.write(&instance, "a.md", "# A\n\n[[b]]\n")?;
    vault.write(&instance, "b.md", "# B\n")?;
    vault.write(&instance, "c.md", "# C\n")?;
    let graph = rebuilding(&instance)?;
    let scanned = instance.scan_graph_notes(graph.as_ref())?;

    vault.write(&instance, "a.md", "# A\n\nNow links elsewhere: [[c]]\n")?;
    vault.write(&instance, "d.md", "# D\n\n[[c]]\n")?;
    instance.commit_graph_rebuild(graph.as_ref(), scanned)?;

    let mut linking_c = backlinks(&instance, "c.md")?;
    linking_c.sort();
    assert_eq!(linking_c, vec!["a.md".to_owned(), "d.md".to_owned()]);
    assert_eq!(backlinks(&instance, "b.md")?, Vec::<String>::new());
    Ok(())
}

/// Where the links of the note at `note` lead, and what each destination is, as the graph reports them.
fn reached(
    instance: &VaultSearchService,
    note: &str,
) -> Result<Vec<(String, crate::GraphNodeKind)>, Box<dyn std::error::Error>> {
    let view = instance.graph_neighbours(note, 1, GraphDirection::Out)?;
    let mut reached: Vec<(String, crate::GraphNodeKind)> = view
        .nodes
        .into_iter()
        .filter(|node| node.path != note)
        .map(|node| (node.path, node.kind))
        .collect();
    reached.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(reached)
}

impl Vault {
    fn write_binary(&self, instance: &VaultSearchService, relative: &str) -> TestResult {
        if let Some(parent) = self.dir.path().join(relative).parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(self.dir.path().join(relative), [0_u8, 159, 146, 150])?;
        instance
            .update(&self.event(OpKind::Create, relative)?)
            .map_err(|warning| format!("recording the write failed: {warning:?}"))?;
        Ok(())
    }
}

#[test]
fn a_link_to_a_file_that_is_not_a_note_resolves_to_it_when_the_note_is_written_after_the_file() -> TestResult {
    let vault = vault()?;
    let instance = vault.instance()?;
    vault.write_binary(&instance, "assets/photo.png")?;
    vault.write(&instance, "a.md", "# A\n\n![[photo.png]] and [x](assets/photo.png)\n")?;

    assert_eq!(
        reached(&instance, "a.md")?,
        vec![("assets/photo.png".to_owned(), crate::GraphNodeKind::File)]
    );
    Ok(())
}

#[test]
fn a_file_created_after_the_link_to_it_resolves_the_link_and_deleting_it_breaks_the_link() -> TestResult {
    let vault = vault()?;
    let instance = vault.instance()?;
    vault.write(&instance, "a.md", "# A\n\n![[photo.png]]\n")?;
    assert_eq!(
        reached(&instance, "a.md")?,
        vec![("photo.png".to_owned(), crate::GraphNodeKind::NotFound)]
    );

    vault.write_binary(&instance, "assets/photo.png")?;
    assert_eq!(
        reached(&instance, "a.md")?,
        vec![("assets/photo.png".to_owned(), crate::GraphNodeKind::File)]
    );

    vault.delete(&instance, "assets/photo.png")?;
    assert_eq!(
        reached(&instance, "a.md")?,
        vec![("photo.png".to_owned(), crate::GraphNodeKind::NotFound)]
    );
    Ok(())
}

#[test]
fn a_link_to_a_note_in_a_folder_the_search_excludes_resolves_to_a_file_node() -> TestResult {
    let vault = vault()?;
    let instance = vault.instance_excluding(GraphBackend::Sqlite, &[".contextos", "templates"])?;
    fs::create_dir_all(vault.dir.path().join("templates"))?;
    vault.write(&instance, "templates/daily.md", "# Daily\n\n[[nowhere]]\n")?;
    vault.write(&instance, "a.md", "# A\n\n[[daily]]\n")?;

    assert_eq!(
        reached(&instance, "a.md")?,
        vec![("templates/daily.md".to_owned(), crate::GraphNodeKind::File)]
    );
    Ok(())
}

#[test]
fn a_note_whose_links_cannot_be_read_is_still_a_note_and_records_why() -> TestResult {
    let vault = vault()?;
    let instance = vault.instance()?;
    vault.write(&instance, "b.md", "# B\n")?;
    vault.write(&instance, "a.md", "# A\n\n[[b]]\n")?;

    vault.write(&instance, "a.md", "# A\n\n[[never closed\n")?;

    let view = instance.graph_neighbours("a.md", 1, GraphDirection::Both)?;
    let note = view.nodes.iter().find(|node| node.path == "a.md");
    assert_eq!(
        note.and_then(|node| node.unreadable.as_deref()),
        Some("wikilink on line 3 has no closing brackets")
    );
    assert!(view.edges.is_empty(), "the stale link to b.md is gone");
    Ok(())
}

#[test]
fn a_moved_folder_re_resolves_the_links_that_reached_its_files() -> TestResult {
    let vault = vault()?;
    let instance = vault.instance()?;
    vault.write_binary(&instance, "old/photo.png")?;
    vault.write(&instance, "a.md", "# A\n\n![[photo.png]]\n")?;

    fs::rename(vault.dir.path().join("old"), vault.dir.path().join("new"))?;
    let moved = OperationEvent {
        kind: OpKind::Move,
        paths: vec![
            VaultPath::try_from(VaultPathInput {
                roots: &vault.roots,
                raw: "old",
            })?,
            VaultPath::try_from(VaultPathInput {
                roots: &vault.roots,
                raw: "new",
            })?,
        ],
        origin: Origin::Tool("fs_move_file".to_owned()),
        summary: "test".to_owned(),
        at: OffsetDateTime::UNIX_EPOCH,
    };
    instance
        .update(&moved)
        .map_err(|warning| format!("recording the move failed: {warning:?}"))?;

    assert_eq!(
        reached(&instance, "a.md")?,
        vec![("new/photo.png".to_owned(), crate::GraphNodeKind::File)]
    );
    Ok(())
}

#[test]
fn a_rebuild_resolves_links_against_every_file_and_records_unreadable_notes() -> TestResult {
    let vault = vault()?;
    let instance = vault.instance_excluding(GraphBackend::Sqlite, &[".contextos", "templates"])?;
    fs::create_dir_all(vault.dir.path().join("assets"))?;
    fs::create_dir_all(vault.dir.path().join("templates"))?;
    fs::write(vault.dir.path().join("assets/photo.png"), [1_u8, 2, 3])?;
    fs::write(vault.dir.path().join("assets/plan.canvas"), "{}")?;
    fs::write(vault.dir.path().join("templates/daily.md"), "# Daily\n")?;
    fs::write(
        vault.dir.path().join("a.md"),
        "# A\n\n![[photo.png]] [[plan.canvas]] [[daily]] [[missing]]\n",
    )?;
    fs::write(vault.dir.path().join("bad.md"), "# Bad\n\n[[open\n")?;

    instance.rebuild(RebuildTarget::Graph)?;

    assert_eq!(
        reached(&instance, "a.md")?,
        vec![
            ("assets/photo.png".to_owned(), crate::GraphNodeKind::File),
            ("assets/plan.canvas".to_owned(), crate::GraphNodeKind::File),
            ("missing".to_owned(), crate::GraphNodeKind::NotFound),
            ("templates/daily.md".to_owned(), crate::GraphNodeKind::File),
        ]
    );
    let bad = instance.graph_neighbours("bad.md", 1, GraphDirection::Both)?;
    assert_eq!(
        bad.nodes
            .iter()
            .find(|node| node.path == "bad.md")
            .and_then(|node| node.unreadable.as_deref()),
        Some("wikilink on line 3 has no closing brackets")
    );
    Ok(())
}

#[test]
fn a_rebuild_never_looks_outside_the_vault_for_a_stand_in_keyed_by_what_a_link_said() -> TestResult {
    let outer = tempfile::Builder::new().prefix("outer").tempdir()?;
    let root = outer.path().join("vault");
    fs::create_dir_all(&root)?;
    fs::write(outer.path().join("escape.png"), [1_u8])?;
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: root.clone(),
        managed: true,
        name: Some("vault".to_owned()),
    })?])?;
    let instance = VaultSearchService::try_from(VaultSearchConfig {
        root_id: VaultRootId::try_from(0_usize)?,
        root,
        excludes: vec![".contextos".to_owned()],
        link_excludes: Vec::new(),
        hidden: Vec::new(),
        state_directory: outer.path().join("vault").join(".contextos"),
        text_enabled: false,
        graph_enabled: true,
        graph_backend: GraphBackend::Sqlite,
        semantic: None,
    })?;
    let changed: BTreeSet<String> = ["../escape.png".to_owned()].into();

    let scan = instance.recheck_changed(
        &roots,
        BTreeMap::new(),
        BTreeSet::from(["../escape.png".to_owned()]),
        &changed,
    )?;

    assert!(scan.files.is_empty(), "{:?}", scan.files);
    Ok(())
}

#[test]
fn a_changed_key_that_is_not_a_vault_path_in_its_normal_form_names_no_file() -> TestResult {
    let vault = vault()?;
    let instance = vault.instance()?;
    fs::write(vault.dir.path().join("x.md"), "x\n")?;
    fs::create_dir_all(vault.dir.path().join("sub"))?;
    fs::write(vault.dir.path().join("sub/inner.md"), "i\n")?;
    let changed: BTreeSet<String> = ["./x.md".to_owned(), "sub/".to_owned(), "a//b.md".to_owned()].into();

    let scan = instance.recheck_changed(&vault.roots, BTreeMap::new(), BTreeSet::new(), &changed)?;

    assert!(scan.files.is_empty(), "{:?}", scan.files);
    assert!(scan.notes.is_empty(), "{:?}", scan.notes);
    Ok(())
}

#[test]
fn a_changed_folder_drops_what_the_scan_held_inside_it_and_takes_what_it_holds_now() -> TestResult {
    let vault = vault()?;
    let instance = vault.instance()?;
    fs::create_dir_all(vault.dir.path().join("kept"))?;
    fs::write(vault.dir.path().join("kept/new.png"), [1_u8])?;
    let stale: BTreeSet<String> = [
        "gone/old.md".to_owned(),
        "gone/deeper/older.png".to_owned(),
        "gone.md".to_owned(),
    ]
    .into();
    let changed: BTreeSet<String> = ["gone".to_owned(), "kept".to_owned()].into();

    let scan = instance.recheck_changed(&vault.roots, BTreeMap::new(), stale, &changed)?;

    assert_eq!(scan.files, ["gone.md".to_owned(), "kept/new.png".to_owned()]);
    Ok(())
}

#[test]
fn a_changed_key_naming_a_hidden_path_is_not_taken_into_the_scan() -> TestResult {
    let vault = vault()?;
    let instance = vault.instance_hiding(&["secret/**", "private.png"])?;
    fs::create_dir_all(vault.dir.path().join("secret"))?;
    fs::write(vault.dir.path().join("secret/inside.md"), "s\n")?;
    fs::write(vault.dir.path().join("private.png"), [1_u8])?;
    fs::write(vault.dir.path().join("shown.png"), [1_u8])?;
    let changed: BTreeSet<String> = [
        "secret/inside.md".to_owned(),
        "private.png".to_owned(),
        "shown.png".to_owned(),
    ]
    .into();

    let scan = instance.recheck_changed(&vault.roots, BTreeMap::new(), BTreeSet::new(), &changed)?;

    assert_eq!(scan.files, ["shown.png".to_owned()]);
    assert!(scan.notes.is_empty(), "{:?}", scan.notes);
    Ok(())
}

/// The graph as it stands, with no question to bring it up to date first.
fn graph_as_it_stands(instance: &VaultSearchService) -> Result<GraphView, Box<dyn std::error::Error>> {
    let graph = instance.graph_store()?;
    let view = VaultSearchService::locked(graph.as_ref()).full_view()?;
    Ok(view)
}

#[test]
fn the_first_write_after_the_service_opens_resolves_a_link_to_a_file_the_vault_already_held() -> TestResult {
    let vault = vault()?;
    fs::write(vault.dir.path().join("pic.png"), [1_u8])?;
    let instance = vault.instance()?;

    vault.write(&instance, "b.md", "![[pic.png]]\n")?;

    let view = graph_as_it_stands(&instance)?;
    let reached: Vec<_> = view
        .edges
        .iter()
        .filter(|edge| edge.from == "b.md")
        .filter_map(|edge| view.nodes.iter().find(|node| node.path == edge.to))
        .map(|node| (node.path.as_str(), node.kind))
        .collect();
    assert_eq!(reached, [("pic.png", GraphNodeKind::File)]);
    Ok(())
}
