use time::Month;

use super::*;

type BoxError = Box<dyn std::error::Error>;

fn date(year: i32, month: Month, day: u8) -> Result<Date, BoxError> {
    Ok(Date::from_calendar_date(year, month, day)?)
}

#[test]
fn tp_date_now_renders_every_supported_token() -> Result<(), BoxError> {
    let d = date(2026, Month::September, 14)?;
    let rendered = render_templater_expressions(
        "date: <% tp.date.now(\"YYYY-MM-DD\") %>\nday: <% tp.date.now(\"dddd\") %>\nweek: <% tp.date.now(\"ww\") %>\n",
        d,
    )?;
    assert!(rendered.contains("date: 2026-09-14"), "{rendered}");
    assert!(rendered.contains("day: Monday"), "{rendered}");
    assert!(rendered.contains("week: 38"), "{rendered}");
    Ok(())
}

#[test]
fn tp_date_now_renders_the_ordinal_long_form_title() -> Result<(), BoxError> {
    let d = date(2026, Month::September, 14)?;
    let rendered = render_templater_expressions("# <% tp.date.now(\"dddd, Do MMMM YYYY\") %>\n", d)?;
    assert_eq!(rendered, "# Monday, 14th September 2026\n");
    Ok(())
}

#[test]
fn tp_date_yesterday_and_tomorrow_shift_the_date() -> Result<(), BoxError> {
    let d = date(2026, Month::September, 14)?;
    let rendered = render_templater_expressions(
        "<< [[<% tp.date.yesterday(\"YYYY-MM-DD\") %>]] | [[<% tp.date.tomorrow(\"YYYY-MM-DD\") %>]] >>\n",
        d,
    )?;
    assert_eq!(rendered, "<< [[2026-09-13]] | [[2026-09-15]] >>\n");
    Ok(())
}

#[test]
fn crossing_a_month_boundary_shifts_the_month_and_year_correctly() -> Result<(), BoxError> {
    let jan_1 = date(2026, Month::January, 1)?;
    let rendered = render_templater_expressions("<% tp.date.yesterday(\"YYYY-MM-DD\") %>", jan_1)?;
    assert_eq!(rendered, "2025-12-31");
    Ok(())
}

#[test]
fn an_unsupported_expression_is_rejected_rather_than_silently_stripped() -> Result<(), BoxError> {
    let d = date(2026, Month::September, 14)?;
    let result = render_templater_expressions("<% tp.file.title %>", d);
    assert!(matches!(result, Err(TemplaterError::UnsupportedExpression(ref expr)) if expr == "tp.file.title"));
    Ok(())
}

#[test]
fn plain_text_with_no_templater_tags_passes_through_unchanged() -> Result<(), BoxError> {
    let d = date(2026, Month::September, 14)?;
    let rendered = render_templater_expressions("# Just a heading\n\nSome body text.\n", d)?;
    assert_eq!(rendered, "# Just a heading\n\nSome body text.\n");
    Ok(())
}
