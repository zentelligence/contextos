use super::*;

fn record(slug: &str, kind: MemberKind, chief_of_staff: bool) -> TeamMemberRecord {
    TeamMemberRecord {
        slug: slug.to_owned(),
        name: format!("Name of {slug}"),
        kind,
        chief_of_staff,
    }
}

fn file(records: Vec<TeamMemberRecord>) -> TeamFile {
    TeamFile { member: records }
}

fn slug(text: &str) -> Result<MemberSlug, InvalidMemberSlug> {
    MemberSlug::try_from(text)
}

fn refused_rule(records: Vec<TeamMemberRecord>) -> Option<TeamRule> {
    TeamRoster::try_from(file(records)).err().map(|error| error.rule())
}

#[test]
fn a_team_with_unique_slugs_and_one_chief_of_staff_is_a_roster() -> Result<(), Box<dyn std::error::Error>> {
    let roster = TeamRoster::try_from(file(vec![
        record("atlas", MemberKind::Agent, true),
        record("vela", MemberKind::Agent, false),
        record("piper", MemberKind::Human, false),
    ]))?;

    assert_eq!(roster.members().len(), 3);
    assert_eq!(roster.chief_of_staff().slug.as_str(), "atlas");
    Ok(())
}

#[test]
fn a_team_with_no_chief_of_staff_is_refused_naming_that_rule() {
    let error = TeamRoster::try_from(file(vec![
        record("atlas", MemberKind::Agent, false),
        record("vela", MemberKind::Agent, false),
    ]))
    .err();

    assert_eq!(
        error.as_ref().map(TeamRosterError::rule),
        Some(TeamRule::NoChiefOfStaff)
    );
    assert_eq!(error.as_ref().map(TeamRosterError::code), Some("workflow/team-invalid"));
    assert!(
        error
            .as_ref()
            .is_some_and(|error| error.to_string().contains(TeamRule::NoChiefOfStaff.as_str())),
        "the message names the rule: {error:?}"
    );
}

#[test]
fn a_team_with_two_chiefs_of_staff_is_refused_naming_that_rule_without_member_names() {
    let error = TeamRoster::try_from(file(vec![
        record("atlas", MemberKind::Agent, true),
        record("vela", MemberKind::Agent, true),
    ]))
    .err();

    assert_eq!(
        error.as_ref().map(TeamRosterError::rule),
        Some(TeamRule::SeveralChiefsOfStaff)
    );
    let shown = error.map(|error| format!("{error} {error:?}")).unwrap_or_default();
    assert!(shown.contains(TeamRule::SeveralChiefsOfStaff.as_str()), "{shown}");
    for private in ["atlas", "vela", "Name of"] {
        assert!(!shown.contains(private), "{shown} leaks {private}");
    }
}

#[test]
fn an_empty_team_is_refused_naming_its_own_rule_not_the_missing_chief_of_staff() {
    assert_eq!(refused_rule(Vec::new()), Some(TeamRule::Empty));
    assert_eq!(TeamRule::Empty.as_str(), "team-empty");
}

#[test]
fn a_slug_that_is_empty_has_capitals_or_other_characters_is_refused() {
    for bad in ["", "Atlas", "at las", "atlas_2", "atlas/vela", "atlás"] {
        assert_eq!(
            refused_rule(vec![record(bad, MemberKind::Agent, true)]),
            Some(TeamRule::InvalidSlug),
            "{bad:?}"
        );
    }
}

#[test]
fn two_members_sharing_a_slug_are_refused() {
    assert_eq!(
        refused_rule(vec![
            record("atlas", MemberKind::Agent, true),
            record("atlas", MemberKind::Human, false),
        ]),
        Some(TeamRule::DuplicateSlug)
    );
}

#[test]
fn the_chief_of_staff_is_found_by_its_flag_whatever_its_slug_kind_or_position() -> Result<(), Box<dyn std::error::Error>>
{
    let last_human = TeamRoster::try_from(file(vec![
        record("atlas", MemberKind::Agent, false),
        record("chief-of-staff", MemberKind::Agent, false),
        record("piper", MemberKind::Human, true),
    ]))?;
    let first_agent = TeamRoster::try_from(file(vec![
        record("vela", MemberKind::Agent, true),
        record("piper", MemberKind::Human, false),
    ]))?;

    assert_eq!(last_human.chief_of_staff().slug.as_str(), "piper");
    assert_eq!(last_human.chief_of_staff().kind, MemberKind::Human);
    assert_eq!(first_agent.chief_of_staff().slug.as_str(), "vela");
    Ok(())
}

#[test]
fn a_member_is_found_only_by_its_exact_slug() -> Result<(), Box<dyn std::error::Error>> {
    let roster = TeamRoster::try_from(file(vec![
        record("atlas", MemberKind::Agent, true),
        record("piper", MemberKind::Human, false),
    ]))?;

    assert_eq!(
        roster.member(&slug("piper")?).map(|member| member.name.as_str()),
        Some("Name of piper")
    );
    assert!(roster.member(&slug("pete")?).is_none());
    assert!(roster.member(&slug("piper-2")?).is_none());
    assert!(
        MemberSlug::try_from("Piper").is_err(),
        "a differently cased slug cannot even be named, so it cannot match"
    );
    Ok(())
}

#[test]
fn member_slug_accepts_lowercase_letters_digits_and_hyphens_only() {
    for good in ["a", "atlas", "agent-7", "7", "a-b-c"] {
        assert_eq!(
            MemberSlug::try_from(good).map(|parsed| parsed.as_str().to_owned()),
            Ok(good.to_owned())
        );
    }
    for bad in ["", " ", "A", "a b", "a_b", "a.b", "a/b", "é", "a\n"] {
        assert_eq!(MemberSlug::try_from(bad), Err(InvalidMemberSlug), "{bad:?}");
    }
}

#[test]
fn an_owner_stands_as_a_member_not_a_member_invalid_or_undeclared() -> Result<(), Box<dyn std::error::Error>> {
    let roster = TeamRoster::try_from(file(vec![
        record("atlas", MemberKind::Agent, true),
        record("piper", MemberKind::Human, false),
    ]))?;
    let piper = DeclaredOwner::Slug(slug("piper")?);
    let ghost = DeclaredOwner::Slug(slug("ghost")?);
    let shouted = DeclaredOwner::Invalid("Piper".to_owned());

    assert!(matches!(
        OwnerStanding::from((Some(&piper), &roster)),
        OwnerStanding::Member(member) if member.slug.as_str() == "piper"
    ));
    assert_eq!(
        OwnerStanding::from((Some(&ghost), &roster)),
        OwnerStanding::NotAMember(slug("ghost")?)
    );
    assert_eq!(
        OwnerStanding::from((Some(&shouted), &roster)),
        OwnerStanding::InvalidSlug("Piper".to_owned()),
        "text that is not a slug is kept as written, never lowercased to match a member"
    );
    assert_eq!(OwnerStanding::from((None, &roster)), OwnerStanding::Undeclared);
    Ok(())
}

#[test]
fn a_team_file_in_toml_parses_into_the_schema_a_job_depends_on() -> Result<(), Box<dyn std::error::Error>> {
    let parsed: TeamFile = toml::from_str(
        "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\nchief_of_staff = true\ndomain = \"Finance\"\nstate = \"working\"\n\n\
         [[member]]\nslug = \"piper\"\nname = \"Piper\"\ntype = \"human\"\n",
    )?;

    let roster = TeamRoster::try_from(parsed)?;

    assert_eq!(roster.chief_of_staff().slug.as_str(), "atlas");
    assert_eq!(
        roster.member(&slug("piper")?).map(|member| member.kind),
        Some(MemberKind::Human)
    );
    Ok(())
}

#[test]
fn a_declared_owner_that_is_not_slug_shaped_keeps_its_text_exactly_and_is_never_lowercased()
-> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(DeclaredOwner::from("vela"), DeclaredOwner::Slug(slug("vela")?));
    for text in ["Vela", "vela ", "Vela Smith", "ve_la", ""] {
        let declared = DeclaredOwner::from(text);
        assert_eq!(declared, DeclaredOwner::Invalid(text.to_owned()), "{text:?}");
        assert_eq!(declared.as_str(), text, "the text is kept as written");
    }
    Ok(())
}
