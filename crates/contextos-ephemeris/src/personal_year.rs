//! Offline personal-year period calculation: which of the seven
//! annually-recurring, birthday-anchored periods contains a given date, and
//! that period's ruling planet in fixed Chaldean order.
//!
//! Source: H. Spencer Lewis, *Self Mastery and Fate with the Cycles of
//! Life*.

/// One of the seven planetary rulers of a personal-year period, in fixed
/// Chaldean order: Sun, Moon, Mars, Mercury, Jupiter, Venus, Saturn.
/// Source: H. Spencer Lewis, *Self Mastery and Fate with the Cycles of
/// Life*.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RulingPlanet {
    Sun,
    Moon,
    Mars,
    Mercury,
    Jupiter,
    Venus,
    Saturn,
}

/// The personal-year result for one `as_of_date`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PersonalYearPeriod {
    /// `1` to `7`; period `1` always begins exactly at the birthday.
    pub period_number: u8,
    pub ruling_planet: RulingPlanet,
    /// Whether `as_of_date` falls within the caller's tolerance window of
    /// either edge of the current period: the source material describes
    /// the transition between periods as gradual, not a precise
    /// day-to-day cutover, so this is a soft boundary rather than a sharp
    /// one.
    pub transition: bool,
}

/// Computes which of the seven annually-recurring personal-year periods
/// contains `as_of_date`, and that period's ruling planet.
pub trait ComputesPersonalYearPeriod {
    /// `transition_tolerance_days` governs the `transition` flag on the
    /// result (`±`, default `2.0`). No birth-time parameter: the source
    /// method carries none, and `birth_date`/`as_of_date` are
    /// already-validated calendar dates, so this never fails.
    fn personal_year_period(
        &self,
        birth_date: time::Date,
        as_of_date: time::Date,
        transition_tolerance_days: f64,
    ) -> PersonalYearPeriod;
}

/// Mean length of a personal-year period in days: the 365.25-day mean
/// tropical year divided evenly across all seven periods, rather than kept
/// at a flat 52 days with a separate absorbing gap.
pub(crate) const PERSONAL_YEAR_PERIOD_LENGTH_DAYS: f64 = 365.25 / 7.0;

pub(crate) fn personal_year_period(
    birth_date: time::Date,
    as_of_date: time::Date,
    transition_tolerance_days: f64,
) -> PersonalYearPeriod {
    let anniversary = most_recent_birthday_anniversary(birth_date, as_of_date);
    let days_since_birthday = (as_of_date - anniversary).as_seconds_f64() / 86_400.0;

    // Clamped, not just floored: a birthday-to-birthday span is 365 or 366
    // real calendar days, not exactly `7 * PERSONAL_YEAR_PERIOD_LENGTH_DAYS`
    // (365.25), so the last real day of a 366-day span can otherwise land
    // one period past the seventh (`period_index_0based == 7`), which does
    // not exist; day 1 of the *next* cycle already reports period 1
    // correctly via `most_recent_birthday_anniversary` picking the new
    // anniversary, so clamping here only ever affects the final day or two
    // of a longer-than-average cycle, keeping it in period 7 rather than
    // an invalid eighth one.
    let period_index_0based = (days_since_birthday / PERSONAL_YEAR_PERIOD_LENGTH_DAYS)
        .floor()
        .clamp(0.0, 6.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0.0..=6.0 immediately above"
    )]
    let period_number = period_index_0based as u8 + 1;

    let period_start_days = period_index_0based * PERSONAL_YEAR_PERIOD_LENGTH_DAYS;
    let period_end_days = (period_index_0based + 1.0) * PERSONAL_YEAR_PERIOD_LENGTH_DAYS;
    let transition = (days_since_birthday - period_start_days).abs() <= transition_tolerance_days
        || (days_since_birthday - period_end_days).abs() <= transition_tolerance_days;

    PersonalYearPeriod {
        period_number,
        ruling_planet: ruling_planet_for_period(period_number),
        transition,
    }
}

/// The fixed Chaldean order: period `1` is always Sun-ruled and always
/// begins at the birthday, recurring identically every year of life,
/// never progressing by age.
pub(crate) fn ruling_planet_for_period(period_number: u8) -> RulingPlanet {
    match period_number {
        1 => RulingPlanet::Sun,
        2 => RulingPlanet::Moon,
        3 => RulingPlanet::Mars,
        4 => RulingPlanet::Mercury,
        5 => RulingPlanet::Jupiter,
        6 => RulingPlanet::Venus,
        _ => RulingPlanet::Saturn,
    }
}

/// The most recent anniversary of `birth_date` at or before `as_of_date`:
/// the start of `as_of_date`'s current birthday-to-birthday year.
pub(crate) fn most_recent_birthday_anniversary(birth_date: time::Date, as_of_date: time::Date) -> time::Date {
    let this_year = anniversary_in_year(birth_date, as_of_date.year());
    if this_year <= as_of_date {
        this_year
    } else {
        anniversary_in_year(birth_date, as_of_date.year() - 1)
    }
}

/// `birth_date`'s month and day, restated in `year`. A 29 February
/// birthday has no exact anniversary in a non-leap `year`; observed on
/// 28 February instead, a deliberate, documented choice (common calendar
/// convention), not a silently invented one. Every other month has a
/// fixed day count in every year, so this is the only case that needs
/// handling; the closing `unwrap_or_else` is therefore defensive only,
/// covering no reachable input, since 28 February is valid in every year.
pub(crate) fn anniversary_in_year(birth_date: time::Date, year: i32) -> time::Date {
    let is_unobservable_leap_day =
        birth_date.month() == time::Month::February && birth_date.day() == 29 && !time::util::is_leap_year(year);
    let day = if is_unobservable_leap_day { 28 } else { birth_date.day() };
    time::Date::from_calendar_date(year, birth_date.month(), day).unwrap_or(birth_date)
}
