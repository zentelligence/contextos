use time::Month;

use super::*;

type BoxError = Box<dyn std::error::Error>;

fn at(year: i32, month: Month, day: u8, hour: u8, minute: u8) -> Result<OffsetDateTime, BoxError> {
    let date = time::Date::from_calendar_date(year, month, day)?;
    Ok(date.with_hms(hour, minute, 0)?.assume_utc())
}

#[test]
fn filename_without_extension_strips_every_folder_and_the_md_suffix() {
    assert_eq!(
        filename_without_extension("2026/09/2026-09-14-0705.md"),
        "2026-09-14-0705"
    );
}

#[test]
fn filename_without_extension_handles_a_note_with_no_folder_at_all() {
    assert_eq!(filename_without_extension("flat.md"), "flat");
}

#[test]
fn note_path_joins_the_configured_directory_and_the_relative_path() {
    let config = NotesConfig {
        notes_dir: "scratch".to_owned(),
        ..NotesConfig::default()
    };
    assert_eq!(
        note_path(&config, "2026/09/2026-09-14-0705.md"),
        "scratch/2026/09/2026-09-14-0705.md"
    );
}

#[test]
fn default_relative_path_renders_the_configured_shipped_default_format() -> Result<(), BoxError> {
    let instant = at(2026, Month::January, 5, 7, 5)?;
    assert_eq!(
        default_relative_path(&NotesConfig::default(), instant),
        "2026/01/2026-01-05-0705.md"
    );
    Ok(())
}

#[test]
fn default_relative_path_honours_an_entirely_different_configured_format() -> Result<(), BoxError> {
    let instant = at(2026, Month::January, 5, 7, 5)?;
    let config = NotesConfig {
        date_format: "DD-MM-YYYY".to_owned(),
        ..NotesConfig::default()
    };
    assert_eq!(default_relative_path(&config, instant), "05-01-2026.md");
    Ok(())
}

#[test]
fn disambiguate_relative_path_returns_the_base_when_nothing_collides() {
    let existing = vec!["2026/09/other.md".to_owned()];
    assert_eq!(
        disambiguate_relative_path("2026/09/2026-09-14-0705.md", &existing),
        "2026/09/2026-09-14-0705.md"
    );
}

#[test]
fn disambiguate_relative_path_appends_the_first_free_numeric_suffix() {
    let base = "2026/09/2026-09-14-0705.md";
    let existing = vec![base.to_owned(), "2026/09/2026-09-14-0705-2.md".to_owned()];
    assert_eq!(
        disambiguate_relative_path(base, &existing),
        "2026/09/2026-09-14-0705-3.md"
    );
}

#[test]
fn disambiguate_relative_path_terminates_immediately_past_a_gap_in_the_taken_suffixes() {
    let base = "2026/09/2026-09-14-0705.md";
    let existing = vec![base.to_owned(), "2026/09/2026-09-14-0705-3.md".to_owned()];
    // `-2` is free even though `-3` is already taken: the first gap wins,
    // not the lowest untaken suffix past every taken one.
    assert_eq!(
        disambiguate_relative_path(base, &existing),
        "2026/09/2026-09-14-0705-2.md"
    );
}

fn summary(relative_path: &str, date: Option<&str>, time: Option<&str>) -> NoteSummary {
    NoteSummary {
        relative_path: relative_path.to_owned(),
        title: relative_path.to_owned(),
        has_custom_title: false,
        date: date.map(str::to_owned),
        time: time.map(str::to_owned),
        status: None,
        tags: Vec::new(),
    }
}

#[test]
fn sort_key_orders_by_date_then_time_then_relative_path() {
    let earlier = summary("a.md", Some("2026-09-13"), Some("07:00"));
    let later = summary("b.md", Some("2026-09-14"), Some("07:00"));
    assert!(sort_key(&later) > sort_key(&earlier));

    let earlier_same_day = summary("c.md", Some("2026-09-14"), Some("06:00"));
    let later_same_day = summary("d.md", Some("2026-09-14"), Some("07:00"));
    assert!(sort_key(&later_same_day) > sort_key(&earlier_same_day));
}

#[test]
fn sort_key_falls_back_to_the_relative_path_when_date_and_time_are_both_absent() {
    let first = summary("a.md", None, None);
    let second = summary("b.md", None, None);
    assert!(sort_key(&second) > sort_key(&first));
}

#[test]
fn classify_delete_failure_maps_the_stable_not_found_code_to_not_found() {
    assert!(matches!(
        classify_delete_failure("path/not-found"),
        DeleteNoteError::NotFound
    ));
}

#[test]
fn classify_delete_failure_treats_every_other_code_as_a_plain_rejection() {
    assert!(matches!(
        classify_delete_failure("io/directory-not-empty"),
        DeleteNoteError::Rejected
    ));
    // No parsable structured body at all (`delete_note`'s own
    // `unwrap_or_default` fallback) degrades to the same plain rejection,
    // never a false `NotFound`.
    assert!(matches!(classify_delete_failure(""), DeleteNoteError::Rejected));
}
