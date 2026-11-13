//! Folders whose files are not checked as link targets: a link is ignored by the integrity reports when the file it
//! resolves to lies under one, or, when it resolves to no file, when the path it names does. Ignored links are counted
//! apart from the links checked, ambiguous links are reported as before, and the link graph itself is unchanged.

use std::fs;

use contextos_core::{VaultPath, VaultPathInput, VaultRoot, VaultRootId, VaultRootInput, VaultSet};
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

    fn path(&self, relative: &str) -> Result<VaultPath, Box<dyn std::error::Error>> {
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

    fn open(&self, ignore_targets: &[&str]) -> Result<VaultSearchService, Box<dyn std::error::Error>> {
        let service = VaultSearchService::try_from(VaultSearchConfig {
            root_id: VaultRootId::try_from(0_usize)?,
            root: self.dir.path().to_path_buf(),
            excludes: Vec::new(),
            link_excludes: Vec::new(),
            link_ignore_targets: ignore_targets.iter().map(|folder| (*folder).to_owned()).collect(),
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
fn a_link_naming_a_missing_file_under_an_ignored_folder_is_ignored_and_counted_apart() -> TestResult {
    let vault = Vault::new()?;
    vault.put("journal/day.md", "[[inbox/gone]]\n[[elsewhere/gone]]\n")?;
    let service = vault.open(&["inbox"])?;

    let report = service.link_integrity()?;

    let targets: Vec<_> = report.unresolved.iter().map(|link| link.target.as_deref()).collect();
    assert_eq!(targets, vec![Some("elsewhere/gone")], "{report:?}");
    assert_eq!(report.checked, 1, "an ignored link is not counted as checked");
    assert_eq!(report.ignored, 1);
    Ok(())
}

#[test]
fn a_resolved_link_into_an_ignored_folder_is_ignored_rather_than_checked() -> TestResult {
    let vault = Vault::new()?;
    vault.put("journal/day.md", "[[kept]]\n[[here]]\n")?;
    vault.put("inbox/kept.md", "x")?;
    vault.put("standards/here.md", "x")?;
    let service = vault.open(&["inbox"])?;

    let report = service.link_integrity()?;

    assert!(report.unresolved.is_empty(), "{report:?}");
    assert_eq!((report.checked, report.ignored), (1, 1));
    Ok(())
}

#[test]
fn an_ambiguous_link_with_a_candidate_in_an_ignored_folder_is_still_reported_with_every_candidate() -> TestResult {
    let vault = Vault::new()?;
    vault.put("a.md", "[[x]]\n")?;
    vault.put("inbox/x.md", "x")?;
    vault.put("standards/x.md", "x")?;
    let service = vault.open(&["inbox"])?;

    let report = service.link_integrity()?;

    assert_eq!(report.unresolved.len(), 1, "{report:?}");
    assert_eq!(report.unresolved[0].kind, UnresolvedKind::Ambiguous);
    assert_eq!(report.unresolved[0].candidates, vec!["inbox/x.md", "standards/x.md"]);
    assert_eq!((report.checked, report.ignored), (1, 0));
    Ok(())
}

#[test]
fn a_bare_name_that_finds_no_file_is_still_reported_since_it_names_no_folder() -> TestResult {
    let vault = Vault::new()?;
    vault.put("a.md", "[[gone]]\n")?;
    let service = vault.open(&["inbox"])?;

    let report = service.link_integrity()?;

    assert_eq!(report.unresolved.len(), 1, "{report:?}");
    assert_eq!(report.unresolved[0].kind, UnresolvedKind::NotFound);
    assert_eq!(report.ignored, 0);
    Ok(())
}

#[test]
fn a_markdown_link_with_a_relative_path_is_read_from_its_notes_folder() -> TestResult {
    let vault = Vault::new()?;
    // `../inbox/gone.md` from `journal/` names `inbox/gone.md`; `inbox/gone.md` from `journal/` names
    // `journal/inbox/gone.md`, which lies under no ignored folder.
    vault.put(
        "journal/day.md",
        "[a](../inbox/gone.md)\n[b](inbox/gone.md)\n[c](/inbox/other.md)\n",
    )?;
    let service = vault.open(&["inbox"])?;

    let report = service.link_integrity()?;

    let texts: Vec<&str> = report.unresolved.iter().map(|link| link.text.as_str()).collect();
    assert_eq!(texts, vec!["[b](inbox/gone.md)"], "{report:?}");
    assert_eq!(report.ignored, 2);
    Ok(())
}

#[test]
fn a_folder_matches_whole_path_segments_only() -> TestResult {
    let vault = Vault::new()?;
    vault.put("a.md", "[[inbox-archive/gone]]\n[[inbox/gone]]\n")?;
    let service = vault.open(&["inbox"])?;

    let report = service.link_integrity()?;

    let targets: Vec<_> = report.unresolved.iter().map(|link| link.target.as_deref()).collect();
    assert_eq!(targets, vec![Some("inbox-archive/gone")], "{report:?}");
    assert_eq!(report.ignored, 1);
    Ok(())
}

#[test]
fn the_unresolved_report_leaves_ignored_links_out_as_the_count_does() -> TestResult {
    let vault = Vault::new()?;
    vault.put("a.md", "[[inbox/gone]]\n[[notes/gone]]\n[[kept/gone]]\n")?;
    let service = vault.open(&["inbox", "notes"])?;

    let links = service.unresolved_links(None)?;

    let targets: Vec<_> = links.iter().map(|link| link.target.as_deref()).collect();
    assert_eq!(targets, vec![Some("kept/gone")]);
    Ok(())
}

#[test]
fn with_no_ignored_folder_every_link_is_checked_as_before() -> TestResult {
    let vault = Vault::new()?;
    vault.put("journal/day.md", "[[inbox/gone]]\n[[kept]]\n")?;
    vault.put("inbox/kept.md", "x")?;
    let service = vault.open(&[])?;

    let report = service.link_integrity()?;

    assert_eq!(report.unresolved.len(), 1, "{report:?}");
    assert_eq!((report.checked, report.ignored), (2, 0));
    Ok(())
}

#[test]
fn the_link_graph_still_holds_an_ignored_link_and_its_target_keeps_its_backlink() -> TestResult {
    let vault = Vault::new()?;
    vault.put("journal/day.md", "[[kept]]\n")?;
    vault.put("inbox/kept.md", "x")?;
    let service = vault.open(&["inbox"])?;

    let incoming = service.incoming_links(&vault.path("inbox/kept.md")?)?;

    let sources: Vec<&str> = incoming.iter().map(|link| link.source.as_str()).collect();
    assert_eq!(sources, vec!["journal/day.md"]);
    Ok(())
}
