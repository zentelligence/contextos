use super::*;

#[test]
fn text_escapes_the_characters_that_open_markup_or_entities() {
    assert_eq!(escape_text("a < b & c > d"), "a &lt; b &amp; c &gt; d");
}

#[test]
fn text_leaves_quotes_alone() {
    assert_eq!(escape_text(r#"say "hi" 'there'"#), r#"say "hi" 'there'"#);
}

#[test]
fn an_attribute_value_also_escapes_both_quotes() {
    assert_eq!(
        escape_attribute(r#"x" onclick='y' <&>"#),
        "x&quot; onclick=&#39;y&#39; &lt;&amp;&gt;"
    );
}
