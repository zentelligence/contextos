use time::macros::datetime;

use super::*;
use crate::frontmatter_timestamp;

/// A clock frozen at one instant, so the local offset can be checked on either side of a transition.
#[derive(Clone, Copy, Debug)]
struct FixedClock(OffsetDateTime);

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

fn sydney() -> Result<OperatorTimeZone, TimeZoneError> {
    Ok(OperatorTimeZone::from(ConfiguredTimeZone::try_from(
        "Australia/Sydney",
    )?))
}

fn local_at(zone: &OperatorTimeZone, instant: OffsetDateTime) -> OffsetDateTime {
    LocalClock::from(LocalClockInput {
        clock: FixedClock(instant),
        zone: zone.clone(),
    })
    .now()
}

#[test]
fn a_configured_zone_reports_standard_time_before_daylight_saving_begins() -> Result<(), TimeZoneError> {
    // 01:00 AEST on the morning Sydney moves to daylight saving time.
    let local = local_at(&sydney()?, datetime!(2026-10-03 15:00 UTC));

    assert_eq!(local, datetime!(2026-10-04 01:00 +10:00));
    Ok(())
}

#[test]
fn a_configured_zone_reports_daylight_saving_time_once_it_has_begun() -> Result<(), TimeZoneError> {
    // 03:30 AEDT, after the 02:00 jump to 03:00.
    let local = local_at(&sydney()?, datetime!(2026-10-03 16:30 UTC));

    assert_eq!(local, datetime!(2026-10-04 03:30 +11:00));
    Ok(())
}

#[test]
fn a_configured_zone_returns_to_standard_time_when_daylight_saving_ends() -> Result<(), TimeZoneError> {
    // Sydney leaves daylight saving at 03:00 AEDT on the first Sunday of April 2027, back to 02:00 AEST.
    let before = local_at(&sydney()?, datetime!(2027-04-03 15:59 UTC));
    let after = local_at(&sydney()?, datetime!(2027-04-03 16:01 UTC));

    assert_eq!(before, datetime!(2027-04-04 02:59 +11:00));
    assert_eq!(after, datetime!(2027-04-04 02:01 +10:00));
    Ok(())
}

#[test]
fn the_local_time_is_the_same_instant_as_the_clock_reported() -> Result<(), TimeZoneError> {
    let instant = datetime!(2026-10-03 16:30 UTC);

    assert_eq!(local_at(&sydney()?, instant), instant);
    Ok(())
}

#[test]
fn a_frontmatter_stamp_carries_the_local_offset_and_date() -> Result<(), TimeZoneError> {
    // 21:45:30 UTC on 8 October is already the morning of 9 October in Sydney, under daylight saving time.
    let stamp = frontmatter_timestamp(local_at(&sydney()?, datetime!(2026-10-08 21:45:30 UTC)));

    assert_eq!(stamp, "2026-10-09T08:45:00+11:00");
    Ok(())
}

#[test]
fn a_configured_zone_is_reported_by_its_iana_name_and_source() -> Result<(), TimeZoneError> {
    let zone = sydney()?;

    assert_eq!(zone.name(), Some("Australia/Sydney"));
    assert_eq!(zone.source(), TimeZoneSource::Configured);
    Ok(())
}

#[test]
fn an_unknown_zone_name_is_rejected_naming_the_value() {
    let error = ConfiguredTimeZone::try_from("Australia/Atlantis").err();

    assert!(
        matches!(&error, Some(TimeZoneError::Unknown { name }) if name == "Australia/Atlantis"),
        "{error:?}"
    );
}

#[test]
fn a_fixed_offset_or_malformed_value_is_rejected_rather_than_read_as_a_zone() {
    for value in ["", " ", "+10:00", "UTC+10", "Australia/Sydney ", "../etc/passwd"] {
        assert!(ConfiguredTimeZone::try_from(value).is_err(), "{value:?}");
    }
}

#[test]
fn utc_is_a_valid_configured_zone() -> Result<(), TimeZoneError> {
    let utc = OperatorTimeZone::from(ConfiguredTimeZone::try_from("UTC")?);

    assert_eq!(
        local_at(&utc, datetime!(2026-10-08 21:45 UTC)),
        datetime!(2026-10-08 21:45 UTC)
    );
    Ok(())
}

#[test]
fn the_host_zone_is_used_when_none_is_configured_and_says_so() {
    let host = OperatorTimeZone::from(None);

    assert!(matches!(
        host.source(),
        TimeZoneSource::System | TimeZoneSource::Undetected
    ));
    if host.source() == TimeZoneSource::Undetected {
        assert_eq!(host.name(), Some("UTC"));
    }
}

#[test]
fn a_zone_reported_by_its_iana_name_is_that_zone() -> Result<(), TimeZoneError> {
    let zone = OperatorTimeZone::try_from(ReportedTimeZone {
        name: Some("Australia/Sydney"),
        rules: None,
    })?;

    assert_eq!(
        local_at(&zone, datetime!(2026-10-03 16:30 UTC)),
        datetime!(2026-10-04 03:30 +11:00)
    );
    Ok(())
}

#[test]
fn a_zone_reported_only_by_posix_rules_keeps_its_daylight_saving() -> Result<(), TimeZoneError> {
    let zone = OperatorTimeZone::try_from(ReportedTimeZone {
        name: None,
        rules: Some("AEST-10AEDT,M10.1.0,M4.1.0/3"),
    })?;

    assert_eq!(
        local_at(&zone, datetime!(2026-10-03 15:00 UTC)),
        datetime!(2026-10-04 01:00 +10:00)
    );
    assert_eq!(
        local_at(&zone, datetime!(2026-10-03 16:30 UTC)),
        datetime!(2026-10-04 03:30 +11:00)
    );
    assert_eq!(zone.rules(), Some("AEST-10AEDT,M10.1.0,M4.1.0/3"));
    Ok(())
}

#[test]
fn a_reported_zone_with_neither_name_nor_usable_rules_is_refused() {
    for reported in [
        ReportedTimeZone {
            name: None,
            rules: None,
        },
        ReportedTimeZone {
            name: None,
            rules: Some("not rules"),
        },
        ReportedTimeZone {
            name: Some("Mars/Olympus"),
            rules: None,
        },
    ] {
        assert!(OperatorTimeZone::try_from(reported).is_err());
    }
}

#[test]
fn an_ordinary_local_time_names_one_instant() -> Result<(), TimeZoneError> {
    let resolved = sydney()?.resolve_local(datetime!(2026-10-09 07:00));

    assert_eq!(resolved, LocalResolution::Single(datetime!(2026-10-09 07:00 +11:00)));
    Ok(())
}

#[test]
fn a_local_time_the_clock_skips_when_daylight_saving_begins_names_no_instant() -> Result<(), TimeZoneError> {
    // Sydney's clocks go from 02:00 straight to 03:00 on 4 October 2026.
    assert_eq!(
        sydney()?.resolve_local(datetime!(2026-10-04 02:30)),
        LocalResolution::Gap
    );
    Ok(())
}

#[test]
fn a_local_time_the_clock_repeats_when_daylight_saving_ends_names_both_instants_earlier_first()
-> Result<(), TimeZoneError> {
    // Sydney's clocks go from 03:00 back to 02:00 on 4 April 2027, so 02:30 happens twice.
    assert_eq!(
        sydney()?.resolve_local(datetime!(2027-04-04 02:30)),
        LocalResolution::Repeated(datetime!(2027-04-04 02:30 +11:00), datetime!(2027-04-04 02:30 +10:00))
    );
    Ok(())
}

#[test]
fn utc_is_available_as_a_zone_without_configuration() {
    assert_eq!(OperatorTimeZone::utc().name(), Some("UTC"));
    assert_eq!(
        OperatorTimeZone::utc().resolve_local(datetime!(2026-10-09 07:00)),
        LocalResolution::Single(datetime!(2026-10-09 07:00 UTC))
    );
}
