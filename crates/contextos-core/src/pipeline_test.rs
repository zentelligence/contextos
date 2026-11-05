use time::macros::datetime;

use super::*;

#[test]
fn frontmatter_timestamp_truncates_seconds_and_subseconds_to_the_minute() {
    let instant = datetime!(2026-10-02 06:05:47.123_456_789 +00:00);
    assert_eq!(frontmatter_timestamp(instant), "2026-10-02T06:05:00Z");
}

#[test]
fn frontmatter_timestamp_preserves_the_offset() {
    let instant = datetime!(2026-10-02 16:05:47 +10:00);
    assert_eq!(frontmatter_timestamp(instant), "2026-10-02T16:05:00+10:00");
}

#[test]
fn frontmatter_timestamp_of_two_instants_in_the_same_minute_compare_equal() {
    let first = datetime!(2026-10-02 06:05:01 +00:00);
    let second = datetime!(2026-10-02 06:05:59 +00:00);
    assert_eq!(frontmatter_timestamp(first), frontmatter_timestamp(second));
}

#[test]
fn text_content_reports_its_own_string_bytes_and_length() {
    let content = WriteContent::from("hello".to_owned());
    assert_eq!(content.as_bytes(), b"hello");
    assert_eq!(content.len(), 5);
    assert!(!content.is_empty());
}

#[test]
fn binary_content_reports_its_own_bytes_and_length() {
    let content = WriteContent::Binary(vec![0, 1, 2, 255]);
    assert_eq!(content.as_bytes(), &[0, 1, 2, 255]);
    assert_eq!(content.len(), 4);
    assert!(!content.is_empty());
}

#[test]
fn empty_content_of_either_kind_reports_empty() {
    assert!(WriteContent::from(String::new()).is_empty());
    assert!(WriteContent::Binary(Vec::new()).is_empty());
}

#[test]
fn a_string_converts_into_text_content() {
    let content: WriteContent = "note".to_owned().into();
    assert_eq!(content, WriteContent::Text(UnixText::from("note")));
}

#[test]
fn a_byte_vec_converts_into_binary_content() {
    let content: WriteContent = vec![1_u8, 2, 3].into();
    assert_eq!(content, WriteContent::Binary(vec![1, 2, 3]));
}

#[test]
fn text_content_is_written_with_unix_line_endings() {
    let content = WriteContent::from("one\r\ntwo\r\nthree".to_owned());
    assert_eq!(content.as_bytes(), b"one\ntwo\nthree");
    assert!(content.converted_line_endings());
}

#[test]
fn a_lone_carriage_return_is_a_line_ending_and_is_written_as_unix() {
    let content = WriteContent::from("one\rtwo\r\nthree\r".to_owned());
    assert_eq!(content.as_bytes(), b"one\ntwo\nthree\n");
    assert!(content.converted_line_endings());
}

#[test]
fn binary_content_is_never_converted() {
    let content = WriteContent::Binary(b"one\r\ntwo".to_vec());
    assert_eq!(content.as_bytes(), b"one\r\ntwo");
    assert!(!content.converted_line_endings());
}
