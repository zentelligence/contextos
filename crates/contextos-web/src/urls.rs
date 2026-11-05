//! Every URL the interface builds within a vault, in one place.
//!
//! A screen's address scheme lives here and nowhere else: code asks [`VaultUrls`] for a URL, and templates print the
//! URLs their views were given. Each method takes paths as written (vault-relative, or relative to its screen's
//! folder) and percent-encodes them itself, so no caller encodes and nothing is encoded twice. A test reads the
//! sources and fails if any other file formats a vault URL.

use crate::inbox_outbox::encode_query_value;

/// The URLs within one vault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultUrls {
    vault: String,
}

impl From<&str> for VaultUrls {
    fn from(vault_name: &str) -> Self {
        Self {
            vault: vault_name.to_owned(),
        }
    }
}

/// `path` without leading or trailing `/`, percent-encoded with its `/` separators kept.
fn encoded(path: &str) -> String {
    encode_query_value(path.trim_matches('/'))
}

impl VaultUrls {
    /// The vault's configured name.
    #[must_use]
    pub fn vault_name(&self) -> &str {
        &self.vault
    }

    /// `/{vault}/{rest}`.
    fn under(&self, rest: &str) -> String {
        format!("/{}/{rest}", self.vault)
    }

    /// `/{vault}/{screen}/` for an empty `relative`, else `/{vault}/{screen}/{relative}`.
    fn in_screen(&self, screen: &str, relative: &str) -> String {
        let relative = encoded(relative);
        if relative.is_empty() {
            self.under(&format!("{screen}/"))
        } else {
            self.under(&format!("{screen}/{relative}"))
        }
    }

    /// `/{vault}/{screen}/{relative}/{action}`, or `/{vault}/{screen}/{action}` for an empty `relative`.
    fn action_in_screen(&self, screen: &str, relative: &str, action: &str) -> String {
        let relative = encoded(relative);
        if relative.is_empty() {
            self.under(&format!("{screen}/{action}"))
        } else {
            self.under(&format!("{screen}/{relative}/{action}"))
        }
    }

    /// The vault's own page, its Overview.
    #[must_use]
    pub fn root(&self) -> String {
        self.under("")
    }

    /// A file of the vault at `path` that no screen shows, served as it is stored; a page there is sent on to the
    /// screen that shows it.
    #[must_use]
    pub fn file(&self, path: &str) -> String {
        self.under(&encoded(path))
    }

    /// The file a page at `page_href` shows, as it is stored, for download. Every screen that shows a file answers
    /// its own address with `?raw=1` this way.
    #[must_use]
    pub fn download(page_href: &str) -> String {
        format!("{page_href}?raw=1")
    }

    /// A folder of the vault, which the generic route sends on to the section that shows it.
    #[must_use]
    pub fn folder(&self, path: &str) -> String {
        let path = encoded(path);
        if path.is_empty() {
            self.root()
        } else {
            self.under(&format!("{path}/"))
        }
    }

    /// A base at `path` showing `view`, or its first view when `view` is empty.
    #[must_use]
    pub fn base_view(&self, path: &str, view: &str) -> String {
        if view.is_empty() {
            self.file(path)
        } else {
            format!("{}?view={}", self.file(path), encode_query_value(view))
        }
    }

    // Vault Settings and health

    #[must_use]
    pub fn settings(&self) -> String {
        self.under("settings/")
    }

    /// Vault Settings, scrolled to Vault health.
    #[must_use]
    pub fn settings_health(&self) -> String {
        self.under("settings/#health")
    }

    /// A Vault Settings management category, at `location` within it.
    #[must_use]
    pub fn settings_category(&self, category: &str, location: &str) -> String {
        self.in_screen(&format!("settings/{category}"), location)
    }

    #[must_use]
    pub fn health(&self) -> String {
        self.under("health")
    }

    // Overview

    /// One panel of the vault Overview (`bridge`, `status`, or `inputs`) as a fragment.
    #[must_use]
    pub fn overview_panel(&self, panel: &str) -> String {
        self.under(&format!("overview/{panel}"))
    }

    /// The compact vault health indicator for `surface` (`header` or `footer`).
    #[must_use]
    pub fn health_summary(&self, surface: &str) -> String {
        format!("{}/summary?surface={surface}", self.health())
    }

    /// The Inbox.
    #[must_use]
    pub fn inbox(&self) -> String {
        self.tray("inbox", "")
    }

    // Alerts

    /// What needs the operator: the bell's dropdown and the Overview's Alert rail.
    #[must_use]
    pub fn alerts(&self) -> String {
        self.under("alerts")
    }

    /// The bell's alert count.
    #[must_use]
    pub fn alerts_summary(&self) -> String {
        self.under("alerts/summary")
    }

    // Signals and the event stream

    /// The Signals screen.
    #[must_use]
    pub fn signals(&self) -> String {
        self.under("signals/")
    }

    /// The Signals screen with `query`, a query string whose values are already encoded.
    #[must_use]
    pub fn signals_with(&self, query: &str) -> String {
        format!("{}?{query}", self.signals())
    }

    /// One region of the Signals screen (`feed`, `attention`, or `background`) as a fragment, with `query` (already
    /// encoded) when it is not empty.
    #[must_use]
    pub fn signals_region(&self, region: &str, query: &str) -> String {
        let path = self.under(&format!("signals/{region}"));
        if query.is_empty() {
            path
        } else {
            format!("{path}?{query}")
        }
    }

    /// The Server-Sent Events stream of the vault's new events, with `query` (already encoded) when it is not empty.
    #[must_use]
    pub fn events_stream(&self, query: &str) -> String {
        let path = self.under("events/stream");
        if query.is_empty() {
            path
        } else {
            format!("{path}?{query}")
        }
    }

    // Search

    #[must_use]
    pub fn search(&self) -> String {
        self.under("search")
    }

    /// Search with `query`, a query string whose values are already encoded (`q=a%20b&kind=note`).
    #[must_use]
    pub fn search_with(&self, query: &str) -> String {
        format!("{}?{query}", self.search())
    }

    // Tasks

    #[must_use]
    pub fn tasks(&self) -> String {
        self.under("tasks/")
    }

    /// The task list with `query`, a query string whose values are already encoded.
    #[must_use]
    pub fn tasks_with(&self, query: &str) -> String {
        format!("{}?{query}", self.tasks())
    }

    #[must_use]
    pub fn tasks_new(&self) -> String {
        self.under("tasks/new")
    }

    /// A task's own screen, by its id, which is lower-cased.
    #[must_use]
    pub fn task(&self, id: &str) -> String {
        self.under(&format!("tasks/{}", encode_query_value(&id.to_ascii_lowercase())))
    }

    #[must_use]
    pub fn task_edit(&self, id: &str) -> String {
        format!("{}/edit", self.task(id))
    }

    /// The link picker's search for a task's `field`.
    #[must_use]
    pub fn task_link_search(&self, id: &str, field: &str) -> String {
        format!("{}/links/search?field={}", self.task(id), encode_query_value(field))
    }

    // Knowledge: wiki, insights, and entities

    /// A wiki page or folder, by its path within the wiki folder; the wiki itself for an empty path.
    #[must_use]
    pub fn wiki(&self, relative: &str) -> String {
        self.in_screen("wiki", relative)
    }

    /// An action (`edit`, `rename`, `confirm-delete`, `new`) on a wiki page or folder.
    #[must_use]
    pub fn wiki_action(&self, relative: &str, action: &str) -> String {
        self.action_in_screen("wiki", relative, action)
    }

    #[must_use]
    pub fn insights(&self, relative: &str) -> String {
        self.in_screen("insights", relative)
    }

    #[must_use]
    pub fn insights_action(&self, relative: &str, action: &str) -> String {
        self.action_in_screen("insights", relative, action)
    }

    /// The entity browser at `path`, a vault path beginning with the entity's slug.
    #[must_use]
    pub fn entity(&self, path: &str) -> String {
        self.in_screen("entity", path)
    }

    /// An action on a file or folder in an entity's folder, `relative` being its path within that folder.
    #[must_use]
    pub fn entity_action(&self, slug: &str, relative: &str, action: &str) -> String {
        self.action_in_screen(&format!("entity/{}", encoded(slug)), relative, action)
    }

    /// A file or folder in an entity's folder, `relative` being its path within that folder; the entity's own folder,
    /// with no trailing `/`, for an empty path.
    #[must_use]
    pub fn entity_file(&self, slug: &str, relative: &str) -> String {
        let screen = format!("entity/{}", encoded(slug));
        if encoded(relative).is_empty() {
            self.under(&screen)
        } else {
            self.in_screen(&screen, relative)
        }
    }

    /// An interface page at `path`, an address the interface itself defines (a saved view's), never a vault path.
    #[must_use]
    pub fn page(&self, path: &str) -> String {
        self.under(path)
    }

    // Notes, journal, and the Inbox and Outbox

    #[must_use]
    pub fn notes(&self, relative: &str) -> String {
        self.in_screen("notes", relative)
    }

    #[must_use]
    pub fn notes_action(&self, relative: &str, action: &str) -> String {
        self.action_in_screen("notes", relative, action)
    }

    #[must_use]
    pub fn journal(&self) -> String {
        self.under("journal/")
    }

    /// The journal for a day, `date` in the journal's own date form.
    #[must_use]
    pub fn journal_day(&self, date: &str) -> String {
        self.under(&format!("journal/{}", encode_query_value(date)))
    }

    /// Section `index` of the journal for a day.
    #[must_use]
    pub fn journal_section(&self, date: &str, index: usize) -> String {
        self.under(&format!("journal/{}/sections/{index}", encode_query_value(date)))
    }

    /// The edit form for section `index` of the journal for a day.
    #[must_use]
    pub fn journal_section_edit(&self, date: &str, index: usize) -> String {
        format!("{}/edit", self.journal_section(date, index))
    }

    /// The Inbox or Outbox (`tray`) at `relative` within it.
    #[must_use]
    pub fn tray(&self, tray: &str, relative: &str) -> String {
        self.in_screen(tray, relative)
    }

    /// An Inbox or Outbox command on the folder at `relative`: `relative` with `suffix`, or `at_root` for the root.
    #[must_use]
    pub fn tray_command(&self, tray: &str, relative: Option<&str>, suffix: &str, at_root: &str) -> String {
        match relative.map(encoded).filter(|relative| !relative.is_empty()) {
            None => self.under(&format!("{tray}/{at_root}")),
            Some(relative) => self.under(&format!("{tray}/{relative}{suffix}")),
        }
    }

    // Profile

    #[must_use]
    pub fn profile(&self) -> String {
        self.under("profile/")
    }

    /// An identity document, or the form for a new one with an empty path.
    #[must_use]
    pub fn profile_identity(&self, relative: &str) -> String {
        self.profile_document("identity", relative)
    }

    /// An operating document, or the form for a new one with an empty path.
    #[must_use]
    pub fn profile_operating(&self, relative: &str) -> String {
        self.profile_document("operating", relative)
    }

    fn profile_document(&self, kind: &str, relative: &str) -> String {
        let relative = encoded(relative);
        if relative.is_empty() {
            self.under(&format!("profile/{kind}"))
        } else {
            self.under(&format!("profile/{kind}/{relative}"))
        }
    }

    #[must_use]
    pub fn profile_entity(&self, slug: &str) -> String {
        self.under(&format!("profile/entities/{}", encoded(slug)))
    }

    // Agents, apps, workflows, and saved views

    #[must_use]
    pub fn agents(&self) -> String {
        self.under("agents/")
    }

    /// The agent list with `query`, a query string whose values are already encoded.
    #[must_use]
    pub fn agents_with(&self, query: &str) -> String {
        format!("{}?{query}", self.agents())
    }

    /// The agent list showing agents in `state`.
    #[must_use]
    pub fn agents_in_state(&self, state: &str) -> String {
        self.agents_with(&format!("state={}", encode_query_value(state)))
    }

    #[must_use]
    pub fn apps(&self) -> String {
        self.under("apps/")
    }

    /// An app, as its favourite toggle and settings address it.
    #[must_use]
    pub fn app(&self, slug: &str) -> String {
        self.under(&format!("apps/{}", encoded(slug)))
    }

    /// An app's own entry page.
    #[must_use]
    pub fn app_home(&self, slug: &str) -> String {
        format!("{}/", self.app(slug))
    }

    #[must_use]
    pub fn workflows(&self) -> String {
        self.under("workflows")
    }

    #[must_use]
    pub fn packages(&self) -> String {
        self.under("packages")
    }

    #[must_use]
    pub fn workflow(&self, package: &str, slug: &str) -> String {
        self.under(&format!("workflows/{}/{}", encoded(package), encoded(slug)))
    }

    #[must_use]
    pub fn job(&self, package: &str, workflow: &str, job: &str) -> String {
        format!("{}/jobs/{}", self.workflow(package, workflow), encoded(job))
    }

    /// A saved view, by its id.
    #[must_use]
    pub fn view(&self, id: &str) -> String {
        self.under(&format!("views/{}", encoded(id)))
    }

    /// The favourite state of the file at `path` (vault-relative), which its star patches.
    #[must_use]
    pub fn favourite(&self, path: &str) -> String {
        self.under(&format!("favourites/{}", encoded(path)))
    }
}

#[cfg(test)]
#[path = "urls_test.rs"]
mod tests;
