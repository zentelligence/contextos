use super::*;

fn tier_rejection(value: &str) -> Option<DefinitionRejection> {
    match Tier::try_from(value) {
        Err(ProjectionError::InvalidDefinition { field: "tier", reason }) => Some(reason),
        _ => None,
    }
}

fn access_rejection(value: &str) -> Option<DefinitionRejection> {
    match Access::try_from(value) {
        Err(ProjectionError::InvalidDefinition {
            field: "access",
            reason,
        }) => Some(reason),
        _ => None,
    }
}

#[test]
fn each_documented_tier_name_is_accepted() -> Result<(), ProjectionError> {
    assert_eq!(Tier::try_from("fast")?, Tier::Fast);
    assert_eq!(Tier::try_from("balanced")?, Tier::Balanced);
    assert_eq!(Tier::try_from("deep")?, Tier::Deep);
    Ok(())
}

#[test]
fn a_vendor_model_name_or_a_variant_spelling_is_not_a_tier() {
    for value in [
        "sonnet", "opus", "haiku", "gpt-5", "Fast", "DEEP", " fast", "fast ", "", "inherit",
    ] {
        assert_eq!(
            tier_rejection(value),
            Some(DefinitionRejection::Unrecognised),
            "{value:?}"
        );
    }
}

#[test]
fn each_documented_access_is_accepted() -> Result<(), ProjectionError> {
    assert_eq!(Access::try_from("all")?, Access::All);
    assert_eq!(Access::try_from("read-only")?, Access::ReadOnly);
    Ok(())
}

#[test]
fn any_other_access_is_refused() {
    for value in ["readonly", "read_only", "Read-Only", "ALL", "Read, Grep", "", "none"] {
        assert_eq!(
            access_rejection(value),
            Some(DefinitionRejection::Unrecognised),
            "{value:?}"
        );
    }
}
