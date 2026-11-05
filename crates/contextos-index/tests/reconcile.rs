use std::collections::BTreeSet;

use contextos_index::{
    IndexDocument, IndexEntry, IndexEntryKind, IndexFrontmatterInput, IndexReconcileInput, UpdatedStamp,
};
use proptest::prelude::*;

const TIMESTAMP: &str = "2026-09-12T10:00:00+10:00";

fn frontmatter() -> IndexFrontmatterInput<'static> {
    IndexFrontmatterInput {
        entity: Some("vault"),
        timestamp: TIMESTAMP,
        created: None,
        updated: UpdatedStamp::Preserve,
        repair: false,
    }
}

#[test]
fn reconciles_membership_order_and_preserves_operator_content() -> Result<(), Box<dyn std::error::Error>> {
    let source = concat!(
        "# Notes: Index\n\n",
        "Operator introduction.\n\n",
        "<!-- contextos:index:begin -->\n",
        "| Item | Summary |\n",
        "| --- | --- |\n",
        "| [keep.md](keep.md) | Hand-edited summary |\n",
        "| [gone.md](gone.md) | Remove me |\n",
        "<!-- contextos:index:end -->\n\n",
        "Operator footer.\n",
    );
    let input = IndexReconcileInput {
        source: Some(source),
        directory_path: "notes",
        directory_name: "notes",
        frontmatter: frontmatter(),
        entries: vec![
            IndexEntry {
                name: "keep.md".to_owned(),
                kind: IndexEntryKind::File,
                title: "Keep".to_owned(),
                suggested_summary: "Generated replacement".to_owned(),
            },
            IndexEntry {
                name: "sub-folder".to_owned(),
                kind: IndexEntryKind::Directory,
                title: "Sub Folder".to_owned(),
                suggested_summary: "New directory".to_owned(),
            },
        ],
    };

    let document = IndexDocument::try_from(input)?;
    let rendered = String::from(document);

    assert!(rendered.contains("# Notes: Index\n\nOperator introduction.\n\n"));
    assert!(rendered.ends_with("\n\nOperator footer.\n"));
    assert!(rendered.contains("| [[notes/sub-folder/index.md|Sub Folder]] | New directory <!-- auto --> |"));
    assert!(rendered.contains("| [[notes/keep.md|Keep]] | Hand-edited summary |"));
    assert!(!rendered.contains("gone.md"));
    let directory_position = rendered.find("sub-folder/").ok_or("directory row missing")?;
    let file_position = rendered.find("notes/keep.md").ok_or("file row missing")?;
    assert!(directory_position < file_position);
    Ok(())
}

#[test]
fn adds_a_default_frontmatter_block_when_absent() -> Result<(), Box<dyn std::error::Error>> {
    let source = "# Notes: Index\n\n<!-- contextos:index:begin -->\n| Item | Summary |\n| --- | --- |\n<!-- contextos:index:end -->\n";
    let input = IndexReconcileInput {
        source: Some(source),
        directory_path: "notes",
        directory_name: "notes",
        frontmatter: frontmatter(),
        entries: Vec::new(),
    };

    let rendered = String::from(IndexDocument::try_from(input)?);

    assert!(rendered.starts_with("---\n"));
    assert!(rendered.contains("type: index\n"));
    assert!(rendered.contains("title: Notes\n"));
    assert!(rendered.contains("entity: vault\n"));
    assert!(!rendered.contains("status:"), "{rendered}");
    assert!(rendered.contains(&format!("created: {TIMESTAMP}\n")));
    assert!(rendered.contains("\n---\n\n# Notes: Index\n\n"));
    Ok(())
}

#[test]
fn keeps_existing_frontmatter_values_in_place_and_adds_only_the_missing_keys() -> Result<(), Box<dyn std::error::Error>>
{
    let source = concat!(
        "---\n",
        "type: index\n",
        "title: Custom Operator Title\n",
        "description: An operator-authored description.\n",
        "created: 2026-01-01T00:00:00+10:00\n",
        "updated: 2026-01-01T00:00:00+10:00\n",
        "---\n",
        "# Notes: Index\n\n",
        "<!-- contextos:index:begin -->\n",
        "| Item | Summary |\n",
        "| --- | --- |\n",
        "<!-- contextos:index:end -->\n",
    );
    let input = IndexReconcileInput {
        source: Some(source),
        directory_path: "notes",
        directory_name: "notes",
        frontmatter: frontmatter(),
        entries: Vec::new(),
    };

    let rendered = String::from(IndexDocument::try_from(input)?);

    assert!(rendered.starts_with(concat!(
        "---\n",
        "type: index\n",
        "title: Custom Operator Title\n",
        "description: An operator-authored description.\n",
        "created: 2026-01-01T00:00:00+10:00\n",
        "updated: 2026-01-01T00:00:00+10:00\n",
        "entity: vault\n",
        "tags: []\n",
        "aliases: []\n",
        "summary: ''\n",
        "excerpt: ''\n",
        "---\n",
        "# Notes: Index\n\n",
    )));
    assert!(!rendered.contains(TIMESTAMP));
    Ok(())
}

#[test]
fn retains_an_additional_operator_authored_column_across_reconciliation() -> Result<(), Box<dyn std::error::Error>> {
    let source = concat!(
        "# Projects: Index\n\n",
        "<!-- contextos:index:begin -->\n",
        "| Item | Owner | Summary |\n",
        "| --- | --- | --- |\n",
        "| [[projects/alpha.md|Alpha]] | Priya | Existing summary |\n",
        "| [[projects/beta.md|Beta]] | Sam | Beta summary |\n",
        "<!-- contextos:index:end -->\n",
    );
    let input = IndexReconcileInput {
        source: Some(source),
        directory_path: "projects",
        directory_name: "projects",
        frontmatter: frontmatter(),
        entries: vec![
            IndexEntry {
                name: "alpha.md".to_owned(),
                kind: IndexEntryKind::File,
                title: "Alpha".to_owned(),
                suggested_summary: "Freshly derived, should be ignored".to_owned(),
            },
            IndexEntry {
                name: "gamma.md".to_owned(),
                kind: IndexEntryKind::File,
                title: "Gamma".to_owned(),
                suggested_summary: String::new(),
            },
        ],
    };

    let rendered = String::from(IndexDocument::try_from(input)?);

    assert!(rendered.contains("| Item | Owner | Summary |"));
    assert!(rendered.contains("| [[projects/alpha.md|Alpha]] | Priya | Existing summary |"));
    assert!(rendered.contains("| [[projects/gamma.md|Gamma]] |  |  |"));
    assert!(!rendered.contains("Beta"));
    Ok(())
}

#[test]
fn tolerates_the_retired_markdown_link_item_syntax_when_upgrading_a_row() -> Result<(), Box<dyn std::error::Error>> {
    let source = concat!(
        "# Notes: Index\n\n",
        "<!-- contextos:index:begin -->\n",
        "| Item | Summary |\n",
        "| --- | --- |\n",
        "| [keep.md](keep.md) | Hand-edited before this change shipped |\n",
        "<!-- contextos:index:end -->\n",
    );
    let input = IndexReconcileInput {
        source: Some(source),
        directory_path: "notes",
        directory_name: "notes",
        frontmatter: frontmatter(),
        entries: vec![IndexEntry {
            name: "keep.md".to_owned(),
            kind: IndexEntryKind::File,
            title: "Keep".to_owned(),
            suggested_summary: "Should not be used".to_owned(),
        }],
    };

    let rendered = String::from(IndexDocument::try_from(input)?);

    assert!(rendered.contains("| [[notes/keep.md|Keep]] | Hand-edited before this change shipped |"));
    Ok(())
}

#[test]
fn appends_a_managed_block_without_restructuring_existing_prose() -> Result<(), Box<dyn std::error::Error>> {
    let source = "# Bespoke landing page\n\nOperator prose without a final newline.";
    let input = IndexReconcileInput {
        source: Some(source),
        directory_path: "custom",
        directory_name: "custom",
        frontmatter: frontmatter(),
        entries: vec![IndexEntry {
            name: "note.md".to_owned(),
            kind: IndexEntryKind::File,
            title: "Note".to_owned(),
            suggested_summary: "A note".to_owned(),
        }],
    };

    let rendered = String::from(IndexDocument::try_from(input)?);

    assert!(rendered.contains("# Bespoke landing page\n\nOperator prose without a final newline."));
    assert!(rendered.contains("\n\n## Contents\n\n<!-- contextos:index:begin -->"));
    Ok(())
}

#[test]
fn creates_a_complete_index_when_none_exists() -> Result<(), Box<dyn std::error::Error>> {
    let input = IndexReconcileInput {
        source: None,
        directory_path: "client-projects",
        directory_name: "client-projects",
        frontmatter: frontmatter(),
        entries: Vec::new(),
    };

    let rendered = String::from(IndexDocument::try_from(input)?);

    assert!(rendered.starts_with("---\n"));
    assert!(rendered.contains("type: index\n"));
    assert!(rendered.ends_with(concat!(
        "# Client Projects: Index\n\n",
        "<!-- contextos:index:begin -->\n",
        "| Item | Summary |\n",
        "| --- | --- |\n",
        "<!-- contextos:index:end -->\n",
    )));
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 1_000,
        .. ProptestConfig::default()
    })]

    #[test]
    fn operator_bytes_and_hand_edited_summaries_survive_random_sequences(
        introduction in "[A-Za-z0-9 .,;:!?\\n]{0,160}",
        footer in "[A-Za-z0-9 .,;:!?\\n]{0,160}",
        operations in prop::collection::vec((0_u8..16, any::<bool>()), 0..64),
    ) {
        let prefix = format!("# Generated Test: Index\n\n{introduction}\n\n");
        let suffix = format!("\n\n{footer}\n");
        let mut source = format!(
            "{prefix}<!-- contextos:index:begin -->\n\
             | Item | Summary |\n\
             | --- | --- |\n\
             | [anchor.md](anchor.md) | Operator-owned summary |\n\
             <!-- contextos:index:end -->{suffix}"
        );
        let mut dynamic_entries = BTreeSet::new();

        for (entry_number, present) in operations {
            let name = format!("generated-{entry_number}.md");
            if present {
                dynamic_entries.insert(name);
            } else {
                dynamic_entries.remove(&name);
            }
            let mut entries = vec![IndexEntry {
                name: "anchor.md".to_owned(),
                kind: IndexEntryKind::File,
                title: "Anchor".to_owned(),
                suggested_summary: "Must never replace operator text".to_owned(),
            }];
            entries.extend(dynamic_entries.iter().map(|name| IndexEntry {
                name: name.clone(),
                kind: IndexEntryKind::File,
                title: name.clone(),
                suggested_summary: format!("Summary for {name}"),
            }));
            let document = IndexDocument::try_from(IndexReconcileInput {
                source: Some(&source),
                directory_path: "generated-test",
                directory_name: "generated-test",
                frontmatter: frontmatter(),
                entries,
            }).map_err(|error| TestCaseError::fail(error.to_string()))?;
            source = String::from(document);

            prop_assert!(source.contains(&prefix));
            prop_assert!(source.ends_with(&suffix));
            prop_assert!(source.contains(
                "| [[generated-test/anchor.md|Anchor]] | Operator-owned summary |"
            ));
        }
    }
}

#[test]
fn frontmatter_that_cannot_be_edited_line_by_line_is_kept_as_found_while_the_block_is_still_reconciled()
-> Result<(), Box<dyn std::error::Error>> {
    let source = "---\n{type: index, title: Flow Style}\n---\n# Notes: Index\n\nIntro.\n";
    let input = IndexReconcileInput {
        source: Some(source),
        directory_path: "notes",
        directory_name: "notes",
        frontmatter: frontmatter(),
        entries: vec![IndexEntry {
            name: "keep.md".to_owned(),
            kind: IndexEntryKind::File,
            title: "Keep".to_owned(),
            suggested_summary: String::new(),
        }],
    };

    let document = IndexDocument::try_from(input)?;

    assert!(document.uneditable_frontmatter().is_some());
    let rendered = String::from(document);
    assert!(rendered.starts_with("---\n{type: index, title: Flow Style}\n---\n# Notes: Index\n\nIntro.\n"));
    assert!(rendered.contains("[[notes/keep.md|Keep]]"), "{rendered}");
    Ok(())
}

#[test]
fn a_legacy_windows_written_row_keeps_its_hand_written_summary() -> Result<(), Box<dyn std::error::Error>> {
    let source = concat!(
        "---\ntype: index\ntitle: Notes\n---\n# Notes: Index\n\n",
        "<!-- contextos:index:begin -->\n",
        "| Item | Summary | Owner |\n| --- | --- | --- |\n",
        "| [[memory\\wiki/analytics/index.md|Analytics]] | Operator wrote this | Piper |\n",
        "<!-- contextos:index:end -->\n",
    );
    let input = IndexReconcileInput {
        source: Some(source),
        directory_path: "memory/wiki",
        directory_name: "wiki",
        frontmatter: frontmatter(),
        entries: vec![IndexEntry {
            name: "analytics".to_owned(),
            kind: IndexEntryKind::Directory,
            title: "Analytics".to_owned(),
            suggested_summary: "Generated instead".to_owned(),
        }],
    };

    let rendered = String::from(IndexDocument::try_from(input)?);

    assert!(
        rendered.contains("| [[memory/wiki/analytics/index.md|Analytics]] | Operator wrote this | Piper |"),
        "{rendered}"
    );
    assert!(!rendered.contains('\\'), "{rendered}");
    Ok(())
}
