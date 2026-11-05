use super::*;

fn model_rejection(value: &str) -> Option<NameRejection> {
    match ModelName::try_from(value) {
        Err(ProjectionError::InvalidHostSetting { field: "model", reason }) => Some(reason),
        _ => None,
    }
}

fn model(value: &str) -> Result<ModelName, ProjectionError> {
    ModelName::try_from(value)
}

#[test]
fn a_model_name_must_be_non_empty_without_whitespace_or_control_characters() -> Result<(), ProjectionError> {
    assert_eq!(<&str>::from(&model("claude-sonnet-4-5")?), "claude-sonnet-4-5");
    assert_eq!(model_rejection(""), Some(NameRejection::Empty));
    assert_eq!(model_rejection("a b"), Some(NameRejection::InvalidCharacter));
    assert_eq!(model_rejection(" a"), Some(NameRejection::InvalidCharacter));
    assert_eq!(model_rejection("a\nb"), Some(NameRejection::ControlCharacter));
    assert_eq!(model_rejection("a\u{0}"), Some(NameRejection::ControlCharacter));
    Ok(())
}

#[test]
fn the_claude_defaults_are_haiku_sonnet_and_opus() {
    let tiers = ClaudeTiers::default();
    assert_eq!(<&str>::from(tiers.model(Tier::Fast)), "haiku");
    assert_eq!(<&str>::from(tiers.model(Tier::Balanced)), "sonnet");
    assert_eq!(<&str>::from(tiers.model(Tier::Deep)), "opus");
}

#[test]
fn claude_tiers_resolve_each_tier_to_its_own_model() -> Result<(), ProjectionError> {
    let tiers = ClaudeTiers::new(model("a")?, model("b")?, model("c")?);
    assert_eq!(<&str>::from(tiers.model(Tier::Fast)), "a");
    assert_eq!(<&str>::from(tiers.model(Tier::Balanced)), "b");
    assert_eq!(<&str>::from(tiers.model(Tier::Deep)), "c");
    Ok(())
}

#[test]
fn each_reasoning_effort_round_trips_through_its_name() -> Result<(), ProjectionError> {
    for name in ["minimal", "low", "medium", "high", "xhigh", "max"] {
        assert_eq!(<&str>::from(ReasoningEffort::try_from(name)?), name);
    }
    Ok(())
}

#[test]
fn an_unknown_reasoning_effort_is_refused() {
    for name in ["", "ultra", "none", "High", "persistent"] {
        assert!(
            matches!(
                ReasoningEffort::try_from(name),
                Err(ProjectionError::InvalidHostSetting {
                    field: "reasoning effort",
                    reason: NameRejection::Unrecognised,
                })
            ),
            "{name:?}"
        );
    }
}

#[test]
fn the_codex_defaults_are_low_medium_and_high_with_no_model() {
    let tiers = CodexTiers::default();
    for (tier, effort) in [
        (Tier::Fast, ReasoningEffort::Low),
        (Tier::Balanced, ReasoningEffort::Medium),
        (Tier::Deep, ReasoningEffort::High),
    ] {
        assert_eq!(tiers.tier(tier).effort(), effort);
        assert_eq!(tiers.tier(tier).model(), None);
    }
}

#[test]
fn codex_tiers_resolve_each_tier_to_its_own_mapping() -> Result<(), ProjectionError> {
    let fast = CodexTier::new(Some(model("m1")?), ReasoningEffort::Minimal);
    let balanced = CodexTier::new(None, ReasoningEffort::Xhigh);
    let deep = CodexTier::new(Some(model("m3")?), ReasoningEffort::Max);
    let tiers = CodexTiers::new(fast.clone(), balanced.clone(), deep.clone());
    assert_eq!(tiers.tier(Tier::Fast), &fast);
    assert_eq!(tiers.tier(Tier::Balanced), &balanced);
    assert_eq!(tiers.tier(Tier::Deep), &deep);
    Ok(())
}

#[test]
fn read_only_tools_default_to_the_contextos_server_with_no_tools() {
    let tools = ReadOnlyTools::default();
    assert_eq!(tools.server(), "contextos");
    assert_eq!(tools.allowlist(), Vec::<String>::new());
}

#[test]
fn the_allowlist_is_sorted_deduplicated_and_prefixed_with_the_server() -> Result<(), ProjectionError> {
    let tools = ReadOnlyTools::try_from((
        "ctx-os",
        ["fs_read_text_file", "base_query", "fs_read_text_file"].as_slice(),
    ))?;
    assert_eq!(tools.server(), "ctx-os");
    assert_eq!(
        tools.allowlist(),
        vec![
            "mcp__ctx-os__base_query".to_owned(),
            "mcp__ctx-os__fs_read_text_file".to_owned()
        ]
    );
    Ok(())
}

#[test]
fn server_and_tool_names_are_limited_to_lowercase_letters_digits_underscores_and_hyphens() {
    let rejection = |server: &str, tool: &str| match ReadOnlyTools::try_from((server, [tool].as_slice())) {
        Err(ProjectionError::InvalidHostSetting { field, reason }) => Some((field, reason)),
        _ => None,
    };
    assert_eq!(rejection("", "ok"), Some(("server", NameRejection::Empty)));
    assert_eq!(
        rejection("Bad", "ok"),
        Some(("server", NameRejection::InvalidCharacter))
    );
    assert_eq!(
        rejection("a b", "ok"),
        Some(("server", NameRejection::InvalidCharacter))
    );
    assert_eq!(rejection("ok", ""), Some(("tool", NameRejection::Empty)));
    assert_eq!(rejection("ok", "a,b"), Some(("tool", NameRejection::InvalidCharacter)));
    assert_eq!(rejection("ok", "Read"), Some(("tool", NameRejection::InvalidCharacter)));
    assert_eq!(rejection("ok", "a\nb"), Some(("tool", NameRejection::InvalidCharacter)));
}
