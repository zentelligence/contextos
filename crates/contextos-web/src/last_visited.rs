//! "Resume where I left off": a per-browser cookie recording the last real file selected on the
//! Wiki, Insights, and each configured entity's own folder screen, keyed by vault. Opening one of
//! those screens' own sidebar link (the bare screen root - `routes::wiki`, `routes::insights`,
//! `routes::entity_docs`'s own "entity's own root" case) redirects to that path when it still
//! resolves to a real file, sparing the operator re-navigating a deep folder tree every time.
//!
//! The same storage class as [`crate::recently_accessed`]'s "Recently accessed" cookie and the
//! sidebar wide/narrow toggle's own (`routes::SIDEBAR_COOKIE`): an ephemeral navigation
//! convenience, never vault content, so nothing here goes through the MCP write pipeline - a
//! stale or missing entry (a different browser, a cleared cookie, the visited file since deleted
//! or renamed) degrades to each screen's own existing landing view, never an error. Read and
//! written purely through the `Cookie`/`Set-Cookie` HTTP headers ([`track`], applied as a
//! whole-router `axum::middleware::from_fn` layer, matching `recently_accessed::track`'s identical
//! placement); no client-side script is involved anywhere in this path, keeping this crate's own
//! SSR+HTMX+SSE contract intact.
//!
//! Unlike `recently_accessed`'s single ordered list (a fixed recency window that a Wiki visit
//! would fall out of after a handful of unrelated clicks elsewhere in the Command Centre), this
//! keeps one dedicated slot per (vault, screen) pair - Wiki and Insights each carry exactly one,
//! entity folders one per configured entity - so a screen's own last-visited file survives however
//! much browsing happens elsewhere in between.

use std::collections::BTreeMap;

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use crate::inbox_outbox::decode_percent_encoded;

pub const LAST_VISITED_COOKIE: &str = "cc_last_visited";

/// The whole cookie's own decoded shape: every vault this browser has ever visited a real Wiki,
/// Insights, or entity-folder file in, each carrying at most one remembered path per screen.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LastVisitedState {
    #[serde(default)]
    vaults: BTreeMap<String, VaultLastVisited>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
struct VaultLastVisited {
    #[serde(default)]
    wiki: Option<String>,
    #[serde(default)]
    insights: Option<String>,
    /// Entity slug to that entity's own last-visited file, one entry per configured entity the
    /// operator has actually opened a file in - never pre-populated for every configured entity,
    /// only the ones actually visited.
    #[serde(default)]
    entity: BTreeMap<String, String>,
}

/// Which of the three screens a visit belongs to - entity folders carry their own slug, since
/// each configured entity's own folder tracks its own last-visited file independently.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VisitedScreen {
    Wiki,
    Insights,
    Entity(String),
}

/// Reads the cookie's own JSON object. Degrades to [`LastVisitedState::default`] for an absent,
/// malformed, or otherwise unparseable cookie value rather than failing a screen's own render -
/// matching `recently_accessed::read_from_headers`'s identical "convenience list, not state the
/// operator depends on for correctness" contract.
#[must_use]
pub fn read_from_headers(headers: &HeaderMap) -> LastVisitedState {
    let Some(raw) = headers.get(header::COOKIE).and_then(|value| value.to_str().ok()) else {
        return LastVisitedState::default();
    };
    raw.split(';')
        .find_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let name = parts.next()?.trim();
            let value = parts.next()?.trim();
            (name == LAST_VISITED_COOKIE).then_some(value)
        })
        .and_then(decode_percent_encoded)
        .and_then(|decoded| serde_json::from_str::<LastVisitedState>(&decoded).ok())
        .unwrap_or_default()
}

#[must_use]
pub fn wiki_last_visited<'a>(state: &'a LastVisitedState, vault_name: &str) -> Option<&'a str> {
    state.vaults.get(vault_name)?.wiki.as_deref()
}

#[must_use]
pub fn insights_last_visited<'a>(state: &'a LastVisitedState, vault_name: &str) -> Option<&'a str> {
    state.vaults.get(vault_name)?.insights.as_deref()
}

#[must_use]
pub fn entity_last_visited<'a>(state: &'a LastVisitedState, vault_name: &str, entity_slug: &str) -> Option<&'a str> {
    state
        .vaults
        .get(vault_name)?
        .entity
        .get(entity_slug)
        .map(String::as_str)
}

/// Records `relative_path` as `vault_name`'s own latest visit to `screen`, replacing whatever that
/// (vault, screen) pair carried before - every other vault and screen slot is left untouched.
#[must_use]
pub fn with_visit(
    mut state: LastVisitedState,
    vault_name: &str,
    screen: VisitedScreen,
    relative_path: String,
) -> LastVisitedState {
    let entry = state.vaults.entry(vault_name.to_owned()).or_default();
    match screen {
        VisitedScreen::Wiki => entry.wiki = Some(relative_path),
        VisitedScreen::Insights => entry.insights = Some(relative_path),
        VisitedScreen::Entity(slug) => {
            entry.entity.insert(slug, relative_path);
        }
    }
    state
}

/// Builds the `Set-Cookie` header value for `state`: the same `Path=/; SameSite=Lax` attributes
/// [`crate::routes::shell::toggle`] and `recently_accessed::set_cookie_header` already use, kept
/// with no `Max-Age` (a session-length default would defeat the point of a feature meant to save
/// re-navigation *between* sessions) but also no explicit long expiry here - a browser's own
/// "persistent until cleared" cookie default already matches what the appearance
/// settings and the sidebar toggle both rely on for their own long-lived state.
#[must_use]
pub fn set_cookie_header(state: &LastVisitedState) -> Option<HeaderValue> {
    let json = serde_json::to_string(state).ok()?;
    let encoded = crate::inbox_outbox::encode_query_value(&json);
    HeaderValue::from_str(&format!("{LAST_VISITED_COOKIE}={encoded}; Path=/; SameSite=Lax")).ok()
}

/// One classified visit, derived purely from a request path's own shape - [`classify`]'s result.
pub struct Visit {
    pub vault: String,
    pub screen: VisitedScreen,
    pub relative_path: String,
}

/// Derives a [`Visit`] from `path` (a request's own raw, percent-encoded `Uri::path()`), `None`
/// for anything that is not a real file's own read/edit address on one of the three tracked
/// screens: a folder browse, the reserved `/new`/`/confirm-delete`/`/rename` sub-paths, or a path
/// outside `{{wiki,insights,entity}}` entirely. This is a URL-shape check only - never a
/// vault lookup - so a path naming a file that has since been deleted or renamed still classifies;
/// the screens that read this state back re-validate against the real tree themselves
/// ([`crate::routes::wiki::render_wiki_fragment`] and its Insights/entity-folder equivalents
/// already do this for every other purpose), degrading to their own ordinary landing view rather
/// than redirecting to a dead address.
#[must_use]
pub fn classify(path: &str) -> Option<Visit> {
    let trimmed = path.trim_start_matches('/');
    let (vault, rest) = trimmed.split_once('/')?;
    if let Some(wiki_rest) = rest.strip_prefix("wiki/") {
        return Some(Visit {
            vault: vault.to_owned(),
            screen: VisitedScreen::Wiki,
            relative_path: file_relative_path(wiki_rest)?,
        });
    }
    if let Some(insights_rest) = rest.strip_prefix("insights/") {
        return Some(Visit {
            vault: vault.to_owned(),
            screen: VisitedScreen::Insights,
            relative_path: file_relative_path(insights_rest)?,
        });
    }
    if let Some(entity_rest) = rest.strip_prefix("entity/") {
        let (slug, file_rest) = entity_rest.split_once('/')?;
        return Some(Visit {
            vault: vault.to_owned(),
            screen: VisitedScreen::Entity(decode_percent_encoded(slug)?),
            relative_path: file_relative_path(file_rest)?,
        });
    }
    None
}

/// `raw` (the URL segment(s) after the screen, and entity slug where present) as a real file's own
/// relative path: strips a trailing `/edit` (an edit-form visit counts as visiting the same file,
/// matching this feature's own "opened, viewed, or edited" scope), then requires what remains to
/// end in `.md` once decoded - the one shape every real page/note/entity-file address on these
/// three screens shares, and a folder browse or a reserved `/new`/`/confirm-delete` sub-path never
/// does. `None` for anything else, including an empty remainder (a screen or entity root).
fn file_relative_path(raw: &str) -> Option<String> {
    let stripped = raw.strip_suffix("/edit").unwrap_or(raw);
    if stripped.is_empty() {
        return None;
    }
    let decoded = decode_percent_encoded(stripped)?;
    decoded.to_ascii_lowercase().ends_with(".md").then_some(decoded)
}

/// Records `request`'s own path as its vault/screen's latest visit, applied once as a whole-router
/// `axum::middleware::from_fn` layer (`app::build_router`), matching `recently_accessed::track`'s
/// identical placement and "full top-level `GET` only" filter: every sidebar tree link on these
/// three screens is a plain `<a href>`, never an HTMX fragment request
/// (`routes::wiki_view::flatten`'s own `TreePage::href`), so a real file selection always arrives
/// here as exactly the request this middleware inspects.
pub async fn track(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let trackable = request.method() == Method::GET && !request.headers().contains_key("hx-request");
    let existing = trackable.then(|| read_from_headers(request.headers()));
    let visit = trackable.then(|| classify(&path)).flatten();
    let mut response = next.run(request).await;
    if let (Some(existing), Some(visit)) = (existing, visit)
        && response.status().is_success()
    {
        let updated = with_visit(existing, &visit.vault, visit.screen, visit.relative_path);
        if let Some(cookie) = set_cookie_header(&updated) {
            response.headers_mut().insert(header::SET_COOKIE, cookie);
        }
    }
    response
}

#[cfg(test)]
#[path = "last_visited_test.rs"]
mod tests;
