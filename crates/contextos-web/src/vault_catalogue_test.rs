use super::*;

fn file(name: &str) -> VaultFile {
    VaultFile {
        name: name.to_owned(),
        relative_path: name.to_owned(),
    }
}

fn folder(name: &str, files: &[&str], folders: Vec<VaultFolder>) -> VaultFolder {
    VaultFolder {
        name: name.to_owned(),
        relative_path: name.to_owned(),
        folders,
        files: files.iter().map(|name| file(name)).collect(),
    }
}

fn empty_catalogue() -> Catalogue {
    Catalogue {
        loaded: LoadedVaultManagementConfig::default(),
        decisions: FolderListing::Missing,
        registry: RegistryKind::ALL
            .into_iter()
            .map(|kind| (kind, FolderListing::Missing))
            .collect(),
        standards: FolderListing::Missing,
        templates: FolderListing::Missing,
        operation_log: FolderListing::Missing,
        sessions: FolderListing::Missing,
    }
}

#[test]
fn only_a_dated_markdown_file_is_a_log_day() {
    assert!(is_log_day(&file("2026-10-07.md")));
    for name in [
        "log.md",
        "2026-08.md",
        "index.md",
        "2026-10-07.txt",
        "2026-1-07.md",
        "20261007.md",
    ] {
        assert!(!is_log_day(&file(name)), "{name}");
    }
}

#[test]
fn a_skill_counts_once_per_folder_that_holds_its_entry_file() {
    let skills = folder(
        "skills",
        &["index.md"],
        vec![
            folder("plan-day", &["SKILL.md", "plan_day.py"], Vec::new()),
            folder("draft", &["notes.md"], Vec::new()),
            folder("synced", &[], vec![folder("nested", &["SKILL.md"], Vec::new())]),
        ],
    );
    assert_eq!(count_definitions(RegistryKind::Skills, &skills), 2);
}

#[test]
fn scripts_count_every_file_but_markdown_definitions_count_only_markdown() {
    let scripts = folder("scripts", &["index.md", "md-to-pdf.py", "fence_blocks.py"], Vec::new());
    assert_eq!(count_definitions(RegistryKind::Scripts, &scripts), 2);
    let agents = folder("agents", &["index.md", "alex.md", "notes.txt"], Vec::new());
    assert_eq!(count_definitions(RegistryKind::Agents, &agents), 1);
}

#[test]
fn an_unprovisioned_vault_shows_only_the_decisions_register() {
    let present: Vec<_> = empty_catalogue()
        .summaries()
        .into_iter()
        .map(|summary| summary.category)
        .collect();
    assert_eq!(present, [ManagementCategory::Decisions]);
}

#[test]
fn one_provisioned_registry_type_is_enough_to_show_the_registry() {
    let mut catalogue = empty_catalogue();
    catalogue.registry[1].1 = FolderListing::Present(folder("hats", &[], Vec::new()));
    assert!(catalogue.is_present(ManagementCategory::Registry));
    assert_eq!(catalogue.count(ManagementCategory::Registry), 0);
}

#[test]
fn an_unreadable_folder_is_still_present_so_its_page_can_say_why() {
    let mut catalogue = empty_catalogue();
    catalogue.standards = FolderListing::Unreadable { code: None };
    assert!(catalogue.is_present(ManagementCategory::Standards));
}

#[test]
fn either_activity_source_is_enough_to_show_the_activity_log() {
    let mut catalogue = empty_catalogue();
    catalogue.sessions = FolderListing::Present(folder("sessions", &[], Vec::new()));
    assert!(catalogue.is_present(ManagementCategory::Activity));
    assert_eq!(catalogue.count(ManagementCategory::Activity), 0);
}

#[test]
fn categories_round_trip_through_their_url_slugs() {
    for category in ManagementCategory::ALL {
        assert_eq!(ManagementCategory::try_from(category.slug()), Ok(category));
    }
    assert!(ManagementCategory::try_from("tasks").is_err());
}
