use super::*;

const TEMPLATE: &str = "---\ndate: 2026-10-31\n---\n\n# Saturday\n\n\
## Morning Intention\n\n> Set before the day moves.\n\n<!-- prep -->\n\n\n\n---\n\n\
## Decisions & commitments\n\n> Any decision made today.\n\n\
| Decision | Notes |\n| -------- | ----- |\n|          |       |\n\n---\n\n\
## Evening Reflection\n\n> Fill at end of day.\n\n**Wins & Successes**\n\n- \n\n**Gratitude:** <!-- Specific -->\n\n1. \n2. \n\n---\n\n\
## Activity Log\n\n> Appended by agent sessions.\n\n<!-- DO NOT EDIT MANUALLY. Agent sessions append below. -->\n";

fn entry(text: &str) -> JournalEntry {
    JournalEntry::from(EntryInput {
        text,
        template: TEMPLATE,
    })
}

fn headings(entry: &JournalEntry) -> Vec<&str> {
    entry
        .sections()
        .iter()
        .map(|section| section.heading.as_str())
        .collect()
}

fn edit(entry: &JournalEntry, index: usize, content: &str) -> Result<String, SectionEditRefused> {
    let heading = entry.sections()[index].heading.clone();
    let base = entry.fingerprint(index).ok_or(SectionEditRefused::Moved)?;
    entry.with_section_replaced(&SectionEdit {
        index,
        heading: &heading,
        base: &base,
        content,
    })
}

#[test]
fn the_sections_are_the_entrys_own_headings_in_the_order_written() {
    let custom = "# Day\n\n## Gym\n\nlegs\n\n## Reading\n\n## Gym\n\narms\n";
    let entry = JournalEntry::from(EntryInput {
        text: custom,
        template: "",
    });
    assert_eq!(headings(&entry), ["Gym", "Reading", "Gym"]);
    assert_eq!(entry.content(0), Some("legs"));
    assert_eq!(entry.content(1), Some(""));
    assert_eq!(entry.content(2), Some("arms"));
    assert_eq!(entry.content(3), None);
}

#[test]
fn a_new_days_entry_keeps_the_templates_guidance_and_rules_out_of_the_editable_content() {
    let entry = entry(TEMPLATE);
    assert_eq!(
        headings(&entry),
        [
            "Morning Intention",
            "Decisions & commitments",
            "Evening Reflection",
            "Activity Log"
        ]
    );
    let morning = &entry.sections()[0];
    assert_eq!(
        morning.guidance.as_deref(),
        Some("> Set before the day moves.\n\n<!-- prep -->")
    );
    assert_eq!(entry.content(0), Some(""));
    assert_eq!(
        entry.content(1),
        Some("| Decision | Notes |\n| -------- | ----- |\n|          |       |")
    );
    assert!(
        entry
            .content(2)
            .is_some_and(|content| content.starts_with("**Wins & Successes**"))
    );
    assert!(entry.content(2).is_some_and(|content| content.ends_with("2.")));
    assert_eq!(entry.content(3), Some(""));
}

#[test]
fn guidance_the_entry_no_longer_matches_is_left_in_the_editable_content() {
    let text = TEMPLATE.replace("> Set before the day moves.", "> My own note, not the template's.");
    let entry = entry(&text);
    assert_eq!(entry.sections()[0].guidance, None);
    assert!(
        entry
            .content(0)
            .is_some_and(|content| content.starts_with("> My own note, not the template's."))
    );
}

#[test]
fn with_no_template_nothing_is_taken_for_guidance() {
    let entry = JournalEntry::from(EntryInput {
        text: TEMPLATE,
        template: "",
    });
    assert!(entry.sections().iter().all(|section| section.guidance.is_none()));
    assert!(
        entry
            .content(0)
            .is_some_and(|content| content.starts_with("> Set before") && content.ends_with("---"))
    );
}

#[test]
fn saving_one_section_leaves_every_other_byte_of_the_file_as_it_was() -> Result<(), SectionEditRefused> {
    let text = TEMPLATE.replace(
        "**Wins & Successes**\n\n- \n",
        "Preamble.\n\n**Wins & Successes**\n\n- [x] won\n  - nested detail\n\n```sh\n## not a heading\n```\n\n**Energy**\n\n7/10\n",
    );
    let before = entry(&text);
    let saved = edit(&before, 0, "- Ship the Journal")?;

    let morning = &before.sections()[0];
    let replaced_start = morning_editable_start(&text);
    let evening_start = text.find("## Decisions").unwrap_or(text.len());
    assert_eq!(&saved[..replaced_start], &text[..replaced_start]);
    let tail = &text[evening_start - "---\n\n".len()..];
    assert!(
        saved.ends_with(tail),
        "everything after the edited section is untouched:\n{saved}"
    );
    assert!(morning.guidance.is_some());

    let after = entry(&saved);
    assert_eq!(headings(&after), headings(&before));
    assert_eq!(after.content(0), Some("- Ship the Journal"));
    assert_eq!(after.sections()[0].guidance, before.sections()[0].guidance);
    for index in 1..before.sections().len() {
        assert_eq!(after.content(index), before.content(index));
    }
    Ok(())
}

fn morning_editable_start(text: &str) -> usize {
    text.find("<!-- prep -->\n")
        .map_or(0, |at| at + "<!-- prep -->\n".len())
}

#[test]
fn saving_the_same_content_again_changes_nothing() -> Result<(), SectionEditRefused> {
    let first = edit(
        &entry(TEMPLATE),
        1,
        "| Decision | Notes |\n| --- | --- |\n| Ship | tests pass |",
    )?;
    let second = edit(
        &entry(&first),
        1,
        "| Decision | Notes |\n| --- | --- |\n| Ship | tests pass |",
    )?;
    assert_eq!(first, second);
    let third = edit(&entry(&second), 0, "")?;
    assert_eq!(third, second, "clearing an already empty section is no change");
    Ok(())
}

#[test]
fn content_after_a_guidance_quote_never_joins_the_quote() -> Result<(), SectionEditRefused> {
    let saved = edit(&entry(TEMPLATE), 3, "plain text that must not continue the quote")?;
    let after = entry(&saved);
    assert!(after.sections()[3].guidance.is_some(), "{saved}");
    assert_eq!(after.content(3), Some("plain text that must not continue the quote"));
    Ok(())
}

#[test]
fn content_before_a_closing_rule_never_turns_into_a_heading() -> Result<(), SectionEditRefused> {
    let saved = edit(&entry(TEMPLATE), 0, "last line before the rule")?;
    let after = entry(&saved);
    assert_eq!(headings(&after), headings(&entry(TEMPLATE)));
    assert_eq!(after.content(0), Some("last line before the rule"));
    Ok(())
}

#[test]
fn clearing_a_section_keeps_its_heading_guidance_and_rule() -> Result<(), SectionEditRefused> {
    let filled = edit(&entry(TEMPLATE), 0, "- something")?;
    let cleared = edit(&entry(&filled), 0, "")?;
    let after = entry(&cleared);
    assert_eq!(after.content(0), Some(""));
    assert!(after.sections()[0].guidance.is_some());
    assert_eq!(headings(&after), headings(&entry(TEMPLATE)));
    Ok(())
}

#[test]
fn submitted_windows_line_endings_and_surrounding_blank_lines_are_not_kept() -> Result<(), SectionEditRefused> {
    let saved = edit(&entry(TEMPLATE), 0, "\r\n\r\n- one\r\n- two\r\n\r\n")?;
    assert!(!saved.contains('\r'));
    assert_eq!(entry(&saved).content(0), Some("- one\n- two"));
    Ok(())
}

#[test]
fn a_save_based_on_an_older_version_of_the_section_is_refused() -> Result<(), SectionEditRefused> {
    let original = entry(TEMPLATE);
    let stale = original.fingerprint(0).ok_or(SectionEditRefused::Moved)?;
    let changed = entry(&edit(&original, 0, "- written elsewhere")?);
    let result = changed.with_section_replaced(&SectionEdit {
        index: 0,
        heading: "Morning Intention",
        base: &stale,
        content: "- mine",
    });
    assert_eq!(result, Err(SectionEditRefused::Changed));
    Ok(())
}

#[test]
fn a_change_to_a_different_section_does_not_refuse_a_save() -> Result<(), SectionEditRefused> {
    let original = entry(TEMPLATE);
    let base = original.fingerprint(0).ok_or(SectionEditRefused::Moved)?;
    let appended = entry(&original.with_item_appended("Activity Log", "session one")?);
    let saved = appended.with_section_replaced(&SectionEdit {
        index: 0,
        heading: "Morning Intention",
        base: &base,
        content: "- mine",
    })?;
    assert!(saved.contains("- session one"));
    assert_eq!(entry(&saved).content(0), Some("- mine"));
    Ok(())
}

#[test]
fn a_save_naming_a_section_that_is_no_longer_at_its_place_is_refused() -> Result<(), SectionEditRefused> {
    let entry = entry(TEMPLATE);
    let base = entry.fingerprint(0).ok_or(SectionEditRefused::Moved)?;
    let moved = entry.with_section_replaced(&SectionEdit {
        index: 0,
        heading: "Evening Reflection",
        base: &base,
        content: "x",
    });
    assert_eq!(moved, Err(SectionEditRefused::Moved));
    let missing = entry.with_section_replaced(&SectionEdit {
        index: 9,
        heading: "Morning Intention",
        base: &base,
        content: "x",
    });
    assert_eq!(missing, Err(SectionEditRefused::Moved));
    Ok(())
}

#[test]
fn a_fingerprint_reads_the_content_as_shown_not_its_surrounding_blank_lines() {
    let tight = JournalEntry::from(EntryInput {
        text: "## A\nbody\n",
        template: "",
    });
    let loose = JournalEntry::from(EntryInput {
        text: "## A\n\n\nbody\n\n\n",
        template: "",
    });
    assert_eq!(tight.fingerprint(0), loose.fingerprint(0));
    assert_ne!(tight.fingerprint(0), entry(TEMPLATE).fingerprint(0));
}

#[test]
fn an_entry_with_no_sections_is_edited_as_one_whole_body() -> Result<(), SectionEditRefused> {
    let text = "---\ntags: [daily]\n---\n# Today\n\nJust writing.\n";
    let whole = JournalEntry::from(EntryInput { text, template: "" });
    assert_eq!(headings(&whole), [""]);
    assert!(whole.sections()[0].whole_body);
    assert_eq!(whole.content(0), Some("# Today\n\nJust writing."));
    let saved = edit(&whole, 0, "# Today\n\nRewritten.")?;
    assert!(saved.starts_with("---\ntags: [daily]\n---\n"), "{saved}");
    assert!(saved.contains("Rewritten."));
    Ok(())
}

#[test]
fn an_appended_item_takes_the_place_of_the_sections_empty_placeholder() {
    let text = TEMPLATE.replace(
        "## Activity Log",
        "## Loops opened today\n\n> Named but not resolved.\n\n- [ ] \n\n---\n\n## Activity Log",
    );
    let once = entry(&text)
        .with_item_appended("loops OPENED today", "First ([[ship-it|Ship it]])")
        .unwrap_or_default();
    let twice = entry(&once)
        .with_item_appended("Loops opened today", "Second")
        .unwrap_or_default();
    let after = entry(&twice);
    let index = after
        .sections()
        .iter()
        .position(|section| section.heading == "Loops opened today")
        .unwrap_or(usize::MAX);
    // The template has no such section, so its quote and rule are content; the items go between them.
    assert!(after.sections()[index].guidance.is_none());
    assert_eq!(
        after.content(index),
        Some("> Named but not resolved.\n\n- First ([[ship-it|Ship it]])\n- Second\n\n---")
    );
}

#[test]
fn an_appended_item_keeps_existing_items_and_the_sections_guidance() {
    let first = entry(TEMPLATE)
        .with_item_appended("Activity Log", "one")
        .unwrap_or_default();
    let second = entry(&first)
        .with_item_appended("Activity Log", "two")
        .unwrap_or_default();
    let after = entry(&second);
    assert_eq!(after.content(3), Some("- one\n- two"));
    assert!(after.sections()[3].guidance.is_some());
}

#[test]
fn an_item_for_a_section_the_entry_lacks_adds_that_section_at_the_end() {
    let appended = entry(TEMPLATE)
        .with_item_appended("Completed actions", "Ship it")
        .unwrap_or_default();
    assert!(appended.starts_with(TEMPLATE), "nothing already written changes");
    let after = entry(&appended);
    assert_eq!(
        after.sections().last().map(|section| section.heading.as_str()),
        Some("Completed actions")
    );
    assert_eq!(after.content(after.sections().len() - 1), Some("- Ship it"));
}

#[test]
fn an_item_for_an_entry_with_no_sections_starts_the_first_one() -> Result<(), SectionEditRefused> {
    for (text, expected) in [
        ("", "## New actions\n\n- Ship it\n"),
        ("# Day\n\nProse.\n", "# Day\n\nProse.\n\n## New actions\n\n- Ship it\n"),
    ] {
        let appended =
            JournalEntry::from(EntryInput { text, template: "" }).with_item_appended("New actions", "Ship it")?;
        assert_eq!(appended, expected);
    }
    Ok(())
}

#[test]
fn an_appended_items_line_breaks_are_flattened_so_it_stays_one_item() {
    let appended = entry(TEMPLATE)
        .with_item_appended("Activity Log", "line one\nline two")
        .unwrap_or_default();
    assert_eq!(entry(&appended).content(3), Some("- line one line two"));
}

#[test]
fn headings_match_trimmed_and_ignoring_case() {
    assert!(same_heading("  Loops Opened Today ", "loops opened today"));
    assert!(!same_heading("Loops opened", "Loops opened today"));
}

#[test]
fn content_that_opens_with_its_own_quote_keeps_the_templates_guidance_held() -> Result<(), SectionEditRefused> {
    let saved = edit(&entry(TEMPLATE), 0, "> A quote of my own.\n\n- then a list")?;
    let after = entry(&saved);
    assert!(after.sections()[0].guidance.is_some(), "{saved}");
    assert_eq!(after.content(0), Some("> A quote of my own.\n\n- then a list"));
    Ok(())
}

#[test]
fn content_that_closes_with_its_own_rule_keeps_the_templates_rule_held() -> Result<(), SectionEditRefused> {
    let saved = edit(&entry(TEMPLATE), 0, "- an item\n\n---")?;
    let after = entry(&saved);
    assert_eq!(after.content(0), Some("- an item\n\n---"), "{saved}");
    Ok(())
}

#[test]
fn a_save_that_would_change_how_later_sections_read_is_refused() {
    let with_comment = TEMPLATE.replace(
        "<!-- DO NOT EDIT MANUALLY. Agent sessions append below. -->",
        "%%a hidden note%%",
    );
    for (text, content) in [
        (TEMPLATE, "```sh\nfoo"),
        (TEMPLATE, "<!-- an unclosed comment"),
        (with_comment.as_str(), "100%% sure"),
    ] {
        let before = entry(text);
        assert_eq!(
            edit(&before, 0, content),
            Err(SectionEditRefused::Unbalanced),
            "{content}"
        );
    }
    // A heading of the content's own is fine: it adds a section and changes nothing after it.
    let added = edit(&entry(TEMPLATE), 0, "- one\n\n## Added\n\n- two");
    assert!(added.is_ok_and(|text| entry(&text).sections().len() == 5));
}

#[test]
fn an_item_that_would_change_how_later_sections_read_is_refused() {
    let with_comment = TEMPLATE.replace(
        "<!-- DO NOT EDIT MANUALLY. Agent sessions append below. -->",
        "%%a hidden note%%",
    );
    let refused = entry(&with_comment).with_item_appended("Morning Intention", "100%% sure");
    assert_eq!(refused, Err(SectionEditRefused::Unbalanced));
}

#[test]
fn a_configured_name_written_with_markup_names_the_heading_it_makes() -> Result<(), SectionEditRefused> {
    assert!(HeadingText::from("Tasks *new*").names("tasks NEW"));
    assert!(HeadingText::from(" `code` [[Link|shown]] ").names("code shown"));
    assert!(!HeadingText::from("Tasks *new*").names("Tasks *new*"));
    let once = entry(TEMPLATE).with_item_appended("Tasks *new*", "first")?;
    let twice = entry(&once).with_item_appended("Tasks *new*", "second")?;
    let after = entry(&twice);
    assert_eq!(after.sections().len(), 5, "{twice}");
    assert_eq!(after.content(4), Some("- first\n- second"));
    Ok(())
}

#[test]
fn text_saved_after_guidance_at_the_very_end_of_the_file_never_joins_it() -> Result<(), SectionEditRefused> {
    let template = "## A\n\n> guide";
    let before = JournalEntry::from(EntryInput {
        text: template,
        template,
    });
    assert!(before.sections()[0].guidance.is_some());
    let base = before.fingerprint(0).ok_or(SectionEditRefused::Moved)?;
    let saved = before.with_section_replaced(&SectionEdit {
        index: 0,
        heading: "A",
        base: &base,
        content: "x",
    })?;
    // A blank line ends the quote; without it, `x` would read as more of the quote.
    assert_eq!(saved, "## A\n\n> guide\n\nx\n");
    let after = JournalEntry::from(EntryInput { text: &saved, template });
    assert!(after.sections()[0].guidance.is_some(), "{saved:?}");
    assert_eq!(after.content(0), Some("x"));
    Ok(())
}

#[test]
fn an_entry_written_with_windows_line_endings_keeps_them() -> Result<(), SectionEditRefused> {
    let crlf = TEMPLATE.replace('\n', "\r\n");
    let crlf_template = crlf.clone();
    let before = JournalEntry::from(EntryInput {
        text: &crlf,
        template: &crlf_template,
    });
    let base = before.fingerprint(0).ok_or(SectionEditRefused::Moved)?;
    let saved = before.with_section_replaced(&SectionEdit {
        index: 0,
        heading: "Morning Intention",
        base: &base,
        content: "- one\n- two",
    })?;
    let appended = JournalEntry::from(EntryInput {
        text: &saved,
        template: &crlf_template,
    })
    .with_item_appended("Activity Log", "three")?;
    for text in [&saved, &appended] {
        assert!(!text.replace("\r\n", "").contains('\n'), "no bare line feed: {text:?}");
    }
    Ok(())
}

#[test]
fn a_fingerprint_is_sixty_four_lowercase_hex_digits() {
    let accepted: Result<SectionFingerprint, _> = serde_json::from_str(&format!("\"{}\"", "a".repeat(64)));
    assert!(accepted.is_ok());
    for refused in ["", "abc", &"A".repeat(64), &"g".repeat(64), &"a".repeat(65)] {
        let parsed: Result<SectionFingerprint, _> = serde_json::from_str(&format!("\"{refused}\""));
        assert!(parsed.is_err(), "{refused}");
    }
}

mod properties {
    use proptest::prelude::*;

    use super::*;

    fn content_line() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("- item".to_owned()),
            Just("- [ ] task".to_owned()),
            Just("1. first".to_owned()),
            Just("> quoted".to_owned()),
            Just("<!-- note -->".to_owned()),
            Just("---".to_owned()),
            Just("```".to_owned()),
            Just("%%".to_owned()),
            Just("<!--".to_owned()),
            Just("## Added".to_owned()),
            Just("| a | b |".to_owned()),
            Just("**Bold label**".to_owned()),
            Just(String::new()),
            "[a-zé✓ ]{1,16}",
        ]
    }

    fn content() -> impl Strategy<Value = String> {
        proptest::collection::vec(content_line(), 0..8).prop_map(|lines| lines.join("\n"))
    }

    proptest! {
        #[test]
        fn an_edit_either_changes_only_its_own_section_or_is_refused(
            index in 0_usize..4,
            submitted in content(),
            other in content(),
        ) {
            // Start from an entry whose other sections already hold content.
            let mut start = entry(TEMPLATE);
            for filled in (0..4).filter(|filled| *filled != index) {
                if let Ok(text) = edit(&start, filled, &other) {
                    start = entry(&text);
                }
            }
            let count = start.sections().len();
            let index = index.min(count - 1);
            let Ok(saved) = edit(&start, index, &submitted) else {
                return Ok(());
            };
            let after = entry(&saved);
            let added = after.sections().len().checked_sub(count);
            prop_assert!(added.is_some(), "a save never removes a section: {}", saved);
            let added = added.unwrap_or_default();
            for earlier in 0..index {
                prop_assert_eq!(after.content(earlier), start.content(earlier));
                prop_assert_eq!(&after.sections()[earlier], &start.sections()[earlier]);
            }
            for later in index + 1..count {
                prop_assert_eq!(after.content(later + added), start.content(later));
                prop_assert_eq!(&after.sections()[later + added].heading, &start.sections()[later].heading);
                prop_assert_eq!(&after.sections()[later + added].guidance, &start.sections()[later].guidance);
            }
            if added == 0 {
                let expected = shown(&submitted).to_owned();
                prop_assert_eq!(after.content(index), Some(expected.as_str()), "{}", saved);
                let again = edit(&after, index, &submitted).map_err(|refused| TestCaseError::fail(refused.to_string()))?;
                prop_assert_eq!(again, saved);
            }
        }
    }
}
