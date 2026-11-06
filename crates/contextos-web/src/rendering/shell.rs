//! Assembles [`NavData`](super::page::NavData) for the Command Centre
//! cockpit shell (vault switcher, grouped sidebar navigation, the header's
//! active destination, per-vault entity accents) every full-page response
//! ([`super::page::render_page`]) is wrapped in.
//!
//! One live MCP call per full page: `vault_info`, feeding the vault
//! switcher's own list and per-vault entity accents.

use super::page::{
    BreadcrumbSegment, HostView, NavData, NavEntityOption, NavVault, PageTitle, SidebarGroup, SidebarLink,
};
use crate::entities;
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::nav_counts::{self, NavCounts};
use crate::urls::VaultUrls;
use crate::vault_info::{ReportedVault, VaultInfoError, VaultInfoReport};

/// Which primary-nav item is current, so [`build_nav`] can mark it active.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveScreen {
    Vault,
    Agents,
    Apps,
    Jobs,
    Settings,
}

/// One `vault_info` call, read for everything the nav shell needs (the switcher's own vault list). A refused or
/// unparsable answer gives no vaults, so the page still renders.
async fn fetch_vault_info(client: &McpClient) -> Result<VaultInfoReport, McpCallError> {
    match client.vault_info().await {
        Ok(report) => Ok(report),
        Err(VaultInfoError::Call(error)) => Err(error),
        Err(VaultInfoError::Refused | VaultInfoError::Unparsable) => Ok(VaultInfoReport {
            vaults: Vec::new(),
            time_zone: None,
        }),
    }
}

/// `pub(crate)` (not just this module's own use): also the bare HTTP server
/// root's own redirect target lookup
/// ([`crate::routes::vault::get_server_root`]), which needs the identical
/// "every configured vault's own `name`, in `vault_info` order" list and
/// should not maintain a second implementation of it.
pub(crate) async fn configured_vault_names(client: &McpClient) -> Result<Vec<String>, McpCallError> {
    let parsed = fetch_vault_info(client).await?;
    Ok(parsed.vaults.into_iter().map(|entry| entry.name).collect())
}

/// Builds a clickable ancestor trail for `path` (vault-relative, already
/// trimmed of leading/trailing `/`; possibly empty for the vault root
/// itself), rooted at `vault_name`: every segment carries a link to that
/// ancestor's own directory route except the last, which is always the
/// current page or directory itself and is never a link back to where the
/// reader already is. Feeds [`NavData::breadcrumb`](super::page::NavData),
/// the top bar's own page-hierarchy trail (`path` a file's own full path, a
/// directory's own path, or a fixed label like `"Apps"`).
///
/// Never includes the vault's own name as a segment: the header's vault
/// switcher, immediately to this trail's left, already names the current
/// vault, so repeating it here would be redundant chrome, not a distinct
/// piece of the page hierarchy. At the vault root (`path` empty), the trail
/// is the single fixed label `"Dashboard"` (the vault's own home page),
/// matching the design mock.
#[must_use]
pub(crate) fn breadcrumb_segments(vault_name: &str, path: &str) -> Vec<BreadcrumbSegment> {
    if path.is_empty() {
        return vec![BreadcrumbSegment {
            label: "Dashboard".to_owned(),
            href: None,
        }];
    }
    let parts: Vec<&str> = path.split('/').collect();
    let mut segments = Vec::with_capacity(parts.len());
    let mut accumulated = String::new();
    for (index, part) in parts.iter().enumerate() {
        if !accumulated.is_empty() {
            accumulated.push('/');
        }
        accumulated.push_str(part);
        let is_last = index == parts.len() - 1;
        segments.push(BreadcrumbSegment {
            label: (*part).to_owned(),
            href: if is_last {
                None
            } else {
                Some(VaultUrls::from(vault_name).folder(&accumulated))
            },
        });
    }
    segments
}

/// The vault-independent breadcrumb (`/server/settings/`): a single unlinked
/// `"settings"` segment, matching every other page's own final,
/// current-location segment carrying no link.
fn settings_breadcrumb() -> Vec<BreadcrumbSegment> {
    vec![BreadcrumbSegment {
        label: "settings".to_owned(),
        href: None,
    }]
}

/// Whether `current_path` (a `build_nav` caller's own `breadcrumb_suffix`,
/// already trimmed) is exactly `target`: a plain equality check, not a
/// prefix match, so a file nested arbitrarily deep inside e.g.
/// `journal/2026/09/` does not spuriously mark the Journal link active.
fn is_current_path(current_path: Option<&str>, target: &str) -> bool {
    current_path.map(|path| path.trim_matches('/')) == Some(target)
}

/// The text of a count badge, or `None` (no badge) for a count that could not be read.
fn count_label(count: Option<usize>) -> Option<String> {
    count.map(|count| count.to_string())
}

/// The sidebar's unheaded "General" section: the vault's own home page plus
/// the fixed set of working destinations a base-scaffolded vault
/// provisions, each a screen of its own. `meta` carries the destination's own
/// count where it has one ([`NavCounts`]); Signals counts what still needs
/// attention and shows an attention dot while anything does.
fn general_links(vault_name: &str, current_path: Option<&str>, counts: &NavCounts) -> Vec<SidebarLink> {
    let urls = VaultUrls::from(vault_name);
    // `screen` is the destination's path within the vault, which marks the link active when it is the current page.
    let link = |label: &str, href: String, screen: &str, icon: &'static str, meta: Option<String>| SidebarLink {
        attention: None,
        label: label.to_owned(),
        href,
        icon,
        active: is_current_path(current_path, screen),
        meta,
        target: None,
    };
    vec![
        link("Dashboard", urls.root(), "", "layout-dashboard", None),
        link("Tasks", urls.tasks(), "tasks", "list-checks", count_label(counts.tasks)),
        SidebarLink {
            attention: counts.signals.filter(|open| *open > 0).map(|_| "attention"),
            ..link(
                "Signals",
                urls.signals(),
                "signals",
                "radio",
                count_label(counts.signals),
            )
        },
        link(
            "Inbox",
            urls.tray("inbox", ""),
            "inbox",
            "inbox",
            count_label(counts.inbox),
        ),
        link(
            "Outbox",
            urls.tray("outbox", ""),
            "outbox",
            "send",
            count_label(counts.outbox),
        ),
        link("Journal", urls.journal(), "journal", "book-open", None),
        link("Notes", urls.notes(""), "notes", "file-text", None),
    ]
}

/// The Agents section's four sub-items, each a query-filtered link into
/// the already-implemented Agents screen. General, Business, and Personal
/// select by `TeamMember::team`: General holds the vault-wide agents that
/// work across every kind of entity (the Context OS plugin seeds them), while
/// Business and Personal hold the agents for that side of the deployment
/// (the names the Business OS and Personal OS plugins seed, though an
/// operator may place agents of their own there too). Each gives quick access to
/// instruct that team's agents. Blocked (`TeamMember::state`) surfaces agents
/// that need action to unblock rather than instruction. `team` is free text a
/// deployment may customise, but the sidebar's own team links always target
/// the literal `general`/`business`/`personal` values.
fn agent_group_links(vault_name: &str, counts: &NavCounts) -> Vec<SidebarLink> {
    let link = |label: &str, query: &str, icon: &'static str, count: Option<usize>| SidebarLink {
        attention: None,
        label: label.to_owned(),
        href: VaultUrls::from(vault_name).agents_with(query),
        icon,
        active: false,
        meta: count_label(count),
        target: None,
    };
    vec![
        link("General", "team=general", "globe", counts.agents_general),
        link("Business", "team=business", "briefcase", counts.agents_business),
        link("Personal", "team=personal", "user-round", counts.agents_personal),
        link(
            "Blocked",
            &format!("state={}", crate::team::MemberState::Blocked.label()),
            "octagon-alert",
            counts.agents_blocked,
        ),
    ]
}

/// The Knowledge section's sub-items: the vault-wide Wiki and Insights
/// stores, plus one link per entity the vault has configured - always
/// every configured entity, regardless of the header's own global entity
/// switcher, so the sidebar stays a real navigation menu between entities
/// rather than narrowing to whichever one happens to be globally selected
/// (that switcher still filters each entity-scoped screen's own *content*;
/// hiding sibling links here would only remove the means to navigate to
/// them). `current_path` (`build_nav`'s own `breadcrumb_suffix`) marks
/// whichever single link is actually open active, the same "compare
/// against the real current path" contract
/// [`general_links`]/[`is_current_path`] already use.
fn knowledge_links(vault_name: &str, entities: &[NavEntityOption], current_path: Option<&str>) -> Vec<SidebarLink> {
    let current_path = current_path.map(|path| path.trim_matches('/'));
    let under = |prefix: &str| current_path.is_some_and(|path| is_within(path, prefix));
    let mut links = vec![
        SidebarLink {
            attention: None,
            label: "Wiki".to_owned(),
            href: VaultUrls::from(vault_name).wiki(""),
            icon: "book",
            active: under("wiki"),
            meta: None,
            target: None,
        },
        SidebarLink {
            attention: None,
            label: "Insights".to_owned(),
            href: VaultUrls::from(vault_name).insights(""),
            icon: "sparkles",
            active: under("insights"),
            meta: None,
            target: None,
        },
    ];
    links.extend(entities.iter().map(|entity| SidebarLink {
        attention: None,
        label: entity.name.clone(),
        href: VaultUrls::from(vault_name).entity(&entity.slug),
        icon: "folder",
        active: under(&format!("entity/{}", entity.slug)) || under(&entity.slug),
        meta: None,
        target: None,
    }));
    links
}

/// Whether `path` is the folder `prefix` or lies beneath it: `wiki-notes` is not within `wiki`.
fn is_within(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Whether `current_path` falls under the Knowledge section's own trees
/// (the Wiki/Insights stores, the entity-folder screen, or any configured
/// entity's own top-level directory reached directly through the general
/// vault content route - a non-markdown file's own row in the
/// entity-folder screen links there rather than re-implementing every
/// content type's own rendering a second time): a prefix match, unlike
/// [`is_current_path`], since Knowledge content nests arbitrarily deep and
/// there is no dedicated `ActiveScreen` variant for it (every vault content
/// page shares `ActiveScreen::Vault`).
fn is_under_knowledge(current_path: Option<&str>, entities: &[NavEntityOption]) -> bool {
    let Some(current_path) = current_path.map(|path| path.trim_matches('/')) else {
        return false;
    };
    is_within(current_path, "wiki") || is_within(current_path, "insights") || is_within(current_path, "entity") || {
        entities
            .iter()
            .any(|entity| current_path == entity.slug || current_path.starts_with(&format!("{}/", entity.slug)))
    }
}

/// Builds every [`SidebarGroup`] the sidebar renders for a vault-scoped
/// page: `None` for a vault-independent page (`/server/settings/`), which shows
/// no groups at all, matching [`NavData::entities`]'s identical "hidden
/// on a vault-independent page" convention.
fn sidebar_groups(
    vault_name: Option<&str>,
    breadcrumb_suffix: Option<&str>,
    active: ActiveScreen,
    entities: &[NavEntityOption],
    favourited_apps: Vec<SidebarLink>,
    counts: &NavCounts,
) -> Vec<SidebarGroup> {
    let Some(vault_name) = vault_name else {
        return Vec::new();
    };
    vec![
        SidebarGroup {
            section: None,
            label: None,
            heading_href: None,
            icon: "",
            active: false,
            links: general_links(vault_name, breadcrumb_suffix, counts),
        },
        SidebarGroup {
            section: None,
            label: Some("Agents"),
            heading_href: Some(VaultUrls::from(vault_name).agents()),
            icon: "agents",
            active: active == ActiveScreen::Agents,
            links: agent_group_links(vault_name, counts),
        },
        SidebarGroup {
            section: None,
            label: Some("Apps"),
            heading_href: Some(VaultUrls::from(vault_name).apps()),
            icon: "apps",
            active: active == ActiveScreen::Apps,
            links: favourited_apps,
        },
        SidebarGroup {
            section: None,
            label: Some("Workflows"),
            heading_href: Some(VaultUrls::from(vault_name).workflows()),
            icon: "jobs",
            active: active == ActiveScreen::Jobs,
            links: Vec::new(),
        },
        SidebarGroup {
            section: None,
            label: Some("Knowledge"),
            heading_href: Some(VaultUrls::from(vault_name).wiki("")),
            icon: "book",
            active: is_under_knowledge(breadcrumb_suffix, entities),
            links: knowledge_links(vault_name, entities, breadcrumb_suffix),
        },
    ]
}

/// The already-stubbed "Apps" [`SidebarGroup`]'s own dynamic population: one [`SidebarLink`] per favourited app in
/// `vault_name`, mirroring [`knowledge_links`]'s identical "build a `SidebarLink` per already-fetched live entry"
/// pattern. Skips app discovery entirely when no app is favourited (the common case): a vault with an empty or
/// absent `.contextos/apps.toml` costs this function nothing beyond the one favourites-file read.
///
/// Apps have no run-history/status concept the way jobs do, so, unlike a future favourited-jobs link, a favourited
/// app's own `SidebarLink` carries no status dot and no `meta` count badge.
///
/// `pub(crate)`: also [`crate::routes::shell::favourites_panel`]'s own "Favourite apps" section, which needs the
/// identical favourited-app list this function already builds for the sidebar, rather than a second
/// implementation of the same favourites-plus-discovery join.
pub(crate) async fn favourited_app_links(
    client: &McpClient,
    vault_name: &str,
    known_mcp_servers: &[String],
) -> Vec<SidebarLink> {
    let favourites = crate::apps_favourites::load_apps_favourites(client, vault_name).await;
    if favourites.apps.is_empty() {
        return Vec::new();
    }
    let Ok(registered) = crate::apps::discover_apps(client, vault_name, known_mcp_servers).await else {
        return Vec::new();
    };
    registered
        .into_iter()
        .filter(|app| favourites.is_favourite(&app.slug))
        .map(|app| SidebarLink {
            attention: None,
            label: app.name,
            href: VaultUrls::from(vault_name).app_home(&app.slug),
            icon: "apps",
            active: false,
            meta: None,
            // Serving mode is derived from `kind` alone, the identical rule `routes::apps::list_entry` already
            // applies to the Apps list card's own "Open" link: a `spa`-kind app keeps opening full-screen in a new
            // tab from every surface that links it, never just the list card.
            target: match app.kind {
                crate::apps::AppKind::Spa => Some("_blank"),
                crate::apps::AppKind::Js | crate::apps::AppKind::Htmx => None,
            },
        })
        .collect()
}

/// The shell footer's own MCP-servers status: every full-page route calls
/// this alongside [`build_nav`] (the same "assembled here, finished by the
/// caller" pattern `appearance` already follows) and assigns the result to
/// [`NavData::mcp_status`], since `build_nav` itself receives one
/// already-resolved [`McpClient`], never the full [`McpClientSet`] a
/// cross-server aggregate needs.
pub async fn mcp_status(clients: &McpClientSet) -> super::page::McpStatus {
    let (connected, total, tools) = clients.status_summary().await;
    super::page::McpStatus {
        connected,
        total,
        tools,
    }
}

/// The `--entity-N` accent by position in `entries`,
/// wrapping after six. A cue only, not identity. `pub(crate)`: also the
/// Tasks Focus view's own fallback for an `entity` value with no explicit
/// `.contextos/entities.toml` entry (`routes::tasks_focus`), which reuses
/// this exact position-to-accent mapping rather than defining a second one.
pub(crate) fn entity_for(index: usize) -> u8 {
    u8::try_from(index % 6 + 1).unwrap_or(1)
}

/// Builds the vault switcher's own list and the current vault's accent from
/// one already-fetched `vault_info` result: the two concerns share the
/// identical per-vault row and accent-by-position derivation, so computing
/// them together here keeps [`build_nav`] itself from re-deriving
/// `entries_ref`/`entity_for` twice.
fn vault_switcher_data(vault_name: Option<&str>, info: Option<&VaultInfoReport>) -> (Vec<NavVault>, Option<u8>) {
    let empty: &[ReportedVault] = &[];
    let entries_ref = info.map_or(empty, |info| info.vaults.as_slice());
    let vaults: Vec<NavVault> = entries_ref
        .iter()
        .enumerate()
        .map(|(index, entry)| NavVault {
            name: entry.name.clone(),
            is_current: vault_name == Some(entry.name.as_str()),
            entity: entity_for(index),
        })
        .collect();
    let current_vault_entity =
        vault_name.and_then(|name| entries_ref.iter().position(|entry| entry.name == name).map(entity_for));
    (vaults, current_vault_entity)
}

/// Builds the entity switcher's own option list and the selected entity's
/// accent, from the vault's configured entities.
fn entity_switcher_data(
    configured_entities: &[entities::EntityEntry],
    current_entity: Option<&str>,
) -> (Vec<NavEntityOption>, Option<u8>) {
    let entity_options: Vec<NavEntityOption> = configured_entities
        .iter()
        .map(|entity| NavEntityOption {
            slug: entity.slug.clone(),
            name: entity.name.clone(),
            accent: entity.accent,
            is_current: current_entity == Some(entity.slug.as_str()),
        })
        .collect();
    let current_entity_accent = current_entity
        .and_then(|slug| configured_entities.iter().find(|entity| entity.slug == slug))
        .map(|entity| entity.accent);
    (entity_options, current_entity_accent)
}

/// What the sidebar and the switchers are drawn from: `vault_info` (the switcher's own list), the configured entities
/// list, the favourited-apps sidebar links, and the counts. They are independent MCP round trips: none's request
/// depends on another's result, so they are issued concurrently rather than paying their IPC latencies back to back on
/// every page render.
async fn nav_inputs(
    client: &McpClient,
    vault_name: Option<&str>,
    known_mcp_servers: &[String],
) -> (
    Option<VaultInfoReport>,
    Vec<entities::EntityEntry>,
    Vec<SidebarLink>,
    NavCounts,
) {
    let Some(vault_name) = vault_name else {
        return (
            fetch_vault_info(client).await.ok(),
            Vec::new(),
            Vec::new(),
            NavCounts::default(),
        );
    };
    let (info, configured_entities, favourited_apps, counts) = tokio::join!(
        Box::pin(fetch_vault_info(client)),
        Box::pin(entities::load_entities(client, vault_name)),
        Box::pin(favourited_app_links(client, vault_name, known_mcp_servers)),
        Box::pin(nav_counts::load(client, vault_name)),
    );
    (info.ok(), configured_entities, favourited_apps, counts)
}

/// The Workflows section lists the jobs needing attention, and the current page's own title bar (when it is a
/// favouritable destination) reads the stored favourite state: two independent MCP round trips, issued together. The
/// attention links are placed in `sidebar_groups`; the title and favourite state are returned.
async fn title_and_attention(
    client: &McpClient,
    vault: &str,
    breadcrumb_suffix: Option<&str>,
    known_mcp_servers: &[String],
    entity_base_path: Option<&str>,
    sidebar_groups: &mut [SidebarGroup],
) -> (Option<PageTitle>, Option<crate::view_favourites::ViewsState>) {
    let view = breadcrumb_suffix.and_then(crate::view_favourites::for_path);
    let (title, attention) = tokio::join!(
        view_title_and_favourites(client, vault, view),
        attention_job_links(client, vault, known_mcp_servers),
    );
    for group in sidebar_groups.iter_mut() {
        if group.label == Some("Workflows") {
            group.links = attention_sidebar_links(vault, &attention, entity_base_path);
        }
    }
    title.map_or((None, None), |(title, state)| (Some(title), Some(state)))
}

/// Where the shell's live stream connects: the vault's, narrowed to the selected entity.
fn events_stream_href(vault_name: &str, current_entity: Option<&str>) -> String {
    let entity = current_entity
        .filter(|entity| !entity.is_empty())
        .map(|entity| format!("entity={}", crate::inbox_outbox::encode_query_value(entity)))
        .unwrap_or_default();
    VaultUrls::from(vault_name).events_stream(&entity)
}

/// Assembles [`NavData`] for one page render.
///
/// `vault_name` is `None` for a vault-independent page (`/server/settings/`).
/// `breadcrumb_suffix` is the caller-formatted path segment(s) shown after
/// the vault name in the breadcrumb (a file's own full path, or a fixed
/// label like `"apps"`), and doubles as the current path
/// [`general_links`]/[`is_under_knowledge`] compare against to mark a
/// sidebar destination active.
///
/// Never fails outright: a transport failure degrades to an empty vault
/// list (the chrome renders with less data rather than the whole page
/// failing over a nav-shell-only concern). `current_entity` is the selected
/// entity switcher's own slug (the `?entity=` query parameter), `None` for
/// "All entities" (the always-available default). `entity_base_path` is the
/// current page's own path with no `?entity=` query parameter, so the
/// switcher can compose each option's own href; `None` on a
/// vault-independent page, where no entity switcher renders regardless of
/// `vault_name`.
///
/// `known_mcp_servers` is every configured `[[mcp_server]]` name
/// ([`McpClientSet::names`]), needed only to populate the sidebar's own
/// favourited-apps links through the identical `mcp_servers` allow-list
/// validation [`crate::apps::discover_apps`] already applies everywhere
/// else it runs; a vault-independent page still passes it (harmlessly
/// unused, since [`favourited_app_links`] never runs without a
/// `vault_name`), matching every other parameter here that a
/// vault-independent page ignores rather than needing a second `build_nav`
/// entry point.
pub async fn build_nav(
    client: &McpClient,
    active: ActiveScreen,
    vault_name: Option<&str>,
    breadcrumb_suffix: Option<&str>,
    current_entity: Option<&str>,
    entity_base_path: Option<&str>,
    known_mcp_servers: &[String],
) -> NavData {
    let (info, configured_entities, favourited_apps, counts) = nav_inputs(client, vault_name, known_mcp_servers).await;

    let (vaults, current_vault_entity) = vault_switcher_data(vault_name, info.as_ref());
    let nav_target_vault = vault_name
        .map(str::to_owned)
        .or_else(|| vaults.first().map(|entry| entry.name.clone()));
    let (entity_options, current_entity_accent) = entity_switcher_data(&configured_entities, current_entity);

    let breadcrumb = vault_name.map_or_else(settings_breadcrumb, |name| {
        let trimmed = breadcrumb_suffix.unwrap_or("").trim_matches('/');
        breadcrumb_segments(name, trimmed)
    });
    let mut sidebar_groups = sidebar_groups(
        vault_name,
        breadcrumb_suffix,
        active,
        &entity_options,
        favourited_apps,
        &counts,
    );
    let (resolved_title, favourites) = if let Some(vault) = vault_name {
        title_and_attention(
            client,
            vault,
            breadcrumb_suffix,
            known_mcp_servers,
            entity_base_path,
            &mut sidebar_groups,
        )
        .await
    } else {
        (None, None)
    };
    sidebar_groups.push(server_group(None));
    // The header's "New note" and "Profile" controls target the vault a
    // reader would land in from a vault-independent page too (the same
    // `nav_target_vault` the header's own wordmark link already falls back
    // to), so they stay reachable rather than merely disabled from
    // `/server/settings/`.
    let nav_target_urls = nav_target_vault.as_deref().map(VaultUrls::from);
    let nav_target_vault_notes = nav_target_urls.as_ref().map(|urls| urls.notes(""));
    let nav_target_vault_profile = nav_target_urls.as_ref().map(VaultUrls::profile);
    let nav_target_vault_settings = nav_target_urls.as_ref().map(VaultUrls::settings);

    NavData {
        vaults,
        current_vault: vault_name.map(str::to_owned),
        nav_target_vault,
        breadcrumb,
        active_vault_screen: active == ActiveScreen::Vault,
        active_agents_screen: active == ActiveScreen::Agents,
        active_apps_screen: active == ActiveScreen::Apps,
        active_jobs_screen: active == ActiveScreen::Jobs,
        active_settings_screen: active == ActiveScreen::Settings,
        current_vault_entity,
        entities: entity_options,
        current_entity: current_entity.map(str::to_owned),
        current_entity_accent,
        entity_base_path: entity_base_path.map(str::to_owned),
        // Callers that have a `web.toml` path (every one in this crate)
        // overwrite this with `config::current_appearance` immediately
        // after; `build_nav` itself has no filesystem access of its own
        // (it is an MCP-only assembler, `shell.rs`'s own module doc).
        appearance: super::page::Appearance::default(),
        sidebar_groups,
        host_view: None,
        // Every caller overwrites this with the toggle cookie's own value
        // immediately after, the same "assembled here, finished by the
        // caller" pattern `appearance` already follows: `build_nav` reads
        // no HTTP headers of its own (it is an MCP-only assembler).
        sidebar_wide: true,
        // Every caller overwrites this with `McpClientSet::status_summary`
        // immediately after: `build_nav` receives one already-resolved
        // `McpClient`, never the full `McpClientSet` a status summary needs.
        mcp_status: super::page::McpStatus::default(),
        note_href: nav_target_vault_notes,
        profile_href: nav_target_vault_profile,
        vault_settings_href: nav_target_vault_settings,
        page_title: resolved_title,
        events_stream_href: vault_name.map(|name| events_stream_href(name, current_entity)),
        favourites,
    }
}

/// A page with no vault in its address.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostScreen {
    Workflows,
    Jobs,
    Libraries,
    Settings,
}

impl HostScreen {
    fn view(self, vault: Option<&str>) -> HostView {
        let (icon, label) = match self {
            Self::Workflows | Self::Jobs | Self::Libraries => ("jobs", "Server View"),
            Self::Settings => ("settings", "Server Settings"),
        };
        HostView {
            icon,
            label,
            detail: vault.map(|vault| format!("Vault Menu: {vault}")),
        }
    }
}

/// The sidebar's Server section, on every page: what the server holds for all its vaults. `screen` marks the current one.
pub(crate) fn server_group(screen: Option<HostScreen>) -> SidebarGroup {
    let link = |label: &str, href: &str, icon: &'static str, shows: HostScreen| SidebarLink {
        attention: None,
        label: label.to_owned(),
        href: href.to_owned(),
        icon,
        active: screen == Some(shows),
        meta: None,
        target: None,
    };
    SidebarGroup {
        section: Some("Server"),
        label: Some("Workflows"),
        heading_href: None,
        icon: "jobs",
        active: false,
        links: vec![
            link(
                "Workflows",
                crate::urls::SERVER_WORKFLOWS,
                "jobs",
                HostScreen::Workflows,
            ),
            link("Jobs", crate::urls::SERVER_JOBS, "clock", HostScreen::Jobs),
            link(
                "Libraries",
                crate::urls::SERVER_PACKAGES,
                "library",
                HostScreen::Libraries,
            ),
        ],
    }
}

/// [`build_nav`] for a page with no vault in its address. Someone reaches it from a vault and goes back to one, so it has
/// that vault's sidebar: `active_vault` when that is still configured, otherwise the first vault the header would lead to.
/// The page itself stays out of every vault, and says so through [`NavData::host_view`].
pub(crate) async fn build_host_nav(
    client: &McpClient,
    screen: HostScreen,
    active_vault: Option<&str>,
    known_mcp_servers: &[String],
) -> NavData {
    let active = match screen {
        HostScreen::Workflows | HostScreen::Jobs | HostScreen::Libraries => ActiveScreen::Jobs,
        HostScreen::Settings => ActiveScreen::Settings,
    };
    let mut nav = Box::pin(build_nav(client, active, None, None, None, None, known_mcp_servers)).await;
    let configured = active_vault.filter(|vault| nav.vaults.iter().any(|entry| entry.name == *vault));
    let target = configured.map(str::to_owned).or_else(|| nav.nav_target_vault.clone());
    if let Some(vault) = target.as_deref() {
        // `Settings` marks no sidebar entry as current, which is right: none of them is this page.
        let vault_nav = Box::pin(build_nav(
            client,
            ActiveScreen::Settings,
            Some(vault),
            None,
            None,
            None,
            known_mcp_servers,
        ))
        .await;
        nav.sidebar_groups = vault_nav.sidebar_groups;
        nav.note_href = vault_nav.note_href;
        nav.profile_href = vault_nav.profile_href;
        nav.vault_settings_href = vault_nav.vault_settings_href;
        nav.nav_target_vault = target.clone();
    }
    if let Some(server) = nav.sidebar_groups.last_mut() {
        *server = server_group(Some(screen));
    }
    nav.host_view = Some(screen.view(target.as_deref()));
    nav
}

#[cfg(test)]
#[path = "shell_test.rs"]
mod tests;

/// The configured vaults' names, or none when the server cannot say. A warning, because a page that wants them still
/// shows without them.
pub(crate) async fn vault_names(client: &McpClient) -> Vec<String> {
    match fetch_vault_info(client).await {
        Ok(info) => info.vaults.into_iter().map(|vault| vault.name).collect(),
        Err(error) => {
            tracing::warn!(%error, "the configured vaults could not be read");
            Vec::new()
        }
    }
}

/// The main content panel's title bar for `view`: its icon and name, and the star that un/favourites it, showing the
/// vault's stored state.
pub(crate) async fn page_title(client: &McpClient, vault: &str, view: &crate::view_favourites::View) -> PageTitle {
    view_title(&crate::view_favourites::load(client, vault).await, vault, view)
}

fn view_title(
    state: &crate::view_favourites::ViewsState,
    vault: &str,
    view: &crate::view_favourites::View,
) -> PageTitle {
    PageTitle {
        icon: view.icon,
        label: view.label.to_owned(),
        actions_html: String::new(),
        download_html: String::new(),
        favourite_toggle_html: super::favourite_toggle::render(
            &VaultUrls::from(vault).view(view.id),
            view.label,
            state.is_favourite(view.id),
        ),
    }
}

/// The screen a file page belongs to, as its title bar names it.
pub(crate) struct ScreenTitle {
    pub icon: &'static str,
    pub label: String,
}

impl From<&crate::view_favourites::View> for ScreenTitle {
    fn from(view: &crate::view_favourites::View) -> Self {
        Self {
            icon: view.icon,
            label: view.label.to_owned(),
        }
    }
}

/// Gives a page that shows the file at `path` (vault-relative), from `page_href`, its address on `screen`, the title
/// bar of a file: the screen's icon and name, the link that downloads the file as it is stored, and the star that
/// un/favourites the file itself, showing the vault's stored state. A screen is already one click away in the sidebar;
/// a file someone keeps returning to is what a favourite saves them finding.
pub(crate) async fn show_file(
    nav: &mut NavData,
    client: &McpClient,
    vault: &str,
    screen: ScreenTitle,
    (path, page_href): (&str, &str),
) {
    let state = match nav.favourites.take() {
        Some(state) => state,
        None => crate::view_favourites::load(client, vault).await,
    };
    let name = path.rsplit('/').next().unwrap_or(path);
    nav.page_title = Some(PageTitle {
        icon: screen.icon,
        label: screen.label,
        actions_html: String::new(),
        download_html: super::title_action::render(&super::title_action::TitleAction::Download {
            href: &VaultUrls::download(page_href),
            name,
        }),
        favourite_toggle_html: super::favourite_toggle::render(
            &VaultUrls::from(vault).favourite(path),
            name,
            state.is_page_favourite(path),
        ),
    });
    nav.favourites = Some(state);
}

/// The title bar of a page that is a favouritable destination, with the favourites read to build it, which a page
/// that then shows a file reuses.
async fn view_title_and_favourites(
    client: &McpClient,
    vault: &str,
    view: Option<&crate::view_favourites::View>,
) -> Option<(PageTitle, crate::view_favourites::ViewsState)> {
    let view = view?;
    let state = crate::view_favourites::load(client, vault).await;
    Some((view_title(&state, vault, view), state))
}

/// The Workflows section's links: one per job needing attention, marked active on that job's own page.
fn attention_sidebar_links(
    vault: &str,
    attention: &[(crate::jobs::RegisteredJob, &'static str)],
    entity_base_path: Option<&str>,
) -> Vec<SidebarLink> {
    attention
        .iter()
        .map(|(job, state)| {
            let href = VaultUrls::from(vault).job(&job.package, &job.workflow_slug, &job.slug);
            SidebarLink {
                label: job.name.clone(),
                active: entity_base_path == Some(href.as_str()),
                href,
                icon: "jobs",
                meta: None,
                target: None,
                attention: Some(*state),
            }
        })
        .collect()
}

async fn attention_job_links(
    client: &McpClient,
    vault: &str,
    servers: &[String],
) -> Vec<(crate::jobs::RegisteredJob, &'static str)> {
    let Ok(jobs) = crate::jobs::discover_jobs(client, vault, servers).await else {
        return Vec::new();
    };
    let mut links = Vec::new();
    for job in jobs {
        let Ok(runs) =
            crate::job_runs::run_history(client, vault, &job.package, &job.workflow_slug, &job.slug, 1).await
        else {
            continue;
        };
        if let Some(attention) = runs.first().and_then(crate::job_runs::RunSummary::attention) {
            links.push((job, attention));
        }
        if links.len() == 20 {
            break;
        }
    }
    links
}
