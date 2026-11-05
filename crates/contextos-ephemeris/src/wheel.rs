//! Offline Wheel-of-the-Year: the eight traditional festival points for a
//! calendar year (four solar boundaries, four cross-quarter checkpoints),
//! hemisphere-correctly named.

use crate::EphemerisError;
use crate::solar::solar_events;

/// Which hemisphere the operator observes seasonal correspondence from:
/// explicit, never auto-detected, since defaulting would silently produce
/// the wrong seasonal correspondence for an operator who genuinely splits
/// time between hemispheres across a year.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Hemisphere {
    Northern,
    Southern,
}

/// Whether a Wheel-of-the-Year point is one of the four solar events (a
/// season's actual start) or one of the four cross-quarter midpoints
/// between them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WheelOfYearRole {
    Boundary,
    Checkpoint,
}

/// One of the eight traditional Wheel-of-the-Year names, hemisphere-
/// agnostic in itself: which of the year's eight fixed positions (four
/// solar events, four cross-quarter dates) carries which name is what
/// [`Hemisphere`] actually determines; the name set itself does not
/// change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WheelOfYearName {
    Imbolc,
    SpringEquinox,
    Beltane,
    SummerSolstice,
    Lughnasadh,
    AutumnEquinox,
    Samhain,
    WinterSolstice,
}

/// The result for one Wheel-of-the-Year point: its hemisphere-correct
/// name, whether it is a boundary or a checkpoint, and when it falls. For a
/// boundary, the exact solar-event instant; for a checkpoint, midnight UTC
/// of its fixed traditional calendar date, which carries no meaningful
/// time of day of its own, unlike a boundary's astronomically precise
/// instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WheelOfYearPoint {
    pub name: WheelOfYearName,
    pub role: WheelOfYearRole,
    pub instant: time::OffsetDateTime,
}

/// Computes all eight Wheel-of-the-Year points for a calendar year, in
/// chronological order, hemisphere-correctly named.
pub trait ComputesWheelOfYear {
    /// # Errors
    ///
    /// Returns [`EphemerisError::YearOutOfRange`] if `year` cannot be
    /// represented by the underlying algorithm.
    fn wheel_of_year(&self, year: i32, hemisphere: Hemisphere) -> Result<[WheelOfYearPoint; 8], EphemerisError>;
}

/// The eight Wheel-of-the-Year positions, in chronological order, each
/// mapped to the [`WheelOfYearName`] it carries in the Northern Hemisphere:
/// position `0` is Imbolc, `1` is the March boundary (Spring Equinox), and
/// so on. The traditional "1/2 February" and "31 October/1 November"
/// notation for these dates leaves Imbolc's and Samhain's exact day
/// genuinely ambiguous
/// between two traditional choices; this picks the earlier day for both
/// (1 February, 31 October), consistent with the "eve of" convention
/// under which many traditions begin the observance the night before,
/// documented here as a deliberate choice, not an unstated default.
pub(crate) fn wheel_of_year(year: i32, hemisphere: Hemisphere) -> Result<[WheelOfYearPoint; 8], EphemerisError> {
    let events = solar_events(year)?;
    let imbolc = checkpoint_instant(year, time::Month::February, 1)?;
    let beltane = checkpoint_instant(year, time::Month::May, 1)?;
    let lughnasadh = checkpoint_instant(year, time::Month::August, 1)?;
    let samhain = checkpoint_instant(year, time::Month::October, 31)?;

    Ok([
        WheelOfYearPoint {
            name: wheel_name_for_position(0, hemisphere),
            role: WheelOfYearRole::Checkpoint,
            instant: imbolc,
        },
        WheelOfYearPoint {
            name: wheel_name_for_position(1, hemisphere),
            role: WheelOfYearRole::Boundary,
            instant: events[0].instant,
        },
        WheelOfYearPoint {
            name: wheel_name_for_position(2, hemisphere),
            role: WheelOfYearRole::Checkpoint,
            instant: beltane,
        },
        WheelOfYearPoint {
            name: wheel_name_for_position(3, hemisphere),
            role: WheelOfYearRole::Boundary,
            instant: events[1].instant,
        },
        WheelOfYearPoint {
            name: wheel_name_for_position(4, hemisphere),
            role: WheelOfYearRole::Checkpoint,
            instant: lughnasadh,
        },
        WheelOfYearPoint {
            name: wheel_name_for_position(5, hemisphere),
            role: WheelOfYearRole::Boundary,
            instant: events[2].instant,
        },
        WheelOfYearPoint {
            name: wheel_name_for_position(6, hemisphere),
            role: WheelOfYearRole::Checkpoint,
            instant: samhain,
        },
        WheelOfYearPoint {
            name: wheel_name_for_position(7, hemisphere),
            role: WheelOfYearRole::Boundary,
            instant: events[3].instant,
        },
    ])
}

/// Maps a chronological Wheel-of-the-Year position (`0` to `7`, Northern
/// Hemisphere ordering: Imbolc, Spring Equinox, Beltane, Summer Solstice,
/// Lughnasadh, Autumn Equinox, Samhain, Winter Solstice) to the name it
/// actually carries for `hemisphere`. The calendar position never moves
/// (checkpoints fall on fixed dates, boundaries on astronomically fixed
/// instants); only which *name* attaches to a given position changes, by
/// exactly half the wheel (`+4`, wrapping), matching how a June solstice
/// is Southern Hemisphere midwinter without the solstice itself happening
/// on a different date.
fn wheel_name_for_position(position: u8, hemisphere: Hemisphere) -> WheelOfYearName {
    let effective_position = match hemisphere {
        Hemisphere::Northern => position,
        Hemisphere::Southern => (position + 4) % 8,
    };
    match effective_position {
        0 => WheelOfYearName::Imbolc,
        1 => WheelOfYearName::SpringEquinox,
        2 => WheelOfYearName::Beltane,
        3 => WheelOfYearName::SummerSolstice,
        4 => WheelOfYearName::Lughnasadh,
        5 => WheelOfYearName::AutumnEquinox,
        6 => WheelOfYearName::Samhain,
        _ => WheelOfYearName::WinterSolstice,
    }
}

/// Midnight UTC of a fixed calendar date: a checkpoint's nominal instant,
/// which carries no astronomically precise time of day of its own, unlike
/// a boundary's solar-event-derived instant.
fn checkpoint_instant(year: i32, month: time::Month, day: u8) -> Result<time::OffsetDateTime, EphemerisError> {
    let date = time::Date::from_calendar_date(year, month, day).map_err(|_| EphemerisError::YearOutOfRange { year })?;
    Ok(time::PrimitiveDateTime::new(date, time::Time::MIDNIGHT).assume_utc())
}
