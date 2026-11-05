use super::*;

#[test]
fn an_empty_file_has_no_favourites() -> Result<(), Box<dyn std::error::Error>> {
    let state: AppsFavouritesState = toml::from_str("")?;
    assert_eq!(state, AppsFavouritesState::default());
    assert!(!state.is_favourite("task-register"));
    Ok(())
}

#[test]
fn a_favourited_slug_round_trips_through_toml() -> Result<(), Box<dyn std::error::Error>> {
    let state: AppsFavouritesState = toml::from_str(
        "[apps.task-register]\n\
         favourite = true\n",
    )?;
    assert!(state.is_favourite("task-register"));
    assert!(!state.is_favourite("some-other-app"));
    Ok(())
}

#[test]
fn an_explicit_false_entry_is_not_favourited() -> Result<(), Box<dyn std::error::Error>> {
    let state: AppsFavouritesState = toml::from_str(
        "[apps.task-register]\n\
         favourite = false\n",
    )?;
    assert!(!state.is_favourite("task-register"));
    Ok(())
}

#[test]
fn an_unknown_key_fails_to_parse_rather_than_being_silently_ignored() {
    let result: Result<AppsFavouritesState, _> = toml::from_str(
        "[apps.task-register]\n\
         favourite = true\n\
         unknown_key = 1\n",
    );
    assert!(result.is_err());
}

#[test]
fn a_favourited_state_serialises_back_to_the_documented_shape() -> Result<(), Box<dyn std::error::Error>> {
    let mut state = AppsFavouritesState::default();
    state
        .apps
        .insert("task-register".to_owned(), AppFavouriteEntry { favourite: true });
    let text = toml::to_string(&state)?;
    let round_tripped: AppsFavouritesState = toml::from_str(&text)?;
    assert_eq!(round_tripped, state);
    Ok(())
}

#[test]
fn an_empty_state_serialises_with_no_apps_table_at_all() -> Result<(), Box<dyn std::error::Error>> {
    let text = toml::to_string(&AppsFavouritesState::default())?;
    assert!(text.is_empty(), "`text` should be empty, got {text:?}");
    Ok(())
}
