use contextos_obsidian::{FrontmatterDocument, NoteCreateInput, NoteDocument, NoteError};
use serde_json::{Map, json};

#[test]
fn note_creation_uses_resolved_defaults_in_stable_key_order() -> Result<(), Box<dyn std::error::Error>> {
    let note = NoteDocument::try_from(NoteCreateInput {
        title: "Daily Reflection",
        frontmatter: Map::new(),
        content: "# Daily Reflection\n\nToday was productive.\n",
        timestamp: "2026-07-18T18:30:00+10:00",
        entity: Some("vault"),
    })?;
    let rendered = String::try_from(note)?;
    let parsed = FrontmatterDocument::try_from(rendered.as_str())?;
    let keys = parsed.frontmatter().keys().map(String::as_str).collect::<Vec<_>>();

    assert_eq!(
        keys,
        [
            "type",
            "title",
            "entity",
            "status",
            "created",
            "updated",
            "tags",
            "aliases",
            "description",
            "summary",
            "excerpt"
        ]
    );
    assert_eq!(parsed.frontmatter().get("type"), Some(&json!("note")));
    assert_eq!(parsed.frontmatter().get("title"), Some(&json!("Daily Reflection")));
    assert_eq!(parsed.frontmatter().get("entity"), Some(&json!("vault")));
    assert_eq!(parsed.frontmatter().get("status"), Some(&json!("new")));
    assert_eq!(parsed.frontmatter().get("tags"), Some(&json!([])));
    assert_eq!(parsed.frontmatter().get("aliases"), Some(&json!([])));
    assert_eq!(parsed.body(), "# Daily Reflection\n\nToday was productive.\n");
    Ok(())
}

#[test]
fn supplied_frontmatter_overrides_defaults_and_keeps_additional_keys() -> Result<(), Box<dyn std::error::Error>> {
    let supplied = serde_json::from_value(json!({
        "entity": "business",
        "status": "active",
        "tags": ["review"],
        "priority": 2
    }))?;

    let note = NoteDocument::try_from(NoteCreateInput {
        title: "Review",
        frontmatter: supplied,
        content: "Review body.\n",
        timestamp: "2026-07-18T18:30:00+10:00",
        entity: Some("vault"),
    })?;
    let rendered = String::try_from(note)?;
    let parsed = FrontmatterDocument::try_from(rendered.as_str())?;

    assert_eq!(parsed.frontmatter().get("entity"), Some(&json!("business")));
    assert_eq!(parsed.frontmatter().get("status"), Some(&json!("active")));
    assert_eq!(parsed.frontmatter().get("tags"), Some(&json!(["review"])));
    assert_eq!(parsed.frontmatter().get("priority"), Some(&json!(2)));
    Ok(())
}

#[test]
fn note_creation_defaults_description_summary_and_excerpt_to_empty_strings() -> Result<(), Box<dyn std::error::Error>> {
    let note = NoteDocument::try_from(NoteCreateInput {
        title: "Daily Reflection",
        frontmatter: Map::new(),
        content: "# Daily Reflection\n\nToday was productive.\n",
        timestamp: "2026-07-18T18:30:00+10:00",
        entity: Some("vault"),
    })?;
    let rendered = String::try_from(note)?;
    let parsed = FrontmatterDocument::try_from(rendered.as_str())?;

    assert_eq!(parsed.frontmatter().get("description"), Some(&json!("")));
    assert_eq!(parsed.frontmatter().get("summary"), Some(&json!("")));
    assert_eq!(parsed.frontmatter().get("excerpt"), Some(&json!("")));
    Ok(())
}

#[test]
fn note_creation_rejects_description_identical_to_title() {
    let mut supplied = Map::new();
    supplied.insert("description".to_owned(), json!("Daily Reflection"));

    let result = NoteDocument::try_from(NoteCreateInput {
        title: "Daily Reflection",
        frontmatter: supplied,
        content: "Body.\n",
        timestamp: "2026-07-18T18:30:00+10:00",
        entity: Some("vault"),
    });

    assert!(matches!(
        result,
        Err(NoteError::TitleCollision { field: "description" })
    ));
}

#[test]
fn note_creation_rejects_summary_identical_to_title() {
    let mut supplied = Map::new();
    supplied.insert("summary".to_owned(), json!("Daily Reflection"));

    let result = NoteDocument::try_from(NoteCreateInput {
        title: "Daily Reflection",
        frontmatter: supplied,
        content: "Body.\n",
        timestamp: "2026-07-18T18:30:00+10:00",
        entity: Some("vault"),
    });

    assert!(matches!(result, Err(NoteError::TitleCollision { field: "summary" })));
}

#[test]
fn note_creation_rejects_excerpt_identical_to_title() {
    let mut supplied = Map::new();
    supplied.insert("excerpt".to_owned(), json!("Daily Reflection"));

    let result = NoteDocument::try_from(NoteCreateInput {
        title: "Daily Reflection",
        frontmatter: supplied,
        content: "Body.\n",
        timestamp: "2026-07-18T18:30:00+10:00",
        entity: Some("vault"),
    });

    assert!(matches!(result, Err(NoteError::TitleCollision { field: "excerpt" })));
}

#[test]
fn note_creation_allows_description_summary_and_excerpt_distinct_from_title() -> Result<(), Box<dyn std::error::Error>>
{
    let supplied = serde_json::from_value(json!({
        "description": "A short description",
        "summary": "A short summary",
        "excerpt": "A short excerpt"
    }))?;

    let note = NoteDocument::try_from(NoteCreateInput {
        title: "Daily Reflection",
        frontmatter: supplied,
        content: "Body.\n",
        timestamp: "2026-07-18T18:30:00+10:00",
        entity: Some("vault"),
    })?;
    let rendered = String::try_from(note)?;
    let parsed = FrontmatterDocument::try_from(rendered.as_str())?;

    assert_eq!(
        parsed.frontmatter().get("description"),
        Some(&json!("A short description"))
    );
    assert_eq!(parsed.frontmatter().get("summary"), Some(&json!("A short summary")));
    assert_eq!(parsed.frontmatter().get("excerpt"), Some(&json!("A short excerpt")));
    Ok(())
}

#[test]
fn a_note_with_no_derivable_entity_carries_no_entity_property() -> Result<(), Box<dyn std::error::Error>> {
    let note = NoteDocument::try_from(NoteCreateInput {
        title: "Loose Note",
        frontmatter: Map::new(),
        content: "Body.\n",
        timestamp: "2026-07-18T18:30:00+10:00",
        entity: None,
    })?;
    let rendered = String::try_from(note)?;
    let parsed = FrontmatterDocument::try_from(rendered.as_str())?;

    assert_eq!(parsed.frontmatter().get("entity"), None);
    assert_eq!(parsed.frontmatter().get("status"), Some(&json!("new")));
    Ok(())
}

#[test]
fn a_derived_entity_is_written_in_its_default_position() -> Result<(), Box<dyn std::error::Error>> {
    let note = NoteDocument::try_from(NoteCreateInput {
        title: "Offer Plan",
        frontmatter: Map::new(),
        content: "Body.\n",
        timestamp: "2026-07-18T18:30:00+10:00",
        entity: Some("zenith"),
    })?;
    let rendered = String::try_from(note)?;
    let parsed = FrontmatterDocument::try_from(rendered.as_str())?;
    let keys = parsed.frontmatter().keys().map(String::as_str).collect::<Vec<_>>();

    assert_eq!(keys.get(2), Some(&"entity"));
    assert_eq!(parsed.frontmatter().get("entity"), Some(&json!("zenith")));
    Ok(())
}
