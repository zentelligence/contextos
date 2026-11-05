use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

use contextos_core::{OperationEvent, OperationWarning, ProjectionScope, ProjectsDefinitions};
use contextos_fs::host_file::FolderReplaceObserver;

use crate::InstructionsObserver;
use crate::report::invalid_name_warning;
use crate::run::{Run, Scope};
use crate::{
    ConfigurationId, DefinitionKind, DefinitionName, DefinitionSource, HostProjection, Mode, ProjectionError,
    ProjectionReport, Slug,
};

/// Keeps every enabled agent host's agent and skill folders in step with the vaults' registry definitions.
///
/// One service per configuration. It performs no locking: the caller serialises runs that touch the same hosts.
pub struct ProjectionService {
    source: Box<dyn DefinitionSource>,
    hosts: Vec<HostProjection>,
    configuration: ConfigurationId,
    observer: Option<Arc<dyn InstructionsObserver>>,
    /// Behind a lock so a test can attach it to a service that is already shared.
    folders: RwLock<Option<Arc<dyn FolderReplaceObserver + Send + Sync>>>,
}

impl ProjectionService {
    /// A service over `source` that projects into `hosts`, recording `configuration` as the owner of what it writes.
    #[must_use]
    pub fn new(source: Box<dyn DefinitionSource>, hosts: Vec<HostProjection>, configuration: ConfigurationId) -> Self {
        Self {
            source,
            hosts,
            configuration,
            observer: None,
            folders: RwLock::new(None),
        }
    }

    /// The same service, reporting each step of an instructions file update to `observer`.
    #[must_use]
    pub fn with_instructions_observer(self, observer: Arc<dyn InstructionsObserver>) -> Self {
        Self {
            observer: Some(observer),
            ..self
        }
    }

    /// The same service, reporting each step of a skill folder swap to `folders`, so a test can interrupt a swap at an
    /// exact point.
    #[must_use]
    pub fn with_folder_observer(self, folders: Arc<dyn FolderReplaceObserver + Send + Sync>) -> Self {
        Self {
            folders: RwLock::new(Some(folders)),
            ..self
        }
    }

    /// As [`Self::with_folder_observer`], for a service already shared between owners.
    pub fn set_folder_observer(&self, folders: Arc<dyn FolderReplaceObserver + Send + Sync>) {
        if let Ok(mut current) = self.folders.write() {
            *current = Some(folders);
        }
    }

    fn run(&self, mode: Mode, scope: Scope) -> ProjectionReport {
        let mut run = Run::new(self.source.as_ref(), &self.hosts, &self.configuration, mode);
        let folders = self.folders.read().ok().and_then(|folders| folders.clone());
        if let Some(folders) = &folders {
            run = run.with_folder_observer(folders.as_ref());
        }
        match &self.observer {
            Some(observer) => run.observed_by(observer.as_ref()).execute(scope),
            None => run.execute(scope),
        }
    }

    /// Reconciles every definition with every host, as at server start, and returns every warning.
    ///
    /// A hand-edited entry whose source has not changed is left for `doctor`, and nothing is withdrawn when no vault
    /// is configured.
    #[must_use]
    pub fn refresh(&self) -> Vec<OperationWarning> {
        self.run(Mode::Refresh, Scope::All).warnings
    }

    /// Reports what `doctor` should say about every definition, and changes nothing.
    #[must_use]
    pub fn assess(&self) -> ProjectionReport {
        self.run(Mode::Assess, Scope::All)
    }

    /// Repairs every auto-fixable finding, replacing hand edits and withdrawing orphans, and reports every finding the
    /// pass met, repaired or not.
    #[must_use]
    pub fn resolve(&self) -> ProjectionReport {
        self.run(Mode::Resolve, Scope::All)
    }

    /// Whether `name` is a folder directly in the skills folder of the vault `event_path` belongs to, which is the
    /// only thing a full refresh treats as a skill: a plain file there is ignored.
    fn is_skill_folder(&self, event_path: &contextos_core::VaultPath, name: &str) -> bool {
        let Some(vault) = self
            .source
            .vaults()
            .into_iter()
            .find(|vault| vault.id == event_path.root_id())
        else {
            return false;
        };
        self.source.skill_names(&vault).is_ok_and(|names| {
            names
                .iter()
                .any(|found| matches!(found, DefinitionName::Invalid { raw, .. } if raw == name))
        })
    }

    fn vault_name(&self, event_path: &contextos_core::VaultPath) -> String {
        self.source
            .vaults()
            .into_iter()
            .find(|vault| vault.id == event_path.root_id())
            .map_or_else(|| "?".to_owned(), |vault| vault.name)
    }
}

impl std::fmt::Debug for ProjectionService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectionService")
            .field("hosts", &self.hosts.len())
            .field("configuration", &self.configuration)
            .finish_non_exhaustive()
    }
}

impl ProjectsDefinitions for ProjectionService {
    fn project(&self, event: &OperationEvent) -> Vec<OperationWarning> {
        let mut agents = BTreeSet::new();
        let mut skills = BTreeSet::new();
        let mut everything = false;
        let mut orientation = false;
        let mut warnings = Vec::new();
        for path in &event.paths {
            let (kind, name, set) = match ProjectionScope::try_from(path) {
                Ok(ProjectionScope::Agent(name)) => (DefinitionKind::Agent, name, &mut agents),
                Ok(ProjectionScope::Skill(name)) => (DefinitionKind::Skill, name, &mut skills),
                Ok(ProjectionScope::AllDefinitions) => {
                    everything = true;
                    continue;
                }
                Ok(ProjectionScope::Orientation) => {
                    orientation = true;
                    continue;
                }
                Err(_) => continue,
            };
            match Slug::try_from(name.as_str()) {
                Ok(slug) => {
                    set.insert(slug);
                }
                Err(ProjectionError::InvalidSlug { reason }) => {
                    if kind == DefinitionKind::Skill && !self.is_skill_folder(path, &name) {
                        continue;
                    }
                    warnings.push(invalid_name_warning(kind, &self.vault_name(path), &name, reason));
                }
                Err(_) => {}
            }
        }
        let scope = if everything {
            Scope::All
        } else {
            Scope::Slugs {
                agents,
                skills,
                orientation,
            }
        };
        warnings.extend(self.run(Mode::Event, scope).warnings);
        warnings
    }
}
