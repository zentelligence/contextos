use super::*;

fn section(text: &str, fragment: &Fragment) -> Result<String, SectionMissing> {
    let found = Section::try_from(SectionInput { text, fragment })?;
    Ok(text.get(found.0).unwrap_or_default().to_owned())
}

const NOTE: &str = "# Top\n\nIntro.\n\n## Goals\n\nGoal text ^g1\n\n### Detail\n\nMore.\n\n## Risks\n\n- one\n- two ^r2\n\n| a |\n| - |\n| 1 |\n\n^tbl\n";

#[test]
fn a_heading_section_runs_to_the_next_heading_of_the_same_or_a_higher_level() -> Result<(), SectionMissing> {
    assert_eq!(
        section(NOTE, &Fragment::Heading("Goals".to_owned()))?,
        "## Goals\n\nGoal text ^g1\n\n### Detail\n\nMore.\n\n"
    );
    assert_eq!(
        section(NOTE, &Fragment::Heading("Detail".to_owned()))?,
        "### Detail\n\nMore.\n\n"
    );
    Ok(())
}

#[test]
fn a_heading_is_matched_by_its_anchor_so_case_and_punctuation_do_not_matter() -> Result<(), SectionMissing> {
    assert_eq!(
        section(NOTE, &Fragment::Heading("goals".to_owned()))?.lines().next(),
        Some("## Goals")
    );
    Ok(())
}

#[test]
fn a_block_is_its_paragraph_its_list_item_or_the_block_before_a_lone_id() -> Result<(), SectionMissing> {
    assert_eq!(section(NOTE, &Fragment::Block("g1".to_owned()))?, "Goal text ^g1\n");
    assert_eq!(section(NOTE, &Fragment::Block("r2".to_owned()))?, "- two ^r2\n");
    assert_eq!(
        section(NOTE, &Fragment::Block("tbl".to_owned()))?,
        "| a |\n| - |\n| 1 |\n"
    );
    Ok(())
}

#[test]
fn a_place_the_note_does_not_have_is_named() {
    assert_eq!(
        section(NOTE, &Fragment::Heading("Nowhere".to_owned())),
        Err(SectionMissing::Heading("Nowhere".to_owned()))
    );
    assert_eq!(
        section(NOTE, &Fragment::Block("zz".to_owned())),
        Err(SectionMissing::Block("zz".to_owned()))
    );
}
