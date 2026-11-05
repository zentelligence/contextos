use super::*;

#[test]
fn settings_breadcrumb_has_no_vault_segment() {
    let segments = settings_breadcrumb();
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].label, "settings");
    assert_eq!(segments[0].href, None);
}

#[test]
fn breadcrumb_segments_for_a_vault_root_is_the_dashboard_label_never_the_vault_name() {
    let segments = breadcrumb_segments("vault", "");
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].label, "Dashboard");
    assert_eq!(segments[0].href, None);
}

#[test]
fn breadcrumb_segments_never_repeats_the_vault_name_the_header_switcher_already_shows() {
    let segments = breadcrumb_segments("vault", "docs/guides/note.md");
    let labels: Vec<&str> = segments.iter().map(|segment| segment.label.as_str()).collect();
    assert_eq!(labels, ["docs", "guides", "note.md"]);
    let hrefs: Vec<Option<&str>> = segments.iter().map(|segment| segment.href.as_deref()).collect();
    assert_eq!(hrefs, [Some("/vault/docs/"), Some("/vault/docs/guides/"), None]);
}

fn entity_option(slug: &str, name: &str, accent: u8) -> NavEntityOption {
    NavEntityOption {
        slug: slug.to_owned(),
        name: name.to_owned(),
        accent,
        is_current: false,
    }
}

#[test]
fn general_links_covers_every_mocked_destination_with_its_own_vault_relative_href() {
    let links = general_links("vault", None, &NavCounts::default());
    let hrefs: Vec<&str> = links.iter().map(|link| link.href.as_str()).collect();
    assert_eq!(
        hrefs,
        [
            "/vault/",
            "/vault/tasks/",
            "/vault/signals/",
            "/vault/inbox/",
            "/vault/outbox/",
            "/vault/journal/",
            "/vault/notes/",
        ]
    );
}

#[test]
fn general_links_marks_the_dashboard_link_active_only_at_the_vault_root() {
    let at_root = general_links("vault", Some(""), &NavCounts::default());
    let elsewhere = general_links("vault", Some("docs/guide.md"), &NavCounts::default());
    assert!(at_root[0].active);
    assert!(!elsewhere[0].active);
}

#[test]
fn general_links_marks_a_destination_active_only_on_an_exact_path_match() {
    let on_journal = general_links("vault", Some("journal"), &NavCounts::default());
    let nested_under_journal = general_links("vault", Some("journal/2026/09/2026-09-12.md"), &NavCounts::default());
    let journal_index = |links: &[SidebarLink]| {
        links
            .iter()
            .find(|link| link.label == "Journal")
            .is_some_and(|link| link.active)
    };
    assert!(journal_index(&on_journal));
    assert!(
        !journal_index(&nested_under_journal),
        "a file nested under journal/ must not mark the Journal link active"
    );
}

fn general_meta(counts: &NavCounts) -> std::collections::HashMap<String, Option<String>> {
    general_links("vault", None, counts)
        .into_iter()
        .map(|link| (link.label, link.meta))
        .collect()
}

#[test]
fn general_links_show_each_destinations_own_count() {
    let counts = NavCounts {
        tasks: Some(7),
        inbox: Some(3),
        outbox: Some(11),
        ..NavCounts::default()
    };
    let meta = general_meta(&counts);
    assert_eq!(meta["Tasks"].as_deref(), Some("7"));
    assert_eq!(meta["Inbox"].as_deref(), Some("3"));
    assert_eq!(meta["Outbox"].as_deref(), Some("11"));
}

#[test]
fn a_count_that_could_not_be_read_shows_no_badge_rather_than_a_zero() {
    let meta = general_meta(&NavCounts::default());
    assert_eq!(meta["Tasks"], None);
    assert_eq!(meta["Inbox"], None);
    assert_eq!(meta["Outbox"], None);
}

#[test]
fn signals_count_what_needs_attention_and_destinations_with_no_count_show_none() {
    let none = general_meta(&NavCounts::default());
    assert_eq!(none["Signals"], None, "events that could not be read show no badge");
    for label in ["Dashboard", "Journal", "Notes"] {
        assert_eq!(none[label], None, "{label}");
    }
    let counted = general_meta(&NavCounts {
        signals: Some(3),
        ..NavCounts::default()
    });
    assert_eq!(counted["Signals"].as_deref(), Some("3"));
}

#[test]
fn signals_show_an_attention_dot_only_while_something_needs_attention() {
    let attention = |signals| {
        general_links(
            "vault",
            None,
            &NavCounts {
                signals,
                ..NavCounts::default()
            },
        )
        .into_iter()
        .find(|link| link.label == "Signals")
        .and_then(|link| link.attention)
    };

    assert_eq!(attention(Some(2)), Some("attention"));
    assert_eq!(attention(Some(0)), None);
    assert_eq!(attention(None), None);
}

#[test]
fn signals_is_marked_active_on_its_own_screen() {
    let links = general_links("vault", Some("signals"), &NavCounts::default());

    let active: Vec<&str> = links
        .iter()
        .filter(|link| link.active)
        .map(|link| link.label.as_str())
        .collect();
    assert_eq!(active, ["Signals"]);
}

#[test]
fn agent_group_links_show_each_filters_own_count() {
    let counts = NavCounts {
        agents_general: Some(3),
        agents_business: Some(4),
        agents_personal: Some(0),
        agents_blocked: Some(2),
        ..NavCounts::default()
    };
    let metas: Vec<Option<String>> = agent_group_links("vault", &counts)
        .into_iter()
        .map(|link| link.meta)
        .collect();
    assert_eq!(
        metas,
        [
            Some("3".to_owned()),
            Some("4".to_owned()),
            Some("0".to_owned()),
            Some("2".to_owned())
        ]
    );
}

#[test]
fn agent_group_links_covers_general_business_personal_and_blocked() {
    let links = agent_group_links("vault", &NavCounts::default());
    let hrefs: Vec<&str> = links.iter().map(|link| link.href.as_str()).collect();
    assert_eq!(
        hrefs,
        [
            "/vault/agents/?team=general",
            "/vault/agents/?team=business",
            "/vault/agents/?team=personal",
            "/vault/agents/?state=blocked",
        ]
    );
    let labels: Vec<&str> = links.iter().map(|link| link.label.as_str()).collect();
    assert_eq!(labels, ["General", "Business", "Personal", "Blocked"]);
}

#[test]
fn knowledge_links_always_lists_every_configured_entity_regardless_of_the_current_path() {
    let entities = vec![
        entity_option("zenith", "Zenith", 1),
        entity_option("personal", "Personal", 2),
    ];
    let links = knowledge_links("vault", &entities, Some("entity/personal"));
    let entity_links: Vec<&str> = links[2..].iter().map(|link| link.label.as_str()).collect();
    assert_eq!(
        entity_links,
        ["Zenith", "Personal"],
        "viewing one entity's own folder must never hide the others from the nav menu"
    );
}

#[test]
fn knowledge_links_degrades_to_wiki_and_insights_only_when_the_vault_has_no_entities() {
    let links = knowledge_links("vault", &[], None);
    assert_eq!(links.len(), 2);
}

#[test]
fn knowledge_links_marks_wiki_active_only_under_its_own_tree() {
    let links = knowledge_links("vault", &[], Some("wiki/finance/policy.md"));
    assert!(links[0].active, "Wiki");
    assert!(!links[1].active, "Insights");
}

#[test]
fn knowledge_links_marks_insights_active_only_under_its_own_tree() {
    let links = knowledge_links("vault", &[], Some("insights/a/note.md"));
    assert!(!links[0].active, "Wiki");
    assert!(links[1].active, "Insights");
}

#[test]
fn knowledge_links_marks_only_the_open_entitys_own_link_active() {
    let entities = vec![
        entity_option("zenith", "Zenith", 1),
        entity_option("personal", "Personal", 2),
    ];
    let links = knowledge_links("vault", &entities, Some("entity/personal/notes.md"));
    assert!(!links[2].active, "zenith");
    assert!(links[3].active, "personal");
}

#[test]
fn knowledge_links_marks_an_entitys_own_link_active_from_its_real_top_level_directory_too() {
    let entities = vec![entity_option("zenith", "Zenith", 1)];
    let links = knowledge_links("vault", &entities, Some("zenith/finance/overview.md"));
    assert!(links[2].active);
}

#[test]
fn is_under_knowledge_matches_the_wiki_and_insights_trees() {
    assert!(is_under_knowledge(Some("wiki/finance/policy.md"), &[]));
    assert!(is_under_knowledge(Some("insights/a/note.md"), &[]));
    assert!(!is_under_knowledge(Some("inbox/item.md"), &[]));
}

#[test]
fn a_folder_that_only_starts_with_a_knowledge_screens_name_is_not_under_it() {
    assert!(!is_under_knowledge(Some("wiki-drafts/page.md"), &[]));
    assert!(!is_under_knowledge(Some("insightsful/a.md"), &[]));
    let links = knowledge_links("vault", &[], Some("wiki-drafts/page.md"));
    assert!(links.iter().all(|link| !link.active));
}

#[test]
fn is_under_knowledge_matches_the_entity_folder_screen() {
    assert!(is_under_knowledge(Some("entity"), &[]));
}

#[test]
fn is_under_knowledge_matches_a_configured_entitys_own_directory() {
    let entities = vec![entity_option("zenith", "Zenith", 1)];
    assert!(is_under_knowledge(Some("zenith/notes.md"), &entities));
    assert!(is_under_knowledge(Some("zenith"), &entities));
    assert!(!is_under_knowledge(Some("other/notes.md"), &entities));
}

#[test]
fn sidebar_groups_is_empty_on_a_vault_independent_page() {
    assert!(
        sidebar_groups(
            None,
            None,
            ActiveScreen::Settings,
            &[],
            Vec::new(),
            &NavCounts::default()
        )
        .is_empty()
    );
}

#[test]
fn sidebar_groups_builds_the_five_documented_sections_in_order() {
    let groups = sidebar_groups(
        Some("vault"),
        None,
        ActiveScreen::Vault,
        &[],
        Vec::new(),
        &NavCounts::default(),
    );
    let labels: Vec<Option<&str>> = groups.iter().map(|group| group.label).collect();
    assert_eq!(
        labels,
        [None, Some("Agents"), Some("Apps"), Some("Workflows"), Some("Knowledge")]
    );
}

/// The title bar shows the icon the sidebar shows for the same destination, so the two must never drift: every
/// favouritable view is reachable from the sidebar, and each sidebar entry that leads to one carries its icon.
#[test]
fn each_view_shows_the_same_icon_in_the_sidebar_and_in_its_title_bar() -> Result<(), Box<dyn std::error::Error>> {
    let entity = NavEntityOption {
        slug: "acme".to_owned(),
        name: "Acme".to_owned(),
        accent: 1,
        is_current: false,
    };
    let groups = sidebar_groups(
        Some("vault"),
        None,
        ActiveScreen::Vault,
        &[entity],
        Vec::new(),
        &NavCounts::default(),
    );
    let mut seen = std::collections::BTreeSet::new();
    for group in &groups {
        let headings = group.heading_href.iter().map(|href| (href.as_str(), group.icon));
        let links = group.links.iter().map(|link| (link.href.as_str(), link.icon));
        for (href, icon) in headings.chain(links).filter(|(href, _)| !href.is_empty()) {
            let relative = href.strip_prefix("/vault/").ok_or("sidebar href outside the vault")?;
            if let Some(view) = crate::view_favourites::VIEWS.iter().find(|view| view.path == relative) {
                assert_eq!(icon, view.icon, "{}", view.id);
                seen.insert(view.id);
            }
        }
    }
    let all: std::collections::BTreeSet<&str> = crate::view_favourites::VIEWS.iter().map(|view| view.id).collect();
    assert_eq!(seen, all, "every view must be reachable from the sidebar");
    Ok(())
}

#[test]
fn sidebar_groups_marks_the_matching_heading_active_for_agents_apps_and_jobs() {
    let groups = sidebar_groups(
        Some("vault"),
        None,
        ActiveScreen::Jobs,
        &[],
        Vec::new(),
        &NavCounts::default(),
    );
    let active_labels: Vec<Option<&str>> = groups
        .iter()
        .filter(|group| group.active)
        .map(|group| group.label)
        .collect();
    assert_eq!(active_labels, [Some("Workflows")]);
}

#[test]
fn sidebar_groups_populates_the_apps_section_from_already_fetched_favourited_links()
-> Result<(), Box<dyn std::error::Error>> {
    let favourited = vec![SidebarLink {
        attention: None,
        label: "Task Register".to_owned(),
        href: "/vault/apps/task-register/".to_owned(),
        icon: "apps",
        active: false,
        meta: None,
        target: None,
    }];
    let groups = sidebar_groups(
        Some("vault"),
        None,
        ActiveScreen::Vault,
        &[],
        favourited,
        &NavCounts::default(),
    );
    let apps_group = groups
        .iter()
        .find(|group| group.label == Some("Apps"))
        .ok_or("Apps group missing")?;
    assert_eq!(apps_group.links.len(), 1);
    assert_eq!(apps_group.links[0].label, "Task Register");
    Ok(())
}

/// Every literal `ic ic-<name>` icon usage in one rendered template's raw
/// source, skipping a templated `ic-{{ ... }}` expression: its actual name
/// comes from whatever Rust value produced it, already covered by scanning
/// this crate's own icon-producing functions directly.
fn icon_names_used_in_template_source(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = source;
    while let Some(pos) = rest.find("ic ic-") {
        rest = &rest[pos + "ic ic-".len()..];
        if rest.starts_with('{') {
            continue;
        }
        let end = rest
            .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
            .unwrap_or(rest.len());
        names.push(rest[..end].to_owned());
    }
    names
}

/// Every icon name a `.ic-<name>{--ic:...}` mask rule defines in the
/// bundled stylesheet.
fn icon_names_defined_in_stylesheet(source: &str) -> std::collections::HashSet<String> {
    source
        .lines()
        .filter_map(|line| line.trim().strip_prefix(".ic-"))
        .filter_map(|rest| rest.split_once('{'))
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// Regression test for a real defect: `activity.svg` and `briefcase.svg`
/// shipped in `static/icons/` with no `.ic-activity`/`.ic-briefcase` mask
/// rule ever added, so the header's live badge and the Agents > Business
/// sidebar link rendered as an unclipped solid `currentColor` square (a
/// "blue box") instead of the icon. Cross-checks every icon name this
/// crate's own sidebar-building functions can produce, plus every literal
/// `ic ic-<name>` usage across every shipped template, against the
/// stylesheet's own defined set, so a future icon addition that forgets its
/// CSS mask rule fails a test rather than shipping a blue box.
#[test]
fn every_icon_the_shell_can_render_has_a_defined_css_mask_rule() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let stylesheet = std::fs::read_to_string(format!("{manifest_dir}/static/contextos-shell.css"))?;
    let defined = icon_names_defined_in_stylesheet(&stylesheet);

    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    let entities = vec![entity_option("zenith", "Zenith", 1)];
    for link in general_links("vault", None, &NavCounts::default()) {
        used.insert(link.icon.to_owned());
    }
    for link in agent_group_links("vault", &NavCounts::default()) {
        used.insert(link.icon.to_owned());
    }
    for link in knowledge_links("vault", &entities, None) {
        used.insert(link.icon.to_owned());
    }
    for group in sidebar_groups(
        Some("vault"),
        None,
        ActiveScreen::Vault,
        &entities,
        Vec::new(),
        &NavCounts::default(),
    ) {
        if !group.icon.is_empty() {
            used.insert(group.icon.to_owned());
        }
        for link in group.links {
            used.insert(link.icon.to_owned());
        }
    }

    let templates_dir = format!("{manifest_dir}/templates");
    for entry in std::fs::read_dir(&templates_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("html") {
            continue;
        }
        let source = std::fs::read_to_string(&path)?;
        used.extend(icon_names_used_in_template_source(&source));
    }

    let missing: Vec<&String> = used.iter().filter(|name| !defined.contains(*name)).collect();
    assert!(
        missing.is_empty(),
        "these icon classes are used but no .ic-<name>{{--ic:...}} rule defines them: {missing:?}"
    );
    Ok(())
}
