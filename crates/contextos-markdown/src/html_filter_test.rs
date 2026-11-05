use proptest::prelude::*;

use super::*;

fn filter(html: &str) -> String {
    String::from(SafeHtml::from(AuthorHtml(html)))
}

#[test]
fn allowed_markup_is_kept_and_rebuilt() {
    assert_eq!(
        filter("<details open><summary>More</summary>\n<kbd>Ctrl</kbd> <sup>2</sup><br></details>"),
        "<details open><summary>More</summary>\n<kbd>Ctrl</kbd> <sup>2</sup><br></details>"
    );
    assert_eq!(
        filter(r#"<SPAN Class="x" STYLE="color: red">t</SPAN>"#),
        r#"<span class="x" style="color: red">t</span>"#
    );
}

#[test]
fn event_handlers_and_unknown_attributes_are_removed() {
    assert_eq!(
        filter(r#"<div onclick="alert(1)" onmouseover=x id="main" data-x="1" title='t'>a</div>"#),
        r#"<div title="t">a</div>"#
    );
}

#[test]
fn a_link_keeps_a_web_or_mail_address_and_gains_rel() {
    assert_eq!(
        filter(r#"<a href="https://example.org/?a=1&amp;b=2">x</a>"#),
        r#"<a href="https://example.org/?a=1&amp;b=2" rel="noopener noreferrer">x</a>"#
    );
    assert_eq!(
        filter(r#"<a href="mailto:a@b.c">m</a>"#),
        r#"<a href="mailto:a@b.c" rel="noopener noreferrer">m</a>"#
    );
    assert_eq!(
        filter(r##"<a href="#part">p</a>"##),
        r##"<a href="#part" rel="noopener noreferrer">p</a>"##
    );
}

#[test]
fn a_script_address_is_removed_however_it_is_disguised() {
    for href in [
        "javascript:alert(1)",
        "JavaScript:alert(1)",
        " javascript:alert(1)",
        "java\tscript:alert(1)",
        "jav&#97;script:alert(1)",
        "jav&#x61;script:alert(1)",
        "javascript&colon;alert(1)",
        "&#106;avascript:alert(1)",
        "vbscript:x",
        "data:text/html,<script>alert(1)</script>",
        "relative/path.md",
        "/vault/path.md",
    ] {
        let html = format!("<a href=\"{href}\">x</a>");
        assert_eq!(filter(&html), r#"<a rel="noopener noreferrer">x</a>"#, "{href}");
    }
}

#[test]
fn an_image_keeps_a_web_or_raster_data_source_and_loses_any_other() {
    assert_eq!(
        filter(r#"<img src="https://x.org/a.png" alt="A" width="20" height="abc" onerror="x">"#),
        r#"<img src="https://x.org/a.png" alt="A" width="20">"#
    );
    assert_eq!(
        filter(r#"<img src="data:image/png;base64,AAAA">"#),
        r#"<img src="data:image/png;base64,AAAA">"#
    );
    assert_eq!(filter(r#"<img src="data:image/svg+xml,<svg onload=x>">"#), "<img>");
    assert_eq!(filter(r#"<img src="pic.png">"#), "<img>");
}

#[test]
fn script_style_and_frame_elements_are_removed_with_their_content() {
    assert_eq!(filter("a<script>alert('<b>')</script>b"), "ab");
    assert_eq!(filter("a<STYLE>body{display:none}</style>b"), "ab");
    assert_eq!(filter(r#"a<iframe src="https://x"></iframe>b"#), "ab");
    assert_eq!(filter("a<svg><g><script>x</script><text>t</text></g></svg>b"), "ab");
    assert_eq!(filter("a<math><mi>x</mi></math>b"), "ab");
    assert_eq!(filter("a<script>never closed"), "a");
}

#[test]
fn an_unknown_or_form_element_loses_its_tags_but_keeps_its_text() {
    assert_eq!(filter("<custom-el>kept</custom-el>"), "kept");
    assert_eq!(
        filter(r#"<form action="x"><button onclick="y">Go</button><input value="v"></form>"#),
        "Go"
    );
}

#[test]
fn comments_doctypes_and_unterminated_tags_produce_nothing() {
    assert_eq!(filter("a<!-- hidden -->b<!DOCTYPE html>c"), "abc");
    assert_eq!(filter("a<!-- never closed"), "a");
    assert_eq!(filter("a<div title=\""), "a");
    assert_eq!(filter("a<div\n"), "a");
}

#[test]
fn text_is_escaped_but_character_references_survive() {
    assert_eq!(
        filter("1 < 2 && 3 > 2 &nbsp;&#169;&copy; &bogus"),
        "1 &lt; 2 &amp;&amp; 3 &gt; 2 &nbsp;&#169;&copy; &amp;bogus"
    );
    assert_eq!(filter("a < b"), "a &lt; b");
}

#[test]
fn an_attribute_value_is_decoded_then_escaped_again() {
    assert_eq!(
        filter(r#"<span title="a &quot;b&quot; &lt;c&gt; &amp; 'd'">x</span>"#),
        r#"<span title="a &quot;b&quot; &lt;c&gt; &amp; &#39;d&#39;">x</span>"#
    );
}

#[test]
fn a_style_that_could_load_or_run_anything_is_removed() {
    for style in [
        "background: url(https://x)",
        "width: expression(alert(1))",
        "behavior: url(x.htc)",
        "color: red; -moz-binding: x",
        "@import 'x'",
        "color: \\72 ed",
        "color: red /* comment */",
    ] {
        let html = format!("<span style=\"{style}\">t</span>");
        assert_eq!(filter(&html), "<span>t</span>", "{style}");
    }
}

#[test]
fn an_end_tag_for_an_element_never_opened_here_is_kept_when_allowed() {
    assert_eq!(filter("</details>\n"), "</details>\n");
    assert_eq!(filter("</script>"), "");
}

#[test]
fn void_elements_have_no_end_tag_and_a_self_closing_slash_is_dropped() {
    assert_eq!(filter("a<br/>b<hr />c</br>"), "a<br>b<hr>c");
}

/// Each tag in `output` (which the filter writes as `<name attr="value" flag>`), as its name and attributes.
fn tags(output: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut found = Vec::new();
    let mut rest = output;
    while let Some(at) = rest.find('<') {
        rest = rest.get(at.saturating_add(1)..).unwrap_or_default();
        let end = rest.find('>').unwrap_or(rest.len());
        let inside = rest.get(..end).unwrap_or_default().trim_start_matches('/');
        let mut parts = inside.splitn(2, ' ');
        let name = parts.next().unwrap_or_default().to_owned();
        let mut attributes = Vec::new();
        let mut text = parts.next().unwrap_or_default();
        while !text.is_empty() {
            let name_end = text.find(['=', ' ']).unwrap_or(text.len());
            let key = text.get(..name_end).unwrap_or_default().to_owned();
            text = text.get(name_end..).unwrap_or_default();
            let mut value = String::new();
            if let Some(quoted) = text.strip_prefix("=\"") {
                let close = quoted.find('"').unwrap_or(quoted.len());
                value = quoted.get(..close).unwrap_or_default().to_owned();
                text = quoted.get(close.saturating_add(1)..).unwrap_or_default();
            }
            text = text.trim_start();
            attributes.push((key, value));
        }
        found.push((name, attributes));
    }
    found
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_000))]

    #[test]
    fn whatever_the_input_every_tag_out_is_allowlisted_and_carries_no_handler_or_script(
        input in "(<|>|&|\"|'|=|/|!|-|;|#|:|&#|&colon;| |\t|\n|a|on|onclick|script|style|svg|div|span|a href|img src|javascript|href|title|x|1)*"
    ) {
        let output = filter(&input);
        for (name, attributes) in tags(&output) {
            prop_assert!(ALLOWED_ELEMENTS.contains(&name.as_str()), "{name} in {output}");
            for (key, value) in attributes {
                prop_assert!(!key.starts_with("on"), "{key} in {output}");
                prop_assert!(!key.is_empty(), "empty attribute in {output}");
                if key == "href" || key == "src" {
                    let lower = value.to_ascii_lowercase();
                    prop_assert!(
                        lower.starts_with("http") || lower.starts_with("mailto:") || lower.starts_with('#')
                            || lower.starts_with("data:image/"),
                        "{key}={value} in {output}"
                    );
                }
            }
        }
    }

    #[test]
    fn filtering_is_deterministic(input in "\\PC*") {
        prop_assert_eq!(filter(&input), filter(&input));
    }
}
