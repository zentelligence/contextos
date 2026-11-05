use super::*;

#[test]
fn a_plain_nested_path_is_accepted_unchanged() -> Result<(), VaultRelativePathError> {
    let path = VaultRelativePath::try_from("memory/decisions.base")?;
    assert_eq!(path.as_str(), "memory/decisions.base");
    Ok(())
}

#[test]
fn surrounding_whitespace_and_one_trailing_slash_are_dropped() -> Result<(), VaultRelativePathError> {
    let path = VaultRelativePath::try_from("  memory/decisions/ ")?;
    assert_eq!(path.as_str(), "memory/decisions");
    Ok(())
}

#[test]
fn every_unsafe_or_ambiguous_form_is_rejected_with_its_own_reason() {
    let cases = [
        ("", VaultRelativePathError::Empty),
        ("   ", VaultRelativePathError::Empty),
        ("/memory/decisions", VaultRelativePathError::Absolute),
        ("memory\\decisions", VaultRelativePathError::Backslash),
        ("C:/vault/memory", VaultRelativePathError::Colon),
        ("memory/notes.md:stream", VaultRelativePathError::Colon),
        ("memory/../secrets", VaultRelativePathError::InvalidSegment),
        ("../outside", VaultRelativePathError::InvalidSegment),
        ("memory/./decisions", VaultRelativePathError::InvalidSegment),
        ("memory//decisions", VaultRelativePathError::InvalidSegment),
        ("memory/decisions//", VaultRelativePathError::InvalidSegment),
    ];
    for (raw, expected) in cases {
        assert_eq!(VaultRelativePath::try_from(raw), Err(expected), "{raw:?}");
    }
}

#[test]
fn every_rejection_carries_a_distinct_stable_code() {
    let codes = [
        VaultRelativePathError::Empty.code(),
        VaultRelativePathError::Absolute.code(),
        VaultRelativePathError::Backslash.code(),
        VaultRelativePathError::Colon.code(),
        VaultRelativePathError::InvalidSegment.code(),
    ];
    let unique: std::collections::HashSet<_> = codes.iter().collect();
    assert_eq!(unique.len(), codes.len());
}

#[test]
fn file_name_and_parent_split_at_the_last_separator() -> Result<(), VaultRelativePathError> {
    let path = VaultRelativePath::try_from("registry/prompts/writing/email.md")?;
    assert_eq!(path.file_name(), "email.md");
    assert_eq!(
        path.parent().map(|parent| parent.as_str().to_owned()),
        Some("registry/prompts/writing".to_owned())
    );
    assert_eq!(VaultRelativePath::try_from("standards")?.parent(), None);
    Ok(())
}

#[test]
fn strip_ancestor_requires_a_whole_folder_boundary() -> Result<(), VaultRelativePathError> {
    let root = VaultRelativePath::try_from("memory/decisions")?;
    let record = VaultRelativePath::try_from("memory/decisions/vd-001.md")?;
    let lookalike = VaultRelativePath::try_from("memory/decisions-old/vd-001.md")?;
    assert_eq!(record.strip_ancestor(&root), Some("vd-001.md"));
    assert_eq!(lookalike.strip_ancestor(&root), None);
    assert_eq!(root.strip_ancestor(&root), None);
    Ok(())
}

#[test]
fn join_places_a_child_beneath_its_parent() -> Result<(), VaultRelativePathError> {
    let joined =
        VaultRelativePath::try_from("registry/skills")?.join(&VaultRelativePath::try_from("plan-day/SKILL.md")?);
    assert_eq!(joined.as_str(), "registry/skills/plan-day/SKILL.md");
    Ok(())
}
