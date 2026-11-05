//! The route table: which web route shows each vault path, and how a frontmatter value that is a single wikilink is
//! read. Which file a link names is the server's answer (`expand_links`); loading this table, and the router built on
//! it, is [`crate::screen_routes`].
//!
//! Routes are derived from configuration each screen already holds (the
//! notes, wiki, insights, and journal folders, the entity slugs, and the fixed
//! tasks folder), so there is one table to keep in step rather than a second
//! set of rules. A Markdown note no screen owns has no route: the content
//! route serves files, never pages, so a link to such a note is shown as
//! unavailable rather than as a link that fails. A file that is not Markdown
//! keeps its plain `/{vault}/{path}` route, since only the content route
//! serves files.

use crate::urls::VaultUrls;

/// The folder whose notes have their own task screen, `memory/tasks.base`'s
/// scope.
pub const TASKS_DIR: &str = "memory/tasks";

/// The tasks base, which the Tasks screen lists.
pub const TASKS_BASE: &str = "memory/tasks.base";

pub use contextos_markdown::{AMBIGUOUS_HINT, UNROUTED_HINT};

/// A screen's own URL.
type ScreenUrl = fn(&VaultUrls) -> String;

/// A page's URL on a screen, from its path within the screen's folder.
type PageUrl = fn(&VaultUrls, &str) -> String;

/// Whether a screen shows the file at a path within its folder.
type Shows = fn(&str) -> bool;

fn shows_any(_: &str) -> bool {
    true
}

/// The Insights screen shows only a file directly inside one of its one-character shards.
fn shows_in_a_shard(rest: &str) -> bool {
    rest.split_once('/').is_some_and(|(shard, name)| {
        !name.is_empty() && !name.contains('/') && crate::insights::list_shards().iter().any(|real| real == shard)
    })
}

/// The folders, date format, and slugs the route table is derived from.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RouteConfig {
    pub notes_dir: String,
    pub wiki_dir: String,
    pub insights_dir: String,
    pub journal_dir: String,
    /// The journal's own date format, which names each entry's date from its path within `journal_dir`.
    pub journal_date_format: String,
    pub entity_slugs: Vec<String>,
    /// Where the Vault Settings management pages keep the records they show.
    pub settings: SettingsRoutes,
}

/// The folders whose notes Vault Settings shows on its own pages: the Decisions register's folder (records directly in
/// it) and each registry type's folder, as `.contextos/vault-management.toml` places them. Standards, templates, the
/// operation log, and session summaries are fixed folders ([`crate::vault_catalogue`]).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SettingsRoutes {
    pub register_dir: String,
    /// The Decisions register's base, which the Decisions page lists.
    pub register_base: String,
    /// Each registry type's slug and folder.
    pub registry: Vec<(String, String)>,
}

/// Maps a vault path to the web route that shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkRoutes {
    urls: VaultUrls,
    config: RouteConfig,
}

fn is_markdown(vault_path: &str) -> bool {
    std::path::Path::new(vault_path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

/// What follows `folder` in `path` when `path` is `folder` itself (empty) or inside it.
fn at_or_inside<'a>(path: &'a str, folder: &str) -> Option<&'a str> {
    let folder = folder.trim_matches('/');
    if path == folder {
        Some("")
    } else {
        path.strip_prefix(folder)?.strip_prefix('/')
    }
}

/// What follows `folder/` in `path`, when `path` is inside `folder`.
fn inside<'a>(path: &'a str, folder: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(folder.trim_matches('/'))?.strip_prefix('/')?;
    (!rest.is_empty()).then_some(rest)
}

/// Whether `vault_path` is a folder's managed `index.md`, which the interface never shows.
pub(crate) fn is_managed_index(vault_path: &str) -> bool {
    vault_path.rsplit('/').next() == Some("index.md")
}

/// Whether `vault_path` is a note of the tasks base: Markdown inside the tasks folder, other than a folder's own
/// `index.md` landing note. A plain string check mirroring the tasks base's own filter, for callers that need only a
/// yes or no.
#[must_use]
pub fn is_task_note_path(vault_path: &str) -> bool {
    let Some(rest) = inside(vault_path, TASKS_DIR) else {
        return false;
    };
    let basename = rest.rsplit('/').next().unwrap_or(rest);
    is_markdown(vault_path) && basename.trim_end_matches(".md") != "index"
}

impl LinkRoutes {
    #[must_use]
    pub fn new(vault_name: &str, config: RouteConfig) -> Self {
        Self {
            urls: VaultUrls::from(vault_name),
            config,
        }
    }

    /// Whether `vault_path` is a note of the tasks base, see [`is_task_note_path`].
    #[must_use]
    pub fn is_task_note(&self, vault_path: &str) -> bool {
        is_task_note_path(vault_path)
    }

    /// The web route that shows `vault_path`, or `None` when no screen does. A task note goes to its own screen when
    /// its `task_id` is known; a note in a configured wiki, insights, or notes folder, in the journal, or in an
    /// entity's folder, goes to that screen. Any other Markdown note has no route, and a file that is not Markdown
    /// keeps its plain vault route, which the content route serves.
    #[must_use]
    pub fn route(&self, vault_path: &str, task_id: Option<&str>) -> Option<String> {
        // A folder's managed `index.md` is never shown anywhere in the interface: no screen lists one, and a link to
        // one is not followed.
        if is_managed_index(vault_path) {
            return None;
        }
        // A base a screen presents as its own list opens that list.
        if vault_path == TASKS_BASE {
            return Some(self.urls.tasks());
        }
        let register_base = self.config.settings.register_base.trim_matches('/');
        if !register_base.is_empty() && vault_path == register_base {
            return Some(crate::routes::vault_management::category_href(
                self.urls.vault_name(),
                crate::vault_catalogue::ManagementCategory::Decisions,
                "",
            ));
        }
        if let Some(route) = self.settings_route(vault_path) {
            return Some(route);
        }
        match contextos_markdown::FileKind::from(vault_path) {
            contextos_markdown::FileKind::Note => {
                if self.is_task_note(vault_path) {
                    return task_id.filter(|id| !id.is_empty()).map(|id| self.urls.task(id));
                }
                self.screen_route(vault_path).or_else(|| self.journal_route(vault_path))
            }
            // A base, canvas, or diagram opens on the screen that owns its folder, as a note does; the journal shows
            // days and the tasks folder shows tasks, so neither shows one.
            kind if kind.is_page() => {
                if inside(vault_path, TASKS_DIR).is_some() {
                    return None;
                }
                self.screen_route(vault_path)
            }
            _ => Some(self.urls.file(vault_path)),
        }
    }

    /// The journal screen for the day a journal note's path names under the configured date format.
    fn journal_route(&self, vault_path: &str) -> Option<String> {
        let rest = inside(vault_path, &self.config.journal_dir)?;
        let matched = crate::date_structure::matched_fields(&self.config.journal_date_format, rest)?;
        let field = |token: &str| crate::templater::matched_field(&matched, token);
        let date = time::Date::from_calendar_date(
            i32::try_from(field("YYYY")?).ok()?,
            time::Month::try_from(u8::try_from(field("MM")?).ok()?).ok()?,
            u8::try_from(field("DD")?).ok()?,
        )
        .ok()?;
        Some(self.urls.journal_day(&crate::journal::format_date(date)))
    }

    /// The section page that shows the folder `folder` (vault-relative, no trailing `/`), or `None` when no section
    /// owns it. A folder is never shown as a page of its own outside a section: an entity's folder opens in its entity
    /// browser, a Vault Settings folder on its settings page, and a wiki, insights, notes, journal, or tasks folder on
    /// that screen.
    #[must_use]
    pub fn folder_route(&self, folder: &str) -> Option<String> {
        use crate::routes::vault_management::category_href;
        use crate::vault_catalogue::{
            ManagementCategory, OPERATION_LOG_DIR, SESSIONS_DIR, STANDARDS_DIR, TEMPLATES_DIR,
        };

        let folder = folder.trim_matches('/');
        let vault = self.urls.vault_name();
        let settings = &self.config.settings;
        if !settings.register_dir.is_empty() && at_or_inside(folder, &settings.register_dir).is_some() {
            return Some(category_href(vault, ManagementCategory::Decisions, ""));
        }
        for (slug, dir) in &settings.registry {
            if let Some(rest) = at_or_inside(folder, dir) {
                let location = if rest.is_empty() {
                    slug.clone()
                } else {
                    format!("{slug}/{rest}")
                };
                return Some(category_href(vault, ManagementCategory::Registry, &location));
            }
        }
        for (dir, category) in [
            (STANDARDS_DIR, ManagementCategory::Standards),
            (TEMPLATES_DIR, ManagementCategory::Templates),
        ] {
            if let Some(rest) = at_or_inside(folder, dir) {
                return Some(category_href(vault, category, rest));
            }
        }
        for (dir, source) in [(OPERATION_LOG_DIR, "log"), (SESSIONS_DIR, "sessions")] {
            if at_or_inside(folder, dir).is_some() {
                return Some(category_href(vault, ManagementCategory::Activity, source));
            }
        }
        if at_or_inside(folder, TASKS_DIR).is_some() {
            return Some(self.urls.tasks());
        }
        let screens: [(&str, ScreenUrl); 4] = [
            (self.config.wiki_dir.as_str(), |urls| urls.wiki("")),
            (self.config.insights_dir.as_str(), |urls| urls.insights("")),
            (self.config.notes_dir.as_str(), |urls| urls.notes("")),
            (self.config.journal_dir.as_str(), VaultUrls::journal),
        ];
        let longest = screens
            .into_iter()
            .filter(|(dir, _)| !dir.trim_matches('/').is_empty())
            .filter(|(dir, _)| at_or_inside(folder, dir).is_some())
            .max_by_key(|(dir, _)| dir.len());
        if let Some((_, screen)) = longest {
            return Some(screen(&self.urls));
        }
        let slug = folder.split('/').next()?;
        self.config
            .entity_slugs
            .iter()
            .any(|known| known == slug)
            .then(|| self.urls.entity(folder))
    }

    /// The Vault Settings page that shows `vault_path`: a Decisions register record, a standard, a template, a
    /// registry definition, or an operation log or session summary file.
    fn settings_route(&self, vault_path: &str) -> Option<String> {
        use crate::routes::vault_management::category_href;
        use crate::vault_catalogue::{
            ManagementCategory, OPERATION_LOG_DIR, SESSIONS_DIR, STANDARDS_DIR, TEMPLATES_DIR,
        };

        let vault = self.urls.vault_name();
        let settings = &self.config.settings;
        if !settings.register_dir.is_empty()
            && let Some(record) = inside(vault_path, &settings.register_dir)
                .filter(|record| !record.contains('/'))
                .and_then(|record| record.strip_suffix(".md"))
        {
            return Some(category_href(vault, ManagementCategory::Decisions, record));
        }
        for (slug, dir) in &settings.registry {
            if let Some(rest) = inside(vault_path, dir) {
                return Some(category_href(
                    vault,
                    ManagementCategory::Registry,
                    &format!("{slug}/{rest}"),
                ));
            }
        }
        let libraries = [
            (STANDARDS_DIR, ManagementCategory::Standards, ""),
            (TEMPLATES_DIR, ManagementCategory::Templates, ""),
            (OPERATION_LOG_DIR, ManagementCategory::Activity, "log/"),
            (SESSIONS_DIR, ManagementCategory::Activity, "sessions/"),
        ];
        libraries.into_iter().find_map(|(dir, category, prefix)| {
            inside(vault_path, dir).map(|rest| category_href(vault, category, &format!("{prefix}{rest}")))
        })
    }

    fn screen_route(&self, vault_path: &str) -> Option<String> {
        let folders: [(&str, PageUrl, Shows); 3] = [
            (self.config.wiki_dir.as_str(), VaultUrls::wiki, shows_any),
            (self.config.insights_dir.as_str(), VaultUrls::insights, shows_in_a_shard),
            (self.config.notes_dir.as_str(), VaultUrls::notes, shows_any),
        ];
        let longest = folders
            .into_iter()
            .filter(|(folder, _, _)| !folder.is_empty())
            .filter_map(|(folder, segment, shows)| {
                inside(vault_path, folder).map(|rest| (folder.len(), segment, shows, rest))
            })
            .max_by_key(|(length, _, _, _)| *length);
        if let Some((_, screen, shows, rest)) = longest {
            return shows(rest).then(|| screen(&self.urls, rest));
        }
        let (slug, rest) = vault_path.split_once('/')?;
        (!rest.is_empty() && self.config.entity_slugs.iter().any(|known| known == slug))
            .then(|| self.urls.entity(vault_path))
    }
}

/// A frontmatter value that is exactly one wikilink.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WikiLinkValue {
    pub target: String,
    pub display: Option<String>,
}

/// `raw` was not exactly one wikilink.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NotAWikilink;

/// Reads a frontmatter value as a single wikilink, refusing anything else: the whole value is `[[target]]` or
/// `[[target|display]]`, with nothing around it and no bracket inside it. A heading or block fragment stays part of
/// the target.
impl TryFrom<&str> for WikiLinkValue {
    type Error = NotAWikilink;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let inner = raw
            .trim()
            .strip_prefix("[[")
            .and_then(|rest| rest.strip_suffix("]]"))
            .filter(|inner| !inner.contains(['[', ']', '\n']))
            .ok_or(NotAWikilink)?;
        let (target, display) = match inner.split_once('|') {
            Some((target, display)) => (target, Some(display.to_owned())),
            None => (inner, None),
        };
        let target = target.trim();
        if target.is_empty() {
            return Err(NotAWikilink);
        }
        Ok(Self {
            target: target.to_owned(),
            display,
        })
    }
}

/// The text to show for a link: its alias, or else the last segment of its target without the extension, so a long
/// path never reaches a narrow panel.
impl From<&WikiLinkValue> for String {
    fn from(link: &WikiLinkValue) -> Self {
        link.display.clone().unwrap_or_else(|| {
            let last = link.target.rsplit('/').next().unwrap_or(&link.target);
            last.strip_suffix(".md").unwrap_or(last).to_owned()
        })
    }
}

#[cfg(test)]
#[path = "vault_links_test.rs"]
mod tests;
