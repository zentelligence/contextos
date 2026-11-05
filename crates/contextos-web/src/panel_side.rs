//! Which side of a split screen its own navigation panel (a folder tree, a
//! shard picker) renders on - shared by every Knowledge screen's own config
//! ([`crate::wiki_config`], [`crate::insights_config`], [`crate::entity_docs_config`]).
//! Deliberately distinct from [`crate::date_structure::DateStructureSide`]:
//! that type names which side a *dated-versus-regular* split puts its dated
//! pane on, a different axis from "which side does the navigation panel
//! itself sit on" that would only invite confusion if the two were merged.

use serde::{Deserialize, Serialize};

/// Which side of a split screen its own navigation panel renders on.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelSide {
    Left,
    #[default]
    Right,
}

#[cfg(test)]
#[path = "panel_side_test.rs"]
mod tests;
