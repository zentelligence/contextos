//! A single reusable star toggle button, shared by the Apps and Jobs list cards and the Job detail screen: an
//! `hx-patch` form targeting the entity's own resource path (`PATCH /{vault}/apps/{slug}`,
//! `PATCH /{vault}/workflows/{workflow_slug}/jobs/{slug}`, per `docs/http-routing-response-contract.md`'s "submit partial
//! update" row),
//! sending the explicit next boolean value as a JSON body (`hx-ext="json-enc"`) rather than asking the server
//! to flip whatever the field currently holds: idempotent, and consistent with every other partial update this
//! contract describes. `hx-target="this" hx-swap="outerHTML"` swaps only the button's own new state, so a click
//! never re-renders the whole list or needs to know which surface (list card, detail page) submitted it.
//! `routes::apps::set_favourite` and `routes::jobs::set_favourite` render this same partial back as their own
//! HTMX response body.

use askama::Template;
use serde::Deserialize;

/// The star toggle's own PATCH request body (`docs/http-routing-response-contract.md`'s "submit partial
/// update" row): the client sends the explicit next value, never asking the server to flip whatever the field
/// currently holds. Shared by `routes::apps::set_favourite` and `routes::jobs::set_favourite`, the two
/// handlers this same rendered button ever posts back to.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FavouriteUpdate {
    pub favourite: bool,
}

#[derive(Template)]
#[template(path = "favourite_toggle.html")]
struct FavouriteToggleTemplate<'a> {
    action_href: &'a str,
    name: &'a str,
    favourite: bool,
}

/// Renders the star toggle for a resource named `name` (used only in the button's own accessible label, never
/// shown as visible text), whose own resource path is `action_href` (a bare `PATCH /{vault}/.../{slug}`, not an
/// `/execute/...` sub-path), currently in state `favourite`. The button always submits `!favourite` as the
/// explicit next value: clicking a pressed star un-favourites, clicking an unpressed one favourites.
#[must_use]
pub fn render(action_href: &str, name: &str, favourite: bool) -> String {
    FavouriteToggleTemplate {
        action_href,
        name,
        favourite,
    }
    .render()
    .unwrap_or_default()
}

/// A successful update refreshes the shared panel without navigating away.
pub(crate) fn response(action_href: &str, name: &str, favourite: bool) -> axum::response::Response {
    let mut response = crate::routes::vault::html_response(render(action_href, name, favourite));
    response
        .headers_mut()
        .insert("hx-trigger", axum::http::HeaderValue::from_static("favourites-changed"));
    response
}
