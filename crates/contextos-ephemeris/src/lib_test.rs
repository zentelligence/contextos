use super::*;

fn date(year: i32, month: time::Month, day: u8) -> Result<time::Date, time::error::ComponentRange> {
    time::Date::from_calendar_date(year, month, day)
}

/// Meeus, *Astronomical Algorithms*, Ch. 49 Example 49.a: the New Moon
/// nearest 1977 February 15 falls at JD 2443192.65118 (1977-02-18
/// ~03:38 UT), independently pinned by `astro-rust`'s own upstream
/// regression test (`astro-2.0.0/tests/lunar.rs::phases`). Querying the
/// same calendar day places the tool ~3.6 hours before that instant.
#[test]
fn a_verified_new_moon_instant_reports_new_phase_and_near_zero_illumination() -> Result<(), time::error::ComponentRange>
{
    let report = AstroEphemeris::new().moon_phase(date(1977, time::Month::February, 18)?, 12.0);

    assert_eq!(report.name, MoonPhaseName::New);
    assert!(report.near_new, "must fall within the default ±12h window");
    assert!(
        report.illumination_fraction < 0.05,
        "illumination so close to New should be near zero, was {}",
        report.illumination_fraction
    );
    assert!(
        !(0.5..29.0).contains(&report.days_into_cycle),
        "should sit at a cycle boundary (near 0 or near the full \
             synodic month), was {}",
        report.days_into_cycle
    );
    Ok(())
}

/// The Full Moon following the same Meeus-cited anchor above, computed
/// via the identical, identically-accurate `time_of_phase` code path
/// (same periodic-term algorithm, only the `k` offset differs): falls
/// at JD 2443208.21807 (1977-03-05 ~17:14 UT). Querying the following
/// calendar day places the tool ~6.8 hours after that instant.
#[test]
fn a_verified_full_moon_instant_reports_full_phase_and_near_total_illumination()
-> Result<(), time::error::ComponentRange> {
    let report = AstroEphemeris::new().moon_phase(date(1977, time::Month::March, 6)?, 12.0);

    assert_eq!(report.name, MoonPhaseName::Full);
    assert!(report.near_full, "must fall within the default ±12h window");
    assert!(
        report.illumination_fraction > 0.98,
        "illumination so close to Full should be near total, was {}",
        report.illumination_fraction
    );
    assert!(
        (13.5..16.0).contains(&report.days_into_cycle),
        "should sit close to half the synodic month (~14.77 days), was {}",
        report.days_into_cycle
    );
    Ok(())
}

#[test]
fn a_date_far_from_any_primary_phase_is_near_none_of_them() -> Result<(), time::error::ComponentRange> {
    // Halfway between the verified New Moon (1977-02-18) and First
    // Quarter (1977-02-26, computed via the same code path): sits
    // ~4 days from both, well outside even a generous 24-hour
    // tolerance.
    let report = AstroEphemeris::new().moon_phase(date(1977, time::Month::February, 22)?, 24.0);

    assert!(!report.near_new);
    assert!(!report.near_first_quarter);
    assert!(!report.near_full);
    assert!(!report.near_last_quarter);
    Ok(())
}

/// Regression pin for the `time_of_phase` anchor-selection quirk
/// `nearest_phase_jd`'s own doc comment describes. A naive,
/// date-anchored call into `astro::lunar::time_of_phase` at this exact
/// fixture date (confirmed empirically while building this crate)
/// skips straight past the true nearest New Moon (1977-02-18,
/// ~5.65 days ahead) to the *next* month's instead
/// (1977-03-19, ~34.77 days ahead), which would report
/// `days_into_cycle` around `24.38` days short, landing in the wrong
/// 45-degree phase bucket entirely (New instead of the correct Waning
/// Crescent). This pins the correct value directly.
#[test]
fn the_time_of_phase_anchor_quirk_does_not_skip_the_true_nearest_occurrence() -> Result<(), time::error::ComponentRange>
{
    let report = AstroEphemeris::new().moon_phase(date(1977, time::Month::February, 13)?, 12.0);
    assert_eq!(report.name, MoonPhaseName::WaningCrescent);
    assert!(
        (23.5..25.5).contains(&report.days_into_cycle),
        "should be ~24.38 days into the cycle that started 1977-01-19 \
             (the true nearest New Moon is 1977-02-18, still ~5.65 days \
             ahead, not yet reached); the quirk this pins against would \
             instead skip to the following month's New Moon and report a \
             days_into_cycle far short of this, was {}",
        report.days_into_cycle
    );
    Ok(())
}

/// The four solar events for a given year, in chronological
/// order, each within its correct calendar month (no external citation
/// needed for this one, only that a March equinox genuinely falls in
/// March and precedes June's solstice, and so on).
#[test]
fn solar_events_returns_four_events_in_chronological_order() {
    let Ok(events) = AstroEphemeris::new().solar_events(2024) else {
        unreachable!("2024 is a representable year");
    };

    assert_eq!(events[0].kind, SolarEventKind::MarchEquinox);
    assert_eq!(events[1].kind, SolarEventKind::JuneSolstice);
    assert_eq!(events[2].kind, SolarEventKind::SeptemberEquinox);
    assert_eq!(events[3].kind, SolarEventKind::DecemberSolstice);

    assert!(events[0].instant < events[1].instant);
    assert!(events[1].instant < events[2].instant);
    assert!(events[2].instant < events[3].instant);

    assert_eq!(events[0].instant.month(), time::Month::March);
    assert_eq!(events[1].instant.month(), time::Month::June);
    assert_eq!(events[2].instant.month(), time::Month::September);
    assert_eq!(events[3].instant.month(), time::Month::December);
}

/// Cross-referenced against commonly published 2024 equinox/solstice UTC times (general astronomical knowledge,
/// not a specific cited almanac source; a tighter cross-check against a named published source is still owed as a
/// follow-up accuracy item). Convergent evidence, not a bare assertion: omitting the nutation/aberration
/// correction in `sun_apparent_longitude_degrees` shifted every one of these four instants by roughly ten minutes
/// away from the values asserted here; adding it, the physically correct thing to do for an *apparent*-longitude
/// crossing, closed the gap to within about a minute, for all four independently. A five-minute tolerance leaves
/// headroom for residual `delta_t`-table or seed-precision variance without masking a genuine regression.
#[test]
fn solar_events_are_close_to_commonly_published_2024_instants() {
    let Ok(events) = AstroEphemeris::new().solar_events(2024) else {
        unreachable!("2024 is a representable year");
    };
    let expected = [
        (time::Month::March, 20, 3, 6),
        (time::Month::June, 20, 20, 51),
        (time::Month::September, 22, 12, 44),
        (time::Month::December, 21, 9, 20),
    ];

    for (event, (month, day, hour, minute)) in events.iter().zip(expected) {
        let Ok(expected_date) = time::Date::from_calendar_date(2024, month, day) else {
            unreachable!("every expected date above is a valid calendar date");
        };
        let Ok(expected_time) = time::Time::from_hms(hour, minute, 0) else {
            unreachable!("every expected time above is a valid time of day");
        };
        let expected_instant = time::PrimitiveDateTime::new(expected_date, expected_time).assume_utc();
        let difference = (event.instant - expected_instant).abs();
        assert!(
            difference < time::Duration::minutes(5),
            "{:?}: computed {:?}, expected close to {expected_instant:?}, \
                 differed by {difference}",
            event.kind,
            event.instant
        );
    }
}

/// A tropical year (equinox-to-equinox) is close to 365.2422 days, not
/// exactly 365 or 365.25 (well-established orbital mechanics, not
/// specific to any one year): consecutive March equinoxes a year
/// apart should land close to that spacing, catching a gross year-to-
/// year regression without needing an external citation for either
/// year individually.
#[test]
fn consecutive_march_equinoxes_are_close_to_one_tropical_year_apart() {
    let ephemeris = AstroEphemeris::new();
    let Ok(this_year) = ephemeris.solar_events(2024) else {
        unreachable!("2024 is a representable year");
    };
    let Ok(next_year) = ephemeris.solar_events(2025) else {
        unreachable!("2025 is a representable year");
    };

    let span = next_year[0].instant - this_year[0].instant;
    let span_days = span.as_seconds_f64().abs() / 86_400.0;
    assert!(
        (365.2..365.3).contains(&span_days),
        "consecutive March equinoxes should be ~365.2422 days apart, was {span_days}"
    );
}

/// Tightens the solar-events accuracy evidence by independently verifying
/// the *correction* term this crate adds beyond `astro`'s own already-
/// cited `geocent_ecl_pos` fixture: nutation in longitude, one of the
/// two corrections `sun_apparent_longitude_degrees` applies (the other
/// being aberration, too simple a one-line formula, a constant divided
/// by distance, to carry meaningful independent-verification risk).
/// Pinned against `astro-2.0.0/tests/nutation.rs`'s own Meeus-cited
/// fixture (JD 2446895.5: nutation in longitude `-3.788` arcseconds),
/// confirming the correction this crate depends on for its own
/// accuracy, not only the base longitude function, is independently
/// verified.
#[test]
fn the_nutation_correction_this_crate_depends_on_matches_astros_own_cited_fixture() {
    let (nutation_in_longitude, _nutation_in_obliquity) = astro::nutation::nutation(2_446_895.5);
    let arcseconds = nutation_in_longitude.to_degrees() * 3600.0;
    assert!(
        (-3.789..-3.787).contains(&arcseconds),
        "expected -3.788 arcseconds (astro-2.0.0/tests/nutation.rs, Meeus-cited), was {arcseconds}"
    );
}

#[test]
fn solar_events_rejects_a_year_outside_the_representable_range() {
    let year = 10_000;
    assert_eq!(
        AstroEphemeris::new().solar_events(year),
        Err(EphemerisError::YearOutOfRange { year })
    );
}

/// Eight points, alternating Checkpoint/Boundary starting
/// with a checkpoint (Imbolc, the year's first position), strictly
/// chronological, matching the fixed calendar dates interleaved
/// with the solar-event instants.
#[test]
fn wheel_of_year_returns_eight_points_in_chronological_order_with_correct_roles() {
    let Ok(points) = AstroEphemeris::new().wheel_of_year(2024, Hemisphere::Northern) else {
        unreachable!("2024 is a representable year");
    };

    let roles: Vec<WheelOfYearRole> = points.iter().map(|point| point.role).collect();
    assert_eq!(
        roles,
        vec![
            WheelOfYearRole::Checkpoint,
            WheelOfYearRole::Boundary,
            WheelOfYearRole::Checkpoint,
            WheelOfYearRole::Boundary,
            WheelOfYearRole::Checkpoint,
            WheelOfYearRole::Boundary,
            WheelOfYearRole::Checkpoint,
            WheelOfYearRole::Boundary,
        ]
    );
    for pair in points.windows(2) {
        assert!(
            pair[0].instant < pair[1].instant,
            "{:?} ({:?}) should precede {:?} ({:?})",
            pair[0].name,
            pair[0].instant,
            pair[1].name,
            pair[1].instant
        );
    }
}

/// The traditional Northern Hemisphere mapping (Sacred Seasons
/// convention): Imbolc (Feb), Spring Equinox (Mar), Beltane
/// (May), Summer Solstice (Jun), Lughnasadh (Aug), Autumn Equinox
/// (Sep), Samhain (Oct/Nov), Winter Solstice (Dec).
#[test]
fn northern_hemisphere_names_match_the_traditional_calendar_mapping() {
    let Ok(points) = AstroEphemeris::new().wheel_of_year(2024, Hemisphere::Northern) else {
        unreachable!("2024 is a representable year");
    };
    let names: Vec<WheelOfYearName> = points.iter().map(|point| point.name).collect();
    assert_eq!(
        names,
        vec![
            WheelOfYearName::Imbolc,
            WheelOfYearName::SpringEquinox,
            WheelOfYearName::Beltane,
            WheelOfYearName::SummerSolstice,
            WheelOfYearName::Lughnasadh,
            WheelOfYearName::AutumnEquinox,
            WheelOfYearName::Samhain,
            WheelOfYearName::WinterSolstice,
        ]
    );
}

/// The core requirement: "a June solstice is midsummer in the
/// north, midwinter in the south". The Southern Hemisphere names are
/// the same eight names rotated by exactly half the wheel (`+4`), and
/// critically, every point's *instant* is completely unchanged from
/// the Northern Hemisphere call for the same year: only the name
/// attached to each fixed position changes, never the position
/// itself, proving hemisphere is a labelling concern only, not a
/// second, different set of dates.
#[test]
fn southern_hemisphere_swaps_names_by_half_the_wheel_but_keeps_the_same_instants() {
    let ephemeris = AstroEphemeris::new();
    let Ok(northern) = ephemeris.wheel_of_year(2024, Hemisphere::Northern) else {
        unreachable!("2024 is a representable year");
    };
    let Ok(southern) = ephemeris.wheel_of_year(2024, Hemisphere::Southern) else {
        unreachable!("2024 is a representable year");
    };

    let southern_names: Vec<WheelOfYearName> = southern.iter().map(|point| point.name).collect();
    assert_eq!(
        southern_names,
        vec![
            WheelOfYearName::Lughnasadh,
            WheelOfYearName::AutumnEquinox,
            WheelOfYearName::Samhain,
            WheelOfYearName::WinterSolstice,
            WheelOfYearName::Imbolc,
            WheelOfYearName::SpringEquinox,
            WheelOfYearName::Beltane,
            WheelOfYearName::SummerSolstice,
        ]
    );
    // The June solstice (index 3) is Southern Hemisphere
    // WinterSolstice by name, at the identical instant Northern
    // Hemisphere calls SummerSolstice.
    assert_eq!(southern[3].name, WheelOfYearName::WinterSolstice);
    assert_eq!(northern[3].name, WheelOfYearName::SummerSolstice);
    for (north_point, south_point) in northern.iter().zip(southern.iter()) {
        assert_eq!(
            north_point.instant, south_point.instant,
            "hemisphere must never change which instant a position falls on"
        );
    }
}

/// The four cross-quarter checkpoints fall on fixed calendar
/// dates regardless of hemisphere, resolving the
/// "1/2 February"/"31 October, 1 November" ambiguity to the earlier day
/// in both cases.
#[test]
fn checkpoint_dates_fall_on_the_fixed_traditional_calendar_dates() {
    let Ok(points) = AstroEphemeris::new().wheel_of_year(2024, Hemisphere::Northern) else {
        unreachable!("2024 is a representable year");
    };
    let expected = [
        (0, time::Month::February, 1),
        (2, time::Month::May, 1),
        (4, time::Month::August, 1),
        (6, time::Month::October, 31),
    ];
    for (index, month, day) in expected {
        assert_eq!(points[index].instant.month(), month);
        assert_eq!(points[index].instant.day(), day);
        assert_eq!(points[index].role, WheelOfYearRole::Checkpoint);
    }
}

#[test]
fn wheel_of_year_rejects_a_year_outside_the_representable_range() {
    let year = 10_000;
    assert_eq!(
        AstroEphemeris::new().wheel_of_year(year, Hemisphere::Northern),
        Err(EphemerisError::YearOutOfRange { year })
    );
}

/// Period 1 is always Sun-ruled and always begins exactly at
/// the birthday itself.
#[test]
fn period_1_always_begins_exactly_at_the_birthday_and_is_sun_ruled() -> Result<(), time::error::ComponentRange> {
    let birth_date = date(1990, time::Month::June, 15)?;
    let result = AstroEphemeris::new().personal_year_period(birth_date, birth_date, 2.0);

    assert_eq!(result.period_number, 1);
    assert_eq!(result.ruling_planet, RulingPlanet::Sun);
    assert!(
        result.transition,
        "the birthday itself is a period boundary by definition"
    );
    Ok(())
}

/// The fixed Chaldean order, checked at the midpoint of every
/// one of the seven periods (safely clear of any boundary, so
/// `transition` cannot interfere with reading `period_number` off
/// cleanly).
#[test]
fn period_progresses_through_the_fixed_chaldean_order_across_the_year() -> Result<(), time::error::ComponentRange> {
    let birth_date = date(2000, time::Month::January, 1)?;
    let ephemeris = AstroEphemeris::new();
    let expected: [(u8, RulingPlanet); 7] = [
        (1, RulingPlanet::Sun),
        (2, RulingPlanet::Moon),
        (3, RulingPlanet::Mars),
        (4, RulingPlanet::Mercury),
        (5, RulingPlanet::Jupiter),
        (6, RulingPlanet::Venus),
        (7, RulingPlanet::Saturn),
    ];

    for (period_number, planet) in expected {
        let midpoint_days = (f64::from(period_number) - 0.5) * (365.25 / 7.0);
        let offset = time::Duration::seconds_f64(midpoint_days * 86_400.0);
        let as_of_datetime = time::PrimitiveDateTime::new(birth_date, time::Time::MIDNIGHT) + offset;
        let result = ephemeris.personal_year_period(birth_date, as_of_datetime.date(), 2.0);
        assert_eq!(
            result.period_number, period_number,
            "at the midpoint of period {period_number}"
        );
        assert_eq!(result.ruling_planet, planet);
        assert!(!result.transition, "a period's own midpoint is not near a boundary");
    }
    Ok(())
}

/// The cycle recurs identically every year of life, never progressing
/// by age: the same number of days past two different birthdays,
/// years apart, must report the same period.
#[test]
fn the_cycle_recurs_identically_every_year_of_life_not_progressing_by_age() -> Result<(), time::error::ComponentRange> {
    let birth_date = date(1985, time::Month::March, 10)?;
    let ephemeris = AstroEphemeris::new();
    let offset = time::Duration::days(100);

    let this_decade = birth_date + offset;
    let next_decade = date(1985 + 10, time::Month::March, 10)? + offset;

    let this_decade_result = ephemeris.personal_year_period(birth_date, this_decade, 2.0);
    let next_decade_result = ephemeris.personal_year_period(birth_date, next_decade, 2.0);
    assert_eq!(this_decade_result.period_number, next_decade_result.period_number);
    assert_eq!(this_decade_result.ruling_planet, next_decade_result.ruling_planet);
    Ok(())
}

#[test]
fn transition_flag_is_set_within_tolerance_of_a_boundary_and_clear_well_inside_a_period()
-> Result<(), time::error::ComponentRange> {
    let birth_date = date(2000, time::Month::January, 1)?;
    let ephemeris = AstroEphemeris::new();

    // The period 1/2 boundary falls at day 52.1786; day 52 sits well
    // within a default ±2-day tolerance of it.
    let near_boundary = birth_date + time::Duration::days(52);
    assert!(
        ephemeris
            .personal_year_period(birth_date, near_boundary, 2.0)
            .transition
    );

    // Day 10 sits comfortably inside period 1, far from either edge.
    let mid_period = birth_date + time::Duration::days(10);
    assert!(!ephemeris.personal_year_period(birth_date, mid_period, 2.0).transition);
    Ok(())
}

/// A 29 February birthday has no exact anniversary in
/// a non-leap year; observed on 28 February instead.
#[test]
fn a_29_february_birthday_is_observed_on_28_february_in_a_non_leap_year() -> Result<(), time::error::ComponentRange> {
    let birth_date = date(2000, time::Month::February, 29)?; // 2000 is a leap year
    let as_of_date = date(2023, time::Month::February, 28)?; // 2023 is not

    let result = AstroEphemeris::new().personal_year_period(birth_date, as_of_date, 2.0);
    assert_eq!(result.period_number, 1);
    assert_eq!(result.ruling_planet, RulingPlanet::Sun);
    assert!(result.transition);
    Ok(())
}

/// A single day is simply `start_date == end_date`, no
/// separate single-date method. Uses the same Meeus-cited 1977 New
/// Moon fixture the moon-phase tests verify elsewhere (JD 2443192.65118,
/// 1977-02-18 ~03:38 UT): querying that exact calendar day should
/// report it without needing any tolerance extension.
#[test]
fn single_day_range_is_the_degenerate_case_start_equals_end() -> Result<(), time::error::ComponentRange> {
    let day = date(1977, time::Month::February, 18)?;
    let events = AstroEphemeris::new().boundaries(day, day, None, None, BoundariesConfig::default());
    let Ok(events) = events else {
        unreachable!("a valid single-day range must not error");
    };

    assert!(
        events.iter().any(|event| matches!(
            event,
            HorizonEvent::MoonQuarter {
                phase: PrimaryMoonPhase::New,
                ..
            }
        )),
        "expected a New Moon event on its own verified date, got {events:?}"
    );
    Ok(())
}

/// Moon-quarter checkpoints are always included, with no `hemisphere`
/// or `birth_date` supplied; the other two categories must then be
/// entirely absent, not defaulted or errored.
#[test]
fn moon_quarters_are_always_included_without_hemisphere_or_birth_date() -> Result<(), time::error::ComponentRange> {
    let start = date(1977, time::Month::February, 17)?;
    let end = date(1977, time::Month::February, 27)?;
    let events = AstroEphemeris::new().boundaries(start, end, None, None, BoundariesConfig::default());
    let Ok(events) = events else {
        unreachable!("a valid range must not error");
    };

    let moon_phases: Vec<PrimaryMoonPhase> = events
        .iter()
        .filter_map(|event| match event {
            HorizonEvent::MoonQuarter { phase, .. } => Some(*phase),
            HorizonEvent::WheelOfYear { .. } | HorizonEvent::PersonalYearPeriodChange { .. } => None,
        })
        .collect();
    assert!(
        moon_phases.contains(&PrimaryMoonPhase::New),
        "expected the verified New Moon (1977-02-18) in range, got {events:?}"
    );
    assert!(
        moon_phases.contains(&PrimaryMoonPhase::FirstQuarter),
        "expected the First Quarter (1977-02-26) in range, got {events:?}"
    );
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, HorizonEvent::WheelOfYear { .. })),
        "no hemisphere supplied; Wheel-of-Year points must be entirely absent"
    );
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, HorizonEvent::PersonalYearPeriodChange { .. })),
        "no birth_date supplied; personal-year period changes must be entirely absent"
    );
    Ok(())
}

/// Wheel-of-Year points appear only once `hemisphere` is supplied.
#[test]
fn wheel_of_year_points_included_only_when_hemisphere_supplied() -> Result<(), time::error::ComponentRange> {
    let start = date(2024, time::Month::March, 15)?;
    let end = date(2024, time::Month::March, 25)?;
    let ephemeris = AstroEphemeris::new();

    let without_hemisphere = ephemeris.boundaries(start, end, None, None, BoundariesConfig::default());
    let Ok(without_hemisphere) = without_hemisphere else {
        unreachable!("a valid range must not error");
    };
    assert!(
        without_hemisphere
            .iter()
            .all(|event| !matches!(event, HorizonEvent::WheelOfYear { .. })),
    );

    let with_hemisphere = ephemeris.boundaries(
        start,
        end,
        None,
        Some(Hemisphere::Northern),
        BoundariesConfig::default(),
    );
    let Ok(with_hemisphere) = with_hemisphere else {
        unreachable!("a valid range must not error");
    };
    assert!(
        with_hemisphere.iter().any(|event| matches!(
            event,
            HorizonEvent::WheelOfYear {
                name: WheelOfYearName::SpringEquinox,
                role: WheelOfYearRole::Boundary,
                ..
            }
        )),
        "expected the March 2024 equinox in range, got {with_hemisphere:?}"
    );
    Ok(())
}

/// Personal-year period changes appear only once `birth_date` is
/// supplied; period 1 always begins exactly at the birthday.
#[test]
fn personal_year_period_changes_included_only_when_birth_date_supplied() -> Result<(), time::error::ComponentRange> {
    let birth_date = date(1990, time::Month::June, 15)?;
    let start = date(2024, time::Month::June, 10)?;
    let end = date(2024, time::Month::June, 20)?;
    let ephemeris = AstroEphemeris::new();

    let without_birth_date = ephemeris.boundaries(start, end, None, None, BoundariesConfig::default());
    let Ok(without_birth_date) = without_birth_date else {
        unreachable!("a valid range must not error");
    };
    assert!(
        without_birth_date
            .iter()
            .all(|event| !matches!(event, HorizonEvent::PersonalYearPeriodChange { .. })),
    );

    let with_birth_date = ephemeris.boundaries(start, end, Some(birth_date), None, BoundariesConfig::default());
    let Ok(with_birth_date) = with_birth_date else {
        unreachable!("a valid range must not error");
    };
    assert!(
        with_birth_date.iter().any(|event| matches!(
            event,
            HorizonEvent::PersonalYearPeriodChange {
                period_number: 1,
                ruling_planet: RulingPlanet::Sun,
                ..
            }
        )),
        "expected period 1 beginning at the 2024 birthday, got {with_birth_date:?}"
    );
    Ok(())
}

/// Events across all three categories, in one call, must come back in
/// strict chronological order.
#[test]
fn events_are_returned_in_chronological_order() -> Result<(), time::error::ComponentRange> {
    let birth_date = date(1990, time::Month::June, 15)?;
    let start = date(2024, time::Month::March, 1)?;
    let end = date(2024, time::Month::April, 1)?;
    let events = AstroEphemeris::new().boundaries(
        start,
        end,
        Some(birth_date),
        Some(Hemisphere::Southern),
        BoundariesConfig::default(),
    );
    let Ok(events) = events else {
        unreachable!("a valid range must not error");
    };

    assert!(events.len() > 1, "expected more than one event to order");
    for pair in events.windows(2) {
        assert!(
            pair[0].instant() <= pair[1].instant(),
            "{:?} should not come after {:?}",
            pair[0],
            pair[1]
        );
    }
    Ok(())
}

/// The "computed once across the window" claim, checked directly:
/// the verified 1977-02-18 ~03:38 UT New Moon is within the default
/// ±12h `moon_phase_tolerance` of 1977-02-17's own midnight-to-
/// midnight span end, even though it falls just outside that single
/// day's strict boundaries. A tolerance-unaware "does the instant fall
/// strictly within the day" check would miss it entirely.
#[test]
fn a_moon_quarter_just_outside_the_strict_range_is_still_reported_within_tolerance()
-> Result<(), time::error::ComponentRange> {
    let day_before = date(1977, time::Month::February, 17)?;
    let events = AstroEphemeris::new().boundaries(day_before, day_before, None, None, BoundariesConfig::default());
    let Ok(events) = events else {
        unreachable!("a valid single-day range must not error");
    };

    assert!(
        events.iter().any(|event| matches!(
            event,
            HorizonEvent::MoonQuarter {
                phase: PrimaryMoonPhase::New,
                ..
            }
        )),
        "expected the New Moon ~20 hours later, within the default ±12h \
             tolerance of this day's own end, got {events:?}"
    );
    Ok(())
}

#[test]
fn an_inverted_date_range_is_a_typed_error() -> Result<(), time::error::ComponentRange> {
    let start = date(2024, time::Month::June, 20)?;
    let end = date(2024, time::Month::June, 10)?;
    assert_eq!(
        AstroEphemeris::new().boundaries(start, end, None, None, BoundariesConfig::default()),
        Err(EphemerisError::InvalidDateRange {
            start_date: start,
            end_date: end,
        })
    );
    Ok(())
}
