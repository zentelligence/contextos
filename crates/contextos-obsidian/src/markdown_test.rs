use super::*;

fn links(source: &str) -> Vec<ObsidianLink> {
    LinkCollection::try_from(source)
        .map(|collection| collection.outgoing().to_vec())
        .unwrap_or_default()
}

fn targets(source: &str) -> Vec<String> {
    links(source).into_iter().map(|link| link.target).collect()
}

fn forms(source: &str) -> Vec<LinkForm> {
    links(source).into_iter().map(|link| link.form).collect()
}

#[test]
fn every_link_form_is_recognised_with_its_line_and_raw_text() {
    let source = "[[a]]\n![[b.png]]\ntext [c](c.md) and ![d](d.png \"Title\")\n";

    let found = links(source);

    assert_eq!(
        found
            .iter()
            .map(|link| (link.form, link.line, link.raw.as_str(), link.target.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (LinkForm::Wikilink, 1, "[[a]]", "a"),
            (LinkForm::WikilinkEmbed, 2, "![[b.png]]", "b.png"),
            (LinkForm::Markdown, 3, "[c](c.md)", "c.md"),
            (LinkForm::MarkdownEmbed, 3, "![d](d.png \"Title\")", "d.png"),
        ]
    );
}

#[test]
fn a_markdown_link_keeps_its_text_as_display_and_drops_the_heading_and_block_from_the_target() {
    let found = links("[the text](notes/n.md#Heading) [t](n.md#^abc)\n");

    assert_eq!(found[0].target, "notes/n.md");
    assert_eq!(found[0].display.as_deref(), Some("the text"));
    assert_eq!(found[0].heading.as_deref(), Some("Heading"));
    assert_eq!(found[1].block.as_deref(), Some("abc"));
}

#[test]
fn url_encoding_is_decoded_and_angle_brackets_are_syntax_in_a_markdown_destination() {
    assert_eq!(
        targets("[x](my%20note.md) [y](<my note.md>) [z](<dir/a (1).md>)\n"),
        ["my note.md", "my note.md", "dir/a (1).md"]
    );
}

#[test]
fn a_destination_may_be_vault_absolute_relative_or_a_folder_and_keeps_that_shape() {
    assert_eq!(
        targets("[a](/memory/raw/a) [b](../raw/2026/) [c](./sibling)\n"),
        ["/memory/raw/a", "../raw/2026/", "./sibling"]
    );
}

#[test]
fn a_reference_style_link_takes_its_target_from_a_definition_anywhere_and_is_reported_at_the_using_line() {
    let source = "intro [the text][Ref One]\n![pic][img]\nshortcut [Ref One] and collapsed [Ref One][]\n\n[ref one]: notes/one.md \"Title\"\n[img]: <my pic.png>\n";

    let found = links(source);

    assert_eq!(
        found
            .iter()
            .map(|link| (link.form, link.line, link.raw.as_str(), link.target.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (LinkForm::Markdown, 1, "[the text][Ref One]", "notes/one.md"),
            (LinkForm::MarkdownEmbed, 2, "![pic][img]", "my pic.png"),
            (LinkForm::Markdown, 3, "[Ref One]", "notes/one.md"),
            (LinkForm::Markdown, 3, "[Ref One][]", "notes/one.md"),
        ]
    );
}

#[test]
fn a_reference_without_a_definition_and_a_footnote_are_not_links() {
    assert_eq!(
        targets("[text][nowhere] and [lonely] and a footnote[^1]\n\n[^1]: a note\n"),
        Vec::<String>::new()
    );
}

#[test]
fn a_definition_inside_a_code_block_does_not_define_a_reference() {
    assert_eq!(targets("[text][ref]\n\n```\n[ref]: a.md\n```\n"), Vec::<String>::new());
}

#[test]
fn targets_with_a_url_scheme_autolinks_raw_html_and_same_note_links_are_left_out() {
    let source = concat!(
        "[a](https://example.com) [b](mailto:x@example.com) [c](obsidian://open?vault=v)\n",
        "<https://example.com> <a href=\"note.md\">n</a> <img src=\"p.png\">\n",
        "[d](#heading) [[#heading]] [[#^block]]\n",
        "![e](http://example.com/i.png)\n",
    );

    assert_eq!(targets(source), Vec::<String>::new());
}

#[test]
fn links_in_fenced_and_indented_code_blocks_code_spans_and_comments_are_left_out() {
    let source = concat!(
        "```\n[[fenced]] [f](f.md)\n```\n",
        "~~~\n[[tilde]]\n~~~\n",
        "\n    [[indented]] [i](i.md)\n    [[still indented]]\n\n",
        "`[[span]]` ``[s](s.md)`` %% [[comment]] [c](c.md) %%\n",
        "%%\n[[multi-line comment]]\n%%\n",
        "[[kept]] [kept](kept.md)\n",
    );

    assert_eq!(targets(source), ["kept", "kept.md"]);
}

#[test]
fn an_indented_line_inside_a_list_or_a_paragraph_continuation_is_not_code() {
    let source = "- item\n\n    [[nested in list]]\n\nparagraph\n    [[continuation]]\n";

    assert_eq!(targets(source), ["nested in list", "continuation"]);
}

#[test]
fn a_four_space_indented_line_after_a_blank_line_following_a_plain_paragraph_is_code() {
    assert_eq!(targets("paragraph\n\n    [[code]]\n[[after]]\n"), ["after"]);
}

#[test]
fn wikilinks_in_frontmatter_are_links() {
    let source = "---\nrelated: \"[[Frontmatter Note]]\"\nlist:\n    - \"[[Indented Entry]]\"\n---\n[[Body]]\n";

    assert_eq!(targets(source), ["Frontmatter Note", "Indented Entry", "Body"]);
}

#[test]
fn a_wikilink_in_a_table_row_may_escape_its_pipe() {
    let found = links("| a |\n| [[target\\|shown]] |\n");

    assert_eq!(found[0].target, "target");
    assert_eq!(found[0].display.as_deref(), Some("shown"));
}

#[test]
fn a_stray_backtick_does_not_hide_a_link() {
    assert_eq!(targets("it`s [[kept]]\n"), ["kept"]);
}

#[test]
fn placeholders_are_recognised_in_wikilinks_but_angle_brackets_are_syntax_in_markdown_links() {
    let found = links("[[{{title}}]] [[Notes/{{slug}}]] [[<slug>]] [[real]] [x](<real note.md>) [y]({{path}})\n");

    assert_eq!(
        found.iter().map(ObsidianLink::is_placeholder).collect::<Vec<_>>(),
        [true, true, true, false, false, true]
    );
}

#[test]
fn an_unclosed_wikilink_and_an_empty_one_are_still_rejected() {
    assert_eq!(
        LinkCollection::try_from("[[open\n"),
        Err(MarkdownError::UnclosedLink { line: 1, embed: false })
    );
    assert_eq!(
        LinkCollection::try_from("ok\n[[]]\n"),
        Err(MarkdownError::EmptyLinkTarget { line: 2 })
    );
}

#[test]
fn a_malformed_markdown_link_is_plain_text_and_never_an_error() {
    assert_eq!(targets("[text](unclosed and [more](\n"), Vec::<String>::new());
}

#[test]
fn embeds_report_their_form() {
    assert_eq!(
        forms("![[a]] ![b](b.png) [[c]]\n"),
        [LinkForm::WikilinkEmbed, LinkForm::MarkdownEmbed, LinkForm::Wikilink]
    );
}

#[test]
fn a_link_form_round_trips_through_its_stored_label() -> Result<(), serde_json::Error> {
    for form in [
        LinkForm::Wikilink,
        LinkForm::WikilinkEmbed,
        LinkForm::Markdown,
        LinkForm::MarkdownEmbed,
    ] {
        let stored = serde_json::to_string(&form)?;
        assert_eq!(stored, format!("\"{}\"", form.label()));
        assert_eq!(serde_json::from_str::<LinkForm>(&stored)?, form);
    }
    Ok(())
}

#[test]
fn a_fence_inside_a_list_item_hides_its_links_whatever_its_indentation() {
    let tabbed = "- item\n\t```\n\t[[tabbed]]\n\t```\n[[after tab]]\n";
    let spaced = "- item\n    ```rust\n    [[spaced]]\n    ```\n[[after spaces]]\n";
    let numbered = "1. item\n   ```\n   [[numbered]]\n   ```\n[[after number]]\n";
    let marker_line = "- ```\n  [[on the marker line]]\n  ```\n[[after marker]]\n";

    assert_eq!(targets(tabbed), ["after tab"]);
    assert_eq!(targets(spaced), ["after spaces"]);
    assert_eq!(targets(numbered), ["after number"]);
    assert_eq!(targets(marker_line), ["after marker"]);
}

#[test]
fn a_fence_inside_a_quote_or_callout_hides_its_links() {
    let callout = "> [!note]\n> ```\n> [[inside]]\n> ```\n> [[outside the fence]]\n[[after]]\n";
    let nested = "> > ```\n> > [[deep]]\n> > ```\n[[after]]\n";

    assert_eq!(targets(callout), ["outside the fence", "after"]);
    assert_eq!(targets(nested), ["after"]);
}

#[test]
fn a_fence_ends_when_the_list_item_or_quote_it_sits_in_ends() {
    assert_eq!(
        targets("- item\n  ```\n  [[hidden]]\n[[ends the item]]\n"),
        ["ends the item"]
    );
    assert_eq!(targets("> ```\n> [[hidden]]\n[[ends the quote]]\n"), ["ends the quote"]);
}

#[test]
fn an_unclosed_wikilink_inside_a_fence_in_a_list_item_or_callout_does_not_refuse_the_note() {
    for source in [
        "- item\n\t```\n\t[[unclosed\n\t```\n",
        "- item\n    ```\n    [[unclosed\n    ```\n",
        "> [!tip]\n> ```\n> [[unclosed\n> ```\n",
    ] {
        assert!(ValidatedMarkdown::try_from(source).is_ok(), "{source:?}");
    }
}

#[test]
fn a_comment_marker_in_frontmatter_does_not_hide_the_links_after_it() {
    let source = "---\ncolour: 100%% sure\nrelated: \"[[Frontmatter]]\"\n---\n[[Body]]\n";

    assert_eq!(targets(source), ["Frontmatter", "Body"]);
}

#[test]
fn an_indented_line_straight_after_a_heading_at_the_start_or_after_a_closing_fence_is_code() {
    assert_eq!(targets("# Heading\n    [[code]]\n[[after]]\n"), ["after"]);
    assert_eq!(targets("    [[code]]\n[[after]]\n"), ["after"]);
    assert_eq!(targets("```\nx\n```\n    [[code]]\n[[after]]\n"), ["after"]);
    assert_eq!(targets("text\n    [[continuation]]\n"), ["continuation"]);
}

#[test]
fn an_escaped_backtick_is_literal_outside_a_code_span_and_a_backslash_is_literal_inside_one() {
    assert_eq!(targets("`a\\` [[x]] `\n"), ["x"]);
    assert_eq!(targets("\\`[[kept]]` \n"), ["kept"]);
}

#[test]
fn a_code_span_may_run_over_several_lines_of_a_paragraph() {
    assert_eq!(
        targets("start `code [[hidden]]\nstill [[hidden too]] code` [[kept]]\n"),
        ["kept"]
    );
    assert_eq!(targets("a ``x\n[[hidden]]\n`` [[kept]]\n"), ["kept"]);
}

#[test]
fn a_backtick_run_with_no_closing_run_in_its_paragraph_is_text() {
    assert_eq!(
        targets("a `unclosed [[kept]]\n\nnext ` [[also kept]]\n"),
        ["kept", "also kept"]
    );
}

#[test]
fn a_wikilink_whose_target_looks_like_a_url_scheme_is_a_link_to_a_note_and_not_external() {
    let found = links("[[Re: meeting]] [[https://example.com]] [[#own heading]] [m](https://example.com)\n");

    assert_eq!(
        found.iter().map(|link| link.target.as_str()).collect::<Vec<_>>(),
        ["Re: meeting", "https://example.com"]
    );
}

#[test]
fn callout_validation_agrees_with_the_scanner_about_what_is_code() {
    for source in [
        "paragraph\n\n    [!oops in indented code\n",
        "````\n```\n[!oops\n```\n````\n",
        "- item\n\t```\n\t[!oops\n\t```\n",
        "%%\n> [!oops\n%%\n",
    ] {
        assert!(ValidatedMarkdown::try_from(source).is_ok(), "{source:?}");
    }
    assert_eq!(
        ValidatedMarkdown::try_from("text\n> [!oops\n"),
        Err(MarkdownError::InvalidCallout { line: 2 })
    );
}
