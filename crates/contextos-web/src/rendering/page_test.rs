use super::*;

fn empty_nav() -> NavData {
    NavData {
        vaults: Vec::new(),
        current_vault: None,
        nav_target_vault: None,
        breadcrumb: vec![BreadcrumbSegment {
            label: "settings".to_owned(),
            href: None,
        }],
        active_vault_screen: false,
        active_agents_screen: false,
        active_apps_screen: false,
        active_jobs_screen: false,
        active_settings_screen: false,
        current_vault_entity: None,
        entities: Vec::new(),
        current_entity: None,
        current_entity_accent: None,
        entity_base_path: None,
        appearance: Appearance::default(),
        sidebar_groups: Vec::new(),
        host_view: None,
        sidebar_wide: true,
        mcp_status: McpStatus::default(),
        note_href: None,
        profile_href: None,
        vault_settings_href: None,
        page_title: None,
        events_stream_href: None,
        favourites: None,
    }
}

fn vault(name: &str, is_current: bool, entity: u8) -> NavVault {
    NavVault {
        name: name.to_owned(),
        is_current,
        entity,
    }
}

fn entity_option(slug: &str, name: &str, accent: u8, is_current: bool) -> NavEntityOption {
    NavEntityOption {
        slug: slug.to_owned(),
        name: name.to_owned(),
        accent,
        is_current,
    }
}

#[test]
fn wraps_the_body_and_carries_the_title() {
    let html = render_page(&empty_nav(), "example.md", "<p>Body.</p>");
    assert!(html.contains("<title>example.md</title>"));
    assert!(html.contains("<p>Body.</p>"));
}

#[test]
fn renders_the_vault_switcher_with_every_configured_vault() {
    let nav = NavData {
        vaults: vec![vault("vault", true, 1), vault("other-vault", false, 2)],
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("<option value=\"vault\" selected>vault</option>"));
    assert!(html.contains("<option value=\"other-vault\">other-vault</option>"));
}

#[test]
fn the_vault_switcher_is_hidden_when_only_one_vault_is_configured() {
    let nav = NavData {
        vaults: vec![vault("vault", true, 1)],
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(!html.contains("vault-switcher"));
}

#[test]
fn the_vault_switcher_is_hidden_when_no_vault_is_configured() {
    let html = render_page(&empty_nav(), "Settings", "<p>Body.</p>");
    assert!(!html.contains("vault-switcher"));
}

#[test]
fn the_current_vault_tints_the_switcher_with_its_entity_accent() {
    let nav = NavData {
        vaults: vec![vault("a", false, 1), vault("b", true, 2)],
        current_vault_entity: Some(2),
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("style=\"--entity: var(--entity-2)\""));
}

#[test]
fn the_top_bar_breadcrumb_links_every_ancestor_but_the_current_page() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        breadcrumb: vec![
            BreadcrumbSegment {
                label: "vault".to_owned(),
                href: Some("/vault/".to_owned()),
            },
            BreadcrumbSegment {
                label: "docs".to_owned(),
                href: Some("/vault/docs/".to_owned()),
            },
            BreadcrumbSegment {
                label: "note.md".to_owned(),
                href: None,
            },
        ],
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("<a href=\"/vault/\">vault</a>"));
    assert!(html.contains("<a href=\"/vault/docs/\">docs</a>"));
    assert!(html.contains("<span class=\"breadcrumb-current\">note.md</span>"));
}

fn sidebar_group(label: Option<&'static str>, href: Option<&str>, active: bool) -> SidebarGroup {
    sidebar_group_with_icon(label, href, active, "apps")
}

fn sidebar_group_with_icon(
    label: Option<&'static str>,
    href: Option<&str>,
    active: bool,
    icon: &'static str,
) -> SidebarGroup {
    SidebarGroup {
        label,
        heading_href: href.map(str::to_owned),
        icon,
        active,
        links: Vec::new(),
    }
}

#[test]
fn marks_the_active_sidebar_group_heading() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        nav_target_vault: Some("vault".to_owned()),
        sidebar_groups: vec![sidebar_group(Some("Apps"), Some("/vault/apps/"), true)],
        ..empty_nav()
    };
    let html = render_page(&nav, "Apps", "<p>Body.</p>");
    assert!(html.contains("class=\"nav-group-heading-link active\" href=\"/vault/apps/\""));
}

#[test]
fn an_inactive_sidebar_group_heading_carries_no_active_class() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        sidebar_groups: vec![sidebar_group(Some("Workflows"), Some("/vault/workflows"), false)],
        ..empty_nav()
    };
    let html = render_page(&nav, "Jobs", "<p>Body.</p>");
    assert!(html.contains("class=\"nav-group-heading-link \" href=\"/vault/workflows\""));
}

#[test]
fn the_general_sections_links_render_with_their_own_icon_and_optional_meta() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        sidebar_groups: vec![SidebarGroup {
            label: None,
            heading_href: None,
            icon: "",
            active: false,
            links: vec![
                SidebarLink {
                    attention: None,
                    label: "Tasks".to_owned(),
                    href: "/vault/tasks/".to_owned(),
                    icon: "list-checks",
                    active: false,
                    meta: Some("0".to_owned()),
                    target: None,
                },
                SidebarLink {
                    attention: None,
                    label: "Notes".to_owned(),
                    href: "/vault/notes/".to_owned(),
                    icon: "file-text",
                    active: true,
                    meta: None,
                    target: None,
                },
            ],
        }],
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("class=\"nav-link \" href=\"/vault/tasks/\""));
    assert!(html.contains("class=\"ic ic-list-checks\""));
    assert!(html.contains("<span class=\"nav-link-meta\">0</span>"));
    assert!(html.contains("class=\"nav-link active\" href=\"/vault/notes/\""));
    assert!(
        !html.contains("nav-group"),
        "the unheaded General section must not render as a <details> group"
    );
    assert!(
        !html.contains("href=\"/vault/tasks/\" target=") && !html.contains("href=\"/vault/notes/\" target="),
        "a sidebar link with no target attribute must render no target attribute at all"
    );
}

#[test]
fn a_sidebar_link_carrying_a_target_renders_it_on_the_anchor() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        sidebar_groups: vec![SidebarGroup {
            label: Some("Apps"),
            heading_href: Some("/vault/apps/".to_owned()),
            icon: "apps",
            active: false,
            links: vec![SidebarLink {
                attention: None,
                label: "Mock Website".to_owned(),
                href: "/vault/apps/mock-website/".to_owned(),
                icon: "apps",
                active: false,
                meta: None,
                target: Some("_blank"),
            }],
        }],
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("href=\"/vault/apps/mock-website/\" target=\"_blank\""));
}

#[test]
fn the_wordmark_links_back_to_the_fallback_vault_from_a_vault_independent_page() {
    let nav = NavData {
        current_vault: None,
        nav_target_vault: Some("vault".to_owned()),
        active_settings_screen: true,
        ..empty_nav()
    };
    let html = render_page(&nav, "Settings", "<p>Body.</p>");
    assert!(html.contains("<a class=\"wordmark\" href=\"/vault/\""));
}

#[test]
fn the_wordmark_is_plain_text_on_a_page_that_already_has_a_current_vault() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        nav_target_vault: Some("vault".to_owned()),
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("<span class=\"wordmark\">"));
}

#[test]
fn header_controls_with_no_vault_target_render_disabled_not_as_dead_links() {
    let html = render_page(&empty_nav(), "Settings", "<p>Body.</p>");
    assert!(html.contains("<span class=\"header-icon-button disabled\""));
    // Settings always stays reachable regardless of vault context.
    assert!(html.contains("href=\"/server/settings/\" aria-label=\"Settings\""));
}

#[test]
fn the_new_note_and_profile_controls_link_to_the_fallback_vault_when_present() {
    let nav = NavData {
        note_href: Some("/vault/notes/".to_owned()),
        profile_href: Some("/vault/profile/".to_owned()),
        ..empty_nav()
    };
    let html = render_page(&nav, "Settings", "<p>Body.</p>");
    assert!(html.contains("href=\"/vault/notes/\" aria-label=\"New note\""));
    assert!(html.contains("href=\"/vault/profile/\" aria-label=\"Profile\""));
}

#[test]
fn the_footer_status_bar_reports_the_mcp_servers_summary() {
    let nav = NavData {
        mcp_status: McpStatus {
            connected: 2,
            total: 3,
            tools: 47,
        },
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("2 of 3 connected"));
    assert!(html.contains("47 tools"));
    assert!(
        html.contains("status-dot attention"),
        "a server that is not answering must not leave the dot green"
    );
}

#[test]
fn the_footer_status_bar_flags_an_error_when_no_server_is_connected() {
    let html = render_page(&empty_nav(), "note.md", "<p>Body.</p>");
    assert!(html.contains("status-dot error"));
}

#[test]
fn the_footer_carries_a_disabled_stream_placeholder_not_invented_live_data() {
    let html = render_page(&empty_nav(), "note.md", "<p>Body.</p>");
    assert!(html.contains("status-bar-item-disabled"));
    assert!(html.contains("Not yet available"));
}

#[test]
fn the_sidebar_carries_the_narrow_class_only_when_collapsed() {
    let wide = render_page(&empty_nav(), "note.md", "<p>Body.</p>");
    assert!(!wide.contains("sidebar-narrow"));

    let narrow_nav = NavData {
        sidebar_wide: false,
        ..empty_nav()
    };
    let narrow = render_page(&narrow_nav, "note.md", "<p>Body.</p>");
    assert!(narrow.contains("class=\"shell sidebar-narrow\""));
}

#[test]
fn the_sidebar_toggle_button_targets_the_shared_toggle_route() {
    let html = render_page(&empty_nav(), "note.md", "<p>Body.</p>");
    assert!(html.contains("action=\"/shell/sidebar-toggle\""));
}

#[test]
fn the_rendered_shell_contains_no_emoji_or_decorative_unicode() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        nav_target_vault: Some("vault".to_owned()),
        sidebar_groups: vec![
            sidebar_group(None, None, false),
            sidebar_group_with_icon(Some("Agents"), Some("/vault/agents/"), false, "agents"),
        ],
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    for ch in html.chars() {
        let cp = ch as u32;
        let is_emoji = (0x1F000..=0x1FAFF).contains(&cp) || (0x2600..=0x27BF).contains(&cp) || cp == 0xFE0F;
        // Arrows (U+2190..U+21FF) and geometric shapes (U+25A0..U+25FF) are
        // decorative Unicode the design kit bans in favour of Lucide marks.
        let is_decorative = (0x2190..=0x21FF).contains(&cp) || (0x25A0..=0x25FF).contains(&cp);
        assert!(!is_emoji, "emoji codepoint U+{cp:04X} in rendered shell");
        assert!(!is_decorative, "decorative Unicode U+{cp:04X} in rendered shell");
    }
    // The glyphs are Lucide masks referenced from the stylesheet.
    assert!(html.contains("class=\"ic ic-settings\""));
    assert!(html.contains("class=\"ic ic-agents\""));
}

#[test]
fn appearance_is_applied_as_html_data_attributes() {
    let nav = NavData {
        appearance: Appearance {
            theme: Some("dark".to_owned()),
            font: Some("serif".to_owned()),
            size: Some("large".to_owned()),
            system_name: "Command Centre".to_owned(),
            logo: "logo.png".to_owned(),
        },
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("<html lang=\"en\" data-theme=\"dark\" data-font=\"serif\" data-size=\"large\">"));
}

#[test]
fn the_top_bar_renders_the_configured_system_name_as_the_wordmark() {
    let nav = NavData {
        appearance: Appearance {
            system_name: "Ops Desk".to_owned(),
            ..Appearance::default()
        },
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("<span class=\"wordmark\">Ops Desk</span>"));
    assert!(!html.contains("shell-header"));
}

#[test]
fn the_logo_image_degrades_silently_when_no_override_file_is_present() {
    let html = render_page(&empty_nav(), "note.md", "<p>Body.</p>");
    assert!(html.contains("src=\"/static/logo.png\""));
    assert!(html.contains("onerror=\"this.remove()\""));
}

#[test]
fn the_wordmark_defaults_to_command_centre() {
    let html = render_page(&empty_nav(), "note.md", "<p>Body.</p>");
    assert!(html.contains("<span class=\"wordmark\">Command Centre</span>"));
}

#[test]
fn the_page_head_links_the_favicon_and_the_stylesheet() {
    let html = render_page(&empty_nav(), "note.md", "<p>Body.</p>");
    assert!(html.contains("<link rel=\"icon\" href=\"/static/favicon.ico\">"));
    assert!(html.contains("<link rel=\"stylesheet\" href=\"/static/contextos-web.css\">"));
}

#[test]
fn no_appearance_set_omits_every_data_attribute() {
    let html = render_page(&empty_nav(), "note.md", "<p>Body.</p>");
    assert!(html.contains("<html lang=\"en\">"));
    assert!(!html.contains("data-theme"));
    assert!(!html.contains("data-font"));
    assert!(!html.contains("data-size"));
}

#[test]
fn the_contextos_web_client_script_loads_before_composed_body_content() -> Result<(), Box<dyn std::error::Error>> {
    // A `kind = "js"` app's own entry document is composed verbatim into
    // `body` (`crate::routes::apps::respond_with_app_content`) and may
    // contain a synchronous `<script>` that calls `window.contextosWeb`
    // at parse time, with no `DOMContentLoaded` guard. Browsers execute
    // non-deferred scripts in document order as the parser reaches them,
    // so `contextos-web-client.js` must appear before `body` in the
    // rendered page or such a script finds `window.contextosWeb`
    // undefined.
    let html = render_page(
        &empty_nav(),
        "Action Manager",
        "<script>window.contextosWeb.callTool();</script>",
    );
    let script_index = html
        .find("/static/contextos-web-client.js")
        .ok_or("client script tag missing")?;
    let body_index = html
        .find("window.contextosWeb.callTool();")
        .ok_or("composed body content missing")?;
    assert!(
        script_index < body_index,
        "contextos-web-client.js must load before composed body content"
    );
    Ok(())
}

#[test]
fn the_entity_switcher_is_hidden_when_the_vault_has_no_configured_entities() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        entity_base_path: Some("/vault/".to_owned()),
        entities: Vec::new(),
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(!html.contains("entity-switcher"));
}

#[test]
fn the_entity_switcher_is_hidden_on_a_vault_independent_page_even_with_entities_present() {
    let nav = NavData {
        entities: vec![entity_option("zenith", "Zenith", 1, false)],
        entity_base_path: None,
        ..empty_nav()
    };
    let html = render_page(&nav, "Settings", "<p>Body.</p>");
    assert!(!html.contains("entity-switcher"));
}

#[test]
fn the_entity_switcher_lists_every_configured_entity_plus_all_entities() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        entity_base_path: Some("/vault/docs/".to_owned()),
        entities: vec![
            entity_option("zenith", "Zenith", 1, false),
            entity_option("personal", "Personal", 2, false),
        ],
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("<option value=\"\" selected>All entities</option>"));
    assert!(html.contains("<option value=\"zenith\">Zenith</option>"));
    assert!(html.contains("<option value=\"personal\">Personal</option>"));
}

#[test]
fn the_selected_entity_is_marked_selected_and_all_entities_is_not() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        entity_base_path: Some("/vault/".to_owned()),
        current_entity: Some("personal".to_owned()),
        current_entity_accent: Some(2),
        entities: vec![
            entity_option("zenith", "Zenith", 1, false),
            entity_option("personal", "Personal", 2, true),
        ],
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("<option value=\"personal\" selected>Personal</option>"));
    assert!(html.contains("<option value=\"\">All entities</option>"));
    assert!(html.contains("style=\"--entity: var(--entity-2)\""));
}

#[test]
fn switching_the_entity_navigates_to_the_base_path_with_an_entity_query_parameter() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        entity_base_path: Some("/vault/docs/".to_owned()),
        entities: vec![entity_option("zenith", "Zenith", 1, false)],
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("onchange=\"location.href='/vault/docs/'+(this.value?'?entity='+this.value:'');\""));
}

#[test]
fn a_page_title_renders_first_in_the_content_with_its_icon_name_and_toggle() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        page_title: Some(PageTitle {
            icon: "list-checks",
            label: "Tasks".to_owned(),
            actions_html: String::new(),
            download_html: String::new(),
            favourite_toggle_html: "<form class=\"app-favourite-form\"></form>".to_owned(),
        }),
        ..empty_nav()
    };
    let html = render_page(&nav, "Tasks", "<p>Body.</p>");
    assert!(html.contains(
        "<main class=\"content\"><header class=\"page-title\">\
         <span class=\"ic ic-list-checks\" aria-hidden=\"true\"></span>\
         <h1>Tasks</h1>\
         <form class=\"app-favourite-form\"></form></header><p>Body.</p></main>"
    ));
}

#[test]
fn a_page_without_a_title_renders_the_content_unchanged() {
    let html = render_page(&empty_nav(), "Settings", "<p>Body.</p>");
    assert!(html.contains("<main class=\"content\"><p>Body.</p></main>"));
    assert!(!html.contains("page-title"));
}

#[test]
fn a_page_title_name_is_html_escaped() {
    let nav = NavData {
        page_title: Some(PageTitle {
            icon: "apps",
            label: "<script>alert(1)</script>".to_owned(),
            actions_html: String::new(),
            download_html: String::new(),
            favourite_toggle_html: String::new(),
        }),
        ..empty_nav()
    };
    let html = render_page(&nav, "x", "");
    assert!(!html.contains("<script>alert(1)</script>"));
    assert!(html.contains("<h1>&#60;script&#62;alert(1)&#60;/script&#62;</h1>"));
}

#[test]
fn title_actions_render_between_the_name_and_the_star() {
    let mut nav = NavData {
        page_title: Some(PageTitle {
            icon: "inbox",
            label: "Inbox".to_owned(),
            actions_html: String::new(),
            download_html: String::new(),
            favourite_toggle_html: "<STAR>".to_owned(),
        }),
        ..empty_nav()
    };
    nav.set_title_actions("<ACTION>".to_owned());
    let html = render_page(&nav, "Inbox", "<p>Body.</p>");
    assert!(html.contains("<h1>Inbox</h1><ACTION><STAR></header>"));
}

#[test]
fn a_files_download_sits_between_the_title_actions_and_the_star() {
    let mut nav = NavData {
        page_title: Some(PageTitle {
            icon: "file-text",
            label: "Notes".to_owned(),
            actions_html: String::new(),
            download_html: "<DOWNLOAD>".to_owned(),
            favourite_toggle_html: "<STAR>".to_owned(),
        }),
        ..empty_nav()
    };
    nav.set_title_actions("<ACTION>".to_owned());
    let html = render_page(&nav, "Notes", "<p>Body.</p>");
    assert!(
        html.contains("<h1>Notes</h1><ACTION><DOWNLOAD><STAR></header>"),
        "{html}"
    );
}

#[test]
fn setting_title_actions_on_a_page_without_a_title_bar_changes_nothing() {
    let mut nav = empty_nav();
    nav.set_title_actions("<ACTION>".to_owned());
    assert!(nav.page_title.is_none());
    assert!(!render_page(&nav, "Settings", "").contains("<ACTION>"));
}

#[test]
fn a_server_that_is_not_answering_changes_the_header_badge_and_the_footer_dot() {
    for (status, badge, class, dot) in [
        (
            McpStatus {
                connected: 2,
                total: 2,
                tools: 40,
            },
            "LIVE",
            "live-badge status-ok",
            "status-dot \"",
        ),
        (
            McpStatus {
                connected: 1,
                total: 2,
                tools: 20,
            },
            "DEGRADED",
            "live-badge status-degraded",
            "status-dot attention",
        ),
        (
            McpStatus {
                connected: 0,
                total: 2,
                tools: 0,
            },
            "OFFLINE",
            "live-badge status-offline",
            "status-dot error",
        ),
    ] {
        let mut nav = empty_nav();
        nav.mcp_status = status;

        let html = render_page(&nav, "example.md", "<p>Body.</p>");

        assert!(html.contains(&format!(">{badge}<")), "{badge}: {html}");
        assert!(html.contains(class), "{class}");
        assert!(html.contains(dot), "{dot}");
        assert!(html.contains(&format!("{} of {} connected", status.connected, status.total)));
    }
}

// The badge sits inside the alerts panel, whose own `hx-target` it would otherwise inherit: that target finds nothing from
// the badge, so its request never runs and the count never shows.
#[test]
fn the_alerts_badge_replaces_itself_rather_than_inheriting_the_panels_target() {
    let mut nav = empty_nav();
    nav.nav_target_vault = Some("alpha".to_owned());

    let html = render_page(&nav, "note.md", "<p>Body.</p>");

    let badge = html.split("<span class=\"alerts-badge\"").nth(1).unwrap_or_default();
    let badge = badge.split('>').next().unwrap_or_default();
    assert!(badge.contains("hx-target=\"this\""), "{badge}");
}

#[test]
fn the_footer_ends_with_an_about_line_naming_zentelligence_and_the_running_version() {
    let html = render_page(&empty_nav(), "note.md", "<p>Body.</p>");
    let footer = html.split("<footer class=\"status-bar\">").nth(1).unwrap_or_default();
    let about = footer.split("status-bar-about").nth(1).unwrap_or_default();
    assert!(
        about.contains("href=\"https://zentelligence.au\"") && about.contains(">Zentelligence</a>"),
        "{footer}"
    );
    assert!(about.contains("rel=\"noopener noreferrer\""), "{about}");
    assert!(about.contains(&format!("v{}", env!("CARGO_PKG_VERSION"))), "{about}");
}
