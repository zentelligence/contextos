use proptest::prelude::*;

use super::support_test::{prepare, render, render_with};
use crate::{BaseViewKey, Diagram, DiagramProblem, EmbedKey, Fragment, Resources};

#[test]
fn a_wikilink_goes_to_the_page_that_owns_its_note_with_a_heading_anchor() {
    assert_eq!(
        render("[[/wiki/Foo.md#Intro Part|Foo#Intro Part]]\n"),
        "<p><a class=\"wikilink\" href=\"/v/page/wiki/Foo.md#intro-part\">Foo#Intro Part</a></p>\n"
    );
}

#[test]
fn a_block_reference_anchors_on_the_block() {
    assert_eq!(
        render("[[/wiki/Foo.md#^b-1|see]]\n"),
        "<p><a class=\"wikilink\" href=\"/v/page/wiki/Foo.md#^b-1\">see</a></p>\n"
    );
}

#[test]
fn a_note_no_screen_shows_is_unavailable_and_says_why() {
    assert_eq!(
        render("[[/unowned/X.md|X]]\n"),
        "<p><span class=\"wikilink dead unrouted\" data-target=\"unowned/X.md\" title=\"No screen shows this note yet.\">X</span></p>\n"
    );
}

#[test]
fn a_link_the_server_could_not_resolve_is_shown_with_its_original_text() {
    assert_eq!(
        render("[[/:!typo|typo]] [[/:?dup|dup]]\n"),
        "<p><span class=\"wikilink dead\" data-target=\"typo\">typo</span> <span class=\"wikilink dead ambiguous\" data-target=\"dup\" title=\"More than one note answers to this name.\">dup</span></p>\n"
    );
}

#[test]
fn a_link_to_a_file_that_is_not_a_page_goes_to_the_file() {
    assert_eq!(
        render("[[/docs/spec.pdf|spec]]\n"),
        "<p><a class=\"wikilink\" href=\"/v/file/docs/spec.pdf\">spec</a></p>\n"
    );
}

#[test]
fn standard_links_are_routed_external_ones_get_rel_and_unsafe_ones_are_dead() {
    assert_eq!(
        render("[a](/wiki/My%20Page.md#Some%20Part \"T\") [b](https://x.org) [c](javascript:alert(1))\n"),
        "<p><a class=\"internal-link\" href=\"/v/page/wiki/My Page.md#some-part\" title=\"T\">a</a> <a class=\"external-link\" href=\"https://x.org\" rel=\"noopener noreferrer\">b</a> <span class=\"internal-link dead\" data-target=\"javascript:alert(1)\">c</span></p>\n"
    );
    assert_eq!(
        render("[d](<#Local Part>)\n"),
        "<p><a class=\"internal-link\" href=\"#local-part\">d</a></p>\n"
    );
}

#[test]
fn a_wikilink_in_a_table_cell_loses_the_backslash_that_escaped_its_pipe() {
    let html = render("| a |\n| - |\n| [[/x/Y.md\\|Y]] |\n");
    assert!(
        html.contains("<a class=\"wikilink\" href=\"/v/page/x/Y.md\">Y</a>"),
        "{html}"
    );
}

#[test]
fn image_audio_video_and_pdf_embeds_use_the_file_url() {
    let html = render(
        "![[/img/p.png|300]]\n\n![[/img/q.png|640x480]]\n\n![[/a/s.mp3]]\n\n![[/a/v.mp4]]\n\n![[/d/doc.pdf#page=3]]\n",
    );
    assert!(
        html.contains("<img class=\"embed-image\" src=\"/v/file/img/p.png\" alt=\"p\" width=\"300\">"),
        "{html}"
    );
    assert!(
        html.contains("<img class=\"embed-image\" src=\"/v/file/img/q.png\" alt=\"q\" width=\"640\" height=\"480\">"),
        "{html}"
    );
    assert!(
        html.contains("<audio class=\"embed-audio\" controls src=\"/v/file/a/s.mp3\">s</audio>"),
        "{html}"
    );
    assert!(
        html.contains("<video class=\"embed-video\" controls src=\"/v/file/a/v.mp4\">v</video>"),
        "{html}"
    );
    assert!(
        html.contains("<iframe class=\"embed-pdf\" src=\"/v/file/d/doc.pdf#page=3\" title=\"doc\"></iframe>"),
        "{html}"
    );
}

#[test]
fn a_standard_image_of_a_vault_file_or_a_web_address_is_shown_and_any_other_is_not() {
    assert_eq!(
        render("![A](/img/p.png) ![B](https://x.org/b.png) ![C](javascript:x)\n"),
        "<p><img src=\"/v/file/img/p.png\" alt=\"A\"> <img src=\"https://x.org/b.png\" alt=\"B\"> <span class=\"embed dead\">C</span></p>\n"
    );
}

#[test]
fn an_embedded_note_is_inlined_from_its_resource() {
    let mut resources = Resources::default();
    resources.embeds.insert(
        EmbedKey {
            path: "wiki/Inner.md".to_owned(),
            fragment: None,
        },
        Some(prepare("Inner *text* with [[/wiki/Other.md|Other]]\n")),
    );
    let html = render_with("![[/wiki/Inner.md]]\n", &resources).html;
    assert_eq!(
        html,
        "<div class=\"embed-block\"><div class=\"embed-label\"><a class=\"wikilink\" href=\"/v/page/wiki/Inner.md\">Inner</a></div><p>Inner <em>text</em> with <a class=\"wikilink\" href=\"/v/page/wiki/Other.md\">Other</a></p>\n</div>\n"
    );
}

#[test]
fn an_embed_inside_an_embed_is_a_link_and_a_missing_one_is_dead() {
    let mut resources = Resources::default();
    resources.embeds.insert(
        EmbedKey {
            path: "wiki/Inner.md".to_owned(),
            fragment: None,
        },
        Some(prepare("![[/wiki/Deeper.md]]\n")),
    );
    resources.embeds.insert(
        EmbedKey {
            path: "wiki/Gone.md".to_owned(),
            fragment: None,
        },
        None,
    );
    let html = render_with("![[/wiki/Inner.md]]\n\n![[/wiki/Gone.md]]\n", &resources).html;
    assert!(
        html.contains("<a class=\"wikilink\" href=\"/v/page/wiki/Deeper.md\">Deeper</a>"),
        "{html}"
    );
    assert!(
        html.contains("<span class=\"wikilink dead\" data-target=\"wiki/Gone.md\">Gone</span>"),
        "{html}"
    );
}

#[test]
fn an_embed_shares_the_pages_heading_anchors_and_numbers_its_own_footnotes() {
    let mut resources = Resources::default();
    resources.embeds.insert(
        EmbedKey {
            path: "wiki/Inner.md".to_owned(),
            fragment: None,
        },
        Some(prepare("# Intro\n\nNote.[^1]\n\n[^1]: Inner.\n")),
    );
    let html = render_with(
        "# Intro\n\n![[/wiki/Inner.md]]\n\nOuter.[^1]\n\n[^1]: Outer.\n",
        &resources,
    )
    .html;
    assert!(html.contains("<h1 id=\"intro\">"), "{html}");
    assert!(html.contains("<h1 id=\"intro-1\">"), "{html}");
    assert!(html.contains("id=\"fn-1-1\""), "{html}");
    assert!(html.contains("id=\"fn-0-1\""), "{html}");
}

#[test]
fn a_base_embed_shows_its_view_from_the_resource() {
    let mut resources = Resources::default();
    resources.bases.insert(
        BaseViewKey {
            path: "memory/tasks.base".to_owned(),
            view: Some("Open".to_owned()),
        },
        "<table class=\"base\"></table>".to_owned(),
    );
    let html = render_with("![[/memory/tasks.base#Open]]\n", &resources).html;
    assert_eq!(
        html,
        "<div class=\"embed-block\"><div class=\"embed-label\"><a class=\"wikilink\" href=\"/v/page/memory/tasks.base\">tasks</a></div><table class=\"base\"></table></div>\n"
    );
}

#[test]
fn a_canvas_or_diagram_embed_is_a_link_to_its_page() {
    assert_eq!(
        render("![[/b/board.canvas]]\n"),
        "<p><a class=\"wikilink\" href=\"/v/page/b/board.canvas\">board</a></p>\n"
    );
}

#[test]
fn a_mermaid_block_shows_its_diagram_or_its_problems() {
    let mut resources = Resources::default();
    resources
        .diagrams
        .insert("graph TD\nA-->B\n".to_owned(), Diagram::Svg("<svg>ok</svg>".to_owned()));
    resources.diagrams.insert(
        "graph bad\n".to_owned(),
        Diagram::Failed(vec![DiagramProblem {
            code: "mermaid/parse".to_owned(),
            message: "Unexpected <token>".to_owned(),
        }]),
    );
    let html = render_with(
        "```mermaid\ngraph TD\nA-->B\n```\n\n```mermaid\ngraph bad\n```\n",
        &resources,
    )
    .html;
    assert!(
        html.contains("<div class=\"mermaid-diagram\"><svg>ok</svg></div>"),
        "{html}"
    );
    assert!(
        html.contains("<div class=\"diagnostic-panel\" role=\"alert\"><div class=\"diagnostic\" data-code=\"mermaid/parse\"><span class=\"diagnostic-code\">mermaid/parse</span><p class=\"diagnostic-message\">Unexpected &lt;token&gt;</p></div></div>"),
        "{html}"
    );
}

#[test]
fn preparing_a_note_lists_the_embeds_diagrams_and_base_views_it_needs() {
    let prepared = prepare(
        "![[/wiki/A.md#^b1]]\n\n![[/t.base#View]]\n\n![[/i/p.png]]\n\n```mermaid\ngraph TD\n```\n\n:::note\n![[/wiki/B.md]]\n:::\n",
    );
    let needs = prepared.needs();
    assert_eq!(
        needs.embeds.iter().cloned().collect::<Vec<_>>(),
        vec![
            EmbedKey {
                path: "wiki/A.md".to_owned(),
                fragment: Some(Fragment::Block("b1".to_owned())),
            },
            EmbedKey {
                path: "wiki/B.md".to_owned(),
                fragment: None,
            },
        ]
    );
    assert_eq!(
        needs.diagrams.iter().cloned().collect::<Vec<_>>(),
        vec!["graph TD\n".to_owned()]
    );
    assert_eq!(
        needs.bases.iter().cloned().collect::<Vec<_>>(),
        vec![BaseViewKey {
            path: "t.base".to_owned(),
            view: Some("View".to_owned()),
        }]
    );
}

#[test]
fn text_from_the_vault_is_escaped_wherever_it_is_placed() {
    let html = render(
        "[[/:!a\"b<i>|x<b onclick=\"z()\">y</b>]] [[/unowned/\"q\".md|q]] [t](https://x.org \"say \\\"hi\\\" <now>\") ![[/i/<p>.png]]\n",
    );
    assert!(html.contains("data-target=\"a&quot;b&lt;i&gt;\""), "{html}");
    // A display is Markdown, so markup in it is author HTML and passes the allowlist like any other.
    assert!(html.contains(">x<b>y</b></span>"), "{html}");
    assert!(!html.contains("onclick"), "{html}");
    assert!(html.contains("data-target=\"unowned/&quot;q&quot;.md\""), "{html}");
    assert!(html.contains("title=\"say &quot;hi&quot; &lt;now&gt;\""), "{html}");
    assert!(html.contains("alt=\"&lt;p&gt;\""), "{html}");
    assert!(!html.contains("<i>") && !html.contains("<p>.png"), "{html}");
}

#[test]
fn a_body_refers_to_the_vault_when_any_link_names_something_other_than_the_web_or_the_page() {
    for (source, refers) in [
        (
            "Plain text and [web](https://x.org) and [here](#part) and [[#Local]].\n",
            false,
        ),
        ("A relative [link](other.md).\n", true),
        ("A [[wikilink]].\n", true),
        ("An ![[embed.png]].\n", true),
        ("A [ref][r].\n\n[r]: notes/x.md\n", true),
        ("```\n[[inside code]]\n```\n", false),
    ] {
        assert_eq!(prepare(source).needs().refers_to_vault, refers, "{source:?}");
    }
}

fn marked_link() -> impl Strategy<Value = String> {
    ("[a-z]{1,5}", prop_oneof![Just("/:!"), Just("/:?")], 0..6_u8).prop_map(|(word, marker, form)| match form {
        0 => format!("[[{marker}{word}|{word}]]"),
        1 => format!("[[{marker}{word}]]"),
        2 => format!("![[{marker}{word}]]"),
        3 => format!("[{word}]({marker}{word})"),
        4 => format!("![{word}]({marker}{word})"),
        _ => format!("[{word}][r]\n\n[r]: {marker}{word}\n"),
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn no_marker_ever_reaches_the_page(
        links in prop::collection::vec(marked_link(), 1..6),
        joins in prop::collection::vec(prop_oneof![Just(" "), Just("\n"), Just("\n\n"), Just(" | "), Just("\n> ")], 6)
    ) {
        let mut text = String::new();
        for (link, join) in links.iter().zip(joins.iter()) {
            text.push_str(link);
            text.push_str(join);
        }
        let html = render(&text);
        prop_assert!(!html.contains("/:!"), "{text:?} => {html}");
        prop_assert!(!html.contains("/:?"), "{text:?} => {html}");
    }
}
