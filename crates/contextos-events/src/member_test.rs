use time::macros::datetime;

use super::*;
use crate::Severity;

const AT: time::OffsetDateTime = datetime!(2026-10-28 03:15:42 UTC);

fn change(from: Option<&str>, to: &str, reason: Option<&str>, has_definition: bool) -> MemberStateChange {
    MemberStateChange {
        member: "ellis".to_owned(),
        from: from.map(str::to_owned),
        to: to.to_owned(),
        reason: reason.map(str::to_owned),
        has_definition,
    }
}

#[test]
fn a_transition_renders_the_member_fields_after_the_common_ones() {
    let event = EventRecord::from((&change(Some("idle"), "working", Some("picked up A-024"), true), AT));

    assert_eq!(
        event.to_string(),
        "---\ntype: event\nkind: member.state-changed\nseverity: info\nat: 2026-10-28T03:15:42Z\nentity: 'vault'\n\
         subject: '[[registry/agents/ellis|ellis]]'\nsubject_path: 'registry/agents/ellis.md'\nmember: 'ellis'\nfrom: 'idle'\nto: 'working'\n\
         reason: 'picked up A-024'\n---\n\n# ellis: idle \u{2192} working\n"
    );
    assert_eq!(
        event.relative_path(0),
        "memory/events/2026/10/2026-10-28-031542-ellis.md"
    );
}

#[test]
fn the_first_transition_omits_from_and_a_missing_reason_is_omitted() {
    let text = EventRecord::from((&change(None, "idle", None, true), AT)).to_string();

    assert!(!text.contains("from:") && !text.contains("reason:"), "{text}");
    assert!(text.ends_with("# ellis: idle\n"), "{text}");
}

#[test]
fn becoming_blocked_is_a_warning_and_anything_else_is_info() {
    assert_eq!(
        EventRecord::from((&change(Some("working"), "blocked", None, true), AT)).severity,
        Severity::Warning
    );
    for to in ["working", "idle", "awaiting-input"] {
        assert_eq!(
            EventRecord::from((&change(Some("blocked"), to, None, true), AT)).severity,
            Severity::Info,
            "{to}"
        );
    }
}

#[test]
fn the_subject_is_the_members_definition_only_when_it_exists() {
    let event = EventRecord::from((&change(None, "idle", None, false), AT));

    assert_eq!(event.subject, None);
    assert!(!event.to_string().contains("subject"));
    assert!(event.relative_path(0).ends_with("-member-state-changed.md"));
}
