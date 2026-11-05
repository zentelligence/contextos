use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use contextos_core::Clock;

use crate::{Git2Vault, GitError};

/// What is uncommitted in the vault, split by who made the change.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GitUncommitted {
    /// Paths an MCP tool changed that are still uncommitted after the debounce window: staged, recorded as pending,
    /// and never committed. With every mutation arriving through MCP this list is always empty.
    pub overdue_mcp_paths: Vec<String>,
    /// Changed paths no tool recorded: edits made outside MCP (in Obsidian, say). Never overdue, never committed by
    /// the server.
    pub operator_changes: usize,
}

impl<C> Git2Vault<C>
where
    C: Clock,
{
    /// Reports uncommitted changes, telling tool changes that outlived the debounce `window` from operator edits.
    ///
    /// A recorded path counts as overdue only while it still differs from the last commit, so a marker left behind
    /// by a commit made some other way is not a fault.
    ///
    /// # Errors
    ///
    /// Returns a typed repository or pending-state error.
    pub fn uncommitted(&self, window: Duration) -> Result<GitUncommitted, GitError> {
        if !self.is_repository() {
            return Ok(GitUncommitted::default());
        }
        let markers = self.recorded_pending_markers()?;
        let status = self.status()?;
        let dirty = status
            .staged
            .iter()
            .chain(&status.unstaged)
            .chain(&status.untracked)
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let now = std::time::SystemTime::from(self.clock.now());
        let covers = |marker: &Path, changed: &str| {
            let changed = Path::new(changed);
            changed == marker || changed.starts_with(marker)
        };

        // The batch commits once it has been quiet for the window, so it is overdue only when its most recently
        // touched path is older than that: an older path in a batch still receiving changes is inside its rolling
        // window, not late.
        let quiet_for = markers
            .values()
            .max()
            .map(|latest| now.duration_since(*latest).unwrap_or_default());
        let mut overdue = Vec::new();
        if quiet_for.is_some_and(|quiet| quiet > window) {
            for marker in markers.keys() {
                if dirty.iter().any(|changed| covers(marker, changed))
                    && let Some(text) = marker.to_str()
                {
                    overdue.push(text.replace(std::path::MAIN_SEPARATOR, "/"));
                }
            }
        }
        let operator_changes = dirty
            .iter()
            .filter(|changed| !markers.keys().any(|marker| covers(marker, changed)))
            .count();
        Ok(GitUncommitted {
            overdue_mcp_paths: overdue,
            operator_changes,
        })
    }
}
