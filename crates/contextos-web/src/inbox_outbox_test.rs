use super::*;

fn file_entry(name: &str) -> RawEntry {
    RawEntry {
        name: name.to_owned(),
        kind: "file".to_owned(),
        size: Some(1024),
        modified: Some("2026-09-14T10:23:45.123456789+00:00".to_owned()),
    }
}

fn dir_entry(name: &str) -> RawEntry {
    RawEntry {
        name: name.to_owned(),
        kind: "dir".to_owned(),
        size: None,
        modified: None,
    }
}

#[test]
fn folders_and_files_are_split_and_each_sorted_by_name() {
    let listing = listing_from_entries(vec![
        file_entry("b.md"),
        dir_entry("z-folder"),
        file_entry("a.md"),
        dir_entry("a-folder"),
    ]);
    let folder_names: Vec<&str> = listing.folders.iter().map(|folder| folder.name.as_str()).collect();
    let file_names: Vec<&str> = listing.files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(folder_names, vec!["a-folder", "z-folder"]);
    assert_eq!(file_names, vec!["a.md", "b.md"]);
}

#[test]
fn index_md_is_excluded_from_the_listing() {
    let listing = listing_from_entries(vec![file_entry("index.md"), file_entry("notes.md")]);
    let file_names: Vec<&str> = listing.files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(file_names, vec!["notes.md"]);
}

#[test]
fn an_empty_directory_yields_an_empty_listing() {
    let listing = listing_from_entries(Vec::new());
    assert!(
        listing.folders.is_empty(),
        "`listing.folders` should be empty, got {:?}",
        listing.folders
    );
    assert!(
        listing.files.is_empty(),
        "`listing.files` should be empty, got {:?}",
        listing.files
    );
}

#[test]
fn size_under_one_mebibyte_renders_as_whole_kibibytes() {
    assert_eq!(format_size(500), "1 KB");
    assert_eq!(format_size(2048), "2 KB");
}

#[test]
fn size_at_or_over_one_mebibyte_renders_with_one_decimal() {
    assert_eq!(format_size(1024 * 1024), "1.0 MB");
    assert_eq!(format_size(1024 * 1024 * 3 / 2), "1.5 MB");
}

#[test]
fn a_full_rfc3339_timestamp_renders_as_date_and_minute() {
    assert_eq!(
        format_modified(Some("2026-09-14T10:23:45.123456789+00:00")),
        "2026-09-14 10:23"
    );
}

#[test]
fn a_missing_or_short_timestamp_renders_as_a_dash_rather_than_panicking() {
    assert_eq!(format_modified(None), "-");
    assert_eq!(format_modified(Some("2026")), "-");
}

#[test]
fn a_plain_path_segment_is_accepted_and_trimmed() {
    assert_eq!(sanitised_path_segment("inbox"), Ok("inbox"));
    assert_eq!(sanitised_path_segment("  inbox  "), Ok("inbox"));
}

#[test]
fn an_empty_or_whitespace_only_segment_is_rejected() {
    assert!(sanitised_path_segment("").is_err());
    assert!(sanitised_path_segment("   ").is_err());
}

#[test]
fn a_segment_carrying_a_path_component_is_rejected() {
    assert!(sanitised_path_segment("../escape").is_err());
    assert!(sanitised_path_segment("sub/dir").is_err());
    assert!(sanitised_path_segment("sub\\dir").is_err());
    assert!(sanitised_path_segment(".").is_err());
    assert!(sanitised_path_segment("..").is_err());
}

#[test]
fn an_empty_relative_path_is_the_root_with_no_segments() -> Result<(), &'static str> {
    assert_eq!(sanitised_relative_path("")?, Vec::<&str>::new());
    assert_eq!(sanitised_relative_path("  ")?, Vec::<&str>::new());
    assert_eq!(sanitised_relative_path("/")?, Vec::<&str>::new());
    Ok(())
}

#[test]
fn a_multi_segment_relative_path_splits_and_trims_slashes() -> Result<(), &'static str> {
    assert_eq!(
        sanitised_relative_path("inbox/2026-09/receipts")?,
        vec!["inbox", "2026-09", "receipts"]
    );
    assert_eq!(sanitised_relative_path("/inbox/2026-09/")?, vec!["inbox", "2026-09"]);
    Ok(())
}

#[test]
fn a_relative_path_with_a_traversal_segment_is_rejected() {
    assert!(sanitised_relative_path("inbox/../escape").is_err());
    assert!(sanitised_relative_path("inbox//receipts").is_err());
}

#[test]
fn encode_query_value_leaves_unreserved_characters_and_slashes_untouched() {
    assert_eq!(
        encode_query_value("inbox/2026-09_receipts.v1"),
        "inbox/2026-09_receipts.v1"
    );
}

#[test]
fn encode_query_value_escapes_spaces_and_reserved_characters() {
    assert_eq!(encode_query_value("Meeting Notes & Q3"), "Meeting%20Notes%20%26%20Q3");
}

#[test]
fn decode_percent_encoded_round_trips_an_encoded_value() {
    let original = "Meeting Notes & Q3/domain-topic.md";
    assert_eq!(
        decode_percent_encoded(&encode_query_value(original)).as_deref(),
        Some(original)
    );
}

#[test]
fn decode_percent_encoded_rejects_invalid_hex_digits() {
    assert_eq!(decode_percent_encoded("100%ZZ"), None);
}

fn config() -> InboxOutboxConfig {
    InboxOutboxConfig::default()
}

#[test]
fn an_absent_or_empty_inbox_path_resolves_to_the_merged_root() -> Result<(), &'static str> {
    assert_eq!(resolve_inbox_location(&config(), None)?, InboxLocation::MergedRoot);
    assert_eq!(resolve_inbox_location(&config(), Some(""))?, InboxLocation::MergedRoot);
    Ok(())
}

#[test]
fn an_inbox_path_resolves_to_a_pending_tree_location_relative_to_the_inbox_directory() -> Result<(), &'static str> {
    let location = resolve_inbox_location(&config(), Some("2026-09/receipts"))?;
    assert_eq!(
        location,
        InboxLocation::Tree {
            root_dir: "inbox".to_owned(),
            status: "pending",
            sub_segments: vec!["2026-09".to_owned(), "receipts".to_owned()],
        }
    );
    assert_eq!(location.directory().as_deref(), Some("inbox/2026-09/receipts"));
    assert_eq!(location.url_path(), "2026-09/receipts");
    Ok(())
}

#[test]
fn a_processed_tree_path_resolves_to_a_processed_tree_location() -> Result<(), &'static str> {
    let location = resolve_inbox_location(&config(), Some("processed/2026-09"))?;
    assert_eq!(
        location,
        InboxLocation::Tree {
            root_dir: "processed".to_owned(),
            status: "processed",
            sub_segments: vec!["2026-09".to_owned()],
        }
    );
    assert_eq!(location.directory().as_deref(), Some("processed/2026-09"));
    assert_eq!(location.url_path(), "processed/2026-09");
    Ok(())
}

#[test]
fn the_processed_directory_name_alone_resolves_to_the_processed_tree_root() -> Result<(), &'static str> {
    let location = resolve_inbox_location(&config(), Some("processed"))?;
    assert_eq!(location.directory().as_deref(), Some("processed"));
    assert_eq!(location.url_path(), "processed");
    Ok(())
}

#[test]
fn the_inbox_directory_name_is_an_ordinary_subfolder_not_a_second_root() -> Result<(), &'static str> {
    let location = resolve_inbox_location(&config(), Some("inbox/2026-09"))?;
    assert_eq!(location.directory().as_deref(), Some("inbox/inbox/2026-09"));
    Ok(())
}

#[test]
fn an_inbox_path_never_leaves_the_inbox_directory() -> Result<(), &'static str> {
    let location = resolve_inbox_location(&config(), Some("memory/tasks"))?;
    assert_eq!(location.directory().as_deref(), Some("inbox/memory/tasks"));
    Ok(())
}

#[test]
fn a_malformed_inbox_path_is_rejected() {
    assert!(resolve_inbox_location(&config(), Some("2026-09/../escape")).is_err());
}

#[test]
fn the_merged_root_has_no_directory_of_its_own() {
    assert_eq!(InboxLocation::MergedRoot.directory(), None);
}

#[test]
fn an_absent_outbox_path_resolves_to_outbox_dir_itself_with_no_ancestor_trail() -> Result<(), &'static str> {
    let (directory, url_segments) = resolve_outbox_directory("outbox", None)?;
    assert_eq!(directory, "outbox");
    assert!(
        url_segments.is_empty(),
        "`url_segments` should be empty, got {url_segments:?}"
    );
    Ok(())
}

#[test]
fn an_outbox_subpath_resolves_relative_to_outbox_dir() -> Result<(), &'static str> {
    let (directory, url_segments) = resolve_outbox_directory("outbox", Some("2026-09"))?;
    assert_eq!(directory, "outbox/2026-09");
    assert_eq!(url_segments, vec!["2026-09".to_owned()]);
    Ok(())
}

#[test]
fn an_outbox_path_never_leaves_the_outbox_directory() -> Result<(), &'static str> {
    let (directory, url_segments) = resolve_outbox_directory("outbox", Some("memory/tasks"))?;
    assert_eq!(directory, "outbox/memory/tasks");
    assert_eq!(url_segments, vec!["memory".to_owned(), "tasks".to_owned()]);
    Ok(())
}

#[test]
fn a_malformed_outbox_path_is_rejected() {
    assert!(resolve_outbox_directory("outbox", Some("2026-09/../escape")).is_err());
}

#[test]
fn ancestor_rows_is_empty_at_the_root() {
    assert!(
        ancestor_rows("contract-fixture", "inbox", "Inbox", &[]).is_empty(),
        "`ancestor_rows(\"contract-fixture\", \"inbox\", \"Inbox\", &[])` should be empty, got {:?}",
        ancestor_rows("contract-fixture", "inbox", "Inbox", &[])
    );
}

#[test]
fn ancestor_rows_names_every_segment_with_only_the_last_unlinked() {
    let segments = vec!["2026-09".to_owned(), "receipts".to_owned()];
    let rows = ancestor_rows("contract-fixture", "inbox", "Inbox", &segments);
    let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
    assert_eq!(labels, vec!["Inbox", "2026-09", "receipts"]);
    assert_eq!(rows[0].href.as_deref(), Some("/contract-fixture/inbox/"));
    assert_eq!(rows[1].href.as_deref(), Some("/contract-fixture/inbox/2026-09"));
    assert_eq!(rows[2].href, None, "the current folder itself is never a link");
    let depths: Vec<usize> = rows.iter().map(|row| row.depth).collect();
    assert_eq!(depths, vec![0, 1, 2]);
}

#[test]
fn mime_type_for_name_recognises_common_extensions() {
    assert_eq!(mime_type_for_name("scan.pdf").as_deref(), Some("application/pdf"));
    assert_eq!(mime_type_for_name("photo.png").as_deref(), Some("image/png"));
    assert_eq!(mime_type_for_name("note.md").as_deref(), Some("text/markdown"));
}

#[test]
fn mime_type_for_name_returns_none_for_an_unrecognised_extension() {
    assert_eq!(mime_type_for_name("mystery.qzx"), None);
}

#[test]
fn an_ingest_path_must_name_something_inside_the_pending_inbox() {
    let config = config();
    assert_eq!(pending_item_path(&config, "inbox/a.md").as_deref(), Ok("inbox/a.md"));
    assert_eq!(
        pending_item_path(&config, " /inbox/2026-09/ ").as_deref(),
        Ok("inbox/2026-09")
    );
    for rejected in [
        "",
        "inbox",
        "processed/a.md",
        "memory/tasks/a.md",
        "inbox/../memory/a.md",
        "outbox/a.md",
    ] {
        assert!(pending_item_path(&config, rejected).is_err(), "{rejected}");
    }
}
