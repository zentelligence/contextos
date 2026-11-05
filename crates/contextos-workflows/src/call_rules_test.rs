use proptest::prelude::*;

use super::*;

fn tool(server: &str, name: &str) -> ToolRef {
    ToolRef {
        server: server.to_owned(),
        tool: name.to_owned(),
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

/// The reason a set of lists was refused as contradicting itself, or a description of what happened instead.
fn contradiction(result: Result<CallRules, PackageError>) -> String {
    match result {
        Err(PackageError::CallRuleContradiction { reason }) => reason,
        other => format!("not refused as a contradiction: {other:?}"),
    }
}

fn rules(lists: CallLists) -> Result<CallRules, PackageError> {
    CallRules::try_from(lists)
}

#[test]
fn nothing_declared_permits_every_call() -> Result<(), PackageError> {
    let rules = rules(CallLists::default())?;
    assert!(rules.permits("anything", "whatever"));
    assert!(rules.permits_server("anything"));
    Ok(())
}

#[test]
fn a_server_off_the_allow_list_is_refused_and_one_on_it_is_permitted() -> Result<(), PackageError> {
    let rules = rules(CallLists {
        server_allow: Some(names(&["crm"])),
        ..CallLists::default()
    })?;
    assert!(rules.permits("crm", "contacts_list"));
    assert!(!rules.permits("mail", "send"));
    Ok(())
}

#[test]
fn an_empty_server_allow_list_permits_no_call() -> Result<(), PackageError> {
    let rules = rules(CallLists {
        server_allow: Some(Vec::new()),
        ..CallLists::default()
    })?;
    assert!(!rules.permits("crm", "contacts_list"));
    Ok(())
}

#[test]
fn a_denied_server_is_refused_while_every_other_is_permitted() -> Result<(), PackageError> {
    let rules = rules(CallLists {
        server_deny: names(&["mail"]),
        ..CallLists::default()
    })?;
    assert!(!rules.permits("mail", "send"));
    assert!(rules.permits("crm", "contacts_list"));
    Ok(())
}

#[test]
fn a_tool_allow_restricts_only_the_servers_it_names() -> Result<(), PackageError> {
    let rules = rules(CallLists {
        tool_allow: vec![tool("crm", "list_contacts")],
        ..CallLists::default()
    })?;
    assert!(rules.permits("crm", "list_contacts"));
    assert!(!rules.permits("crm", "delete_contact"));
    assert!(rules.permits("contextos", "fs_read_text_file"));
    Ok(())
}

#[test]
fn a_denied_tool_is_refused_and_its_siblings_are_not() -> Result<(), PackageError> {
    let rules = rules(CallLists {
        tool_deny: vec![tool("contextos", "fs_delete_file")],
        ..CallLists::default()
    })?;
    assert!(!rules.permits("contextos", "fs_delete_file"));
    assert!(rules.permits("contextos", "fs_write_file"));
    Ok(())
}

#[test]
fn the_servers_an_allow_list_names_include_those_of_the_tool_allow_list() -> Result<(), PackageError> {
    let rules = rules(CallLists {
        server_allow: Some(names(&["contextos", "crm"])),
        tool_allow: vec![tool("crm", "list_contacts")],
        ..CallLists::default()
    })?;
    assert_eq!(
        rules.servers_named_by_allow_lists().into_iter().collect::<Vec<_>>(),
        vec!["contextos", "crm"]
    );
    Ok(())
}

#[test]
fn a_server_on_both_server_lists_is_refused_naming_it() {
    let refused = rules(CallLists {
        server_allow: Some(names(&["mail"])),
        server_deny: names(&["mail"]),
        ..CallLists::default()
    });
    let reason = contradiction(refused);
    assert!(reason.contains("mail"), "{reason}");
    assert!(reason.contains("both"), "{reason}");
}

#[test]
fn a_pair_on_both_tool_lists_is_refused_naming_it() {
    let refused = rules(CallLists {
        tool_allow: vec![tool("crm", "list_contacts")],
        tool_deny: vec![tool("crm", "list_contacts")],
        ..CallLists::default()
    });
    let reason = contradiction(refused);
    assert!(reason.contains("crm") && reason.contains("list_contacts"), "{reason}");
}

#[test]
fn a_tool_allow_on_a_denied_server_is_refused_naming_it() {
    let refused = rules(CallLists {
        server_deny: names(&["crm"]),
        tool_allow: vec![tool("crm", "list_contacts")],
        ..CallLists::default()
    });
    let reason = contradiction(refused);
    assert!(reason.contains("crm") && reason.contains("denied"), "{reason}");
}

#[test]
fn a_blank_server_or_tool_name_is_refused() {
    assert!(matches!(
        rules(CallLists {
            server_deny: names(&[" "]),
            ..CallLists::default()
        }),
        Err(PackageError::CallRuleContradiction { .. })
    ));
    assert!(matches!(
        rules(CallLists {
            tool_deny: vec![tool("crm", "")],
            ..CallLists::default()
        }),
        Err(PackageError::CallRuleContradiction { .. })
    ));
}

#[test]
fn the_summary_says_when_nothing_is_restricted_and_otherwise_lists_each_rule() -> Result<(), PackageError> {
    assert_eq!(CallRules::default().summary(), "Every configured server and tool");
    let rules = rules(CallLists {
        server_allow: Some(names(&["contextos", "crm"])),
        server_deny: names(&["mail"]),
        tool_allow: vec![tool("crm", "list_contacts")],
        tool_deny: vec![tool("contextos", "fs_delete_file")],
    })?;
    assert_eq!(
        rules.summary(),
        "Servers allowed: contextos, crm. Servers denied: mail. Tools allowed: crm/list_contacts. Tools denied: contextos/fs_delete_file"
    );
    let only_deny = CallRules::try_from(CallLists {
        server_deny: names(&["mail"]),
        ..CallLists::default()
    })?;
    assert_eq!(only_deny.summary(), "Servers denied: mail");
    Ok(())
}

#[test]
fn every_tool_on_a_server_is_permitted_only_when_nothing_restricts_that_server() -> Result<(), PackageError> {
    assert!(CallRules::default().permits_every_tool("crm"));
    let denied_server = rules(CallLists {
        server_deny: names(&["crm"]),
        ..CallLists::default()
    })?;
    assert!(!denied_server.permits_every_tool("crm"));
    assert!(denied_server.permits_every_tool("mail"));
    let off_the_allow_list = rules(CallLists {
        server_allow: Some(names(&["mail"])),
        ..CallLists::default()
    })?;
    assert!(!off_the_allow_list.permits_every_tool("crm"));
    assert!(off_the_allow_list.permits_every_tool("mail"));
    let one_tool_denied = rules(CallLists {
        tool_deny: vec![tool("crm", "delete_contact")],
        ..CallLists::default()
    })?;
    assert!(!one_tool_denied.permits_every_tool("crm"));
    assert!(one_tool_denied.permits_every_tool("mail"));
    let one_tool_allowed = rules(CallLists {
        tool_allow: vec![tool("crm", "list_contacts")],
        ..CallLists::default()
    })?;
    assert!(!one_tool_allowed.permits_every_tool("crm"));
    assert!(one_tool_allowed.permits_every_tool("mail"));
    Ok(())
}

#[test]
fn a_wildcard_in_a_workflow_tool_list_is_refused_saying_server_lists_cover_every_tool() {
    for lists in [
        CallLists {
            tool_allow: vec![tool("crm", "*")],
            ..CallLists::default()
        },
        CallLists {
            tool_deny: vec![tool("crm", "*")],
            ..CallLists::default()
        },
    ] {
        let reason = contradiction(rules(lists));
        assert!(reason.contains('*') && reason.contains("crm"), "{reason}");
        assert!(reason.contains("server lists cover every tool"), "{reason}");
    }
}

const SERVERS: [&str; 3] = ["a", "b", "c"];
const TOOLS: [&str; 3] = ["x", "y", "z"];

fn any_server() -> impl Strategy<Value = String> {
    prop::sample::select(SERVERS.to_vec()).prop_map(str::to_owned)
}

fn any_tool_ref() -> impl Strategy<Value = ToolRef> {
    (any_server(), prop::sample::select(TOOLS.to_vec())).prop_map(|(server, name)| ToolRef {
        server,
        tool: name.to_owned(),
    })
}

fn any_lists() -> impl Strategy<Value = CallLists> {
    (
        prop::option::of(prop::collection::vec(any_server(), 0..4)),
        prop::collection::vec(any_server(), 0..3),
        prop::collection::vec(any_tool_ref(), 0..4),
        prop::collection::vec(any_tool_ref(), 0..4),
    )
        .prop_map(|(server_allow, server_deny, tool_allow, tool_deny)| CallLists {
            server_allow,
            server_deny,
            tool_allow,
            tool_deny,
        })
}

proptest! {
    #[test]
    fn a_call_is_permitted_exactly_when_the_four_rules_all_hold(lists in any_lists(), server in any_server(), name in prop::sample::select(TOOLS.to_vec())) {
        let Ok(rules) = CallRules::try_from(lists.clone()) else {
            return Ok(());
        };
        let denied_server = lists.server_deny.contains(&server);
        let allowed_server = lists.server_allow.as_ref().is_none_or(|allow| allow.contains(&server));
        let denied_tool = lists.tool_deny.contains(&ToolRef { server: server.clone(), tool: name.to_owned() });
        let restricted = lists.tool_allow.iter().any(|entry| entry.server == server);
        let allowed_tool = !restricted || lists.tool_allow.contains(&ToolRef { server: server.clone(), tool: name.to_owned() });
        prop_assert_eq!(rules.permits(&server, name), !denied_server && allowed_server && !denied_tool && allowed_tool);
    }

    #[test]
    fn a_deny_is_never_lifted_by_any_allow(lists in any_lists(), server in any_server(), name in prop::sample::select(TOOLS.to_vec())) {
        let Ok(rules) = CallRules::try_from(lists.clone()) else {
            return Ok(());
        };
        if lists.server_deny.contains(&server) {
            prop_assert!(!rules.permits(&server, name));
        }
        if lists.tool_deny.contains(&ToolRef { server: server.clone(), tool: name.to_owned() }) {
            prop_assert!(!rules.permits(&server, name));
        }
    }

    #[test]
    fn a_server_deny_is_never_lifted_by_a_tool_allow_that_names_the_server(server in any_server(), name in prop::sample::select(TOOLS.to_vec())) {
        let refused = CallRules::try_from(CallLists {
            server_deny: vec![server.clone()],
            tool_allow: vec![ToolRef { server, tool: name.to_owned() }],
            ..CallLists::default()
        });
        prop_assert!(refused.is_err());
    }

    #[test]
    fn every_tool_is_permitted_exactly_when_each_tool_named_or_unnamed_is(lists in any_lists(), server in any_server()) {
        let Ok(rules) = CallRules::try_from(lists) else {
            return Ok(());
        };
        let each = TOOLS.iter().chain(["unlisted"].iter()).all(|name| rules.permits(&server, name));
        prop_assert_eq!(rules.permits_every_tool(&server), each);
    }

    #[test]
    fn permitting_every_tool_of_a_server_permits_any_one_of_them(lists in any_lists(), server in any_server(), name in prop::sample::select(TOOLS.to_vec())) {
        let Ok(rules) = CallRules::try_from(lists) else {
            return Ok(());
        };
        if rules.permits_every_tool(&server) {
            prop_assert!(rules.permits(&server, name));
            prop_assert!(rules.permits_server(&server));
        }
    }

    #[test]
    fn permitting_a_server_is_necessary_for_permitting_any_of_its_calls(lists in any_lists(), server in any_server(), name in prop::sample::select(TOOLS.to_vec())) {
        let Ok(rules) = CallRules::try_from(lists) else {
            return Ok(());
        };
        if rules.permits(&server, name) {
            prop_assert!(rules.permits_server(&server));
        }
    }
}
