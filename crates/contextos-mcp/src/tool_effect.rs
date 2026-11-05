//! The one production classification of every tool by what it does to a vault.
//!
//! Host projection needs the names of every tool that cannot change a vault, so a read-only agent can be allowed
//! exactly those. The contract tests that prove every mutating tool is versioned and every read-only tool leaves a
//! vault untouched take their read-only list from here too, so there is no second list to drift. A tool added without
//! being classified here fails `tool_effect_test.rs`.

/// What a tool does to a vault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolEffect {
    /// Leaves every vault, and its version history, exactly as it was, and never moves or replaces derived state.
    ReadOnly,
    /// May write to a vault, manage its version history, or rebuild derived stores and move a damaged one aside.
    Mutating,
}

/// Every tool the server ships, with its effect, in name order.
const TOOL_EFFECTS: &[(&str, ToolEffect)] = &[
    ("agent_state_change", ToolEffect::Mutating),
    ("base_apply", ToolEffect::Mutating),
    ("base_create", ToolEffect::Mutating),
    ("base_query", ToolEffect::ReadOnly),
    ("base_read", ToolEffect::ReadOnly),
    ("base_record_create", ToolEffect::Mutating),
    ("canvas_apply", ToolEffect::Mutating),
    ("canvas_create", ToolEffect::Mutating),
    ("canvas_read", ToolEffect::ReadOnly),
    ("doctor", ToolEffect::ReadOnly),
    ("doctor_resolve", ToolEffect::Mutating),
    ("ephemeris_boundaries", ToolEffect::ReadOnly),
    ("ephemeris_moon_phase", ToolEffect::ReadOnly),
    ("ephemeris_personal_year_period", ToolEffect::ReadOnly),
    ("ephemeris_solar_events", ToolEffect::ReadOnly),
    ("ephemeris_wheel_of_year", ToolEffect::ReadOnly),
    ("frontmatter_link", ToolEffect::Mutating),
    ("frontmatter_read", ToolEffect::ReadOnly),
    ("frontmatter_update", ToolEffect::Mutating),
    ("fs_attach_file", ToolEffect::ReadOnly),
    ("fs_create_directory", ToolEffect::Mutating),
    ("fs_delete_file", ToolEffect::Mutating),
    ("fs_directory_tree", ToolEffect::ReadOnly),
    ("fs_edit_file", ToolEffect::Mutating),
    ("fs_get_file_info", ToolEffect::ReadOnly),
    ("fs_list_allowed_directories", ToolEffect::ReadOnly),
    ("fs_list_directory", ToolEffect::ReadOnly),
    ("fs_move_file", ToolEffect::Mutating),
    ("fs_read_multiple_files", ToolEffect::ReadOnly),
    ("fs_read_text_file", ToolEffect::ReadOnly),
    ("fs_search_files", ToolEffect::ReadOnly),
    ("fs_write_file", ToolEffect::Mutating),
    ("git_commit", ToolEffect::Mutating),
    ("git_diff", ToolEffect::ReadOnly),
    ("git_init", ToolEffect::Mutating),
    ("git_log", ToolEffect::ReadOnly),
    ("git_restore", ToolEffect::Mutating),
    ("git_status", ToolEffect::ReadOnly),
    ("links_read", ToolEffect::ReadOnly),
    ("mermaid_render", ToolEffect::ReadOnly),
    ("mermaid_validate", ToolEffect::ReadOnly),
    ("note_create", ToolEffect::Mutating),
    ("note_update", ToolEffect::Mutating),
    ("query_blended", ToolEffect::ReadOnly),
    ("query_graph", ToolEffect::ReadOnly),
    ("query_index_rebuild", ToolEffect::Mutating),
    ("query_index_status", ToolEffect::ReadOnly),
    ("query_semantic", ToolEffect::ReadOnly),
    ("query_text", ToolEffect::ReadOnly),
    ("vault_index_rebuild", ToolEffect::Mutating),
    ("vault_info", ToolEffect::ReadOnly),
    ("vault_log_append", ToolEffect::Mutating),
];

/// The names of every tool with `effect`, in name order.
#[must_use]
pub fn tool_names(effect: ToolEffect) -> Vec<&'static str> {
    TOOL_EFFECTS
        .iter()
        .filter(|(_, candidate)| *candidate == effect)
        .map(|(name, _)| *name)
        .collect()
}

#[cfg(test)]
#[path = "tool_effect_test.rs"]
mod tests;
