use time::macros::datetime;

use super::*;

fn cursor(second: u32, path: &str) -> Cursor {
    Cursor::new(
        datetime!(2026-10-28 03:00:00 UTC) + time::Duration::seconds(i64::from(second)),
        path,
    )
}

#[test]
fn nothing_is_delivered_until_it_is_recorded_and_a_recorded_event_is_not_new_again() {
    let mut delivered = Delivered::default();
    assert!(delivered.is_new(&cursor(1, "b")));
    assert_eq!(delivered.read_from(), None);

    delivered.record(&cursor(1, "b"));

    assert!(!delivered.is_new(&cursor(1, "b")));
    assert_eq!(delivered.read_from(), Some(datetime!(2026-10-28 03:00:01 UTC)));
}

#[test]
fn a_later_event_in_the_same_second_with_a_lower_path_is_new() {
    let mut delivered = Delivered::default();
    delivered.record(&cursor(1, "memory/events/z.md"));

    assert!(delivered.is_new(&cursor(1, "memory/events/a.md")));
}

#[test]
fn an_event_older_than_the_second_reads_start_from_is_not_new() {
    let mut delivered = Delivered::default();
    delivered.record(&cursor(5, "x"));

    assert!(!delivered.is_new(&cursor(4, "y")));
}

#[test]
fn recording_a_later_second_forgets_the_paths_of_the_seconds_before_it() {
    let mut delivered = Delivered::default();
    delivered.record(&cursor(1, "a"));
    delivered.record(&cursor(1, "b"));
    delivered.record(&cursor(2, "c"));

    assert_eq!(delivered.paths.len(), 1, "only the newest second is remembered");
    assert!(!delivered.is_new(&cursor(2, "c")));
}

#[test]
fn a_set_started_at_a_second_reads_from_its_start_and_counts_earlier_events_as_old() {
    let delivered = Delivered::from(datetime!(2026-10-28 03:00:07.5 UTC));

    assert_eq!(delivered.read_from(), Some(datetime!(2026-10-28 03:00:07 UTC)));
    assert!(delivered.is_new(&cursor(7, "a")));
    assert!(!delivered.is_new(&cursor(6, "a")));
}
