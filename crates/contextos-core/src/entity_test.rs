use super::*;

fn roster(slugs: &[&str]) -> EntityRoster {
    EntityRoster::from(&EntitiesFile {
        entities: slugs
            .iter()
            .map(|slug| EntityRecord {
                slug: (*slug).to_owned(),
                name: (*slug).to_owned(),
                accent: 1,
            })
            .collect(),
    })
}

fn scope(roster: &EntityRoster, segments: &[&str]) -> String {
    <&str>::from(&roster.scope_of_segments(segments)).to_owned()
}

#[test]
fn a_top_level_folder_named_for_an_entity_belongs_to_that_entity() {
    let roster = roster(&["zenith", "personal"]);
    assert_eq!(scope(&roster, &["zenith"]), "zenith");
    assert_eq!(scope(&roster, &["zenith", "projects", "beacon"]), "zenith");
    assert_eq!(scope(&roster, &["personal", "journal"]), "personal");
}

#[test]
fn a_memory_category_folder_named_for_an_entity_belongs_to_the_vault() {
    let roster = roster(&["zenith", "acme"]);
    for segments in [
        &["memory", "tasks", "zenith"][..],
        &["memory", "tasks", "acme", "a-001.md"],
        &["memory", "research", "zenith", "x"],
        &["memory", "acme", "x.md"],
    ] {
        assert_eq!(scope(&roster, segments), "vault", "{segments:?}");
    }
}

#[test]
fn a_folder_named_for_an_entity_below_the_first_segment_belongs_to_the_vault() {
    let roster = roster(&["acme"]);
    for segments in [
        &["projects", "acme", "x.md"][..],
        &["registry", "acme"],
        &["templates", "acme", "x.md"],
        &["memory", "tasks", "misc", "acme"],
    ] {
        assert_eq!(scope(&roster, segments), "vault", "{segments:?}");
    }
}

#[test]
fn shared_areas_and_the_vault_root_belong_to_the_vault() {
    let roster = roster(&["acme"]);
    for segments in [
        &[][..],
        &["x.md"],
        &["memory"],
        &["registry", "roles", "x.md"],
        &["templates", "x.md"],
        &["standards"],
        &["scripts"],
    ] {
        assert_eq!(scope(&roster, segments), "vault", "{segments:?}");
    }
}

proptest::proptest! {
    #[test]
    fn only_the_first_segment_ever_decides_the_entity(
        rest in proptest::collection::vec(
            proptest::prop_oneof!["acme", "family", "[a-z0-9]{1,6}"],
            0..5,
        ),
        first in proptest::prop_oneof!["memory", "projects", "[a-z0-9]{1,6}"],
        pick_entity in proptest::bool::ANY,
    ) {
        let roster = roster(&["acme", "family"]);
        let first_segment = if pick_entity { "ACME".to_owned() } else { first.clone() };
        let mut segments = vec![first_segment.clone()];
        segments.extend(rest.iter().cloned());
        let borrowed = segments.iter().map(String::as_str).collect::<Vec<_>>();
        let expected = if first_segment.eq_ignore_ascii_case("acme") {
            "acme"
        } else if first_segment.eq_ignore_ascii_case("family") {
            "family"
        } else {
            "vault"
        };
        proptest::prop_assert_eq!(scope(&roster, &borrowed), expected);
    }
}

#[test]
fn paths_outside_every_entity_belong_to_the_vault() {
    let roster = roster(&["zenith", "personal"]);
    for segments in [
        &[][..],
        &["memory"],
        &["memory", "tasks"],
        &["memory", "wiki", "analytics"],
        &["registry"],
        &["registry", "personal"],
        &["templates", "zenith"],
        &["standards"],
        &["scripts"],
    ] {
        assert_eq!(scope(&roster, segments), "vault", "{segments:?}");
    }
}

#[test]
fn an_entity_slug_at_any_depth_below_the_first_segment_does_not_claim_a_path() {
    let roster = roster(&["family"]);
    assert_eq!(scope(&roster, &["memory", "tasks", "misc", "family"]), "vault");
    assert_eq!(scope(&roster, &["memory", "family"]), "vault");
}

#[test]
fn a_top_level_folder_matches_without_regard_to_ascii_case_and_reports_the_configured_spelling() {
    let roster = roster(&["zenith"]);
    assert_eq!(scope(&roster, &["Zenith", "projects"]), "zenith");
    assert_eq!(scope(&roster, &["ZENITH"]), "zenith");
}

#[test]
fn a_roster_with_no_entities_places_every_path_in_the_vault() {
    let roster = roster(&[]);
    assert_eq!(scope(&roster, &["zenith"]), "vault");
    assert_eq!(scope(&roster, &[]), "vault");
}

#[test]
fn blank_slugs_never_claim_a_path() {
    let roster = roster(&["", "  "]);
    assert_eq!(scope(&roster, &["", "anything"]), "vault");
}

#[test]
fn an_entities_file_reads_slug_name_and_accent_and_tolerates_an_absent_list() -> Result<(), toml::de::Error> {
    let parsed = toml::from_str::<EntitiesFile>("[[entities]]\nslug = \"family\"\nname = \"Family\"\naccent = 4\n")?;
    assert_eq!(
        parsed.entities,
        vec![EntityRecord {
            slug: "family".to_owned(),
            name: "Family".to_owned(),
            accent: 4,
        }]
    );
    assert!(
        toml::from_str::<EntitiesFile>("")?.entities.is_empty(),
        "`toml::from_str::<EntitiesFile>(\"\")?.entities` should be empty, got {:?}",
        toml::from_str::<EntitiesFile>("")?.entities
    );
    Ok(())
}
