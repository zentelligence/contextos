use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use contextos_core::VaultRootId;
use contextos_fs::FsError;

use crate::{DefinitionName, DefinitionSource, ProjectionError, SkillFilePath, SkillFolder, Slug, SourceVault};

/// A skill's files: each relative path with its bytes.
type Files = Vec<(String, Vec<u8>)>;

#[derive(Debug, Default)]
struct State {
    agents: BTreeMap<(String, String), Vec<u8>>,
    skills: BTreeMap<(String, String), Files>,
    orientation: BTreeMap<String, Vec<u8>>,
    failing: Vec<String>,
}

/// A [`DefinitionSource`] held in memory, for tests of everything that consumes one.
///
/// Vaults are named in configuration order; definitions are keyed by the raw file stem or folder name, so a name that
/// is not a valid slug can be exercised too.
#[derive(Debug)]
pub struct MemorySource {
    vaults: Vec<SourceVault>,
    state: Mutex<State>,
}

impl MemorySource {
    /// A source with the named vaults, in configuration order, and no definitions.
    #[must_use]
    pub fn new(vault_names: &[&str]) -> Self {
        let vaults = vault_names
            .iter()
            .enumerate()
            .filter_map(|(index, name)| {
                Some(SourceVault {
                    id: VaultRootId::try_from(index).ok()?,
                    name: (*name).to_owned(),
                })
            })
            .collect();
        Self {
            vaults,
            state: Mutex::new(State::default()),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets the bytes of `registry/agents/<name>.md` in `vault`.
    pub fn set_agent(&self, vault: &str, name: &str, bytes: &[u8]) {
        self.state()
            .agents
            .insert((vault.to_owned(), name.to_owned()), bytes.to_vec());
    }

    /// Removes `registry/agents/<name>.md` from `vault`.
    pub fn remove_agent(&self, vault: &str, name: &str) {
        self.state().agents.remove(&(vault.to_owned(), name.to_owned()));
    }

    /// Sets the files of the skill folder `<name>` in `vault`, each as a relative path and its bytes.
    pub fn set_skill(&self, vault: &str, name: &str, files: &[(&str, &[u8])]) {
        let files = files
            .iter()
            .map(|(path, bytes)| ((*path).to_owned(), bytes.to_vec()))
            .collect();
        self.state().skills.insert((vault.to_owned(), name.to_owned()), files);
    }

    /// Removes the skill folder `<name>` from `vault`.
    pub fn remove_skill(&self, vault: &str, name: &str) {
        self.state().skills.remove(&(vault.to_owned(), name.to_owned()));
    }

    /// Sets the bytes of `registry/instructions/global.md` in `vault`.
    pub fn set_orientation(&self, vault: &str, bytes: &[u8]) {
        self.state().orientation.insert(vault.to_owned(), bytes.to_vec());
    }

    /// Removes `registry/instructions/global.md` from `vault`.
    pub fn remove_orientation(&self, vault: &str) {
        self.state().orientation.remove(vault);
    }

    /// Makes every read from `vault` fail, as an unreadable vault does.
    pub fn fail_reads_in(&self, vault: &str) {
        self.state().failing.push(vault.to_owned());
    }

    fn check(&self, vault: &SourceVault) -> Result<(), ProjectionError> {
        if self.state().failing.contains(&vault.name) {
            return Err(ProjectionError::SourceRead {
                vault: vault.name.clone(),
                entry: "registry".to_owned(),
                source: Box::new(FsError::ReadContent {
                    path: PathBuf::from("registry"),
                    source: io::Error::other("injected failure"),
                }),
            });
        }
        Ok(())
    }
}

impl DefinitionSource for MemorySource {
    fn vaults(&self) -> Vec<SourceVault> {
        self.vaults.clone()
    }

    fn agent_names(&self, vault: &SourceVault) -> Result<Vec<DefinitionName>, ProjectionError> {
        self.check(vault)?;
        Ok(self
            .state()
            .agents
            .keys()
            .filter(|(owner, _)| *owner == vault.name)
            .map(|(_, name)| DefinitionName::from(name.as_str()))
            .collect())
    }

    fn read_agent(&self, vault: &SourceVault, slug: &Slug) -> Result<Option<Vec<u8>>, ProjectionError> {
        self.check(vault)?;
        Ok(self
            .state()
            .agents
            .get(&(vault.name.clone(), <&str>::from(slug).to_owned()))
            .cloned())
    }

    fn read_orientation(&self, vault: &SourceVault) -> Result<Option<Vec<u8>>, ProjectionError> {
        self.check(vault)?;
        Ok(self.state().orientation.get(&vault.name).cloned())
    }

    fn skill_names(&self, vault: &SourceVault) -> Result<Vec<DefinitionName>, ProjectionError> {
        self.check(vault)?;
        Ok(self
            .state()
            .skills
            .keys()
            .filter(|(owner, _)| *owner == vault.name)
            .map(|(_, name)| DefinitionName::from(name.as_str()))
            .collect())
    }

    fn read_skill(&self, vault: &SourceVault, slug: &Slug) -> Result<Option<SkillFolder>, ProjectionError> {
        self.check(vault)?;
        let files = self
            .state()
            .skills
            .get(&(vault.name.clone(), <&str>::from(slug).to_owned()))
            .cloned();
        files
            .map(|files| {
                files
                    .into_iter()
                    .map(|(path, bytes)| Ok((SkillFilePath::try_from(path.as_str())?, bytes)))
                    .collect::<Result<Vec<_>, ProjectionError>>()
                    .and_then(SkillFolder::try_from)
            })
            .transpose()
    }
}

#[cfg(test)]
#[path = "memory_source_test.rs"]
mod tests;
