use super::*;

fn strip(text: &str, code: &[Range<usize>]) -> String {
    String::from(CommentFree::from(CommentInput { text, code }))
}

#[test]
fn an_inline_comment_is_removed() {
    assert_eq!(strip("a %%hidden%% b\n", &[]), "a  b\n");
}

#[test]
fn a_comment_across_lines_is_removed_but_its_line_breaks_are_kept() {
    assert_eq!(strip("a\n%%\n# Hidden\ntext\n%%\nb\n", &[]), "a\n\n\n\n\nb\n");
}

#[test]
fn an_unclosed_comment_stays_visible() {
    assert_eq!(strip("a %% never closed\nb\n", &[]), "a %% never closed\nb\n");
}

#[test]
fn a_comment_marker_inside_code_is_text() {
    let text = "`%%` a %%x%% `%%`\n";
    let first = 0..4;
    let second = 13..17;
    assert_eq!(strip(text, &[first, second]), "`%%` a  `%%`\n");
}

#[test]
fn a_comment_may_not_open_inside_code_and_close_outside_it() {
    let text = "```\n%%\n```\nkeep %%\n";
    let code = [Range { start: 0, end: 11 }];
    assert_eq!(strip(text, &code), text);
}
