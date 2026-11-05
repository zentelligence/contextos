//! `.contextos/vault-management.toml`: where the Vault Settings management categories find what they manage. The
//! Decisions register names its `.base` and the folder its records live in, and, for each of its two record types
//! (decisions and questions), the id series a new record of that type takes its id from and the template its body
//! starts from. Each of the six registry definition types names its folder and the optional template a new
//! definition starts from.
//!
//! Every key is optional and falls back to the base scaffold's own location, so a vault that has never written this
//! file behaves exactly as a freshly scaffolded one. A file that cannot be parsed, names an unknown key, or carries an
//! unsafe path is not silently repaired: [`load_vault_management_config`] reports the problem beside the defaults it
//! falls back to, so the screen can say why it is showing the defaults rather than what was written. Writes go
//! through [`with_register_settings`] and [`with_registry_type_paths`], which edit only the table being saved and
//! keep every comment and unrelated key in the file.
//!
//! A managed folder may not be one of the vault's append-only activity records (`memory/log`, `memory/sessions`),
//! which the Activity log shows read only, and no configured path may pass through a hidden (dot-prefixed) folder.

use serde::Deserialize;
use toml_edit::{DocumentMut, Item, Table, value};

use crate::mcp_client::McpClient;
use crate::vault_content::{AttachedRead, read_attached};
use crate::vault_relative_path::{VaultRelativePath, VaultRelativePathError};

pub const VAULT_MANAGEMENT_TOML_PATH: &str = ".contextos/vault-management.toml";

/// The folders a managed category may never point at: the append-only records the Activity log shows read only.
const ACTIVITY_ROOTS: &[&str] = &["memory/log", "memory/sessions"];

/// One kind of record in the Decisions register: told apart by its `type` frontmatter, or, for a record without one,
/// by its id's prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum RecordType {
    /// A commitment about how the vault works.
    Decision,
    /// A gap in information that could not be closed when it was found.
    Question,
}

impl RecordType {
    pub const ALL: [Self; 2] = [Self::Decision, Self::Question];

    /// The `type` frontmatter value, URL parameter, and configuration table name.
    #[must_use]
    pub const fn value(self) -> &'static str {
        match self {
            Self::Decision => "decision",
            Self::Question => "question",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Decision => "Decision",
            Self::Question => "Question",
        }
    }

    #[must_use]
    pub const fn plural_label(self) -> &'static str {
        match self {
            Self::Decision => "Decisions",
            Self::Question => "Questions",
        }
    }

    fn default_config(self) -> RecordTypeConfig {
        let (prefix, template) = match self {
            Self::Decision => ("VD", "templates/registry-vault-decision.md"),
            Self::Question => ("VQ", "templates/registry-vault-question.md"),
        };
        RecordTypeConfig {
            prefix: prefix.to_owned(),
            template: Some(scaffold_path(template)),
        }
    }
}

impl TryFrom<&str> for RecordType {
    type Error = UnknownKind;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.value() == raw)
            .ok_or(UnknownKind)
    }
}

/// One of the registry's definition types managed from Vault Settings. `registry/apps/` and `registry/workflows/`
/// are deliberately absent: the Apps and Workflows screens own them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum RegistryKind {
    Agents,
    Hats,
    Prompts,
    Roles,
    Scripts,
    Skills,
}

impl RegistryKind {
    pub const ALL: [Self; 6] = [
        Self::Agents,
        Self::Hats,
        Self::Prompts,
        Self::Roles,
        Self::Scripts,
        Self::Skills,
    ];

    /// The URL segment and configuration table name.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Hats => "hats",
            Self::Prompts => "prompts",
            Self::Roles => "roles",
            Self::Scripts => "scripts",
            Self::Skills => "skills",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Agents => "Agents",
            Self::Hats => "Hats",
            Self::Prompts => "Prompts",
            Self::Roles => "Roles",
            Self::Scripts => "Scripts",
            Self::Skills => "Skills",
        }
    }

    #[must_use]
    pub const fn singular(self) -> &'static str {
        match self {
            Self::Agents => "agent",
            Self::Hats => "hat",
            Self::Prompts => "prompt",
            Self::Roles => "role",
            Self::Scripts => "script",
            Self::Skills => "skill",
        }
    }

    /// One line on what a definition of this type is for, and which skill consumes it where one does.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Agents => "Delegated sub-agents with a contracted inputs and outputs shape, used by `SLUG:agent`.",
            Self::Hats => "Task-phase thinking modes and methods, used by `SLUG:hat`.",
            Self::Prompts => "Reusable prompt templates, drafted and retrieved by the prompt skill.",
            Self::Roles => "Cognitive postures adopted for a session, used by `SLUG:role`.",
            Self::Scripts => "Deterministic helper scripts called by skills and jobs.",
            Self::Skills => "Vault-defined skills, one folder each with a `SKILL.md` entry file.",
        }
    }

    /// How one definition of this type is laid out on disk, which decides what "New" creates.
    #[must_use]
    pub const fn layout(self) -> DefinitionLayout {
        match self {
            Self::Agents | Self::Hats | Self::Prompts | Self::Roles => DefinitionLayout::Markdown,
            Self::Scripts => DefinitionLayout::AnyFile,
            Self::Skills => DefinitionLayout::Folder { entry: "SKILL.md" },
        }
    }

    fn default_paths(self) -> RegistryTypePaths {
        let (dir, template) = match self {
            Self::Agents => ("registry/agents", Some("templates/registry-agent.md")),
            Self::Hats => ("registry/hats", Some("templates/registry-hat.md")),
            Self::Prompts => ("registry/prompts", Some("templates/registry-prompt.md")),
            Self::Roles => ("registry/roles", Some("templates/registry-role.md")),
            Self::Scripts => ("registry/scripts", None),
            Self::Skills => ("registry/skills", Some("templates/registry-skill.md")),
        };
        RegistryTypePaths {
            dir: scaffold_path(dir),
            template: template.map(scaffold_path),
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Agents => 0,
            Self::Hats => 1,
            Self::Prompts => 2,
            Self::Roles => 3,
            Self::Scripts => 4,
            Self::Skills => 5,
        }
    }
}

impl TryFrom<&str> for RegistryKind {
    type Error = UnknownKind;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        Self::ALL.into_iter().find(|kind| kind.slug() == raw).ok_or(UnknownKind)
    }
}

/// A URL segment or value that names no record type or registry type.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("no such vault management category")]
pub struct UnknownKind;

/// How one registry definition is laid out on disk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefinitionLayout {
    /// One Markdown file per definition; a new definition's name gains `.md`.
    Markdown,
    /// One file of any kind per definition (a script); a new definition's name must carry its own extension.
    AnyFile,
    /// One folder per definition, holding `entry` as its definition file.
    Folder { entry: &'static str },
}

/// Where the Decisions register lives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterPaths {
    /// The `.base` whose sidecar assigns each new record's id.
    pub base: VaultRelativePath,
    /// The folder every record note, of either type, lives in.
    pub dir: VaultRelativePath,
}

/// How one record type is minted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordTypeConfig {
    /// The id series a new record of this type takes its id from, and what tells an untyped record's type.
    pub prefix: String,
    /// The note whose body a new record of this type starts from, if any.
    pub template: Option<VaultRelativePath>,
}

/// The Decisions register's whole configuration: its locations and both record types.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterSettings {
    pub paths: RegisterPaths,
    pub decision: RecordTypeConfig,
    pub question: RecordTypeConfig,
}

impl RegisterSettings {
    #[must_use]
    pub fn record_type(&self, kind: RecordType) -> &RecordTypeConfig {
        match kind {
            RecordType::Decision => &self.decision,
            RecordType::Question => &self.question,
        }
    }

    /// Checks the shape the register needs beyond each path being safe: a `.base` base, Markdown templates, plain
    /// and distinct id prefixes, and a folder that is not one of the activity records.
    ///
    /// # Errors
    ///
    /// Returns the first key that is wrong.
    pub fn validate(&self) -> Result<(), VaultManagementConfigError> {
        if !has_extension(&self.paths.base, "base") {
            return Err(VaultManagementConfigError::BaseExtension {
                key: "decisions.base".to_owned(),
            });
        }
        check_managed_folder(&self.paths.dir, "decisions.dir")?;
        for kind in RecordType::ALL {
            let config = self.record_type(kind);
            let table = format!("decisions.{}", kind.value());
            let valid_prefix = (1..=32).contains(&config.prefix.len())
                && config.prefix.starts_with(|ch: char| ch.is_ascii_alphabetic())
                && config.prefix.bytes().all(|byte| byte.is_ascii_alphanumeric());
            if !valid_prefix {
                return Err(VaultManagementConfigError::Prefix {
                    key: format!("{table}.prefix"),
                });
            }
            if config
                .template
                .as_ref()
                .is_some_and(|template| !has_extension(template, "md"))
            {
                return Err(VaultManagementConfigError::TemplateExtension {
                    key: format!("{table}.template"),
                });
            }
        }
        if self.decision.prefix.eq_ignore_ascii_case(&self.question.prefix) {
            return Err(VaultManagementConfigError::DuplicatePrefix);
        }
        Ok(())
    }
}

impl Default for RegisterSettings {
    fn default() -> Self {
        Self {
            paths: RegisterPaths {
                base: scaffold_path("memory/decisions.base"),
                dir: scaffold_path("memory/decisions"),
            },
            decision: RecordType::Decision.default_config(),
            question: RecordType::Question.default_config(),
        }
    }
}

/// Where one registry definition type lives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryTypePaths {
    pub dir: VaultRelativePath,
    /// The file a new definition's content starts from, if any.
    pub template: Option<VaultRelativePath>,
}

impl RegistryTypePaths {
    /// Checks the type's folder is not one of the activity records.
    ///
    /// # Errors
    ///
    /// Returns [`VaultManagementConfigError::ReservedLocation`] naming the key.
    pub fn validate(&self, kind: RegistryKind) -> Result<(), VaultManagementConfigError> {
        check_managed_folder(&self.dir, &format!("registry.{}.dir", kind.slug()))
    }
}

/// `.contextos/vault-management.toml`'s resolved content: every location, configured or defaulted.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VaultManagementConfig {
    register: RegisterSettings,
    registry: RegistryPaths,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RegistryPaths([RegistryTypePaths; 6]);

impl Default for RegistryPaths {
    fn default() -> Self {
        Self(RegistryKind::ALL.map(RegistryKind::default_paths))
    }
}

/// Why `.contextos/vault-management.toml` could not be used as written.
#[derive(Debug, thiserror::Error)]
pub enum VaultManagementConfigError {
    #[error("the file is not valid: {0}")]
    Toml(String),
    #[error("`{key}` {source}")]
    Path {
        key: String,
        source: VaultRelativePathError,
    },
    #[error("`{key}` must name a `.base` file")]
    BaseExtension { key: String },
    #[error("`{key}` must name a Markdown (`.md`) file")]
    TemplateExtension { key: String },
    #[error("`{key}` must be 1 to 32 letters or digits, starting with a letter")]
    Prefix { key: String },
    #[error("decisions and questions need different id prefixes")]
    DuplicatePrefix,
    #[error("`{key}` must not be the operation log or session summaries, which are append-only")]
    ReservedLocation { key: String },
    #[error("`{key}` must not pass through a hidden (dot-prefixed) folder")]
    Hidden { key: String },
}

impl VaultManagementConfigError {
    /// A stable machine-readable code for this failure.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Toml(_) => "vault-management/invalid-toml",
            Self::Path { source, .. } => source.code(),
            Self::BaseExtension { .. } => "vault-management/base-extension",
            Self::TemplateExtension { .. } => "vault-management/template-extension",
            Self::Prefix { .. } => "vault-management/invalid-prefix",
            Self::DuplicatePrefix => "vault-management/duplicate-prefix",
            Self::ReservedLocation { .. } => "vault-management/reserved-location",
            Self::Hidden { .. } => "vault-management/hidden-path",
        }
    }
}

impl VaultManagementConfig {
    #[must_use]
    pub fn register(&self) -> &RegisterSettings {
        &self.register
    }

    #[must_use]
    pub fn registry(&self, kind: RegistryKind) -> &RegistryTypePaths {
        &self.registry.0[kind.index()]
    }
}

fn scaffold_path(literal: &'static str) -> VaultRelativePath {
    VaultRelativePath::scaffold(literal)
}

/// Refuses a managed folder that is, or lies inside, one of the append-only activity records.
fn check_managed_folder(dir: &VaultRelativePath, key: &str) -> Result<(), VaultManagementConfigError> {
    let reserved = ACTIVITY_ROOTS.iter().any(|root| {
        let root = VaultRelativePath::scaffold(root);
        *dir == root || dir.strip_ancestor(&root).is_some()
    });
    if reserved {
        Err(VaultManagementConfigError::ReservedLocation { key: key.to_owned() })
    } else {
        Ok(())
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawFile {
    decisions: RawRegister,
    registry: RawRegistry,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawRegister {
    base: Option<String>,
    dir: Option<String>,
    decision: RawRecordType,
    question: RawRecordType,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawRecordType {
    prefix: Option<String>,
    template: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawRegistry {
    agents: RawRegistryType,
    hats: RawRegistryType,
    prompts: RawRegistryType,
    roles: RawRegistryType,
    scripts: RawRegistryType,
    skills: RawRegistryType,
}

impl RawRegistry {
    fn take(&mut self, kind: RegistryKind) -> RawRegistryType {
        std::mem::take(match kind {
            RegistryKind::Agents => &mut self.agents,
            RegistryKind::Hats => &mut self.hats,
            RegistryKind::Prompts => &mut self.prompts,
            RegistryKind::Roles => &mut self.roles,
            RegistryKind::Scripts => &mut self.scripts,
            RegistryKind::Skills => &mut self.skills,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawRegistryType {
    dir: Option<String>,
    template: Option<String>,
}

/// `raw` as a configured location: a safe vault-relative path through no hidden folder.
///
/// # Errors
///
/// Returns the reason, naming `key`.
pub fn configured_location(raw: &str, key: &str) -> Result<VaultRelativePath, VaultManagementConfigError> {
    let path = VaultRelativePath::try_from(raw).map_err(|source| VaultManagementConfigError::Path {
        key: key.to_owned(),
        source,
    })?;
    if path.as_str().split('/').any(|segment| segment.starts_with('.')) {
        return Err(VaultManagementConfigError::Hidden { key: key.to_owned() });
    }
    Ok(path)
}

fn configured_path(
    raw: Option<String>,
    default: &VaultRelativePath,
    key: &str,
) -> Result<VaultRelativePath, VaultManagementConfigError> {
    match raw {
        None => Ok(default.clone()),
        Some(raw) => configured_location(&raw, key),
    }
}

/// A template key: absent keeps the default, an explicit empty string means "no template".
fn configured_template(
    raw: Option<String>,
    default: Option<&VaultRelativePath>,
    key: &str,
) -> Result<Option<VaultRelativePath>, VaultManagementConfigError> {
    match raw {
        None => Ok(default.cloned()),
        Some(raw) if raw.trim().is_empty() => Ok(None),
        Some(raw) => configured_location(&raw, key).map(Some),
    }
}

fn has_extension(path: &VaultRelativePath, extension: &str) -> bool {
    path.file_name()
        .rsplit_once('.')
        .is_some_and(|(stem, found)| !stem.is_empty() && found.eq_ignore_ascii_case(extension))
}

impl TryFrom<(RecordType, RawRecordType)> for RecordTypeConfig {
    type Error = VaultManagementConfigError;

    fn try_from((kind, raw): (RecordType, RawRecordType)) -> Result<Self, Self::Error> {
        let defaults = kind.default_config();
        let table = format!("decisions.{}", kind.value());
        Ok(Self {
            prefix: raw.prefix.map_or(defaults.prefix, |prefix| prefix.trim().to_owned()),
            template: configured_template(raw.template, defaults.template.as_ref(), &format!("{table}.template"))?,
        })
    }
}

impl TryFrom<RawRegister> for RegisterSettings {
    type Error = VaultManagementConfigError;

    fn try_from(raw: RawRegister) -> Result<Self, Self::Error> {
        let defaults = Self::default();
        let settings = Self {
            paths: RegisterPaths {
                base: configured_path(raw.base, &defaults.paths.base, "decisions.base")?,
                dir: configured_path(raw.dir, &defaults.paths.dir, "decisions.dir")?,
            },
            decision: RecordTypeConfig::try_from((RecordType::Decision, raw.decision))?,
            question: RecordTypeConfig::try_from((RecordType::Question, raw.question))?,
        };
        settings.validate()?;
        Ok(settings)
    }
}

impl TryFrom<(RegistryKind, RawRegistryType)> for RegistryTypePaths {
    type Error = VaultManagementConfigError;

    fn try_from((kind, raw): (RegistryKind, RawRegistryType)) -> Result<Self, Self::Error> {
        let defaults = kind.default_paths();
        let table = format!("registry.{}", kind.slug());
        let paths = Self {
            dir: configured_path(raw.dir, &defaults.dir, &format!("{table}.dir"))?,
            template: configured_template(raw.template, defaults.template.as_ref(), &format!("{table}.template"))?,
        };
        paths.validate(kind)?;
        Ok(paths)
    }
}

impl TryFrom<&str> for VaultManagementConfig {
    type Error = VaultManagementConfigError;

    fn try_from(source: &str) -> Result<Self, Self::Error> {
        let mut raw: RawFile =
            toml::from_str(source).map_err(|error| VaultManagementConfigError::Toml(error.message().to_owned()))?;
        let mut registry = Vec::with_capacity(RegistryKind::ALL.len());
        for kind in RegistryKind::ALL {
            registry.push(RegistryTypePaths::try_from((kind, raw.registry.take(kind)))?);
        }
        let registry: [RegistryTypePaths; 6] = registry
            .try_into()
            .map_err(|_| VaultManagementConfigError::Toml("the registry table is incomplete".to_owned()))?;
        Ok(Self {
            register: RegisterSettings::try_from(raw.decisions)?,
            registry: RegistryPaths(registry),
        })
    }
}

fn parse_document(source: &str) -> Result<DocumentMut, VaultManagementConfigError> {
    source
        .parse::<DocumentMut>()
        .map_err(|error| VaultManagementConfigError::Toml(error.message().to_owned()))
}

/// The table at `path` in `document`, created as a regular table when absent; a table created only to hold another
/// stays implicit so it is not written as an empty header.
fn table_at<'a>(document: &'a mut DocumentMut, path: &[&str]) -> Result<&'a mut Table, VaultManagementConfigError> {
    let mut table = document.as_table_mut();
    for segment in path {
        let entry = table.entry(segment).or_insert_with(|| {
            let mut created = Table::new();
            created.set_implicit(true);
            Item::Table(created)
        });
        table = entry
            .as_table_mut()
            .ok_or_else(|| VaultManagementConfigError::Toml(format!("`{segment}` must be a table")))?;
    }
    Ok(table)
}

fn template_value(template: Option<&VaultRelativePath>) -> Item {
    value(template.map_or("", VaultRelativePath::as_str))
}

/// `source` with the `[decisions]` table and its two record-type tables set to `settings`, every comment and other
/// table kept as written.
///
/// # Errors
///
/// Returns [`VaultManagementConfigError::Toml`] when `source` is not valid TOML: a file broken by hand is reported,
/// never overwritten.
pub fn with_register_settings(source: &str, settings: &RegisterSettings) -> Result<String, VaultManagementConfigError> {
    let mut document = parse_document(source)?;
    let table = table_at(&mut document, &["decisions"])?;
    table.set_implicit(false);
    table.insert("base", value(settings.paths.base.as_str()));
    table.insert("dir", value(settings.paths.dir.as_str()));
    for kind in RecordType::ALL {
        let config = settings.record_type(kind);
        let kind_table = table_at(&mut document, &["decisions", kind.value()])?;
        kind_table.set_implicit(false);
        kind_table.insert("prefix", value(config.prefix.as_str()));
        kind_table.insert("template", template_value(config.template.as_ref()));
    }
    Ok(document.to_string())
}

/// `source` with `[registry.<kind>]` set to `paths`, every comment and other table kept as written.
///
/// # Errors
///
/// Returns [`VaultManagementConfigError::Toml`] when `source` is not valid TOML.
pub fn with_registry_type_paths(
    source: &str,
    kind: RegistryKind,
    paths: &RegistryTypePaths,
) -> Result<String, VaultManagementConfigError> {
    let mut document = parse_document(source)?;
    let table = table_at(&mut document, &["registry", kind.slug()])?;
    table.set_implicit(false);
    table.insert("dir", value(paths.dir.as_str()));
    table.insert("template", template_value(paths.template.as_ref()));
    Ok(document.to_string())
}

/// How the file stood when it was read.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ConfigFileState {
    /// No file: the scaffold defaults are in effect, and a save creates it.
    #[default]
    Absent,
    /// The file was read and is in effect.
    Present,
    /// The file exists but is not in effect, for the reason given.
    Unusable(String),
    /// The file could not be read at all, so nothing may be written over it.
    Unreadable(String),
}

/// The configuration a screen renders against, the file's raw text and hash for a later save, and the file's state.
#[derive(Clone, Debug, Default)]
pub struct LoadedVaultManagementConfig {
    pub config: VaultManagementConfig,
    /// The file's exact text, empty when it does not exist.
    pub source: String,
    /// The hash a save presents as `expected_hash`; `None` when the file does not exist yet.
    pub expected_hash: Option<String>,
    pub state: ConfigFileState,
}

impl LoadedVaultManagementConfig {
    /// Why the file's content is not in effect, when it is not.
    #[must_use]
    pub fn problem(&self) -> Option<String> {
        match &self.state {
            ConfigFileState::Absent | ConfigFileState::Present => None,
            ConfigFileState::Unusable(reason) | ConfigFileState::Unreadable(reason) => Some(reason.clone()),
        }
    }
}

/// Loads `vault_name`'s vault management configuration. Never fails outright: a missing file is the scaffold
/// defaults with no problem to report; an unreadable or invalid one is the defaults plus the reason.
pub async fn load_vault_management_config(client: &McpClient, vault_name: &str) -> LoadedVaultManagementConfig {
    let read = read_attached(client, &format!("{vault_name}://{VAULT_MANAGEMENT_TOML_PATH}")).await;
    match read {
        Ok(AttachedRead::Text(attached)) => match VaultManagementConfig::try_from(attached.text.as_str()) {
            Ok(config) => LoadedVaultManagementConfig {
                config,
                source: attached.text,
                expected_hash: attached.content_hash,
                state: ConfigFileState::Present,
            },
            Err(error) => LoadedVaultManagementConfig {
                config: VaultManagementConfig::default(),
                source: attached.text,
                expected_hash: attached.content_hash,
                state: ConfigFileState::Unusable(format!("{VAULT_MANAGEMENT_TOML_PATH}: {error}")),
            },
        },
        Ok(AttachedRead::Refused { code }) if code.as_deref() == Some("path/not-found") => {
            LoadedVaultManagementConfig::default()
        }
        Ok(AttachedRead::Refused { .. } | AttachedRead::NotText) => LoadedVaultManagementConfig {
            state: ConfigFileState::Unreadable(format!("{VAULT_MANAGEMENT_TOML_PATH} could not be read as text.")),
            ..LoadedVaultManagementConfig::default()
        },
        Err(_) => LoadedVaultManagementConfig {
            state: ConfigFileState::Unreadable(
                "The vault could not be reached to read its management settings.".to_owned(),
            ),
            ..LoadedVaultManagementConfig::default()
        },
    }
}

#[cfg(test)]
#[path = "vault_management_config_test.rs"]
mod tests;
