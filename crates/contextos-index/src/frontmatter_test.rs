use time::OffsetDateTime;
use time::macros::datetime;

use super::*;

const NOW: &str = "2026-09-28 9:00:00.0 +10:00:00";

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn settings<'a>() -> IndexFrontmatterInput<'a> {
    IndexFrontmatterInput {
        entity: Some("zenith"),
        timestamp: NOW,
        created: None,
        updated: UpdatedStamp::Preserve,
        repair: false,
    }
}

fn reconcile(
    prefix: Option<&str>,
    settings: &IndexFrontmatterInput<'_>,
) -> Result<FrontmatterReconciliation, IndexError> {
    FrontmatterReconciliation::try_from(FrontmatterReconcileInput {
        prefix,
        title: "Analytics",
        path: "wiki/analytics/index.md",
        settings,
    })
}

fn reconcile_error(
    prefix: &str,
    settings: &IndexFrontmatterInput<'_>,
) -> Result<IndexError, Box<dyn std::error::Error>> {
    match reconcile(Some(prefix), settings) {
        Ok(_) => Err("the frontmatter should have been rejected".into()),
        Err(error) => Ok(error),
    }
}

fn complete(entity: &str, status: Option<&str>, created: &str, updated: &str) -> String {
    let status = status.map_or_else(String::new, |status| format!("status: {status}\n"));
    format!(
        "---\ntype: index\ntitle: Analytics\nentity: {entity}\n{status}created: {created}\nupdated: {updated}\n\
         tags: []\naliases: []\ndescription: ''\nsummary: ''\nexcerpt: ''\n---\n"
    )
}

#[test]
fn a_new_index_gets_the_complete_default_set_without_a_status() -> TestResult {
    let result = reconcile(None, &settings())?;

    assert_eq!(result.block, format!("{}\n", complete("zenith", None, NOW, NOW)));
    assert!(
        result.repairs.is_empty(),
        "`result.repairs` should be empty, got {:?}",
        result.repairs
    );
    Ok(())
}

#[test]
fn a_new_index_omits_the_entity_when_no_roster_is_available() -> TestResult {
    let result = reconcile(
        None,
        &IndexFrontmatterInput {
            entity: None,
            ..settings()
        },
    )?;

    assert!(!result.block.contains("entity"), "{}", result.block);
    Ok(())
}

#[test]
fn a_new_index_uses_an_authoritative_creation_date_over_the_rebuild_timestamp() -> TestResult {
    let result = reconcile(
        None,
        &IndexFrontmatterInput {
            created: Some(datetime!(2026-01-02 3:04:05 +10:00)),
            ..settings()
        },
    )?;

    assert!(
        result.block.contains("created: 2026-01-02T03:04:00+10:00\n"),
        "{}",
        result.block
    );
    assert!(result.block.contains(&format!("updated: {NOW}\n")), "{}", result.block);
    Ok(())
}

#[test]
fn complete_frontmatter_is_returned_byte_for_byte() -> TestResult {
    let prefix = concat!(
        "---\n# operator comment\ntype: index\ntitle: \"Analytics\"\nentity: personal\nstatus: new\n",
        "created: 2020-01-01\nupdated: 2020-02-02\ntags: [a, b]\naliases: []\ndescription: ''\nsummary: ''\n",
        "excerpt: ''\ncssclasses:\n  - wide\n---\n\n"
    );

    let result = reconcile(Some(prefix), &settings())?;

    assert_eq!(result.block, prefix);
    assert!(
        result.repairs.is_empty(),
        "`result.repairs` should be empty, got {:?}",
        result.repairs
    );
    Ok(())
}

#[test]
fn partial_frontmatter_gains_only_its_missing_keys_and_keeps_existing_text() -> TestResult {
    let prefix = "---\ntitle: 'Analytics'\n# kept\ntags:\n- one\n- two\n---\n";

    let result = reconcile(Some(prefix), &settings())?;

    assert_eq!(
        result.block,
        format!(
            "---\ntitle: 'Analytics'\n# kept\ntags:\n- one\n- two\ntype: index\nentity: zenith\ncreated: {NOW}\n\
             updated: {NOW}\naliases: []\ndescription: ''\nsummary: ''\nexcerpt: ''\n---\n"
        )
    );
    Ok(())
}

#[test]
fn an_existing_wrong_entity_is_preserved_when_not_repairing() -> TestResult {
    let prefix = complete("personal", None, NOW, NOW);

    let result = reconcile(Some(&prefix), &settings())?;

    assert_eq!(result.block, prefix);
    Ok(())
}

#[test]
fn stamping_replaces_the_existing_updated_value_and_nothing_else() -> TestResult {
    let prefix = complete("zenith", None, "2020-01-01", "2020-02-02");

    let result = reconcile(
        Some(&prefix),
        &IndexFrontmatterInput {
            updated: UpdatedStamp::Stamp,
            ..settings()
        },
    )?;

    assert_eq!(
        result.block,
        prefix.replace("updated: 2020-02-02", &format!("updated: {NOW}"))
    );
    Ok(())
}

#[test]
fn stamping_keeps_multi_line_values_beside_the_replaced_key_intact() -> TestResult {
    let prefix = "---\ntype: index\ntitle: Analytics\nentity: zenith\ncreated: 2020-01-01\nupdated: 2020-02-02\n\
                  tags:\n- a\n- b\naliases: []\ndescription: ''\nsummary: ''\nexcerpt: ''\n---\n";

    let result = reconcile(
        Some(prefix),
        &IndexFrontmatterInput {
            updated: UpdatedStamp::Stamp,
            ..settings()
        },
    )?;

    assert_eq!(
        result.block,
        prefix.replace("updated: 2020-02-02", &format!("updated: {NOW}"))
    );
    Ok(())
}

#[test]
fn stamping_appends_updated_when_the_frontmatter_has_none() -> TestResult {
    let prefix = complete("zenith", None, "2020-01-01", "2020-02-02").replace("updated: 2020-02-02\n", "");

    let result = reconcile(
        Some(&prefix),
        &IndexFrontmatterInput {
            updated: UpdatedStamp::Stamp,
            ..settings()
        },
    )?;

    assert_eq!(
        result.block,
        prefix.replace("excerpt: ''\n---\n", &format!("excerpt: ''\nupdated: {NOW}\n---\n"))
    );
    Ok(())
}

#[test]
fn a_carriage_return_line_ending_style_is_kept_for_added_keys() -> TestResult {
    let prefix = "---\r\ntype: index\r\ntitle: Analytics\r\n---\r\n";

    let result = reconcile(Some(prefix), &settings())?;

    assert!(
        result
            .block
            .starts_with("---\r\ntype: index\r\ntitle: Analytics\r\nentity: zenith\r\n")
    );
    assert!(!result.block.replace("\r\n", "").contains('\n'), "{:?}", result.block);
    Ok(())
}

#[test]
fn repair_rederives_an_entity_that_disagrees_with_the_location() -> TestResult {
    let prefix = complete("personal", Some("published"), NOW, NOW);

    let result = reconcile(
        Some(&prefix),
        &IndexFrontmatterInput {
            repair: true,
            ..settings()
        },
    )?;

    assert_eq!(result.block, prefix.replace("entity: personal", "entity: zenith"));
    assert_eq!(
        result.repairs,
        vec![IndexRepairAction::EntityRederived {
            from: "personal".to_owned(),
            to: "zenith".to_owned(),
        }]
    );
    Ok(())
}

#[test]
fn repair_leaves_a_matching_entity_and_an_unknown_entity_alone() -> TestResult {
    let prefix = complete("zenith", None, NOW, NOW);
    let repairing = IndexFrontmatterInput {
        repair: true,
        ..settings()
    };
    let unknown = IndexFrontmatterInput {
        entity: None,
        repair: true,
        ..settings()
    };
    let personal = prefix.replace("zenith", "personal");

    assert_eq!(reconcile(Some(&prefix), &repairing)?.block, prefix);
    assert_eq!(reconcile(Some(&personal), &unknown)?.block, personal);
    Ok(())
}

#[test]
fn repair_removes_a_status_of_new_but_keeps_any_other_status() -> TestResult {
    let new = complete("zenith", Some("new"), NOW, NOW);
    let published = complete("zenith", Some("published"), NOW, NOW);
    let repairing = IndexFrontmatterInput {
        repair: true,
        ..settings()
    };

    let removed = reconcile(Some(&new), &repairing)?;
    let kept = reconcile(Some(&published), &repairing)?;

    assert_eq!(removed.block, new.replace("status: new\n", ""));
    assert_eq!(removed.repairs, vec![IndexRepairAction::StatusRemoved]);
    assert_eq!(kept.block, published);
    assert!(
        kept.repairs.is_empty(),
        "`kept.repairs` should be empty, got {:?}",
        kept.repairs
    );
    Ok(())
}

#[test]
fn repair_resets_a_created_date_that_only_repeats_updated_when_a_real_source_reports_an_earlier_one() -> TestResult {
    let prefix = complete("zenith", None, NOW, NOW);
    let source = datetime!(2025-05-06 7:08:09 +10:00);

    let reset = reconcile(
        Some(&prefix),
        &IndexFrontmatterInput {
            created: Some(source),
            repair: true,
            ..settings()
        },
    )?;
    let no_source = reconcile(
        Some(&prefix),
        &IndexFrontmatterInput {
            repair: true,
            ..settings()
        },
    )?;

    assert_eq!(reset.block, complete("zenith", None, "2025-05-06T07:08:00+10:00", NOW));
    assert_eq!(
        reset.repairs,
        vec![IndexRepairAction::CreatedReset {
            from: NOW.to_owned(),
            to: "2025-05-06T07:08:00+10:00".to_owned(),
        }]
    );
    assert_eq!(no_source.block, prefix);
    Ok(())
}

#[test]
fn repair_never_moves_created_later_even_when_a_source_disagrees() -> TestResult {
    let prefix = complete(
        "zenith",
        None,
        "2026-03-01 9:00:00.0 +10:00:00",
        "2026-03-01 9:00:00.0 +10:00:00",
    );

    let result = reconcile(
        Some(&prefix),
        &IndexFrontmatterInput {
            created: Some(datetime!(2026-09-28 9:00:00 +10:00)),
            repair: true,
            ..settings()
        },
    )?;

    assert_eq!(result.block, prefix);
    assert!(
        result.repairs.is_empty(),
        "`result.repairs` should be empty, got {:?}",
        result.repairs
    );
    Ok(())
}

#[test]
fn repair_leaves_a_created_it_cannot_read_as_a_date_alone() -> TestResult {
    let prefix = complete("zenith", None, "last spring", "last spring");

    let result = reconcile(
        Some(&prefix),
        &IndexFrontmatterInput {
            created: Some(datetime!(2025-05-06 7:08:09 +10:00)),
            repair: true,
            ..settings()
        },
    )?;

    assert_eq!(result.block, prefix);
    Ok(())
}

#[test]
fn a_stored_timestamp_reads_back_in_the_iso_and_the_display_form() {
    let expected = datetime!(2026-09-28 9:00:00 +10:00);

    let stored = |text: &str| StoredTimestamp::try_from(text).ok().map(OffsetDateTime::from);

    assert_eq!(stored("2026-09-28T09:00:00+10:00"), Some(expected));
    assert_eq!(stored(NOW), Some(expected));
    assert_eq!(stored("2026-09-28 09:00:00.0 +10:00:00"), Some(expected));
    assert_eq!(stored("2026-09-28"), None);
    assert_eq!(stored(&expected.to_string()), Some(expected));
}

#[test]
fn repair_that_needs_a_line_edit_of_flow_style_frontmatter_is_rejected_rather_than_rewritten() -> TestResult {
    let prefix = "---\n{type: index, title: Analytics, entity: personal}\n---\n";

    let error = reconcile_error(
        prefix,
        &IndexFrontmatterInput {
            repair: true,
            ..settings()
        },
    )?;

    assert!(matches!(error, IndexError::FrontmatterLayout { .. }), "{error:?}");
    Ok(())
}

#[test]
fn malformed_yaml_is_rejected_with_the_frontmatter_error() -> TestResult {
    let error = reconcile_error("---\ntype: [unclosed\n---\n", &settings())?;

    assert!(matches!(error, IndexError::DirectoryFrontmatter { .. }), "{error:?}");
    Ok(())
}

#[test]
fn stamping_and_status_removal_keep_a_carriage_return_line_ending_style() -> TestResult {
    let prefix = complete("zenith", Some("new"), "2020-01-01", "2020-02-02").replace('\n', "\r\n");

    let result = reconcile(
        Some(&prefix),
        &IndexFrontmatterInput {
            updated: UpdatedStamp::Stamp,
            repair: true,
            ..settings()
        },
    )?;

    assert_eq!(
        result.block,
        prefix
            .replace("status: new\r\n", "")
            .replace("updated: 2020-02-02", &format!("updated: {NOW}"))
    );
    assert!(!result.block.replace("\r\n", "").contains('\n'), "{:?}", result.block);
    Ok(())
}
