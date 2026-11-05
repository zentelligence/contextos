use crate::apps::{AppKind, AppStatus, RegisteredApp};
use crate::apps_favourites::{AppFavouriteEntry, AppsFavouritesState};

use super::{is_html_content_type, list_entry};

fn app(kind: AppKind, status: AppStatus) -> RegisteredApp {
    RegisteredApp {
        slug: "task-register".to_owned(),
        name: "Task Register Dashboard".to_owned(),
        kind,
        entry: "index.html".to_owned(),
        mcp_servers: vec!["contextos".to_owned()],
        status,
    }
}

#[test]
fn a_supported_js_entry_is_servable_embed_labelled_and_carries_no_target_attribute() {
    let entry = list_entry(
        "vault",
        &app(AppKind::Js, AppStatus::Supported),
        &AppsFavouritesState::default(),
    );
    assert!(entry.servable);
    assert_eq!(entry.kind_label, "js");
    assert_eq!(entry.kind_icon, "apps");
    assert_eq!(entry.target_attr, None);
    assert_eq!(entry.opens_label, "inline");
    assert_eq!(entry.slug, "task-register");
    assert_eq!(entry.name, "Task Register Dashboard");
    assert!(entry.favourite_toggle_html.contains("aria-pressed=\"false\""));
    assert!(entry.favourite_toggle_html.contains("/vault/apps/task-register"));
}

#[test]
fn a_supported_spa_entry_is_servable_and_opens_in_a_new_tab() {
    let entry = list_entry(
        "vault",
        &app(AppKind::Spa, AppStatus::Supported),
        &AppsFavouritesState::default(),
    );
    assert!(entry.servable);
    assert_eq!(entry.kind_label, "spa");
    assert_eq!(entry.kind_icon, "arrow-up-right");
    assert_eq!(entry.target_attr, Some("_blank"));
    assert_eq!(entry.opens_label, "new tab");
}

#[test]
fn a_not_yet_supported_htmx_entry_is_not_servable_and_labelled_htmx() {
    let entry = list_entry(
        "vault",
        &app(AppKind::Htmx, AppStatus::NotYetSupported),
        &AppsFavouritesState::default(),
    );
    assert!(!entry.servable);
    assert_eq!(entry.kind_label, "htmx");
    assert_eq!(entry.kind_icon, "apps");
    assert_eq!(entry.target_attr, None);
    assert_eq!(entry.opens_label, "inline");
}

#[test]
fn an_entry_whose_slug_is_favourited_carries_the_favourite_flag() {
    let mut favourites = AppsFavouritesState::default();
    favourites
        .apps
        .insert("task-register".to_owned(), AppFavouriteEntry { favourite: true });
    let entry = list_entry("vault", &app(AppKind::Js, AppStatus::Supported), &favourites);
    assert!(entry.favourite_toggle_html.contains("aria-pressed=\"true\""));
}

#[test]
fn plain_text_html_is_recognised_as_html() {
    assert!(is_html_content_type("text/html"));
}

#[test]
fn text_html_with_a_charset_parameter_is_still_recognised_as_html() {
    assert!(is_html_content_type("text/html; charset=utf-8"));
}

#[test]
fn html_recognition_is_case_insensitive() {
    assert!(is_html_content_type("TEXT/HTML"));
    assert!(is_html_content_type("Text/Html; charset=UTF-8"));
}

#[test]
fn a_non_html_content_type_is_not_recognised_as_html() {
    assert!(!is_html_content_type("application/json"));
    assert!(!is_html_content_type("text/javascript"));
    // Not a prefix match: a real-world type that merely starts with
    // "text/html" but is not it must not be misclassified.
    assert!(!is_html_content_type("text/html+something"));
}
