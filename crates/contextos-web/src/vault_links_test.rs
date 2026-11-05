use super::*;

fn routes() -> LinkRoutes {
    LinkRoutes::new(
        "harbour",
        RouteConfig {
            notes_dir: "notes".to_owned(),
            wiki_dir: "memory/wiki".to_owned(),
            insights_dir: "memory/insights".to_owned(),
            journal_dir: "journal".to_owned(),
            journal_date_format: "YYYY-MM-DD".to_owned(),
            entity_slugs: vec!["zenith".to_owned(), "vega".to_owned()],
            settings: SettingsRoutes {
                register_dir: "memory/decisions".to_owned(),
                register_base: "memory/decisions.base".to_owned(),
                registry: vec![
                    ("agents".to_owned(), "registry/agents".to_owned()),
                    ("scripts".to_owned(), "registry/scripts".to_owned()),
                ],
            },
        },
    )
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

#[test]
fn a_task_note_routes_to_its_own_screen_by_id_lower_cased() {
    assert_eq!(
        routes().route("memory/tasks/zenith/mcp/rec-007.md", Some("REC-007")),
        Some("/harbour/tasks/rec-007".to_owned())
    );
}

#[test]
fn a_task_note_without_a_known_id_has_no_screen_and_so_no_route() {
    assert_eq!(routes().route("memory/tasks/rec-007.md", None), None);
}

#[test]
fn the_tasks_landing_note_and_other_folders_are_not_task_notes() {
    let routes = routes();

    assert!(routes.is_task_note("memory/tasks/a-1.md"));
    assert!(routes.is_task_note("memory/tasks/deep/a-1.md"));
    assert!(!routes.is_task_note("memory/tasks/index.md"));
    assert!(!routes.is_task_note("memory/tasks/deep/index.md"));
    assert!(!routes.is_task_note("memory/tasks-archive/a-1.md"));
    assert!(!routes.is_task_note("memory/wiki/a-1.md"));
    assert!(!routes.is_task_note("memory/tasks/a-1.png"));
}

#[test]
fn wiki_insights_and_notes_route_by_their_configured_folders() {
    let routes = routes();

    assert_eq!(
        routes.route("memory/wiki/topic/page.md", None),
        Some("/harbour/wiki/topic/page.md".to_owned())
    );
    assert_eq!(
        routes.route("memory/insights/f/first.md", None),
        Some("/harbour/insights/f/first.md".to_owned())
    );
    // The Insights screen shows only a file directly inside a one-character shard.
    for unshown in [
        "memory/insights/first.md",
        "memory/insights/2026/first.md",
        "memory/insights/f/deeper/first.md",
    ] {
        assert_eq!(routes.route(unshown, None), None, "{unshown}");
    }
    assert_eq!(
        routes.route("notes/2026/one.md", None),
        Some("/harbour/notes/2026/one.md".to_owned())
    );
}

#[test]
fn an_entity_folder_routes_to_the_entity_screen_with_its_whole_vault_path() {
    assert_eq!(
        routes().route("zenith/projects/plan.md", None),
        Some("/harbour/entity/zenith/projects/plan.md".to_owned())
    );
}

#[test]
fn an_entity_folder_with_no_file_inside_it_is_not_a_page_and_has_no_route() {
    assert_eq!(routes().route("zenith.md", None), None);
}

#[test]
fn markdown_no_screen_owns_has_no_route_and_a_configured_folder_never_matches_a_lookalike() {
    let routes = routes();

    assert_eq!(routes.route("memory/wiki-old/x.md", None), None);
    assert_eq!(routes.route("projects/a.md", None), None);
    assert_eq!(routes.route("index.md", None), None);
}

#[test]
fn a_file_that_is_not_a_page_is_served_from_its_own_vault_address() {
    let routes = routes();

    assert_eq!(
        routes.route("memory/wiki/logo.png", None),
        Some("/harbour/memory/wiki/logo.png".to_owned())
    );
    assert_eq!(
        routes.route("loose/report.pdf", None),
        Some("/harbour/loose/report.pdf".to_owned())
    );
}

#[test]
fn a_base_canvas_or_diagram_opens_on_the_screen_that_owns_its_folder() {
    let routes = routes();

    assert_eq!(
        routes.route("memory/wiki/topic/plan.base", None),
        Some("/harbour/wiki/topic/plan.base".to_owned())
    );
    assert_eq!(
        routes.route("memory/insights/b/board.canvas", None),
        Some("/harbour/insights/b/board.canvas".to_owned())
    );
    assert_eq!(routes.route("memory/insights/board.canvas", None), None);
    assert_eq!(
        routes.route("zenith/projects/flow.mermaid", None),
        Some("/harbour/entity/zenith/projects/flow.mermaid".to_owned())
    );
    assert_eq!(routes.route("loose/stray.base", None), None);
    assert_eq!(routes.route("journal/2026-09-28.canvas", None), None);
    assert_eq!(routes.route("memory/tasks/board.canvas", None), None);
}

#[test]
fn a_base_a_screen_lists_opens_that_screens_list() {
    let routes = routes();

    assert_eq!(
        routes.route("memory/tasks.base", None),
        Some("/harbour/tasks/".to_owned())
    );
    assert_eq!(
        routes.route("memory/decisions.base", None),
        Some("/harbour/settings/decisions/".to_owned())
    );
}

#[test]
fn a_journal_note_routes_to_the_journal_screen_for_the_date_its_configured_format_names() {
    let routes = routes();

    assert_eq!(
        routes.route("journal/2026-09-28.md", None),
        Some("/harbour/journal/2026-09-28".to_owned())
    );
    assert_eq!(routes.route("journal/not-a-date.md", None), None);
    assert_eq!(routes.route("journal/2026-13-45.md", None), None, "not a real date");
    assert_eq!(routes.route("journal-old/2026-09-28.md", None), None);
}

#[test]
fn a_journal_format_with_date_folders_is_read_from_the_whole_path() {
    let routes = LinkRoutes::new(
        "v",
        RouteConfig {
            journal_dir: "journal".to_owned(),
            journal_date_format: "YYYY/MM/YYYY-MM-DD".to_owned(),
            ..RouteConfig::default()
        },
    );

    assert_eq!(
        routes.route("journal/2026/09/2026-09-28.md", None),
        Some("/v/journal/2026-09-28".to_owned())
    );
    assert_eq!(routes.route("journal/2026-09-28.md", None), None);
}

#[test]
fn the_longest_configured_folder_wins() {
    let routes = LinkRoutes::new(
        "v",
        RouteConfig {
            notes_dir: "memory".to_owned(),
            wiki_dir: "memory/wiki".to_owned(),
            insights_dir: "memory/insights".to_owned(),
            journal_dir: String::new(),
            journal_date_format: String::new(),
            entity_slugs: vec![],
            settings: SettingsRoutes::default(),
        },
    );

    assert_eq!(routes.route("memory/wiki/x.md", None), Some("/v/wiki/x.md".to_owned()));
    assert_eq!(
        routes.route("memory/other/x.md", None),
        Some("/v/notes/other/x.md".to_owned())
    );
}

#[test]
fn a_routed_path_and_a_files_route_are_both_percent_encoded() {
    let routes = routes();

    assert_eq!(
        routes.route("memory/wiki/a page/it's.md", None),
        Some("/harbour/wiki/a%20page/it%27s.md".to_owned())
    );
    assert_eq!(
        routes.route("misc/My Plan #2?.png", None),
        Some("/harbour/misc/My%20Plan%20%232%3F.png".to_owned())
    );
}

// ---------------------------------------------------------------------------
// expand_links target classification
// ---------------------------------------------------------------------------
// Link values in frontmatter
// ---------------------------------------------------------------------------

#[test]
fn a_frontmatter_value_that_is_exactly_one_wikilink_is_read() {
    let link = WikiLinkValue::try_from("  [[mcp/rec-007|REC-007]]  ").ok();

    assert_eq!(
        link,
        Some(WikiLinkValue {
            target: "mcp/rec-007".to_owned(),
            display: Some("REC-007".to_owned())
        })
    );
    assert_eq!(
        WikiLinkValue::try_from("[[rec-007]]").ok(),
        Some(WikiLinkValue {
            target: "rec-007".to_owned(),
            display: None
        })
    );
}

#[test]
fn any_other_value_is_not_a_link() {
    for value in [
        "plain",
        "",
        "[[a]] and [[b]]",
        "![[a]]",
        "see [[a]]",
        "[[a]] later",
        "[a](b)",
    ] {
        assert_eq!(WikiLinkValue::try_from(value), Err(NotAWikilink), "{value:?}");
    }
}

#[test]
fn display_text_is_the_alias_or_else_the_last_segment_of_the_target() {
    let aliased = WikiLinkValue {
        target: "mcp/rec-007".to_owned(),
        display: Some("REC-007".to_owned()),
    };
    let bare = WikiLinkValue {
        target: "deep/inner/one.md".to_owned(),
        display: None,
    };

    assert_eq!(String::from(&aliased), "REC-007");
    assert_eq!(String::from(&bare), "one");
}

#[test]
fn a_managed_index_note_has_no_screen_even_inside_a_screens_folder() {
    let routes = routes();

    for path in [
        "zenith/projects/index.md",
        "zenith/index.md",
        "memory/wiki/index.md",
        "memory/wiki/topic/index.md",
        "notes/2026/index.md",
    ] {
        assert_eq!(routes.route(path, None), None, "{path}");
    }
}

#[test]
fn a_register_record_opens_on_its_vault_settings_page() {
    assert_eq!(
        routes().route("memory/decisions/vd-001.md", None),
        Some("/harbour/settings/decisions/vd-001".to_owned())
    );
    // A record sits directly in the register's folder; a note beneath it is not one.
    assert_eq!(routes().route("memory/decisions/archive/vd-000.md", None), None);
}

#[test]
fn standards_templates_registry_and_activity_open_on_their_vault_settings_pages() {
    let routes = routes();

    assert_eq!(
        routes.route("standards/console-ui/rust.md", None),
        Some("/harbour/settings/standards/console-ui/rust.md".to_owned())
    );
    assert_eq!(
        routes.route("templates/task.md", None),
        Some("/harbour/settings/templates/task.md".to_owned())
    );
    assert_eq!(
        routes.route("registry/agents/critic.md", None),
        Some("/harbour/settings/registry/agents/critic.md".to_owned())
    );
    assert_eq!(
        routes.route("registry/scripts/tidy.py", None),
        Some("/harbour/settings/registry/scripts/tidy.py".to_owned())
    );
    assert_eq!(
        routes.route("memory/log/2026/10/2026-10-09.md", None),
        Some("/harbour/settings/activity/log/2026/10/2026-10-09.md".to_owned())
    );
    assert_eq!(
        routes.route("memory/sessions/2026-10-09-review.md", None),
        Some("/harbour/settings/activity/sessions/2026-10-09-review.md".to_owned())
    );
}

#[test]
fn a_managed_index_note_has_no_route_anywhere() {
    let routes = routes();

    for path in [
        "index.md",
        "memory/decisions/index.md",
        "standards/index.md",
        "registry/agents/index.md",
        "memory/log/index.md",
        "loose/index.md",
    ] {
        assert_eq!(routes.route(path, None), None, "{path}");
    }
}

#[test]
fn a_folder_a_section_owns_opens_in_that_section() {
    let routes = routes();

    assert_eq!(
        routes.folder_route("zenith/projects"),
        Some("/harbour/entity/zenith/projects".to_owned())
    );
    assert_eq!(
        routes.folder_route("memory/wiki/topic"),
        Some("/harbour/wiki/".to_owned())
    );
    assert_eq!(routes.folder_route("notes/2026"), Some("/harbour/notes/".to_owned()));
    assert_eq!(
        routes.folder_route("journal/2026"),
        Some("/harbour/journal/".to_owned())
    );
    assert_eq!(
        routes.folder_route("memory/tasks/zenith"),
        Some("/harbour/tasks/".to_owned())
    );
    assert_eq!(
        routes.folder_route("memory/decisions"),
        Some("/harbour/settings/decisions/".to_owned())
    );
    assert_eq!(
        routes.folder_route("standards/console-ui"),
        Some("/harbour/settings/standards/console-ui".to_owned())
    );
    assert_eq!(
        routes.folder_route("registry/agents"),
        Some("/harbour/settings/registry/agents".to_owned())
    );
    assert_eq!(
        routes.folder_route("memory/log/2026"),
        Some("/harbour/settings/activity/log".to_owned())
    );
}

#[test]
fn a_folder_no_section_owns_has_no_route() {
    assert_eq!(routes().folder_route("projects/alpha"), None);
    assert_eq!(routes().folder_route("memory"), None);
}
