use super::*;

#[test]
fn serialised_codes_are_snake_case_and_stable() -> Result<(), serde_json::Error> {
    let codes = [
        (DoctorKind::Configuration, "configuration"),
        (DoctorKind::VaultAccess, "vault_access"),
        (DoctorKind::ManagedIndexes, "managed_indexes"),
        (DoctorKind::GitRecovery, "git_recovery"),
        (DoctorKind::UncommittedChanges, "uncommitted_changes"),
        (DoctorKind::SemanticSearch, "semantic_search"),
        (DoctorKind::Frontmatter, "frontmatter"),
        (DoctorKind::DuplicateIds, "duplicate_ids"),
        (DoctorKind::RecordConformance, "record_conformance"),
        (DoctorKind::TeamConfig, "team_config"),
        (DoctorKind::FenceVocabulary, "fence_vocabulary"),
        (DoctorKind::TextIndexStore, "text_index_store"),
        (DoctorKind::LinkGraphStore, "link_graph_store"),
        (DoctorKind::VectorStore, "vector_store"),
        (DoctorKind::ProjectionStale, "projection_stale"),
        (DoctorKind::ProjectionHandEdited, "projection_hand_edited"),
        (DoctorKind::ProjectionOrphaned, "projection_orphaned"),
        (DoctorKind::ProjectionInvalidDefinition, "projection_invalid_definition"),
        (DoctorKind::ProjectionShadowed, "projection_shadowed"),
        (DoctorKind::ProjectionUnownedCollision, "projection_unowned_collision"),
        (DoctorKind::ProjectionForeignOwner, "projection_foreign_owner"),
        (DoctorKind::OrientationBlock, "orientation_block"),
        (DoctorKind::CodexReadOnlyUnenforced, "codex_read_only_unenforced"),
        (DoctorKind::CodexOverrideHidesAgents, "codex_override_hides_agents"),
        (DoctorKind::EventKindInvalid, "event_kind_invalid"),
        (DoctorKind::EventKindDuplicate, "event_kind_duplicate"),
        (DoctorKind::EventUnreadable, "event_unreadable"),
        (DoctorKind::EventKindTruncated, "event_kind_truncated"),
        (DoctorKind::EventKindMisplaced, "event_kind_misplaced"),
    ];
    for (kind, code) in codes {
        assert_eq!(serde_json::to_value(kind)?, serde_json::Value::from(code));
    }
    Ok(())
}

#[test]
fn subjects_match_the_labels_the_cli_has_always_printed() {
    assert_eq!(DoctorKind::ManagedIndexes.to_string(), "Managed indexes");
    assert_eq!(DoctorKind::UncommittedChanges.to_string(), "Uncommitted MCP changes");
    assert_eq!(DoctorKind::DuplicateIds.to_string(), "Duplicate id detection");
    assert_eq!(DoctorKind::Frontmatter.to_string(), "Frontmatter validity");
    assert_eq!(DoctorKind::RecordConformance.to_string(), "Record conformance");
}

#[test]
fn every_projection_finding_kind_serialises_as_the_code_the_projection_crate_spells() -> Result<(), serde_json::Error> {
    use FindingKind::*;
    for kind in [
        ProjectionStale,
        ProjectionHandEdited,
        ProjectionOrphaned,
        ProjectionInvalidDefinition,
        ProjectionShadowed,
        ProjectionUnownedCollision,
        ProjectionForeignOwner,
        CodexReadOnlyUnenforced,
        OrientationBlock,
        CodexOverrideHidesAgents,
    ] {
        let code: &str = kind.into();
        assert_eq!(
            serde_json::to_value(DoctorKind::from(kind))?,
            serde_json::Value::from(code)
        );
    }
    Ok(())
}

#[test]
fn the_unassessed_projection_kinds_serialise_as_stable_snake_case_codes() -> Result<(), serde_json::Error> {
    assert_eq!(
        serde_json::to_value(DoctorKind::ProjectionBusy)?,
        serde_json::Value::from("projection_busy")
    );
    assert_eq!(
        serde_json::to_value(DoctorKind::ProjectionLockFailed)?,
        serde_json::Value::from("projection_lock_failed")
    );
    Ok(())
}
