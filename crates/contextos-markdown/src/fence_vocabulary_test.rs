use super::*;

fn parse(source: &str) -> Result<FenceVocabulary, String> {
    FenceVocabulary::try_from(source).map_err(|error| error.to_string())
}

#[test]
fn the_built_in_vocabulary_parses_and_covers_the_standard() -> Result<(), String> {
    let vocabulary = parse(DEFAULT_FENCES_TOML)?;
    assert_eq!(vocabulary.standard(), Some("markdown-fence-conventions 2026-10"));
    for name in [
        "note",
        "warning",
        "decision",
        "grid",
        "card",
        "blocked",
        "prompt",
        "page-break",
    ] {
        assert!(vocabulary.fence(name).is_some(), "{name}");
    }
    assert_eq!(vocabulary, FenceVocabulary::default());
    Ok(())
}

#[test]
fn an_alias_is_drawn_as_the_fence_it_names() -> Result<(), String> {
    let vocabulary = parse(DEFAULT_FENCES_TOML)?;
    let banner = vocabulary.fence("banner").ok_or("banner is missing")?;
    assert_eq!(banner.name, "hero");
    assert_eq!(banner.role, FenceRole::Layout);
    Ok(())
}

#[test]
fn a_fence_carries_its_role_tone_and_closing_rule() -> Result<(), String> {
    let vocabulary = parse(DEFAULT_FENCES_TOML)?;
    assert_eq!(
        vocabulary.fence("warning"),
        Some(&FenceSpec {
            name: "warning".to_owned(),
            role: FenceRole::Callout,
            tone: Some(FenceTone::Caution),
            self_closing: false,
        })
    );
    assert!(vocabulary.fence("page-break").is_some_and(|fence| fence.self_closing));
    Ok(())
}

#[test]
fn an_attribute_carries_its_type_and_allowed_values() -> Result<(), String> {
    let vocabulary = parse(DEFAULT_FENCES_TOML)?;
    assert_eq!(
        vocabulary.attribute("display"),
        Some(&AttributeSpec {
            kind: AttributeType::Token,
            values: Some(vec!["block".to_owned(), "inline".to_owned()]),
        })
    );
    assert_eq!(
        vocabulary.attribute("cols").map(|spec| spec.kind),
        Some(AttributeType::Integer)
    );
    Ok(())
}

#[test]
fn a_minimal_manifest_is_accepted() -> Result<(), String> {
    let vocabulary = parse("schema = 1\n[fences.aside]\nrole = \"layout\"\n")?;
    assert!(vocabulary.fence("aside").is_some());
    assert!(vocabulary.fence("note").is_none());
    assert!(vocabulary.standard().is_none());
    Ok(())
}

#[test]
fn a_newer_schema_is_refused_by_name() {
    let error = parse("schema = 2\n").err().unwrap_or_default();
    assert!(error.contains("schema 2 is newer"), "{error}");
}

#[test]
fn a_malformed_manifest_is_refused() {
    let cases = [
        ("not toml at all [", "syntax"),
        ("[fences.note]\nrole = \"callout\"\n", "no schema"),
        ("schema = 1\nextra = true\n", "unknown top-level key"),
        (
            "schema = 1\n[fences.note]\nrole = \"callout\"\ncolour = \"red\"\n",
            "unknown fence key",
        ),
        ("schema = 1\n[fences.note]\nrole = \"shouty\"\n", "unknown role"),
        (
            "schema = 1\n[fences.note]\nrole = \"callout\"\ntone = \"loud\"\n",
            "unknown tone",
        ),
        ("schema = 1\n[fences.note]\n", "no role"),
        (
            "schema = 1\n[fences.a]\nalias_of = \"b\"\n",
            "alias of an unknown fence",
        ),
        (
            "schema = 1\n[fences.a]\nrole = \"layout\"\n[fences.b]\nalias_of = \"a\"\n[fences.c]\nalias_of = \"b\"\n",
            "alias of an alias",
        ),
        (
            "schema = 1\n[fences.a]\nalias_of = \"b\"\nrole = \"layout\"\n[fences.b]\nrole = \"layout\"\n",
            "alias with a role",
        ),
        (
            "schema = 1\n[fences.Note]\nrole = \"callout\"\n",
            "a name outside [a-z0-9-]",
        ),
        (
            "schema = 1\n[attributes.cols]\ntype = \"number\"\n",
            "unknown attribute type",
        ),
        (
            "schema = 1\n[attributes.cols]\ntype = \"integer\"\nvalues = [\"1\"]\n",
            "values on a non-token",
        ),
    ];
    for (source, why) in cases {
        assert!(parse(source).is_err(), "{why}");
    }
}
