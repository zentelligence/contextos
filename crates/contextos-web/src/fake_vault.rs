//! A vault served in process over MCP from a map of files, for tests of what the host reads from a vault and how it
//! reacts when the server cannot answer. It answers the three reads the host makes of workflow and library files
//! (`fs_list_directory`, `fs_get_file_info`, `fs_attach_file`) and, when told what to report, `vault_info`. It records
//! every call, and can be told to refuse or garble a listing, to refuse a file, or to drop the connection.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

use crate::mcp_client::McpClient;

/// How the vault answers a request to list a folder.
#[derive(Clone, Debug, Default)]
pub(crate) enum Listing {
    /// The folder's entries, or a refusal with `path/not-found` when nothing is beneath it.
    #[default]
    Honest,
    /// A refusal with this stable code.
    Refused(&'static str),
    /// An answer that is not a listing.
    Garbled,
}

#[derive(Debug, Default)]
struct State {
    files: BTreeMap<String, String>,
    listing: Listing,
    /// Paths whose read is refused with `io/unreadable`, whatever their details say.
    unreadable: Vec<String>,
    /// Paths whose read is refused with the stable code given, such as `io/too-large`.
    refused: BTreeMap<String, &'static str>,
    /// Calls to answer before every later call fails as a dropped connection, when set.
    calls_before_drop: Option<usize>,
    calls: Vec<String>,
    /// What `vault_info` answers, or a refusal when none is set.
    vault_info: Option<Value>,
}

/// The vault, shared between the test and the server task that answers for it.
#[derive(Clone, Default)]
pub(crate) struct FakeVault(Arc<Mutex<State>>);

impl FakeVault {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Puts `text` at `path`, relative to the vault root, such as `registry/libraries/crm/library.toml`.
    #[must_use]
    pub(crate) fn with_file(self, path: &str, text: &str) -> Self {
        self.edit(|state| {
            state.files.insert(path.to_owned(), text.to_owned());
        });
        self
    }

    /// Answers `vault_info` with `report`, as the server would for the vaults it serves.
    #[must_use]
    pub(crate) fn with_vault_info(self, report: Value) -> Self {
        self.edit(|state| state.vault_info = Some(report));
        self
    }

    #[must_use]
    pub(crate) fn listing(self, listing: Listing) -> Self {
        self.edit(|state| state.listing = listing);
        self
    }

    #[must_use]
    pub(crate) fn unreadable(self, path: &str) -> Self {
        self.edit(|state| state.unreadable.push(path.to_owned()));
        self
    }

    /// Refuses a read of `path` with the stable `code`, whatever the file holds.
    #[must_use]
    pub(crate) fn refusing(self, path: &str, code: &'static str) -> Self {
        self.edit(|state| {
            state.refused.insert(path.to_owned(), code);
        });
        self
    }

    /// Answers `calls` calls and then drops the connection for every later one.
    #[must_use]
    pub(crate) fn dropping_after(self, calls: usize) -> Self {
        self.edit(|state| state.calls_before_drop = Some(calls));
        self
    }

    /// Every call answered so far, as `tool path`, in order.
    pub(crate) fn calls(&self) -> Vec<String> {
        self.edit(|state| state.calls.clone())
    }

    /// How many times `tool` was called for a path that begins with `prefix`.
    pub(crate) fn calls_to(&self, tool: &str, prefix: &str) -> usize {
        let wanted = format!("{tool} {prefix}");
        self.calls().iter().filter(|call| call.starts_with(&wanted)).count()
    }

    fn edit<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        let mut state = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        change(&mut state)
    }

    /// A client over a session served by this vault.
    pub(crate) async fn session(&self) -> Result<McpClient, Box<dyn std::error::Error>> {
        let (client_io, server_io) = tokio::io::duplex(1 << 20);
        let server = self.clone();
        tokio::spawn(async move {
            if let Ok(running) = server.serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        Ok(McpClient::over("contextos", ().serve(client_io).await?))
    }

    fn answer(&self, tool: &str, path: &str) -> Result<CallToolResult, ErrorData> {
        self.edit(|state| {
            if state.calls_before_drop.is_some_and(|left| state.calls.len() >= left) {
                state.calls.push(format!("{tool} {path} (dropped)"));
                return Err(ErrorData::internal_error("the connection dropped", None));
            }
            state.calls.push(format!("{tool} {path}"));
            let relative = path
                .split_once("://")
                .map_or(path, |(_, rest)| rest)
                .trim_end_matches('/');
            Ok(match tool {
                "fs_list_directory" => list(state, relative),
                "fs_get_file_info" => info(state, relative),
                "fs_attach_file" => attach(state, relative),
                "vault_info" => state
                    .vault_info
                    .clone()
                    .map_or_else(|| refusal("tool/unavailable"), CallToolResult::structured),
                _ => refusal("tool/unknown"),
            })
        })
    }
}

fn refusal(code: &str) -> CallToolResult {
    CallToolResult::structured_error(json!({ "code": code, "message": "refused" }))
}

fn list(state: &State, relative: &str) -> CallToolResult {
    match &state.listing {
        Listing::Refused(code) => return refusal(code),
        Listing::Garbled => return CallToolResult::structured(json!({ "unexpected": true })),
        Listing::Honest => {}
    }
    let prefix = format!("{relative}/");
    let mut entries: BTreeMap<String, &str> = BTreeMap::new();
    for key in state.files.keys() {
        let Some(rest) = key.strip_prefix(&prefix) else {
            continue;
        };
        match rest.split_once('/') {
            Some((folder, _)) => {
                entries.insert(folder.to_owned(), "dir");
            }
            None => {
                entries.insert(rest.to_owned(), "file");
            }
        }
    }
    if entries.is_empty() {
        return refusal("path/not-found");
    }
    let entries: Vec<Value> = entries
        .into_iter()
        .map(|(name, kind)| json!({ "name": name, "kind": kind }))
        .collect();
    CallToolResult::structured(json!({ "entries": entries }))
}

fn info(state: &State, relative: &str) -> CallToolResult {
    if let Some(text) = state.files.get(relative) {
        let size = u64::try_from(text.len()).unwrap_or(u64::MAX);
        return CallToolResult::structured(json!({ "kind": "file", "size": size }));
    }
    let prefix = format!("{relative}/");
    if state.files.keys().any(|key| key.starts_with(&prefix)) {
        return CallToolResult::structured(json!({ "kind": "dir", "size": 0 }));
    }
    refusal("path/not-found")
}

fn attach(state: &mut State, relative: &str) -> CallToolResult {
    if let Some(code) = state.refused.get(relative) {
        return refusal(code);
    }
    if state.unreadable.iter().any(|path| path == relative) {
        return refusal("io/unreadable");
    }
    let Some(text) = state.files.get(relative).cloned() else {
        return refusal("path/not-found");
    };
    CallToolResult::structured(json!({ "content": text, "content_hash": "0".repeat(64) }))
}

impl ServerHandler for FakeVault {
    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + Send + '_ {
        let path = request
            .arguments
            .as_ref()
            .and_then(|arguments| arguments.get("path"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        std::future::ready(self.answer(request.name.as_ref(), &path).map(CallToolResponse::from))
    }
}
