use time::Month;

use super::*;

type BoxError = Box<dyn std::error::Error>;

fn at(year: i32, month: Month, day: u8, hour: u8, minute: u8) -> Result<OffsetDateTime, BoxError> {
    let date = Date::from_calendar_date(year, month, day)?;
    Ok(date.with_hms(hour, minute, 0)?.assume_utc())
}

#[test]
fn tp_date_now_renders_the_time_of_day_tokens_journal_never_needs() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 7, 5)?;
    let rendered = render_templater_expressions(
        "time: <% tp.date.now(\"HH:mm\") %>\ndate: <% tp.date.now(\"YYYY-MM-DD\") %>\n",
        instant,
    )?;
    assert_eq!(rendered, "time: 07:05\ndate: 2026-09-14\n");
    Ok(())
}

#[test]
fn hh_and_mm_are_zero_padded() -> Result<(), BoxError> {
    let instant = at(2026, Month::January, 1, 0, 0)?;
    let rendered = render_templater_expressions("<% tp.date.now(\"HH:mm\") %>", instant)?;
    assert_eq!(rendered, "00:00");
    Ok(())
}

#[test]
fn tp_date_yesterday_shifts_the_calendar_day_but_keeps_the_time_of_day() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 23, 41)?;
    let rendered = render_templater_expressions("<% tp.date.yesterday(\"YYYY-MM-DD HH:mm\") %>", instant)?;
    assert_eq!(rendered, "2026-09-13 23:41");
    Ok(())
}

#[test]
fn an_unsupported_expression_is_rejected_rather_than_silently_stripped() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 12, 0)?;
    let result = render_templater_expressions("<% tp.file.title %>", instant);
    assert!(matches!(result, Err(TemplaterError::UnsupportedExpression(ref expr)) if expr == "tp.file.title"));
    Ok(())
}

#[test]
fn parse_moment_like_is_the_exact_inverse_of_rendering_the_same_format() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 14, 18, 58)?;
    let format = "YYYY/MM/YYYY-MM-DD-HHmm";
    let rendered = format_moment_like(instant, format);
    assert_eq!(rendered, "2026/09/2026-09-14-1858");
    let matched = parse_moment_like(format, &rendered).ok_or("expected a match")?;
    assert_eq!(matched_field(&matched, "YYYY"), Some(2026));
    assert_eq!(matched_field(&matched, "MM"), Some(9));
    assert_eq!(matched_field(&matched, "DD"), Some(14));
    assert_eq!(matched_field(&matched, "HH"), Some(18));
    assert_eq!(matched_field(&matched, "mm"), Some(58));
    Ok(())
}

#[test]
fn parse_moment_like_reads_fields_by_name_regardless_of_the_configured_order() -> Result<(), BoxError> {
    // A day-first format, unlike the shipped default: the field order in
    // the format string is not assumed anywhere, only its own name.
    let matched = parse_moment_like("DD-MM-YYYY", "14-09-2026").ok_or("expected a match")?;
    assert_eq!(matched_field(&matched, "YYYY"), Some(2026));
    assert_eq!(matched_field(&matched, "MM"), Some(9));
    assert_eq!(matched_field(&matched, "DD"), Some(14));
    Ok(())
}

#[test]
fn parse_moment_like_rejects_a_literal_mismatch() {
    assert!(parse_moment_like("YYYY-MM-DD", "2026/09/14").is_none());
}

#[test]
fn parse_moment_like_rejects_a_value_out_of_range_for_its_token() {
    // "13" is not a valid month.
    assert!(parse_moment_like("YYYY-MM", "2026-13").is_none());
}

#[test]
fn parse_moment_like_rejects_trailing_or_missing_characters() {
    assert!(parse_moment_like("YYYY-MM", "2026-09-extra").is_none());
    assert!(parse_moment_like("YYYY-MM-DD", "2026-09").is_none());
}

#[test]
fn parse_moment_like_reads_month_and_weekday_names_case_sensitively() -> Result<(), BoxError> {
    let matched = parse_moment_like("dddd, Do MMMM YYYY", "Monday, 14th September 2026").ok_or("expected a match")?;
    assert_eq!(matched_field(&matched, "MMMM"), Some(9));
    assert_eq!(matched_field(&matched, "Do"), Some(14));
    assert_eq!(matched_field(&matched, "YYYY"), Some(2026));
    assert!(parse_moment_like("MMMM", "september").is_none());
    Ok(())
}

#[test]
fn midnight_keeps_the_calendar_date_at_time_zero() -> Result<(), BoxError> {
    let date = Date::from_calendar_date(2026, Month::September, 14)?;
    let instant = midnight(date);
    assert_eq!(instant.date(), date);
    assert_eq!(instant.hour(), 0);
    assert_eq!(instant.minute(), 0);
    Ok(())
}
