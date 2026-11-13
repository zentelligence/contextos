//! Link integrity over a real vault: the links that resolve to nothing or to several files, the notes no other note
//! links to, and the folders whose notes are not checked as link sources.

use std::fs;

use contextos_core::{VaultPath, VaultPathInput, VaultRoot, VaultRootId, VaultRootInput, VaultSet};
use contextos_obsidian::LinkForm;
use contextos_search::{GraphBackend, UnresolvedKind, VaultSearchConfig, VaultSearchService};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Vault {
    dir: tempfile::TempDir,
}

impl Vault {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            dir: tempfile::Builder::new().prefix("vault").tempdir()?,
        })
    }

    fn put(&self, relative: &str, content: &str) -> TestResult {
        let path = self.dir.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)?;
        Ok(())
    }

    /// The validated path of a folder of this vault.
    fn folder(&self, relative: &str) -> Result<VaultPath, Box<dyn std::error::Error>> {
        let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
            path: self.dir.path().to_path_buf(),
            managed: true,
            name: Some("vault".to_owned()),
        })?])?;
        Ok(VaultPath::try_from(VaultPathInput {
            roots: &roots,
            raw: relative,
        })?)
    }

    fn open(
        &self,
        link_excludes: &[&str],
        excludes: &[&str],
    ) -> Result<VaultSearchService, Box<dyn std::error::Error>> {
        let service = VaultSearchService::try_from(VaultSearchConfig {
            root_id: VaultRootId::try_from(0_usize)?,
            root: self.dir.path().to_path_buf(),
            excludes: excludes.iter().map(|folder| (*folder).to_owned()).collect(),
            link_excludes: link_excludes.iter().map(|folder| (*folder).to_owned()).collect(),
            link_ignore_targets: Vec::new(),
            hidden: Vec::new(),
            state_directory: self.dir.path().join(".contextos"),
            text_enabled: false,
            graph_enabled: true,
            graph_backend: GraphBackend::Sqlite,
            semantic: None,
        })?;
        service.rebuild(contextos_search::RebuildTarget::Graph)?;
        Ok(service)
    }
}

#[test]
fn each_broken_link_form_is_reported_once_with_its_line_text_target_and_kind() -> TestResult {
    let vault = Vault::new()?;
    vault.put(
        "a.md",
        "---\nrelated: [\"[[gone-fm]]\"]\n---\n[[gone]]\n![[gone.png]]\n[x](gone.md)\n![y](gone.png)\n",
    )?;
    let service = vault.open(&[], &[])?;

    let links = service.unresolved_links(None)?;

    let seen: Vec<_> = links
        .iter()
        .map(|link| {
            (
                link.line,
                link.text.as_str(),
                link.target.as_deref(),
                link.form,
                link.kind,
            )
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            (
                Some(2),
                "[[gone-fm]]",
                Some("gone-fm"),
                Some(LinkForm::Wikilink),
                UnresolvedKind::NotFound
            ),
            (
                Some(4),
                "[[gone]]",
                Some("gone"),
                Some(LinkForm::Wikilink),
                UnresolvedKind::NotFound
            ),
            (
                Some(5),
                "![[gone.png]]",
                Some("gone.png"),
                Some(LinkForm::WikilinkEmbed),
                UnresolvedKind::NotFound
            ),
            (
                Some(6),
                "[x](gone.md)",
                Some("gone.md"),
                Some(LinkForm::Markdown),
                UnresolvedKind::NotFound
            ),
            (
                Some(7),
                "![y](gone.png)",
                Some("gone.png"),
                Some(LinkForm::MarkdownEmbed),
                UnresolvedKind::NotFound
            ),
        ]
    );
    Ok(())
}

#[test]
fn a_name_two_files_answer_to_is_ambiguous_with_both_candidates_and_a_single_file_resolves() -> TestResult {
    let vault = Vault::new()?;
    vault.put("a.md", "[[status]] and [[solo]]\n")?;
    vault.put("one/status.md", "x")?;
    vault.put("two/status.md", "x")?;
    vault.put("one/solo.md", "x")?;
    let service = vault.open(&[], &[])?;

    let links = service.unresolved_links(None)?;

    assert_eq!(links.len(), 1);
    assert_eq!(links[0].kind, UnresolvedKind::Ambiguous);
    assert_eq!(links[0].candidates, vec!["one/status.md", "two/status.md"]);
    Ok(())
}

#[test]
fn links_to_attachments_and_to_notes_in_search_excluded_folders_resolve() -> TestResult {
    let vault = Vault::new()?;
    vault.put("a.md", "![[diagram.png]] [r](report.pdf) [[hidden-note]]\n")?;
    vault.put("assets/diagram.png", "x")?;
    vault.put("assets/report.pdf", "x")?;
    vault.put("private/hidden-note.md", "x")?;
    let service = vault.open(&[], &["private"])?;

    assert_eq!(service.unresolved_links(None)?.len(), 0);
    Ok(())
}

#[test]
fn placeholders_and_links_in_code_or_comments_are_never_reported() -> TestResult {
    let vault = Vault::new()?;
    vault.put(
        "a.md",
        "[[{{slug}}]] [[<slug>]]\n`[[inline]]`\n```\n[[fenced]]\n```\n%% [[commented]] %%\n",
    )?;
    let service = vault.open(&[], &[])?;

    assert_eq!(service.unresolved_links(None)?.len(), 0);
    Ok(())
}

#[test]
fn a_note_whose_links_cannot_be_parsed_is_listed_once_as_unreadable_with_the_reason() -> TestResult {
    let vault = Vault::new()?;
    vault.put("bad.md", "fine\nthen [[unclosed\n")?;
    let service = vault.open(&[], &[])?;

    let links = service.unresolved_links(None)?;

    assert_eq!(links.len(), 1);
    assert_eq!(links[0].kind, UnresolvedKind::Unreadable);
    assert_eq!(links[0].source, "bad.md");
    assert_eq!(links[0].line, None);
    assert_eq!(links[0].target, None);
    assert_eq!(links[0].form, None);
    assert_ne!(links[0].text, "");
    Ok(())
}

#[test]
fn sources_in_link_excluded_folders_are_skipped_but_links_into_them_still_resolve() -> TestResult {
    let vault = Vault::new()?;
    vault.put("templates/t.md", "[[nowhere]]\n")?;
    vault.put("a.md", "[[t]] and [[nowhere2]]\n")?;
    let service = vault.open(&["templates"], &[])?;

    let links = service.unresolved_links(None)?;

    assert_eq!(links.len(), 1);
    assert_eq!(links[0].source, "a.md");
    assert_eq!(links[0].target.as_deref(), Some("nowhere2"));
    Ok(())
}

#[test]
fn the_folder_scope_limits_unresolved_links_to_sources_inside_it() -> TestResult {
    let vault = Vault::new()?;
    vault.put("wiki/a.md", "[[x1]]\n")?;
    vault.put("wikis/b.md", "[[x2]]\n")?;
    let service = vault.open(&[], &[])?;

    let links = service.unresolved_links(Some(&vault.folder("wiki")?))?;

    assert_eq!(
        links.iter().map(|link| link.source.as_str()).collect::<Vec<_>>(),
        vec!["wiki/a.md"]
    );
    Ok(())
}

#[test]
fn links_are_sorted_by_source_then_line() -> TestResult {
    let vault = Vault::new()?;
    vault.put("b.md", "[[x]]\n\n[[y]]\n")?;
    vault.put("a.md", "[[z]]\n")?;
    let service = vault.open(&[], &[])?;

    let order: Vec<_> = service
        .unresolved_links(None)?
        .into_iter()
        .map(|link| (link.source, link.line))
        .collect();

    assert_eq!(
        order,
        vec![
            ("a.md".to_owned(), Some(1)),
            ("b.md".to_owned(), Some(1)),
            ("b.md".to_owned(), Some(3))
        ]
    );
    Ok(())
}

#[test]
fn an_edit_made_outside_the_server_is_seen_by_the_next_question() -> TestResult {
    let vault = Vault::new()?;
    vault.put("a.md", "[[b]]\n")?;
    vault.put("b.md", "x")?;
    let service = vault.open(&[], &[])?;
    assert_eq!(service.unresolved_links(None)?.len(), 0);

    fs::remove_file(vault.dir.path().join("b.md"))?;

    assert_eq!(service.unresolved_links(None)?.len(), 1);
    Ok(())
}

#[test]
fn the_integrity_report_counts_every_checked_link_resolved_or_not() -> TestResult {
    let vault = Vault::new()?;
    vault.put("a.md", "[[b]] [[gone]] [[{{p}}]]\n")?;
    vault.put("b.md", "x")?;
    let service = vault.open(&[], &[])?;

    let report = service.link_integrity()?;

    assert_eq!(report.checked, 2);
    assert_eq!(report.unresolved.len(), 1);
    Ok(())
}

#[test]
fn a_note_only_a_markdown_link_reaches_is_not_unlinked() -> TestResult {
    let vault = Vault::new()?;
    vault.put("a.md", "[b](b.md)\n")?;
    vault.put("b.md", "x")?;
    let service = vault.open(&[], &[])?;

    let unlinked: Vec<_> = service
        .unlinked_notes(None)?
        .into_iter()
        .map(|note| note.path)
        .collect();

    assert_eq!(unlinked, vec!["a.md"]);
    Ok(())
}

#[test]
fn index_files_and_excluded_folders_are_never_unlinked_and_excluded_sources_do_not_count() -> TestResult {
    let vault = Vault::new()?;
    vault.put("index.md", "x")?;
    vault.put("sub/index.md", "x")?;
    vault.put("templates/t.md", "[[c]]\n")?;
    vault.put("c.md", "x")?;
    vault.put("d.md", "[[d]] self link\n")?;
    let service = vault.open(&["templates"], &[])?;

    let unlinked: Vec<_> = service
        .unlinked_notes(None)?
        .into_iter()
        .map(|note| note.path)
        .collect();

    assert_eq!(unlinked, vec!["c.md", "d.md"]);
    Ok(())
}

#[test]
fn an_unlinked_note_with_outgoing_links_is_still_unlinked_and_scope_limits_the_result() -> TestResult {
    let vault = Vault::new()?;
    vault.put("wiki/a.md", "[[wiki/b]]\n")?;
    vault.put("wiki/b.md", "x")?;
    vault.put("other/c.md", "x")?;
    let service = vault.open(&[], &[])?;

    let unlinked: Vec<_> = service
        .unlinked_notes(Some(&vault.folder("wiki")?))?
        .into_iter()
        .map(|note| note.path)
        .collect();

    assert_eq!(unlinked, vec!["wiki/a.md"]);
    Ok(())
}
