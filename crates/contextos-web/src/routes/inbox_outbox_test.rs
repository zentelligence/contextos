use super::*;

#[test]
fn the_unregistered_curator_error_names_the_missing_slug_and_where_to_fix_it() {
    let message = unregistered_curator_error("karla");
    assert!(message.contains("karla"));
    assert!(message.contains(".contextos/team.toml"));
}

fn item(name: &str, modified: Option<&str>) -> inbox_outbox::DirectoryItem {
    inbox_outbox::DirectoryItem {
        name: name.to_owned(),
        size: Some(2048),
        modified: modified.map(str::to_owned),
    }
}

#[test]
fn a_directory_item_becomes_a_row_with_formatted_size_and_timestamp() {
    let row = file_row(
        "contract-fixture",
        "inbox",
        ("inbox", ""),
        None,
        item("notes.md", Some("2026-09-14T10:23:45.123456789+00:00")),
    );
    assert_eq!(row.name, "notes.md");
    assert_eq!(row.size_label, "2 KB");
    assert_eq!(row.modified_label, "2026-09-14 10:23");
}

#[test]
fn an_item_with_no_size_renders_a_dash_rather_than_zero() {
    let mut no_size = item("notes.md", None);
    no_size.size = None;
    assert_eq!(
        file_row("contract-fixture", "inbox", ("inbox", ""), None, no_size).size_label,
        "-"
    );
}

#[test]
fn every_file_downloads_from_its_own_address_on_the_screen() {
    for (row, expected) in [
        (
            file_row(
                "contract-fixture",
                "inbox",
                ("inbox", ""),
                Some("pending"),
                item("a b.md", None),
            ),
            "/contract-fixture/inbox/a%20b.md?raw=1",
        ),
        (
            file_row(
                "contract-fixture",
                "inbox",
                ("processed/2026", "processed/2026"),
                Some("processed"),
                item("b.pdf", None),
            ),
            "/contract-fixture/inbox/processed/2026/b.pdf?raw=1",
        ),
        (
            file_row(
                "contract-fixture",
                "outbox",
                ("outbox/reports", "reports"),
                None,
                item("c.txt", None),
            ),
            "/contract-fixture/outbox/reports/c.txt?raw=1",
        ),
    ] {
        assert_eq!(row.download_href, expected);
    }
}

#[test]
fn only_a_pending_file_row_shows_the_ingest_action_with_its_own_full_path() {
    let pending = file_row(
        "contract-fixture",
        "inbox",
        ("inbox", ""),
        Some("pending"),
        item("a.md", None),
    );
    assert!(pending.ingest_path.is_some());
    assert_eq!(pending.ingest_path.as_deref(), Some("inbox/a.md"));

    let processed = file_row(
        "contract-fixture",
        "inbox",
        ("processed", "processed"),
        Some("processed"),
        item("b.md", None),
    );
    assert!(processed.ingest_path.is_none());

    let outbox = file_row("contract-fixture", "outbox", ("outbox", ""), None, item("c.md", None));
    assert!(outbox.ingest_path.is_none());
}

fn folder(name: &str) -> inbox_outbox::DirectoryFolder {
    inbox_outbox::DirectoryFolder { name: name.to_owned() }
}

#[test]
fn a_folder_row_navigates_into_its_own_full_path() {
    let row = folder_row(
        "contract-fixture",
        "inbox",
        "inbox",
        "",
        Some("pending"),
        folder("2026-09"),
    );
    assert_eq!(row.href, "/contract-fixture/inbox/2026-09");
}

#[test]
fn a_nested_folder_row_extends_the_browsed_url_path_rather_than_the_real_directory() {
    let row = folder_row(
        "contract-fixture",
        "inbox",
        "inbox/2026-09",
        "2026-09",
        Some("pending"),
        folder("receipts"),
    );
    assert_eq!(row.href, "/contract-fixture/inbox/2026-09/receipts");
    assert_eq!(row.ingest_path.as_deref(), Some("inbox/2026-09/receipts"));
}

#[test]
fn a_processed_folder_row_is_addressed_beneath_the_processed_directory_name() {
    let row = folder_row(
        "contract-fixture",
        "inbox",
        "processed",
        "processed",
        Some("processed"),
        folder("2026-08"),
    );
    assert_eq!(row.href, "/contract-fixture/inbox/processed/2026-08");
}

#[test]
fn an_outbox_folder_row_never_repeats_the_outbox_directory_name() {
    let row = folder_row("contract-fixture", "outbox", "outbox", "", None, folder("2026-09"));
    assert_eq!(row.href, "/contract-fixture/outbox/2026-09");
}

#[test]
fn only_a_pending_folder_row_shows_the_ingest_action_with_its_own_full_path() {
    let pending = folder_row(
        "contract-fixture",
        "inbox",
        "inbox",
        "",
        Some("pending"),
        folder("2026-09"),
    );
    assert!(pending.ingest_path.is_some());
    assert_eq!(pending.ingest_path.as_deref(), Some("inbox/2026-09"));

    let processed = folder_row(
        "contract-fixture",
        "inbox",
        "processed",
        "processed",
        Some("processed"),
        folder("2026-09"),
    );
    assert!(processed.ingest_path.is_none());

    let outbox = folder_row("contract-fixture", "outbox", "outbox", "", None, folder("2026-09"));
    assert!(outbox.ingest_path.is_none());
}

#[test]
fn compare_by_recency_orders_most_recently_modified_first() {
    let older = item("older.md", Some("2026-01-01T00:00:00+00:00"));
    let newer = item("newer.md", Some("2026-06-01T00:00:00+00:00"));
    let mut items = [older, newer];
    items.sort_by(compare_by_recency);
    let names: Vec<&str> = items.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, vec!["newer.md", "older.md"]);
}

#[test]
fn compare_by_recency_sorts_items_with_no_modified_time_last_by_name() {
    let mut items = [
        item("b-no-time.md", None),
        item("has-time.md", Some("2026-01-01T00:00:00+00:00")),
        item("a-no-time.md", None),
    ];
    items.sort_by(compare_by_recency);
    let names: Vec<&str> = items.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, vec!["has-time.md", "a-no-time.md", "b-no-time.md"]);
}

fn ingest_body(file_path: Option<&str>, folder_path: Option<&str>) -> IngestBody {
    IngestBody {
        file_path: file_path.map(str::to_owned),
        folder_path: folder_path.map(str::to_owned),
    }
}

#[test]
fn ingest_task_fields_for_a_single_file_names_it_in_title_and_content() -> Result<(), String> {
    let (title, content) = ingest_task_fields(&ingest_body(Some("inbox/2026-09/receipt.md"), None))?;
    assert_eq!(title, "Ingest receipt.md");
    assert!(content.contains("inbox/2026-09/receipt.md"));
    assert!(content.contains("[[inbox/2026-09/receipt.md|receipt.md]]"));
    Ok(())
}

#[test]
fn ingest_task_fields_for_a_whole_folder_names_it_in_title_and_content() -> Result<(), String> {
    let (title, content) = ingest_task_fields(&ingest_body(None, Some("inbox/2026-09")))?;
    assert_eq!(title, "Ingest 2026-09/");
    assert!(content.contains("every file under `inbox/2026-09/`"));
    assert!(content.contains("[[inbox/2026-09|2026-09]]"));
    Ok(())
}

#[test]
fn ingest_task_fields_rejects_neither_or_both_being_named() {
    assert!(ingest_task_fields(&ingest_body(None, None)).is_err());
    assert!(ingest_task_fields(&ingest_body(Some("inbox/a.md"), Some("inbox/sub"))).is_err());
}

#[test]
fn a_directory_item_row_carries_its_own_best_effort_type() {
    let row = file_row("contract-fixture", "inbox", ("inbox", ""), None, item("scan.pdf", None));
    assert_eq!(row.type_label, "application/pdf");
}

#[test]
fn a_directory_item_with_an_unrecognised_extension_falls_back_to_a_generic_type() {
    let row = file_row(
        "contract-fixture",
        "inbox",
        ("inbox", ""),
        None,
        item("mystery.qzx", None),
    );
    assert_eq!(row.type_label, "application/octet-stream");
}
