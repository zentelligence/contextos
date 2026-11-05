use contextos_obsidian::{FrontmatterDocument, NoteError, NoteUpdate, NoteUpdateInput};
use serde_json::{Map, Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const NOTE: &str = "---\nid: VD-001\ntitle: One file per record\nstatus: adopted\ncreated: 2026-08-07T09:00+10:00\nupdated: 2026-08-07T09:00+10:00\n---\n\n# One file per record\n\nOld body.\n";
const STAMP: &str = "2026-10-08T11:30+10:00";

fn patch(value: Value) -> Result<Map<String, Value>, serde_json::Error> {
    serde_json::from_value(value)
}

fn update(source: &str, patch: Map<String, Value>, body: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    let note = NoteUpdate::try_from(NoteUpdateInput {
        document: FrontmatterDocument::try_from(source)?,
        patch,
        body,
        timestamp: STAMP,
    })?;
    Ok(String::try_from(note)?)
}

#[test]
fn one_update_patches_frontmatter_and_replaces_the_body_together() -> TestResult {
    let rendered = update(
        NOTE,
        patch(json!({"status": "superseded", "superseded_by": "VD-002"}))?,
        Some("\n# One file per record\n\nNew body.\n"),
    )?;
    let parsed = FrontmatterDocument::try_from(rendered.as_str())?;

    assert_eq!(parsed.frontmatter().get("status"), Some(&json!("superseded")));
    assert_eq!(parsed.frontmatter().get("superseded_by"), Some(&json!("VD-002")));
    assert_eq!(parsed.body(), "\n# One file per record\n\nNew body.\n");
    Ok(())
}

#[test]
fn created_is_kept_and_updated_is_stamped() -> TestResult {
    let rendered = update(NOTE, patch(json!({"status": "superseded"}))?, None)?;
    let parsed = FrontmatterDocument::try_from(rendered.as_str())?;

    assert_eq!(
        parsed.frontmatter().get("created"),
        Some(&json!("2026-08-07T09:00+10:00"))
    );
    assert_eq!(parsed.frontmatter().get("updated"), Some(&json!(STAMP)));
    let keys = parsed.frontmatter().keys().map(String::as_str).collect::<Vec<_>>();
    assert_eq!(
        keys,
        ["id", "title", "status", "created", "updated"],
        "key order is kept"
    );
    Ok(())
}

#[test]
fn a_body_only_update_keeps_the_frontmatter_and_stamps_updated() -> TestResult {
    let rendered = update(NOTE, Map::new(), Some("\nReplaced.\n"))?;

    assert_eq!(
        rendered,
        "---\nid: VD-001\ntitle: One file per record\nstatus: adopted\ncreated: 2026-08-07T09:00+10:00\nupdated: 2026-10-08T11:30+10:00\n---\n\nReplaced.\n"
    );
    Ok(())
}

#[test]
fn a_patch_may_not_set_or_remove_the_server_maintained_timestamps() -> TestResult {
    for (field, value) in [
        ("created", json!("2020-01-01")),
        ("created", Value::Null),
        ("updated", json!("2020-01-01")),
        ("updated", Value::Null),
    ] {
        let mut fields = Map::new();
        fields.insert(field.to_owned(), value);
        let result = NoteUpdate::try_from(NoteUpdateInput {
            document: FrontmatterDocument::try_from(NOTE)?,
            patch: fields,
            body: None,
            timestamp: STAMP,
        });
        let Err(error) = result else {
            return Err(format!("a patch setting {field} was accepted").into());
        };
        assert_eq!(
            error,
            NoteError::ManagedField {
                field: if field == "created" { "created" } else { "updated" }
            }
        );
        assert_eq!(error.code(), "format/note-managed-field");
    }
    Ok(())
}

#[test]
fn a_patch_may_not_empty_the_title_or_copy_it_into_a_summary_field() -> TestResult {
    for (fields, expected) in [
        (json!({"title": ""}), NoteError::EmptyTitle),
        (json!({"title": null}), NoteError::EmptyTitle),
        (
            json!({"description": "One file per record"}),
            NoteError::TitleCollision { field: "description" },
        ),
        (
            json!({"title": "Same", "summary": "Same"}),
            NoteError::TitleCollision { field: "summary" },
        ),
    ] {
        let result = NoteUpdate::try_from(NoteUpdateInput {
            document: FrontmatterDocument::try_from(NOTE)?,
            patch: patch(fields.clone())?,
            body: None,
            timestamp: STAMP,
        });
        assert_eq!(result.err(), Some(expected), "{fields}");
    }
    Ok(())
}

#[test]
fn an_existing_collision_the_patch_does_not_touch_is_not_a_reason_to_refuse() -> TestResult {
    let source = "---\ntitle: Same\ndescription: Same\n---\nBody\n";
    let rendered = update(source, patch(json!({"status": "open"}))?, None)?;

    assert!(rendered.contains("status: open"), "{rendered}");
    Ok(())
}

#[test]
fn a_body_with_malformed_obsidian_markdown_is_refused() -> TestResult {
    let result = NoteUpdate::try_from(NoteUpdateInput {
        document: FrontmatterDocument::try_from(NOTE)?,
        patch: Map::new(),
        body: Some("See [[unclosed.\n"),
        timestamp: STAMP,
    });

    assert!(matches!(result, Err(NoteError::Markdown(_))));
    Ok(())
}

#[test]
fn a_windows_line_ending_note_is_parsed_and_its_frontmatter_written_with_unix_line_endings() -> TestResult {
    let source = "---\r\ntitle: Windows note\r\nstatus: open\r\n---\r\n\r\nOld.\r\n";
    let rendered = update(source, patch(json!({"status": "resolved"}))?, Some("\nNew.\n"))?;

    assert_eq!(
        rendered,
        "---\ntitle: Windows note\nstatus: resolved\nupdated: 2026-10-08T11:30+10:00\n---\n\nNew.\n"
    );
    Ok(())
}

#[test]
fn a_note_without_frontmatter_gains_a_frontmatter_block() -> TestResult {
    let rendered = update("# Plain\n\nBody.\n", patch(json!({"status": "open"}))?, None)?;

    assert_eq!(
        rendered,
        "---\nstatus: open\nupdated: 2026-10-08T11:30+10:00\n---\n# Plain\n\nBody.\n"
    );
    Ok(())
}

#[test]
fn an_empty_timestamp_is_refused() -> TestResult {
    let result = NoteUpdate::try_from(NoteUpdateInput {
        document: FrontmatterDocument::try_from(NOTE)?,
        patch: patch(json!({"status": "open"}))?,
        body: None,
        timestamp: " ",
    });

    assert_eq!(result.err(), Some(NoteError::EmptyTimestamp));
    Ok(())
}

#[test]
fn a_note_starting_with_a_byte_order_mark_keeps_its_frontmatter_and_the_mark() -> TestResult {
    let source = "\u{feff}---\ntitle: A\ncreated: C\n---\nOld\n";

    let patched = update(source, patch(json!({"status": "x"}))?, None)?;
    assert_eq!(
        patched,
        "\u{feff}---\ntitle: A\ncreated: C\nstatus: x\nupdated: 2026-10-08T11:30+10:00\n---\nOld\n"
    );
    let replaced = update(source, patch(json!({"status": "x"}))?, Some("New\n"))?;
    assert_eq!(
        replaced,
        "\u{feff}---\ntitle: A\ncreated: C\nstatus: x\nupdated: 2026-10-08T11:30+10:00\n---\nNew\n"
    );
    Ok(())
}

#[test]
fn a_title_patch_colliding_with_a_description_already_held_is_refused() -> TestResult {
    let source = "---\ntitle: Old\ndescription: New\n---\nBody\n";
    let result = NoteUpdate::try_from(NoteUpdateInput {
        document: FrontmatterDocument::try_from(source)?,
        patch: patch(json!({"title": "New"}))?,
        body: None,
        timestamp: STAMP,
    });

    assert_eq!(result.err(), Some(NoteError::TitleCollision { field: "description" }));
    Ok(())
}

#[test]
fn a_title_that_is_not_text_is_refused_as_such() -> TestResult {
    let result = NoteUpdate::try_from(NoteUpdateInput {
        document: FrontmatterDocument::try_from(NOTE)?,
        patch: patch(json!({"title": 2026}))?,
        body: None,
        timestamp: STAMP,
    });

    assert_eq!(result.err(), Some(NoteError::TitleNotText));
    Ok(())
}
