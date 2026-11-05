//! Turns the loaded configuration into validated agent host projection settings.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use contextos_core::{ResolvedPath, VaultSet};
use contextos_projection::{
    ClaudeTiers, CodexTier, CodexTiers, ConfigurationId, HostRender, ModelName, ProjectionError, ReadOnlyTools,
    ReasoningEffort,
};
use sha2::{Digest, Sha256};

use crate::projection_config::{
    ClaudeProjectionConfig, ClaudeTiersConfig, CodexProjectionConfig, CodexTierConfig, CodexTiersConfig,
    ProjectionConfig,
};
use crate::state_dir::default_state_root;
use crate::tool_effect::{ToolEffect, tool_names};
use crate::{Config, ConfigError, ConfigLocation};

/// One enabled agent host, with every path absolute and outside every vault.
#[derive(Clone, Debug)]
pub(crate) struct HostSettings {
    /// The folder agent files are written to.
    pub(crate) agents: PathBuf,
    /// The folder skill folders are written to.
    pub(crate) skills: PathBuf,
    /// The global instructions file the orientation block is kept in. Validated at load; its folder says whether the host is installed, and the
    /// orientation block keeps its block in it.
    pub(crate) instructions: PathBuf,
    /// How the host's agent files are rendered, which also says which host it is.
    pub(crate) render: HostRender,
}

impl HostSettings {
    /// Whether the host is installed: its home folder, the one holding its instructions file, exists. Projection never
    /// creates that folder, so a host the operator does not use is never given one.
    pub(crate) fn is_installed(&self) -> bool {
        self.instructions.parent().is_some_and(Path::is_dir)
    }
}

/// Everything projection needs from the configuration when it is enabled.
#[derive(Clone, Debug)]
pub(crate) struct ProjectionSettings {
    /// Identifies this configuration as the owner of what it projects.
    pub(crate) configuration: ConfigurationId,
    /// The server's state root, which holds the ownership records and the projection lock.
    pub(crate) state_root: PathBuf,
    /// Every enabled host, Claude Code first.
    pub(crate) hosts: Vec<HostSettings>,
}

/// Whether, and how, this configuration projects into agent hosts.
#[derive(Clone, Debug)]
pub(crate) enum ProjectionSetup {
    /// Nothing is projected and no host folder is touched.
    Disabled,
    /// Projection runs for the listed hosts.
    Enabled(ProjectionSettings),
}

/// A configuration file's path, as the identity of the configuration that owns what it projects.
struct ConfigFilePath<'a>(&'a Path);

impl TryFrom<ConfigFilePath<'_>> for ConfigurationId {
    type Error = ProjectionError;

    /// The SHA-256 of the file's resolved path (the path as written while the file does not exist yet), so every
    /// process started from the same file agrees on who owns an entry, and two files never do.
    fn try_from(value: ConfigFilePath<'_>) -> Result<Self, Self::Error> {
        // The same resolution policy as every other location comparison: links and `..` are followed as far as the
        // path exists and the rest is kept as written, so the identity does not change when the file is created.
        let resolved = ResolvedPath::try_from(value.0).map_or_else(
            |_| value.0.to_path_buf(),
            |resolved| <&Path>::from(&resolved).to_path_buf(),
        );
        let digest = Sha256::digest(resolved.as_os_str().as_encoded_bytes());
        let hex = digest.iter().fold(String::with_capacity(64), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        });
        Self::try_from(hex.as_str())
    }
}

/// One configured agents or skills folder that is a symbolic link, and where the link points.
struct LinkedFolder {
    key: &'static str,
    link: PathBuf,
    destination: PathBuf,
}

/// Every enabled host's agents and skills folder that is itself a symbolic link, dangling or not. Projection replaces
/// whole entries inside these folders and refuses to follow a link below them, so a linked folder cannot be projected
/// into; links in the folders above one are followed by the resolution policy as before.
fn linked_folders(home: &Path, section: &ProjectionConfig) -> Vec<LinkedFolder> {
    let mut configured = Vec::new();
    if section.claude.enabled {
        configured.push(("projection.claude.agents", &section.claude.agents));
        configured.push(("projection.claude.skills", &section.claude.skills));
    }
    if section.codex.enabled {
        configured.push(("projection.codex.agents", &section.codex.agents));
        configured.push(("projection.codex.skills", &section.codex.skills));
    }
    configured
        .into_iter()
        .filter_map(|(key, path)| {
            let link = expand_home(path, home);
            if !link.is_absolute() {
                return None;
            }
            let is_link = std::fs::symlink_metadata(&link).is_ok_and(|metadata| metadata.file_type().is_symlink());
            let destination = std::fs::read_link(&link).ok().filter(|_| is_link)?;
            Some(LinkedFolder { key, link, destination })
        })
        .collect()
}

/// Expands a leading `~` component from `home`; `~name` and every other path are left as written.
fn expand_home(path: &Path, home: &Path) -> PathBuf {
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(first)) if first == "~" => home.join(components.as_path()),
        _ => path.to_path_buf(),
    }
}

/// What a configured target names, which decides the folder that must not hold a vault.
#[derive(Clone, Copy)]
enum TargetKind {
    /// A folder projected entries are written into.
    Folder,
    /// A file whose containing folder is the host's home.
    FileIn,
}

/// Everything a target is checked against: the operator's home, the vaults and state folder it must stay out of, and
/// the targets already accepted, which it must not overlap.
struct Targets<'a> {
    home: &'a Path,
    roots: &'a VaultSet,
    state_root: ResolvedPath,
    accepted: RefCell<Vec<(String, ResolvedPath)>>,
}

impl Targets<'_> {
    /// `configured` with `~` expanded, required absolute, outside every vault root and the state folder, not a folder
    /// that holds a vault root (a folder projected into), nor a folder (or the folder of a file) that holds the state
    /// folder, because projecting an entry named like that folder would replace it, and not overlapping any target accepted before it. Every comparison uses the one
    /// [`ResolvedPath`] policy.
    fn resolve(&self, key: &str, configured: &Path, kind: TargetKind) -> Result<PathBuf, ConfigError> {
        let expanded = expand_home(configured, self.home);
        if !expanded.is_absolute() {
            return Err(ConfigError::ProjectionTargetNotAbsolute {
                key: key.to_owned(),
                path: configured.to_path_buf(),
            });
        }
        let unresolvable = |source| ConfigError::ProjectionTargetUnresolvable {
            key: key.to_owned(),
            path: configured.to_path_buf(),
            source,
        };
        if expanded.components().any(|component| component == Component::ParentDir) {
            return Err(unresolvable(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "it contains a `..` component",
            )));
        }
        let resolved = ResolvedPath::try_from(expanded.as_path()).map_err(unresolvable)?;
        for root in self.roots {
            let root_path = ResolvedPath::try_from(root.path()).map_err(unresolvable)?;
            if root_path.contains(&resolved) {
                return Err(ConfigError::ProjectionTargetInVault {
                    key: key.to_owned(),
                    path: configured.to_path_buf(),
                    vault: root.name().to_owned(),
                });
            }
            // Only a folder projected into can replace a vault named like one of its entries. A vault beside an
            // instructions file is safe: the file's own place is checked above, and nothing else is written there.
            if matches!(kind, TargetKind::Folder) && Self::holds(&resolved, kind, &root_path) {
                return Err(ConfigError::ProjectionVaultInTarget {
                    key: key.to_owned(),
                    path: configured.to_path_buf(),
                    vault: root.name().to_owned(),
                });
            }
        }
        if self.state_root.contains(&resolved) {
            return Err(ConfigError::ProjectionTargetInStateRoot {
                key: key.to_owned(),
                path: configured.to_path_buf(),
            });
        }
        if Self::holds(&resolved, kind, &self.state_root) {
            return Err(ConfigError::ProjectionStateRootInTarget {
                key: key.to_owned(),
                path: configured.to_path_buf(),
            });
        }
        let mut accepted = self.accepted.borrow_mut();
        if let Some((other, _)) = accepted
            .iter()
            .find(|(_, earlier)| earlier.contains(&resolved) || resolved.contains(earlier))
        {
            return Err(ConfigError::ProjectionTargetsOverlap {
                key: key.to_owned(),
                path: configured.to_path_buf(),
                other: other.clone(),
            });
        }
        accepted.push((key.to_owned(), resolved));
        Ok(expanded)
    }

    /// Whether the folder a target writes into (the target, or for a file its containing folder) holds `inner`.
    fn holds(resolved: &ResolvedPath, kind: TargetKind, inner: &ResolvedPath) -> bool {
        match kind {
            TargetKind::Folder => resolved.contains(inner),
            TargetKind::FileIn => resolved.parent().is_some_and(|folder| folder.contains(inner)),
        }
    }
}

fn invalid(key: &str) -> impl FnOnce(ProjectionError) -> ConfigError + '_ {
    move |source| ConfigError::ProjectionSetting {
        key: key.to_owned(),
        source: Box::new(source),
    }
}

fn model(key: &str, value: &str) -> Result<ModelName, ConfigError> {
    ModelName::try_from(value).map_err(invalid(key))
}

impl TryFrom<&ClaudeTiersConfig> for ClaudeTiers {
    type Error = ConfigError;

    fn try_from(value: &ClaudeTiersConfig) -> Result<Self, Self::Error> {
        Ok(Self::new(
            model("projection.claude.tiers.fast", &value.fast)?,
            model("projection.claude.tiers.balanced", &value.balanced)?,
            model("projection.claude.tiers.deep", &value.deep)?,
        ))
    }
}

/// One Codex tier's configuration with the key it came from and the effort it takes when none is set.
struct CodexTierInput<'a> {
    key: &'static str,
    configured: &'a CodexTierConfig,
    default_effort: ReasoningEffort,
}

impl TryFrom<CodexTierInput<'_>> for CodexTier {
    type Error = ConfigError;

    fn try_from(value: CodexTierInput<'_>) -> Result<Self, Self::Error> {
        let model = value
            .configured
            .model
            .as_deref()
            .map(|name| model(&format!("{}.model", value.key), name))
            .transpose()?;
        let effort = match value.configured.effort.as_deref() {
            None => value.default_effort,
            Some(name) => ReasoningEffort::try_from(name).map_err(invalid(&format!("{}.effort", value.key)))?,
        };
        Ok(Self::new(model, effort))
    }
}

impl TryFrom<&CodexTiersConfig> for CodexTiers {
    type Error = ConfigError;

    fn try_from(value: &CodexTiersConfig) -> Result<Self, Self::Error> {
        Ok(Self::new(
            CodexTier::try_from(CodexTierInput {
                key: "projection.codex.tiers.fast",
                configured: &value.fast,
                default_effort: ReasoningEffort::Low,
            })?,
            CodexTier::try_from(CodexTierInput {
                key: "projection.codex.tiers.balanced",
                configured: &value.balanced,
                default_effort: ReasoningEffort::Medium,
            })?,
            CodexTier::try_from(CodexTierInput {
                key: "projection.codex.tiers.deep",
                configured: &value.deep,
                default_effort: ReasoningEffort::High,
            })?,
        ))
    }
}

fn claude_host(targets: &Targets<'_>, configured: &ClaudeProjectionConfig) -> Result<HostSettings, ConfigError> {
    let tools = tool_names(ToolEffect::ReadOnly);
    Ok(HostSettings {
        agents: targets.resolve("projection.claude.agents", &configured.agents, TargetKind::Folder)?,
        skills: targets.resolve("projection.claude.skills", &configured.skills, TargetKind::Folder)?,
        instructions: targets.resolve(
            "projection.claude.instructions",
            &configured.instructions,
            TargetKind::FileIn,
        )?,
        render: HostRender::Claude {
            tiers: ClaudeTiers::try_from(&configured.tiers)?,
            read_only_tools: ReadOnlyTools::try_from((configured.mcp_server.as_str(), tools.as_slice()))
                .map_err(invalid("projection.claude.mcp_server"))?,
        },
    })
}

fn codex_host(targets: &Targets<'_>, configured: &CodexProjectionConfig) -> Result<HostSettings, ConfigError> {
    Ok(HostSettings {
        agents: targets.resolve("projection.codex.agents", &configured.agents, TargetKind::Folder)?,
        skills: targets.resolve("projection.codex.skills", &configured.skills, TargetKind::Folder)?,
        instructions: targets.resolve(
            "projection.codex.instructions",
            &configured.instructions,
            TargetKind::FileIn,
        )?,
        render: HostRender::Codex {
            tiers: CodexTiers::try_from(&configured.tiers)?,
        },
    })
}

impl TryFrom<&Config> for ProjectionSetup {
    type Error = ConfigError;

    fn try_from(value: &Config) -> Result<Self, Self::Error> {
        let section = value.projection.clone().unwrap_or_default();
        let enabled = section
            .enabled
            .unwrap_or(value.provenance.location == ConfigLocation::PlatformDefault);
        if !enabled || !(section.claude.enabled || section.codex.enabled) {
            return Ok(Self::Disabled);
        }
        let home = value
            .provenance
            .home
            .as_deref()
            .ok_or(ConfigError::ProjectionHomeUnavailable)?;
        let path = value
            .provenance
            .path
            .as_deref()
            .ok_or(ConfigError::ProjectionConfigurationUnnamed)?;
        let configuration = ConfigurationId::try_from(ConfigFilePath(path)).map_err(invalid("projection"))?;
        let roots = VaultSet::try_from(value)?;
        let state_root = default_state_root(home);
        let targets = Targets {
            home,
            roots: &roots,
            state_root: ResolvedPath::try_from(state_root.as_path()).map_err(|source| {
                ConfigError::ProjectionTargetUnresolvable {
                    key: "state root".to_owned(),
                    path: state_root.clone(),
                    source,
                }
            })?,
            accepted: RefCell::new(Vec::new()),
        };
        let mut hosts = Vec::new();
        let mut resolved = Ok(());
        if section.claude.enabled {
            match claude_host(&targets, &section.claude) {
                Ok(host) => hosts.push(host),
                Err(error) => resolved = Err(error),
            }
        }
        if resolved.is_ok() && section.codex.enabled {
            match codex_host(&targets, &section.codex) {
                Ok(host) => hosts.push(host),
                Err(error) => resolved = Err(error),
            }
        }
        // A linked folder is the more useful thing to name, unless resolution found a different fault first: a link
        // that points nowhere cannot be resolved at all, and says so as the same problem.
        let linked = linked_folders(home, &section);
        match resolved {
            Err(error) if linked.is_empty() || !matches!(error, ConfigError::ProjectionTargetUnresolvable { .. }) => {
                return Err(error);
            }
            _ if !linked.is_empty() => {
                return Err(ConfigError::ProjectionLinkedFolders {
                    folders: linked
                        .into_iter()
                        .map(|folder| (folder.key.to_owned(), folder.link, folder.destination))
                        .collect(),
                });
            }
            _ => {}
        }
        Ok(Self::Enabled(ProjectionSettings {
            configuration,
            state_root,
            hosts,
        }))
    }
}

#[cfg(test)]
#[path = "projection_setup_test.rs"]
mod tests;
