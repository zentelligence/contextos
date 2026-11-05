use super::*;
use crate::inbox_outbox::{DirectoryFolder, DirectoryItem};
use crate::team::TeamMemberKind;

fn member(kind: TeamMemberKind, team: Option<&str>, state: MemberState) -> TeamMember {
    TeamMember {
        slug: "m".to_owned(),
        name: "m".to_owned(),
        kind,
        domain: None,
        team: team.map(str::to_owned),
        chief_of_staff: false,
        state,
    }
}

#[test]
fn a_team_count_covers_only_agents_of_that_team_case_insensitively() {
    let members = vec![
        member(TeamMemberKind::Agent, Some("business"), MemberState::Idle),
        member(TeamMemberKind::Agent, Some("Business"), MemberState::Idle),
        member(TeamMemberKind::Human, Some("business"), MemberState::Idle),
        member(TeamMemberKind::Agent, Some("personal"), MemberState::Idle),
        member(TeamMemberKind::Agent, Some("General"), MemberState::Idle),
        member(TeamMemberKind::Human, Some("general"), MemberState::Idle),
        member(TeamMemberKind::Agent, None, MemberState::Idle),
    ];
    assert_eq!(agents_in_team(&members, "business"), 2);
    assert_eq!(agents_in_team(&members, "personal"), 1);
    assert_eq!(agents_in_team(&members, "general"), 1);
}

#[test]
fn the_blocked_count_covers_every_blocked_agent_of_any_team_but_no_human() {
    let members = vec![
        member(TeamMemberKind::Agent, Some("business"), MemberState::Blocked),
        member(TeamMemberKind::Agent, None, MemberState::Blocked),
        member(TeamMemberKind::Human, None, MemberState::Blocked),
        member(TeamMemberKind::Agent, None, MemberState::Working),
    ];
    assert_eq!(blocked_agents(&members), 2);
}

#[test]
fn a_listing_counts_its_folders_and_its_files_together() {
    let listing = DirectoryListing {
        folders: vec![DirectoryFolder { name: "a".to_owned() }],
        files: vec![
            DirectoryItem {
                name: "x.md".to_owned(),
                size: None,
                modified: None,
            },
            DirectoryItem {
                name: "y.md".to_owned(),
                size: None,
                modified: None,
            },
        ],
    };
    assert_eq!(listed_items(&listing), 3);
}

fn stages(pairs: &[(&str, &str)]) -> Vec<TaskStage> {
    pairs
        .iter()
        .map(|(task_type, state)| TaskStage {
            task_type: (*task_type).to_owned(),
            state: (*state).to_owned(),
        })
        .collect()
}

#[test]
fn open_tasks_are_counted_by_each_tasks_own_types_states() {
    let tasks = stages(&[
        ("action", "doing"),
        ("action", "next"),
        ("action", "waiting"),
        ("action", "review"),
        ("action", "done"),
        ("action", "dropped"),
        ("action", "made-up"),
        ("", ""),
        ("decision", "proposed"),
        ("decision", "adopted"),
        ("decision", "pending"),
        ("question", "open"),
        ("question", "answered"),
        ("bug", "doing"),
        ("bug", "done"),
    ]);
    assert_eq!(
        count_open(&TasksConfig::default(), &tasks),
        7,
        "a task whose type is not declared counts by the shared stages"
    );
}

#[test]
fn no_tasks_count_as_zero_open() {
    assert_eq!(count_open(&TasksConfig::default(), &[]), 0);
}

fn result(content: &str, truncated: bool) -> BaseQueryResult {
    BaseQueryResult {
        content: content.to_owned(),
        truncated,
    }
}

#[test]
fn each_tasks_type_and_state_are_read_from_its_row() {
    let read = TaskStages::try_from(result(
        r#"[{"state":"doing","task_type":"action"},{"state":"proposed","task_type":"decision"},{}]"#,
        false,
    ));
    assert_eq!(
        read.map(|read| read.0).ok(),
        Some(stages(&[("action", "doing"), ("decision", "proposed"), ("", "")]))
    );
}

#[test]
fn a_truncated_task_result_is_unreadable_rather_than_an_under_count() {
    assert!(TaskStages::try_from(result(r#"[{"state":"doing"}]"#, true)).is_err());
}

#[test]
fn a_task_result_that_is_not_rows_is_unreadable() {
    assert!(TaskStages::try_from(result("not json", false)).is_err());
}
