//! A file answered as it is stored, from whichever address names it: the generic route for a file no screen shows,
//! and each screen for a file in its own folder. `?raw=1` asks for any file this way, a page included, which is what
//! a page's download link is. A file that is not a page (an image, a PDF) is always answered this way, since no
//! screen has a view of its own for one.

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};

use super::vault::{
    Attached, AttachedBody, asset_response, error_response, fetch_attached, html_response, not_found, unreachable,
};
use crate::mcp_client::{McpCallError, McpClient};
use crate::rendering::base;

/// Whether a request asks for the file itself (`?raw=1`) rather than the page that shows it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Raw(pub bool);

/// A `raw` value other than `1`.
#[derive(Debug, Eq, PartialEq)]
pub struct MalformedRaw;

/// `raw` as a query gives it: absent asks for the page, `1` for the file, and anything else is refused rather than
/// read as either.
impl TryFrom<Option<&str>> for Raw {
    type Error = MalformedRaw;

    fn try_from(value: Option<&str>) -> Result<Self, Self::Error> {
        match value {
            None => Ok(Self(false)),
            Some("1") => Ok(Self(true)),
            Some(_) => Err(MalformedRaw),
        }
    }
}

/// A `raw` or `view` parameter the interface never sends: a `raw` other than `1`, or either given twice.
#[derive(Debug, Eq, PartialEq)]
pub struct MalformedQuery;

impl IntoResponse for MalformedQuery {
    fn into_response(self) -> Response {
        malformed_query()
    }
}

/// The response to a [`MalformedQuery`]: `400`, `route/malformed-query`.
pub(crate) fn malformed_query() -> Response {
    error_response(StatusCode::BAD_REQUEST, "route/malformed-query")
}

/// What a request to a screen's address asks of the file there: the file itself (`?raw=1`), or, from a base's view
/// tabs (an htmx request naming a `view`), that view alone.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FileRequest {
    pub raw: Raw,
    pub base_view: Option<String>,
}

#[derive(serde::Deserialize)]
struct FileQuery {
    raw: Option<String>,
    view: Option<String>,
}

impl<S: Send + Sync> FromRequestParts<S> for FileRequest {
    type Rejection = MalformedQuery;

    fn from_request_parts(parts: &mut Parts, _state: &S) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        // Any other parameter belongs to the screen, so only these two are this request's to refuse: a `raw` other
        // than `1`, or either given twice.
        let request = axum::extract::Query::<FileQuery>::try_from_uri(&parts.uri)
            .map_err(|_| MalformedQuery)
            .and_then(|query| {
                let raw = Raw::try_from(query.0.raw.as_deref()).map_err(|MalformedRaw| MalformedQuery)?;
                let htmx = parts.headers.contains_key("hx-request");
                Ok(Self {
                    raw,
                    base_view: query.0.view.filter(|view| htmx && !view.is_empty()),
                })
            });
        std::future::ready(request)
    }
}

/// What a screen answers for the file at `path` (vault-relative) before it shows anything: the file as stored when
/// the request asks for it or the file is not a page, and a base's view alone when its tabs ask for one. `None` leaves
/// the request to the screen, including a path with no extension (a folder, or one of the screen's own actions) and
/// a path that names no file, which the screen then shows or refuses as it always does.
pub(crate) async fn before_screen(
    client: &McpClient,
    vault_name: &str,
    path: &str,
    request: &FileRequest,
) -> Option<Response> {
    let kind = contextos_markdown::FileKind::from(path);
    if let Some(view) = request.base_view.as_deref()
        && kind == contextos_markdown::FileKind::Base
        && !request.raw.0
    {
        return Some(match base::render_view(client, vault_name, path, Some(view)).await {
            Ok(fragment) => html_response(fragment),
            Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
        });
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let has_extension = name.rsplit_once('.').is_some_and(|(stem, _)| !stem.is_empty());
    let raw = request.raw;
    if !raw.0 && (!has_extension || kind.is_page()) {
        return None;
    }
    match fetch_attached(client, format!("{vault_name}://{path}")).await {
        Ok(Attached::Found { content_type, body }) => Some(found(content_type, body, raw)),
        Ok(Attached::NotFound) if raw.0 => Some(not_found()),
        Ok(Attached::NotFound) => None,
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Some(unreachable()),
    }
}

/// The file at `path` (vault-relative) as stored, or not found.
pub(crate) async fn stored(client: &McpClient, vault_name: &str, path: &str, raw: Raw) -> Response {
    match fetch_attached(client, format!("{vault_name}://{path}")).await {
        Ok(Attached::Found { content_type, body }) => found(content_type, body, raw),
        Ok(Attached::NotFound) => not_found(),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}

/// Whether a file, not a folder, is at `path` (vault-relative).
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the transport fails.
pub(crate) async fn is_file(client: &McpClient, vault_name: &str, path: &str) -> Result<bool, McpCallError> {
    #[derive(serde::Deserialize)]
    struct FileInfo {
        kind: String,
    }
    let mut args = serde_json::Map::new();
    args.insert(
        "path".to_owned(),
        serde_json::Value::String(format!("{vault_name}://{path}")),
    );
    let result = client.call_tool("fs_get_file_info".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Ok(false);
    }
    Ok(result.into_typed::<FileInfo>().is_ok_and(|info| info.kind == "file"))
}

/// Text asked for raw is plain text, so a note or an HTML file is read, never rendered or run; anything else keeps
/// its own type, under the sandbox [`asset_response`] gives any type that could run script.
fn found(content_type: String, body: AttachedBody, raw: Raw) -> Response {
    match body {
        AttachedBody::Text(text) if raw.0 => (
            StatusCode::OK,
            [
                ("content-type", "text/plain; charset=utf-8"),
                ("x-content-type-options", "nosniff"),
            ],
            text,
        )
            .into_response(),
        body => asset_response(content_type, body),
    }
}

#[cfg(test)]
#[path = "file_response_test.rs"]
mod tests;
