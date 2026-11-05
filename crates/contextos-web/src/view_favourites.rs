//! Vault-wide favourites: fixed navigation destinations, and the files someone keeps returning to. All persistence
//! crosses MCP.
use crate::mcp_client::McpClient;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One favouritable navigation destination. `icon` is the sidebar's own icon name for the destination, shown again
/// beside the page name in the title bar; `path` is relative to the vault and may carry a query for a filtered view.
/// `covers_subpages` marks a destination whose one screen also shows the pages beneath its directory (a split-pane
/// tree and reader), so those pages keep the destination's title bar; elsewhere a page beneath the directory is its
/// own page.
#[derive(Clone, Copy)]
pub(crate) struct View {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
    pub path: &'static str,
    pub covers_subpages: bool,
}

pub(crate) const TASKS: View = View {
    id: "tasks",
    label: "Tasks",
    icon: "list-checks",
    path: "tasks/",
    covers_subpages: false,
};

pub(crate) const JOURNAL: View = View {
    id: "journal",
    label: "Journal",
    icon: "book-open",
    path: "journal/",
    covers_subpages: false,
};

pub(crate) const NOTES: View = View {
    id: "notes",
    label: "Notes",
    icon: "file-text",
    path: "notes/",
    covers_subpages: false,
};

pub(crate) const WIKI: View = View {
    id: "wiki",
    label: "Wiki",
    icon: "book",
    path: "wiki/",
    covers_subpages: true,
};

pub(crate) const INSIGHTS: View = View {
    id: "insights",
    label: "Insights",
    icon: "sparkles",
    path: "insights/",
    covers_subpages: true,
};

pub(crate) const APPS: View = View {
    id: "apps",
    label: "Apps",
    icon: "apps",
    path: "apps/",
    covers_subpages: false,
};

pub(crate) const WORKFLOWS: View = View {
    id: "workflows",
    label: "Workflows",
    icon: "jobs",
    path: "workflows",
    covers_subpages: false,
};

pub(crate) const SIGNALS: View = View {
    id: "signals",
    label: "Signals",
    icon: "radio",
    path: "signals/",
    covers_subpages: false,
};

const AGENTS: View = View {
    id: "agents",
    label: "Agents",
    icon: "agents",
    path: "agents/",
    covers_subpages: false,
};

const AGENTS_GENERAL: View = View {
    id: "agents-general",
    label: "General agents",
    icon: "globe",
    path: "agents/?team=general",
    covers_subpages: false,
};

const AGENTS_BUSINESS: View = View {
    id: "agents-business",
    label: "Business agents",
    icon: "briefcase",
    path: "agents/?team=business",
    covers_subpages: false,
};

const AGENTS_PERSONAL: View = View {
    id: "agents-personal",
    label: "Personal agents",
    icon: "user-round",
    path: "agents/?team=personal",
    covers_subpages: false,
};

const AGENTS_BLOCKED: View = View {
    id: "agents-blocked",
    label: "Blocked agents",
    icon: "octagon-alert",
    path: "agents/?state=blocked",
    covers_subpages: false,
};

pub(crate) const VIEWS: &[View] = &[
    View {
        id: "dashboard",
        label: "Dashboard",
        icon: "layout-dashboard",
        path: "",
        covers_subpages: false,
    },
    TASKS,
    SIGNALS,
    View {
        id: "inbox",
        label: "Inbox",
        icon: "inbox",
        path: "inbox/",
        covers_subpages: false,
    },
    View {
        id: "outbox",
        label: "Outbox",
        icon: "send",
        path: "outbox/",
        covers_subpages: false,
    },
    JOURNAL,
    NOTES,
    AGENTS,
    AGENTS_GENERAL,
    AGENTS_BUSINESS,
    AGENTS_PERSONAL,
    AGENTS_BLOCKED,
    WIKI,
    INSIGHTS,
    APPS,
    WORKFLOWS,
];

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ViewsState {
    #[serde(default)]
    pub views: BTreeMap<String, Favourite>,
    /// Favourite files, by vault path.
    #[serde(default)]
    pub pages: BTreeMap<String, Favourite>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Favourite {
    #[serde(default)]
    pub favourite: bool,
}
impl ViewsState {
    pub fn is_favourite(&self, id: &str) -> bool {
        self.views.get(id).is_some_and(|entry| entry.favourite)
    }

    /// Whether the file at `path` (vault-relative) is a favourite.
    pub fn is_page_favourite(&self, path: &str) -> bool {
        self.pages.get(path).is_some_and(|entry| entry.favourite)
    }

    /// Every favourite file's vault path, in path order.
    pub fn favourite_pages(&self) -> impl Iterator<Item = &str> {
        self.pages
            .iter()
            .filter(|(_, entry)| entry.favourite)
            .map(|(path, _)| path.as_str())
    }
}

/// The view a vault-relative `path` (with or without surrounding slashes) belongs to: the view whose own directory it
/// is, or, for a view that covers its subpages, any page beneath that directory. Filtered views are not
/// path-addressable, since a filter lives in the query, never the path: see [`agents_view`]. A page beneath any other
/// view's directory is its own page, so it resolves to nothing.
pub(crate) fn for_path(path: &str) -> Option<&'static View> {
    let path = path.trim_matches('/');
    VIEWS.iter().find(|view| {
        if view.path.contains('?') {
            return false;
        }
        let directory = view.path.trim_matches('/');
        path == directory
            || (view.covers_subpages && path.strip_prefix(directory).is_some_and(|rest| rest.starts_with('/')))
    })
}

/// The Agents view a request's own filters select: the named General, Business, Personal, or Blocked view when exactly
/// that one filter is set, otherwise the plain Agents view. `team` matches case-insensitively, as the list itself does;
/// `state` matches exactly. Combinations no named view stands for (both filters, any other value) resolve to the
/// plain Agents view rather than to a view whose title would misdescribe the list shown.
pub(crate) fn agents_view(team: Option<&str>, state: Option<&str>) -> &'static View {
    let blocked = crate::team::MemberState::Blocked.label();
    match (team, state) {
        (Some(team), None) if team.eq_ignore_ascii_case("general") => &AGENTS_GENERAL,
        (Some(team), None) if team.eq_ignore_ascii_case("business") => &AGENTS_BUSINESS,
        (Some(team), None) if team.eq_ignore_ascii_case("personal") => &AGENTS_PERSONAL,
        (None, Some(state)) if state == blocked => &AGENTS_BLOCKED,
        _ => &AGENTS,
    }
}

use crate::vault_state::{FavouriteError, read};

pub(crate) async fn load(client: &McpClient, vault: &str) -> ViewsState {
    read::<ViewsState>(client, vault, ".contextos/views.toml")
        .await
        .map(|snapshot| snapshot.value)
        .unwrap_or_default()
}
pub(crate) async fn set(client: &McpClient, vault: &str, id: &str, favourite: bool) -> Result<(), FavouriteError> {
    let mut snapshot = read::<ViewsState>(client, vault, ".contextos/views.toml").await?;
    snapshot.value.views.insert(id.to_owned(), Favourite { favourite });
    snapshot.save(client, vault, ".contextos/views.toml").await
}

/// Favourites or un-favourites the file at `path` (vault-relative). An un-favourited file is removed rather than kept
/// as `false`, so the state lists only the files someone chose and never grows with every file they ever starred.
pub(crate) async fn set_page(
    client: &McpClient,
    vault: &str,
    path: &str,
    favourite: bool,
) -> Result<(), FavouriteError> {
    let mut snapshot = read::<ViewsState>(client, vault, ".contextos/views.toml").await?;
    if favourite {
        snapshot.value.pages.insert(path.to_owned(), Favourite { favourite });
    } else {
        snapshot.value.pages.remove(path);
    }
    snapshot.save(client, vault, ".contextos/views.toml").await
}

#[cfg(test)]
#[path = "view_favourites_test.rs"]
mod tests;
