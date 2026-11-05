use super::*;
use crate::BaseDocument;
use serde_json::{Map, Value, json};

fn frontmatter(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn active_row<'a>(frontmatter: &'a Map<String, Value>, tags: &'a [String]) -> RowContext<'a> {
    RowContext {
        frontmatter,
        file: FileMetadata {
            name: "alpha.md",
            basename: "alpha",
            path: "notes/alpha.md",
            ext: "md",
            folder: "notes",
            size: 42,
            ctime: Some("2024-01-01 00:00:00.0 +00:00:00"),
            mtime: Some("2024-06-01 00:00:00.0 +00:00:00"),
        },
        tags,
        links: &[],
        embeds: &[],
        backlinks: &[],
    }
}

#[test]
fn equality_and_inequality_match_frontmatter_values() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({ "status": "active" }));
    let row = active_row(&frontmatter, &[]);
    assert!(evaluate_filters(Some(&json!("status == \"active\"")), &row)?);
    assert!(!evaluate_filters(Some(&json!("status != \"active\"")), &row)?);
    assert!(!evaluate_filters(Some(&json!("status == \"archived\"")), &row)?);
    Ok(())
}

#[test]
fn a_note_dot_prefix_resolves_the_same_frontmatter_key_as_the_bare_name() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({ "state": "done" }));
    let row = active_row(&frontmatter, &[]);
    assert!(evaluate_filters(Some(&json!("note.state == \"done\"")), &row)?);
    assert!(!evaluate_filters(Some(&json!("note.state == \"backlog\"")), &row)?);
    assert_eq!(
        resolve_property("note.state", &row, "columns[0]")?,
        resolve_property("state", &row, "columns[0]")?,
    );
    Ok(())
}

#[test]
fn missing_property_compares_as_null() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({}));
    let row = active_row(&frontmatter, &[]);
    assert!(evaluate_filters(Some(&json!("status == null")), &row)?);
    assert!(!evaluate_filters(Some(&json!("status != null")), &row)?);
    Ok(())
}

#[test]
fn contains_matches_a_string_substring_only() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({ "summary": "quarterly review notes" }));
    let row = active_row(&frontmatter, &[]);
    assert!(evaluate_filters(Some(&json!("summary.contains(\"review\")")), &row)?);
    assert!(!evaluate_filters(Some(&json!("summary.contains(\"absent\")")), &row)?);
    Ok(())
}

#[test]
fn has_tag_checks_the_resolved_tag_set() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({}));
    let tags = vec!["project/alpha".to_owned()];
    let row = active_row(&frontmatter, &tags);
    assert!(evaluate_filters(Some(&json!("file.hasTag(\"project/alpha\")")), &row)?);
    assert!(!evaluate_filters(Some(&json!("file.hasTag(\"archived\")")), &row)?);
    Ok(())
}

#[test]
fn file_ext_name_basename_and_path_are_readable_leaf_properties() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({}));
    let row = active_row(&frontmatter, &[]);
    assert!(evaluate_filters(Some(&json!("file.ext == \"md\"")), &row)?);
    assert!(evaluate_filters(Some(&json!("file.name == \"alpha.md\"")), &row)?);
    assert!(evaluate_filters(Some(&json!("file.basename == \"alpha\"")), &row)?);
    assert!(evaluate_filters(Some(&json!("file.path == \"notes/alpha.md\"")), &row)?);
    Ok(())
}

#[test]
fn file_size_ctime_and_mtime_are_readable_equality_properties() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({}));
    let row = active_row(&frontmatter, &[]);
    assert!(evaluate_filters(Some(&json!("file.size == 42")), &row)?);
    assert!(!evaluate_filters(Some(&json!("file.size == 7")), &row)?);
    assert!(evaluate_filters(
        Some(&json!("file.ctime == \"2024-01-01 00:00:00.0 +00:00:00\"")),
        &row
    )?);
    assert!(evaluate_filters(
        Some(&json!("file.mtime == \"2024-06-01 00:00:00.0 +00:00:00\"")),
        &row
    )?);

    let no_timestamps = RowContext {
        frontmatter: &frontmatter,
        file: FileMetadata {
            ctime: None,
            mtime: None,
            ..row.file
        },
        ..row
    };
    assert!(evaluate_filters(Some(&json!("file.ctime == null")), &no_timestamps)?);
    Ok(())
}

#[test]
fn file_tags_links_embeds_and_backlinks_are_list_properties_with_membership_contains()
-> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({}));
    let tags = vec!["project/alpha".to_owned()];
    let links = vec!["Target Note".to_owned()];
    let embeds = vec!["diagram.png".to_owned()];
    let backlinks = vec!["notes/referrer.md".to_owned()];
    let row = RowContext {
        frontmatter: &frontmatter,
        tags: &tags,
        links: &links,
        embeds: &embeds,
        backlinks: &backlinks,
        ..active_row(&frontmatter, &[])
    };
    assert!(evaluate_filters(
        Some(&json!("file.tags.contains(\"project/alpha\")")),
        &row
    )?);
    assert!(!evaluate_filters(
        Some(&json!("file.tags.contains(\"archived\")")),
        &row
    )?);
    assert!(evaluate_filters(
        Some(&json!("file.links.contains(\"Target Note\")")),
        &row
    )?);
    assert!(evaluate_filters(
        Some(&json!("file.embeds.contains(\"diagram.png\")")),
        &row
    )?);
    assert!(evaluate_filters(
        Some(&json!("file.backlinks.contains(\"notes/referrer.md\")")),
        &row
    )?);
    Ok(())
}

#[test]
fn file_properties_exposes_the_whole_frontmatter_map_for_display() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({ "status": "active" }));
    let row = active_row(&frontmatter, &[]);
    let resolved = super::resolve_property("file.properties", &row, "columns[0]")?;
    assert_eq!(resolved, Some(json!({ "status": "active" })));
    Ok(())
}

#[test]
fn file_folder_is_a_readable_leaf_property_derived_from_path() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({}));
    let nested = RowContext {
        frontmatter: &frontmatter,
        file: FileMetadata {
            name: "foo.md",
            basename: "foo",
            path: "memory/tasks/foo.md",
            ext: "md",
            folder: "memory/tasks",
            size: 0,
            ctime: None,
            mtime: None,
        },
        tags: &[],
        links: &[],
        embeds: &[],
        backlinks: &[],
    };
    assert!(evaluate_filters(
        Some(&json!("file.folder == \"memory/tasks\"")),
        &nested
    )?);
    assert!(!evaluate_filters(Some(&json!("file.folder == \"memory\"")), &nested)?);

    let root = RowContext {
        frontmatter: &frontmatter,
        file: FileMetadata {
            name: "root-note.md",
            basename: "root-note",
            path: "root-note.md",
            ext: "md",
            folder: "",
            size: 0,
            ctime: None,
            mtime: None,
        },
        tags: &[],
        links: &[],
        embeds: &[],
        backlinks: &[],
    };
    assert!(evaluate_filters(Some(&json!("file.folder == \"\"")), &root)?);
    assert!(!evaluate_filters(
        Some(&json!("file.folder == \"memory/tasks\"")),
        &root
    )?);
    Ok(())
}

#[test]
fn and_or_not_combine_per_the_obsidian_bases_tree_shape() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({ "status": "active" }));
    let tags = vec!["review".to_owned()];
    let row = active_row(&frontmatter, &tags);
    let filters = json!({
        "and": [
            "file.ext == \"md\"",
            { "or": ["status == \"active\"", { "not": ["file.hasTag(\"archived\")"] }] }
        ]
    });
    assert!(evaluate_filters(Some(&filters), &row)?);

    let excluding = json!({
        "and": [
            "file.ext == \"md\"",
            { "not": ["status == \"active\""] }
        ]
    });
    assert!(!evaluate_filters(Some(&excluding), &row)?);
    Ok(())
}

#[test]
fn string_level_and_or_not_operators_combine_leaves_with_js_precedence() -> Result<(), Box<dyn std::error::Error>> {
    // The real-world shape memory/tasks.base's "Decision pending
    // review" view uses: a single string leaf combining two
    // comparisons with `&&`, previously rejected outright as an
    // unsupported filter expression.
    let decision_frontmatter = frontmatter(json!({ "register": "decision", "completed": null }));
    let decision_row = active_row(&decision_frontmatter, &[]);
    assert!(evaluate_filters(
        Some(&json!("register == \"decision\" && completed == null")),
        &decision_row
    )?);
    assert!(!evaluate_filters(
        Some(&json!("register == \"decision\" && completed == \"2024-01-01\"")),
        &decision_row
    )?);

    // `||` binds looser than `&&`: `a || (b && c)`, not `(a || b) && c`.
    let frontmatter = frontmatter(json!({ "status": "active", "priority": "low" }));
    let row = active_row(&frontmatter, &[]);
    assert!(evaluate_filters(
        Some(&json!(
            "status == \"archived\" || status == \"active\" && priority == \"low\""
        )),
        &row
    )?);
    assert!(!evaluate_filters(
        Some(&json!(
            "status == \"archived\" || status == \"active\" && priority == \"high\""
        )),
        &row
    )?);

    // Explicit `(...)` grouping overrides the default precedence.
    assert!(!evaluate_filters(
        Some(&json!(
            "(status == \"archived\" || status == \"active\") && priority == \"high\""
        )),
        &row
    )?);

    // Unary `!`, and `!=` inside a leaf is never mistaken for it.
    assert!(evaluate_filters(
        Some(&json!("!(status == \"archived\") && priority != \"high\"")),
        &row
    )?);
    Ok(())
}

#[test]
fn unbalanced_parentheses_in_a_string_expression_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({}));
    let row = active_row(&frontmatter, &[]);
    let Err(error) = evaluate_filters(Some(&json!("(status == \"active\"")), &row) else {
        return Err("expected an unclosed group to be rejected".into());
    };
    assert!(matches!(&error, BaseQueryError::UnsupportedFilterExpression { .. }));
    Ok(())
}

#[test]
fn none_filters_matches_every_row() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({}));
    let row = active_row(&frontmatter, &[]);
    assert!(evaluate_filters(None, &row)?);
    Ok(())
}

#[test]
fn a_formula_reference_fails_closed_rather_than_being_ignored() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({}));
    let row = active_row(&frontmatter, &[]);
    let Err(error) = evaluate_filters(Some(&json!("formula.display_status != \"\"")), &row) else {
        return Err("expected formula.display_status to be rejected".into());
    };
    assert!(matches!(&error, BaseQueryError::FormulaReference { name, .. } if name == "display_status"));
    assert_eq!(error.code(), "query/base-query-formula-reference");
    Ok(())
}

#[test]
fn an_unsupported_file_property_is_a_named_error_not_a_silent_frontmatter_lookup()
-> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({}));
    let row = active_row(&frontmatter, &[]);
    let Err(error) = evaluate_filters(Some(&json!("file.frobnicate == \"x\"")), &row) else {
        return Err("expected file.frobnicate to be rejected".into());
    };
    assert!(matches!(&error, BaseQueryError::UnsupportedFileProperty { property, .. } if property == "frobnicate"));
    assert_eq!(error.code(), "query/base-query-unsupported-file-property");
    Ok(())
}

#[test]
fn an_expression_outside_the_documented_grammar_is_rejected_naming_the_expression()
-> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({ "price": 12 }));
    let row = active_row(&frontmatter, &[]);
    let Err(error) = evaluate_filters(Some(&json!("price > 10")), &row) else {
        return Err("expected an unsupported comparator to be rejected".into());
    };
    assert!(
        matches!(&error, BaseQueryError::UnsupportedFilterExpression { expression, .. } if expression == "price > 10")
    );
    assert_eq!(error.code(), "query/base-query-unsupported-filter");
    Ok(())
}

#[test]
fn scan_root_hint_finds_a_file_path_equality_leaf_anywhere_in_the_tree() {
    let filters = json!({
        "and": ["file.path == \"notes/alpha.md\"", "status == \"active\""]
    });
    assert_eq!(
        scan_root_hint(Some(&filters)),
        Some(ScanRootHint::Path("notes/alpha.md"))
    );

    let none = json!("status == \"active\"");
    assert_eq!(scan_root_hint(Some(&none)), None);
    assert_eq!(scan_root_hint(None), None);
}

#[test]
fn scan_root_hint_ignores_contains_since_a_substring_can_match_outside_the_directory() {
    let filters = json!("file.path.contains(\"archive\")");
    assert_eq!(scan_root_hint(Some(&filters)), None);
}

#[test]
fn scan_root_hint_finds_a_file_folder_equality_leaf_and_uses_the_value_directly() {
    let filters = json!("file.folder == \"memory/tasks\"");
    assert_eq!(
        scan_root_hint(Some(&filters)),
        Some(ScanRootHint::Folder("memory/tasks"))
    );

    let contains = json!("file.folder.contains(\"tasks\")");
    assert_eq!(scan_root_hint(Some(&contains)), None);
}

#[test]
fn file_in_folder_matches_the_folder_itself_and_any_subfolder_but_not_a_sibling_with_a_shared_prefix()
-> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({}));
    let exact = RowContext {
        frontmatter: &frontmatter,
        file: FileMetadata {
            folder: "memory/tasks",
            ..active_row(&frontmatter, &[]).file
        },
        ..active_row(&frontmatter, &[])
    };
    assert!(evaluate_filters(
        Some(&json!("file.inFolder(\"memory/tasks\")")),
        &exact
    )?);

    let nested = RowContext {
        file: FileMetadata {
            folder: "memory/tasks/family/finance",
            ..exact.file
        },
        ..exact
    };
    assert!(evaluate_filters(
        Some(&json!("file.inFolder(\"memory/tasks\")")),
        &nested
    )?);

    // "memory/tasks2" shares "memory/tasks" as a string prefix but is a
    // sibling folder, not a subfolder: `file.inFolder` must not treat
    // the two as related the way an unanchored `.contains()` would.
    let sibling = RowContext {
        file: FileMetadata {
            folder: "memory/tasks2",
            ..exact.file
        },
        ..exact
    };
    assert!(!evaluate_filters(
        Some(&json!("file.inFolder(\"memory/tasks\")")),
        &sibling
    )?);
    Ok(())
}

#[test]
fn scan_root_hint_recognises_file_in_folder_as_a_safe_narrowing_hint() {
    let filters = json!("file.inFolder(\"memory/tasks\")");
    assert_eq!(
        scan_root_hint(Some(&filters)),
        Some(ScanRootHint::InFolder("memory/tasks"))
    );
}

#[test]
fn scan_root_hint_never_narrows_through_or_unless_every_operand_agrees() {
    // The real bug this guards: an `or` where only one operand happens
    // to carry a `file.folder`/`file.path` equality leaf must not
    // narrow the scan, since a row could satisfy the `or` through the
    // *other* operand, entirely outside that leaf's directory.
    let disagreeing = json!({
        "or": ["file.folder == \"a\"", "status == \"active\""]
    });
    assert_eq!(scan_root_hint(Some(&disagreeing)), None);

    // Every operand agreeing on the identical hint is still safe.
    let agreeing = json!({
        "or": ["file.folder == \"a\"", "file.folder == \"a\""]
    });
    assert_eq!(scan_root_hint(Some(&agreeing)), Some(ScanRootHint::Folder("a")));
}

#[test]
fn scan_root_hint_never_narrows_through_not() {
    // A hint on the negated operand describes where matches *of that
    // operand* live, not matches of its negation: narrowing here would
    // scan only "a" when the real matches are everywhere outside it.
    let filters = json!({ "not": ["file.folder == \"a\""] });
    assert_eq!(scan_root_hint(Some(&filters)), None);
}

#[test]
fn scan_root_hint_parses_compound_string_expressions_with_the_real_grammar() {
    // `&&`: either conjunct narrowing is safe, regardless of position.
    assert_eq!(
        scan_root_hint(Some(&json!("file.basename != \"index\" && file.folder == \"a\""))),
        Some(ScanRootHint::Folder("a"))
    );

    // The exact real-world shape memory/tasks.base uses: a `||` whose
    // second operand is an unanchored `.contains()` must not narrow,
    // even combined with an outer `&&` and parentheses, and even though
    // a naive whole-string `==`/`!=` split would previously have
    // mis-parsed this compound expression entirely.
    let unsafe_shape = json!(
        "(file.folder == \"memory/tasks\" || file.folder.contains(\"memory/tasks/\")) && file.basename != \"index\""
    );
    assert_eq!(scan_root_hint(Some(&unsafe_shape)), None);

    // The `file.inFolder`-based rewrite of that same intent narrows
    // correctly: it is a single anchored leaf, not an `or`.
    let rewritten = json!("file.inFolder(\"memory/tasks\") && file.basename != \"index\"");
    assert_eq!(
        scan_root_hint(Some(&rewritten)),
        Some(ScanRootHint::InFolder("memory/tasks"))
    );

    // `!`-negated leaves never narrow.
    assert_eq!(
        scan_root_hint(Some(&json!("!(file.folder == \"a\") && status == \"active\""))),
        None
    );
}

#[test]
fn query_definition_resolves_the_named_view_or_defaults_to_the_first() -> Result<(), Box<dyn std::error::Error>> {
    let source = "views:\n  - type: table\n    name: First\n    order: [file.name]\n  - type: table\n    name: Second\n    order: [status]\n";
    let document = BaseDocument::try_from(source)?;

    let first = QueryDefinition::from_document(&document, None)?;
    assert_eq!(first.columns, ["file.name"]);

    let second = QueryDefinition::from_document(&document, Some("Second"))?;
    assert_eq!(second.columns, ["status"]);

    let Err(error) = QueryDefinition::from_document(&document, Some("Missing")) else {
        return Err("expected an unknown view name to be rejected".into());
    };
    assert!(matches!(&error, BaseQueryError::ViewNotFound { name } if name == "Missing"));
    assert_eq!(error.code(), "query/base-view-not-found");
    Ok(())
}

#[test]
fn query_definition_rejects_a_view_with_neither_order_nor_properties() -> Result<(), Box<dyn std::error::Error>> {
    let source = "views:\n  - type: table\n    name: Bare\n";
    let document = BaseDocument::try_from(source)?;

    let Err(error) = QueryDefinition::from_document(&document, None) else {
        return Err("expected a columnless view to be rejected".into());
    };
    assert!(matches!(&error, BaseQueryError::NoColumns { view } if view == "Bare"));
    assert_eq!(error.code(), "query/base-query-no-columns");
    Ok(())
}

#[test]
fn query_definition_from_inline_reads_the_flat_shape_without_a_views_wrapper() -> Result<(), Box<dyn std::error::Error>>
{
    let definition = frontmatter(json!({
        "filters": "status == \"active\"",
        "order": ["file.name", "status"],
        "sort": [{ "property": "file.name", "direction": "DESC" }],
        "limit": 5
    }));

    let resolved = QueryDefinition::from_inline(&definition)?;
    assert_eq!(resolved.columns, ["file.name", "status"]);
    assert_eq!(resolved.sort[0].property, "file.name");
    assert!(resolved.sort[0].descending);
    assert_eq!(resolved.limit, Some(5));
    assert!(resolved.filters.is_some());
    Ok(())
}

#[test]
fn compare_values_orders_missing_then_by_type_then_by_value() {
    assert_eq!(compare_values(None, Some(&json!("a"))), std::cmp::Ordering::Less);
    assert_eq!(
        compare_values(Some(&json!(1)), Some(&json!("a"))),
        std::cmp::Ordering::Less
    );
    assert_eq!(
        compare_values(Some(&json!(1)), Some(&json!(2))),
        std::cmp::Ordering::Less
    );
    assert_eq!(
        compare_values(Some(&json!("a")), Some(&json!("b"))),
        std::cmp::Ordering::Less
    );
}

#[test]
fn a_formula_display_column_with_no_matching_formula_definition_is_an_unevaluated_marker_not_an_error()
-> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({}));
    let row = active_row(&frontmatter, &[]);
    let value = resolve_column("formula.display_status", &row, "columns[0]", &Map::new())?;
    assert_eq!(value, ColumnValue::UnevaluatedFormula("display_status".to_owned()));
    Ok(())
}

#[test]
fn a_formula_body_outside_the_documented_subset_is_an_unevaluated_marker_not_an_error()
-> Result<(), Box<dyn std::error::Error>> {
    let formulas = frontmatter(json!({ "days_old": "(now() - file.ctime).days" }));
    let frontmatter = frontmatter(json!({}));
    let row = active_row(&frontmatter, &[]);
    let value = resolve_column("formula.days_old", &row, "columns[0]", &formulas)?;
    assert_eq!(value, ColumnValue::UnevaluatedFormula("days_old".to_owned()));
    Ok(())
}

#[test]
fn a_bare_property_reference_formula_resolves_to_that_propertys_own_value() -> Result<(), Box<dyn std::error::Error>> {
    let formulas = frontmatter(json!({ "priority_display": "priority", "path_display": "file.path" }));
    let frontmatter = frontmatter(json!({ "priority": "high" }));
    let row = active_row(&frontmatter, &[]);
    assert_eq!(
        resolve_column("formula.priority_display", &row, "columns[0]", &formulas)?,
        ColumnValue::Value(json!("high"))
    );
    assert_eq!(
        resolve_column("formula.path_display", &row, "columns[0]", &formulas)?,
        ColumnValue::Value(json!("notes/alpha.md"))
    );
    Ok(())
}

#[test]
fn file_as_link_resolves_to_a_link_targeting_the_rows_own_file() -> Result<(), Box<dyn std::error::Error>> {
    let formulas = frontmatter(json!({
        "id_link": "file.asLink(note.id)",
        "bare_link": "file.asLink()",
    }));
    let frontmatter = frontmatter(json!({ "id": "a-021" }));
    let row = active_row(&frontmatter, &[]);
    assert_eq!(
        resolve_column("formula.id_link", &row, "columns[0]", &formulas)?,
        ColumnValue::Link {
            target: "notes/alpha.md".to_owned(),
            display: "a-021".to_owned(),
        }
    );
    // No argument: falls back to the file's own basename.
    assert_eq!(
        resolve_column("formula.bare_link", &row, "columns[0]", &formulas)?,
        ColumnValue::Link {
            target: "notes/alpha.md".to_owned(),
            display: "alpha".to_owned(),
        }
    );
    Ok(())
}

#[test]
fn file_as_link_falls_back_to_the_basename_when_its_display_property_is_absent()
-> Result<(), Box<dyn std::error::Error>> {
    let formulas = frontmatter(json!({ "id_link": "file.asLink(note.id)" }));
    let frontmatter = frontmatter(json!({}));
    let row = active_row(&frontmatter, &[]);
    assert_eq!(
        resolve_column("formula.id_link", &row, "columns[0]", &formulas)?,
        ColumnValue::Link {
            target: "notes/alpha.md".to_owned(),
            display: "alpha".to_owned(),
        }
    );
    Ok(())
}

#[test]
fn file_as_link_with_an_argument_outside_the_documented_subset_is_an_unevaluated_marker()
-> Result<(), Box<dyn std::error::Error>> {
    let formulas = frontmatter(json!({ "title_link": "file.asLink(note.title + \"!\")" }));
    let frontmatter = frontmatter(json!({}));
    let row = active_row(&frontmatter, &[]);
    assert_eq!(
        resolve_column("formula.title_link", &row, "columns[0]", &formulas)?,
        ColumnValue::UnevaluatedFormula("title_link".to_owned())
    );
    Ok(())
}

#[test]
fn resolve_row_resolves_frontmatter_file_and_formula_columns_in_order() -> Result<(), Box<dyn std::error::Error>> {
    let formulas = frontmatter(json!({ "title_link": "file.asLink()" }));
    let frontmatter = frontmatter(json!({ "status": "active" }));
    let row = active_row(&frontmatter, &[]);
    let columns = [
        "file.name".to_owned(),
        "status".to_owned(),
        "formula.display_status".to_owned(),
        "formula.title_link".to_owned(),
        "missing".to_owned(),
    ];
    let resolved = resolve_row(&columns, &row, &formulas)?;
    assert_eq!(resolved[0], ColumnValue::Value(json!("alpha.md")));
    assert_eq!(resolved[1], ColumnValue::Value(json!("active")));
    assert_eq!(
        resolved[2],
        ColumnValue::UnevaluatedFormula("display_status".to_owned())
    );
    assert_eq!(
        resolved[3],
        ColumnValue::Link {
            target: "notes/alpha.md".to_owned(),
            display: "alpha".to_owned(),
        }
    );
    assert_eq!(resolved[4], ColumnValue::Value(Value::Null));
    Ok(())
}

#[test]
fn render_table_escapes_pipes_marks_unevaluated_formulas_and_renders_links_as_markdown() {
    let columns = [
        "file.name".to_owned(),
        "formula.display_status".to_owned(),
        "formula.id_link".to_owned(),
    ];
    let rows = vec![vec![
        ColumnValue::Value(json!("a | b")),
        ColumnValue::UnevaluatedFormula("display_status".to_owned()),
        ColumnValue::Link {
            target: "notes/alpha.md".to_owned(),
            display: "a-021".to_owned(),
        },
    ]];
    let table = render(&columns, &rows, QueryFormat::Table);
    assert_eq!(
        table,
        "| file.name | formula.display_status | formula.id_link |\n| --- | --- | --- |\n\
             | a \\| b | formula.display_status (not evaluated) | [a-021](notes/alpha.md) |\n"
    );
}

#[test]
fn render_json_produces_one_object_per_row_keyed_by_column() -> Result<(), Box<dyn std::error::Error>> {
    let columns = ["status".to_owned()];
    let rows = vec![vec![ColumnValue::Value(json!("active"))]];
    let content = render(&columns, &rows, QueryFormat::Json);
    let parsed: Value = serde_json::from_str(&content)?;
    assert_eq!(parsed, json!([{ "status": "active" }]));
    Ok(())
}

#[test]
fn render_json_represents_a_link_column_as_a_typed_object() -> Result<(), Box<dyn std::error::Error>> {
    let columns = ["formula.id_link".to_owned()];
    let rows = vec![vec![ColumnValue::Link {
        target: "notes/alpha.md".to_owned(),
        display: "a-021".to_owned(),
    }]];
    let content = render(&columns, &rows, QueryFormat::Json);
    let parsed: Value = serde_json::from_str(&content)?;
    assert_eq!(
        parsed,
        json!([{ "formula.id_link": { "type": "link", "target": "notes/alpha.md", "display": "a-021" } }])
    );
    Ok(())
}

#[test]
fn render_csv_quotes_fields_containing_commas_or_quotes() {
    let columns = ["title".to_owned()];
    let rows = vec![vec![ColumnValue::Value(json!("hello, \"world\""))]];
    let csv = render(&columns, &rows, QueryFormat::Csv);
    assert_eq!(csv, "title\n\"hello, \"\"world\"\"\"\n");
}

#[test]
fn contains_any_matches_when_a_string_or_list_holds_any_of_the_values() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({ "status": "dropped", "aliases": ["beta", "gamma"] }));
    let row = active_row(&frontmatter, &[]);
    assert!(evaluate_filters(
        Some(&json!("status.containsAny(\"done\", \"dropped\")")),
        &row
    )?);
    assert!(!evaluate_filters(
        Some(&json!("status.containsAny(\"done\", \"closed\")")),
        &row
    )?);
    assert!(evaluate_filters(
        Some(&json!("aliases.containsAny('alpha', 'gamma')")),
        &row
    )?);
    assert!(!evaluate_filters(
        Some(&json!("aliases.containsAny('alpha', 'delta')")),
        &row
    )?);
    Ok(())
}

#[test]
fn a_negated_contains_any_selects_what_holds_none_of_the_values() -> Result<(), Box<dyn std::error::Error>> {
    let open = frontmatter(json!({ "status": "draft" }));
    let closed = frontmatter(json!({ "status": "closed" }));
    let filter = json!("!status.containsAny(\"done\", \"dropped\", \"closed\")");
    assert!(evaluate_filters(Some(&filter), &active_row(&open, &[]))?);
    assert!(!evaluate_filters(Some(&filter), &active_row(&closed, &[]))?);
    Ok(())
}

#[test]
fn contains_all_requires_every_value() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({ "summary": "weekly review notes", "aliases": ["beta", "gamma"] }));
    let row = active_row(&frontmatter, &[]);
    assert!(evaluate_filters(
        Some(&json!("summary.containsAll(\"weekly\", \"review\")")),
        &row
    )?);
    assert!(!evaluate_filters(
        Some(&json!("summary.containsAll(\"weekly\", \"absent\")")),
        &row
    )?);
    assert!(evaluate_filters(
        Some(&json!("aliases.containsAll(\"beta\", \"gamma\")")),
        &row
    )?);
    assert!(!evaluate_filters(
        Some(&json!("aliases.containsAll(\"beta\", \"delta\")")),
        &row
    )?);
    Ok(())
}

#[test]
fn a_value_list_that_is_empty_or_not_all_quoted_is_rejected_naming_the_expression() {
    let frontmatter = frontmatter(json!({ "status": "draft" }));
    let row = active_row(&frontmatter, &[]);
    for expression in [
        "status.containsAny()",
        "status.containsAny(\"done\", dropped)",
        "status.containsAll(\"done\",)",
    ] {
        let result = evaluate_filters(Some(&json!(expression)), &row);
        assert!(
            matches!(result, Err(BaseQueryError::UnsupportedFilterExpression { ref expression, .. }) if expression.contains("contains")),
            "{expression}: {result:?}"
        );
    }
}

#[test]
fn a_comma_inside_a_quoted_value_belongs_to_that_value() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = frontmatter(json!({ "title": "Fish, chips" }));
    let row = active_row(&frontmatter, &[]);
    assert!(evaluate_filters(
        Some(&json!("title.containsAny(\"Fish, chips\", \"Pie\")")),
        &row
    )?);
    Ok(())
}

#[test]
fn validating_a_filter_reports_an_unsupported_construct_without_any_row() {
    let filters = json!({ "and": ["state == \"next\"", "due > \"2026-01-01\""] });
    let result = validate_filters(Some(&filters));
    assert!(
        matches!(result, Err(BaseQueryError::UnsupportedFilterExpression { ref path, ref expression }) if path == "filters.and[1]" && expression.contains("due >")),
        "{result:?}"
    );
}

#[test]
fn validating_a_supported_filter_succeeds() -> Result<(), Box<dyn std::error::Error>> {
    validate_filters(Some(&json!(
        "file.inFolder(\"memory/tasks\") && !state.containsAny(\"done\", \"dropped\") && file.basename != \"index\""
    )))?;
    validate_filters(None)?;
    Ok(())
}
