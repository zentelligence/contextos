use time::macros::datetime;

use contextos_core::{ConfiguredTimeZone, OperatorTimeZone};

use super::*;

#[test]
fn daily_schedule_fires_later_the_same_day_when_still_before_its_hour() -> Result<(), String> {
    let schedule = CronSchedule::parse("0 3 * * *").map_err(|error| error.to_string())?;
    let Some(next) = schedule.next_fire_after(datetime!(2026-09-08 01:00:00 UTC), &OperatorTimeZone::utc()) else {
        return Err("expected a next occurrence".to_owned());
    };
    assert_eq!(next, datetime!(2026-09-08 03:00:00 UTC));
    Ok(())
}

#[test]
fn daily_schedule_rolls_over_to_the_next_day_once_its_hour_has_passed() -> Result<(), String> {
    let schedule = CronSchedule::parse("0 3 * * *").map_err(|error| error.to_string())?;
    let Some(next) = schedule.next_fire_after(datetime!(2026-09-08 03:00:00 UTC), &OperatorTimeZone::utc()) else {
        return Err("expected a next occurrence".to_owned());
    };
    assert_eq!(next, datetime!(2026-09-09 03:00:00 UTC));
    Ok(())
}

#[test]
fn weekly_schedule_matches_the_named_day_of_week() -> Result<(), String> {
    // Friday 2026-09-11 00:00 UTC.
    let schedule = CronSchedule::parse("0 0 * * FRI").map_err(|error| error.to_string())?;
    let Some(next) = schedule.next_fire_after(datetime!(2026-09-08 00:00:00 UTC), &OperatorTimeZone::utc()) else {
        return Err("expected a next occurrence".to_owned());
    };
    assert_eq!(next, datetime!(2026-09-11 00:00:00 UTC));
    Ok(())
}

#[test]
fn invalid_pattern_is_rejected_with_the_offending_expression() -> Result<(), String> {
    let Err(error) = CronSchedule::parse("not a cron expression") else {
        return Err("expected the pattern to be rejected".to_owned());
    };
    let message = error.to_string();
    assert!(message.contains("not a cron expression"), "message was {message:?}");
    Ok(())
}

#[test]
fn the_next_fire_is_the_same_instant_whatever_offset_the_search_starts_from() -> Result<(), CronScheduleError> {
    let schedule = CronSchedule::parse("0 9 * * *")?;
    // 23:00 UTC is already 10:00 the next morning in Sydney, past that day's 09:00.
    let utc = time::macros::datetime!(2026-10-08 23:00 UTC);
    let sydney = utc.to_offset(time::macros::offset!(+11));

    assert_eq!(
        schedule.next_fire_after(sydney, &OperatorTimeZone::utc()),
        schedule.next_fire_after(utc, &OperatorTimeZone::utc())
    );
    assert_eq!(
        schedule.next_fire_after(sydney, &OperatorTimeZone::utc()),
        Some(time::macros::datetime!(2026-10-09 09:00 UTC))
    );
    Ok(())
}

fn sydney() -> Result<OperatorTimeZone, Box<dyn std::error::Error>> {
    Ok(OperatorTimeZone::from(ConfiguredTimeZone::try_from(
        "Australia/Sydney",
    )?))
}

#[test]
fn a_daily_seven_am_is_seven_am_in_the_operators_zone_in_summer_and_winter() -> Result<(), Box<dyn std::error::Error>> {
    let schedule = CronSchedule::parse("0 7 * * *")?;

    // 21:45 UTC on 8 October is 08:45 on 9 October in Sydney (+11:00), past that day's 07:00.
    assert_eq!(
        schedule.next_fire_after(datetime!(2026-10-08 21:45 UTC), &sydney()?),
        Some(datetime!(2026-10-10 07:00 +11:00))
    );
    // 21:45 UTC on 8 July is 07:45 on 9 July in Sydney (+10:00).
    assert_eq!(
        schedule.next_fire_after(datetime!(2026-07-08 21:45 UTC), &sydney()?),
        Some(datetime!(2026-07-10 07:00 +10:00))
    );
    Ok(())
}

#[test]
fn a_fixed_time_the_clock_skips_runs_at_the_first_time_after_the_skip() -> Result<(), Box<dyn std::error::Error>> {
    // On 4 October 2026 Sydney's clocks go from 02:00 straight to 03:00, so 02:30 never happens that night.
    let schedule = CronSchedule::parse("30 2 * * *")?;

    assert_eq!(
        schedule.next_fire_after(datetime!(2026-10-03 12:00 UTC), &sydney()?),
        Some(datetime!(2026-10-04 03:00 +11:00))
    );
    Ok(())
}

#[test]
fn a_fixed_time_the_clock_repeats_runs_once_at_its_first_occurrence() -> Result<(), Box<dyn std::error::Error>> {
    // On 4 April 2027 Sydney's clocks go from 03:00 back to 02:00, so 02:30 happens twice; the job runs at the first.
    let schedule = CronSchedule::parse("30 2 * * *")?;
    let zone = sydney()?;

    let first = schedule.next_fire_after(datetime!(2027-04-03 12:00 UTC), &zone);
    let after_first = first.and_then(|first| schedule.next_fire_after(first, &zone));

    assert_eq!(first, Some(datetime!(2027-04-04 02:30 +11:00)));
    assert_eq!(after_first, Some(datetime!(2027-04-05 02:30 +10:00)));
    Ok(())
}
