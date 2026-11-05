use super::*;

// `body_start_line` is `frontmatter_read`'s own 1-based source line number
// (`contextos_obsidian::FrontmatterDocument::body_start_line`'s own doc:
// "the 1-based source line at which the body begins"): for
// "---\ntitle: Tax\n---\n\n# Tax rules\n..." that is real file line 4 (the
// blank line immediately after the closing `---`), not line 3 (the
// delimiter itself) - every value below is that real 1-based line, not a
// 0-based index into `content.lines()`.

#[test]
fn body_from_lines_keeps_everything_from_the_start_line_onward() {
    let content = "---\ntitle: Tax\n---\n\n# Tax rules\n\nPay quarterly.\n";
    assert_eq!(body_from_lines(content, 4), "\n# Tax rules\n\nPay quarterly.\n");
}

#[test]
fn body_from_lines_is_empty_when_nothing_follows_the_header() {
    let content = "---\ntitle: Tax\n---\n";
    assert_eq!(body_from_lines(content, 4), "");
}

#[test]
fn header_from_lines_keeps_everything_strictly_before_the_start_line() {
    let content = "---\ntitle: Tax\n---\n\n# Tax rules\n";
    assert_eq!(header_from_lines(content, 4), "---\ntitle: Tax\n---\n");
}

#[test]
fn header_and_body_from_lines_round_trip_the_original_content() {
    let content = "---\ntitle: Tax\nstatus: draft\n---\n\n# Tax rules\n\nPay quarterly.\n";
    let start_line = 5;
    let header = header_from_lines(content, start_line);
    let body = body_from_lines(content, start_line);
    assert_eq!(format!("{header}{body}"), content);
}

#[test]
fn body_from_lines_saturates_a_start_line_past_the_end_of_the_file() {
    let content = "---\ntitle: Tax\n---\n";
    assert_eq!(body_from_lines(content, 999), "");
}

#[test]
fn header_from_lines_saturates_a_start_line_past_the_end_of_the_file() {
    let content = "---\ntitle: Tax\n---\n";
    assert_eq!(header_from_lines(content, 999), content);
}

#[test]
fn header_from_lines_with_a_start_line_of_one_yields_an_empty_header() {
    let content = "First line.\nSecond line.\n";
    assert_eq!(header_from_lines(content, 1), "");
}

#[test]
fn the_body_of_a_note_is_what_follows_its_frontmatter() {
    assert_eq!(
        body_of("---\ntype: note\nstatus: active\n---\nBody text."),
        "Body text."
    );
    assert_eq!(body_of("---\n---\nBody."), "Body.");
    assert_eq!(body_of("---\r\ntype: note\r\n---\r\nBody text.\r\n"), "Body text.\r\n");
}

#[test]
fn a_note_without_closed_frontmatter_is_all_body() {
    assert_eq!(body_of("# Heading\n\nBody text."), "# Heading\n\nBody text.");
    assert_eq!(
        body_of("---\ntype: note\nnever closed"),
        "---\ntype: note\nnever closed"
    );
}
