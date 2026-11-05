use axum::http::{HeaderMap, HeaderValue, header};

use super::*;

fn headers_with_cookie(value: &str) -> Result<HeaderMap, Box<dyn std::error::Error>> {
    let mut headers = HeaderMap::new();
    headers.insert(header::COOKIE, HeaderValue::from_str(value)?);
    Ok(headers)
}

#[test]
fn a_missing_cookie_header_reads_as_empty_state() {
    let state = read_from_headers(&HeaderMap::new());
    assert_eq!(wiki_last_visited(&state, "myvault"), None);
}

#[test]
fn a_malformed_cookie_value_reads_as_empty_state_rather_than_failing() -> Result<(), Box<dyn std::error::Error>> {
    let state = read_from_headers(&headers_with_cookie("cc_last_visited=not-json-at-all")?);
    assert_eq!(wiki_last_visited(&state, "myvault"), None);
    Ok(())
}

#[test]
fn a_written_cookie_round_trips_through_reading_it_back() -> Result<(), Box<dyn std::error::Error>> {
    let state = with_visit(
        LastVisitedState::default(),
        "myvault",
        VisitedScreen::Wiki,
        "domain/topic/page.md".to_owned(),
    );
    let cookie = set_cookie_header(&state).ok_or("cookie header could not be built")?;
    let cookie_str = cookie.to_str()?;
    let value_only = cookie_str.split(';').next().ok_or("cookie header had no value")?;
    let read_back = read_from_headers(&headers_with_cookie(value_only)?);
    assert_eq!(wiki_last_visited(&read_back, "myvault"), Some("domain/topic/page.md"));
    Ok(())
}

#[test]
fn each_screen_and_vault_tracks_its_own_last_visited_path_independently() {
    let state = LastVisitedState::default();
    let state = with_visit(state, "vault-a", VisitedScreen::Wiki, "domain/wiki-page.md".to_owned());
    let state = with_visit(state, "vault-a", VisitedScreen::Insights, "shard/note.md".to_owned());
    let state = with_visit(
        state,
        "vault-a",
        VisitedScreen::Entity("acme-corp".to_owned()),
        "policies/onboarding.md".to_owned(),
    );
    let state = with_visit(state, "vault-b", VisitedScreen::Wiki, "other/page.md".to_owned());

    assert_eq!(wiki_last_visited(&state, "vault-a"), Some("domain/wiki-page.md"));
    assert_eq!(insights_last_visited(&state, "vault-a"), Some("shard/note.md"));
    assert_eq!(
        entity_last_visited(&state, "vault-a", "acme-corp"),
        Some("policies/onboarding.md")
    );
    assert_eq!(wiki_last_visited(&state, "vault-b"), Some("other/page.md"));
    assert_eq!(insights_last_visited(&state, "vault-b"), None);
    assert_eq!(entity_last_visited(&state, "vault-a", "some-other-entity"), None);
}

#[test]
fn a_later_visit_to_the_same_screen_replaces_the_earlier_one() {
    let state = LastVisitedState::default();
    let state = with_visit(state, "vault-a", VisitedScreen::Wiki, "domain/first.md".to_owned());
    let state = with_visit(state, "vault-a", VisitedScreen::Wiki, "domain/second.md".to_owned());
    assert_eq!(wiki_last_visited(&state, "vault-a"), Some("domain/second.md"));
}

#[test]
fn different_entities_within_the_same_vault_track_independently() {
    let state = LastVisitedState::default();
    let state = with_visit(
        state,
        "vault-a",
        VisitedScreen::Entity("acme-corp".to_owned()),
        "policies/one.md".to_owned(),
    );
    let state = with_visit(
        state,
        "vault-a",
        VisitedScreen::Entity("globex".to_owned()),
        "contracts/two.md".to_owned(),
    );
    assert_eq!(
        entity_last_visited(&state, "vault-a", "acme-corp"),
        Some("policies/one.md")
    );
    assert_eq!(
        entity_last_visited(&state, "vault-a", "globex"),
        Some("contracts/two.md")
    );
}

// ---------------------------------------------------------------------
// classify: deriving a visit from a raw request path
// ---------------------------------------------------------------------

#[test]
fn a_wiki_page_path_classifies_as_a_wiki_visit() -> Result<(), &'static str> {
    let visit = classify("/myvault/wiki/domain/topic/page.md").ok_or("should classify")?;
    assert_eq!(visit.vault, "myvault");
    assert!(matches!(visit.screen, VisitedScreen::Wiki));
    assert_eq!(visit.relative_path, "domain/topic/page.md");
    Ok(())
}

#[test]
fn a_wiki_edit_path_classifies_as_a_visit_to_the_same_page() -> Result<(), &'static str> {
    let visit = classify("/myvault/wiki/domain/topic/page.md/edit").ok_or("should classify")?;
    assert_eq!(visit.relative_path, "domain/topic/page.md");
    Ok(())
}

#[test]
fn a_wiki_folder_browse_path_does_not_classify_as_a_visit() {
    assert!(classify("/myvault/wiki/domain/topic").is_none());
    assert!(classify("/myvault/wiki/").is_none());
}

#[test]
fn a_wiki_new_page_form_path_does_not_classify_as_a_visit() {
    assert!(classify("/myvault/wiki/domain/topic/new").is_none());
}

#[test]
fn a_wiki_confirm_delete_path_does_not_classify_as_a_visit() {
    assert!(classify("/myvault/wiki/domain/topic/page.md/confirm-delete").is_none());
}

#[test]
fn an_insights_note_path_classifies_as_an_insights_visit() -> Result<(), &'static str> {
    let visit = classify("/myvault/insights/a/some-note.md").ok_or("should classify")?;
    assert_eq!(visit.vault, "myvault");
    assert!(matches!(visit.screen, VisitedScreen::Insights));
    assert_eq!(visit.relative_path, "a/some-note.md");
    Ok(())
}

#[test]
fn an_entity_file_path_classifies_as_an_entity_visit_with_its_slug() -> Result<(), &'static str> {
    let visit = classify("/myvault/entity/acme-corp/policies/onboarding.md").ok_or("should classify")?;
    assert_eq!(visit.vault, "myvault");
    let VisitedScreen::Entity(slug) = visit.screen else {
        return Err("expected an entity visit");
    };
    assert_eq!(slug, "acme-corp");
    assert_eq!(visit.relative_path, "policies/onboarding.md");
    Ok(())
}

#[test]
fn an_entity_root_path_with_no_file_does_not_classify_as_a_visit() {
    assert!(classify("/myvault/entity/acme-corp").is_none());
}

#[test]
fn a_percent_encoded_segment_is_decoded_in_the_classified_visit() -> Result<(), &'static str> {
    let visit = classify("/myvault/wiki/domain/Meeting%20Notes.md").ok_or("should classify")?;
    assert_eq!(visit.relative_path, "domain/Meeting Notes.md");
    Ok(())
}

#[test]
fn an_unrelated_path_does_not_classify_as_a_visit() {
    assert!(classify("/myvault/tasks/").is_none());
    assert!(classify("/settings/").is_none());
}
