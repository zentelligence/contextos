use contextos_index::{IndexSummary, IndexSummaryInput};

#[test]
fn derives_summary_from_frontmatter_description() -> Result<(), Box<dyn std::error::Error>> {
    let source = concat!(
        "---\n",
        "title: Better Title\n",
        "description: A concise description.\n",
        "---\n",
        "# Ignored Heading\n\n",
        "First sentence explains the note. Second sentence is omitted.\n",
    );

    let summary = IndexSummary::try_from(IndexSummaryInput { source: Some(source) })?;

    assert_eq!(<&str>::from(&summary), "A concise description.");
    Ok(())
}

#[test]
fn falls_back_to_summary_when_description_is_absent() -> Result<(), Box<dyn std::error::Error>> {
    let source = "---\ntitle: Project Alpha\nsummary: Tracks the active delivery work.\n---\nBody.\n";

    let summary = IndexSummary::try_from(IndexSummaryInput { source: Some(source) })?;

    assert_eq!(<&str>::from(&summary), "Tracks the active delivery work.");
    Ok(())
}

#[test]
fn falls_back_to_excerpt_when_description_and_summary_are_absent() -> Result<(), Box<dyn std::error::Error>> {
    let source = "---\ntitle: Project Alpha\nexcerpt: A short excerpt.\n---\nBody.\n";

    let summary = IndexSummary::try_from(IndexSummaryInput { source: Some(source) })?;

    assert_eq!(<&str>::from(&summary), "A short excerpt.");
    Ok(())
}

#[test]
fn is_empty_when_no_frontmatter_summary_property_is_present() -> Result<(), Box<dyn std::error::Error>> {
    let source = "---\ntitle: Project Alpha\n---\n# Project Alpha\n\nTracks the active delivery work!\n";

    let summary = IndexSummary::try_from(IndexSummaryInput { source: Some(source) })?;

    assert_eq!(<&str>::from(&summary), "");
    Ok(())
}

#[test]
fn is_empty_when_there_is_no_source_at_all() -> Result<(), Box<dyn std::error::Error>> {
    let summary = IndexSummary::try_from(IndexSummaryInput { source: None })?;

    assert_eq!(<&str>::from(&summary), "");
    Ok(())
}

#[test]
fn is_empty_when_the_frontmatter_property_is_blank() -> Result<(), Box<dyn std::error::Error>> {
    let source = "---\ntitle: Project Alpha\ndescription: \"   \"\n---\nBody.\n";

    let summary = IndexSummary::try_from(IndexSummaryInput { source: Some(source) })?;

    assert_eq!(<&str>::from(&summary), "");
    Ok(())
}
