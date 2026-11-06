//! The header Favourites panel's own "Recently accessed" section: a per-browser cookie recording the last few
//! pages visited, capped at six entries, most-recent-first. An ephemeral navigation convenience, the same storage
//! class as the sidebar wide/narrow toggle's own cookie (`routes::SIDEBAR_COOKIE`); never vault content, so
//! nothing here goes through the MCP write pipeline.

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::Response;

pub const RECENTLY_ACCESSED_COOKIE: &str = "cc_recent";

/// Matches the mock's own client-side cap for this section.
const MAX_ENTRIES: usize = 6;

/// One visited page: a human label and the path to return to it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecentEntry {
    pub label: String,
    pub href: String,
}

/// Reads the cookie's own JSON array. Degrades to an empty list for an absent, malformed, or
/// otherwise unparseable cookie value rather than failing the panel render: this is a convenience
/// list, not state the operator depends on for correctness.
#[must_use]
pub fn read_from_headers(headers: &HeaderMap) -> Vec<RecentEntry> {
    let Some(raw) = headers.get(header::COOKIE).and_then(|value| value.to_str().ok()) else {
        return Vec::new();
    };
    raw.split(';')
        .find_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let name = parts.next()?.trim();
            let value = parts.next()?.trim();
            (name == RECENTLY_ACCESSED_COOKIE).then_some(value)
        })
        .and_then(crate::inbox_outbox::decode_percent_encoded)
        .and_then(|decoded| serde_json::from_str::<Vec<RecentEntry>>(&decoded).ok())
        .unwrap_or_default()
}

/// Pushes `entry` to the front of `current`, moving it there (deduped by `href`) if it was
/// already present, and drops the oldest entries past [`MAX_ENTRIES`].
#[must_use]
pub fn with_visit(current: Vec<RecentEntry>, entry: RecentEntry) -> Vec<RecentEntry> {
    let mut next = Vec::with_capacity(MAX_ENTRIES);
    let href = entry.href.clone();
    next.push(entry);
    for existing in current {
        if existing.href != href && next.len() < MAX_ENTRIES {
            next.push(existing);
        }
    }
    next
}

/// Builds the `Set-Cookie` header value for `entries`: the same `Path=/; SameSite=Lax`
/// attributes [`crate::routes::shell::toggle`] already sets on the sidebar toggle's own cookie.
#[must_use]
pub fn set_cookie_header(entries: &[RecentEntry]) -> Option<HeaderValue> {
    let json = serde_json::to_string(entries).ok()?;
    let encoded = crate::inbox_outbox::encode_query_value(&json);
    HeaderValue::from_str(&format!("{RECENTLY_ACCESSED_COOKIE}={encoded}; Path=/; SameSite=Lax")).ok()
}

/// Whether `path` (a request's own `Uri::path()`) is a page worth remembering: excludes the static asset
/// service, the shell-chrome-only routes (the sidebar toggle and this panel's own data route), and the MCP proxy,
/// none of which is a destination an operator would want to revisit from this list.
#[must_use]
pub fn is_trackable_path(path: &str) -> bool {
    let trimmed = path.trim_start_matches('/');
    !(trimmed.starts_with("static/")
        || trimmed.starts_with("shell/")
        || trimmed.starts_with("mcp/")
        || trimmed.ends_with("/events/stream")
        || trimmed.is_empty())
}

/// A human label for `path`, derived from its own last segment: the fixed vault-independent `/settings/` page is
/// named directly; a bare `/{{vault_name}}/` (no segment past the vault name) is the vault's own Dashboard, the
/// one case [`crate::rendering::shell::breadcrumb_segments`] also special-cases; every other path is labelled
/// from its own final segment, hyphens and underscores read as word breaks.
#[must_use]
pub fn label_for_path(path: &str) -> String {
    let trimmed = path.trim_matches('/');
    if trimmed == "settings" {
        return "Settings".to_owned();
    }
    if trimmed.is_empty() || !trimmed.contains('/') {
        return "Dashboard".to_owned();
    }
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    last.split(['-', '_'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Records `request`'s own path as the most recently accessed page, applied once as a whole-router
/// `axum::middleware::from_fn` layer (`app::build_router`) rather than threaded through every route handler
/// individually: every full-page navigation (a plain `GET`, no `hx-request` header, matching the identical
/// full-page/fragment split every route already renders through) updates the cookie; an HTMX fragment request
/// (a filter change, a form re-render) is not itself a distinct "page" the operator navigated to, so it is left
/// untouched, and neither is a non-`GET` request or one [`is_trackable_path`] excludes.
pub async fn track(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let should_track =
        request.method() == Method::GET && !request.headers().contains_key("hx-request") && is_trackable_path(&path);
    let existing = should_track.then(|| read_from_headers(request.headers()));
    let mut response = next.run(request).await;
    if let Some(existing) = existing
        && response.status().is_success()
    {
        let updated = with_visit(
            existing,
            RecentEntry {
                label: label_for_path(&path),
                href: path,
            },
        );
        if let Some(cookie) = set_cookie_header(&updated) {
            response.headers_mut().append(header::SET_COOKIE, cookie);
        }
    }
    response
}

#[cfg(test)]
#[path = "recently_accessed_test.rs"]
mod tests;
