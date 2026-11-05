use super::*;

const TEAM_TOML: &str = "\
[[member]]
slug = \"atlas\"
name = \"Atlas\"
domain = \"Finance\"
team = \"business\"
chief_of_staff = true
state = \"working\"

[[member]]
slug = \"vela\"
name = \"Vela\"
domain = \"Finance\"

[[member]]
slug = \"piper\"
name = \"Piper\"
type = \"human\"
";

#[test]
fn team_file_parses_every_field_including_the_optional_ones() -> Result<(), Box<dyn std::error::Error>> {
    let file: TeamFile = toml::from_str(TEAM_TOML)?;
    let atlas = file.member.first().ok_or("atlas present")?;
    assert_eq!(atlas.slug, "atlas");
    assert_eq!(atlas.domain.as_deref(), Some("Finance"));
    assert_eq!(atlas.team.as_deref(), Some("business"));
    assert!(atlas.chief_of_staff);
    assert_eq!(atlas.state, MemberState::Working);
    Ok(())
}

#[test]
fn a_member_with_no_recorded_state_defaults_to_idle() -> Result<(), Box<dyn std::error::Error>> {
    let file: TeamFile = toml::from_str(TEAM_TOML)?;
    let vela = file.member.get(1).ok_or("vela present")?;
    assert_eq!(vela.state, MemberState::Idle);
    assert_eq!(vela.team, None);
    assert!(!vela.chief_of_staff);
    Ok(())
}

#[test]
fn a_member_with_no_type_field_defaults_to_agent() -> Result<(), Box<dyn std::error::Error>> {
    let file: TeamFile = toml::from_str(TEAM_TOML)?;
    let atlas = file.member.first().ok_or("atlas present")?;
    assert_eq!(atlas.kind, TeamMemberKind::Agent);
    Ok(())
}

#[test]
fn a_member_explicitly_typed_human_parses_as_human() -> Result<(), Box<dyn std::error::Error>> {
    let file: TeamFile = toml::from_str(TEAM_TOML)?;
    let piper = file.member.get(2).ok_or("piper present")?;
    assert_eq!(piper.kind, TeamMemberKind::Human);
    Ok(())
}

#[test]
fn member_state_labels_are_lowercase_kebab_case() {
    assert_eq!(MemberState::Working.label(), "working");
    assert_eq!(MemberState::Blocked.label(), "blocked");
    assert_eq!(MemberState::Idle.label(), "idle");
    assert_eq!(MemberState::AwaitingInput.label(), "awaiting-input");
}

#[test]
fn an_empty_team_file_parses_to_an_empty_list() -> Result<(), Box<dyn std::error::Error>> {
    let file: TeamFile = toml::from_str("")?;
    assert!(file.member.is_empty());
    Ok(())
}

fn member(slug: &str, kind: TeamMemberKind, team: Option<&str>, state: MemberState) -> TeamMember {
    TeamMember {
        slug: slug.to_owned(),
        name: slug.to_owned(),
        kind,
        domain: None,
        team: team.map(str::to_owned),
        chief_of_staff: false,
        state,
    }
}

#[test]
fn a_state_filter_is_permissive_when_none_is_selected() {
    let blocked = member("a", TeamMemberKind::Agent, None, MemberState::Blocked);
    assert!(blocked.matches_state(None));
}

#[test]
fn a_state_filter_only_matches_the_selected_state() {
    let blocked = member("a", TeamMemberKind::Agent, None, MemberState::Blocked);
    assert!(blocked.matches_state(Some("blocked")));
    assert!(!blocked.matches_state(Some("working")));
}

#[test]
fn a_team_filter_is_permissive_when_none_is_selected() {
    assert!(member("a", TeamMemberKind::Agent, None, MemberState::Idle).matches_team(None));
}

#[test]
fn a_team_filter_only_matches_the_selected_team_case_insensitively() {
    let business = member("a", TeamMemberKind::Agent, Some("Business"), MemberState::Idle);
    assert!(business.matches_team(Some("business")));
    assert!(!business.matches_team(Some("personal")));
}

#[test]
fn a_team_filter_excludes_a_member_with_no_team_set() {
    assert!(!member("a", TeamMemberKind::Agent, None, MemberState::Idle).matches_team(Some("business")));
}

#[test]
fn matching_agents_never_includes_a_human_member() {
    let members = vec![
        member("a", TeamMemberKind::Agent, Some("business"), MemberState::Idle),
        member("h", TeamMemberKind::Human, Some("business"), MemberState::Idle),
    ];
    let slugs: Vec<&str> = matching_agents(&members, Some("business"), None)
        .map(|member| member.slug.as_str())
        .collect();
    assert_eq!(slugs, ["a"]);
}

#[test]
fn matching_agents_combines_both_filters() {
    let members = vec![
        member("a", TeamMemberKind::Agent, Some("business"), MemberState::Blocked),
        member("b", TeamMemberKind::Agent, Some("business"), MemberState::Idle),
        member("c", TeamMemberKind::Agent, Some("personal"), MemberState::Blocked),
    ];
    assert_eq!(matching_agents(&members, Some("business"), Some("blocked")).count(), 1);
    assert_eq!(matching_agents(&members, None, None).count(), 3);
}

fn text(content: &str) -> AttachedRead {
    AttachedRead::Text(crate::vault_content::AttachedText {
        text: content.to_owned(),
        content_hash: None,
    })
}

fn rule_of(read: AttachedRead) -> Option<TeamRule> {
    match TeamRoster::try_from(read) {
        Err(RosterLoadError::Invalid(error)) => Some(error.rule()),
        Err(RosterLoadError::Unreachable(_) | RosterLoadError::Unreadable { .. }) | Ok(_) => None,
    }
}

fn refused(code: Option<&str>) -> AttachedRead {
    AttachedRead::Refused {
        code: code.map(str::to_owned),
    }
}

#[test]
fn a_valid_team_read_is_a_roster() {
    let valid = TeamRoster::try_from(text(TEAM_TOML));
    assert!(
        valid
            .as_ref()
            .is_ok_and(|roster| roster.chief_of_staff().slug.as_str() == "atlas"),
        "a valid team is a roster: {valid:?}"
    );
}

#[test]
fn an_absent_team_file_is_invalid_naming_that_it_is_absent() {
    assert_eq!(rule_of(refused(Some("path/not-found"))), Some(TeamRule::FileAbsent));
}

#[test]
fn a_team_file_that_is_not_text_is_invalid_naming_that_it_is_not_text() {
    assert_eq!(rule_of(AttachedRead::NotText), Some(TeamRule::FileNotText));
}

#[test]
fn a_team_file_that_does_not_parse_is_invalid_naming_that_it_does_not_parse_and_not_a_missing_chief() {
    assert_eq!(rule_of(text("[[member]\nslug = ")), Some(TeamRule::FileUnparsable));
    assert_eq!(
        rule_of(text("[[member]]\nslug = \"atlas\"\nchief_of_staff = \"yes\"\n")),
        Some(TeamRule::FileUnparsable),
        "a field of the wrong type is a file that does not parse as a team"
    );
}

#[test]
fn a_parsed_team_that_breaks_a_rule_names_that_rule() {
    assert_eq!(
        rule_of(text(
            "[[member]]\nslug = \"atlas\"\nchief_of_staff = true\n[[member]]\nslug = \"vela\"\nchief_of_staff = true\n"
        )),
        Some(TeamRule::SeveralChiefsOfStaff)
    );
    assert_eq!(
        rule_of(text("[[member]]\nslug = \"atlas\"\n[[member]]\nslug = \"vela\"\n")),
        Some(TeamRule::NoChiefOfStaff)
    );
    assert_eq!(
        rule_of(text("")),
        Some(TeamRule::Empty),
        "a team with no members is its own rule, not a team that lacks a chief"
    );
    assert_eq!(
        rule_of(text("[[member]]\nslug = \"Atlas\"\nchief_of_staff = true\n")),
        Some(TeamRule::InvalidSlug)
    );
    assert_eq!(
        rule_of(text(
            "[[member]]\nslug = \"atlas\"\nchief_of_staff = true\n[[member]]\nslug = \"atlas\"\n"
        )),
        Some(TeamRule::DuplicateSlug)
    );
}

#[test]
fn a_read_refused_for_any_reason_but_absence_is_an_unreadable_team_and_not_an_invalid_one() {
    for code in [Some("io/permission-denied"), Some("io/too-large"), None] {
        let loaded = TeamRoster::try_from(refused(code));
        assert!(
            matches!(&loaded, Err(RosterLoadError::Unreadable { code: reported }) if reported.as_deref() == code),
            "{code:?}: {loaded:?}"
        );
    }
}
