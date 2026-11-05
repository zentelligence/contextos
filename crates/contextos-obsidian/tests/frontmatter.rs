use contextos_obsidian::{FrontmatterDocument, FrontmatterError, FrontmatterInput};
use serde_json::{Map, json};

#[test]
fn reads_ordered_yaml_frontmatter_and_preserves_body() -> Result<(), Box<dyn std::error::Error>> {
    let source = "---\ntitle: Example\nstatus: active\ntags:\n  - project\n---\n\n# Body\nText.\n";

    let document = FrontmatterDocument::try_from(source)?;
    let keys = document.frontmatter().keys().map(String::as_str).collect::<Vec<_>>();

    assert_eq!(keys, ["title", "status", "tags"]);
    assert_eq!(document.frontmatter().get("title"), Some(&json!("Example")));
    assert_eq!(document.frontmatter().get("tags"), Some(&json!(["project"])));
    assert_eq!(document.body(), "\n# Body\nText.\n");
    assert_eq!(document.body_start_line(), 7);
    Ok(())
}

#[test]
fn missing_frontmatter_returns_an_empty_object() -> Result<(), Box<dyn std::error::Error>> {
    let source = "# Plain note\n\nBody.\n";

    let document = FrontmatterDocument::try_from(source)?;

    assert!(document.frontmatter().is_empty());
    assert_eq!(document.body(), source);
    assert_eq!(document.body_start_line(), 1);
    Ok(())
}

#[test]
fn malformed_yaml_reports_source_line_and_column_without_repair() -> Result<(), Box<dyn std::error::Error>> {
    let source = "---\ntitle: [broken\n---\nBody\n";

    let result = FrontmatterDocument::try_from(source);

    let (line, column) = match result {
        Err(FrontmatterError::InvalidYaml { line, column, .. }) => (line, column),
        Err(error) => return Err(error.into()),
        Ok(_) => {
            return Err(std::io::Error::other("malformed YAML did not return its precise location").into());
        }
    };
    assert_eq!((line, column), (3, 1));
    Ok(())
}

#[test]
fn empty_frontmatter_returns_an_empty_object() -> Result<(), Box<dyn std::error::Error>> {
    let source = "---\n---\nBody\n";

    let document = FrontmatterDocument::try_from(source)?;

    assert!(document.frontmatter().is_empty());
    assert_eq!(document.body(), "Body\n");
    assert_eq!(document.body_start_line(), 3);
    Ok(())
}

#[test]
fn unclosed_frontmatter_is_rejected_without_consuming_the_body() {
    let source = "---\ntitle: Example\n# Body mistaken for YAML\n";

    let result = FrontmatterDocument::try_from(source);

    assert!(matches!(result, Err(FrontmatterError::Unclosed { line: 1 })));
}

#[test]
fn merge_patch_preserves_existing_key_order_and_body_bytes() -> Result<(), Box<dyn std::error::Error>> {
    let source = concat!(
        "---\n",
        "title: Original\n",
        "status: draft\n",
        "metadata:\n",
        "  owner: Piper\n",
        "  obsolete: true\n",
        "---\n",
        "\n# Body\n\nUntouched.\n",
    );
    let mut document = FrontmatterDocument::try_from(source)?;
    let patch = serde_json::from_value(json!({
        "title": "Revised",
        "status": null,
        "metadata": {"obsolete": null, "priority": 2},
        "entity": "zenith"
    }))?;

    document.apply_merge_patch(patch, "2026-07-18T08:30:00+10:00");
    let rendered = String::try_from(&document)?;
    let reparsed = FrontmatterDocument::try_from(rendered.as_str())?;
    let keys = reparsed.frontmatter().keys().map(String::as_str).collect::<Vec<_>>();

    assert_eq!(keys, ["title", "metadata", "entity", "updated"]);
    assert_eq!(reparsed.frontmatter().get("title"), Some(&json!("Revised")));
    assert_eq!(
        reparsed.frontmatter().get("metadata"),
        Some(&json!({"owner": "Piper", "priority": 2}))
    );
    assert_eq!(
        reparsed.frontmatter().get("updated"),
        Some(&json!("2026-07-18T08:30:00+10:00"))
    );
    assert_eq!(reparsed.body(), "\n# Body\n\nUntouched.\n");
    Ok(())
}

#[test]
fn explicit_updated_value_is_not_replaced() -> Result<(), Box<dyn std::error::Error>> {
    let mut document = FrontmatterDocument::try_from("Body only\n")?;
    let patch = serde_json::from_value(json!({"updated": "operator-value"}))?;

    document.apply_merge_patch(patch, "automatic-value");

    assert_eq!(document.frontmatter().get("updated"), Some(&json!("operator-value")));
    Ok(())
}

#[test]
fn merge_frontmatter_applies_a_patch_without_stamping_a_generic_updated_field() -> Result<(), Box<dyn std::error::Error>>
{
    let source = "---\nkey_id: 1\nlast_update: old\n---\nBody.\n";
    let mut document = FrontmatterDocument::try_from(source)?;
    let patch = serde_json::from_value(json!({"key_id": 2, "last_update": "new"}))?;

    document.merge_frontmatter(patch);

    assert_eq!(document.frontmatter().get("key_id"), Some(&json!(2)));
    assert_eq!(document.frontmatter().get("last_update"), Some(&json!("new")));
    assert_eq!(document.frontmatter().get("updated"), None);
    Ok(())
}

#[test]
fn a_document_can_be_built_directly_from_frontmatter_and_body() -> Result<(), Box<dyn std::error::Error>> {
    let mut frontmatter = Map::new();
    frontmatter.insert("key_format".to_owned(), json!("FILEPATH"));
    let document = FrontmatterDocument::from(FrontmatterInput {
        frontmatter,
        body: "![[example.base|example]]\n".to_owned(),
    });

    let rendered = String::try_from(&document)?;
    let reparsed = FrontmatterDocument::try_from(rendered.as_str())?;

    assert_eq!(reparsed.frontmatter().get("key_format"), Some(&json!("FILEPATH")));
    assert_eq!(reparsed.body(), "![[example.base|example]]\n");
    Ok(())
}

#[test]
fn a_windows_line_ending_note_is_read_and_its_frontmatter_written_with_unix_line_endings()
-> Result<(), Box<dyn std::error::Error>> {
    let source = "---\r\ntitle: Example\r\nstatus: open\r\n---\r\nBody.\n";
    let mut document = FrontmatterDocument::try_from(source)?;

    assert_eq!(document.frontmatter().get("title"), Some(&json!("Example")));
    document.apply_merge_patch(
        serde_json::from_value(json!({"status": "done"}))?,
        "2026-10-08T11:30+10:00",
    );
    assert_eq!(
        String::try_from(&document)?,
        "---\ntitle: Example\nstatus: done\nupdated: 2026-10-08T11:30+10:00\n---\nBody.\n"
    );
    Ok(())
}

#[test]
fn a_byte_order_mark_before_the_frontmatter_is_recognised_and_written_back() -> Result<(), Box<dyn std::error::Error>> {
    let source = "\u{feff}---\ntitle: Example\n---\nBody.\n";
    let mut document = FrontmatterDocument::try_from(source)?;

    assert_eq!(document.frontmatter().get("title"), Some(&json!("Example")));
    assert_eq!(document.body(), "Body.\n");
    assert_eq!(document.body_start_line(), 4);
    document.apply_merge_patch(serde_json::from_value(json!({"status": "done"}))?, "T");
    assert_eq!(
        String::try_from(&document)?,
        "\u{feff}---\ntitle: Example\nstatus: done\nupdated: T\n---\nBody.\n"
    );
    Ok(())
}
