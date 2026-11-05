//! The workflow package manifest as the package store engine reads it: `contextos_packages::ReleaseManifest` for the
//! manifest model of `contextos_workflows::package`, so both stores read releases with the one model.

use contextos_packages::{
    InterfaceChange, InterfaceChanges, ListedFile, ManifestRejection, PackagesError, ReleaseDigest, ReleaseFilePath,
    ReleaseFiles, ReleaseId, ReleaseKey, ReleaseManifest, ReleaseName, ReleaseVersion, StoreHome, VersionStep, codes,
};
use contextos_workflows::call_rules::ToolRef;
use contextos_workflows::package::{FieldKind, PackageError, PackageManifest, TypedField, WorkflowDefinition};
use contextos_workflows::workflow_definition::WorkflowText;
use contextos_workflows::{PackageFileText, PackageKind};
use serde::{Deserialize, Serialize};

/// The code a vault store refuses a workflow that declares a `vault` input with: a vault's workflow runs only in that
/// vault, so it names no vault.
pub const VAULT_INPUT_IN_VAULT_STORE: &str = "packages/vault-input-in-vault-store";

/// A package manifest as the engine reads it: the parsed manifest, with the release it names and the files it lists
/// already read into the engine's validated types, so the engine never reads a release from text it has not checked.
#[derive(Clone, Debug)]
pub struct PackageRelease {
    manifest: PackageManifest,
    release: ReleaseId,
    files: Vec<ListedFile>,
}

impl PackageRelease {
    /// The manifest this release was read from, for the callers that need more than the engine does.
    #[must_use]
    pub fn manifest(&self) -> &PackageManifest {
        &self.manifest
    }
}

impl ReleaseManifest for PackageRelease {
    type Interface = PackageInterface;

    fn release(&self) -> &ReleaseId {
        &self.release
    }

    fn listed_files(&self) -> &[ListedFile] {
        &self.files
    }

    fn check_files(&self, files: &ReleaseFiles, home: StoreHome) -> Result<(), ManifestRejection> {
        let refuse = |code: &'static str, message: String| ManifestRejection { code, message };
        let held = |path: &str| ReleaseFilePath::try_from(path).is_ok_and(|path| files.0.contains_key(&path));
        let holds_beneath = |path: &str| {
            held(path)
                || files.0.keys().any(|key| {
                    key.to_string()
                        .strip_prefix(path)
                        .is_some_and(|rest| rest.starts_with('/'))
                })
        };
        let kind = PackageKind::try_from(PackageFilesOf {
            manifest: &self.manifest,
            files,
        })
        .map_err(|error| refuse(codes::INVALID_MANIFEST, error.to_string()))?;
        let package_file = self.manifest.package_file_name();
        for module in kind.modules().entries() {
            if !held(module.as_str()) {
                return Err(refuse(
                    codes::INVALID_MANIFEST,
                    format!("{package_file} declares the module {module}, which the release does not list"),
                ));
            }
        }
        if let Some(templates) = kind.templates() {
            for template in templates.entries() {
                if !holds_beneath(template.as_str()) {
                    return Err(refuse(
                        codes::INVALID_MANIFEST,
                        format!("{package_file} declares the template {template}, which the release does not list"),
                    ));
                }
            }
        }
        for folder in kind.workflows() {
            let file = format!("{folder}/workflow.toml");
            if !held(&file) {
                return Err(refuse(
                    codes::INVALID_MANIFEST,
                    format!("{package_file} lists the workflow {folder}, and the release does not list {file}"),
                ));
            }
        }
        for key in files.0.keys() {
            if let Some(folder) = workflow_folder_of(&key.to_string())
                && !kind.workflows().iter().any(|listed| listed == folder)
            {
                return Err(refuse(
                    codes::INVALID_MANIFEST,
                    format!(
                        "{key} is in a folder {package_file} does not list under `workflows`: list {folder} there or remove the file"
                    ),
                ));
            }
        }
        for folder in kind.workflows() {
            let file = format!("{folder}/workflow.toml");
            let definition = workflow_in(files, folder)
                .ok_or_else(|| refuse(codes::INVALID_MANIFEST, format!("{file} is not valid UTF-8 text")))?;
            let definition = definition.map_err(|error| refuse(codes::INVALID_MANIFEST, format!("{file}: {error}")))?;
            let entry = format!("{folder}/{}", definition.entry);
            if !held(&entry) {
                return Err(refuse(
                    codes::INVALID_MANIFEST,
                    format!("{file} names the entry {entry}, which the release does not list"),
                ));
            }
            if home == StoreHome::Vault
                && let Some(input) = definition.inputs.first_vault_input()
            {
                return Err(refuse(
                    VAULT_INPUT_IN_VAULT_STORE,
                    format!(
                        "{file} declares the vault input {input:?}; a workflow that names vaults is shared and \
                         installs into the host store, and a vault's store holds only workflows that run in that vault"
                    ),
                ));
            }
        }
        Ok(())
    }

    fn interface(&self, files: &ReleaseFiles) -> PackageInterface {
        let Ok(kind) = PackageKind::try_from(PackageFilesOf {
            manifest: &self.manifest,
            files,
        }) else {
            return PackageInterface::default();
        };
        PackageInterface {
            workflows: kind
                .workflows()
                .iter()
                .filter_map(|folder| workflow_in(files, folder)?.ok())
                .map(|definition| WorkflowInterface::from(&definition))
                .collect(),
            exports: kind
                .exports()
                .map(|exports| exports.entries().iter().map(|path| path.as_str().to_owned()).collect())
                .unwrap_or_default(),
        }
    }
}

/// The manifest of a release and the files it holds, from which its package file is read.
#[derive(Clone, Copy, Debug)]
pub struct PackageFilesOf<'a> {
    pub manifest: &'a PackageManifest,
    pub files: &'a ReleaseFiles,
}

impl TryFrom<PackageFilesOf<'_>> for PackageKind {
    type Error = PackageError;

    /// The package file the release lists, read from its files.
    fn try_from(source: PackageFilesOf<'_>) -> Result<Self, PackageError> {
        let file = source.manifest.package_file_name();
        let text = text_of(source.files, file).ok_or(PackageError::PackageFileUnreadable { file })?;
        Self::try_from(PackageFileText {
            manifest: source.manifest,
            text: &text,
        })
    }
}

/// The folder `path` is the `workflow.toml` of: exactly `{folder}/workflow.toml`.
fn workflow_folder_of(path: &str) -> Option<&str> {
    let (folder, file) = path.split_once('/')?;
    (file == "workflow.toml" && !folder.is_empty()).then_some(folder)
}

impl TryFrom<&str> for PackageRelease {
    type Error = ManifestRejection;

    fn try_from(text: &str) -> Result<Self, ManifestRejection> {
        let manifest = PackageManifest::parse(text).map_err(Refusal::from)?;
        let release = ReleaseId::try_from(Named(&manifest))?;
        let files = manifest
            .files
            .iter()
            .map(|file| {
                Ok(ListedFile {
                    path: ReleaseFilePath::try_from(file.path.as_str()).map_err(Refusal::from)?,
                    hash: file.hash.clone(),
                })
            })
            .collect::<Result<Vec<_>, Refusal>>()?;
        Ok(Self {
            manifest,
            release,
            files,
        })
    }
}

/// The text of the file at `path` in `files`, when the release holds it as UTF-8.
fn text_of(files: &ReleaseFiles, path: &str) -> Option<String> {
    let key = ReleaseFilePath::try_from(path).ok()?;
    String::from_utf8(files.0.get(&key)?.clone()).ok()
}

/// The workflow kept in `folder` as its `workflow.toml` declares it: none when the file is absent or not text.
fn workflow_in(files: &ReleaseFiles, folder: &str) -> Option<Result<WorkflowDefinition, PackageError>> {
    let text = text_of(files, &format!("{folder}/workflow.toml"))?;
    Some(WorkflowDefinition::try_from(WorkflowText { text: &text, folder }))
}

/// A refusal on its way to becoming the [`ManifestRejection`] the engine takes.
struct Refusal(ManifestRejection);

impl From<Refusal> for ManifestRejection {
    fn from(refusal: Refusal) -> Self {
        refusal.0
    }
}

impl From<PackageError> for Refusal {
    /// The model's refusal under the code the engine reports for it: a bad name, a bad version, or any other schema
    /// failure as an invalid manifest.
    fn from(error: PackageError) -> Self {
        let code = match error {
            PackageError::InvalidPackageName { .. } => codes::INVALID_NAME,
            PackageError::InvalidVersion { .. } => codes::INVALID_VERSION,
            _ => codes::INVALID_MANIFEST,
        };
        Self(ManifestRejection {
            code,
            message: error.to_string(),
        })
    }
}

impl From<PackagesError> for Refusal {
    /// An engine-side refusal of a part of the manifest, under its own code.
    fn from(error: PackagesError) -> Self {
        Self(ManifestRejection {
            code: error.code(),
            message: error.to_string(),
        })
    }
}

/// A manifest read for the release it names.
struct Named<'a>(&'a PackageManifest);

impl TryFrom<Named<'_>> for ReleaseId {
    type Error = Refusal;

    fn try_from(Named(manifest): Named<'_>) -> Result<Self, Refusal> {
        let identity = &manifest.identity;
        Ok(Self {
            key: ReleaseKey {
                name: ReleaseName::try_from(identity.name.as_str())?,
                version: ReleaseVersion::try_from(identity.version.to_string().as_str())?,
            },
            digest: ReleaseDigest::try_from(identity.digest.as_str())?,
        })
    }
}

/// What a release offers and needs, as the ledger keeps it to judge the next version against. It comes from each
/// workflow's `workflow.toml` and the manifest's exports: every workflow's inputs and outputs, the tools it requires, and
/// the four lists that say what a run of it may call, and the modules the package exports.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PackageInterface {
    /// The package's workflows, in the order the manifest lists their files.
    #[serde(default)]
    pub workflows: Vec<WorkflowInterface>,
    /// The modules other packages may import, as declared.
    #[serde(default)]
    pub exports: Vec<String>,
}

/// One workflow's part of a [`PackageInterface`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkflowInterface {
    /// The workflow's slug.
    pub slug: String,
    /// The inputs it declares.
    #[serde(default)]
    pub inputs: Vec<InterfaceField>,
    /// The outputs it declares.
    #[serde(default)]
    pub outputs: Vec<InterfaceField>,
    /// The tool contracts it requires.
    #[serde(default)]
    pub tools: Vec<InterfaceTool>,
    /// `[mcp_servers] allow`: absent when the workflow does not restrict the servers.
    #[serde(default)]
    pub server_allow: Option<Vec<String>>,
    /// `[mcp_servers] deny`.
    #[serde(default)]
    pub server_deny: Vec<String>,
    /// `[mcp_tools] allow`.
    #[serde(default)]
    pub tool_allow: Vec<InterfaceEffect>,
    /// `[mcp_tools] deny`.
    #[serde(default)]
    pub tool_deny: Vec<InterfaceEffect>,
}

/// A declared input or output.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InterfaceField {
    /// The field's name.
    pub name: String,
    /// The field's type.
    pub kind: InterfaceKind,
    /// Whether a run must supply it.
    pub required: bool,
}

/// A field's type, recursive for an array's elements and an object's properties.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceKind {
    /// Text.
    String,
    /// A number.
    Number,
    /// A boolean.
    Bool,
    /// Null.
    Null,
    /// A calendar date.
    Date,
    /// A time of day.
    Time,
    /// A date and time with an offset.
    DateTime,
    /// Whole seconds since the Unix epoch.
    Timestamp,
    /// A vault's name.
    Vault,
    /// A list of one element type.
    Array {
        /// The element type.
        items: Box<InterfaceKind>,
    },
    /// A structure with declared properties.
    Object {
        /// The declared properties.
        properties: Vec<InterfaceField>,
    },
}

/// A tool a workflow requires, at a contract version.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InterfaceTool {
    /// The MCP server's name.
    pub server: String,
    /// The tool's name.
    pub tool: String,
    /// The contract version the workflow was written against.
    pub contract_version: String,
}

/// A `(server, tool)` pair named in one of the tool lists.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct InterfaceEffect {
    /// The MCP server's name.
    pub server: String,
    /// The tool's name.
    pub tool: String,
}

impl PackageInterface {
    /// The step this interface requires from `previous`, the largest step any change requires. A workflow removed is
    /// major and a workflow added is minor. Within a workflow, an input removed, made required, or given another type is
    /// major, as is an output removed or changed, and a new required input; a new optional input, a new output, and an
    /// entry added to or removed from any of the four call lists, and a required tool added, removed, or given another contract version, are minor. An export removed is major and one added is
    /// minor.
    #[must_use]
    pub fn required_step(&self, previous: &Self) -> VersionStep {
        self.changes_from(previous)
            .iter()
            .map(|change| change.step)
            .max()
            .unwrap_or(VersionStep::Patch)
    }
}

impl InterfaceChanges for PackageInterface {
    fn changes_from(&self, previous: &Self) -> Vec<InterfaceChange> {
        let mut changes = Vec::new();
        for before in &previous.workflows {
            match self.workflows.iter().find(|now| now.slug == before.slug) {
                Some(now) => changes.extend(now.changes_from(before)),
                None => changes.push(InterfaceChange {
                    step: VersionStep::Major,
                    what: format!("workflow {} removed", before.slug),
                }),
            }
        }
        for now in &self.workflows {
            if !previous.workflows.iter().any(|before| before.slug == now.slug) {
                changes.push(InterfaceChange {
                    step: VersionStep::Minor,
                    what: format!("workflow {} added", now.slug),
                });
            }
        }
        for export in previous.exports.iter().filter(|export| !self.exports.contains(export)) {
            changes.push(InterfaceChange {
                step: VersionStep::Major,
                what: format!("export {export} removed"),
            });
        }
        for export in self.exports.iter().filter(|export| !previous.exports.contains(export)) {
            changes.push(InterfaceChange {
                step: VersionStep::Minor,
                what: format!("export {export} added"),
            });
        }
        changes
    }
}

impl WorkflowInterface {
    fn changes_from(&self, previous: &Self) -> Vec<InterfaceChange> {
        let slug = &self.slug;
        let change = |step: VersionStep, what: String| InterfaceChange {
            step,
            what: format!("{slug}: {what}"),
        };
        let mut changes = Vec::new();
        for before in &previous.inputs {
            let name = &before.name;
            match self.inputs.iter().find(|now| now.name == *name) {
                None => changes.push(change(VersionStep::Major, format!("input {name} removed"))),
                Some(now) if now.kind != before.kind => {
                    changes.push(change(VersionStep::Major, format!("input {name} retyped")));
                }
                Some(now) if now.required && !before.required => {
                    changes.push(change(VersionStep::Major, format!("input {name} made required")));
                }
                Some(now) if now.required != before.required => {
                    changes.push(change(VersionStep::Minor, format!("input {name} made optional")));
                }
                Some(_) => {}
            }
        }
        for now in &self.inputs {
            if !previous.inputs.iter().any(|before| before.name == now.name) {
                let name = &now.name;
                changes.push(if now.required {
                    change(VersionStep::Major, format!("required input {name} added"))
                } else {
                    change(VersionStep::Minor, format!("optional input {name} added"))
                });
            }
        }
        for before in &previous.outputs {
            let name = &before.name;
            match self.outputs.iter().find(|now| now.name == *name) {
                Some(now) if now == before => {}
                Some(_) => changes.push(change(VersionStep::Major, format!("output {name} changed"))),
                None => changes.push(change(VersionStep::Major, format!("output {name} removed"))),
            }
        }
        for now in &self.outputs {
            if !previous.outputs.iter().any(|before| before.name == now.name) {
                changes.push(change(VersionStep::Minor, format!("output {} added", now.name)));
            }
        }
        for before in &previous.tools {
            let (server, tool) = (&before.server, &before.tool);
            match self.tools.iter().find(|now| now.server == *server && now.tool == *tool) {
                Some(now) if now.contract_version != before.contract_version => changes.push(change(
                    VersionStep::Minor,
                    format!(
                        "tool {server}.{tool} contract version changed from {} to {}",
                        before.contract_version, now.contract_version
                    ),
                )),
                Some(_) => {}
                None => changes.push(change(
                    VersionStep::Minor,
                    format!("required tool {server}.{tool} removed"),
                )),
            }
        }
        for now in &self.tools {
            if !previous
                .tools
                .iter()
                .any(|before| before.server == now.server && before.tool == now.tool)
            {
                changes.push(change(
                    VersionStep::Minor,
                    format!("required tool {}.{} added", now.server, now.tool),
                ));
            }
        }
        if self.call_lists_differ(previous) {
            changes.push(change(
                VersionStep::Minor,
                "the allowed servers or tools changed".to_owned(),
            ));
        }
        changes
    }

    fn call_lists_differ(&self, previous: &Self) -> bool {
        fn as_set<T: Ord + Clone>(entries: &[T]) -> std::collections::BTreeSet<T> {
            entries.iter().cloned().collect()
        }
        self.server_allow.as_deref().map(as_set) != previous.server_allow.as_deref().map(as_set)
            || as_set(&self.server_deny) != as_set(&previous.server_deny)
            || as_set(&self.tool_allow) != as_set(&previous.tool_allow)
            || as_set(&self.tool_deny) != as_set(&previous.tool_deny)
    }
}

impl From<&WorkflowDefinition> for WorkflowInterface {
    fn from(workflow: &WorkflowDefinition) -> Self {
        let lists = workflow.calls.lists();
        let effect = |entry: &ToolRef| InterfaceEffect {
            server: entry.server.clone(),
            tool: entry.tool.clone(),
        };
        Self {
            slug: workflow.folder.clone(),
            inputs: workflow.inputs.fields().iter().map(InterfaceField::from).collect(),
            outputs: workflow.outputs.fields().iter().map(InterfaceField::from).collect(),
            tools: workflow
                .required_tool_contracts
                .iter()
                .map(|contract| InterfaceTool {
                    server: contract.mcp_server_name.clone(),
                    tool: contract.tool.clone(),
                    contract_version: contract.contract_version.clone(),
                })
                .collect(),
            server_allow: lists.server_allow.clone(),
            server_deny: lists.server_deny.clone(),
            tool_allow: lists.tool_allow.iter().map(effect).collect(),
            tool_deny: lists.tool_deny.iter().map(effect).collect(),
        }
    }
}

impl From<&TypedField> for InterfaceField {
    fn from(field: &TypedField) -> Self {
        Self {
            name: field.name.clone(),
            kind: InterfaceKind::from(&field.kind),
            required: field.required,
        }
    }
}

impl From<&FieldKind> for InterfaceKind {
    fn from(kind: &FieldKind) -> Self {
        match kind {
            FieldKind::String => Self::String,
            FieldKind::Number => Self::Number,
            FieldKind::Bool => Self::Bool,
            FieldKind::Null => Self::Null,
            FieldKind::Date => Self::Date,
            FieldKind::Time => Self::Time,
            FieldKind::DateTime => Self::DateTime,
            FieldKind::Timestamp => Self::Timestamp,
            FieldKind::Vault => Self::Vault,
            FieldKind::Array { items } => Self::Array {
                items: Box::new(Self::from(items.as_ref())),
            },
            FieldKind::Object { properties } => Self::Object {
                properties: properties.iter().map(InterfaceField::from).collect(),
            },
        }
    }
}

#[cfg(test)]
#[path = "package_release_manifest_test.rs"]
mod tests;
