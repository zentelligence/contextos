use super::*;

#[test]
fn a_kind_name_is_dot_separated_lower_case_segments() {
    for valid in [
        "import",
        "task.state-changed",
        "session.run-finished",
        "a1.b-2.c3",
        "trade.executed",
    ] {
        let name = KindName::try_from(valid).ok();
        assert_eq!(
            name.as_ref().map(KindName::as_str),
            Some(valid),
            "{valid} should be accepted"
        );
    }
}

#[test]
fn a_kind_name_refuses_anything_else() {
    let too_long = "a".repeat(101);
    for invalid in [
        "",
        ".",
        "a.",
        ".a",
        "a..b",
        "Task.created",
        "task created",
        "task_created",
        "-a",
        "a-",
        "a.-b",
        "a.b-",
        "task/created",
        "tâsk",
        too_long.as_str(),
    ] {
        let error = KindName::try_from(invalid).err().map(|error| error.code());
        assert_eq!(error, Some("events/invalid-fields"), "{invalid:?} should be refused");
    }
}

#[test]
fn every_server_kind_is_named_and_found_by_its_name() {
    let names: Vec<&str> = ServerKind::ALL.iter().map(|kind| kind.as_str()).collect();
    assert_eq!(
        names,
        [
            "task.created",
            "task.state-changed",
            "task.reassigned",
            "task.claim-lapsed",
            "task.unknown-state",
            "task.moved",
            "member.state-changed",
            "vault.degraded",
            "vault.recovered",
        ]
    );
    for kind in ServerKind::ALL {
        assert_eq!(ServerKind::try_from(kind.as_str()).ok(), Some(kind));
        assert!(
            KindName::try_from(kind.as_str()).is_ok(),
            "{kind:?} must be a valid kind name"
        );
    }
    assert_eq!(
        ServerKind::try_from("import.completed").err().map(|e| e.code()),
        Some("events/unknown-kind")
    );
}

#[test]
fn server_kinds_default_to_the_severity_their_meaning_calls_for() {
    let warning = [
        ServerKind::TaskClaimLapsed,
        ServerKind::TaskUnknownState,
        ServerKind::VaultDegraded,
    ];
    for kind in ServerKind::ALL {
        let expected = if warning.contains(&kind) {
            Severity::Warning
        } else {
            Severity::Info
        };
        assert_eq!(kind.default_severity(), expected, "{kind:?}");
    }
}

#[test]
fn severities_parse_from_and_print_as_their_lower_case_names() {
    for severity in Severity::ALL {
        assert_eq!(Severity::try_from(severity.as_str()).ok(), Some(severity));
    }
    assert_eq!(
        Severity::try_from("Info").err().map(|e| e.code()),
        Some("events/invalid-severity")
    );
    assert_eq!(
        Severity::try_from("").err().map(|e| e.code()),
        Some("events/invalid-severity")
    );
}

#[test]
fn severities_order_from_info_to_error() {
    assert!(Severity::Info < Severity::Warning && Severity::Warning < Severity::Error);
}

#[test]
fn a_shipped_kind_is_matched_exactly_never_by_prefix_or_substring() {
    for shipped in [
        "task.created",
        "member.state-changed",
        "vault.recovered",
        "job.failed",
        "job.retired",
        "session.run-finished",
        "agent.delegated",
    ] {
        assert!(is_shipped_kind(shipped), "{shipped}");
    }
    for external in [
        "task.",
        "my-task.created",
        "task.created-by-me",
        "job.custom",
        "session.x",
        "agent.custom",
        "trade.executed",
        "vault",
    ] {
        assert!(!is_shipped_kind(external), "{external}");
    }
}

#[test]
fn every_server_kind_names_the_fields_it_carries() {
    for kind in ServerKind::ALL {
        assert!(!kind.fields().is_empty(), "{}", kind.as_str());
    }
    assert_eq!(ServerKind::TaskReassigned.fields(), ["from", "to"]);
}
