//! The "where does this live" strip shared by the Decisions register and the registry types: the configured settings
//! shown as text, an Edit action that turns them into text boxes with an Update button, and the save that writes them
//! to `.contextos/vault-management.toml`.
//!
//! An update is validated before anything is written: every path must be a plain vault-relative path through no
//! hidden folder, a managed folder may not be one of the append-only activity records, the folder and (for the
//! register) the base must already exist in the vault, and a template, when one is named, must exist too. A location
//! that names nothing would otherwise leave the category silently empty, which reads as "no records" rather than
//! "wrong place". The file is rewritten with only the edited table changed, conflict-checked against the version the
//! page was rendered from, which the form carries.

use serde::Deserialize;

use crate::mcp_client::McpCallError;
use crate::vault_content::{AttachedRead, read_attached};
use crate::vault_management_config::{
    ConfigFileState, LoadedVaultManagementConfig, RecordTypeConfig, RegisterPaths, RegisterSettings, RegistryKind,
    RegistryTypePaths, VAULT_MANAGEMENT_TOML_PATH, configured_location, with_register_settings,
    with_registry_type_paths,
};
use crate::vault_relative_path::VaultRelativePath;
use crate::vault_tree::{FolderListing, list_folder};
use crate::vault_write::{WriteOutcome, write_text};

use super::vault_management::ManagementContext;

/// One setting in the strip.
pub(crate) struct PathsField {
    pub(crate) name: &'static str,
    pub(crate) label: &'static str,
    pub(crate) value: String,
    /// Shown when the value is empty, in view and edit alike.
    pub(crate) empty_label: &'static str,
    pub(crate) required: bool,
}

/// The strip as rendered.
pub(crate) struct PathsView {
    pub(crate) editing: bool,
    pub(crate) fields: Vec<PathsField>,
    pub(crate) error: Option<String>,
    /// The resource the strip belongs to: `GET` shows it, `PATCH` saves the strip.
    pub(crate) resource_href: String,
    pub(crate) edit_href: String,
    /// The version of `.contextos/vault-management.toml` the strip was rendered from; empty when it does not exist.
    pub(crate) expected_hash: String,
    /// A sentence on what the templates are for.
    pub(crate) hint: &'static str,
}

fn path_field(name: &'static str, label: &'static str, value: &VaultRelativePath) -> PathsField {
    PathsField {
        name,
        label,
        value: value.to_string(),
        empty_label: "",
        required: true,
    }
}

fn template_field(name: &'static str, label: &'static str, value: Option<&VaultRelativePath>) -> PathsField {
    PathsField {
        name,
        label,
        value: value.map(ToString::to_string).unwrap_or_default(),
        empty_label: "None",
        required: false,
    }
}

fn prefix_field(name: &'static str, label: &'static str, value: &str) -> PathsField {
    PathsField {
        name,
        label,
        value: value.to_owned(),
        empty_label: "",
        required: true,
    }
}

impl PathsView {
    fn new(
        fields: Vec<PathsField>,
        resource_href: String,
        loaded: &LoadedVaultManagementConfig,
        hint: &'static str,
    ) -> Self {
        Self {
            editing: false,
            fields,
            error: None,
            edit_href: format!("{}/edit", resource_href.trim_end_matches('/')),
            resource_href,
            expected_hash: loaded.expected_hash.clone().unwrap_or_default(),
            hint,
        }
    }

    pub(crate) fn register(loaded: &LoadedVaultManagementConfig, resource_href: String) -> Self {
        let settings = loaded.config.register();
        let fields = vec![
            path_field("base", "Base", &settings.paths.base),
            path_field("dir", "Folder", &settings.paths.dir),
            prefix_field("decision_prefix", "Decision id prefix", &settings.decision.prefix),
            template_field(
                "decision_template",
                "Decision template",
                settings.decision.template.as_ref(),
            ),
            prefix_field("question_prefix", "Question id prefix", &settings.question.prefix),
            template_field(
                "question_template",
                "Question template",
                settings.question.template.as_ref(),
            ),
        ];
        Self::new(
            fields,
            resource_href,
            loaded,
            "A new record's body starts from its type's template; leave a template empty to start blank. A record \
             with no `type` is told apart by its id prefix.",
        )
    }

    pub(crate) fn registry(loaded: &LoadedVaultManagementConfig, kind: RegistryKind, resource_href: String) -> Self {
        let paths = loaded.config.registry(kind);
        let fields = vec![
            path_field("dir", "Folder", &paths.dir),
            template_field("template", "Template", paths.template.as_ref()),
        ];
        Self::new(
            fields,
            resource_href,
            loaded,
            "A new definition starts from the template; leave it empty to start blank.",
        )
    }

    /// The strip in edit mode showing what was submitted and why it was not saved.
    pub(crate) fn rejected(mut self, submitted: &[(&str, &str)], expected_hash: &str, error: String) -> Self {
        self.editing = true;
        for field in &mut self.fields {
            if let Some((_, value)) = submitted.iter().find(|(name, _)| *name == field.name) {
                (*value).clone_into(&mut field.value);
            }
        }
        expected_hash.clone_into(&mut self.expected_hash);
        self.error = Some(error);
        self
    }
}

/// A submitted register strip. Unknown fields are rejected.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegisterSubmission {
    base: String,
    dir: String,
    decision_prefix: String,
    #[serde(default)]
    decision_template: String,
    question_prefix: String,
    #[serde(default)]
    question_template: String,
    #[serde(default)]
    pub(crate) expected_hash: String,
}

impl RegisterSubmission {
    pub(crate) fn values(&self) -> Vec<(&str, &str)> {
        vec![
            ("base", self.base.as_str()),
            ("dir", self.dir.as_str()),
            ("decision_prefix", self.decision_prefix.as_str()),
            ("decision_template", self.decision_template.as_str()),
            ("question_prefix", self.question_prefix.as_str()),
            ("question_template", self.question_template.as_str()),
        ]
    }
}

/// A submitted registry type strip. Unknown fields are rejected.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistrySubmission {
    dir: String,
    #[serde(default)]
    template: String,
    #[serde(default)]
    pub(crate) expected_hash: String,
}

impl RegistrySubmission {
    pub(crate) fn values(&self) -> Vec<(&str, &str)> {
        vec![("dir", self.dir.as_str()), ("template", self.template.as_str())]
    }
}

fn location(raw: &str, key: &str) -> Result<VaultRelativePath, String> {
    configured_location(raw, key).map_err(|error| format!("{error}."))
}

fn template(raw: &str, key: &str) -> Result<Option<VaultRelativePath>, String> {
    if raw.trim().is_empty() {
        Ok(None)
    } else {
        location(raw, key).map(Some)
    }
}

async fn require_folder(context: &ManagementContext<'_>, label: &str, path: &VaultRelativePath) -> Result<(), String> {
    match list_folder(context.client, context.vault_name, path).await {
        Ok(FolderListing::Present(_)) => Ok(()),
        Ok(FolderListing::Missing) => Err(format!("{label} `{path}` does not exist in this vault.")),
        Ok(FolderListing::Unreadable { .. }) => Err(format!("{label} `{path}` is not a folder this vault can list.")),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => {
            Err("The vault could not be reached.".to_owned())
        }
    }
}

async fn require_file(context: &ManagementContext<'_>, label: &str, path: &VaultRelativePath) -> Result<(), String> {
    match read_attached(context.client, &format!("{}://{path}", context.vault_name)).await {
        Ok(AttachedRead::Text(_) | AttachedRead::NotText) => Ok(()),
        Ok(AttachedRead::Refused { .. }) => Err(format!("{label} `{path}` does not exist in this vault.")),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => {
            Err("The vault could not be reached.".to_owned())
        }
    }
}

/// Refuses to write over a file that could not be read, since its content would be lost.
fn writable_source<'a>(context: &ManagementContext<'a>) -> Result<&'a str, String> {
    let loaded = &context.catalogue.loaded;
    match &loaded.state {
        ConfigFileState::Unreadable(reason) => Err(format!("{reason} It is not written over until it can be read.")),
        ConfigFileState::Absent | ConfigFileState::Present | ConfigFileState::Unusable(_) => Ok(&loaded.source),
    }
}

async fn write_config(context: &ManagementContext<'_>, toml_text: &str, expected_hash: &str) -> Result<(), String> {
    let path = VaultRelativePath::scaffold(VAULT_MANAGEMENT_TOML_PATH);
    let hash = Some(expected_hash).filter(|hash| !hash.is_empty());
    match write_text(context.client, context.vault_name, &path, toml_text, hash).await {
        Ok(WriteOutcome::Written) => Ok(()),
        Ok(refused) => Err(refused
            .refusal_reason()
            .unwrap_or_else(|| "The settings could not be saved.".to_owned())),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => {
            Err("The vault could not be reached.".to_owned())
        }
    }
}

/// Validates and saves the Decisions register's settings.
///
/// # Errors
///
/// Returns a sentence saying what is wrong, for the strip to show beside the submitted values.
pub(crate) async fn save_register_settings(
    context: &ManagementContext<'_>,
    submitted: &RegisterSubmission,
) -> Result<(), String> {
    let settings = RegisterSettings {
        paths: RegisterPaths {
            base: location(&submitted.base, "decisions.base")?,
            dir: location(&submitted.dir, "decisions.dir")?,
        },
        decision: RecordTypeConfig {
            prefix: submitted.decision_prefix.trim().to_owned(),
            template: template(&submitted.decision_template, "decisions.decision.template")?,
        },
        question: RecordTypeConfig {
            prefix: submitted.question_prefix.trim().to_owned(),
            template: template(&submitted.question_template, "decisions.question.template")?,
        },
    };
    settings.validate().map_err(|error| format!("{error}."))?;
    require_folder(context, "Folder", &settings.paths.dir).await?;
    require_file(context, "Base", &settings.paths.base).await?;
    for template in [&settings.decision.template, &settings.question.template]
        .into_iter()
        .flatten()
    {
        require_file(context, "Template", template).await?;
    }
    let updated = with_register_settings(writable_source(context)?, &settings)
        .map_err(|error| format!("{VAULT_MANAGEMENT_TOML_PATH} cannot be updated until it is fixed: {error}."))?;
    write_config(context, &updated, &submitted.expected_hash).await
}

/// Validates and saves a registry type's locations.
///
/// # Errors
///
/// Returns a sentence saying what is wrong, for the strip to show beside the submitted values.
pub(crate) async fn save_registry_paths(
    context: &ManagementContext<'_>,
    kind: RegistryKind,
    submitted: &RegistrySubmission,
) -> Result<(), String> {
    let table = format!("registry.{}", kind.slug());
    let paths = RegistryTypePaths {
        dir: location(&submitted.dir, &format!("{table}.dir"))?,
        template: template(&submitted.template, &format!("{table}.template"))?,
    };
    paths.validate(kind).map_err(|error| format!("{error}."))?;
    require_folder(context, "Folder", &paths.dir).await?;
    if let Some(template) = &paths.template {
        require_file(context, "Template", template).await?;
    }
    let updated = with_registry_type_paths(writable_source(context)?, kind, &paths)
        .map_err(|error| format!("{VAULT_MANAGEMENT_TOML_PATH} cannot be updated until it is fixed: {error}."))?;
    write_config(context, &updated, &submitted.expected_hash).await
}
