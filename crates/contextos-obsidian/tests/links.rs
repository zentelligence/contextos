use contextos_obsidian::{LinkCollection, LinkForm, MarkdownError, ValidatedMarkdown};

#[test]
fn reads_wikilinks_and_embeds_in_source_order() -> Result<(), Box<dyn std::error::Error>> {
    let source = concat!(
        "---\n",
        "related: \"[[Frontmatter Note]]\"\n",
        "---\n",
        "[[Plain]] [[Target|Display]] [[Target#Heading]] [[Target#^block-id]]\n",
        "![[image.png|300]]\n",
    );

    let links = LinkCollection::try_from(source)?;

    let read: Vec<(LinkForm, usize, &str, &str)> = links
        .outgoing()
        .iter()
        .map(|link| (link.form, link.line, link.raw.as_str(), link.target.as_str()))
        .collect();
    assert_eq!(
        read,
        [
            (LinkForm::Wikilink, 2, "[[Frontmatter Note]]", "Frontmatter Note"),
            (LinkForm::Wikilink, 4, "[[Plain]]", "Plain"),
            (LinkForm::Wikilink, 4, "[[Target|Display]]", "Target"),
            (LinkForm::Wikilink, 4, "[[Target#Heading]]", "Target"),
            (LinkForm::Wikilink, 4, "[[Target#^block-id]]", "Target"),
            (LinkForm::WikilinkEmbed, 5, "![[image.png|300]]", "image.png"),
        ]
    );
    let outgoing = links.outgoing();
    assert_eq!(outgoing[2].display.as_deref(), Some("Display"));
    assert_eq!(outgoing[3].heading.as_deref(), Some("Heading"));
    assert_eq!(outgoing[4].block.as_deref(), Some("block-id"));
    assert_eq!(outgoing[5].display.as_deref(), Some("300"));
    Ok(())
}

#[test]
fn ignores_code_comments_and_escaped_wikilink_syntax() -> Result<(), Box<dyn std::error::Error>> {
    let source = concat!(
        "`[[Inline Code]]` and \\[[Escaped]]\n",
        "%% [[Commented]] %%\n",
        "```markdown\n[[Fenced Code]]\n```\n",
        "[[Visible]]\n",
    );

    let links = LinkCollection::try_from(source)?;

    assert_eq!(links.outgoing().len(), 1);
    assert_eq!(links.outgoing()[0].target, "Visible");
    Ok(())
}

#[test]
fn rejects_an_unclosed_wikilink_with_its_source_line() {
    let source = "# Note\n\nText ![[broken embed\n";

    let result = LinkCollection::try_from(source);

    assert!(matches!(
        result,
        Err(MarkdownError::UnclosedLink { line: 3, embed: true })
    ));
}

#[test]
fn rejects_empty_wikilink_targets() {
    let result = LinkCollection::try_from("Before [[  ]] after\n");

    assert!(matches!(result, Err(MarkdownError::EmptyLinkTarget { line: 1 })));
}

#[test]
fn accepts_nested_foldable_and_custom_callouts() -> Result<(), Box<dyn std::error::Error>> {
    let source = concat!(
        "> [!warning]- Review\n",
        "> Content with [[Target]].\n",
        "> > [!custom-type]+ Nested\n",
        "> > More content.\n",
    );

    let document = ValidatedMarkdown::try_from(source)?;

    assert_eq!(document.links().outgoing().len(), 1);
    Ok(())
}

#[test]
fn rejects_broken_callout_syntax_with_its_source_line() {
    let result = ValidatedMarkdown::try_from("# Note\n\n> [!warning Missing close\n");

    assert!(matches!(result, Err(MarkdownError::InvalidCallout { line: 3 })));
}

#[test]
fn rejects_an_empty_callout_type() {
    let result = ValidatedMarkdown::try_from("> [!] Empty type\n");

    assert!(matches!(result, Err(MarkdownError::InvalidCallout { line: 1 })));
}
