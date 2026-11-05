use time::Month;

use super::*;

type BoxError = Box<dyn std::error::Error>;

fn at(year: i32, month: Month, day: u8, hour: u8, minute: u8) -> Result<OffsetDateTime, BoxError> {
    let date = time::Date::from_calendar_date(year, month, day)?;
    Ok(date.with_hms(hour, minute, 0)?.assume_utc())
}

#[test]
fn default_title_renders_a_readable_timestamp_based_title() -> Result<(), BoxError> {
    let instant = at(2026, Month::September, 17, 7, 5)?;
    assert_eq!(default_title(instant), "Insight 2026-09-17 0705");
    Ok(())
}

#[test]
fn default_title_always_slugifies_to_a_non_empty_sluggable_string() -> Result<(), BoxError> {
    let instant = at(2026, Month::January, 1, 0, 0)?;
    assert!(
        !crate::slug::slugify(&default_title(instant)).is_empty(),
        "`crate::slug::slugify(&default_title(instant))` should not be empty"
    );
    Ok(())
}

#[test]
fn classify_delete_failure_maps_the_stable_not_found_code_to_not_found() {
    assert!(matches!(
        classify_delete_failure("path/not-found"),
        DeleteNoteError::NotFound
    ));
}

#[test]
fn classify_delete_failure_treats_every_other_code_as_a_plain_rejection() {
    assert!(matches!(
        classify_delete_failure("io/directory-not-empty"),
        DeleteNoteError::Rejected
    ));
    // No parsable structured body at all (`delete_note`'s own
    // `unwrap_or_default` fallback) degrades to the same plain rejection,
    // never a false `NotFound`.
    assert!(matches!(classify_delete_failure(""), DeleteNoteError::Rejected));
}

#[test]
fn filename_without_extension_strips_the_shard_folder_and_the_md_suffix() {
    assert_eq!(
        filename_without_extension("a/agent-backbone-is-three-components.md"),
        "agent-backbone-is-three-components"
    );
}

#[test]
fn load_insight_error_from_read_insight_error_preserves_not_found() {
    assert!(matches!(
        LoadInsightError::from(ReadInsightError::NotFound),
        LoadInsightError::NotFound
    ));
}

#[test]
fn note_path_joins_the_configured_directory_and_the_relative_path() {
    let config = InsightsConfig {
        insights_dir: "scratch".to_owned(),
        ..InsightsConfig::default()
    };
    assert_eq!(note_path(&config, "a/some-note.md"), "scratch/a/some-note.md");
}

#[test]
fn statuses_is_the_closed_zettelkasten_vocabulary() {
    assert_eq!(STATUSES, ["proposed", "adopted", "dismissed"]);
}

#[test]
fn list_shards_returns_the_full_fixed_thirty_six_key_space_in_order() {
    let shards = list_shards();
    assert_eq!(shards.len(), 36);
    assert_eq!(shards.first(), Some(&"a".to_owned()));
    assert_eq!(shards.last(), Some(&"9".to_owned()));
    // Digits and letters absent from any real vault on disk must still
    // appear: this is a fixed keyspace, never derived from what a vault
    // happens to already contain.
    for key in ["a", "q", "z", "0", "5", "9"] {
        assert!(shards.iter().any(|shard| shard == key), "missing shard key {key}");
    }
    let mut sorted = shards.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 36, "shard keys must be unique");
}
