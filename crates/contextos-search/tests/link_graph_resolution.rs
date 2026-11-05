//! How the link graph resolves links: one Obsidian-style rule for every link form, against every file in the vault,
//! and kept right as files come and go.

use std::path::PathBuf;

use contextos_obsidian::{LinkCollection, LinkForm};
use contextos_search::{
    GraphBackend, GraphDirection, GraphEdge, GraphNode, GraphNodeKind, LinkGraph, LinkGraphConfig, UnreadableNote,
    VaultScan,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BACKENDS: [GraphBackend; 3] = [GraphBackend::Serde, GraphBackend::Fjall, GraphBackend::Sqlite];

fn open(dir: &tempfile::TempDir, backend: GraphBackend) -> Result<LinkGraph, Box<dyn std::error::Error>> {
    Ok(LinkGraph::try_from(LinkGraphConfig {
        store_directory: PathBuf::from(dir.path()).join(".contextos").join("graph"),
        backend,
        write_wait: contextos_search::DEFAULT_WRITE_WAIT,
    })?)
}

/// Writes `source` as the note at `path`, as the service does on a write.
fn note(graph: &mut LinkGraph, path: &str, source: &str) -> TestResult {
    let links = LinkCollection::try_from(source)?;
    graph.upsert_note(path, path, links.outgoing())?;
    Ok(())
}

fn node(graph: &mut LinkGraph, path: &str) -> Result<Option<GraphNode>, Box<dyn std::error::Error>> {
    Ok(graph.full_view()?.nodes.into_iter().find(|node| node.path == path))
}

fn edges_from(graph: &mut LinkGraph, from: &str) -> Result<Vec<GraphEdge>, Box<dyn std::error::Error>> {
    Ok(graph
        .full_view()?
        .edges
        .into_iter()
        .filter(|edge| edge.from == from)
        .collect())
}

fn destinations(graph: &mut LinkGraph, from: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(edges_from(graph, from)?.into_iter().map(|edge| edge.to).collect())
}

#[test]
fn every_edge_records_its_form_target_line_and_raw_text() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = open(&dir, backend)?;
        for file in ["b.md", "c.md", "d.png", "e.png"] {
            graph.file_added(file)?;
        }
        note(
            &mut graph,
            "a.md",
            "[[b]]\n![[d.png]]\ntext [c](c.md \"Title\")\n![e](e.png)\n",
        )?;

        let edges = edges_from(&mut graph, "a.md")?;

        let facts: Vec<_> = edges
            .iter()
            .map(|edge| {
                (
                    edge.form,
                    edge.line,
                    edge.raw.as_str(),
                    edge.target.as_str(),
                    edge.to.as_str(),
                )
            })
            .collect();
        assert_eq!(
            facts,
            [
                (LinkForm::Wikilink, 1, "[[b]]", "b", "b.md"),
                (LinkForm::Markdown, 3, "[c](c.md \"Title\")", "c.md", "c.md"),
                (LinkForm::WikilinkEmbed, 2, "![[d.png]]", "d.png", "d.png"),
                (LinkForm::MarkdownEmbed, 4, "![e](e.png)", "e.png", "e.png"),
            ],
            "backend {backend:?}"
        );
    }
    Ok(())
}

#[test]
fn the_form_target_line_and_raw_text_survive_a_reopen_of_the_store() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        {
            let mut graph = open(&dir, backend)?;
            graph.file_added("b.md")?;
            graph.rebuild_vault(VaultScan {
                notes: vec![(
                    "a.md".to_owned(),
                    "A".to_owned(),
                    LinkCollection::try_from("x\n[b](b.md#Head) ![[ghost]]\n")?
                        .outgoing()
                        .to_vec(),
                )],
                unreadable: vec![],
                files: vec!["a.md".to_owned(), "b.md".to_owned()],
                ..VaultScan::default()
            })?;
        }

        let mut reopened = open(&dir, backend)?;

        assert!(!reopened.needs_rebuild(), "backend {backend:?}");
        let facts: Vec<_> = edges_from(&mut reopened, "a.md")?
            .into_iter()
            .map(|edge| (edge.form, edge.line, edge.raw, edge.target, edge.to))
            .collect();
        assert_eq!(
            facts,
            [
                (
                    LinkForm::Markdown,
                    2,
                    "[b](b.md#Head)".to_owned(),
                    "b.md".to_owned(),
                    "b.md".to_owned()
                ),
                (
                    LinkForm::WikilinkEmbed,
                    2,
                    "![[ghost]]".to_owned(),
                    "ghost".to_owned(),
                    "ghost".to_owned()
                ),
            ],
            "backend {backend:?}"
        );
        assert_eq!(
            node(&mut reopened, "ghost")?.map(|node| node.kind),
            Some(GraphNodeKind::NotFound),
            "backend {backend:?}"
        );
    }
    Ok(())
}

#[test]
fn links_to_attachments_canvases_bases_and_excluded_notes_resolve_without_becoming_phantoms() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = open(&dir, backend)?;
        for file in [
            "assets/photo.png",
            "docs/spec.pdf",
            "boards/plan.canvas",
            "registry/decisions.base",
            "templates/daily.md",
        ] {
            graph.file_added(file)?;
        }
        note(
            &mut graph,
            "a.md",
            "![[photo.png]] [[spec.pdf]] [[plan.canvas]] [[decisions.base]] [daily](templates/daily.md)\n",
        )?;

        assert_eq!(
            destinations(&mut graph, "a.md")?,
            [
                "assets/photo.png",
                "boards/plan.canvas",
                "docs/spec.pdf",
                "registry/decisions.base",
                "templates/daily.md",
            ],
            "backend {backend:?}"
        );
        for path in ["assets/photo.png", "templates/daily.md", "boards/plan.canvas"] {
            assert_eq!(
                node(&mut graph, path)?.map(|node| node.kind),
                Some(GraphNodeKind::File),
                "backend {backend:?}: {path}"
            );
        }
        assert!(graph.unresolved_targets("a.md")?.is_empty(), "backend {backend:?}");
    }
    Ok(())
}

#[test]
fn a_target_nothing_answers_to_is_a_phantom_that_remembers_it_was_not_found() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = open(&dir, backend)?;
        note(&mut graph, "a.md", "[[ghost]] [gone](missing/page.md)\n")?;

        let ghost = node(&mut graph, "ghost")?;
        let gone = node(&mut graph, "missing/page.md")?;

        assert_eq!(
            ghost.as_ref().map(|node| node.kind),
            Some(GraphNodeKind::NotFound),
            "{backend:?}"
        );
        assert!(
            ghost.is_some_and(|node| node.phantom() && node.candidates.is_empty()),
            "{backend:?}"
        );
        assert_eq!(gone.map(|node| node.kind), Some(GraphNodeKind::NotFound), "{backend:?}");
        assert_eq!(
            graph.unresolved_targets("a.md")?,
            ["ghost", "missing/page.md"],
            "backend {backend:?}"
        );
    }
    Ok(())
}

#[test]
fn a_link_resolves_to_the_nearest_match_and_a_name_with_no_nearest_match_is_ambiguous_with_its_candidates() -> TestResult
{
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = open(&dir, backend)?;
        graph.upsert_note("one/x.md", "x", &[])?;
        graph.upsert_note("two/x.md", "x", &[])?;
        note(&mut graph, "one/sub/a.md", "[[x]]\n")?;
        note(&mut graph, "three/b.md", "[[x]]\n")?;

        assert_eq!(
            destinations(&mut graph, "one/sub/a.md")?,
            ["one/x.md"],
            "backend {backend:?}"
        );
        assert_eq!(destinations(&mut graph, "three/b.md")?, ["x"], "backend {backend:?}");
        let ambiguous = node(&mut graph, "x")?;
        assert_eq!(
            ambiguous.as_ref().map(|node| node.kind),
            Some(GraphNodeKind::Ambiguous),
            "{backend:?}"
        );
        assert_eq!(
            ambiguous.map(|node| node.candidates),
            Some(vec!["one/x.md".to_owned(), "two/x.md".to_owned()]),
            "backend {backend:?}"
        );
        assert_eq!(graph.unresolved_targets("three/b.md")?, ["x"], "backend {backend:?}");
    }
    Ok(())
}

#[test]
fn a_folder_link_reaches_the_index_note_inside_it_and_a_folder_without_one_is_not_found() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = open(&dir, backend)?;
        graph.file_added("raw/2026/index.md")?;
        graph.file_added("raw/2025/a.md")?;
        note(&mut graph, "a.md", "[y](raw/2026/) [z](raw/2025/)\n")?;

        assert_eq!(
            destinations(&mut graph, "a.md")?,
            ["raw/2025/", "raw/2026/index.md"],
            "backend {backend:?}"
        );
        assert_eq!(
            node(&mut graph, "raw/2025/")?.map(|node| node.kind),
            Some(GraphNodeKind::NotFound),
            "backend {backend:?}"
        );
    }
    Ok(())
}

#[test]
fn a_file_node_is_not_an_orphan_and_is_not_reported_by_any_note_oriented_query() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = open(&dir, backend)?;
        graph.file_added("lonely.png")?;
        graph.file_added("used.png")?;
        note(&mut graph, "a.md", "![[used.png]]\n")?;
        graph.upsert_note("orphan.md", "orphan", &[])?;

        let orphans: Vec<String> = graph.orphans()?.nodes.into_iter().map(|node| node.path).collect();

        assert_eq!(orphans, ["orphan.md"], "backend {backend:?}");
        assert!(node(&mut graph, "lonely.png")?.is_none(), "backend {backend:?}");
    }
    Ok(())
}

#[test]
fn a_file_appearing_resolves_a_link_that_was_missing_and_the_phantom_goes() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = open(&dir, backend)?;
        note(&mut graph, "a.md", "![[photo.png]] [[later]]\n")?;
        assert_eq!(destinations(&mut graph, "a.md")?, ["later", "photo.png"], "{backend:?}");

        graph.file_added("assets/photo.png")?;
        graph.upsert_note("notes/later.md", "later", &[])?;

        assert_eq!(
            destinations(&mut graph, "a.md")?,
            ["assets/photo.png", "notes/later.md"],
            "backend {backend:?}"
        );
        assert!(node(&mut graph, "photo.png")?.is_none(), "backend {backend:?}");
        assert!(node(&mut graph, "later")?.is_none(), "backend {backend:?}");
        assert!(graph.unresolved_targets("a.md")?.is_empty(), "backend {backend:?}");
    }
    Ok(())
}

#[test]
fn a_nearer_match_appearing_takes_the_link_from_a_farther_one() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = open(&dir, backend)?;
        graph.upsert_note("far/x.md", "x", &[])?;
        note(&mut graph, "near/deep/a.md", "[[x]]\n")?;
        assert_eq!(destinations(&mut graph, "near/deep/a.md")?, ["far/x.md"], "{backend:?}");

        graph.upsert_note("near/x.md", "x", &[])?;

        assert_eq!(
            destinations(&mut graph, "near/deep/a.md")?,
            ["near/x.md"],
            "backend {backend:?}"
        );
    }
    Ok(())
}

#[test]
fn a_second_file_with_the_same_name_makes_a_link_ambiguous_and_removing_it_makes_it_resolve_again() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = open(&dir, backend)?;
        graph.upsert_note("one/x.md", "x", &[])?;
        note(&mut graph, "z/a.md", "[[x]]\n")?;
        assert_eq!(destinations(&mut graph, "z/a.md")?, ["one/x.md"], "{backend:?}");

        graph.file_added("two/x.md")?;
        assert_eq!(destinations(&mut graph, "z/a.md")?, ["x"], "backend {backend:?}");
        assert_eq!(
            node(&mut graph, "x")?.map(|node| (node.kind, node.candidates)),
            Some((
                GraphNodeKind::Ambiguous,
                vec!["one/x.md".to_owned(), "two/x.md".to_owned()]
            )),
            "backend {backend:?}"
        );

        graph.file_added("three/x.md")?;
        assert_eq!(
            node(&mut graph, "x")?.map(|node| node.candidates.len()),
            Some(3),
            "backend {backend:?}"
        );

        graph.file_removed("two/x.md")?;
        graph.file_removed("three/x.md")?;
        assert_eq!(destinations(&mut graph, "z/a.md")?, ["one/x.md"], "backend {backend:?}");
        assert!(node(&mut graph, "x")?.is_none(), "backend {backend:?}");
    }
    Ok(())
}

#[test]
fn removing_a_linked_file_leaves_its_links_as_not_found_phantoms_keyed_by_what_was_written() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = open(&dir, backend)?;
        graph.file_added("assets/photo.png")?;
        graph.upsert_note("notes/target.md", "target", &[])?;
        note(&mut graph, "a.md", "![[photo.png]] [[target]] [t](notes/target.md)\n")?;

        graph.file_removed("assets/photo.png")?;
        graph.remove_note("notes/target.md")?;

        assert_eq!(
            destinations(&mut graph, "a.md")?,
            ["notes/target.md", "photo.png", "target"],
            "backend {backend:?}"
        );
        assert_eq!(
            graph.unresolved_targets("a.md")?,
            ["notes/target.md", "photo.png", "target"]
        );
        assert!(node(&mut graph, "assets/photo.png")?.is_none(), "backend {backend:?}");
    }
    Ok(())
}

#[test]
fn renaming_a_file_keeps_name_links_working_and_breaks_path_links() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = open(&dir, backend)?;
        graph.upsert_note("old/n.md", "n", &[])?;
        note(&mut graph, "a.md", "[[n]] [p](old/n.md)\n")?;

        graph.remove_note("old/n.md")?;
        graph.upsert_note("new/n.md", "n", &[])?;

        let facts: Vec<_> = edges_from(&mut graph, "a.md")?
            .into_iter()
            .map(|edge| (edge.raw, edge.to))
            .collect();
        assert_eq!(
            facts,
            [
                ("[[n]]".to_owned(), "new/n.md".to_owned()),
                ("[p](old/n.md)".to_owned(), "old/n.md".to_owned())
            ],
            "backend {backend:?}"
        );
        assert_eq!(graph.unresolved_targets("a.md")?, ["old/n.md"], "backend {backend:?}");
    }
    Ok(())
}

#[test]
fn removing_a_folder_removes_every_file_under_it_and_re_resolves_what_linked_to_them() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = open(&dir, backend)?;
        graph.file_added("dir/pic.png")?;
        graph.upsert_note("dir/inner.md", "inner", &[])?;
        graph.file_added("dir2/keep.png")?;
        note(&mut graph, "a.md", "![[pic.png]] [[inner]] ![[keep.png]]\n")?;

        graph.file_removed("dir")?;

        assert_eq!(
            destinations(&mut graph, "a.md")?,
            ["dir2/keep.png", "inner", "pic.png"],
            "backend {backend:?}"
        );
        assert!(node(&mut graph, "dir/inner.md")?.is_none(), "backend {backend:?}");
    }
    Ok(())
}

#[test]
fn a_note_whose_links_cannot_be_read_is_recorded_with_the_reason_and_keeps_no_stale_edges() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = open(&dir, backend)?;
        graph.upsert_note("b.md", "b", &[])?;
        note(&mut graph, "a.md", "[[b]]\n")?;

        graph.upsert_unreadable_note("a.md", "a", "wikilink on line 3 has no closing brackets")?;

        let recorded = node(&mut graph, "a.md")?;
        assert_eq!(
            recorded.as_ref().and_then(|node| node.unreadable.as_deref()),
            Some("wikilink on line 3 has no closing brackets"),
            "backend {backend:?}"
        );
        assert_eq!(recorded.map(|node| node.kind), Some(GraphNodeKind::Note), "{backend:?}");
        assert!(edges_from(&mut graph, "a.md")?.is_empty(), "backend {backend:?}");
        let orphans: Vec<String> = graph.orphans()?.nodes.into_iter().map(|node| node.path).collect();
        assert_eq!(
            orphans,
            ["b.md"],
            "an unreadable note is not claimed to be an orphan; {backend:?}"
        );

        note(&mut graph, "a.md", "[[b]]\n")?;
        assert_eq!(
            node(&mut graph, "a.md")?.and_then(|node| node.unreadable),
            None,
            "a readable rewrite clears the record; {backend:?}"
        );
    }
    Ok(())
}

#[test]
fn unreadable_notes_in_a_rebuild_are_recorded_and_their_record_survives_a_reopen() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        {
            let mut graph = open(&dir, backend)?;
            graph.rebuild_vault(VaultScan {
                notes: vec![("ok.md".to_owned(), "ok".to_owned(), vec![])],
                unreadable: vec![UnreadableNote {
                    path: "bad.md".to_owned(),
                    title: "bad".to_owned(),
                    reason: "wikilink on line 1 has no closing brackets".to_owned(),
                }],
                files: vec![],
                ..VaultScan::default()
            })?;
        }

        let mut reopened = open(&dir, backend)?;

        assert_eq!(
            node(&mut reopened, "bad.md")?
                .and_then(|node| node.unreadable)
                .as_deref(),
            Some("wikilink on line 1 has no closing brackets"),
            "backend {backend:?}"
        );
    }
    Ok(())
}

#[test]
fn a_rebuild_resolves_against_every_file_the_scan_lists() -> TestResult {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = open(&dir, backend)?;
        let links = LinkCollection::try_from("![[photo.png]] [[b]] [[missing]]\n")?;

        graph.rebuild_vault(VaultScan {
            notes: vec![
                ("a.md".to_owned(), "a".to_owned(), links.outgoing().to_vec()),
                ("b.md".to_owned(), "b".to_owned(), vec![]),
            ],
            unreadable: vec![],
            files: vec!["a.md".to_owned(), "b.md".to_owned(), "img/photo.png".to_owned()],
            ..VaultScan::default()
        })?;

        assert_eq!(
            destinations(&mut graph, "a.md")?,
            ["b.md", "img/photo.png", "missing"],
            "backend {backend:?}"
        );
        assert_eq!(graph.unresolved_targets("a.md")?, ["missing"], "backend {backend:?}");
    }
    Ok(())
}

#[test]
fn a_placeholder_edge_is_recognisable_from_its_form_and_target() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut graph = open(&dir, GraphBackend::Serde)?;
    note(&mut graph, "t.md", "[[{{title}}]] [[<slug>]] [[real]] [x]({{path}})\n")?;

    let placeholders: Vec<bool> = edges_from(&mut graph, "t.md")?
        .iter()
        .map(GraphEdge::is_placeholder)
        .collect();

    assert_eq!(placeholders, [true, false, true, true]);
    Ok(())
}

#[test]
fn a_file_added_by_one_instance_re_resolves_links_for_another_sharing_the_sqlite_store() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut first = open(&dir, GraphBackend::Sqlite)?;
    let mut second = open(&dir, GraphBackend::Sqlite)?;
    note(&mut first, "a.md", "![[photo.png]]\n")?;
    assert_eq!(destinations(&mut second, "a.md")?, ["photo.png"]);

    first.file_added("img/photo.png")?;

    assert_eq!(destinations(&mut second, "a.md")?, ["img/photo.png"]);
    assert_eq!(second.unresolved_targets("a.md")?, Vec::<String>::new());
    let neighbours = second.neighbours("a.md", 1, GraphDirection::Out)?;
    assert_eq!(neighbours.nodes.iter().filter(|node| node.phantom()).count(), 0);
    Ok(())
}
