use time::Month;

use super::*;

type BoxError = Box<dyn std::error::Error>;

fn date(year: i32, month: Month, day: u8) -> Result<Date, BoxError> {
    Ok(Date::from_calendar_date(year, month, day)?)
}

#[test]
fn build_calendar_pads_the_leading_week_to_the_months_own_first_weekday() -> Result<(), BoxError> {
    // 2026-09-01 is a Tuesday: two leading blank cells (Sunday, Monday).
    let viewed = date(2026, Month::September, 14)?;
    let today = viewed;
    let calendar = build_calendar(viewed, today, &[]);
    assert_eq!(calendar.month_label, "September 2026");
    let first_week = &calendar.weeks[0];
    assert!(first_week.days[0].is_none(), "{first_week:?}");
    assert!(first_week.days[1].is_none(), "{first_week:?}");
    assert_eq!(first_week.days[2].as_ref().map(|d| d.day), Some(1));
    Ok(())
}

#[test]
fn build_calendar_marks_today_selected_and_entries_with_a_dot() -> Result<(), BoxError> {
    let today = date(2026, Month::September, 14)?;
    let with_entry = date(2026, Month::September, 10)?;
    let calendar = build_calendar(today, today, &[with_entry]);
    let all_days: Vec<&CalendarDay> = calendar
        .weeks
        .iter()
        .flat_map(|week| week.days.iter().flatten())
        .collect();
    let today_cell = all_days.iter().find(|d| d.day == 14).ok_or("missing day 14")?;
    assert!(today_cell.is_today);
    assert!(today_cell.is_selected);
    assert!(!today_cell.has_entry);
    let entry_cell = all_days.iter().find(|d| d.day == 10).ok_or("missing day 10")?;
    assert!(!entry_cell.is_today);
    assert!(entry_cell.has_entry);
    Ok(())
}

#[test]
fn build_calendar_every_week_has_exactly_seven_cells() -> Result<(), BoxError> {
    let viewed = date(2026, Month::September, 14)?;
    let calendar = build_calendar(viewed, viewed, &[]);
    for week in &calendar.weeks {
        assert_eq!(week.days.len(), 7);
    }
    Ok(())
}

#[test]
fn build_tree_groups_dates_by_year_and_month_newest_first() -> Result<(), BoxError> {
    let today = date(2026, Month::September, 14)?;
    let entries = vec![date(2026, Month::September, 1)?, date(2025, Month::December, 31)?];
    let years = build_tree(&entries, today, today);
    assert_eq!(years.len(), 2);
    assert_eq!(years[0].label, "2026");
    assert_eq!(years[1].label, "2025");
    assert_eq!(years[0].months.len(), 1);
    assert_eq!(years[0].months[0].label, "September");
    // Newest day first within the month too.
    assert_eq!(years[0].months[0].days[0].date_value, "2026-09-14");
    assert_eq!(years[0].months[0].days[1].date_value, "2026-09-01");
    Ok(())
}

#[test]
fn build_tree_marks_only_the_viewed_dates_own_year_and_month_open() -> Result<(), BoxError> {
    let today = date(2026, Month::September, 14)?;
    let viewed = date(2026, Month::September, 1)?;
    let other_year = date(2025, Month::December, 31)?;
    let years = build_tree(&[viewed, other_year], viewed, today);

    let current = years.iter().find(|year| year.label == "2026").ok_or("missing 2026")?;
    assert!(current.is_open);
    assert!(current.months[0].is_open);

    let past = years.iter().find(|year| year.label == "2025").ok_or("missing 2025")?;
    assert!(!past.is_open);
    assert!(!past.months[0].is_open);
    Ok(())
}

#[test]
fn build_tree_always_includes_today_even_with_no_real_entries() -> Result<(), BoxError> {
    let today = date(2026, Month::September, 14)?;
    let years = build_tree(&[], today, today);
    assert_eq!(years.len(), 1);
    assert_eq!(years[0].months[0].days[0].date_value, "2026-09-14");
    assert!(years[0].months[0].days[0].is_today);
    Ok(())
}

#[test]
fn build_tree_deduplicates_a_date_that_is_both_todays_and_a_real_entry() -> Result<(), BoxError> {
    let today = date(2026, Month::September, 14)?;
    let years = build_tree(&[today], today, today);
    assert_eq!(years.len(), 1);
    assert_eq!(years[0].months.len(), 1);
    assert_eq!(years[0].months[0].days.len(), 1);
    Ok(())
}

#[test]
fn build_tree_marks_the_currently_viewed_date_selected() -> Result<(), BoxError> {
    let today = date(2026, Month::September, 14)?;
    let viewed = date(2026, Month::September, 1)?;
    let years = build_tree(&[viewed], viewed, today);
    let selected: Vec<&TreeDay> = years
        .iter()
        .flat_map(|y| y.months.iter())
        .flat_map(|m| m.days.iter())
        .filter(|d| d.is_selected)
        .collect();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].date_value, "2026-09-01");
    Ok(())
}
