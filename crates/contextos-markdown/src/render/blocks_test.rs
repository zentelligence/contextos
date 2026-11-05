use super::support_test::{render, render_template, render_with};
use crate::Resources;

#[test]
fn plain_markdown_renders_with_heading_anchors() {
    assert_eq!(
        render("# Title\n\nSome *text*.\n"),
        "<h1 id=\"title\">Title</h1>\n<p>Some <em>text</em>.</p>\n"
    );
}

#[test]
fn headings_with_the_same_text_get_distinct_anchors() {
    assert_eq!(
        render("## Notes\n\n## Notes\n\n## Notes!\n"),
        "<h2 id=\"notes\">Notes</h2>\n<h2 id=\"notes-1\">Notes</h2>\n<h2 id=\"notes-2\">Notes!</h2>\n"
    );
}

#[test]
fn frontmatter_and_comments_are_not_rendered() {
    assert_eq!(
        render("---\ntitle: T\n---\nA %%hidden%% B\n%%\n# Gone\n%%\n"),
        "<p>A  B</p>\n"
    );
}

#[test]
fn a_callout_renders_by_its_type_with_its_title_and_body() {
    assert_eq!(
        render("> [!warning] Mind the gap\n> Body text.\n"),
        "<div class=\"callout callout-warning\" data-callout=\"warning\"><div class=\"callout-title\">Mind the gap</div><div class=\"callout-body\">\n<p>Body text.</p>\n</div></div>\n"
    );
}

#[test]
fn a_callout_with_no_title_is_titled_by_its_type_and_an_alias_takes_its_style() {
    assert_eq!(
        render("> [!faq]\n> Why?\n"),
        "<div class=\"callout callout-question\" data-callout=\"faq\"><div class=\"callout-title\">Faq</div><div class=\"callout-body\">\n<p>Why?</p>\n</div></div>\n"
    );
}

#[test]
fn a_foldable_callout_folds_without_script() {
    assert_eq!(
        render("> [!tip]- Hidden\n> Inside.\n"),
        "<details class=\"callout callout-tip\" data-callout=\"tip\"><summary class=\"callout-title\">Hidden</summary><div class=\"callout-body\">\n<p>Inside.</p>\n</div></details>\n"
    );
    assert!(
        render("> [!tip]+ Shown\n> Inside.\n")
            .starts_with("<details class=\"callout callout-tip\" data-callout=\"tip\" open>")
    );
}

#[test]
fn callouts_nest_and_a_plain_blockquote_stays_a_blockquote() {
    let html = render("> [!note] Outer\n> > [!danger] Inner\n> > Deep.\n");
    assert!(html.contains("<div class=\"callout callout-note\""), "{html}");
    assert!(
        html.contains(
            "<div class=\"callout callout-danger\" data-callout=\"danger\"><div class=\"callout-title\">Inner</div>"
        ),
        "{html}"
    );
    assert_eq!(
        render("> Just a quote.\n"),
        "<blockquote>\n<p>Just a quote.</p>\n</blockquote>\n"
    );
}

#[test]
fn a_callout_title_keeps_its_inline_markup() {
    let html = render("> [!note] See **this**\n> Body.\n");
    assert!(
        html.contains("<div class=\"callout-title\">See <strong>this</strong></div>"),
        "{html}"
    );
}

#[test]
fn a_known_fence_is_a_titled_container_with_its_role_and_tone() {
    assert_eq!(
        render(":::warning{title=\"Careful\"}\nThe *body*.\n:::\n"),
        "<div class=\"fence fence-warning fence-recognised fence-role-callout fence-tone-caution\" data-fence=\"warning\"><div class=\"fence-label\">Careful</div><div class=\"fence-body\">\n<p>The <em>body</em>.</p>\n</div></div>\n"
    );
}

#[test]
fn a_layout_fence_shows_no_label_unless_titled_and_carries_its_columns() {
    assert_eq!(
        render(":::grid{cols=3 gap=md}\n:::card{title=\"A\"}\nOne\n:::\n:::\n"),
        "<div class=\"fence fence-grid fence-recognised fence-role-layout fence-gap-md\" data-fence=\"grid\" style=\"--fence-cols: 3\"><div class=\"fence-body\"><div class=\"fence fence-card fence-recognised fence-role-layout\" data-fence=\"card\"><div class=\"fence-label\">A</div><div class=\"fence-body\">\n<p>One</p>\n</div></div>\n</div></div>\n"
    );
}

#[test]
fn a_collapsible_fence_is_a_details_element() {
    assert_eq!(
        render(":::note{collapse open title=\"More\"}\nx\n:::\n"),
        "<details class=\"fence fence-note fence-recognised fence-role-callout fence-tone-neutral\" data-fence=\"note\" open><summary class=\"fence-label\">More</summary><div class=\"fence-body\">\n<p>x</p>\n</div></details>\n"
    );
}

#[test]
fn an_unknown_fence_renders_its_content_plainly_and_is_flagged() {
    assert_eq!(
        render(":::details\nx\n:::\n"),
        "<div class=\"fence fence-details fence-unrecognised\" data-fence=\"details\"><div class=\"fence-label\">details</div><div class=\"render-flag\" role=\"note\">This vault's fence vocabulary has no details fence.</div><div class=\"fence-body\">\n<p>x</p>\n</div></div>\n"
    );
}

#[test]
fn an_attribute_problem_is_flagged_inside_the_fence() {
    let html = render(":::note{shade=red}\nx\n:::\n");
    assert!(
        html.contains(
            "<div class=\"render-flag\" role=\"note\">shade is not an attribute this vault's fences take</div>"
        ),
        "{html}"
    );
}

#[test]
fn a_page_break_is_an_invisible_marker() {
    assert_eq!(
        render("a\n\n:::page-break\n\nb\n"),
        "<p>a</p>\n<div class=\"fence fence-page-break fence-role-page\" aria-hidden=\"true\"></div>\n<p>b</p>\n"
    );
}

#[test]
fn fences_deeper_than_eight_render_as_text_with_a_flag() {
    let mut source = String::new();
    for _ in 0..9 {
        source.push_str(":::panel\n");
    }
    source.push_str("core\n");
    for _ in 0..9 {
        source.push_str(":::\n");
    }
    let html = render(&source);
    assert_eq!(html.matches("data-fence=\"panel\"").count(), 8, "{html}");
    assert!(
        html.contains("Fences nested deeper than eight are shown as text."),
        "{html}"
    );
    assert!(html.contains(":::panel"), "{html}");
}

#[test]
fn highlights_mark_text_even_across_emphasis_but_not_in_code() {
    assert_eq!(
        render("A ==bright *very* light== and `==code==` and == lone\n"),
        "<p>A <mark>bright <em>very</em> light</mark> and <code>==code==</code> and == lone</p>\n"
    );
}

#[test]
fn a_highlight_that_would_cross_markup_boundaries_stays_literal() {
    assert_eq!(render("==a *b== c*\n"), "<p>==a <em>b== c</em></p>\n");
}

#[test]
fn tags_are_marked_where_obsidian_reads_them() {
    assert_eq!(
        render("Tagged #project/alpha and #todo, not #123 or a#b or `#code`.\n"),
        "<p>Tagged <span class=\"tag\">#project/alpha</span> and <span class=\"tag\">#todo</span>, not #123 or a#b or <code>#code</code>.</p>\n"
    );
}

#[test]
fn a_block_id_becomes_an_anchor_and_leaves_the_text() {
    assert_eq!(
        render("A paragraph ^para-1\n\n- item ^item2\n"),
        "<p>A paragraph<span class=\"block-anchor\" id=\"^para-1\"></span></p>\n<ul>\n<li>item<span class=\"block-anchor\" id=\"^item2\"></span></li>\n</ul>\n"
    );
}

#[test]
fn a_block_id_on_its_own_line_is_an_anchor_for_the_block_before_it() {
    assert_eq!(
        render("| a |\n| - |\n| 1 |\n\n^table1\n"),
        "<table><thead><tr><th>a</th></tr></thead><tbody>\n<tr><td>1</td></tr>\n</tbody></table>\n<span class=\"block-anchor\" id=\"^table1\"></span>\n"
    );
}

#[test]
fn math_is_typeset_as_mathml_and_an_expression_that_fails_is_flagged_source() {
    let html = render("Inline $x^2$ and\n\n$$\\frac{1}{2}$$\n");
    assert!(html.contains("<math"), "{html}");
    assert!(html.contains("<msup><mi>x</mi><mn>2</mn></msup>"), "{html}");
    assert!(html.contains("<math display=\"block\""), "{html}");
    let failed = render("Bad $\\notacommand{x}$ here\n");
    assert!(failed.contains("class=\"render-flag math-source\""), "{failed}");
    assert!(failed.contains("<code>\\notacommand{x}</code>"), "{failed}");
}

#[test]
fn math_text_cannot_carry_markup() {
    let html = render("$\\text{<b>bold</b>}$\n");
    assert!(!html.contains("<b>"), "{html}");
}

#[test]
fn footnotes_are_numbered_across_fences_and_listed_at_the_end() {
    let html = render(":::note\nSee this.[^a]\n:::\n\nAnd that.[^b]\n\n[^b]: Second.\n[^a]: First.\n");
    assert!(
        html.contains("<sup class=\"footnote-ref\"><a href=\"#fn-0-1\" id=\"fnref-0-1\">1</a></sup>"),
        "{html}"
    );
    assert!(
        html.contains("<sup class=\"footnote-ref\"><a href=\"#fn-0-2\" id=\"fnref-0-2\">2</a></sup>"),
        "{html}"
    );
    let list = html.split("<section class=\"footnotes\">").nth(1).unwrap_or_default();
    let first = list.find("First.").unwrap_or(usize::MAX);
    let second = list.find("Second.").unwrap_or(0);
    assert!(first < second, "{html}");
    assert!(list.contains("<li id=\"fn-0-1\">"), "{html}");
}

#[test]
fn a_footnote_with_no_definition_stays_as_written() {
    assert_eq!(render("Missing.[^none]\n"), "<p>Missing.[^none]</p>\n");
}

#[test]
fn a_reference_link_resolves_across_fences() {
    let html = render(":::note\nSee [the page][p].\n:::\n\n[p]: /wiki/Page.md\n");
    assert!(
        html.contains("<a class=\"internal-link\" href=\"/v/page/wiki/Page.md\">the page</a>"),
        "{html}"
    );
}

#[test]
fn author_html_passes_the_allowlist() {
    assert_eq!(
        render(
            "<details>\n<summary onclick=\"x()\">More</summary>\n\nInside **md**.\n\n</details>\n\nText <script>alert(1)</script> after.\n"
        ),
        "<details>\n<summary>More</summary>\n<p>Inside <strong>md</strong>.</p>\n</details>\n<p>Text  after.</p>\n"
    );
}

#[test]
fn an_html_tag_split_over_lines_is_judged_whole() {
    let html = render("<div\n  onclick=\"alert(1)\"\n  title=\"t\">\nx\n</div>\n");
    assert!(!html.contains("onclick"), "{html}");
    assert!(html.contains("<div title=\"t\">"), "{html}");
}

#[test]
fn rendering_is_deterministic() {
    let source = "# A\n\n[[/wiki/B.md|B]] #tag ==hi== $x$ [^1]\n\n> [!note] T\n> b\n\n:::grid\nx\n:::\n\n[^1]: n\n";
    assert_eq!(render(source), render(source));
}

#[test]
fn the_metadata_lists_the_outline_tags_block_ids_and_links_with_their_lines() {
    let source = "---\nk: v\n---\n# Top\n\nText #alpha ^b1\n\n## Sub\n\n[[/wiki/X.md|X]] and [y](https://y.org)\n";
    let rendered = render_with(source, &Resources::default());
    let metadata = rendered.metadata;
    let outline: Vec<(u8, &str, &str, usize)> = metadata
        .outline
        .iter()
        .map(|heading| {
            (
                heading.level,
                heading.text.as_str(),
                heading.anchor.as_str(),
                heading.line,
            )
        })
        .collect();
    assert_eq!(outline, vec![(1, "Top", "top", 4), (2, "Sub", "sub", 8)]);
    assert_eq!(
        metadata
            .tags
            .iter()
            .map(|tag| (tag.name.as_str(), tag.line))
            .collect::<Vec<_>>(),
        vec![("alpha", 6)]
    );
    assert_eq!(
        metadata
            .block_ids
            .iter()
            .map(|block| (block.id.as_str(), block.line))
            .collect::<Vec<_>>(),
        vec![("b1", 6)]
    );
    assert_eq!(metadata.links.len(), 2);
    assert_eq!(
        metadata.links.iter().map(|link| link.line).collect::<Vec<_>>(),
        vec![10, 10]
    );
}

#[test]
fn a_template_shows_its_tokens_as_written_and_a_note_does_not() {
    assert_eq!(
        render_template("# <% tp.file.title %>\n\nDue {{date}} and `code`.\n"),
        "<h1 id=\"tp-file-title\"><code class=\"template-token\">&lt;% tp.file.title %&gt;</code></h1>\n<p>Due <code class=\"template-token\">{{date}}</code> and <code>code</code>.</p>\n"
    );
    assert_eq!(render("Due `{{date}}`.\n"), "<p>Due <code>{{date}}</code>.</p>\n");
}
