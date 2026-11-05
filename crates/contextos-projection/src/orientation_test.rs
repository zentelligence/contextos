use super::*;

const BEGIN: &str = "<!-- contextos:orientation:begin -->";
const END: &str = "<!-- contextos:orientation:end -->";

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn place(existing: Option<&str>, content: &str) -> Result<String, ProjectionError> {
    OrientationBlock::place(existing.map(str::as_bytes), content.as_bytes()).map(|bytes| lossless(&bytes))
}

fn withdraw(existing: &str) -> Result<String, ProjectionError> {
    OrientationBlock::withdraw(existing.as_bytes()).map(|bytes| lossless(&bytes))
}

/// Latin-1 style decoding, so a test can compare bytes that are not UTF-8 as text.
fn lossless(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

fn block(lines: &str) -> String {
    format!("{BEGIN}\n{lines}{END}\n")
}

fn code_of(result: Result<String, ProjectionError>) -> Result<&'static str, String> {
    match result {
        Ok(text) => Err(format!("expected an error, got bytes {text:?}")),
        Err(error) => Ok(error.code()),
    }
}

#[test]
fn markers_are_the_documented_whole_lines() {
    assert_eq!(OrientationBlock::BEGIN, BEGIN);
    assert_eq!(OrientationBlock::END, END);
}

#[test]
fn a_missing_file_becomes_a_file_holding_only_the_block() -> TestResult {
    assert_eq!(place(None, "Hello\nWorld\n")?, block("Hello\nWorld\n"));
    Ok(())
}

#[test]
fn source_without_a_final_newline_still_gives_a_block_ending_in_a_newline() -> TestResult {
    assert_eq!(place(None, "Hello")?, block("Hello\n"));
    Ok(())
}

#[test]
fn a_file_without_the_block_gets_it_appended_after_one_blank_line() -> TestResult {
    assert_eq!(
        place(Some("# Mine\nKeep me.\n"), "Hello\n")?,
        format!("# Mine\nKeep me.\n\n{}", block("Hello\n"))
    );
    Ok(())
}

#[test]
fn a_file_lacking_a_final_newline_gains_one_newline_before_the_blank_separator() -> TestResult {
    assert_eq!(
        place(Some("# Mine\nKeep me."), "Hello\n")?,
        format!("# Mine\nKeep me.\n\n{}", block("Hello\n"))
    );
    Ok(())
}

#[test]
fn existing_bytes_that_are_not_utf8_are_kept_when_the_block_is_appended() -> TestResult {
    let result = OrientationBlock::place(Some(b"\xff\xfe raw\n"), b"Hello\n")?;
    assert!(result.starts_with(b"\xff\xfe raw\n\n"), "{result:?}");
    assert!(result.ends_with(block("Hello\n").as_bytes()), "{result:?}");
    Ok(())
}

#[test]
fn only_the_lines_between_the_markers_change() -> TestResult {
    let existing = format!("top\n\n{}\nbottom\n", block("old one\nold two\n").trim_end());
    assert_eq!(
        place(Some(&existing), "new\n")?,
        format!("top\n\n{}\nbottom\n", block("new\n").trim_end())
    );
    Ok(())
}

#[test]
fn a_block_at_the_end_of_a_file_without_a_final_newline_stays_without_one() -> TestResult {
    let existing = format!("top\n{BEGIN}\nold\n{END}");
    assert_eq!(place(Some(&existing), "new\n")?, format!("top\n{BEGIN}\nnew\n{END}"));
    Ok(())
}

#[test]
fn bytes_around_a_block_survive_even_when_they_are_not_utf8() -> TestResult {
    let mut existing = b"\xff\xfe head\n".to_vec();
    existing.extend_from_slice(format!("{BEGIN}\nold\n{END}\n").as_bytes());
    existing.extend_from_slice(b"tail \xc3\x28\n");
    let result = OrientationBlock::place(Some(&existing), b"new\n")?;
    let mut expected = b"\xff\xfe head\n".to_vec();
    expected.extend_from_slice(block("new\n").as_bytes());
    expected.extend_from_slice(b"tail \xc3\x28\n");
    assert_eq!(result, expected);
    Ok(())
}

#[test]
fn text_that_merely_mentions_a_marker_is_not_a_marker() -> TestResult {
    let existing = format!("see {BEGIN} in prose\n");
    assert_eq!(
        place(Some(&existing), "Hello\n")?,
        format!("see {BEGIN} in prose\n\n{}", block("Hello\n"))
    );
    Ok(())
}

#[test]
fn a_crlf_file_receives_the_appended_block_with_crlf_endings() -> TestResult {
    assert_eq!(
        place(Some("# Mine\r\n"), "Hello\nWorld\n")?,
        format!("# Mine\r\n\r\n{BEGIN}\r\nHello\r\nWorld\r\n{END}\r\n")
    );
    Ok(())
}

#[test]
fn a_crlf_file_receives_a_replaced_block_with_crlf_endings_and_keeps_the_rest() -> TestResult {
    let existing = format!("top\r\n{BEGIN}\r\nold\r\n{END}\r\nbottom\r\n");
    assert_eq!(
        place(Some(&existing), "new one\nnew two\n")?,
        format!("top\r\n{BEGIN}\r\nnew one\r\nnew two\r\n{END}\r\nbottom\r\n")
    );
    Ok(())
}

#[test]
fn a_source_written_with_crlf_is_normalised_to_an_lf_file() -> TestResult {
    assert_eq!(
        place(Some("# Mine\n"), "Hello\r\nWorld\r\n")?,
        format!("# Mine\n\n{}", block("Hello\nWorld\n"))
    );
    Ok(())
}

#[test]
fn a_missing_file_uses_lf_endings() -> TestResult {
    assert_eq!(place(None, "Hello\r\nWorld\r\n")?, block("Hello\nWorld\n"));
    Ok(())
}

#[test]
fn two_begin_markers_are_malformed() -> TestResult {
    let existing = format!("{BEGIN}\na\n{END}\n{BEGIN}\nb\n{END}\n");
    assert_eq!(code_of(place(Some(&existing), "new\n"))?, "projection/malformed-block");
    Ok(())
}

#[test]
fn an_end_marker_without_a_begin_is_malformed() -> TestResult {
    let existing = format!("top\n{END}\n");
    assert_eq!(code_of(place(Some(&existing), "new\n"))?, "projection/malformed-block");
    Ok(())
}

#[test]
fn an_end_marker_before_the_begin_marker_is_malformed() -> TestResult {
    let existing = format!("{END}\ntop\n{BEGIN}\nold\n");
    assert_eq!(code_of(place(Some(&existing), "new\n"))?, "projection/malformed-block");
    Ok(())
}

#[test]
fn a_begin_marker_without_an_end_is_malformed() -> TestResult {
    let existing = format!("top\n{BEGIN}\nold\n");
    assert_eq!(code_of(place(Some(&existing), "new\n"))?, "projection/malformed-block");
    Ok(())
}

#[test]
fn removing_from_a_malformed_file_is_refused() -> TestResult {
    let existing = format!("top\n{BEGIN}\nold\n");
    assert_eq!(code_of(withdraw(&existing))?, "projection/malformed-block");
    Ok(())
}

#[test]
fn a_source_holding_a_begin_marker_line_is_refused() -> TestResult {
    let content = format!("Hello\n{BEGIN}\nWorld\n");
    assert_eq!(code_of(place(None, &content))?, "projection/invalid-definition");
    Ok(())
}

#[test]
fn a_source_holding_an_end_marker_line_is_refused_even_with_crlf() -> TestResult {
    let content = format!("Hello\r\n{END}\r\nWorld\r\n");
    assert_eq!(
        code_of(place(Some("# Mine\n"), &content))?,
        "projection/invalid-definition"
    );
    Ok(())
}

#[test]
fn removal_deletes_the_markers_and_the_lines_between_and_nothing_else() -> TestResult {
    let existing = format!("top\n\n{BEGIN}\nline one\nline two\n{END}\nbottom\n");
    assert_eq!(withdraw(&existing)?, "top\n\nbottom\n");
    Ok(())
}

#[test]
fn removal_from_a_file_holding_only_the_block_leaves_an_empty_file() -> TestResult {
    assert_eq!(withdraw(&block("Hello\n"))?, "");
    Ok(())
}

#[test]
fn removal_keeps_the_files_crlf_endings_elsewhere() -> TestResult {
    let existing = format!("top\r\n{BEGIN}\r\nold\r\n{END}\r\nbottom\r\n");
    assert_eq!(withdraw(&existing)?, "top\r\nbottom\r\n");
    Ok(())
}

#[test]
fn removal_from_a_file_without_the_block_changes_nothing() -> TestResult {
    assert_eq!(withdraw("# Mine\nKeep me.")?, "# Mine\nKeep me.");
    Ok(())
}

#[test]
fn projecting_the_same_content_twice_gives_identical_bytes() -> TestResult {
    let once = place(Some("# Mine\n"), "Hello\n")?;
    let twice = place(Some(&once), "Hello\n")?;
    assert_eq!(twice, once);
    assert!(once.contains(BEGIN), "the block must be present at all: {once:?}");
    Ok(())
}

#[test]
fn projecting_into_a_file_created_from_the_same_content_is_stable() -> TestResult {
    let created = place(None, "Hello\n")?;
    assert_eq!(place(Some(&created), "Hello\n")?, created);
    assert_eq!(created, block("Hello\n"));
    Ok(())
}
