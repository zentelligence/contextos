//! Shared fixture: real temporary vaults, host folders, and state root around a [`ProjectionService`].

// Each test target compiles this module and uses a different subset of it, so unused items are expected.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use contextos_core::{
    OpKind, OperationEvent, OperationWarning, Origin, ProjectsDefinitions, VaultPath, VaultPathInput, VaultRoot,
    VaultRootInput, VaultSet,
};
use contextos_fs::{Filesystem, FilesystemConfig, FsLimits};
use contextos_projection::{
    ClaudeTiers, CodexTiers, ConfigurationId, HostFolder, HostProjection, HostRender, InstructionsFile,
    ManifestLocation, OwnershipManifest, ProjectionService, ProjectionTarget, ReadOnlyTools, Slug, VaultSetSource,
};
use tempfile::{TempDir, tempdir};
use time::OffsetDateTime;

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

pub struct Hosts {
    pub claude_agents: TempDir,
    pub claude_skills: TempDir,
    pub codex_agents: TempDir,
    pub codex_skills: TempDir,
    pub state: TempDir,
}

pub struct World {
    pub vaults: Vec<TempDir>,
    pub roots: VaultSet,
    pub hosts: Hosts,
}

pub const VAULT_NAMES: [&str; 3] = ["alpha", "beta", "gamma"];

pub fn world(vault_count: usize) -> Result<World, Box<dyn std::error::Error>> {
    let mut vaults = Vec::new();
    let mut roots = Vec::new();
    for name in VAULT_NAMES.iter().take(vault_count) {
        let dir = tempdir()?;
        roots.push(VaultRoot::try_from(VaultRootInput {
            path: dir.path().to_path_buf(),
            managed: false,
            name: Some((*name).to_owned()),
        })?);
        vaults.push(dir);
    }
    Ok(World {
        vaults,
        roots: VaultSet::try_from(roots)?,
        hosts: Hosts {
            claude_agents: tempdir()?,
            claude_skills: tempdir()?,
            codex_agents: tempdir()?,
            codex_skills: tempdir()?,
            state: tempdir()?,
        },
    })
}

fn folder(root: &Path, state: &Path) -> Result<HostFolder, Box<dyn std::error::Error>> {
    let target = ProjectionTarget::try_from(root)?;
    let manifest = ManifestLocation::for_target(state, &target)?;
    Ok(HostFolder::new(target, manifest))
}

impl World {
    pub fn service(&self, configuration: &str) -> Result<ProjectionService, Box<dyn std::error::Error>> {
        self.service_over(&self.roots, configuration)
    }

    /// A service reading `roots` (which may be a different set from the one the vault folders were made for).
    pub fn service_over(
        &self,
        roots: &VaultSet,
        configuration: &str,
    ) -> Result<ProjectionService, Box<dyn std::error::Error>> {
        self.build_service(roots, configuration, None)
    }

    /// A service that also keeps the orientation block of the given Claude and Codex instructions files.
    pub fn service_with_instructions(
        &self,
        configuration: &str,
        claude_instructions: &Path,
        codex_instructions: &Path,
    ) -> Result<ProjectionService, Box<dyn std::error::Error>> {
        self.build_service(
            &self.roots,
            configuration,
            Some((claude_instructions, codex_instructions)),
        )
    }

    /// As [`Self::service_with_instructions`], reading `roots`.
    pub fn service_with_instructions_over(
        &self,
        roots: &VaultSet,
        configuration: &str,
        claude_instructions: &Path,
        codex_instructions: &Path,
    ) -> Result<ProjectionService, Box<dyn std::error::Error>> {
        self.build_service(roots, configuration, Some((claude_instructions, codex_instructions)))
    }

    fn build_service(
        &self,
        roots: &VaultSet,
        configuration: &str,
        instructions: Option<(&Path, &Path)>,
    ) -> Result<ProjectionService, Box<dyn std::error::Error>> {
        let count = roots.len();
        let filesystem = Filesystem::try_from(FilesystemConfig {
            roots: roots.clone(),
            limits: vec![
                FsLimits {
                    max_read_bytes: 1 << 20,
                    max_batch_files: 16,
                };
                count
            ],
            hidden: vec![Vec::new(); count],
            atomic_write_guard: None,
        })?;
        let state = self.hosts.state.path();
        let claude = HostProjection::new(
            folder(self.hosts.claude_agents.path(), state)?,
            folder(self.hosts.claude_skills.path(), state)?,
            HostRender::Claude {
                tiers: ClaudeTiers::default(),
                read_only_tools: ReadOnlyTools::default(),
            },
        );
        let codex = HostProjection::new(
            folder(self.hosts.codex_agents.path(), state)?,
            folder(self.hosts.codex_skills.path(), state)?,
            HostRender::Codex {
                tiers: CodexTiers::default(),
            },
        );
        let (claude, codex) = match instructions {
            Some((claude_file, codex_file)) => (
                claude.with_instructions(InstructionsFile::try_from(claude_file)?),
                codex.with_instructions(InstructionsFile::try_from(codex_file)?),
            ),
            None => (claude, codex),
        };
        Ok(ProjectionService::new(
            Box::new(VaultSetSource::new(roots.clone(), filesystem)),
            vec![claude, codex],
            ConfigurationId::try_from(configuration)?,
        ))
    }

    /// A vault set with no vaults, for the zero-vault start.
    pub fn no_roots() -> Result<VaultSet, Box<dyn std::error::Error>> {
        Ok(VaultSet::try_from(Vec::<VaultRoot>::new())?)
    }

    pub fn vault_file(&self, vault: usize, relative: &str) -> PathBuf {
        self.vaults[vault].path().join(relative)
    }

    pub fn write_vault(&self, vault: usize, relative: &str, content: &str) -> std::io::Result<()> {
        let path = self.vault_file(vault, relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)
    }

    pub fn remove_vault(&self, vault: usize, relative: &str) -> std::io::Result<()> {
        let path = self.vault_file(vault, relative);
        if path.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    }

    pub fn event(&self, kind: OpKind, paths: &[(usize, &str)]) -> Result<OperationEvent, Box<dyn std::error::Error>> {
        let mut resolved = Vec::new();
        for (vault, relative) in paths {
            resolved.push(VaultPath::try_from(VaultPathInput {
                roots: &self.roots,
                raw: &format!("{}://{relative}", VAULT_NAMES[*vault]),
            })?);
        }
        Ok(OperationEvent {
            kind,
            paths: resolved,
            origin: Origin::Tool("test".to_owned()),
            summary: "test event".to_owned(),
            at: OffsetDateTime::UNIX_EPOCH,
        })
    }

    pub fn project(
        &self,
        service: &ProjectionService,
        kind: OpKind,
        paths: &[(usize, &str)],
    ) -> Result<Vec<OperationWarning>, Box<dyn std::error::Error>> {
        Ok(service.project(&self.event(kind, paths)?))
    }

    pub fn claude_agent(&self, slug: &str) -> PathBuf {
        self.hosts.claude_agents.path().join(format!("{slug}.md"))
    }

    pub fn codex_agent(&self, slug: &str) -> PathBuf {
        self.hosts.codex_agents.path().join(format!("{slug}.toml"))
    }

    pub fn claude_skill(&self, slug: &str) -> PathBuf {
        self.hosts.claude_skills.path().join(slug)
    }

    pub fn codex_skill(&self, slug: &str) -> PathBuf {
        self.hosts.codex_skills.path().join(slug)
    }

    /// The manifest of the Claude agents folder.
    pub fn claude_agent_manifest(&self) -> Result<OwnershipManifest, Box<dyn std::error::Error>> {
        let target = ProjectionTarget::try_from(self.hosts.claude_agents.path())?;
        let location = ManifestLocation::for_target(self.hosts.state.path(), &target)?;
        Ok(OwnershipManifest::read(&location, &target)?)
    }

    /// The vault that the Claude agents manifest records `slug` as projected from.
    pub fn recorded_vault(&self, slug: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let manifest = self.claude_agent_manifest()?;
        Ok(manifest
            .get(&Slug::try_from(slug)?)
            .map(|entry| entry.source_vault().to_owned()))
    }
}

pub fn agent(description: &str, tier: &str, access: &str, body: &str) -> String {
    format!("---\ndescription: {description}\ntier: {tier}\naccess: {access}\n---\n\n{body}\n")
}

pub fn reviewer(body: &str) -> String {
    agent("Reviews code", "balanced", "all", body)
}

pub const REVIEWER: &str = "registry/agents/reviewer.md";

pub fn skill(name: &str) -> String {
    format!("---\nname: {name}\ndescription: Teaches {name}\n---\n\nHow to {name}.\n")
}

pub fn codes(warnings: &[OperationWarning]) -> Vec<&str> {
    warnings.iter().map(|warning| warning.code.as_str()).collect()
}
