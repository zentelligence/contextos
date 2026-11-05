use super::*;

#[test]
fn every_configured_view_has_a_unique_id() {
    let mut ids: Vec<&str> = VIEWS.iter().map(|view| view.id).collect();
    let original_len = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), original_len, "every view id must be unique");
}

#[test]
fn every_configured_view_has_a_non_empty_label() {
    assert!(VIEWS.iter().all(|view| !view.label.is_empty()));
}

#[test]
fn an_empty_state_has_no_favourites() -> Result<(), Box<dyn std::error::Error>> {
    let state: ViewsState = toml::from_str("")?;
    assert!(!state.is_favourite("tasks"));
    Ok(())
}

#[test]
fn a_favourited_id_round_trips_through_toml() -> Result<(), Box<dyn std::error::Error>> {
    let state: ViewsState = toml::from_str("[views.tasks]\nfavourite = true\n")?;
    assert!(state.is_favourite("tasks"));
    assert!(!state.is_favourite("agents"));
    Ok(())
}

#[test]
fn an_explicit_false_entry_is_not_favourited() -> Result<(), Box<dyn std::error::Error>> {
    let state: ViewsState = toml::from_str("[views.tasks]\nfavourite = false\n")?;
    assert!(!state.is_favourite("tasks"));
    Ok(())
}

#[test]
fn an_unknown_key_fails_to_parse_rather_than_being_silently_ignored() {
    let result: Result<ViewsState, _> = toml::from_str("[views.tasks]\nfavourite = true\nunknown_key = 1\n");
    assert!(result.is_err());
}

#[test]
fn a_favourited_state_serialises_back_to_the_documented_shape() -> Result<(), Box<dyn std::error::Error>> {
    let mut state = ViewsState::default();
    state.views.insert("tasks".to_owned(), Favourite { favourite: true });
    let text = toml::to_string(&state)?;
    let round_tripped: ViewsState = toml::from_str(&text)?;
    assert!(round_tripped.is_favourite("tasks"));
    Ok(())
}

#[test]
fn every_configured_view_has_a_non_empty_icon() {
    assert!(VIEWS.iter().all(|view| !view.icon.is_empty()));
}

#[test]
fn a_directory_path_resolves_to_its_view_with_or_without_surrounding_slashes() {
    for path in ["tasks", "tasks/", "/tasks/"] {
        assert_eq!(for_path(path).map(|view| view.id), Some("tasks"), "{path}");
    }
    // Signals has no screen yet, so there is nothing to favourite.
    assert_eq!(for_path("memory/events/").map(|view| view.id), None);
    assert_eq!(for_path("wiki").map(|view| view.id), Some("wiki"));
}

#[test]
fn the_vault_root_resolves_to_the_dashboard() {
    assert_eq!(for_path("").map(|view| view.id), Some("dashboard"));
    assert_eq!(for_path("/").map(|view| view.id), Some("dashboard"));
}

#[test]
fn a_path_below_a_view_or_outside_every_view_resolves_to_nothing() {
    for path in [
        "tasks/TASK-1",
        "memory/events/2026.md",
        "settings",
        "Tasks",
        "agents/?team=business",
    ] {
        assert!(for_path(path).is_none(), "{path}");
    }
}

#[test]
fn a_page_beneath_a_view_that_covers_its_subpages_resolves_to_that_view() {
    assert_eq!(for_path("wiki/some-note").map(|view| view.id), Some("wiki"));
    assert_eq!(for_path("wiki/a/b/c.md").map(|view| view.id), Some("wiki"));
    assert_eq!(for_path("insights/ab/note").map(|view| view.id), Some("insights"));
}

#[test]
fn a_sibling_directory_sharing_a_covering_views_name_prefix_is_not_covered() {
    for path in ["wikipedia", "wiki-archive/note", "insightsx/a"] {
        assert!(for_path(path).is_none(), "{path}");
    }
}

#[test]
fn an_agents_selection_with_a_single_named_filter_resolves_to_that_filter_view() {
    assert_eq!(agents_view(None, None).id, "agents");
    assert_eq!(agents_view(Some("general"), None).id, "agents-general");
    assert_eq!(agents_view(Some("business"), None).id, "agents-business");
    assert_eq!(agents_view(Some("personal"), None).id, "agents-personal");
    assert_eq!(agents_view(None, Some("blocked")).id, "agents-blocked");
}

#[test]
fn an_agents_team_filter_matches_case_insensitively_as_the_list_itself_does() {
    assert_eq!(agents_view(Some("General"), None).id, "agents-general");
    assert_eq!(agents_view(Some("Business"), None).id, "agents-business");
    assert_eq!(agents_view(Some("PERSONAL"), None).id, "agents-personal");
}

#[test]
fn an_agents_selection_matching_no_named_filter_view_resolves_to_the_plain_agents_view() {
    assert_eq!(agents_view(Some("business"), Some("blocked")).id, "agents");
    assert_eq!(agents_view(None, Some("working")).id, "agents");
    assert_eq!(agents_view(Some("other"), None).id, "agents");
}

#[test]
fn a_favourite_file_round_trips_by_its_vault_path_beside_the_views() -> Result<(), Box<dyn std::error::Error>> {
    let state: ViewsState = toml::from_str(
        "[views.tasks]\nfavourite = true\n\n[pages.\"memory/wiki/a b.md\"]\nfavourite = true\n\n[pages.\"notes/x.md\"]\nfavourite = false\n",
    )?;
    assert!(state.is_favourite("tasks"));
    assert!(state.is_page_favourite("memory/wiki/a b.md"));
    assert!(!state.is_page_favourite("notes/x.md"));
    assert!(!state.is_page_favourite("memory/wiki/other.md"));
    assert_eq!(state.favourite_pages().collect::<Vec<_>>(), ["memory/wiki/a b.md"]);
    let round_tripped: ViewsState = toml::from_str(&toml::to_string(&state)?)?;
    assert!(round_tripped.is_page_favourite("memory/wiki/a b.md"));
    Ok(())
}

#[test]
fn a_state_written_before_favourite_files_existed_still_reads() -> Result<(), Box<dyn std::error::Error>> {
    let state: ViewsState = toml::from_str("[views.wiki]\nfavourite = true\n")?;
    assert!(state.is_favourite("wiki"));
    assert_eq!(state.favourite_pages().count(), 0);
    Ok(())
}
