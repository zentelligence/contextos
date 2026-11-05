use time::Month;

use super::*;

type BoxError = Box<dyn std::error::Error>;

fn date(year: i32, month: Month, day: u8) -> Result<Date, BoxError> {
    Ok(Date::from_calendar_date(year, month, day)?)
}

// ---------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------

#[test]
fn parse_date_accepts_the_iso_form_and_rejects_everything_else() -> Result<(), BoxError> {
    assert_eq!(parse_date("2026-09-14"), Some(date(2026, Month::September, 14)?));
    assert_eq!(parse_date(" 2026-09-14 "), Some(date(2026, Month::September, 14)?));
    assert_eq!(parse_date("14-09-2026"), None);
    assert_eq!(parse_date("not a date"), None);
    assert_eq!(parse_date(""), None);
    // Calendar-shaped but impossible (February never has a 30th day):
    // `routes::journal::get`'s own contract relies on this rejecting, not
    // silently clamping, an out-of-range day - the day-path-segment
    // equivalent of `routes::wiki`'s own "an unresolvable path is a genuine
    // 404" rule.
    assert_eq!(parse_date("2026-02-30"), None);
    Ok(())
}

#[test]
fn format_date_round_trips_through_parse_date() -> Result<(), BoxError> {
    let d = date(2026, Month::January, 5)?;
    assert_eq!(format_date(d), "2026-01-05");
    assert_eq!(parse_date(&format_date(d)), Some(d));
    Ok(())
}

#[test]
fn add_days_crosses_month_and_year_boundaries() -> Result<(), BoxError> {
    assert_eq!(
        add_days(date(2026, Month::September, 14)?, 1),
        date(2026, Month::September, 15)?
    );
    assert_eq!(
        add_days(date(2026, Month::September, 14)?, -1),
        date(2026, Month::September, 13)?
    );
    assert_eq!(
        add_days(date(2026, Month::January, 1)?, -1),
        date(2025, Month::December, 31)?
    );
    Ok(())
}

#[test]
fn month_label_names_the_month_and_year() -> Result<(), BoxError> {
    assert_eq!(month_label(date(2026, Month::September, 14)?), "September 2026");
    Ok(())
}

// ---------------------------------------------------------------------
// Entry path
// ---------------------------------------------------------------------

#[test]
fn entry_path_follows_the_configured_directory_and_the_shipped_default_format() -> Result<(), BoxError> {
    let config = JournalConfig {
        journal_dir: "diary".to_owned(),
        ..JournalConfig::default()
    };
    assert_eq!(
        entry_path(&config, date(2026, Month::January, 5)?),
        "diary/2026/01/2026-01-05.md"
    );
    Ok(())
}

#[test]
fn entry_path_honours_an_entirely_different_configured_format() -> Result<(), BoxError> {
    let config = JournalConfig {
        journal_dir: "diary".to_owned(),
        date_format: "YYYY-MM-DD".to_owned(),
        ..JournalConfig::default()
    };
    assert_eq!(
        entry_path(&config, date(2026, Month::January, 5)?),
        "diary/2026-01-05.md"
    );
    Ok(())
}

#[test]
fn entry_date_from_relative_path_reads_the_configured_format_regardless_of_token_order() -> Result<(), BoxError> {
    let expected = date(2026, Month::January, 5)?;
    assert_eq!(
        entry_date_from_relative_path("YYYY/MM/YYYY-MM-DD", "2026/01/2026-01-05.md"),
        Some(expected)
    );
    assert_eq!(
        entry_date_from_relative_path("DD-MM-YYYY", "05-01-2026.md"),
        Some(expected)
    );
    Ok(())
}

#[test]
fn entry_date_from_relative_path_ignores_a_stray_non_dated_file() {
    assert_eq!(
        entry_date_from_relative_path("YYYY/MM/YYYY-MM-DD", "notes-to-self.md"),
        None
    );
}
