use time::macros::datetime;

use super::*;

type BoxError = Box<dyn std::error::Error>;

fn sample_file() -> Result<TeamFile, BoxError> {
    Ok(toml::from_str(
        "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\ndomain = \"Finance\"\n\n[[member]]\nslug = \"vela\"\nname = \"Vela\"\n",
    )?)
}

#[test]
fn apply_state_change_updates_the_matching_agent_and_reports_no_previous_state() -> Result<(), BoxError> {
    let mut file = sample_file()?;
    let at = datetime!(2026-09-11 02:15:30 UTC);
    let outcome = apply_state_change(&mut file, "atlas", AgentStateValue::Working, at)?;
    assert_eq!(outcome.previous_state, None);
    let atlas = file
        .member
        .iter()
        .find(|entry| entry.slug == "atlas")
        .ok_or("atlas present")?;
    assert_eq!(atlas.state, Some(AgentStateValue::Working));
    assert_eq!(atlas.state_updated_at.as_deref(), Some("2026-09-11T02:15:30Z"));
    Ok(())
}

#[test]
fn apply_state_change_leaves_every_other_agent_untouched() -> Result<(), BoxError> {
    let mut file = sample_file()?;
    let at = datetime!(2026-09-11 02:15:30 UTC);
    apply_state_change(&mut file, "atlas", AgentStateValue::Working, at)?;
    let vela = file
        .member
        .iter()
        .find(|entry| entry.slug == "vela")
        .ok_or("vela present")?;
    assert_eq!(vela.state, None);
    assert_eq!(vela.state_updated_at, None);
    Ok(())
}

#[test]
fn apply_state_change_reports_the_agents_prior_state_on_a_second_transition() -> Result<(), BoxError> {
    let mut file = sample_file()?;
    let first = datetime!(2026-09-11 02:15:30 UTC);
    apply_state_change(&mut file, "atlas", AgentStateValue::Working, first)?;
    let second = datetime!(2026-09-11 03:00:00 UTC);
    let outcome = apply_state_change(&mut file, "atlas", AgentStateValue::Blocked, second)?;
    assert_eq!(outcome.previous_state, Some(AgentStateValue::Working));
    Ok(())
}

#[test]
fn apply_state_change_rejects_an_unknown_agent_slug() -> Result<(), BoxError> {
    let mut file = sample_file()?;
    let at = datetime!(2026-09-11 02:15:30 UTC);
    let Err(error) = apply_state_change(&mut file, "nonexistent", AgentStateValue::Idle, at) else {
        return Err("expected AgentNotFound".into());
    };
    assert!(matches!(&error, AgentStateError::AgentNotFound { agent } if agent == "nonexistent"));
    assert_eq!(error.code(), "agent/not-found");
    Ok(())
}

#[test]
fn team_file_round_trips_through_toml_preserving_unrelated_fields() -> Result<(), BoxError> {
    let mut file = sample_file()?;
    let at = datetime!(2026-09-11 02:15:30 UTC);
    apply_state_change(&mut file, "atlas", AgentStateValue::AwaitingInput, at)?;
    let serialised = toml::to_string(&file)?;
    let reparsed: TeamFile = toml::from_str(&serialised)?;
    let atlas = reparsed
        .member
        .iter()
        .find(|entry| entry.slug == "atlas")
        .ok_or("atlas present")?;
    assert_eq!(atlas.name, "Atlas");
    assert_eq!(atlas.domain.as_deref(), Some("Finance"));
    assert_eq!(atlas.state, Some(AgentStateValue::AwaitingInput));
    Ok(())
}

/// The real bug this test guards: an earlier version of `TeamRecord` declared only the fields this tool itself
/// reads or writes (`slug`/`name`/`domain`/`chief_of_staff`/`state`/`state_updated_at`), so any other
/// operator-authored field (here, `type` and `team`) silently vanished the moment `agent_state_change` next wrote
/// the file, because `Deserialize` without `deny_unknown_fields` ignores an unrecognised key rather than
/// preserving it through to `Serialize`.
#[test]
fn team_file_round_trip_preserves_the_type_and_team_fields() -> Result<(), BoxError> {
    let mut file: TeamFile =
        toml::from_str("[[member]]\nslug = \"piper\"\nname = \"Piper\"\ntype = \"human\"\nteam = \"business\"\n")?;
    let at = datetime!(2026-09-11 02:15:30 UTC);
    apply_state_change(&mut file, "piper", AgentStateValue::Working, at)?;
    let serialised = toml::to_string(&file)?;
    let reparsed: TeamFile = toml::from_str(&serialised)?;
    let piper = reparsed
        .member
        .iter()
        .find(|entry| entry.slug == "piper")
        .ok_or("piper present")?;
    assert_eq!(piper.kind.as_deref(), Some("human"));
    assert_eq!(piper.team.as_deref(), Some("business"));
    Ok(())
}
