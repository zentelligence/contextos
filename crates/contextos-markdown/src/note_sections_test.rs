use super::*;

fn headings(sections: &NoteSections) -> Vec<&str> {
    sections
        .sections
        .iter()
        .map(|section| section.heading.as_str())
        .collect()
}

#[test]
fn each_top_level_second_level_heading_starts_a_section_that_runs_to_the_next() {
    let text = "---\ndate: 2026-10-31\n---\n\n# Today\n\n## First\n\none\n\n## Second\n\ntwo\n";
    let sections = NoteSections::from(text);
    assert_eq!(headings(&sections), ["First", "Second"]);
    assert_eq!(
        &text[sections.body_start..],
        "\n# Today\n\n## First\n\none\n\n## Second\n\ntwo\n"
    );
    let first = &sections.sections[0];
    assert_eq!(&text[first.range.clone()], "## First\n\none\n\n");
    assert_eq!(&text[first.content.clone()], "\none\n\n");
    let second = &sections.sections[1];
    assert_eq!(&text[second.range.clone()], "## Second\n\ntwo\n");
    assert_eq!(&text[second.content.clone()], "\ntwo\n");
}

#[test]
fn a_note_with_no_second_level_heading_has_no_sections() {
    let text = "---\ntags: [daily]\n---\n# Title\n\nJust prose.\n### Deeper\n";
    let sections = NoteSections::from(text);
    assert!(sections.sections.is_empty(), "{sections:?}");
    assert_eq!(&text[sections.body_start..], "# Title\n\nJust prose.\n### Deeper\n");
}

#[test]
fn a_heading_line_inside_code_or_a_comment_is_not_a_section() {
    let text = "## Real\n\n```sh\n## not a heading\n```\n\n%%\n## hidden\n%%\n\n`%%` and ~~~\n\n~~~\n## fenced\n~~~\n";
    let sections = NoteSections::from(text);
    assert_eq!(headings(&sections), ["Real"]);
    assert_eq!(sections.sections[0].range, 0..text.len());
}

#[test]
fn a_heading_nested_in_a_quote_or_list_and_a_deeper_heading_are_not_sections() {
    let text = "## Outer\n\n> ## quoted\n\n- ## listed\n\n### Deeper\n\n# Top\n\nstill outer\n";
    let sections = NoteSections::from(text);
    assert_eq!(headings(&sections), ["Outer"]);
    assert_eq!(sections.sections[0].content.end, text.len());
}

#[test]
fn frontmatter_is_never_read_as_a_heading_even_when_it_looks_like_one() {
    let text = "---\ntitle: looks like a setext heading\n## a yaml comment\n---\n## Only\n";
    let sections = NoteSections::from(text);
    assert_eq!(headings(&sections), ["Only"]);
}

#[test]
fn a_heading_is_named_by_its_plain_text() {
    let text = "## **Bold** and `code` ##\n\n## Decisions & commitments\n\n##\n";
    let sections = NoteSections::from(text);
    assert_eq!(headings(&sections), ["Bold and code", "Decisions & commitments", ""]);
}

#[test]
fn leading_quotes_and_comments_and_trailing_rules_frame_a_section() {
    let text = "## Morning\n\n> Set before the day moves.\n\n<!-- guidance -->\n\n- first\n\n---\n\n## Next\n";
    let sections = NoteSections::from(text);
    let morning = &sections.sections[0];
    assert_eq!(
        &text[morning.lead.clone()],
        "\n> Set before the day moves.\n\n<!-- guidance -->\n"
    );
    assert_eq!(&text[morning.trail.clone()], "---\n\n");
    assert_eq!(&text[morning.lead.end..morning.trail.start], "\n- first\n\n");
}

#[test]
fn a_section_with_no_frame_has_empty_lead_and_trail_at_its_content_edges() {
    let text = "## Plain\n\nsome text\n> a quote after content is content\n\n---\n\nmore text\n";
    let section = &NoteSections::from(text).sections[0];
    assert_eq!(section.lead, section.content.start..section.content.start);
    assert_eq!(section.trail, section.content.end..section.content.end);
}

#[test]
fn a_section_that_is_only_frame_has_a_lead_and_a_trail_that_meet_without_overlap() {
    let text = "## Log\n\n> Appended by sessions.\n\n<!-- DO NOT EDIT -->\n\n---\n";
    let section = &NoteSections::from(text).sections[0];
    assert!(section.lead.end <= section.trail.start, "{section:?}");
    assert_eq!(&text[section.trail.clone()], "---\n");
    assert!(
        text[section.lead.end..section.trail.start].trim().is_empty(),
        "{section:?}"
    );
}

#[test]
fn every_range_lies_on_character_boundaries_of_the_original_text() {
    let text = "## Café ☕\n\n> ünïcode %%hidden ✓%% quote\n\nbody ✓\n\n---\n## Zwei\n";
    let sections = NoteSections::from(text);
    assert_eq!(headings(&sections), ["Café ☕", "Zwei"]);
    for section in &sections.sections {
        for range in [&section.range, &section.content, &section.lead, &section.trail] {
            assert!(text.get(range.clone()).is_some(), "{range:?}");
        }
    }
}

mod properties {
    use proptest::prelude::*;

    use super::*;

    fn line() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("## Heading".to_owned()),
            Just("##".to_owned()),
            Just("# Title".to_owned()),
            Just("### Deeper".to_owned()),
            Just("---".to_owned()),
            Just("```".to_owned()),
            Just("~~~".to_owned()),
            Just("%%".to_owned()),
            Just("> quote".to_owned()),
            Just("<!-- comment -->".to_owned()),
            Just("- item".to_owned()),
            Just("| a | b |".to_owned()),
            Just(String::new()),
            "[a-zé✓ #>`%-]{0,12}",
        ]
    }

    proptest! {
        #[test]
        fn sections_are_ordered_contiguous_and_framed_within_their_content(
            lines in proptest::collection::vec(line(), 0..40),
            crlf in any::<bool>(),
        ) {
            let text = lines.join(if crlf { "\r\n" } else { "\n" });
            let sections = NoteSections::from(text.as_str());
            prop_assert!(sections.body_start <= text.len());
            let mut previous_end = sections.sections.first().map_or(text.len(), |section| section.range.start);
            prop_assert!(sections.body_start <= previous_end);
            for section in &sections.sections {
                prop_assert_eq!(section.range.start, previous_end);
                prop_assert!(section.range.start <= section.content.start);
                prop_assert_eq!(section.content.end, section.range.end);
                prop_assert_eq!(section.lead.start, section.content.start);
                prop_assert!(section.lead.end <= section.trail.start);
                prop_assert_eq!(section.trail.end, section.content.end);
                for range in [&section.range, &section.content, &section.lead, &section.trail] {
                    prop_assert!(text.get(range.clone()).is_some());
                }
                previous_end = section.range.end;
            }
            prop_assert_eq!(previous_end, text.len());
        }
    }
}
