//! What a workflow run may call: the four lists a `workflow.toml` declares, and the one rule that reads them.
//!
//! `[mcp_servers]` carries an `allow` and a `deny` list of server names, and `[mcp_tools]` an `allow` and a `deny` list
//! of `{ server, tool }` pairs. A call to server S, tool T is permitted when S is not on the server deny list, S is on
//! the server allow list or that list is absent, (S, T) is not on the tool deny list, and the tool allow list names no
//! tool on S or names (S, T). A workflow that declares nothing permits every call. A deny always wins, and no tool
//! allow lifts a server deny.

use std::collections::BTreeSet;

use crate::package_error::PackageError;

/// The tool name that stands for every tool of a server, which only a library's `mcp_tools` may use.
pub const ANY_TOOL: &str = "*";

/// One tool of one server, as the tool lists name it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ToolRef {
    /// The MCP server's name.
    pub server: String,
    /// The tool's name.
    pub tool: String,
}

/// The four lists as written, before they are checked against each other.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CallLists {
    /// `[mcp_servers] allow`: absent means every configured server.
    pub server_allow: Option<Vec<String>>,
    /// `[mcp_servers] deny`.
    pub server_deny: Vec<String>,
    /// `[mcp_tools] allow`: restricts only the servers it names.
    pub tool_allow: Vec<ToolRef>,
    /// `[mcp_tools] deny`.
    pub tool_deny: Vec<ToolRef>,
}

/// The four lists of a workflow, checked: no entry contradicts another.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CallRules {
    lists: CallLists,
}

impl CallRules {
    /// Whether a call to `tool` on `server` is permitted.
    #[must_use]
    pub fn permits(&self, server: &str, tool: &str) -> bool {
        let lists = &self.lists;
        let names_call = |entry: &ToolRef| entry.server == server && entry.tool == tool;
        self.permits_server(server)
            && !lists.tool_deny.iter().any(names_call)
            && (!lists.tool_allow.iter().any(|entry| entry.server == server) || lists.tool_allow.iter().any(names_call))
    }

    /// Whether the rules permit every tool on `server`, whatever its name: the server is reachable, no tool on it is
    /// denied, and no tool allow entry names it, since such an entry restricts the server to the tools it lists. A
    /// library that declares every tool on a server is accepted only by a workflow for which this holds.
    #[must_use]
    pub fn permits_every_tool(&self, server: &str) -> bool {
        let lists = &self.lists;
        self.permits_server(server)
            && !lists.tool_deny.iter().any(|entry| entry.server == server)
            && !lists.tool_allow.iter().any(|entry| entry.server == server)
    }

    /// Whether the rules let a run reach `server` at all: it is not denied, and it is allowed or no allow list is declared.
    #[must_use]
    pub fn permits_server(&self, server: &str) -> bool {
        let lists = &self.lists;
        !lists.server_deny.iter().any(|denied| denied == server)
            && lists
                .server_allow
                .as_ref()
                .is_none_or(|allowed| allowed.iter().any(|listed| listed == server))
    }

    /// Every server an allow list names, in name order, which validation requires to be configured.
    #[must_use]
    pub fn servers_named_by_allow_lists(&self) -> BTreeSet<&str> {
        self.lists
            .server_allow
            .iter()
            .flatten()
            .map(String::as_str)
            .chain(self.lists.tool_allow.iter().map(|entry| entry.server.as_str()))
            .collect()
    }

    /// Which declaration refused a call to `tool` on `server`, for the failure a script sees. Only meaningful for a call
    /// [`CallRules::permits`] refuses.
    #[must_use]
    pub fn refusal_for(&self, server: &str, tool: &str) -> String {
        let lists = &self.lists;
        if lists.server_deny.iter().any(|denied| denied == server) {
            format!("server {server:?} is on this workflow's `[mcp_servers] deny` list")
        } else if !self.permits_server(server) {
            format!("server {server:?} is not on this workflow's `[mcp_servers] allow` list")
        } else if lists
            .tool_deny
            .iter()
            .any(|entry| entry.server == server && entry.tool == tool)
        {
            format!("{tool} on server {server:?} is on this workflow's `[mcp_tools] deny` list")
        } else {
            format!("{tool} on server {server:?} is not on this workflow's `[mcp_tools] allow` list")
        }
    }

    /// The rules in a line an operator reads: what is allowed and denied, or that nothing is restricted.
    #[must_use]
    pub fn summary(&self) -> String {
        let lists = &self.lists;
        let pairs = |entries: &[ToolRef]| {
            entries
                .iter()
                .map(|entry| format!("{}/{}", entry.server, entry.tool))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut parts = Vec::new();
        if let Some(allowed) = &lists.server_allow {
            parts.push(if allowed.is_empty() {
                "Servers allowed: none".to_owned()
            } else {
                format!("Servers allowed: {}", allowed.join(", "))
            });
        }
        if !lists.server_deny.is_empty() {
            parts.push(format!("Servers denied: {}", lists.server_deny.join(", ")));
        }
        if !lists.tool_allow.is_empty() {
            parts.push(format!("Tools allowed: {}", pairs(&lists.tool_allow)));
        }
        if !lists.tool_deny.is_empty() {
            parts.push(format!("Tools denied: {}", pairs(&lists.tool_deny)));
        }
        if parts.is_empty() {
            "Every configured server and tool".to_owned()
        } else {
            parts.join(". ")
        }
    }

    /// The four lists as declared.
    #[must_use]
    pub fn lists(&self) -> &CallLists {
        &self.lists
    }
}

impl TryFrom<CallLists> for CallRules {
    type Error = PackageError;

    fn try_from(lists: CallLists) -> Result<Self, PackageError> {
        let contradiction = |reason: String| PackageError::CallRuleContradiction { reason };
        let blank = |name: &str| name.trim().is_empty();
        if lists
            .server_allow
            .iter()
            .flatten()
            .chain(&lists.server_deny)
            .any(|name| blank(name))
        {
            return Err(contradiction("a server list holds a blank server name".to_owned()));
        }
        if lists
            .tool_allow
            .iter()
            .chain(&lists.tool_deny)
            .any(|entry| blank(&entry.server) || blank(&entry.tool))
        {
            return Err(contradiction(
                "a tool list holds an entry with a blank server or tool name".to_owned(),
            ));
        }
        if let Some(entry) = lists
            .tool_allow
            .iter()
            .chain(&lists.tool_deny)
            .find(|entry| entry.tool == ANY_TOOL)
        {
            return Err(contradiction(format!(
                "a tool list names {ANY_TOOL:?} on server {:?}, and only a library's `mcp_tools` may use it: server \
                 lists cover every tool on a server, so allow or deny the server in `[mcp_servers]`",
                entry.server
            )));
        }
        if let Some(server) = lists
            .server_allow
            .iter()
            .flatten()
            .find(|server| lists.server_deny.contains(server))
        {
            return Err(contradiction(format!(
                "server {server:?} is on both `[mcp_servers] allow` and `[mcp_servers] deny`"
            )));
        }
        if let Some(entry) = lists.tool_allow.iter().find(|entry| lists.tool_deny.contains(entry)) {
            return Err(contradiction(format!(
                "{} on server {:?} is on both `[mcp_tools] allow` and `[mcp_tools] deny`",
                entry.tool, entry.server
            )));
        }
        if let Some(entry) = lists
            .tool_allow
            .iter()
            .find(|entry| lists.server_deny.contains(&entry.server))
        {
            return Err(contradiction(format!(
                "`[mcp_tools] allow` names {} on server {:?}, but that server is denied by `[mcp_servers] deny`",
                entry.tool, entry.server
            )));
        }
        Ok(Self { lists })
    }
}

#[cfg(test)]
#[path = "call_rules_test.rs"]
mod tests;
