use contextos_core::{Clock, ContentHash, Origin, WriteMutation, WritesVault};
use git2::{IndexAddOption, Repository, RepositoryInitOptions};
use sha2::{Digest, Sha256};

use contextos_core::{OperationEvent, VaultPath, VaultPathInput};

use crate::{Git2Vault, GitError, GitWriteError, PendingBatch, open_repository};

const DERIVED_IGNORE: &str = ".contextos/";
const OBSIDIAN_WORKSPACE_IGNORE: &str = ".obsidian/workspace*";

/// Outcome of initialising local recovery history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitInitResult {
    pub initialised: bool,
    pub commit_id: Option<String>,
}

impl<C> Git2Vault<C>
where
    C: Clock,
{
    /// Initialises a repository, maintains ignore policy, and commits vault state.
    ///
    /// # Errors
    ///
    /// Returns a typed path, write, or libgit2 error when initialisation cannot
    /// establish a recoverable initial state.
    pub fn initialise<W>(&self, writer: &W) -> Result<GitInitResult, GitWriteError<W::Error>>
    where
        W: WritesVault,
        W::Error: std::error::Error + 'static,
    {
        if self.root.path().join(".git").exists() {
            if let Some(event) = self.maintain_ignore_policy(writer)? {
                self.stage_event(&event)?;
            }
            let repository = open_repository(self.root.path())?;
            let commit_id = repository
                .head()
                .ok()
                .and_then(|head| head.target())
                .map(|id| id.to_string());
            if commit_id.is_none() {
                repository.set_head("refs/heads/main").map_err(GitError::Repository)?;
                let mut index = repository.index().map_err(GitError::Repository)?;
                index
                    .add_all(["*"], IndexAddOption::DEFAULT, None)
                    .map_err(GitError::Repository)?;
                index.write().map_err(GitError::Repository)?;
                let tree_id = index.write_tree().map_err(GitError::Repository)?;
                let tree = repository.find_tree(tree_id).map_err(GitError::Repository)?;
                let signature = self.signature()?;
                let id = repository
                    .commit(
                        Some("HEAD"),
                        &signature,
                        &signature,
                        "mcp: initialise ContextOS vault",
                        &tree,
                        &[],
                    )
                    .map_err(GitError::Repository)?;
                let recorded = self.recorded_pending_paths()?;
                self.clear_pending_paths(&recorded.into_iter().collect::<Vec<_>>())?;
                *self.pending.lock().map_err(|_| GitError::PendingStateUnavailable)? = PendingBatch::default();
                return Ok(GitInitResult {
                    initialised: true,
                    commit_id: Some(id.to_string()),
                });
            }
            return Ok(GitInitResult {
                initialised: false,
                commit_id,
            });
        }

        let _ignore_event = self.maintain_ignore_policy(writer)?;
        let mut options = RepositoryInitOptions::new();
        options.initial_head("main");
        let repository = Repository::init_opts(self.root.path(), &options).map_err(GitError::Repository)?;
        let mut index = repository.index().map_err(GitError::Repository)?;
        index
            .add_all(["*"], IndexAddOption::DEFAULT, None)
            .map_err(GitError::Repository)?;
        index.write().map_err(GitError::Repository)?;
        let tree_id = index.write_tree().map_err(GitError::Repository)?;
        let tree = repository.find_tree(tree_id).map_err(GitError::Repository)?;
        let signature = self.signature()?;
        let commit_id = repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "mcp: initialise ContextOS vault",
                &tree,
                &[],
            )
            .map_err(GitError::Repository)?;

        Ok(GitInitResult {
            initialised: true,
            commit_id: Some(commit_id.to_string()),
        })
    }

    fn maintain_ignore_policy<W>(&self, writer: &W) -> Result<Option<OperationEvent>, GitWriteError<W::Error>>
    where
        W: WritesVault,
        W::Error: std::error::Error + 'static,
    {
        let target = self.root.path().join(".gitignore");
        let (mut content, expected_hash) = match contextos_fs::read_regular_file(&target) {
            Ok(bytes) => {
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                let hash = ContentHash::from(<[u8; 32]>::from(hasher.finalize()));
                let content = String::from_utf8(bytes).map_err(|source| GitError::NonUtf8Ignore {
                    path: target.clone(),
                    source,
                })?;
                (content, Some(hash))
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => (String::new(), None),
            Err(source) => {
                return Err(GitError::ReadIgnore { path: target, source }.into());
            }
        };
        let mut changed = false;
        for required in [DERIVED_IGNORE, OBSIDIAN_WORKSPACE_IGNORE] {
            if !content.lines().any(|line| line == required) {
                if !content.is_empty() && !content.ends_with('\n') {
                    content.push('\n');
                }
                content.push_str(required);
                content.push('\n');
                changed = true;
            }
        }
        if !changed {
            return Ok(None);
        }

        let raw = target
            .to_str()
            .ok_or_else(|| GitError::NonUtf8Path { path: target.clone() })?;
        let path = VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw,
        })
        .map_err(GitError::Path)?;
        let result = writer
            .persist(&WriteMutation {
                path,
                content: content.into(),
                expected_hash,
                force: false,
                origin: Origin::Internal("git".to_owned()),
            })
            .map_err(GitWriteError::Write)?;
        Ok(result.event)
    }
}
