//! What the Vault Settings "Vault management" group can show for one vault: the six management categories, whether
//! each is present in the sidebar, and how many items each holds. Built from one folder listing per configured
//! location, read once per request and handed on to whichever category page is being rendered, so a page never lists
//! the same folder twice.
//!
//! Presence follows what the vault has provisioned. Standards, Templates, and the Activity log appear only when their
//! folder exists, and an existing but empty folder still appears, with an empty view. The Decisions register always
//! appears, as does the Registry once any one of its types is provisioned: their locations are configurable from
//! their own page, so hiding them while a location is wrong would leave nowhere to correct it.

use crate::mcp_client::McpClient;
use crate::vault_management_config::{
    DefinitionLayout, LoadedVaultManagementConfig, RegistryKind, load_vault_management_config,
};
use crate::vault_relative_path::VaultRelativePath;
use crate::vault_tree::{FolderListing, VaultFile, VaultFolder, list_folder};

/// The top-level standards folder.
pub const STANDARDS_DIR: &str = "standards";
/// The top-level templates folder.
pub const TEMPLATES_DIR: &str = "templates";
/// The append-only operation log written by `contextos-mcp`.
pub const OPERATION_LOG_DIR: &str = "memory/log";
/// The append-only session summaries written at the end of a working session.
pub const SESSIONS_DIR: &str = "memory/sessions";

/// One category in the "Vault management" sidebar group, in display order.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ManagementCategory {
    /// The Decisions register: decisions and questions, in one tree.
    Decisions,
    Registry,
    Standards,
    Templates,
    Activity,
}

impl ManagementCategory {
    pub const ALL: [Self; 5] = [
        Self::Decisions,
        Self::Registry,
        Self::Standards,
        Self::Templates,
        Self::Activity,
    ];

    /// The URL segment beneath `/{vault}/settings/`.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Decisions => "decisions",
            Self::Registry => "registry",
            Self::Standards => "standards",
            Self::Templates => "templates",
            Self::Activity => "activity",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Decisions => "Decisions",
            Self::Registry => "Registry",
            Self::Standards => "Standards",
            Self::Templates => "Templates",
            Self::Activity => "Activity log",
        }
    }

    #[must_use]
    pub const fn icon(self) -> &'static str {
        match self {
            Self::Decisions => "scale",
            Self::Registry => "library",
            Self::Standards => "ruler",
            Self::Templates => "layout-template",
            Self::Activity => "history",
        }
    }

    /// What the count beside this category in the sidebar is counting, for its accessible label.
    #[must_use]
    pub const fn count_noun(self) -> &'static str {
        match self {
            Self::Decisions => "records",
            Self::Registry => "definitions",
            Self::Standards => "standards",
            Self::Templates => "templates",
            Self::Activity => "logged days",
        }
    }
}

impl TryFrom<&str> for ManagementCategory {
    type Error = crate::vault_management_config::UnknownKind;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|category| category.slug() == raw)
            .ok_or(crate::vault_management_config::UnknownKind)
    }
}

/// Every configured location's listing for one vault, read once per request.
#[derive(Clone, Debug)]
pub struct Catalogue {
    pub loaded: LoadedVaultManagementConfig,
    pub decisions: FolderListing,
    pub registry: Vec<(RegistryKind, FolderListing)>,
    pub standards: FolderListing,
    pub templates: FolderListing,
    pub operation_log: FolderListing,
    pub sessions: FolderListing,
}

/// One "Vault management" sidebar entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CategorySummary {
    pub category: ManagementCategory,
    pub count: usize,
}

/// A record note in a register folder: Markdown, and not the folder's own index.
#[must_use]
pub fn is_record(file: &VaultFile) -> bool {
    file.is_markdown() && !file.is_index()
}

/// A day file in the operation log: `YYYY-MM-DD.md`.
#[must_use]
pub fn is_log_day(file: &VaultFile) -> bool {
    let Some(stem) = file.name.strip_suffix(".md") else {
        return false;
    };
    let bytes = stem.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

/// How many definitions a registry type's folder holds, by that type's own layout.
#[must_use]
pub fn count_definitions(kind: RegistryKind, folder: &VaultFolder) -> usize {
    match kind.layout() {
        DefinitionLayout::Markdown => folder.count_files(&|file| file.is_markdown() && !file.is_index()),
        DefinitionLayout::AnyFile => folder.count_files(&|file| !file.is_index()),
        DefinitionLayout::Folder { entry } => count_entry_folders(folder, entry),
    }
}

fn count_entry_folders(folder: &VaultFolder, entry: &str) -> usize {
    folder
        .folders
        .iter()
        .map(|child| usize::from(child.files.iter().any(|file| file.name == entry)) + count_entry_folders(child, entry))
        .sum()
}

impl Catalogue {
    /// Whether `category` appears in the sidebar and can be opened.
    #[must_use]
    pub fn is_present(&self, category: ManagementCategory) -> bool {
        match category {
            ManagementCategory::Decisions => true,
            ManagementCategory::Registry => self.registry.iter().any(|(_, listing)| is_provisioned(listing)),
            ManagementCategory::Standards => is_provisioned(&self.standards),
            ManagementCategory::Templates => is_provisioned(&self.templates),
            ManagementCategory::Activity => is_provisioned(&self.operation_log) || is_provisioned(&self.sessions),
        }
    }

    #[must_use]
    pub fn count(&self, category: ManagementCategory) -> usize {
        let records = |listing: &FolderListing| listing.folder().map_or(0, |folder| folder.count_files(&is_record));
        match category {
            ManagementCategory::Decisions => records(&self.decisions),
            ManagementCategory::Registry => self
                .registry
                .iter()
                .filter_map(|(kind, listing)| listing.folder().map(|folder| count_definitions(*kind, folder)))
                .sum(),
            ManagementCategory::Standards => records(&self.standards),
            ManagementCategory::Templates => records(&self.templates),
            ManagementCategory::Activity => self
                .operation_log
                .folder()
                .map_or(0, |folder| folder.count_files(&is_log_day)),
        }
    }

    /// The present categories, in display order, with their counts.
    #[must_use]
    pub fn summaries(&self) -> Vec<CategorySummary> {
        ManagementCategory::ALL
            .into_iter()
            .filter(|category| self.is_present(*category))
            .map(|category| CategorySummary {
                category,
                count: self.count(category),
            })
            .collect()
    }

    #[must_use]
    pub fn registry_listing(&self, kind: RegistryKind) -> &FolderListing {
        self.registry
            .iter()
            .find(|(candidate, _)| *candidate == kind)
            .map_or(&FolderListing::Missing, |(_, listing)| listing)
    }
}

/// A listing that exists, or that exists but could not be read: either way the vault has provisioned something at
/// that location, and the page says what went wrong rather than pretending it is absent.
fn is_provisioned(listing: &FolderListing) -> bool {
    !matches!(listing, FolderListing::Missing)
}

async fn listing(client: &McpClient, vault_name: &str, path: &VaultRelativePath) -> FolderListing {
    Box::pin(list_folder(client, vault_name, path))
        .await
        .unwrap_or(FolderListing::Unreadable {
            code: Some("mcp/unreachable".to_owned()),
        })
}

/// Reads every configured location for `vault_name` concurrently.
pub async fn load_catalogue(client: &McpClient, vault_name: &str) -> Catalogue {
    let loaded = load_vault_management_config(client, vault_name).await;
    let config = &loaded.config;
    let standards = VaultRelativePath::scaffold(STANDARDS_DIR);
    let templates = VaultRelativePath::scaffold(TEMPLATES_DIR);
    let operation_log = VaultRelativePath::scaffold(OPERATION_LOG_DIR);
    let sessions = VaultRelativePath::scaffold(SESSIONS_DIR);
    let registry_dirs = RegistryKind::ALL.map(|kind| config.registry(kind).dir.clone());
    let [agents_dir, hats_dir, prompts_dir, roles_dir, scripts_dir, skills_dir] = &registry_dirs;
    let (decisions, standards, templates, operation_log, sessions) = tokio::join!(
        listing(client, vault_name, &config.register().paths.dir),
        listing(client, vault_name, &standards),
        listing(client, vault_name, &templates),
        listing(client, vault_name, &operation_log),
        listing(client, vault_name, &sessions),
    );
    let (agents, hats, prompts, roles, scripts, skills) = tokio::join!(
        listing(client, vault_name, agents_dir),
        listing(client, vault_name, hats_dir),
        listing(client, vault_name, prompts_dir),
        listing(client, vault_name, roles_dir),
        listing(client, vault_name, scripts_dir),
        listing(client, vault_name, skills_dir),
    );
    let registry = RegistryKind::ALL
        .into_iter()
        .zip([agents, hats, prompts, roles, scripts, skills])
        .collect();
    Catalogue {
        loaded,
        decisions,
        registry,
        standards,
        templates,
        operation_log,
        sessions,
    }
}

#[cfg(test)]
#[path = "vault_catalogue_test.rs"]
mod tests;
