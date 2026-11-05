//! `POST`/`PATCH`/`PUT`/`DELETE /{{vault_name}}/{{relative-path}}`:
//! HTMX-driven mutation of vault content, dispatched to the
//! MCP tool appropriate to the file's type and the specific edit, never a
//! direct write from `contextos-web` itself. A file type or edit with no
//! defined mutation path (a `.canvas` file, read-only here)
//! returns `405`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClient};
use crate::routes::vault::{VaultRoutesState, extension_of};

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
}

fn error(status: StatusCode, code: &'static str) -> Response {
    (status, Json(ErrorBody { error: code })).into_response()
}

fn method_not_allowed() -> Response {
    error(StatusCode::METHOD_NOT_ALLOWED, "route/method-not-allowed")
}

fn malformed_body() -> Response {
    error(StatusCode::BAD_REQUEST, "route/malformed-body")
}

fn unreachable() -> Response {
    error(StatusCode::BAD_GATEWAY, "mcp/unreachable")
}

/// `POST`/`PATCH`/`PUT`/`DELETE /{{vault_name}}/{{relative-path}}`.
pub async fn mutate(
    State(state): State<VaultRoutesState>,
    Path((vault_name, relative_path)): Path<(String, String)>,
    method: Method,
    body: axum::body::Bytes,
) -> Response {
    let client = match crate::routes::primary_session(&state.clients) {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let extension = extension_of(&relative_path).to_ascii_lowercase();
    match extension.as_str() {
        "md" => mutate_md(&client, &vault_name, &relative_path, &method, &body).await,
        "base" => mutate_base(&client, &vault_name, &relative_path, &method, &body).await,
        "canvas" => method_not_allowed(),
        _ => mutate_generic(&client, &vault_name, &relative_path, &method).await,
    }
}

/// A `.md` route's own two distinct mutation shapes: editing frontmatter
/// (`patch`, dispatched to `frontmatter_update`) or replacing the body text
/// verbatim (`body`, dispatched to `fs_edit_file` as a single whole-body
/// edit). The two are structurally distinguishable (`patch` and `body` are
/// each other's own required field), never conflated by shape.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MdMutationBody {
    Frontmatter {
        patch: Map<String, Value>,
        #[serde(default)]
        expected_hash: Option<String>,
    },
    Body {
        body: BodyEdit,
        #[serde(default)]
        expected_hash: Option<String>,
    },
}

/// A whole-body replacement: `old` is the body text the editor loaded the
/// page with, `new` is the edited text. Submitting the full previous body
/// (rather than a computed diff) lets this dispatch through `fs_edit_file`'s
/// existing exact-match edit contract unchanged, with `expected_hash`
/// providing the real conflict guard at the whole-file level.
#[derive(Debug, Deserialize)]
struct BodyEdit {
    old: String,
    new: String,
}

async fn mutate_md(
    client: &McpClient,
    vault_name: &str,
    relative_path: &str,
    method: &Method,
    body: &[u8],
) -> Response {
    match *method {
        Method::PATCH | Method::PUT => {
            let Ok(parsed) = serde_json::from_slice::<MdMutationBody>(body) else {
                return malformed_body();
            };
            match parsed {
                MdMutationBody::Frontmatter { patch, expected_hash } => {
                    let mut args = Map::new();
                    args.insert(
                        "path".to_owned(),
                        Value::String(format!("{vault_name}://{relative_path}")),
                    );
                    args.insert("patch".to_owned(), Value::Object(patch));
                    if let Some(hash) = expected_hash {
                        args.insert("expected_hash".to_owned(), Value::String(hash));
                    }
                    forward(client, "frontmatter_update", args).await
                }
                MdMutationBody::Body {
                    body: edit,
                    expected_hash,
                } => {
                    let mut args = Map::new();
                    args.insert(
                        "path".to_owned(),
                        Value::String(format!("{vault_name}://{relative_path}")),
                    );
                    let mut one_edit = Map::new();
                    one_edit.insert("old_text".to_owned(), Value::String(edit.old));
                    one_edit.insert("new_text".to_owned(), Value::String(edit.new));
                    args.insert("edits".to_owned(), Value::Array(vec![Value::Object(one_edit)]));
                    if let Some(hash) = expected_hash {
                        args.insert("expected_hash".to_owned(), Value::String(hash));
                    }
                    forward(client, "fs_edit_file", args).await
                }
            }
        }
        Method::DELETE => delete_file(client, vault_name, relative_path).await,
        _ => method_not_allowed(),
    }
}

/// `.base`'s own route carries two distinct mutation targets
///: editing a matched row's content
/// (`target = "row"`) dispatches `frontmatter_update` against that row's
/// own note path, never the `.base` file's path; editing the view
/// definition itself (`target = "definition"`) dispatches `base_apply`
/// against the `.base` file's own path. The two are never conflated.
#[derive(Debug, Deserialize)]
#[serde(tag = "target", rename_all = "lowercase")]
enum BaseMutationBody {
    Row {
        note_path: String,
        patch: Map<String, Value>,
        #[serde(default)]
        expected_hash: Option<String>,
    },
    Definition {
        operations: Vec<Value>,
        #[serde(default)]
        expected_hash: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
struct DefinitionOnlyBody {
    operations: Vec<Value>,
    #[serde(default)]
    expected_hash: Option<String>,
}

async fn mutate_base(
    client: &McpClient,
    vault_name: &str,
    relative_path: &str,
    method: &Method,
    body: &[u8],
) -> Response {
    match *method {
        Method::POST | Method::PATCH | Method::PUT => {
            let Ok(parsed) = serde_json::from_slice::<BaseMutationBody>(body) else {
                return malformed_body();
            };
            match parsed {
                BaseMutationBody::Row {
                    note_path,
                    patch,
                    expected_hash,
                } => {
                    let mut args = Map::new();
                    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{note_path}")));
                    args.insert("patch".to_owned(), Value::Object(patch));
                    if let Some(hash) = expected_hash {
                        args.insert("expected_hash".to_owned(), Value::String(hash));
                    }
                    forward(client, "frontmatter_update", args).await
                }
                BaseMutationBody::Definition {
                    operations,
                    expected_hash,
                } => apply_base_definition(client, vault_name, relative_path, operations, expected_hash).await,
            }
        }
        // A `.base` route's own DELETE removes a filter/view/formula from
        // the definition (`base_apply`'s `remove_*` operations), never a
        // matched note: deleting a note is an operation on that note's own
        // route instead.
        Method::DELETE => {
            let Ok(parsed) = serde_json::from_slice::<DefinitionOnlyBody>(body) else {
                return malformed_body();
            };
            apply_base_definition(
                client,
                vault_name,
                relative_path,
                parsed.operations,
                parsed.expected_hash,
            )
            .await
        }
        _ => method_not_allowed(),
    }
}

async fn apply_base_definition(
    client: &McpClient,
    vault_name: &str,
    relative_path: &str,
    operations: Vec<Value>,
    expected_hash: Option<String>,
) -> Response {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{relative_path}")),
    );
    args.insert("operations".to_owned(), Value::Array(operations));
    if let Some(hash) = expected_hash {
        args.insert("expected_hash".to_owned(), Value::String(hash));
    }
    forward(client, "base_apply", args).await
}

/// Any file type with no dedicated mutation path (`.mermaid`, or any other
/// extension) still accepts `DELETE`, since `fs_delete_file` operates on
/// any file; `POST`/`PATCH`/`PUT` have no defined target here and are `405`.
async fn mutate_generic(client: &McpClient, vault_name: &str, relative_path: &str, method: &Method) -> Response {
    match *method {
        Method::DELETE => delete_file(client, vault_name, relative_path).await,
        _ => method_not_allowed(),
    }
}

async fn delete_file(client: &McpClient, vault_name: &str, relative_path: &str) -> Response {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{relative_path}")),
    );
    forward(client, "fs_delete_file", args).await
}

/// Calls `tool_name` and relays whatever the tool reports (success or an
/// MCP-level tool error) as `200`, the same non-conflation-of-transport-
/// versus-tool-error contract the MCP proxy route uses; only an
/// unreachable transport is `502`.
async fn forward(client: &McpClient, tool_name: &str, args: Map<String, Value>) -> Response {
    match client.call_tool(tool_name.to_owned(), args).await {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}

#[cfg(test)]
#[path = "vault_mutations_test.rs"]
mod tests;
