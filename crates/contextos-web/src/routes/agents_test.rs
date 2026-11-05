use crate::team::{MemberState, TeamMember, TeamMemberKind};

use std::collections::HashMap;

use super::{group_by_domain, list_entry, render_list};

fn agent(slug: &str, domain: Option<&str>, state: MemberState) -> TeamMember {
    TeamMember {
        slug: slug.to_owned(),
        name: slug.to_owned(),
        kind: TeamMemberKind::Agent,
        domain: domain.map(str::to_owned),
        team: None,
        chief_of_staff: false,
        state,
    }
}

fn human(slug: &str) -> TeamMember {
    TeamMember {
        kind: TeamMemberKind::Human,
        ..agent(slug, None, MemberState::Idle)
    }
}

#[test]
fn list_entry_builds_the_registry_detail_href_from_the_slug() {
    let entry = list_entry(
        "vault",
        &agent("atlas", Some("Finance"), MemberState::Working),
        &HashMap::new(),
    );
    assert_eq!(entry.detail_href, "/vault/registry/agents/atlas.md");
    assert_eq!(entry.state_label, "working");
}

#[test]
fn group_by_domain_groups_agents_sharing_a_domain_together() {
    let agents = vec![
        agent("atlas", Some("Finance"), MemberState::Idle),
        agent("vela", Some("Ops"), MemberState::Idle),
        agent("corvus", Some("Finance"), MemberState::Idle),
    ];
    let groups = group_by_domain("vault", &agents, &HashMap::new());
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].domain, "Finance");
    assert_eq!(groups[0].agents.len(), 2);
    assert_eq!(groups[1].domain, "Ops");
    assert_eq!(groups[1].agents.len(), 1);
}

#[test]
fn group_by_domain_places_an_unassigned_agent_in_its_own_literal_group() {
    let agents = vec![agent("atlas", None, MemberState::Idle)];
    let groups = group_by_domain("vault", &agents, &HashMap::new());
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].domain, "Unassigned");
}

#[test]
fn render_list_excludes_a_human_team_member_from_the_agents_screen() {
    let members = vec![agent("atlas", Some("Finance"), MemberState::Idle), human("piper")];
    let fragment = render_list("vault", &members, &super::AgentsQuery::default(), &HashMap::new());
    assert!(fragment.contains("atlas"), "{fragment}");
    assert!(!fragment.contains("piper"), "{fragment}");
}
