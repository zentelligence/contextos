use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn a_note_that_does_not_open_with_a_delimiter_line_has_no_frontmatter() {
    assert_eq!(
        FrontmatterSpan::from("# Title\n\n---\nnot: frontmatter\n---\n"),
        FrontmatterSpan::Absent
    );
    assert_eq!(FrontmatterSpan::from(""), FrontmatterSpan::Absent);
    assert_eq!(
        FrontmatterSpan::from(" ---\nkey: value\n---\n"),
        FrontmatterSpan::Absent
    );
}

#[test]
fn a_closed_block_spans_its_yaml_and_the_body_starts_after_the_closing_line() -> TestResult {
    let source = "---\ndate: 2026-10-31\n---\n\n# Today\n";
    let FrontmatterSpan::Closed {
        yaml,
        body_start,
        body_start_line,
    } = FrontmatterSpan::from(source)
    else {
        return Err("expected a closed block".into());
    };
    assert_eq!(&source[yaml], "date: 2026-10-31\n");
    assert_eq!(&source[body_start..], "\n# Today\n");
    assert_eq!(body_start_line, 4);
    Ok(())
}

#[test]
fn a_byte_order_mark_and_crlf_line_endings_still_delimit_the_block() -> TestResult {
    let source = "\u{feff}---\r\ntags: [daily]\r\n---\r\nBody\r\n";
    let FrontmatterSpan::Closed { yaml, body_start, .. } = FrontmatterSpan::from(source) else {
        return Err("expected a closed block".into());
    };
    assert_eq!(&source[yaml], "tags: [daily]\r\n");
    assert_eq!(&source[body_start..], "Body\r\n");
    Ok(())
}

#[test]
fn an_empty_block_and_a_block_closing_at_the_end_of_the_file_are_closed() -> TestResult {
    let empty = "---\n---\nBody";
    let FrontmatterSpan::Closed { yaml, body_start, .. } = FrontmatterSpan::from(empty) else {
        return Err("expected a closed block".into());
    };
    assert!(empty[yaml.clone()].is_empty(), "{yaml:?}");
    assert_eq!(&empty[body_start..], "Body");

    let at_end = "---\nkey: value\n---";
    let FrontmatterSpan::Closed { body_start, .. } = FrontmatterSpan::from(at_end) else {
        return Err("expected a closed block".into());
    };
    assert_eq!(body_start, at_end.len());
    Ok(())
}

#[test]
fn an_opening_line_with_no_closing_line_is_unclosed() {
    assert_eq!(FrontmatterSpan::from("---\nkey: value\n"), FrontmatterSpan::Unclosed);
    assert_eq!(FrontmatterSpan::from("---"), FrontmatterSpan::Unclosed);
}

#[test]
fn malformed_yaml_between_the_delimiters_is_still_a_closed_block() {
    let source = "---\n: : not yaml [\n---\nBody\n";
    assert!(matches!(FrontmatterSpan::from(source), FrontmatterSpan::Closed { .. }));
}
