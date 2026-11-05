//! `contextos_web::team::load_roster` against a real `contextos` session and a real temporary vault: the strict reading
//! of `.contextos/team.toml` that jobs depend on, telling a server that cannot be reached, or a file it cannot read,
//! from a team that cannot be used, and leaving the lenient `load_team` the screens use as it was.

mod support;

use contextos_core::TeamRule;
use contextos_web::team::{RosterLoadError, load_roster, load_team};
use support::BoxError;
use support::jobs::{JobVault, Spec, TEAM, TEAM_UNPARSABLE, TEAM_WITH_TWO_CHIEFS, TEAM_WITHOUT_CHIEF, VAULT_NAME};

async fn roster_of(
    team: Option<&'static str>,
) -> Result<Result<contextos_core::TeamRoster, RosterLoadError>, BoxError> {
    let host = JobVault::open(&Spec {
        team,
        ..Spec::default()
    })
    .await?;
    let client = host.clients.primary().ok_or("no primary server")?;
    Ok(load_roster(&client, VAULT_NAME).await)
}

fn rule_of(loaded: Result<contextos_core::TeamRoster, RosterLoadError>, case: &str) -> Result<TeamRule, BoxError> {
    match loaded {
        Err(RosterLoadError::Invalid(error)) => {
            assert_eq!(error.code(), "workflow/team-invalid", "{case}");
            Ok(error.rule())
        }
        Err(other) => Err(format!("{case}: expected an invalid team, got {other}").into()),
        Ok(_) => Err(format!("{case}: expected an invalid team, got a roster").into()),
    }
}

#[tokio::test]
async fn a_valid_team_loads_as_a_roster_whose_chief_of_staff_is_the_flagged_member() -> Result<(), BoxError> {
    let roster = roster_of(Some(TEAM)).await?.map_err(|error| error.to_string())?;

    assert_eq!(roster.chief_of_staff().slug.as_str(), "atlas");
    let slugs: Vec<&str> = roster.members().iter().map(|member| member.slug.as_str()).collect();
    assert_eq!(
        slugs,
        ["atlas", "vela", "piper"],
        "every member, in the order the file lists them"
    );
    Ok(())
}

#[tokio::test]
async fn a_team_with_two_chiefs_of_staff_is_invalid_naming_that_rule() -> Result<(), BoxError> {
    let rule = rule_of(roster_of(Some(TEAM_WITH_TWO_CHIEFS)).await?, "two chiefs")?;

    assert_eq!(rule, TeamRule::SeveralChiefsOfStaff);
    Ok(())
}

#[tokio::test]
async fn a_team_with_no_chief_of_staff_is_invalid_naming_that_rule() -> Result<(), BoxError> {
    // A usable team first, so a loader that refuses everything cannot satisfy the case below.
    assert!(roster_of(Some(TEAM)).await?.is_ok(), "a valid team loads");

    let rule = rule_of(roster_of(Some(TEAM_WITHOUT_CHIEF)).await?, "no chief")?;

    assert_eq!(rule, TeamRule::NoChiefOfStaff);
    Ok(())
}

#[tokio::test]
async fn an_absent_team_is_invalid_naming_that_it_is_absent() -> Result<(), BoxError> {
    // A usable team first, so a loader that refuses everything cannot satisfy the case below.
    assert!(roster_of(Some(TEAM)).await?.is_ok(), "a valid team loads");

    let rule = rule_of(roster_of(None).await?, "absent")?;

    assert_eq!(
        rule,
        TeamRule::FileAbsent,
        "not a missing chief, and not an unreachable server"
    );
    Ok(())
}

#[tokio::test]
async fn an_unparsable_team_is_invalid_naming_that_it_does_not_parse() -> Result<(), BoxError> {
    assert!(roster_of(Some(TEAM)).await?.is_ok(), "a valid team loads");

    let rule = rule_of(roster_of(Some(TEAM_UNPARSABLE)).await?, "unparsable")?;

    assert_eq!(
        rule,
        TeamRule::FileUnparsable,
        "a syntax error is not a missing chief of staff"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_team_file_the_server_refuses_to_read_is_unreadable_and_not_an_invalid_team() -> Result<(), BoxError> {
    use std::os::unix::fs::PermissionsExt as _;

    if support::directory_permissions_are_ignored()? {
        return Ok(());
    }
    let host = JobVault::open(&Spec::default()).await?;
    let client = host.clients.primary().ok_or("no primary server")?;
    assert!(
        load_roster(&client, VAULT_NAME).await.is_ok(),
        "the team reads while it is readable"
    );
    let team = host.root().join(".contextos/team.toml");
    std::fs::set_permissions(&team, std::fs::Permissions::from_mode(0o000))?;

    let loaded = load_roster(&client, VAULT_NAME).await;

    std::fs::set_permissions(&team, std::fs::Permissions::from_mode(0o644))?;
    match loaded {
        Err(RosterLoadError::Unreadable { .. }) => Ok(()),
        Err(other) => Err(format!("expected an unreadable team, got {other}").into()),
        Ok(_) => Err("expected an unreadable team, got a roster".into()),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_server_that_cannot_be_reached_is_unreachable_and_not_an_invalid_team() -> Result<(), BoxError> {
    let host = JobVault::open(&Spec::default()).await?;
    let client = host.clients.primary().ok_or("no primary server")?;
    assert!(
        load_roster(&client, VAULT_NAME).await.is_ok(),
        "the server answers before it is stopped"
    );

    let pid = client
        .pid()
        .ok_or("the vault's stdio session reports its child process id")?;
    let status = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()?;
    assert!(status.success(), "a server was running to stop");

    match load_roster(&client, VAULT_NAME).await {
        Err(RosterLoadError::Unreachable(_)) => Ok(()),
        Err(other) => Err(format!("expected an unreachable server, got {other}").into()),
        Ok(_) => Err("expected an unreachable server, got a roster".into()),
    }
}

#[tokio::test]
async fn the_lenient_team_for_screens_still_reads_an_absent_file_as_empty_while_the_roster_refuses_it()
-> Result<(), BoxError> {
    let valid = JobVault::open(&Spec::default()).await?;
    let client = valid.clients.primary().ok_or("no primary server")?;
    assert_eq!(
        load_team(&client, VAULT_NAME).await.len(),
        3,
        "a valid team lists its members"
    );
    assert!(load_roster(&client, VAULT_NAME).await.is_ok());

    for (case, team, rule) in [
        ("absent", None, TeamRule::FileAbsent),
        ("unparsable", Some(TEAM_UNPARSABLE), TeamRule::FileUnparsable),
    ] {
        let host = JobVault::open(&Spec {
            team,
            ..Spec::default()
        })
        .await?;
        let client = host.clients.primary().ok_or("no primary server")?;

        assert!(
            load_team(&client, VAULT_NAME).await.is_empty(),
            "{case}: the screens read it as an empty team"
        );
        assert_eq!(rule_of(load_roster(&client, VAULT_NAME).await, case)?, rule, "{case}");
    }
    Ok(())
}
