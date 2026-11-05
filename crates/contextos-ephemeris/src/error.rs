/// A calendar year whose corresponding ephemeris calculation would fall
/// outside what the underlying algorithm can represent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EphemerisError {
    #[error("year {year} is outside the range ephemeris calculations support")]
    YearOutOfRange { year: i32 },
    #[error("start_date {start_date:?} must not be after end_date {end_date:?}")]
    InvalidDateRange {
        start_date: time::Date,
        end_date: time::Date,
    },
}

impl EphemerisError {
    /// Stable, machine-readable error code, mirroring
    /// `contextos_core::PathError::code`'s own convention.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::YearOutOfRange { .. } => "ephemeris/year-out-of-range",
            Self::InvalidDateRange { .. } => "ephemeris/invalid-date-range",
        }
    }

    /// Actionable remediation text, mirroring
    /// `contextos_core::PathError::remediation`'s own convention.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::YearOutOfRange { .. } => {
                "Use a year within -9999 to 9999, this workspace's supported calendar range."
            }
            Self::InvalidDateRange { .. } => "Ensure start_date is not after end_date.",
        }
    }
}
