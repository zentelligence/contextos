//! The full-page shell every route wraps its rendered fragment into for a
//! plain browser navigation (no `HX-Request` header,
//! `docs/http-routing-response-contract.md`): a full-width
//! header (logo/wordmark, vault and entity switchers, breadcrumb, header
//! controls), a collapsible sidebar (grouped navigation) beside the main
//! content, and a full-width footer status bar, around the content area.
//!
//! This module is a pure template renderer: it knows nothing about MCP. Its
//! caller ([`crate::rendering::shell`]) fetches the live data ([`NavData`])
//! this template needs and hands it in already assembled, keeping the
//! async/MCP concern and the template-rendering concern separate, matching
//! this crate's established pattern ([`crate::rendering::canvas`] takes
//! already-fetched node/edge data the identical way).

use askama::Template;

/// The running build's version, shown in the footer's about line.
pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub use crate::config::Appearance;

/// One configured vault, as offered in the nav shell's vault switcher.
pub struct NavVault {
    pub name: String,
    pub is_current: bool,
    /// The `--entity-N` accent (1 to 6) this vault carries in the switcher
    /// and breadcrumb, assigned by position in the `vault_info` order and
    /// wrapping after six. A cue, never an identity
    /// or security boundary.
    pub entity: u8,
}

/// One configured entity, as offered in the sidebar's entity switcher.
/// Distinct from [`NavVault::entity`]: this accent comes from
/// `.contextos/entities.toml`, the vault's own configured entities, never
/// from `vault_info` position.
pub struct NavEntityOption {
    pub slug: String,
    pub name: String,
    /// The `--entity-N` accent this entity carries in the switcher, one of
    /// six defined hues (`--entity-1` to `--entity-6`), assigned by row
    /// order and wrapping back to `--entity-1` past the sixth: a vault may
    /// configure any number of entities (0-n), but the palette itself only
    /// defines six distinct colours to cycle through.
    pub accent: u8,
    pub is_current: bool,
}

/// What a page with no vault in its address is showing, in place of the vault its sidebar belongs to.
pub struct HostView {
    pub icon: &'static str,
    pub label: &'static str,
    /// Which vault the sidebar's links open, or nothing when no vault is configured.
    pub detail: Option<String>,
}

/// One destination in a [`SidebarGroup`]'s own link list (the shell's
/// grouped sidebar navigation: General/Agents/Knowledge/Apps/Workflows
/// sections). Every link here already resolves through a route that exists
/// today, either the general vault content route or an
/// already-implemented screen (Agents); a dedicated, purpose-built screen
/// for a destination like Tasks or Notes, delivered separately later,
/// replaces this generic rendering without changing the link itself.
pub struct SidebarLink {
    pub attention: Option<&'static str>,
    pub label: String,
    pub href: String,
    pub icon: &'static str,
    pub active: bool,
    /// The count shown next to the label ([`crate::nav_counts`]); `None`
    /// renders no badge at all, including for a count that could not be
    /// read.
    pub meta: Option<String>,
    /// The anchor's own `target` attribute: `Some("_blank")` opens this
    /// destination full-screen in a new tab (a `spa`-kind registered app);
    /// `None` renders no `target` attribute at all, an ordinary same-tab
    /// navigation. Every other sidebar destination today is same-tab, so
    /// only the Apps group's own favourited-app links ever set this.
    pub target: Option<&'static str>,
}

/// One section of the sidebar's grouped navigation: `label: None` is the
/// unheaded "General" section; every other section carries a clickable
/// heading (`heading_href`) alongside its own expand/collapse, rendered as
/// a native `<details>`/`<summary>` (`page_shell.html`) rather than a
/// second piece of cookie- or script-driven state alongside the sidebar's
/// own wide/narrow toggle.
pub struct SidebarGroup {
    /// The heading of the part of the sidebar this group starts, for a group that is not the vault's own.
    pub section: Option<&'static str>,
    pub label: Option<&'static str>,
    pub heading_href: Option<String>,
    pub icon: &'static str,
    pub active: bool,
    pub links: Vec<SidebarLink>,
}

/// The title bar at the top of the main content panel: the icon and name of the screen, and the star that
/// un/favourites what the page shows, already rendered by [`crate::rendering::favourite_toggle::render`]. On a
/// screen's own page the star favourites the screen; on a page that shows one file it favourites that file, and a
/// link beside it downloads the file.
pub struct PageTitle {
    pub icon: &'static str,
    pub label: String,
    /// The destination's own action controls ([`crate::rendering::title_action::render`]), shown beside the star;
    /// empty for a destination with none.
    pub actions_html: String,
    /// The link that downloads the file the page shows, beside the star; empty on a page that shows no one file.
    pub download_html: String,
    pub favourite_toggle_html: String,
}

/// The connected `[[mcp_server]]` sessions' aggregate status, for the shell
/// footer's status bar (connected-session count and total tool count
/// across every configured server; the SSE/stream half of that same
/// footer stays unbuilt pending a live event-store design this crate does
/// not yet have, and renders as an inert placeholder rather than invented
/// data). Always present, defaulting to every field zero rather than
/// `Option`-wrapped: an `McpClientSet` exists on every route already, even
/// one configured with zero entries, so there is always a real (if empty)
/// summary to show.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct McpStatus {
    pub connected: usize,
    pub total: usize,
    pub tools: usize,
}

/// One segment of a clickable ancestor trail: the top bar's own breadcrumb
/// ([`NavData::breadcrumb`]), built via
/// [`crate::rendering::shell::breadcrumb_segments`]. `href` is `Some` for
/// every ancestor (ordered from the vault root down)
/// and `None` for exactly the last segment, the current page or directory
/// itself: never a link back to where the reader already is.
pub struct BreadcrumbSegment {
    pub label: String,
    pub href: Option<String>,
}

/// Everything the nav shell needs to render around one page's own content,
/// assembled by [`crate::rendering::shell::build_nav`].
///
/// The five `active_*_screen` fields mirror the shell's five fixed
/// destinations (the sidebar's Dashboard/Agents/Apps/Workflows entries and the
/// header's own Settings icon, `page_shell.html`) one flag apiece, not an
/// arbitrary pile clippy's `struct_excessive_bools` is meant to catch:
/// `build_nav` is this struct's only constructor outside a fallback
/// literal, and it sets every one of them from a single `ActiveScreen`
/// equality check, so an invalid combination (more than one `true`, or
/// none) cannot arise even though the type itself does not forbid it.
#[allow(clippy::struct_excessive_bools)]
pub struct NavData {
    pub vaults: Vec<NavVault>,
    /// `None` for a vault-independent page (`/server/settings/`); the vault
    /// switcher still lists every configured vault.
    pub current_vault: Option<String>,
    /// The vault the header's own wordmark link targets when there is no
    /// current vault: `current_vault` itself when there is one, otherwise
    /// the active vault or, failing that, the first configured vault
    /// ([`crate::rendering::shell::build_host_nav`]), so a vault-independent page like
    /// `/server/settings/` still offers a way back rather than degrading to inert
    /// text merely because the current page has no vault of its own.
    /// `None` only when no vault is configured at all (or none could be
    /// listed), the one case that link has nothing to point at.
    pub nav_target_vault: Option<String>,
    /// The top bar's own clickable ancestor trail for the page currently
    /// showing in `shell-main`: vault root first, the page itself
    /// (unlinked) last. A single unlinked `"settings"` segment on a
    /// vault-independent page.
    pub breadcrumb: Vec<BreadcrumbSegment>,
    pub active_vault_screen: bool,
    pub active_agents_screen: bool,
    pub active_apps_screen: bool,
    pub active_jobs_screen: bool,
    pub active_settings_screen: bool,
    /// The current vault's own `--entity-N` accent (see [`NavEntityOption::accent`]
    /// for the six-hue wrapping rule), so the header's vault switcher can be
    /// tinted with it. `None` on a vault-independent
    /// page (`/server/settings/`).
    pub current_vault_entity: Option<u8>,
    /// The current vault's configured entities (`.contextos/entities.toml`):
    /// empty when the vault has none configured, or there is no current
    /// vault at all. The sidebar renders no entity switcher at all when
    /// this is empty, since a vault may configure zero, one, or many.
    pub entities: Vec<NavEntityOption>,
    /// The selected entity's own slug (the `?entity=` query parameter);
    /// `None` for "All entities", the always-available default scope.
    pub current_entity: Option<String>,
    /// The selected entity's own `--entity-N` accent, so the switcher's
    /// `<select>` can be tinted with it the same way `current_vault_entity`
    /// tints the vault switcher; `None` for "All entities" or when the
    /// selected slug matches no configured entity.
    pub current_entity_accent: Option<u8>,
    /// The current page's own path, carrying no `?entity=` query parameter,
    /// so the entity switcher can compose each option's own href by
    /// appending one; `None` on a vault-independent page (`/server/settings/`),
    /// where no entity switcher renders regardless of `entities`.
    pub entity_base_path: Option<String>,
    /// `web.toml`'s `[server.ui]` theme/font/size/`system_name` (the
    /// `/server/settings/` Appearance pane): `theme`/`font`/`size` are applied as
    /// `<html data-theme="..." data-font="..." data-size="...">` so
    /// `contextos-web.css`'s corresponding attribute selectors take effect
    /// on the very next page render after a save; `system_name` is the
    /// header's own title, next to the logo. Each of `theme`/`font`/`size`
    /// absent (the key unset, non-string, or `web.toml` unreadable) falls
    /// back to the built-in default (system colour scheme, the default
    /// sans-serif stack, the default text size); `system_name` falls back to
    /// [`crate::config::DEFAULT_SYSTEM_NAME`]. Never a broken or
    /// half-applied appearance.
    pub appearance: Appearance,
    /// The sidebar's grouped navigation sections (General, Agents,
    /// Knowledge, Apps, Workflows), in render order. A vault-independent page has
    /// the groups of [`Self::nav_target_vault`], and none when no vault is configured.
    pub sidebar_groups: Vec<SidebarGroup>,
    /// What a page with no vault in its address is showing, since its sidebar is a vault's; `None` on a vault's own page.
    pub host_view: Option<HostView>,
    /// Whether the sidebar renders in its wide (grouped, labelled) form or
    /// its narrow (icon-only) form, read from the toggle's own cookie by
    /// the route handler; `true` (wide) is the default a first-ever visit
    /// (no cookie set yet) renders as.
    pub sidebar_wide: bool,
    /// The connected `[[mcp_server]]` sessions' aggregate status, for the
    /// footer status bar.
    pub mcp_status: McpStatus,
    /// The header's "New note" control target: `notes/` under the current
    /// vault; `None` when there is no current vault to create a note in,
    /// where the control renders disabled instead of linking nowhere.
    pub note_href: Option<String>,
    /// The header's "Profile" control target: the current vault's own
    /// Profile screen (`/{{vault_name}}/profile/`); `None` when there is no
    /// current vault, where the control renders disabled instead.
    pub profile_href: Option<String>,
    /// The header's "Vault Settings" control target: the current vault's
    /// own vault-scoped settings screen (`/{{vault_name}}/settings/`,
    /// distinct from the vault-independent `/server/settings/`); `None` when
    /// there is no current vault, where the control renders disabled
    /// instead.
    pub vault_settings_href: Option<String>,
    /// The main content panel's title bar: `Some` on a page that is a favouritable navigation destination or shows
    /// one file, `None` on every other page, which renders its content with no title bar at all.
    pub page_title: Option<PageTitle>,
    /// Where the shell opens the vault's live event stream, for the footer's stream panel and any region of the page
    /// that updates as events arrive. `None` on a vault-independent page, which has no vault to stream.
    pub events_stream_href: Option<String>,
    /// The vault's favourites, when building the title bar already read them, so a page that then shows a file
    /// reads them no second time.
    pub(crate) favourites: Option<crate::view_favourites::ViewsState>,
}

impl NavData {
    /// Sets the action controls shown in the title bar, beside the star. A page that is not a favouritable
    /// destination has no title bar, so it has nowhere to show them and the call changes nothing.
    pub fn set_title_actions(&mut self, actions_html: String) {
        if let Some(title) = self.page_title.as_mut() {
            title.actions_html = actions_html;
        }
    }
}

#[derive(Template)]
#[template(path = "page_shell.html")]
struct PageShellTemplate<'a> {
    title: &'a str,
    body: &'a str,
    nav: &'a NavData,
}

/// Wraps `body` (an already-rendered content fragment) in the full-page
/// shell, titled `title`, chromed with `nav`.
#[must_use]
pub fn render_page(nav: &NavData, title: &str, body: &str) -> String {
    PageShellTemplate { title, body, nav }
        .render()
        .unwrap_or_else(|_| body.to_owned())
}

#[cfg(test)]
#[path = "page_test.rs"]
mod tests;
