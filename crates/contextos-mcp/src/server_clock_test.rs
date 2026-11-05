use super::*;
use time::macros::datetime;

#[test]
fn an_injected_clock_reports_exactly_the_instant_it_was_given() {
    let fixed = datetime!(2030-01-02 03:04:05 UTC);
    let clock = ServerClock::from(move || fixed);
    assert_eq!(clock.now(), fixed);
    assert_eq!(clock.clone().now(), fixed);
}

#[test]
fn the_default_clock_is_the_system_clock() {
    let before = OffsetDateTime::now_utc();
    let reported = ServerClock::default().now();
    assert!(reported >= before);
    assert!(reported - before < time::Duration::minutes(1));
}
