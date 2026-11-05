//! Shared fixtures and request helpers for the `notes_*` integration tests. Not a test target itself: a `tests/`
//! subdirectory module is compiled once into each including test crate, and each of those uses only some helpers,
//! so `#![allow(dead_code)]` keeps the rest from warning (the same arrangement as `support`).
#![allow(dead_code)]

use tower::ServiceExt as _;

use crate::support;

pub use axum::Router;
pub use axum::body::{Body, to_bytes};
pub use axum::http::{HeaderMap, Request, StatusCode};
pub use contextos_web::McpClientSet;
pub use std::path::Path;
pub use std::sync::Arc;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub const VAULT_NAME: &str = "contract-fixture";

pub async fn router_over(vault_dir: &Path, config_dir: &Path) -> Result<Router, BoxError> {
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
    ))
}

pub async fn get(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

pub async fn post(router: &Router, path: &str, json_body: &str) -> Result<(StatusCode, String, HeaderMap), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(json_body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned(), headers))
}

pub async fn patch(router: &Router, path: &str, json_body: &str) -> Result<(StatusCode, String, HeaderMap), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(json_body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned(), headers))
}

pub async fn delete(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().method("DELETE").uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

pub fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// Every regular file found anywhere under `dir` (recursive), relative to
/// `dir` itself, forward-slash separated: this crate's own tests have no
/// dependency on a directory-walking crate (`contextos-fs` owns that one),
/// so a brand-new note's own real path - never predictable in advance,
/// since its filename is timestamp-based - is discovered this way instead.
pub fn files_under(dir: &Path) -> std::io::Result<Vec<String>> {
    fn walk(root: &Path, current: &Path, found: &mut Vec<String>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, found)?;
            } else {
                let relative = path.strip_prefix(root).unwrap_or(&path);
                found.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    walk(dir, dir, &mut found)?;
    found.sort();
    Ok(found)
}

/// The note's own path segment from an `HX-Redirect` response header
/// (`/{vault}/notes/{path}`), URL-decoded just enough for these fixtures'
/// own plain (no reserved characters) relative paths.
pub fn redirected_note(headers: &HeaderMap) -> Option<String> {
    let target = headers.get("HX-Redirect")?.to_str().ok()?;
    let (_, path) = target.split_once("/notes/")?;
    Some(path.to_owned())
}

/// The rendered body-save form's own current `expected_hash` hidden field:
/// a real save always presents the value the page last rendered (never
/// blank, once a file exists), exactly as the browser's own form submission
/// would.
pub fn extract_expected_hash(body: &str) -> Option<String> {
    let marker = "name=\"expected_hash\" value=\"";
    let after = body.split(marker).nth(1)?;
    let (hash, _) = after.split_once('"')?;
    Some(hash.to_owned())
}

/// The "New" form's own pre-filled path value (`GET .../new`'s own rendered fragment).
pub fn extract_create_form_path(body: &str) -> Option<String> {
    let marker = "name=\"path\" value=\"";
    let after = body.split(marker).nth(1)?;
    let (path, _) = after.split_once('"')?;
    Some(path.to_owned())
}

pub const UNIQUE_NOTE_TEMPLATE: &str = "---\n\
time: <% tp.date.now(\"HH:mm\") %>\n\
date: <% tp.date.now(\"YYYY-MM-DD\") %>\n\
day: <% tp.date.now(\"dddd\") %>\n\
week: <% tp.date.now(\"ww\") %>\n\
tags: [note]\n\
status: draft\n\
---\n\
\n\
# <% tp.date.now(\"dddd, Do MMMM YYYY\") %>\n\
\n\
\n";

pub fn seed_template(vault_dir: &Path) -> std::io::Result<()> {
    write(vault_dir, "templates/unique-note.md", UNIQUE_NOTE_TEMPLATE)
}

// ---------------------------------------------------------------------
// POST /{{vault_name}}/notes/ - `note_create` itself rejecting the write
// ---------------------------------------------------------------------

/// A template whose rendered `description` is identical to its `title`:
/// `note_create`'s own server-side validation
/// (`contextos_obsidian::NoteError::TitleCollision`) rejects this
/// deterministically, without depending on a filesystem race - the create
/// path's own `disambiguate_relative_path` already defeats any ordinary
/// same-path collision for a sequential request, so a title/description
/// collision is the one `CreateNoteError::Rejected` cause this crate's own
/// template rendering can reach on its own.
pub const TITLE_COLLIDING_TEMPLATE: &str = "---\n\
title: Duplicate title\n\
description: Duplicate title\n\
status: draft\n\
tags: [note]\n\
---\n\
\n\
Body.\n";

// ---------------------------------------------------------------------
// A killed `contextos-mcp` session mid-save/rename
// (`SaveNoteError::Write`/`RenameNoteError::Write`, and, for a save,
// `SaveNoteError::Load(LoadNoteError::Write(_))` - `save_note_body`'s own
// fresh pre-write re-read, which fails first for a session already dead
// before the call ever starts).
//
// Exercised against `notes::save_note_body`/`notes::rename_note`
// themselves, not the HTTP routes: every route handler in
// `routes::notes` calls `ensure_vault` first, which issues its own real
// MCP round trip and so observes and reports (as a `502`) the identical
// killed session before ever reaching these domain functions' own calls -
// see `routes::notes::ensure_vault`. A single request therefore has no
// way to reach `patch_body`/`patch_rename`'s own transport-failure arms
// specifically; this exercises the domain functions directly instead of
// relying on a race between two calls in the one request.
// ---------------------------------------------------------------------

#[cfg(unix)]
pub async fn wait_until_unreachable(client: &contextos_web::McpClient) -> Result<(), BoxError> {
    // The transport does not notice the closed pipe synchronously with the
    // kill signal (`tests/settings_routes.rs`'s own killed-session tests
    // document the identical reasoning); `status()` is a side-effect-free
    // liveness probe, safe to poll without corrupting the fixture the way
    // repeatedly retrying a real mutation would.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if !client.status().await.reachable {
                return;
            }
        }
    })
    .await
    .map_err(|_| "the killed session was never observed as unreachable within the timeout".into())
}
