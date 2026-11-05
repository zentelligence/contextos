use super::*;

#[test]
fn each_template_token_becomes_a_code_span() {
    assert_eq!(
        String::from(TokensAsCode::from("Title: <% tp.file.title %> on {{date}}\n")),
        "Title: `<% tp.file.title %>` on `{{date}}`\n"
    );
}

#[test]
fn a_token_holding_backticks_gets_a_longer_fence() {
    assert_eq!(String::from(TokensAsCode::from("<% `x` %>\n")), "`` <% `x` %> ``\n");
}

#[test]
fn tokens_inside_code_and_unclosed_tokens_are_left_alone() {
    let text = "```\n<% inside %>\n```\n`{{ inline }}` and <% never closed\n";
    assert_eq!(String::from(TokensAsCode::from(text)), text);
}

#[test]
fn a_token_does_not_run_over_a_line_break() {
    let text = "{{ starts\nends }}\n";
    assert_eq!(String::from(TokensAsCode::from(text)), text);
}
