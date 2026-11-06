//! Workflow and job registration and manifest parsing: a vault package is a folder
//! `registry/workflows/{package}/` with a `workflows.toml` that lists its workflows, and each workflow is a folder of its
//! own, `{package}/{workflow}/`, with a `workflow.toml`. Its saved jobs are `jobs/{job}.toml` files in that folder. A job
//! is found by listing the job files where they live, and its workflow is the folder it sits in. A job's slug is unique
//! only within its parent workflow, never vault-wide: two different workflows may each have a job named `nightly-sync`.
//!
//! A workflow owns its own `entry` script and the rules for what its runs may call; a
//! job is purely a saved configuration of one (optional `schedule`,
//! `timeout_secs` override, `owner`) and has no files of its own beyond
//! its single manifest. This module also carries this crate's
//! `contextos_workflows::McpCaller` implementation, dispatching a job
//! script's `call_mcp` through the identical [`McpClientSet`] sessions
//! every other feature already uses. `contextos-mcp`
//! gains no workflow-awareness, no new tool, no new client kind: every
//! effect here is an ordinary MCP tool call this crate was already making
//! for other features.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use contextos_workflows::call_rules::CallRules;
pub(crate) use contextos_workflows::workflow_definition::is_url_safe_slug;
use contextos_workflows::workflow_definition::{WorkflowDefinition, WorkflowText};
use contextos_workflows::{
    DeclaredFiles, DeclaredInputs, DeclaredModules, DeclaredOutputs, JobFileRelease, JobFileText, LibraryName,
    LogLevel, McpCallFailure, McpCaller, SuppliedInputs, ValidatedInputs, ValidationCode, ValidationFailure,
    WorkflowFailureKind, WorkflowsFile,
};
use serde::Deserialize;
use serde_json::{Map, Value};

pub use crate::job_discovery::{WorkflowFolder, discover_jobs, discover_jobs_for_workflow, discover_workflows};
pub(crate) use crate::job_discovery::{WorkflowLoadOutcome, list_job_files, load_workflow};
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::source_read::{SourceRead, read_source};
use crate::tool_result::ToolResultJson;

/// The default applied to a one-off run of a workflow that sets no `timeout_secs` of its own. A saved
/// job never takes it: every job file states its own timeout.
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// One successfully validated workflow: the reusable
/// executable definition, its entry script, and the rules for what its runs may call
/// allow-list every one of its jobs inherits.
#[derive(Clone, Debug)]
pub struct RegisteredWorkflow {
    /// The vault package the workflow belongs to: its folder under `registry/workflows/`, which is the package's name.
    pub package: String,
    /// The workflow's slug, which is the name of its folder within its package.
    pub slug: String,
    pub name: String,
    pub entry: String,
    /// What a one-off run of the workflow is limited to. A saved job states its own and never takes this.
    pub timeout_secs: u64,
    /// The entity the workflow declares, which its jobs belong to unless they declare their own.
    pub entity: Option<String>,
    /// What a run of the workflow may call: its `[mcp_servers]` and `[mcp_tools]` lists.
    pub calls: CallRules,
    /// The typed inputs the entry script accepts; never `vault`-kinded, because a vault-registered workflow's scope
    /// is its own vault.
    pub inputs: DeclaredInputs,
    /// The typed outputs the entry script sets with `set_output`; never `vault`-kinded, as for inputs.
    pub outputs: DeclaredOutputs,
    /// The minimum log level this workflow sets for its own runs.
    pub log_level: Option<LogLevel>,
    /// The files and folders, relative to the package, its scripts may read with `workflow_file` and `workflow_files`,
    /// as the package's `workflows.toml` declares them.
    pub templates: DeclaredFiles,
    /// The `.rhai` files, relative to the package, its scripts may import, as the package's `workflows.toml` declares them.
    pub modules: DeclaredModules,
    /// The vault libraries (`registry/libraries/`) whose exports the package's files may import, in name order.
    pub libraries: Vec<LibraryName>,
}

impl RegisteredWorkflow {
    /// Where the workflow is kept under `registry/workflows/`.
    pub(crate) fn folder(&self) -> WorkflowFolder<'_> {
        WorkflowFolder {
            package: &self.package,
            directory: &self.slug,
        }
    }

    /// A workflow that declares nothing, which stands in for one whose declarations cannot be read when a job file is
    /// checked for what does not depend on them.
    pub(crate) fn without_declarations(package: &str, directory: &str) -> Self {
        Self {
            package: package.to_owned(),
            slug: directory.to_owned(),
            name: directory.to_owned(),
            entry: String::new(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            entity: None,
            calls: CallRules::default(),
            inputs: DeclaredInputs::default(),
            outputs: DeclaredOutputs::default(),
            log_level: None,
            templates: DeclaredFiles::default(),
            modules: DeclaredModules::default(),
            libraries: Vec::new(),
        }
    }
}

/// The entity a job belongs to: the job's own `entity`, else its workflow's, else `vault`. The one place it is worked
/// out, so a run record and a raised task always agree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedEntity(String);

impl ResolvedEntity {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<(Option<&str>, Option<&str>)> for ResolvedEntity {
    /// From the job's declared entity and the workflow's declared entity.
    fn from((job, workflow): (Option<&str>, Option<&str>)) -> Self {
        Self(
            job.or(workflow)
                .map_or_else(|| contextos_core::VAULT_SCOPE.to_owned(), str::to_owned),
        )
    }
}

/// One successfully validated job: a saved configuration of its parent
/// workflow. `entry` and `calls` are copied from that workflow
/// (`RegisteredWorkflow`) at discovery time so a caller driving a run
/// (the scheduler) or rendering a job's own detail page needs no second
/// lookup back to it.
#[derive(Clone, Debug)]
pub struct RegisteredJob {
    /// The vault package of the job's workflow: its folder under `registry/workflows/`.
    pub package: String,
    pub workflow_slug: String,
    pub slug: String,
    pub name: String,
    /// Resolved: this job's own `timeout_secs` when set, otherwise its
    /// parent workflow's own resolved default.
    pub timeout_secs: u64,
    /// Validated team member owning this saved job, when configured.
    pub owner: Option<crate::team::TeamMember>,
    /// The owner the job file declares as written, whether or not it names a member or is even slug-shaped.
    pub declared_owner: Option<contextos_core::DeclaredOwner>,
    /// The entity the job belongs to.
    pub entity: ResolvedEntity,
    /// The release of its workflow the job runs and, for a successor, the job it succeeds.
    pub release: JobFileRelease,
    pub entry: String,
    /// What a run of the workflow may call: its `[mcp_servers]` and `[mcp_tools]` lists.
    pub calls: CallRules,
    /// This job's input values, checked against the parent workflow's declaration at registration.
    pub inputs: ValidatedInputs,
    /// The inputs the parent workflow declares, copied at discovery, so a run's overrides can be checked against them.
    pub declared_inputs: DeclaredInputs,
    /// The outputs the parent workflow declares, copied at discovery like `entry`.
    pub outputs: DeclaredOutputs,
    /// The minimum log level the parent workflow sets, copied at discovery like `entry`.
    pub workflow_log_level: Option<LogLevel>,
    /// The minimum log level this job sets, overriding its workflow's and the host's.
    pub log_level: Option<LogLevel>,
    /// Whether the job file says authoring is finished.
    pub ready: bool,
    /// The job file's name within its workflow's `jobs/` folder, which the slug need not match.
    pub file_name: String,
    /// The files the parent workflow declares, copied at discovery like `entry`.
    pub templates: DeclaredFiles,
    /// The modules the parent workflow declares, copied at discovery like `entry`.
    pub modules: DeclaredModules,
    /// The vault libraries the parent workflow names, copied at discovery like `entry`.
    pub libraries: Vec<LibraryName>,
}

impl RegisteredJob {
    /// How this job's declared owner stands against `roster`.
    #[must_use]
    pub fn owner_standing<'a>(&self, roster: &'a contextos_core::TeamRoster) -> contextos_core::OwnerStanding<'a> {
        contextos_core::OwnerStanding::from((self.declared_owner.as_ref(), roster))
    }
}

/// `jobs/{{job-slug}}.toml`'s schema: a saved configuration of its parent
/// workflow. No `entry` and no call rules of its own: both are the
/// parent workflow's, never re-declared or overridden per job.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobManifestRaw {
    name: String,
    #[serde(default)]
    slug: Option<String>,
    /// Required: a job states how long it may run, and never takes its workflow's value.
    timeout_secs: std::num::NonZeroU64,
    /// Who the job's failures are for. Optional at this layer so a job without one still loads, and validation, which
    /// fails it with `validation/owner-unresolved`, can say so.
    #[serde(default)]
    owner: Option<String>,
    /// The entity the job belongs to, when it differs from its workflow's.
    #[serde(default)]
    entity: Option<String>,
    /// Required: the exact release of the job's workflow the job runs.
    version: String,
    /// The key of the job of the same workflow this one succeeds, written by `jobs successor`.
    #[serde(default)]
    predecessor: Option<String>,
    #[serde(default)]
    inputs: Map<String, Value>,
    #[serde(default)]
    log_level: Option<LogLevel>,
    /// Says authoring is finished, so the job is validated automatically. It sets no state and is not part of the version.
    #[serde(default)]
    ready: bool,
}

/// One thing wrong with an authored file, with the stable code for its kind and the file it concerns.
pub(crate) fn failure(code: ValidationCode, artefact: &str, message: impl Into<String>) -> ValidationFailure {
    ValidationFailure {
        code,
        artefact: artefact.to_owned(),
        message: message.into(),
    }
}

/// What a `workflow.toml` declares, read from its text alone: the schema, the name, the inputs and outputs, and what a
/// run may call. Nothing here looks at a file or at the configured servers, so a workflow read from source and one read
/// from a release's listed `workflow.toml` are declared by the same rules. `folder` is where the workflow is kept, which
/// names it. The package-level modules, templates, and libraries are not in the file; [`RegisteredWorkflow::declaring`]
/// adds those.
///
/// # Errors
///
/// The first failure found, against `workflow.toml`.
pub(crate) fn declarations_from_text(
    text: &str,
    folder: WorkflowFolder<'_>,
) -> Result<RegisteredWorkflow, ValidationFailure> {
    let invalid = |message: String| failure(ValidationCode::ArtefactInvalid, "workflow.toml", message);
    let declared = WorkflowDefinition::try_from(WorkflowText {
        text,
        folder: folder.directory,
    })
    .map_err(|source| invalid(source.to_string()))?;
    let inputs = declared
        .inputs
        .without_vault_kind()
        .map_err(|source| invalid(source.to_string()))?;
    let outputs = declared
        .outputs
        .without_vault_kind()
        .map_err(|source| invalid(source.to_string()))?;
    Ok(RegisteredWorkflow {
        package: folder.package.to_owned(),
        slug: declared.folder,
        name: declared.name,
        entry: declared.entry,
        timeout_secs: declared.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
        entity: declared.entity,
        calls: declared.calls,
        inputs,
        outputs,
        log_level: declared.log_level,
        templates: DeclaredFiles::default(),
        modules: DeclaredModules::default(),
        libraries: Vec::new(),
    })
}

impl RegisteredWorkflow {
    /// The workflow with the modules, templates, and libraries its package's `workflows.toml` declares for every one of
    /// its workflows.
    #[must_use]
    pub(crate) fn declaring(self, file: &WorkflowsFile) -> Self {
        Self {
            templates: file.templates().clone(),
            modules: file.modules().clone(),
            libraries: file.libraries().names().to_vec(),
            ..self
        }
    }
}

/// The failure for the first server an allow list of `calls` names that is not among the servers `web.toml` configures.
pub(crate) fn unconfigured_server(calls: &CallRules, known_mcp_servers: &[String]) -> Option<ValidationFailure> {
    calls
        .servers_named_by_allow_lists()
        .into_iter()
        .find(|server| !known_mcp_servers.iter().any(|name| name == server))
        .map(|server| {
            failure(
                ValidationCode::ServerNotConfigured,
                "workflow.toml",
                format!("server {server:?}, named in an allow list, is not configured in web.toml"),
            )
        })
}

pub(crate) enum JobLoadOutcome {
    Invalid(ValidationFailure),
    Valid(Box<RegisteredJob>),
}

pub(crate) async fn load_job(
    client: &McpClient,
    vault_name: &str,
    workflow: &RegisteredWorkflow,
    file_name: &str,
    team: &[crate::team::TeamMember],
) -> Result<JobLoadOutcome, McpCallError> {
    let manifest = format!(
        "{vault_name}://registry/workflows/{}/jobs/{file_name}",
        workflow.folder()
    );
    let read = match read_source(client, &manifest).await? {
        SourceRead::Text(text) => text,
        SourceRead::Missing => {
            return Ok(JobLoadOutcome::Invalid(failure(
                ValidationCode::ArtefactMissing,
                file_name,
                "job manifest could not be read",
            )));
        }
        SourceRead::Unusable(reason) => {
            return Ok(JobLoadOutcome::Invalid(failure(
                ValidationCode::ArtefactInvalid,
                file_name,
                format!("job manifest {reason}"),
            )));
        }
    };
    Ok(job_from_text(workflow, file_name, &read, team))
}

/// Loads a job from the text of its file, which is everything loading a job does apart from reading the file: the same
/// checks a placed file meets, whether the file was read from the vault or is about to be written to it.
pub(crate) fn job_from_text(
    workflow: &RegisteredWorkflow,
    file_name: &str,
    read: &str,
    team: &[crate::team::TeamMember],
) -> JobLoadOutcome {
    let raw: JobManifestRaw = match toml::from_str(read) {
        Ok(raw) => raw,
        Err(source) => {
            return JobLoadOutcome::Invalid(failure(
                ValidationCode::ArtefactInvalid,
                file_name,
                format!(
                    "job manifest does not match the required schema: {}",
                    crate::shared_jobs::schema_message(&source, read)
                ),
            ));
        }
    };
    if raw.name.trim().is_empty() {
        return JobLoadOutcome::Invalid(failure(
            ValidationCode::ArtefactInvalid,
            file_name,
            "name must not be empty",
        ));
    }
    let file_stem = file_name.strip_suffix(".toml").unwrap_or(file_name);
    let slug = match raw.slug {
        Some(explicit) if is_url_safe_slug(&explicit) && !contextos_workflows::is_reserved_job_slug(&explicit) => {
            explicit
        }
        Some(explicit) => {
            return JobLoadOutcome::Invalid(failure(
                ValidationCode::ArtefactInvalid,
                file_name,
                format!(
                    "slug {explicit:?} is not a job slug (expected [a-z0-9-]+, and not current, previous, or future)"
                ),
            ));
        }
        None if contextos_workflows::is_reserved_job_slug(file_stem) => {
            return JobLoadOutcome::Invalid(failure(
                ValidationCode::ArtefactInvalid,
                file_name,
                format!("the file name {file_stem:?} is not a job slug (not current, previous, or future)"),
            ));
        }
        None => file_stem.to_owned(),
    };
    let release = match JobFileRelease::try_from(JobFileText {
        version: &raw.version,
        predecessor: raw.predecessor.as_deref(),
    }) {
        Ok(release) => release,
        Err(source) => {
            return JobLoadOutcome::Invalid(failure(ValidationCode::ArtefactInvalid, file_name, source.to_string()));
        }
    };
    let declared_owner = raw.owner.as_deref().map(contextos_core::DeclaredOwner::from);
    let owner = resolve_owner(raw.owner.as_deref(), team);
    let inputs = match validate_job_inputs(workflow, raw.inputs) {
        Ok(inputs) => inputs,
        Err(reason) => {
            return JobLoadOutcome::Invalid(failure(ValidationCode::InputInvalid, file_name, reason));
        }
    };
    JobLoadOutcome::Valid(Box::new(RegisteredJob {
        package: workflow.package.clone(),
        workflow_slug: workflow.slug.clone(),
        slug,
        name: raw.name,
        timeout_secs: raw.timeout_secs.get(),
        owner,
        declared_owner,
        entity: ResolvedEntity::from((raw.entity.as_deref(), workflow.entity.as_deref())),
        release,
        entry: workflow.entry.clone(),
        calls: workflow.calls.clone(),
        inputs,
        declared_inputs: workflow.inputs.clone(),
        outputs: workflow.outputs.clone(),
        workflow_log_level: workflow.log_level,
        log_level: raw.log_level,
        ready: raw.ready,
        file_name: file_name.to_owned(),
        templates: workflow.templates.clone(),
        modules: workflow.modules.clone(),
        libraries: workflow.libraries.clone(),
    }))
}

/// Checks a job's `[inputs]` values against its parent workflow's declaration, so an undeclared key, a missing
/// required key, or a value of the wrong kind fails that job's registration rather than a later run.
fn validate_job_inputs(workflow: &RegisteredWorkflow, values: Map<String, Value>) -> Result<ValidatedInputs, String> {
    ValidatedInputs::try_from(SuppliedInputs {
        declared: workflow.inputs.fields(),
        values,
    })
    .map_err(|source| format!("inputs are invalid: {source}"))
}

/// The member `raw_owner` names, for a screen to show, from `team` (`.contextos/team.toml`'s members as the screens read
/// them). Only an exact slug matches. A job whose owner is missing or is no member is not refused here: that is a
/// validation failure and a refusal to run, decided against the strictly read roster, and the job still loads so its
/// page and a dry run can see it.
fn resolve_owner(raw_owner: Option<&str>, team: &[crate::team::TeamMember]) -> Option<crate::team::TeamMember> {
    let slug = raw_owner?;
    team.iter().find(|member| member.slug == slug).cloned()
}

/// The owner a job file declares, read without checking the rest of it, so a job that fails to load still has an owner
/// to hand its failures to.
pub(crate) fn declared_owner_of(job_toml: &str) -> Option<contextos_core::DeclaredOwner> {
    toml::from_str::<toml::Table>(job_toml)
        .ok()?
        .get("owner")?
        .as_str()
        .map(contextos_core::DeclaredOwner::from)
}

/// The `entity` a manifest declares, read without checking the rest of it.
pub(crate) fn declared_entity_of(manifest_toml: &str) -> Option<String> {
    toml::from_str::<toml::Table>(manifest_toml)
        .ok()?
        .get("entity")?
        .as_str()
        .map(str::to_owned)
}

/// Dispatches a job script's `call_mcp(server, tool, args)` through
/// whichever configured `[[mcp_server]]` session `server` names, exactly
/// as `POST /mcp/{{server_name}}/{{tool_name}}` ([`crate::proxy::handle`])
/// already does for every other caller, and returns the result in its
/// clean form ([`ToolResultJson`]), never the protocol envelope.
///
/// This type only issues calls. What a run may call is decided before a
/// call reaches it, by the wrapper each run is given
/// ([`crate::run_scope::RunScope`] for a vault-registered workflow,
/// [`crate::shared_runner::execute_shared`] for a shared one), so
/// every call is checked against the workflow's call rules and the vaults the run is scoped to.
pub struct WebMcpCaller {
    clients: Arc<McpClientSet>,
}

impl WebMcpCaller {
    #[must_use]
    pub fn new(clients: Arc<McpClientSet>) -> Self {
        Self { clients }
    }
}

impl McpCaller for WebMcpCaller {
    fn call<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        arguments: Map<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpCallFailure>> + Send + 'a>> {
        Box::pin(async move {
            let unavailable = |message: String| McpCallFailure {
                kind: WorkflowFailureKind::ServerUnavailable,
                server: server.to_owned(),
                tool: tool.to_owned(),
                message,
            };
            let Some(client) = self.clients.get(server) else {
                let message = if self.clients.is_configured(server) {
                    "MCP server is not connected; /server/settings/ shows why and connects it again"
                } else {
                    "MCP server is not configured"
                };
                return Err(unavailable(message.to_owned()));
            };
            // The same refresh gate a proxied call passes, so a run never calls with a token known to be stale.
            let refreshed = client.ensure_oauth_token_fresh().await;
            self.clients.note_refresh_outcome(server, &client, refreshed.is_ok());
            if let Err(error) = refreshed {
                return Err(unavailable(error.to_string()));
            }
            let result = client
                .call_tool(tool.to_owned(), arguments)
                .await
                .map_err(|source| unavailable(source.to_string()))?;
            if result.is_error == Some(true) {
                let tool_error = ToolErrorResult::from(&result);
                return Err(McpCallFailure {
                    kind: tool_error.failure_kind(),
                    server: server.to_owned(),
                    tool: tool.to_owned(),
                    message: tool_error.message,
                });
            }
            ToolResultJson::try_from(result)
                .map(|decoded| decoded.0)
                .map_err(|source| McpCallFailure {
                    kind: WorkflowFailureKind::ResultUnreadable,
                    server: server.to_owned(),
                    tool: tool.to_owned(),
                    message: format!("tool result could not be read as JSON: {source}"),
                })
        })
    }
}

/// The MCP-level error a tool returned (`is_error: true`): the server's stable code when it sent structured content
/// with one, and a human-readable message, the first text content block or a generic fallback when the server sent
/// none (a job run's `error` field benefits from the server's own text where available; `routes/vault.rs` and
/// `rendering/shell.rs` both discard it and substitute a fixed message of their own).
pub(crate) struct ToolErrorResult {
    pub(crate) code: Option<String>,
    pub(crate) message: String,
}

impl From<&rmcp::model::CallToolResult> for ToolErrorResult {
    fn from(result: &rmcp::model::CallToolResult) -> Self {
        let code = result
            .structured_content
            .as_ref()
            .and_then(|content| content.get("code"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let message = result
            .content
            .iter()
            .find_map(|block| block.as_text().map(|text| text.text.clone()))
            .unwrap_or_else(|| "the tool call failed".to_owned());
        Self { code, message }
    }
}

impl ToolErrorResult {
    /// Which kind of workflow failure this tool error is. The server reports a vault name it does not know with
    /// `path/unknown-vault-name`, which for a run means a vault removed or renamed since it began.
    fn failure_kind(&self) -> WorkflowFailureKind {
        match self.code.as_deref() {
            Some("path/unknown-vault-name") => WorkflowFailureKind::VaultUnavailable,
            _ => WorkflowFailureKind::ToolError,
        }
    }
}

#[cfg(test)]
#[path = "jobs_test.rs"]
mod tests;

#[cfg(test)]
#[path = "jobs_caller_test.rs"]
mod caller_tests;
