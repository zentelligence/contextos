use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn md_mutation_body_parses_a_patch_with_no_expected_hash() -> TestResult {
    let parsed: MdMutationBody = serde_json::from_str(r#"{"patch": {"status": "active"}}"#)?;
    let MdMutationBody::Frontmatter { patch, expected_hash } = parsed else {
        return Err("expected a Frontmatter variant".into());
    };
    assert_eq!(patch.get("status").and_then(Value::as_str), Some("active"));
    assert_eq!(expected_hash, None);
    Ok(())
}

#[test]
fn md_mutation_body_parses_a_whole_body_replacement() -> TestResult {
    let parsed: MdMutationBody =
        serde_json::from_str(r#"{"body": {"old": "Old text.", "new": "New text."}, "expected_hash": "abc"}"#)?;
    let MdMutationBody::Body { body, expected_hash } = parsed else {
        return Err("expected a Body variant".into());
    };
    assert_eq!(body.old, "Old text.");
    assert_eq!(body.new, "New text.");
    assert_eq!(expected_hash.as_deref(), Some("abc"));
    Ok(())
}

#[test]
fn md_mutation_body_a_payload_carrying_both_fields_resolves_to_frontmatter() {
    // `patch` and `body` are each other's own required field: a real caller
    // never sends both, but an untagged enum tries variants in declaration
    // order, so a payload carrying both still parses deterministically
    // (as `Frontmatter`, ignoring the extra `body` key) rather than
    // panicking or picking ambiguously.
    let result: Result<MdMutationBody, _> = serde_json::from_str(r#"{"patch": {}, "body": {"old": "", "new": ""}}"#);
    assert!(matches!(result, Ok(MdMutationBody::Frontmatter { .. })));
}

#[test]
fn base_mutation_body_row_target_requires_a_note_path_and_patch() -> TestResult {
    let parsed: BaseMutationBody =
        serde_json::from_str(r#"{"target": "row", "note_path": "tasks/one.md", "patch": {"status": "done"}}"#)?;
    let BaseMutationBody::Row { note_path, patch, .. } = parsed else {
        return Err("expected a Row variant".into());
    };
    assert_eq!(note_path, "tasks/one.md");
    assert_eq!(patch.get("status").and_then(Value::as_str), Some("done"));
    Ok(())
}

#[test]
fn base_mutation_body_definition_target_requires_operations() -> TestResult {
    let parsed: BaseMutationBody = serde_json::from_str(
        r#"{"target": "definition", "operations": [{"op": "set_filters", "filter": "status == \"active\""}]}"#,
    )?;
    let BaseMutationBody::Definition { operations, .. } = parsed else {
        return Err("expected a Definition variant".into());
    };
    assert_eq!(operations.len(), 1);
    Ok(())
}

#[test]
fn base_mutation_body_rejects_an_unknown_target() {
    let result: Result<BaseMutationBody, _> = serde_json::from_str(r#"{"target": "mystery", "operations": []}"#);
    assert!(result.is_err());
}

#[test]
fn base_mutation_body_row_and_definition_are_never_conflated_by_shape() {
    // A row body's fields (`note_path`, `patch`) never satisfy a
    // Definition variant's required `operations` field, and vice versa:
    // this is what makes the "never conflated" guarantee structural rather
    // than a runtime check.
    let row: Result<BaseMutationBody, _> =
        serde_json::from_str(r#"{"target": "definition", "note_path": "x.md", "patch": {}}"#);
    assert!(row.is_err());
}
