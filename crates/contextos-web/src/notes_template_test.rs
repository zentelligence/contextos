use time::Month;

use super::*;

type BoxError = Box<dyn std::error::Error>;

fn at(year: i32, month: Month, day: u8, hour: u8, minute: u8) -> Result<OffsetDateTime, BoxError> {
    let date = time::Date::from_calendar_date(year, month, day)?;
    Ok(date.with_hms(hour, minute, 0)?.assume_utc())
}

const UNIQUE_NOTE_FIXTURE: &str = "---\n\
time: <% tp.date.now(\"HH:mm\") %>\n\
date: <% tp.date.now(\"YYYY-MM-DD\") %>\n\
day: <% tp.date.now(\"dddd\") %>\n\
week: <% tp.date.now(\"ww\") %>\n\
tags: [note]\n\
status: draft\n\
---\n\
\n\
# <% tp.date.now(\"dddd, Do MMMM YYYY\") %>\n\
\n\
\n";

#[test]
fn renders_the_shipped_unique_note_fixture_into_structured_frontmatter_and_a_heading_body() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 7, 5)?;
    let rendered = render_new_note(UNIQUE_NOTE_FIXTURE, instant)?;
    assert_eq!(
        rendered.frontmatter.get("time"),
        Some(&Value::String("07:05".to_owned()))
    );
    assert_eq!(
        rendered.frontmatter.get("date"),
        Some(&Value::String("2026-09-14".to_owned()))
    );
    assert_eq!(
        rendered.frontmatter.get("day"),
        Some(&Value::String("Monday".to_owned()))
    );
    assert_eq!(rendered.frontmatter.get("week"), Some(&Value::String("38".to_owned())));
    assert_eq!(
        rendered.frontmatter.get("tags"),
        Some(&Value::Array(vec![Value::String("note".to_owned())]))
    );
    assert_eq!(
        rendered.frontmatter.get("status"),
        Some(&Value::String("draft".to_owned()))
    );
    // The rendered body keeps the template's own blank line between the
    // closing `---` and the heading byte-for-byte, matching
    // `frontmatter_read`'s identical "body starts right after the closing
    // delimiter" contract for a real file (`notes_test.rs`'s own
    // `body_from_lines` tests).
    assert!(
        rendered
            .body
            .trim_start()
            .starts_with("# Monday, 14th September 2026\n"),
        "{}",
        rendered.body
    );
    Ok(())
}

#[test]
fn an_empty_template_yields_no_overrides_and_an_empty_body() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 7, 5)?;
    let rendered = render_new_note("", instant)?;
    assert!(rendered.frontmatter.is_empty());
    assert_eq!(rendered.body, "");
    Ok(())
}

#[test]
fn a_template_with_no_frontmatter_block_is_treated_as_body_only() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 7, 5)?;
    let rendered = render_new_note("# Just a heading\n\nSome text.\n", instant)?;
    assert!(rendered.frontmatter.is_empty());
    assert_eq!(rendered.body, "# Just a heading\n\nSome text.\n");
    Ok(())
}

#[test]
fn an_empty_value_parses_as_null() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 7, 5)?;
    let rendered = render_new_note("---\nsummary:\n---\nbody\n", instant)?;
    assert_eq!(rendered.frontmatter.get("summary"), Some(&Value::Null));
    Ok(())
}

#[test]
fn single_and_double_quoted_scalars_are_unquoted() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 7, 5)?;
    let rendered = render_new_note("---\na: \"double\"\nb: 'single'\n---\n", instant)?;
    assert_eq!(rendered.frontmatter.get("a"), Some(&Value::String("double".to_owned())));
    assert_eq!(rendered.frontmatter.get("b"), Some(&Value::String("single".to_owned())));
    Ok(())
}

#[test]
fn an_empty_flow_list_parses_as_an_empty_array() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 7, 5)?;
    let rendered = render_new_note("---\ntags: []\n---\n", instant)?;
    assert_eq!(rendered.frontmatter.get("tags"), Some(&Value::Array(Vec::new())));
    Ok(())
}

#[test]
fn an_indented_nested_mapping_line_is_rejected() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 7, 5)?;
    let result = render_new_note("---\nparent:\n  child: value\n---\n", instant);
    assert!(matches!(result, Err(NotesTemplateError::UnsupportedFrontmatterLine(_))));
    Ok(())
}

#[test]
fn a_block_sequence_line_is_rejected() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 7, 5)?;
    let result = render_new_note("---\n- item one\n- item two\n---\n", instant);
    assert!(matches!(result, Err(NotesTemplateError::UnsupportedFrontmatterLine(_))));
    Ok(())
}

#[test]
fn an_unterminated_flow_list_is_rejected() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 7, 5)?;
    let result = render_new_note("---\ntags: [a, b\n---\n", instant);
    assert!(matches!(result, Err(NotesTemplateError::UnsupportedFrontmatterLine(_))));
    Ok(())
}

#[test]
fn a_block_scalar_value_is_rejected() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 7, 5)?;
    let result = render_new_note("---\nsummary: |\n  multi\n  line\n---\n", instant);
    assert!(matches!(result, Err(NotesTemplateError::UnsupportedFrontmatterLine(_))));
    Ok(())
}

#[test]
fn a_line_with_no_colon_is_rejected() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 7, 5)?;
    let result = render_new_note("---\njust some text\n---\n", instant);
    assert!(matches!(result, Err(NotesTemplateError::UnsupportedFrontmatterLine(_))));
    Ok(())
}

#[test]
fn an_unsupported_templater_expression_propagates_as_its_own_variant() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 7, 5)?;
    let result = render_new_note("---\ntitle: <% tp.file.title %>\n---\n", instant);
    assert!(matches!(
        result,
        Err(NotesTemplateError::Templater(TemplaterError::UnsupportedExpression(_)))
    ));
    Ok(())
}

#[test]
fn first_heading_title_finds_the_first_top_level_heading_only() {
    assert_eq!(
        first_heading_title("## Not this one\n# This one\n# Not this either\n"),
        Some("This one".to_owned())
    );
}

#[test]
fn first_heading_title_is_none_when_the_body_has_no_top_level_heading() {
    assert_eq!(first_heading_title("## Only a sub-heading\nplain text\n"), None);
    assert_eq!(first_heading_title(""), None);
}
