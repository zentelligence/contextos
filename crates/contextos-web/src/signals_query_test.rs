use time::Duration;
use time::macros::datetime;

use super::*;

fn cursor(at: OffsetDateTime) -> Cursor {
    Cursor::new(at, "memory/events/x.md")
}

#[test]
fn the_feed_filter_adds_only_the_filters_that_are_set() {
    let none = feed_filter(&FeedFilters::default());
    assert_eq!(none, EVENT_NOTES_FILTER);

    let all = feed_filter(&FeedFilters {
        kind: Some("task.created".to_owned()),
        severity: Some("warning".to_owned()),
        entity: Some("zenith".to_owned()),
    });
    assert!(
        all.ends_with("kind == \"task.created\" && severity == \"warning\" && entity == \"zenith\""),
        "{all}"
    );

    let blank = feed_filter(&FeedFilters {
        kind: Some(String::new()),
        ..FeedFilters::default()
    });
    assert_eq!(blank, EVENT_NOTES_FILTER, "an empty selection is no filter");
}

#[test]
fn a_filter_value_with_quotes_cannot_break_out_of_its_literal() {
    let filter = feed_filter(&FeedFilters {
        kind: Some("a\" || file.path != \"".to_owned()),
        ..FeedFilters::default()
    });

    assert!(filter.contains("kind == \"a\\\" || file.path != \\\"\""), "{filter}");
}

#[test]
fn the_attention_filter_selects_warnings_and_errors_and_narrows_to_an_entity() {
    let all = attention_filter(None);
    assert!(all.contains("severity == \"warning\" || severity == \"error\""));
    assert!(!all.contains("entity =="));

    assert!(attention_filter(Some("acme")).ends_with("entity == \"acme\""));
}

#[test]
fn the_clearing_filter_names_only_the_subjects_codes_and_jobs_of_the_warnings_it_is_for() {
    let filter = clearing_filter(&Clearers {
        subjects: vec!["registry/agents/ellis".to_owned(), "registry/agents/rowan".to_owned()],
        keys: vec!["A-1".to_owned()],
        codes: vec!["git/unavailable".to_owned()],
        jobs: vec!["nightly".to_owned()],
    })
    .unwrap_or_default();

    assert!(
        filter.contains("subject.containsAny(\"registry/agents/ellis\", \"registry/agents/rowan\")"),
        "{filter}"
    );
    assert!(filter.contains("subject_id.containsAny(\"A-1\")"), "{filter}");
    assert!(
        filter.contains("subject_path.containsAny(\"registry/agents/ellis\", \"registry/agents/rowan\")"),
        "a note linked by name is found by the path the server stamped: {filter}"
    );
    assert!(
        filter.contains("kind == \"vault.recovered\" && code.containsAny(\"git/unavailable\")"),
        "{filter}"
    );
    assert!(
        filter.contains("kind == \"job.recovered\" && job.containsAny(\"nightly\")"),
        "{filter}"
    );
}

#[test]
fn the_clearing_filter_leaves_out_what_the_warnings_give_no_reason_to_look_for() {
    let subjects_only = clearing_filter(&Clearers {
        subjects: vec!["memory/tasks/a-1".to_owned()],
        ..Clearers::default()
    })
    .unwrap_or_default();

    assert!(
        !subjects_only.contains("recovered"),
        "no recovery can clear them: {subjects_only}"
    );
    assert_eq!(
        clearing_filter(&Clearers::default()),
        None,
        "nothing could clear nothing"
    );
}

#[test]
fn the_background_filter_leaves_out_every_shipped_kind_by_exact_name_and_no_other() {
    let filter = background_filter(None);

    for kind in [
        "task.created",
        "task.moved",
        "member.state-changed",
        "vault.degraded",
        "job.failed",
        "job.retired",
        "session.run-finished",
        "agent.delegated",
    ] {
        assert!(filter.contains(&format!("kind != \"{kind}\"")), "{kind}: {filter}");
    }
    assert!(
        !filter.contains("contains("),
        "no substring comparison can hide a recorded kind: {filter}"
    );
    assert_eq!(filter.matches("kind != ").count(), 16, "{filter}");
}

#[test]
fn events_after_a_cursor_scan_its_month_to_now_narrowed_to_the_cursors_second_and_the_days_after_it_to_tomorrow() {
    let now = datetime!(2026-10-28 03:00:00 UTC);
    let filter = after_filter(Some(&cursor(datetime!(2026-10-27 23:59:59 UTC))), now);

    assert!(filter.contains("file.inFolder(\"memory/events/2026/10\")"), "{filter}");
    assert!(
        filter.contains("at.containsAny(\"2026-10-27T23:59:59\", \"2026-10-28\", \"2026-10-29\")"),
        "{filter}"
    );
}

#[test]
fn near_a_month_boundary_the_previous_months_folder_is_scanned_too() {
    let now = datetime!(2026-11-01 00:00:10 UTC);

    let with_cursor = after_filter(Some(&cursor(datetime!(2026-10-31 23:59:58 UTC))), now);
    assert!(
        with_cursor.contains("memory/events/2026/10") && with_cursor.contains("memory/events/2026/11"),
        "{with_cursor}"
    );

    let without = after_filter(None, now);
    assert!(
        without.contains("memory/events/2026/10") && without.contains("memory/events/2026/11"),
        "{without}"
    );
}

#[test]
fn a_year_boundary_rolls_the_month_over() {
    let now = datetime!(2027-01-01 00:00:10 UTC);

    let filter = after_filter(Some(&cursor(datetime!(2026-12-31 23:59:58 UTC))), now);

    assert!(
        filter.contains("memory/events/2026/12") && filter.contains("memory/events/2027/01"),
        "{filter}"
    );
}

#[test]
fn a_cursor_months_old_scans_the_whole_events_folder_and_is_still_bounded_below_by_whole_months() {
    let now = datetime!(2026-10-28 03:00:00 UTC);

    let filter = after_filter(Some(&cursor(datetime!(2026-04-01 00:00:00 UTC))), now);

    assert!(!filter.contains("memory/events/2026/"), "{filter}");
    assert!(
        filter.contains("\"2026-04-30\", \"2026-05\", \"2026-06\""),
        "the rest of the cursor's month by day, then whole months: {filter}"
    );
    assert!(filter.contains("\"2026-10\""), "{filter}");
    assert!(
        !filter.contains("\"2026-10-28\""),
        "later months are not listed by day: {filter}"
    );
}

/// Whether an `at` written as `instant` starts with one of `prefixes`.
fn covered(prefixes: &[String], instant: OffsetDateTime) -> bool {
    let text = instant
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    prefixes.iter().any(|prefix| text.starts_with(prefix.as_str()))
}

fn end_of(instant: OffsetDateTime, unit: &str) -> OffsetDateTime {
    let date = instant.date();
    let last_day = |year: i32, month: time::Month| {
        time::Date::from_calendar_date(year, month, time::util::days_in_month(month, year)).unwrap_or(date)
    };
    let day_end = |date: time::Date| {
        date.with_hms(23, 59, 59)
            .map_or(instant, time::PrimitiveDateTime::assume_utc)
    };
    match unit {
        "minute" => instant.replace_second(59).unwrap_or(instant),
        "hour" => instant
            .replace_second(59)
            .and_then(|at| at.replace_minute(59))
            .unwrap_or(instant),
        "day" => day_end(date),
        "month" => day_end(last_day(date.year(), date.month())),
        "year" => day_end(last_day(date.year(), time::Month::December)),
        _ => instant,
    }
}

#[test]
fn the_prefixes_cover_every_instant_from_the_cursors_second_to_the_end_of_tomorrow_and_nothing_before() {
    for (from, now, narrowed) in [
        (
            datetime!(2026-10-28 10:15:57 UTC),
            datetime!(2026-10-28 12:00:00 UTC),
            true,
        ),
        (
            datetime!(2026-10-28 23:59:59 UTC),
            datetime!(2026-10-29 00:00:01 UTC),
            true,
        ),
        (
            datetime!(2026-10-31 23:59:59 UTC),
            datetime!(2026-11-02 00:00:01 UTC),
            true,
        ),
        (
            datetime!(2026-12-31 23:59:59 UTC),
            datetime!(2027-01-01 00:00:01 UTC),
            true,
        ),
        (
            datetime!(2026-09-01 00:00:00 UTC),
            datetime!(2026-10-05 00:00:00 UTC),
            true,
        ),
        (
            datetime!(2026-08-01 00:00:00 UTC),
            datetime!(2026-10-28 03:00:00 UTC),
            false,
        ),
        (
            datetime!(2026-01-31 23:59:59 UTC),
            datetime!(2026-10-28 03:00:00 UTC),
            false,
        ),
        (
            datetime!(2024-01-01 00:00:00 UTC),
            datetime!(2026-10-28 03:00:00 UTC),
            false,
        ),
        (
            datetime!(2023-12-31 23:59:59 UTC),
            datetime!(2026-01-01 00:00:10 UTC),
            false,
        ),
        (
            datetime!(2025-12-15 08:30:30 UTC),
            datetime!(2026-02-20 03:00:00 UTC),
            false,
        ),
    ] {
        let prefixes = at_prefixes(from, now);
        let tomorrow_end = end_of(
            now.date().next_day().map_or(now, |day| day.midnight().assume_utc()),
            "day",
        );
        let mut edges = vec![from, tomorrow_end];
        for unit in ["minute", "hour", "day", "month", "year"] {
            edges.push(end_of(from, unit));
        }
        for edge in edges {
            for instant in [edge - Duration::seconds(1), edge, edge + Duration::seconds(1)] {
                if instant < from {
                    assert!(!covered(&prefixes, instant), "{instant} is before {from}");
                } else if instant <= tomorrow_end {
                    assert!(covered(&prefixes, instant), "{instant} is within the read from {from}");
                } else if narrowed {
                    assert!(
                        !covered(&prefixes, instant),
                        "{instant} is beyond tomorrow, from {from}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_cursor_a_century_old_is_bounded_without_listing_thousands_of_years() {
    let now = datetime!(2026-10-28 03:00:00 UTC);

    let prefixes = at_prefixes(datetime!(0001-01-01 00:00:00 UTC), now);

    assert!(prefixes.len() < 400, "{} prefixes", prefixes.len());
    assert!(covered(&prefixes, now));
}

#[test]
fn a_read_from_a_second_is_bounded_below_by_prefixes_of_at_down_to_that_second() {
    let now = datetime!(2026-10-28 12:00:00 UTC);

    let filter = since_filter(EVENT_NOTES_FILTER, Some(datetime!(2026-10-28 10:15:57 UTC)), now, &[]);

    for prefix in [
        "2026-10-28T10:15:57",
        "2026-10-28T10:15:59",
        "2026-10-28T10:16",
        "2026-10-28T10:59",
        "2026-10-28T11",
        "2026-10-28T23",
    ] {
        assert!(filter.contains(&format!("\"{prefix}\"")), "{prefix}: {filter}");
    }
    for prefix in [
        "2026-10-28T10:15:56",
        "2026-10-28T10:15\"",
        "2026-10-28T10\"",
        "2026-10-28\"",
    ] {
        assert!(
            !filter.contains(prefix),
            "an earlier instant would be read: {prefix}: {filter}"
        );
    }
}

#[test]
fn a_read_from_an_earlier_day_adds_each_later_day_whole() {
    let now = datetime!(2026-10-30 01:00:00 UTC);

    let filter = since_filter(EVENT_NOTES_FILTER, Some(datetime!(2026-10-28 23:59:59 UTC)), now, &[]);

    assert!(
        filter.contains("\"2026-10-28T23:59:59\", \"2026-10-29\", \"2026-10-30\""),
        "{filter}"
    );
}

#[test]
fn a_read_that_is_not_bounded_below_excludes_the_paths_already_found() {
    let now = datetime!(2026-10-28 12:00:00 UTC);

    let filter = since_filter(EVENT_NOTES_FILTER, None, now, &["memory/events/2026/10/a.md"]);

    assert!(
        filter.ends_with("&& file.path != \"memory/events/2026/10/a.md\""),
        "{filter}"
    );
    assert!(!filter.contains("at.containsAny"), "{filter}");
}
