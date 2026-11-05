use axum::http::{HeaderMap, HeaderValue, header};

use super::*;

fn entry(label: &str, href: &str) -> RecentEntry {
    RecentEntry {
        label: label.to_owned(),
        href: href.to_owned(),
    }
}

#[test]
fn a_missing_cookie_header_reads_as_an_empty_list() {
    assert_eq!(read_from_headers(&HeaderMap::new()), Vec::new());
}

#[test]
fn a_cookie_with_no_matching_entry_reads_as_an_empty_list() {
    let mut headers = HeaderMap::new();
    headers.insert(header::COOKIE, HeaderValue::from_static("cc_sidebar=narrow"));
    assert_eq!(read_from_headers(&headers), Vec::new());
}

#[test]
fn a_written_cookie_round_trips_through_reading_it_back() -> Result<(), Box<dyn std::error::Error>> {
    let entries = vec![entry("Tasks", "/vault/tasks/"), entry("Dashboard", "/vault/")];
    let cookie = set_cookie_header(&entries).ok_or("cookie header could not be built")?;
    let cookie_str = cookie.to_str()?;
    let value_only = cookie_str.split(';').next().ok_or("cookie header had no value")?;
    let mut headers = HeaderMap::new();
    headers.insert(header::COOKIE, HeaderValue::from_str(value_only)?);
    assert_eq!(read_from_headers(&headers), entries);
    Ok(())
}

#[test]
fn a_malformed_cookie_value_reads_as_an_empty_list_rather_than_failing() {
    let mut headers = HeaderMap::new();
    headers.insert(header::COOKIE, HeaderValue::from_static("cc_recent=not-json-at-all"));
    assert_eq!(read_from_headers(&headers), Vec::new());
}

#[test]
fn with_visit_puts_the_new_entry_first() {
    let current = vec![entry("Tasks", "/vault/tasks/")];
    let next = with_visit(current, entry("Journal", "/vault/journal/"));
    assert_eq!(
        next,
        vec![entry("Journal", "/vault/journal/"), entry("Tasks", "/vault/tasks/")]
    );
}

#[test]
fn with_visit_moves_an_existing_entry_to_the_front_rather_than_duplicating_it() {
    let current = vec![entry("Journal", "/vault/journal/"), entry("Tasks", "/vault/tasks/")];
    let next = with_visit(current, entry("Tasks", "/vault/tasks/"));
    assert_eq!(
        next,
        vec![entry("Tasks", "/vault/tasks/"), entry("Journal", "/vault/journal/")]
    );
}

#[test]
fn static_shell_and_mcp_paths_are_not_trackable() {
    assert!(!is_trackable_path("/static/contextos-web.css"));
    assert!(!is_trackable_path("/shell/sidebar-toggle"));
    assert!(!is_trackable_path("/mcp/contextos/fs_read_text_file"));
    assert!(
        !is_trackable_path("/myvault/events/stream"),
        "the live stream is not a page someone visited"
    );
    assert!(!is_trackable_path("/"));
}

#[test]
fn an_ordinary_vault_page_is_trackable() {
    assert!(is_trackable_path("/myvault/tasks/"));
    assert!(is_trackable_path("/settings/"));
}

#[test]
fn a_bare_vault_root_is_labelled_dashboard() {
    assert_eq!(label_for_path("/myvault/"), "Dashboard");
    assert_eq!(label_for_path("/myvault"), "Dashboard");
}

#[test]
fn the_vault_independent_settings_page_is_labelled_settings() {
    assert_eq!(label_for_path("/settings/"), "Settings");
}

#[test]
fn a_hyphenated_final_segment_is_humanised_into_title_case_words() {
    assert_eq!(label_for_path("/myvault/wiki/"), "Wiki");
    assert_eq!(label_for_path("/myvault/inbox/"), "Inbox");
}

#[test]
fn with_visit_caps_at_six_entries_dropping_the_oldest() {
    let current: Vec<RecentEntry> = (0..6)
        .map(|i| entry(&format!("Page {i}"), &format!("/vault/{i}/")))
        .collect();
    let next = with_visit(current, entry("New", "/vault/new/"));
    assert_eq!(next.len(), 6);
    assert_eq!(next[0], entry("New", "/vault/new/"));
    assert!(
        !next.iter().any(|e| e.href == "/vault/5/"),
        "the oldest entry must be dropped"
    );
}
