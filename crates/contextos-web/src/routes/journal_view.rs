//! Pure view-model construction for the Journal screen: the calendar
//! popover's own month grid and the sidebar's year/month/day tree, both
//! derived from a plain list of real entry dates with no MCP call of their
//! own (`routes::journal` gathers `journal::entry_dates` once and passes the
//! result in here, alongside the operator's local date from `vault_time::VaultTime`).

use time::Date;

use crate::journal::{self, month_label, month_name};

// ---------------------------------------------------------------------
// Calendar popover (month grid)
// ---------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CalendarDay {
    pub day: u8,
    pub date_value: String,
    pub is_today: bool,
    pub is_selected: bool,
    pub has_entry: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CalendarWeek {
    pub days: Vec<Option<CalendarDay>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Calendar {
    pub month_label: String,
    pub weeks: Vec<CalendarWeek>,
}

/// The calendar popover's own month grid for `viewed`'s own month: one row
/// per week, Sunday-first (matching the design mock's own `S M T W T F S`
/// header), with a blank cell for every day outside the month at either
/// end. `entry_dates` marks a real, already-saved entry's own day with a
/// dot; `viewed` is always outlined as `is_today` too when it genuinely is
/// today, independent of whether a file exists yet for it.
#[must_use]
pub fn build_calendar(viewed: Date, today: Date, entry_dates: &[Date]) -> Calendar {
    let year = viewed.year();
    let month = viewed.month();
    let Some(first_of_month) = Date::from_calendar_date(year, month, 1).ok() else {
        return Calendar {
            month_label: month_label(viewed),
            weeks: Vec::new(),
        };
    };
    let days_in_month = month.length(year);
    let leading_blanks = usize::from(first_of_month.weekday().number_days_from_sunday());

    let mut cells: Vec<Option<CalendarDay>> = std::iter::repeat_n(None, leading_blanks).collect();
    for day in 1..=days_in_month {
        let Some(date) = Date::from_calendar_date(year, month, day).ok() else {
            continue;
        };
        cells.push(Some(CalendarDay {
            day,
            date_value: journal::format_date(date),
            is_today: date == today,
            is_selected: date == viewed,
            has_entry: entry_dates.contains(&date),
        }));
    }
    while !cells.len().is_multiple_of(7) {
        cells.push(None);
    }

    let weeks = cells
        .chunks(7)
        .map(|chunk| CalendarWeek { days: chunk.to_vec() })
        .collect();
    Calendar {
        month_label: month_label(viewed),
        weeks,
    }
}

// ---------------------------------------------------------------------
// Sidebar year/month/day tree
// ---------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeDay {
    pub label: String,
    pub date_value: String,
    pub is_today: bool,
    pub is_selected: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeMonth {
    pub label: String,
    /// Whether this month's own day list starts expanded: only the month
    /// containing the currently viewed date, so a growing entry history
    /// never renders as one endlessly scrolling flat list.
    pub is_open: bool,
    pub days: Vec<TreeDay>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeYear {
    pub label: String,
    /// Whether this year's own month list starts expanded: only the year
    /// containing the currently viewed date, matching [`TreeMonth::is_open`].
    pub is_open: bool,
    pub months: Vec<TreeMonth>,
}

/// The sidebar's own navigable date tree, newest year/month/day first: every
/// real entry date, plus today even when no file exists for it yet (an
/// operator can always navigate to today, matching the design mock's own
/// unconditional inclusion of it). Every year and month collapses to its own
/// `<details>`; only `viewed`'s own year and month start open, so an
/// operator drills into any other period deliberately rather than scrolling
/// past it.
#[must_use]
pub fn build_tree(entry_dates: &[Date], viewed: Date, today: Date) -> Vec<TreeYear> {
    let mut dates: Vec<Date> = entry_dates.to_vec();
    if !dates.contains(&today) {
        dates.push(today);
    }
    dates.sort_unstable();
    dates.dedup();
    dates.reverse();

    let viewed_year_label = format!("{:04}", viewed.year());
    let viewed_month_label = month_name(viewed.month()).to_owned();

    let mut years: Vec<TreeYear> = Vec::new();
    for date in dates {
        let year_label = format!("{:04}", date.year());
        let month_label = month_name(date.month()).to_owned();
        let day = TreeDay {
            label: journal::format_date(date),
            date_value: journal::format_date(date),
            is_today: date == today,
            is_selected: date == viewed,
        };
        // Indexing by a freshly-pushed `Vec`'s own new `len() - 1`, rather
        // than an `Option` from `iter().position()`/`last_mut()`, needs no
        // `unwrap`/`expect` (both denied workspace-wide) to express "the
        // entry this loop iteration just created is right there".
        let year_index = if let Some(index) = years.iter().position(|year| year.label == year_label) {
            index
        } else {
            years.push(TreeYear {
                label: year_label.clone(),
                is_open: year_label == viewed_year_label,
                months: Vec::new(),
            });
            years.len() - 1
        };
        let year = &mut years[year_index];
        match year.months.iter().position(|month| month.label == month_label) {
            Some(index) => year.months[index].days.push(day),
            None => year.months.push(TreeMonth {
                is_open: year_label == viewed_year_label && month_label == viewed_month_label,
                label: month_label,
                days: vec![day],
            }),
        }
    }
    years
}

#[cfg(test)]
#[path = "journal_view_test.rs"]
mod tests;
