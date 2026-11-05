//! One workflow's `workflow.toml`, read by one parser for every store.
//!
//! A workflow lives in a folder of its own, `{workflow}/`, in a package release and, as source, in a vault. Its
//! `workflow.toml` declares that one workflow: the entry script, the inputs and outputs, and what a run of it may call
//! ([`CallRules`]). The modules, templates, and libraries its scripts use belong to the package, declared in
//! `workflows.toml`. The vault's source and the files of a release in either store are read here and nowhere else, so
//! a workflow means the same wherever it is kept.

use serde::Deserialize;

use crate::call_rules::{CallLists, CallRules, ToolRef};
use crate::declared_inputs::DeclaredInputs;
use crate::logging::LogLevel;
use crate::outputs::DeclaredOutputs;
use crate::package::PackageError;
use crate::workflow_files::WorkflowFilePath;

/// A requirement that a specific, named MCP server expose a given tool at a given contract version, carried verbatim
/// for an operator: nothing compares the version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequiredToolContract {
    pub mcp_server_name: String,
    pub tool: String,
    pub contract_version: String,
}

/// What a `workflow.toml` declares.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowDefinition {
    /// The name of the folder the workflow is kept in, which is the workflow's name.
    pub folder: String,
    pub name: String,
    /// The entry script, relative to the workflow's folder.
    pub entry: String,
    /// What a one-off run is limited to, when the workflow sets it.
    pub timeout_secs: Option<u64>,
    /// The entity the workflow's jobs belong to unless they declare their own.
    pub entity: Option<String>,
    pub calls: CallRules,
    pub inputs: DeclaredInputs,
    pub outputs: DeclaredOutputs,
    pub required_tool_contracts: Vec<RequiredToolContract>,
    /// The minimum level this workflow's script messages are written at, overriding the host's.
    pub log_level: Option<LogLevel>,
}

/// The text of a `workflow.toml` and the folder it was found in.
#[derive(Clone, Copy, Debug)]
pub struct WorkflowText<'a> {
    pub text: &'a str,
    pub folder: &'a str,
}

/// Whether `slug` is lower-case letters, digits, and hyphens only, so it is safe in a URL.
#[must_use]
pub fn is_url_safe_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl TryFrom<WorkflowText<'_>> for WorkflowDefinition {
    type Error = PackageError;

    fn try_from(source: WorkflowText<'_>) -> Result<Self, PackageError> {
        let invalid = |reason: String| PackageError::InvalidWorkflowFile { reason };
        let raw: WorkflowFileRaw = toml::from_str(source.text)
            .map_err(|error| invalid(format!("workflow.toml does not match the required schema: {error}")))?;
        if raw.name.trim().is_empty() {
            return Err(invalid("name must not be empty".to_owned()));
        }
        if raw.entry.trim().is_empty() {
            return Err(invalid(
                "entry must not be empty: name the workflow's entry script".to_owned(),
            ));
        }
        WorkflowFilePath::try_from(raw.entry.as_str())
            .map_err(|error| PackageError::InvalidTemplatePath(error.path))?;
        let calls = CallRules::try_from(CallLists {
            server_allow: raw.mcp_servers.allow,
            server_deny: raw.mcp_servers.deny,
            tool_allow: raw.mcp_tools.allow.into_iter().map(ToolRef::from).collect(),
            tool_deny: raw.mcp_tools.deny.into_iter().map(ToolRef::from).collect(),
        })?;
        Ok(Self {
            folder: source.folder.to_owned(),
            name: raw.name,
            entry: raw.entry,
            timeout_secs: raw.timeout_secs,
            entity: raw.entity,
            calls,
            inputs: raw.input,
            outputs: raw.output,
            required_tool_contracts: raw
                .required_tool_contracts
                .into_iter()
                .map(RequiredToolContract::from)
                .collect(),
            log_level: raw.log_level,
        })
    }
}

/// `workflow.toml` as written. `deny_unknown_fields`: an unrecognised key is a schema violation, not a silently ignored
/// typo.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowFileRaw {
    name: String,
    entry: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    entity: Option<String>,
    #[serde(default)]
    mcp_servers: ServersRaw,
    #[serde(default)]
    mcp_tools: ToolsRaw,
    #[serde(default)]
    input: DeclaredInputs,
    #[serde(default)]
    output: DeclaredOutputs,
    #[serde(default)]
    required_tool_contracts: Vec<RequiredToolContractRaw>,
    #[serde(default)]
    log_level: Option<LogLevel>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServersRaw {
    #[serde(default)]
    allow: Option<Vec<String>>,
    #[serde(default)]
    deny: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolsRaw {
    #[serde(default)]
    allow: Vec<ToolRaw>,
    #[serde(default)]
    deny: Vec<ToolRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolRaw {
    server: String,
    tool: String,
}

impl From<ToolRaw> for ToolRef {
    fn from(raw: ToolRaw) -> Self {
        Self {
            server: raw.server,
            tool: raw.tool,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequiredToolContractRaw {
    mcp_server_name: String,
    tool: String,
    contract_version: String,
}

impl From<RequiredToolContractRaw> for RequiredToolContract {
    fn from(raw: RequiredToolContractRaw) -> Self {
        Self {
            mcp_server_name: raw.mcp_server_name,
            tool: raw.tool,
            contract_version: raw.contract_version,
        }
    }
}

#[cfg(test)]
#[path = "workflow_definition_test.rs"]
mod tests;

#[cfg(test)]
#[path = "workflow_fields_test.rs"]
mod fields_tests;
