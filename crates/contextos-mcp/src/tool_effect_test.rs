use super::*;
use crate::ContextOsServer;

#[test]
fn every_shipped_tool_is_classified_exactly_once() {
    let mut shipped = ContextOsServer::catalogue()
        .list_all()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect::<Vec<_>>();
    shipped.sort();
    let classified = TOOL_EFFECTS
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        classified, shipped,
        "classify each new tool in TOOL_EFFECTS, in name order, as read-only or mutating"
    );
}

#[test]
fn the_table_is_in_name_order_without_duplicates() {
    assert!(TOOL_EFFECTS.windows(2).all(|pair| pair[0].0 < pair[1].0));
}

#[test]
fn read_only_and_mutating_names_partition_the_table() {
    let read_only = tool_names(ToolEffect::ReadOnly);
    let mutating = tool_names(ToolEffect::Mutating);
    assert_eq!(read_only.len() + mutating.len(), TOOL_EFFECTS.len());
    assert!(read_only.iter().all(|name| !mutating.contains(name)));
}

#[test]
fn a_tool_that_writes_a_vault_is_never_read_only() {
    let read_only = tool_names(ToolEffect::ReadOnly);
    for name in [
        "fs_write_file",
        "fs_delete_file",
        "note_update",
        "doctor_resolve",
        "git_commit",
        "vault_log_append",
    ] {
        assert!(!read_only.contains(&name), "{name} writes a vault");
    }
    for name in [
        "fs_read_text_file",
        "vault_info",
        "query_text",
        "doctor",
        "ephemeris_moon_phase",
    ] {
        assert!(read_only.contains(&name), "{name} only reads");
    }
}

#[test]
fn a_tool_that_can_move_derived_stores_aside_is_mutating_so_a_read_only_agent_is_not_allowed_it() {
    let read_only = tool_names(ToolEffect::ReadOnly);
    let mutating = tool_names(ToolEffect::Mutating);

    assert!(
        !read_only.contains(&"query_index_rebuild"),
        "it rebuilds and can move stores aside"
    );
    assert!(mutating.contains(&"query_index_rebuild"));
    assert!(read_only.contains(&"doctor"), "doctor only assesses");
    assert!(mutating.contains(&"doctor_resolve"));
    assert!(read_only.contains(&"query_index_status"));
}
