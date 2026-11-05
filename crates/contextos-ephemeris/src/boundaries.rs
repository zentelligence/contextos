//! Offline horizon-boundary aggregation: every moon-quarter instant,
//! Wheel-of-the-Year point, and personal-year period change crossed within
//! an inclusive date range, computed once across the window rather than
//! once per day.

use crate::moon::{phase_jd_at_integer_k, phase_offset};
use crate::personal_year::{
    PERSONAL_YEAR_PERIOD_LENGTH_DAYS, anniversary_in_year, most_recent_birthday_anniversary, ruling_planet_for_period,
};
use crate::solar::jd_dynamical_time_to_utc;
use crate::wheel::{Hemisphere, WheelOfYearName, WheelOfYearPoint, WheelOfYearRole, wheel_of_year};
use crate::{EphemerisError, RulingPlanet, to_astro_date};

/// One of the four primary Moon phases (the eight-name `MoonPhaseName`
/// set narrowed to only the ones reported as discrete, instant-in-time
/// checkpoints): the waxing/waning gibbous/crescent names describe a range
/// of days, not a single crossable moment, so they have no place in an
/// aggregate of instants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrimaryMoonPhase {
    New,
    FirstQuarter,
    Full,
    LastQuarter,
}

/// One horizon boundary or checkpoint, aggregated across moon phases,
/// Wheel-of-the-Year points, and personal-year periods: a moon-quarter
/// instant, a Wheel-of-the-Year point, or a personal-year period change,
/// each carrying its own exact UTC instant and its own domain-specific
/// detail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HorizonEvent {
    MoonQuarter {
        instant: time::OffsetDateTime,
        phase: PrimaryMoonPhase,
    },
    WheelOfYear {
        instant: time::OffsetDateTime,
        name: WheelOfYearName,
        role: WheelOfYearRole,
    },
    PersonalYearPeriodChange {
        instant: time::OffsetDateTime,
        period_number: u8,
        ruling_planet: RulingPlanet,
    },
}

impl HorizonEvent {
    #[must_use]
    pub fn instant(&self) -> time::OffsetDateTime {
        match self {
            Self::MoonQuarter { instant, .. }
            | Self::WheelOfYear { instant, .. }
            | Self::PersonalYearPeriodChange { instant, .. } => *instant,
        }
    }
}

/// The two tolerance windows that govern boundary detection (`config`'s
/// entire scope, deliberately: `hemisphere`/`birth_date` stay named
/// parameters rather than folding into this, since silently defaulting
/// either would produce a wrong result, not a merely suboptimal one, the
/// same reasoning that keeps `hemisphere` explicit rather than
/// auto-detected).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoundariesConfig {
    /// `±`, default `12.0`.
    pub moon_phase_tolerance_hours: f64,
    /// `±`, default `2.0`.
    pub personal_year_transition_tolerance_days: f64,
}

impl Default for BoundariesConfig {
    fn default() -> Self {
        Self {
            moon_phase_tolerance_hours: 12.0,
            personal_year_transition_tolerance_days: 2.0,
        }
    }
}

/// Aggregates moon phases, Wheel-of-the-Year points, and personal-year
/// periods into every horizon boundary or checkpoint crossed within
/// `[start_date, end_date]` inclusive, computed once across the window
/// rather than once per day. A single day
/// is simply the case `start_date == end_date`; no separate single-date
/// method exists. Moon-quarter checkpoints are always included; Wheel-of-
/// Year points are included only when `hemisphere` is supplied;
/// personal-year period changes are included only when `birth_date` is
/// supplied, so a caller with no operator-specific data still gets a
/// valid, narrower result rather than an error.
pub trait ComputesBoundaries {
    /// # Errors
    ///
    /// Returns [`EphemerisError::InvalidDateRange`] if `start_date` is
    /// after `end_date`, or [`EphemerisError::YearOutOfRange`] if any year
    /// the range touches cannot be represented by the underlying
    /// algorithm.
    fn boundaries(
        &self,
        start_date: time::Date,
        end_date: time::Date,
        birth_date: Option<time::Date>,
        hemisphere: Option<Hemisphere>,
        config: BoundariesConfig,
    ) -> Result<Vec<HorizonEvent>, EphemerisError>;
}

pub(crate) fn boundaries(
    start_date: time::Date,
    end_date: time::Date,
    birth_date: Option<time::Date>,
    hemisphere: Option<Hemisphere>,
    config: BoundariesConfig,
) -> Result<Vec<HorizonEvent>, EphemerisError> {
    if start_date > end_date {
        return Err(EphemerisError::InvalidDateRange { start_date, end_date });
    }

    let range_start_jd = astro::time::julian_day(&to_astro_date(start_date));
    // Exclusive: the instant one tick past the end of `end_date` itself,
    // so the inclusive calendar range `[start_date, end_date]` becomes a
    // half-open Julian Day interval, simplifying every comparison below to
    // a plain `>=`/`<`.
    let range_end_jd = astro::time::julian_day(&to_astro_date(end_date)) + 1.0;

    let mut events = Vec::new();

    for (phase, jd_dynamical_time) in moon_quarter_occurrences_in_range(
        start_date,
        range_start_jd,
        range_end_jd,
        config.moon_phase_tolerance_hours,
    ) {
        events.push(HorizonEvent::MoonQuarter {
            instant: jd_dynamical_time_to_utc(jd_dynamical_time)?,
            phase,
        });
    }

    if let Some(hemisphere) = hemisphere {
        for point in wheel_of_year_events_in_range(start_date, end_date, hemisphere)? {
            events.push(HorizonEvent::WheelOfYear {
                instant: point.instant,
                name: point.name,
                role: point.role,
            });
        }
    }

    if let Some(birth_date) = birth_date {
        for (period_number, instant) in personal_year_period_changes_in_range(
            birth_date,
            start_date,
            end_date,
            config.personal_year_transition_tolerance_days,
        ) {
            events.push(HorizonEvent::PersonalYearPeriodChange {
                instant,
                period_number,
                ruling_planet: ruling_planet_for_period(period_number),
            });
        }
    }

    events.sort_by_key(HorizonEvent::instant);
    Ok(events)
}

fn primary_moon_phase(phase: &astro::lunar::Phase) -> PrimaryMoonPhase {
    match phase {
        astro::lunar::Phase::New => PrimaryMoonPhase::New,
        astro::lunar::Phase::First => PrimaryMoonPhase::FirstQuarter,
        astro::lunar::Phase::Full => PrimaryMoonPhase::Full,
        astro::lunar::Phase::Last => PrimaryMoonPhase::LastQuarter,
    }
}

/// Every occurrence (Dynamical Time Julian Day) of each of the four
/// primary Moon phases whose instant falls within `[range_start_jd -
/// tolerance, range_end_jd + tolerance)`, `tolerance` converted from
/// `tolerance_hours`. Reproduces exactly what a per-day loop calling
/// `moon_phase` with the same tolerance and checking its four `near_*`
/// flags would collect (the union, across every day in range, of "is this
/// phase's instant within tolerance of this day's midnight"), without
/// looping per day: for `tolerance_hours` at its default `12.0`,
/// consecutive per-day windows of `±12h` tile the range with no gaps, so
/// this range-based check is exact, not merely an approximation of the
/// per-day definition.
///
/// Walks `k` (the same integer lunation index [`phase_jd_at_integer_k`]
/// already uses) forward from a starting point anchored on `start_date`'s
/// own decimal year, `2` steps early as a safety margin far larger than
/// any realistic tolerance value, so the first in-range occurrence is
/// never missed; stops as soon as a candidate's Julian Day exceeds the
/// upper bound, since `k` maps monotonically to Julian Day.
fn moon_quarter_occurrences_in_range(
    start_date: time::Date,
    range_start_jd: f64,
    range_end_jd: f64,
    tolerance_hours: f64,
) -> Vec<(PrimaryMoonPhase, f64)> {
    let tolerance_days = tolerance_hours / 24.0;
    let lower_bound = range_start_jd - tolerance_days;
    let upper_bound = range_end_jd + tolerance_days;
    let anchor_decimal_year = astro::time::decimal_year(&to_astro_date(start_date));

    let mut occurrences = Vec::new();
    for phase in [
        astro::lunar::Phase::New,
        astro::lunar::Phase::First,
        astro::lunar::Phase::Full,
        astro::lunar::Phase::Last,
    ] {
        let raw_k = 12.3685 * (anchor_decimal_year - 2000.0) - phase_offset(&phase);
        let mut k = raw_k.floor() - 2.0;
        loop {
            let jd = phase_jd_at_integer_k(k, &phase);
            if jd > upper_bound {
                break;
            }
            if jd >= lower_bound {
                occurrences.push((primary_moon_phase(&phase), jd));
            }
            k += 1.0;
        }
    }
    occurrences
}

/// Every Wheel-of-the-Year point whose instant falls within
/// `[start_date, end_date]` inclusive, across every calendar year the
/// range touches. No tolerance: unlike a moon-quarter checkpoint or a
/// personal-year transition, a Wheel-of-the-Year point has no "near"
/// window of its own for `config` to carry.
fn wheel_of_year_events_in_range(
    start_date: time::Date,
    end_date: time::Date,
    hemisphere: Hemisphere,
) -> Result<Vec<WheelOfYearPoint>, EphemerisError> {
    let range_start = time::PrimitiveDateTime::new(start_date, time::Time::MIDNIGHT).assume_utc();
    let range_end = time::PrimitiveDateTime::new(end_date, time::Time::MIDNIGHT).assume_utc() + ONE_DAY;

    let mut points = Vec::new();
    for year in start_date.year()..=end_date.year() {
        for point in wheel_of_year(year, hemisphere)? {
            if point.instant >= range_start && point.instant < range_end {
                points.push(point);
            }
        }
    }
    Ok(points)
}

/// One day, as a [`time::Duration`]: reads more plainly at each call site
/// below than a bare `time::Duration::days(1)` repeated three times.
const ONE_DAY: time::Duration = time::Duration::days(1);

/// Every personal-year period change within tolerance of
/// `[start_date, end_date]` inclusive: the instant each of the seven
/// periods begins, across every birthday-year the (tolerance-widened)
/// range touches. Mirrors [`moon_quarter_occurrences_in_range`]'s "what a
/// per-day loop checking `transition` would have found" equivalence, for
/// the same reason: exhaustive per-day checking and this range-based
/// check agree exactly.
fn personal_year_period_changes_in_range(
    birth_date: time::Date,
    start_date: time::Date,
    end_date: time::Date,
    tolerance_days: f64,
) -> Vec<(u8, time::OffsetDateTime)> {
    let tolerance = time::Duration::seconds_f64(tolerance_days * 86_400.0);
    let range_start = time::PrimitiveDateTime::new(start_date, time::Time::MIDNIGHT).assume_utc() - tolerance;
    let range_end = time::PrimitiveDateTime::new(end_date, time::Time::MIDNIGHT).assume_utc() + ONE_DAY + tolerance;

    let mut changes = Vec::new();
    let mut anniversary = most_recent_birthday_anniversary(birth_date, start_date);
    loop {
        let anniversary_instant = time::PrimitiveDateTime::new(anniversary, time::Time::MIDNIGHT).assume_utc();
        if anniversary_instant > range_end {
            break;
        }
        for period_index in 0_u8..7 {
            let offset_days = f64::from(period_index) * PERSONAL_YEAR_PERIOD_LENGTH_DAYS;
            let boundary_instant = anniversary_instant + time::Duration::seconds_f64(offset_days * 86_400.0);
            if boundary_instant >= range_start && boundary_instant <= range_end {
                changes.push((period_index + 1, boundary_instant));
            }
        }
        anniversary = anniversary_in_year(birth_date, anniversary.year() + 1);
    }
    changes
}
