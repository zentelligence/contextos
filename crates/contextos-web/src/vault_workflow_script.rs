//! A vault workflow's code read from source, for a run of source: its entry script and every module its `workflows.toml`
//! declares, the entry script beneath the workflow's own folder and each module beneath its package's folder, all read
//! through the primary server, together with the library releases the run resolved.

use std::collections::BTreeMap;

use contextos_workflows::{
    PackageCode, PackageKind, PackageName, PackageToRead, RunFailure, ScriptParts, WorkflowFailureKind,
    WorkflowFileFailure, WorkflowFilePath, WorkflowScript, WorkflowsFile,
};

use crate::jobs::WorkflowFolder;
use crate::mcp_client::{McpCallError, McpClient};
use crate::vault_content::{AttachedRead, read_attached};
use crate::vault_workflow_files::read_file_or_outage;

/// Why a workflow's code could not be read: the file that could not be, relative to the workflow's folder, and why.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScriptUnreadable {
    pub artefact: String,
    pub message: String,
}

/// Why a workflow's code was not read.
#[derive(Debug)]
pub(crate) enum ScriptReadError {
    /// The server answered and a file cannot be read: it is missing, refused, not text, or too large. The workflow has
    /// no version until it can be.
    Unreadable(ScriptUnreadable),
    /// The server could not be asked, so nothing is known about the files: an outage, never a change to the job.
    Unreachable(McpCallError),
}

impl From<ScriptReadError> for RunFailure {
    /// A run whose code cannot be read ends before its script starts: `workflow/entry-unreadable` for a file that cannot
    /// be read, and `workflow/server-unavailable` when the server could not be asked.
    fn from(error: ScriptReadError) -> Self {
        let (kind, message) = match error {
            ScriptReadError::Unreadable(unreadable) => (WorkflowFailureKind::EntryUnreadable, unreadable.message),
            ScriptReadError::Unreachable(outage) => (WorkflowFailureKind::ServerUnavailable, outage.to_string()),
        };
        Self {
            kind: Some(kind),
            message: format!("{}: {message}", kind.code()),
        }
    }
}

/// A workflow kept in a vault's source, and the library releases its run resolved.
pub(crate) struct SourceCode<'a> {
    pub folder: WorkflowFolder<'a>,
    pub entry: &'a str,
    /// The package's `workflows.toml`, which declares the modules to read.
    pub file: &'a WorkflowsFile,
    /// The code of each library release the run resolved, keyed by name.
    pub libraries: BTreeMap<PackageName, PackageCode>,
}

/// Reads the entry script from the workflow's folder, `{vault}://registry/workflows/{package}/{directory}/`, and every
/// module the package declares from the package's folder, `{vault}://registry/workflows/{package}/`, and joins them to
/// the library releases in `source`. The package is the root of the script, as an installed release's is, so what it may
/// import is the libraries its `workflows.toml` names.
///
/// # Errors
///
/// Returns [`ScriptReadError::Unreachable`] when the server could not be asked about any file, and otherwise
/// [`ScriptReadError::Unreadable`] for the first file that cannot be read: the entry script first, then each module in
/// the order declared.
pub(crate) async fn read_source_script(
    client: &McpClient,
    vault: &str,
    source: SourceCode<'_>,
) -> Result<WorkflowScript, ScriptReadError> {
    let SourceCode {
        folder,
        entry,
        file,
        mut libraries,
    } = source;
    let root = format!("{vault}://registry/workflows/{}", folder.package);
    let unreadable = |message: String| {
        ScriptReadError::Unreadable(ScriptUnreadable {
            artefact: entry.to_owned(),
            message,
        })
    };
    let text = match read_attached(client, &format!("{root}/{}/{entry}", folder.directory))
        .await
        .map_err(ScriptReadError::Unreachable)?
    {
        AttachedRead::Text(read) => read.text,
        AttachedRead::Refused { .. } => return Err(unreadable(format!("entry script {entry:?} could not be read"))),
        AttachedRead::NotText => return Err(unreadable(format!("entry script {entry:?} is not a text file"))),
    };
    let mut read: BTreeMap<WorkflowFilePath, _> = BTreeMap::new();
    for path in file.modules().entries() {
        let found = read_file_or_outage(client, &root, path)
            .await
            .map_err(ScriptReadError::Unreachable)?;
        read.insert(path.clone(), found);
    }
    let name = PackageName::try_from(folder.package).map_err(|error| unreadable(error.to_string()))?;
    let own = PackageCode::read(
        PackageToRead {
            name: name.clone(),
            kind: PackageKind::Workflows(file.clone()),
        },
        |path| {
            read.remove(path).unwrap_or_else(|| {
                Err(WorkflowFileFailure::new(
                    WorkflowFailureKind::FileUnreadable,
                    format!("{path} was not read"),
                ))
            })
        },
    )
    .map_err(|error| {
        ScriptReadError::Unreadable(ScriptUnreadable {
            artefact: error.path.to_string(),
            message: error.to_string(),
        })
    })?;
    libraries.insert(name.clone(), own);
    Ok(WorkflowScript::from(ScriptParts {
        entry: text,
        package: Some(name),
        packages: libraries,
    }))
}

#[cfg(test)]
#[path = "vault_workflow_script_test.rs"]
mod tests;
