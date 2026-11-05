#![forbid(unsafe_code)]

//! Offline moon phase, solar Wheel-of-the-Year, and personal-year period
//! calculations, backed by the `astro` crate's Meeus-algorithm
//! implementation. Pure computation: no I/O, no vault or filesystem
//! dependency, mirroring `contextos-mermaid`'s trait-behind-a-crate-boundary
//! shape so swapping to a different underlying algorithm implementation
//! stays a trait-impl change, not a rewrite.
//!
//! Each calculation lives in its own module ([`moon`], [`solar`], [`wheel`],
//! [`personal_year`], [`boundaries`]); [`AstroEphemeris`] is the single
//! stateless type that implements every trait, delegating to the
//! module-private computation each one owns.

mod boundaries;
mod error;
mod moon;
mod personal_year;
mod solar;
mod wheel;

pub use boundaries::{BoundariesConfig, ComputesBoundaries, HorizonEvent, PrimaryMoonPhase};
pub use error::EphemerisError;
pub use moon::{ComputesMoonPhase, MoonPhaseName, MoonPhaseReport};
pub use personal_year::{ComputesPersonalYearPeriod, PersonalYearPeriod, RulingPlanet};
pub use solar::{ComputesSolarEvents, SolarEvent, SolarEventKind};
pub use wheel::{ComputesWheelOfYear, Hemisphere, WheelOfYearName, WheelOfYearPoint, WheelOfYearRole};

/// [`ComputesMoonPhase`], [`ComputesSolarEvents`], [`ComputesWheelOfYear`],
/// [`ComputesPersonalYearPeriod`], and [`ComputesBoundaries`] backed by the
/// `astro` crate's Meeus-algorithm implementation. Stateless:
/// holds no configuration or cache.
#[derive(Clone, Copy, Debug, Default)]
pub struct AstroEphemeris;

impl AstroEphemeris {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ComputesMoonPhase for AstroEphemeris {
    fn moon_phase(&self, date: time::Date, tolerance_hours: f64) -> MoonPhaseReport {
        moon::moon_phase(date, tolerance_hours)
    }
}

impl ComputesSolarEvents for AstroEphemeris {
    fn solar_events(&self, year: i32) -> Result<[SolarEvent; 4], EphemerisError> {
        solar::solar_events(year)
    }
}

impl ComputesWheelOfYear for AstroEphemeris {
    fn wheel_of_year(&self, year: i32, hemisphere: Hemisphere) -> Result<[WheelOfYearPoint; 8], EphemerisError> {
        wheel::wheel_of_year(year, hemisphere)
    }
}

impl ComputesPersonalYearPeriod for AstroEphemeris {
    fn personal_year_period(
        &self,
        birth_date: time::Date,
        as_of_date: time::Date,
        transition_tolerance_days: f64,
    ) -> PersonalYearPeriod {
        personal_year::personal_year_period(birth_date, as_of_date, transition_tolerance_days)
    }
}

impl ComputesBoundaries for AstroEphemeris {
    fn boundaries(
        &self,
        start_date: time::Date,
        end_date: time::Date,
        birth_date: Option<time::Date>,
        hemisphere: Option<Hemisphere>,
        config: BoundariesConfig,
    ) -> Result<Vec<HorizonEvent>, EphemerisError> {
        boundaries::boundaries(start_date, end_date, birth_date, hemisphere, config)
    }
}

/// Converts a validated `time::Date` to the `astro` crate's own date type,
/// the anchor every Julian-Day and decimal-year conversion in this crate
/// starts from.
pub(crate) fn to_astro_date(date: time::Date) -> astro::time::Date {
    // `time::Date`'s year is guaranteed within `i16` range (15-bit,
    // -16384..16383) under this workspace's default (non-`large-dates`)
    // feature set (see `time-0.3.53/src/date.rs`'s own `MIN_YEAR`/
    // `MAX_YEAR`), so this cast never truncates; there is no reachable
    // input that would make it lossy, so no fallible conversion or error
    // type exists for a case the type system already rules out.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "time::Date's year is always within i16 range without the large-dates feature"
    )]
    let year = date.year() as i16;
    astro::time::Date {
        year,
        month: u8::from(date.month()),
        decimal_day: f64::from(date.day()),
        cal_type: astro::time::CalType::Gregorian,
    }
}

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
