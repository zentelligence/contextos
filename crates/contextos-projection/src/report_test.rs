use super::*;

fn message(kind: FindingKind, vault: Option<&str>) -> String {
    Finding::from(&Subject {
        kind,
        host: Some(HostKind::ClaudeCode),
        definition: DefinitionKind::Agent,
        name: "sam",
        vault,
        other_vault: None,
        rejection: None,
        owner: None,
    })
    .message
}

#[test]
fn a_stale_finding_names_the_vault_whose_definition_it_differs_from() {
    assert_eq!(
        message(FindingKind::ProjectionStale, Some("psyphi")),
        "The agent `sam` for claude is missing or differs from what vault `psyphi` now defines."
    );
}

#[test]
fn a_stale_finding_without_a_vault_still_reads_as_a_sentence() {
    assert_eq!(
        message(FindingKind::ProjectionStale, None),
        "The agent `sam` for claude is missing or differs from its source definition."
    );
}

#[test]
fn an_unowned_collision_names_the_vault_whose_definition_it_differs_from() {
    assert_eq!(
        message(FindingKind::ProjectionUnownedCollision, Some("psyphi")),
        "A file or folder at the agent `sam` for claude was not projected by ContextOS and differs from what vault \
         `psyphi` defines; move it or rename the definition."
    );
}

#[test]
fn an_unowned_collision_without_a_vault_still_reads_as_a_sentence() {
    assert_eq!(
        message(FindingKind::ProjectionUnownedCollision, None),
        "A file or folder at the agent `sam` for claude was not projected by ContextOS and differs from its source \
         definition; move it or rename the definition."
    );
}
