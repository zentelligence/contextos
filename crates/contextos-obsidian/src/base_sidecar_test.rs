use serde_json::json;

use super::*;

const T0: &str = "2026-09-12T10:00:00+10:00";
const T1: &str = "2026-09-12T10:05:00+10:00";
const T2: &str = "2026-09-12T11:00:00+10:00";

fn sidecar(key_prefix: Option<&str>, key_prefixes: &[&str]) -> Result<BaseSidecar, BaseSidecarError> {
    let key_prefixes: Vec<String> = key_prefixes.iter().map(|prefix| (*prefix).to_owned()).collect();
    BaseSidecar::try_from(BaseSidecarInput {
        key_prefix,
        key_prefixes: &key_prefixes,
        timestamp: T0,
    })
}

fn declared(sidecar: &BaseSidecar) -> Vec<(String, u64)> {
    sidecar
        .series()
        .iter()
        .map(|series| (series.prefix().to_owned(), series.last_id()))
        .collect()
}

/// A fixture is always a JSON object literal; anything else yields an empty map, which the
/// calling test's own assertions then fail on.
fn frontmatter(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

// ---------------------------------------------------------------------------
// Creation
// ---------------------------------------------------------------------------

#[test]
fn a_fresh_single_series_sidecar_starts_at_id_zero_with_matching_create_and_update() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(Some("REC"), &[])?;

    assert_eq!(sidecar.default_prefix(), Some("REC"));
    assert_eq!(declared(&sidecar), vec![("REC".to_owned(), 0)]);
    assert_eq!(sidecar.last_create(), T0);
    assert_eq!(sidecar.last_update(), T0);
    Ok(())
}

#[test]
fn a_fresh_sidecar_without_any_prefix_is_filepath_keyed() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(None, &[])?;

    assert_eq!(sidecar.scheme(), &BaseKeyScheme::FilePath);
    assert!(
        sidecar.series().is_empty(),
        "`sidecar.series()` should be empty, got {:?}",
        sidecar.series()
    );
    assert_eq!(sidecar.default_prefix(), None);
    Ok(())
}

#[test]
fn a_fresh_multi_series_sidecar_declares_the_default_first_then_the_others_in_order() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(Some("TASK"), &["DEC", "QUE"])?;

    assert_eq!(sidecar.default_prefix(), Some("TASK"));
    assert_eq!(
        declared(&sidecar),
        vec![("TASK".to_owned(), 0), ("DEC".to_owned(), 0), ("QUE".to_owned(), 0)]
    );
    Ok(())
}

#[test]
fn a_multi_series_sidecar_may_have_no_default_series() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(None, &["SPEC", "NEED", "SCOPE", "OUT"])?;

    assert_eq!(sidecar.default_prefix(), None);
    assert_eq!(sidecar.series().len(), 4);
    Ok(())
}

#[test]
fn the_default_prefix_repeated_in_the_others_is_declared_once() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(Some("TASK"), &["TASK", "DEC"])?;

    assert_eq!(declared(&sidecar), vec![("TASK".to_owned(), 0), ("DEC".to_owned(), 0)]);
    Ok(())
}

#[test]
fn a_prefix_that_is_not_ascii_letters_and_digits_starting_with_a_letter_is_rejected() {
    for invalid in [
        "", "1A", "TASK-B", "TASK/B", "..", "TASK B", "TASK_B", "\u{c4}B", "TASK.md",
    ] {
        let result = sidecar(Some(invalid), &[]);
        assert!(
            matches!(result, Err(BaseSidecarError::InvalidKeyPrefix { .. })),
            "{invalid:?} must be rejected, got {result:?}"
        );

        let result = sidecar(None, &[invalid]);
        assert!(
            matches!(result, Err(BaseSidecarError::InvalidKeyPrefix { .. })),
            "{invalid:?} must be rejected as an additional prefix, got {result:?}"
        );
    }
}

#[test]
fn prefixes_that_differ_only_by_case_are_rejected_because_they_would_share_filenames() {
    let result = sidecar(Some("TASK"), &["task"]);

    assert!(matches!(result, Err(BaseSidecarError::DuplicateKeyPrefix { .. })));
}

// ---------------------------------------------------------------------------
// Choosing a series
// ---------------------------------------------------------------------------

#[test]
fn next_key_zero_pads_to_three_digits_and_increments_from_the_series_counter() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(Some("REC"), &[])?.advance(None, T1)?;

    assert_eq!(sidecar.next_key(None)?.as_deref(), Some("REC-002"));
    Ok(())
}

#[test]
fn next_key_widens_naturally_past_three_digits() -> Result<(), BaseSidecarError> {
    let mut sidecar = sidecar(Some("REC"), &[])?;
    for _ in 0..999 {
        sidecar = sidecar.advance(None, T1)?;
    }

    assert_eq!(sidecar.next_key(None)?.as_deref(), Some("REC-1000"));
    Ok(())
}

#[test]
fn each_series_keeps_its_own_counter() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(Some("TASK"), &["DEC"])?
        .advance(None, T1)?
        .advance(None, T1)?
        .advance(Some("DEC"), T1)?;

    assert_eq!(declared(&sidecar), vec![("TASK".to_owned(), 2), ("DEC".to_owned(), 1)]);
    assert_eq!(sidecar.next_key(None)?.as_deref(), Some("TASK-003"));
    assert_eq!(sidecar.next_key(Some("DEC"))?.as_deref(), Some("DEC-002"));
    Ok(())
}

#[test]
fn omitting_the_prefix_uses_the_default_series_and_naming_it_is_equivalent() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(Some("TASK"), &["DEC"])?;

    assert_eq!(sidecar.next_key(None)?, sidecar.next_key(Some("TASK"))?);
    assert_eq!(sidecar.advance(None, T1)?, sidecar.advance(Some("TASK"), T1)?);
    Ok(())
}

#[test]
fn a_prefix_the_sidecar_has_not_declared_is_rejected_naming_the_declared_ones() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(Some("TASK"), &["DEC"])?;

    for undeclared in ["QUE", "dec", "TASKS", ""] {
        let result = sidecar.next_key(Some(undeclared));
        assert!(
            matches!(
                &result,
                Err(BaseSidecarError::UndeclaredKeyPrefix { requested, declared })
                    if requested == undeclared && declared == &["TASK".to_owned(), "DEC".to_owned()]
            ),
            "{undeclared:?} must be rejected with the declared set, got {result:?}"
        );
        assert!(sidecar.advance(Some(undeclared), T1).is_err());
    }
    Ok(())
}

#[test]
fn omitting_the_prefix_is_rejected_when_there_is_no_default_series() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(None, &["SPEC", "NEED"])?;

    assert!(matches!(
        sidecar.next_key(None),
        Err(BaseSidecarError::KeyPrefixRequired { declared }) if declared == ["SPEC".to_owned(), "NEED".to_owned()]
    ));
    assert_eq!(sidecar.next_key(Some("NEED"))?.as_deref(), Some("NEED-001"));
    Ok(())
}

#[test]
fn a_filepath_sidecar_has_no_next_key_and_rejects_any_prefix() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(None, &[])?.advance(None, T1)?;

    assert_eq!(sidecar.next_key(None)?, None);
    assert_eq!(sidecar.last_create(), T1);
    assert_eq!(sidecar.last_update(), T1);
    assert!(matches!(
        sidecar.next_key(Some("TASK")),
        Err(BaseSidecarError::KeyPrefixNotApplicable { .. })
    ));
    Ok(())
}

#[test]
fn touch_refreshes_only_last_update() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(Some("TASK"), &["DEC"])?.advance(Some("DEC"), T1)?.touch(T2);

    assert_eq!(declared(&sidecar), vec![("TASK".to_owned(), 0), ("DEC".to_owned(), 1)]);
    assert_eq!(sidecar.last_create(), T1);
    assert_eq!(sidecar.last_update(), T2);
    Ok(())
}

// ---------------------------------------------------------------------------
// Frontmatter representation
// ---------------------------------------------------------------------------

#[test]
fn a_multi_series_sidecar_renders_and_restores_through_frontmatter() -> Result<(), Box<dyn std::error::Error>> {
    let original = sidecar(Some("TASK"), &["DEC", "QUE"])?.advance(Some("DEC"), T1)?;

    let rendered = Map::from(&original);

    assert_eq!(rendered.get("key_prefix"), Some(&json!("TASK")));
    assert_eq!(rendered.get("key_format"), Some(&json!("{key_prefix}-{key_id}")));
    assert_eq!(
        rendered.get("key_prefixes"),
        Some(&json!({"TASK": 0, "DEC": 1, "QUE": 0}))
    );
    assert_eq!(rendered.get("last_id"), None, "the counters live only in the map");
    assert_eq!(rendered.get("last_create"), Some(&json!(T1)));
    assert_eq!(BaseSidecar::try_from(&rendered)?, original);
    Ok(())
}

#[test]
fn a_sidecar_with_no_default_renders_no_key_prefix_and_restores() -> Result<(), Box<dyn std::error::Error>> {
    let original = sidecar(None, &["SPEC", "NEED"])?;

    let rendered = Map::from(&original);

    assert_eq!(rendered.get("key_prefix"), None);
    assert_eq!(rendered.get("key_prefixes"), Some(&json!({"SPEC": 0, "NEED": 0})));
    assert_eq!(BaseSidecar::try_from(&rendered)?, original);
    Ok(())
}

#[test]
fn a_filepath_sidecar_renders_unchanged_and_restores() -> Result<(), Box<dyn std::error::Error>> {
    let original = sidecar(None, &[])?;

    let rendered = Map::from(&original);

    assert_eq!(rendered.get("key_prefix"), None);
    assert_eq!(rendered.get("key_prefixes"), None);
    assert_eq!(rendered.get("key_format"), Some(&json!("FILEPATH")));
    assert_eq!(rendered.get("last_id"), Some(&json!(0)));
    assert_eq!(BaseSidecar::try_from(&rendered)?, original);
    Ok(())
}

#[test]
fn a_legacy_single_series_sidecar_reads_as_a_one_entry_map() -> Result<(), Box<dyn std::error::Error>> {
    let legacy = frontmatter(json!({
        "key_prefix": "TASK",
        "key_format": "{key_prefix}-{key_id}",
        "last_id": 159,
        "last_create": T0,
        "last_update": T0,
    }));

    let sidecar = BaseSidecar::try_from(&legacy)?;

    assert_eq!(sidecar.default_prefix(), Some("TASK"));
    assert_eq!(declared(&sidecar), vec![("TASK".to_owned(), 159)]);
    assert_eq!(sidecar.next_key(None)?.as_deref(), Some("TASK-160"));
    Ok(())
}

#[test]
fn writing_a_legacy_sidecar_migrates_it_to_the_map_form_and_removes_last_id() -> Result<(), Box<dyn std::error::Error>>
{
    let legacy = frontmatter(json!({
        "key_prefix": "TASK",
        "key_format": "{key_prefix}-{key_id}",
        "last_id": 159,
        "last_create": T0,
        "last_update": T0,
    }));
    let advanced = BaseSidecar::try_from(&legacy)?.advance(None, T1)?;

    let mut document = legacy;
    apply_patch(&mut document, BaseSidecarPatch::from(&advanced));

    assert_eq!(document.get("key_prefixes"), Some(&json!({"TASK": 160})));
    assert_eq!(
        document.get("last_id"),
        None,
        "the legacy counter is removed once migrated"
    );
    assert_eq!(document.get("key_prefix"), Some(&json!("TASK")));
    assert_eq!(BaseSidecar::try_from(&document)?, advanced);
    Ok(())
}

#[test]
fn a_migration_patch_leaves_every_unrelated_frontmatter_key_alone() -> Result<(), Box<dyn std::error::Error>> {
    let existing = frontmatter(json!({
        "template": "[[tasks|Tasks]]",
        "key_prefix": "TASK",
        "key_format": "{key_prefix}-{key_id}",
        "last_id": 3,
        "last_create": T0,
        "last_update": T0,
        "aliases": ["task counter"],
    }));
    let advanced = BaseSidecar::try_from(&existing)?.advance(None, T1)?;

    let mut document = existing;
    apply_patch(&mut document, BaseSidecarPatch::from(&advanced));

    assert_eq!(document.get("template"), Some(&json!("[[tasks|Tasks]]")));
    assert_eq!(document.get("aliases"), Some(&json!(["task counter"])));
    Ok(())
}

#[test]
fn a_patch_for_a_filepath_sidecar_does_not_delete_last_id() -> Result<(), Box<dyn std::error::Error>> {
    let original = sidecar(None, &[])?;

    let patch = BaseSidecarPatch::from(&original);

    let BaseSidecarPatch(patch) = patch;
    assert_eq!(patch.get("last_id"), None, "a patch neither deletes nor resets it");
    Ok(())
}

#[test]
fn a_patch_for_a_sidecar_with_no_default_removes_a_stale_key_prefix() -> Result<(), Box<dyn std::error::Error>> {
    let original = sidecar(None, &["SPEC", "NEED"])?;

    let BaseSidecarPatch(patch) = BaseSidecarPatch::from(&original);

    assert_eq!(patch.get("key_prefix"), Some(&Value::Null));
    assert_eq!(patch.get("last_id"), Some(&Value::Null));
    Ok(())
}

// ---------------------------------------------------------------------------
// Rejecting malformed sidecars
// ---------------------------------------------------------------------------

fn multi_series_frontmatter() -> Map<String, Value> {
    frontmatter(json!({
        "key_prefix": "TASK",
        "key_format": "{key_prefix}-{key_id}",
        "key_prefixes": {"TASK": 4, "DEC": 2},
        "last_create": T0,
        "last_update": T0,
    }))
}

#[test]
fn a_map_alongside_the_legacy_counter_is_rejected_as_ambiguous() {
    let mut document = multi_series_frontmatter();
    document.insert("last_id".to_owned(), json!(4));

    assert!(matches!(
        BaseSidecar::try_from(&document),
        Err(BaseSidecarError::AmbiguousCounters)
    ));
}

#[test]
fn a_default_prefix_missing_from_the_map_is_rejected() {
    let mut document = multi_series_frontmatter();
    document.insert("key_prefix".to_owned(), json!("ZED"));

    assert!(matches!(
        BaseSidecar::try_from(&document),
        Err(BaseSidecarError::UndeclaredDefaultPrefix { .. })
    ));
}

#[test]
fn an_empty_map_with_no_default_is_rejected() {
    let mut document = multi_series_frontmatter();
    document.shift_remove("key_prefix");
    document.insert("key_prefixes".to_owned(), json!({}));

    assert!(matches!(
        BaseSidecar::try_from(&document),
        Err(BaseSidecarError::EmptyKeyPrefixes)
    ));
}

#[test]
fn a_map_on_a_filepath_sidecar_is_rejected() {
    let document = frontmatter(json!({
        "key_format": "FILEPATH",
        "key_prefixes": {"TASK": 0},
        "last_id": 0,
        "last_create": T0,
        "last_update": T0,
    }));

    assert!(matches!(
        BaseSidecar::try_from(&document),
        Err(BaseSidecarError::FilePathWithKeyPrefixes)
    ));
}

#[test]
fn a_malformed_declared_prefix_or_counter_is_rejected() {
    for (map, expect_wrong_type) in [
        (json!({"BAD-1": 0}), false),
        (json!({"TASK": "four"}), true),
        (json!({"TASK": -1}), true),
        (json!({"TASK": 1.5}), true),
        (json!(["TASK"]), true),
        (json!({"TASK": 0, "task": 0}), false),
    ] {
        let mut document = multi_series_frontmatter();
        document.shift_remove("key_prefix");
        document.insert("key_prefixes".to_owned(), map.clone());

        let result = BaseSidecar::try_from(&document);

        if expect_wrong_type {
            assert!(
                matches!(result, Err(BaseSidecarError::WrongFieldType { field: "key_prefixes" })),
                "{map} must be a wrong-type error, got {result:?}"
            );
        } else {
            assert!(
                matches!(
                    result,
                    Err(BaseSidecarError::InvalidKeyPrefix { .. } | BaseSidecarError::DuplicateKeyPrefix { .. })
                ),
                "{map} must be rejected as a bad prefix, got {result:?}"
            );
        }
    }
}

#[test]
fn a_legacy_sidecar_with_an_unsafe_key_prefix_is_rejected() {
    let legacy = frontmatter(json!({
        "key_prefix": "../x",
        "key_format": "{key_prefix}-{key_id}",
        "last_id": 0,
        "last_create": T0,
        "last_update": T0,
    }));

    assert!(matches!(
        BaseSidecar::try_from(&legacy),
        Err(BaseSidecarError::InvalidKeyPrefix { .. })
    ));
}

#[test]
fn a_missing_key_format_is_rejected() {
    let document = frontmatter(json!({"last_id": 0, "last_create": T0, "last_update": T0}));

    assert!(matches!(
        BaseSidecar::try_from(&document),
        Err(BaseSidecarError::MissingField { field: "key_format" })
    ));
}

#[test]
fn a_sequential_scheme_without_a_prefix_or_map_is_rejected() {
    let document = frontmatter(json!({
        "key_format": "{key_prefix}-{key_id}",
        "last_id": 0,
        "last_create": T0,
        "last_update": T0,
    }));

    assert!(matches!(
        BaseSidecar::try_from(&document),
        Err(BaseSidecarError::MissingField { field: "key_prefix" })
    ));
}

#[test]
fn a_missing_last_create_is_rejected() {
    let document = frontmatter(json!({"key_format": "FILEPATH", "last_id": 0, "last_update": T0}));

    assert!(matches!(
        BaseSidecar::try_from(&document),
        Err(BaseSidecarError::MissingField { field: "last_create" })
    ));
}

#[test]
fn error_codes_separate_a_malformed_sidecar_from_a_bad_request() {
    assert_eq!(BaseSidecarError::AmbiguousCounters.code(), "format/base-sidecar");
    assert_eq!(
        BaseSidecarError::InvalidKeyPrefix {
            prefix: "x-y".to_owned()
        }
        .code(),
        "format/base-sidecar"
    );
    assert_eq!(
        BaseSidecarError::UndeclaredKeyPrefix {
            requested: "QUE".to_owned(),
            declared: vec!["TASK".to_owned()]
        }
        .code(),
        "io/invalid-key-prefix"
    );
    assert_eq!(
        BaseSidecarError::KeyPrefixRequired { declared: vec![] }.code(),
        "io/invalid-key-prefix"
    );
    assert_eq!(
        BaseSidecarError::KeyPrefixNotApplicable {
            requested: "TASK".to_owned()
        }
        .code(),
        "io/invalid-key-prefix"
    );
}

#[test]
fn a_bad_request_error_names_the_declared_prefixes_in_its_message() {
    let error = BaseSidecarError::UndeclaredKeyPrefix {
        requested: "QUE".to_owned(),
        declared: vec!["TASK".to_owned(), "DEC".to_owned()],
    };

    let message = error.to_string();

    assert!(message.contains("\"QUE\""), "{message}");
    assert!(message.contains("TASK, DEC"), "{message}");
}

/// Applies `patch` to `document` through the real frontmatter merge a
/// rewritten sidecar goes through, so these tests exercise its actual RFC 7386
/// behaviour (a null removes a key, an object merges recursively).
fn apply_patch(document: &mut Map<String, Value>, patch: BaseSidecarPatch) {
    let BaseSidecarPatch(patch) = patch;
    let mut merged = crate::FrontmatterDocument::from(crate::FrontmatterInput {
        frontmatter: document.clone(),
        body: String::new(),
    });
    merged.merge_frontmatter(patch);
    document.clone_from(merged.frontmatter());
}

// ---------------------------------------------------------------------------
// Catching a counter up to keys that were assigned outside the tool
// ---------------------------------------------------------------------------

#[test]
fn series_prefix_names_the_series_a_record_would_be_created_under() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(Some("REC"), &["DEC"])?;

    assert_eq!(sidecar.series_prefix(None)?, Some("REC"));
    assert_eq!(sidecar.series_prefix(Some("DEC"))?, Some("DEC"));
    assert!(matches!(
        sidecar.series_prefix(Some("NOPE")),
        Err(BaseSidecarError::UndeclaredKeyPrefix { .. })
    ));
    Ok(())
}

#[test]
fn a_filepath_sidecar_has_no_series_prefix() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(None, &[])?;

    assert_eq!(sidecar.series_prefix(None)?, None);
    assert!(matches!(
        sidecar.series_prefix(Some("REC")),
        Err(BaseSidecarError::KeyPrefixNotApplicable { .. })
    ));
    Ok(())
}

#[test]
fn catching_up_raises_the_chosen_series_counter_and_leaves_every_other_series_alone() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(Some("REC"), &["DEC"])?;

    let caught_up = sidecar.caught_up_to(Some("DEC"), 7)?;

    assert_eq!(declared(&caught_up), vec![("REC".to_owned(), 0), ("DEC".to_owned(), 7)]);
    assert_eq!(caught_up.next_key(Some("DEC"))?, Some("DEC-008".to_owned()));
    assert_eq!(caught_up.next_key(None)?, Some("REC-001".to_owned()));
    Ok(())
}

#[test]
fn catching_up_never_lowers_a_counter() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(Some("REC"), &[])?.advance(None, T1)?.advance(None, T1)?;
    assert_eq!(declared(&sidecar), vec![("REC".to_owned(), 2)]);

    let caught_up = sidecar.caught_up_to(None, 1)?;

    assert_eq!(declared(&caught_up), vec![("REC".to_owned(), 2)]);
    Ok(())
}

#[test]
fn catching_up_is_not_record_activity_so_it_leaves_both_timestamps_alone() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(Some("REC"), &[])?;

    let caught_up = sidecar.caught_up_to(None, 5)?;

    assert_eq!(caught_up.last_create(), T0);
    assert_eq!(caught_up.last_update(), T0);
    Ok(())
}

#[test]
fn catching_up_keeps_the_default_series_and_the_link_fields() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(Some("REC"), &["DEC"])?;

    let caught_up = sidecar.caught_up_to(Some("DEC"), 3)?;

    assert_eq!(caught_up.default_prefix(), Some("REC"));
    assert_eq!(caught_up.link_fields(), sidecar.link_fields());
    Ok(())
}

#[test]
fn catching_up_a_series_that_is_not_declared_is_rejected() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(Some("REC"), &[])?;

    assert!(matches!(
        sidecar.caught_up_to(Some("NOPE"), 3),
        Err(BaseSidecarError::UndeclaredKeyPrefix { .. })
    ));
    Ok(())
}

#[test]
fn catching_up_a_filepath_sidecar_changes_nothing() -> Result<(), BaseSidecarError> {
    let sidecar = sidecar(None, &[])?;

    assert_eq!(sidecar.caught_up_to(None, 9)?, sidecar);
    Ok(())
}
