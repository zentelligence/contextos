//! Offline moon-phase calculation: the eight named phases, illumination
//! fraction, and days into the current synodic cycle, plus the
//! Meeus-algorithm phase-instant helpers the Wheel-of-the-Year and
//! boundary aggregators reuse.

use crate::to_astro_date;

/// One of the Moon's eight named phases, determined by its
/// position within the current synodic (New-Moon-to-New-Moon) cycle: eight
/// equal 45-degree arcs, each centred on its name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MoonPhaseName {
    New,
    WaxingCrescent,
    FirstQuarter,
    WaxingGibbous,
    Full,
    WaningGibbous,
    LastQuarter,
    WaningCrescent,
}

/// The full moon-phase result for one calendar date.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the four near_* flags are MoonPhaseReport's own named \
              fields, one per primary phase; not mutually exclusive (a \
              large enough tolerance could in principle flag more than \
              one), so an enum would misrepresent them"
)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoonPhaseReport {
    pub name: MoonPhaseName,
    /// Fraction of the lunar disk illuminated, `0.0` (new) to `1.0` (full).
    pub illumination_fraction: f64,
    /// Days elapsed since the most recent New Moon at or before the
    /// queried date, `0.0` (inclusive) to just under the synodic month's
    /// length (~29.53 days).
    pub days_into_cycle: f64,
    /// Whether the queried date falls within the caller's tolerance window
    /// of the New Moon nearest to it.
    pub near_new: bool,
    /// As `near_new`, for the nearest First Quarter.
    pub near_first_quarter: bool,
    /// As `near_new`, for the nearest Full Moon.
    pub near_full: bool,
    /// As `near_new`, for the nearest Last Quarter.
    pub near_last_quarter: bool,
}

/// Computes [`MoonPhaseReport`] for a calendar date.
pub trait ComputesMoonPhase {
    /// `tolerance_hours` governs the four `near_*` window flags on the
    /// result (`±`, default `12.0`).
    fn moon_phase(&self, date: time::Date, tolerance_hours: f64) -> MoonPhaseReport;
}

/// One astronomical unit, in kilometres (IAU 2012 exact definition): needed
/// to bring `astro::sun::geocent_ecl_pos`'s AU-denominated Earth-Sun
/// distance into the same unit as `astro::lunar::geocent_ecl_pos`'s
/// kilometre-denominated Earth-Moon distance before
/// [`astro::lunar::illum_frac_frm_ecl_coords`] can compare them; the two
/// functions do not share a unit by default, and passing mismatched units
/// silently produces a nonsense illumination fraction rather than an error.
const AU_IN_KM: f64 = 149_597_870.7;

/// Mean synodic (New-Moon-to-New-Moon) month length in days, the same
/// constant `astro::lunar::time_of_phase`'s own algorithm is built on.
pub(crate) const SYNODIC_MONTH_DAYS: f64 = 29.530_588_861;

/// The fraction of a synodic month `astro::lunar::time_of_phase`'s own `k`
/// numbering advances between one named phase and the next (New=0,
/// First=0.25, Full=0.5, Last=0.75).
pub(crate) fn phase_offset(phase: &astro::lunar::Phase) -> f64 {
    match phase {
        astro::lunar::Phase::New => 0.0,
        astro::lunar::Phase::First => 0.25,
        astro::lunar::Phase::Full => 0.5,
        astro::lunar::Phase::Last => 0.75,
    }
}

/// A synthetic `astro::time::Date` engineered so `astro::time::decimal_year`
/// reproduces `target_decimal_year` exactly: year and month are not
/// meaningful calendar components here (month is fixed to January, which
/// astro's own `decimal_year` treats as a plain 365-day year with no
/// leap-day branch, making the reconstruction exact rather than
/// approximate), only the resulting `decimal_year` value matters, since
/// that is the only thing [`astro::lunar::time_of_phase`] reads from an
/// anchor before discarding it in favour of its own derived `k`.
fn decimal_year_anchor(target_decimal_year: f64) -> astro::time::Date {
    let year = target_decimal_year.floor();
    let decimal_day = (target_decimal_year - year) * 365.0;
    // `target_decimal_year` is always within roughly one year of a real
    // `time::Date`'s own year (see `to_astro_date`'s equivalent
    // comment), so this is exact given the caller's own year range, never
    // a lossy truncation in practice.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "bounded to a real time::Date's year range, always within i16"
    )]
    let year = year as i16;
    astro::time::Date {
        year,
        month: 1,
        decimal_day,
        cal_type: astro::time::CalType::Gregorian,
    }
}

/// The exact Meeus-algorithm instant of `phase` closest in `k`-numbering to
/// `integer_k`, bypassing [`astro::lunar::time_of_phase`]'s own anchor-date
/// selection entirely: anchors at that `k` bucket's midpoint, immune to the
/// truncation-boundary jitter [`nearest_phase_jd`]'s own doc comment
/// describes.
pub(crate) fn phase_jd_at_integer_k(integer_k: f64, phase: &astro::lunar::Phase) -> f64 {
    let bucket_midpoint = if integer_k >= 0.0 {
        integer_k + 0.5
    } else {
        integer_k - 0.5
    };
    let target_decimal_year = bucket_midpoint / 12.3685 + 2000.0;
    astro::lunar::time_of_phase(&decimal_year_anchor(target_decimal_year), phase)
}

/// The Julian Day of the occurrence of `phase` genuinely closest to
/// `date_jd`.
///
/// `astro::lunar::time_of_phase`'s own doc comment claims to return
/// whichever occurrence is "closest to date", but its actual
/// implementation truncates (rather than rounds) a linear estimate of the
/// lunation number derived from the *anchor's* decimal year, which can
/// jump a full synodic month ahead of the true nearest occurrence for an
/// anchor sitting close to (or slightly past) its target: confirmed
/// empirically against `astro-2.0.0/tests/lunar.rs`'s own cited 1977
/// fixture, where anchoring at the exact New Moon date it names still
/// returned the *next* month's New Moon instead. Rather than anchor at
/// `date` itself and trust that selection, this computes the caller's own
/// continuous lunation-number estimate, evaluates both integer `k`
/// candidates it could reasonably round to (via
/// [`phase_jd_at_integer_k`], which sidesteps the same truncation bug by
/// engineering its own anchor), and returns whichever real result is
/// actually closer to `date_jd`. Checking both neighbours rather than
/// trusting a single rounded estimate matters most exactly at a boundary,
/// where the periodic correction terms inside the full Meeus polynomial
/// (not just the linear `k` estimate) can make either side the true
/// answer.
fn nearest_phase_jd(query_decimal_year: f64, date_jd: f64, phase: &astro::lunar::Phase) -> f64 {
    let raw_k = 12.3685 * (query_decimal_year - 2000.0) - phase_offset(phase);
    let lower_k = raw_k.floor();
    let candidate_lower = phase_jd_at_integer_k(lower_k, phase);
    let candidate_upper = phase_jd_at_integer_k(lower_k + 1.0, phase);
    if (candidate_lower - date_jd).abs() <= (candidate_upper - date_jd).abs() {
        candidate_lower
    } else {
        candidate_upper
    }
}

pub(crate) fn moon_phase(date: time::Date, tolerance_hours: f64) -> MoonPhaseReport {
    let astro_date = to_astro_date(date);
    let date_jd = astro::time::julian_day(&astro_date);
    let query_decimal_year = astro::time::decimal_year(&astro_date);

    // The most recent New Moon at or before `date`: `nearest_phase_jd` can
    // legitimately return a New Moon *after* `date` (whenever `date` sits
    // in the second half of its cycle, closer to the upcoming New Moon
    // than the previous one); rolling back one synodic month in that case
    // anchors `days_into_cycle` on the preceding one instead, never a
    // future one. The mean synodic month is an approximation (the true
    // month varies by a few tenths of a day around it), acceptable here
    // since `days_into_cycle` only needs to be accurate to well under the
    // 45-degree (~3.7-day) resolution `phase_name_for_angle` buckets at.
    let mut new_moon_jd = nearest_phase_jd(query_decimal_year, date_jd, &astro::lunar::Phase::New);
    if new_moon_jd > date_jd {
        new_moon_jd -= SYNODIC_MONTH_DAYS;
    }
    let days_into_cycle = date_jd - new_moon_jd;
    let phase_angle_degrees = (days_into_cycle / SYNODIC_MONTH_DAYS) * 360.0;
    let name = phase_name_for_angle(phase_angle_degrees);

    let (moon_ecl, moon_dist_km) = astro::lunar::geocent_ecl_pos(date_jd);
    let (sun_ecl, sun_dist_au) = astro::sun::geocent_ecl_pos(date_jd);
    let illumination_fraction = astro::lunar::illum_frac_frm_ecl_coords(
        moon_ecl.long,
        moon_ecl.lat,
        sun_ecl.long,
        moon_dist_km,
        sun_dist_au * AU_IN_KM,
    );

    let is_near = |phase: &astro::lunar::Phase| {
        let target_jd = nearest_phase_jd(query_decimal_year, date_jd, phase);
        (target_jd - date_jd).abs() * 24.0 <= tolerance_hours
    };

    MoonPhaseReport {
        name,
        illumination_fraction,
        days_into_cycle,
        near_new: is_near(&astro::lunar::Phase::New),
        near_first_quarter: is_near(&astro::lunar::Phase::First),
        near_full: is_near(&astro::lunar::Phase::Full),
        near_last_quarter: is_near(&astro::lunar::Phase::Last),
    }
}

/// Buckets a continuous phase angle (`0.0` to `360.0` degrees, `0` = New)
/// into one of the eight named phases, each spanning 45 degrees centred on
/// its name (e.g. Full spans `157.5` to `202.5`).
fn phase_name_for_angle(angle_degrees: f64) -> MoonPhaseName {
    let normalized = angle_degrees.rem_euclid(360.0);
    if !(22.5..337.5).contains(&normalized) {
        MoonPhaseName::New
    } else if normalized < 67.5 {
        MoonPhaseName::WaxingCrescent
    } else if normalized < 112.5 {
        MoonPhaseName::FirstQuarter
    } else if normalized < 157.5 {
        MoonPhaseName::WaxingGibbous
    } else if normalized < 202.5 {
        MoonPhaseName::Full
    } else if normalized < 247.5 {
        MoonPhaseName::WaningGibbous
    } else if normalized < 292.5 {
        MoonPhaseName::LastQuarter
    } else {
        MoonPhaseName::WaningCrescent
    }
}
