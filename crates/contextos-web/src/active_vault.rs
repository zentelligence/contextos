//! The vault a browser was last in: a per-browser cookie, for the pages that have no vault in their address.
//!
//! A vault lives in the URL, and this never changes that: every vault page is still addressed by its own path. This only
//! answers "which vault's sidebar should a shared or settings page show?", so it is the same storage class as
//! [`crate::last_visited`] and the sidebar toggle: ephemeral navigation state, never vault content or a server-side file.
//!
//! The browser sends the value back, so nothing here believes it: a reader checks it against the configured vaults
//! ([`crate::rendering::shell::build_host_nav`]), and one no longer configured counts as no value at all.

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::inbox_outbox::{decode_percent_encoded, encode_query_value};

pub const ACTIVE_VAULT_COOKIE: &str = "cc_active_vault";

/// The first segments of the addresses that belong to the host rather than to a vault.
const HOST_SEGMENTS: [&str; 6] = ["api", "mcp", "settings", "shared", "shell", "static"];

/// The active vault this request's cookie names, if it names one.
#[must_use]
pub fn read_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == ACTIVE_VAULT_COOKIE).then_some(value)
        })
        .and_then(decode_percent_encoded)
        .filter(|vault| !vault.is_empty())
}

/// The vault `path` (a request's raw, percent-encoded path) is in, by its shape alone, or `None` for a host address.
fn vault_of(path: &str) -> Option<String> {
    let first = path.trim_start_matches('/').split('/').next()?;
    if first.is_empty() || HOST_SEGMENTS.contains(&first) {
        return None;
    }
    decode_percent_encoded(first)
}

/// Whether `response` is a page, which only a real vault's address answers with success.
fn is_a_page(response: &Response) -> bool {
    response.status().is_success()
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/html"))
}

/// The `Set-Cookie` value for `vault`, with the same `Path=/; SameSite=Lax` and browser-lifetime default as
/// [`crate::last_visited::set_cookie_header`].
fn set_cookie_header(vault: &str) -> Option<HeaderValue> {
    let encoded = encode_query_value(vault);
    HeaderValue::from_str(&format!("{ACTIVE_VAULT_COOKIE}={encoded}; Path=/; SameSite=Lax")).ok()
}

/// Records the vault of a full page load, applied once as a whole-router layer beside [`crate::last_visited::track`]. A
/// fragment is part of a page already counted, and an unchanged vault needs no new cookie.
pub async fn track(request: Request, next: Next) -> Response {
    let trackable = request.method() == Method::GET && !request.headers().contains_key("hx-request");
    let vault = trackable.then(|| vault_of(request.uri().path())).flatten();
    let existing = read_from_headers(request.headers());
    let mut response = next.run(request).await;
    if let Some(vault) = vault
        && existing.as_deref() != Some(vault.as_str())
        && is_a_page(&response)
        && let Some(cookie) = set_cookie_header(&vault)
    {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}

#[cfg(test)]
#[path = "active_vault_test.rs"]
mod tests;
