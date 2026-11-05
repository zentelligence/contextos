use super::*;

fn entity(slug: &str, name: &str) -> EntityEntry {
    EntityEntry {
        slug: slug.to_owned(),
        name: name.to_owned(),
        accent: 1,
    }
}

#[test]
fn entity_by_slug_finds_a_configured_entity() {
    let entities = vec![entity("zenith", "Zenith"), entity("personal", "Personal")];
    let found = entity_by_slug(&entities, "personal");
    assert_eq!(found.map(|entity| entity.slug.as_str()), Some("personal"));
}

#[test]
fn entity_by_slug_is_none_for_an_unconfigured_slug() {
    let entities = vec![entity("zenith", "Zenith")];
    assert!(entity_by_slug(&entities, "nonexistent").is_none());
}

#[test]
fn split_entity_path_separates_the_slug_from_the_remainder() {
    assert_eq!(
        split_entity_path("zenith/finance/overview.md"),
        Some(("zenith".to_owned(), "finance/overview.md".to_owned()))
    );
}

#[test]
fn split_entity_path_accepts_a_bare_slug_as_the_entitys_own_root() {
    assert_eq!(split_entity_path("zenith"), Some(("zenith".to_owned(), String::new())));
}

#[test]
fn split_entity_path_rejects_traversal() {
    assert_eq!(split_entity_path("zenith/../secrets"), None);
}

#[test]
fn split_entity_path_is_none_for_an_empty_string() {
    assert_eq!(split_entity_path(""), None);
}

#[test]
fn dir_href_carries_only_the_entity_at_its_own_root() {
    assert_eq!(dir_href("vault", "zenith", ""), "/vault/entity/zenith");
}

#[test]
fn dir_href_carries_the_entity_and_dir_for_a_nested_folder() {
    assert_eq!(dir_href("vault", "zenith", "finance"), "/vault/entity/zenith/finance");
}

#[test]
fn file_href_carries_the_entity_and_file() {
    assert_eq!(
        file_href("vault", "zenith", "finance/overview.md"),
        "/vault/entity/zenith/finance/overview.md"
    );
}

#[test]
fn general_route_href_points_at_the_generic_vault_content_route() {
    assert_eq!(
        general_route_href("vault", "zenith", "finance/export.csv"),
        "/vault/zenith/finance/export.csv"
    );
}

#[test]
fn a_file_and_its_metadata_save_to_its_address_within_the_entity() {
    assert_eq!(
        file_href("vault", "zenith", "finance/a plan.md"),
        "/vault/entity/zenith/finance/a%20plan.md"
    );
}

#[test]
fn entity_layout_class_ignores_the_metadata_side_when_no_file_is_selected() {
    assert_eq!(entity_layout_class(true, false, false), "");
    assert_eq!(entity_layout_class(false, true, false), "notes-layout-panel-right");
}

#[test]
fn entity_layout_class_is_the_plain_base_grid_when_both_panels_share_the_left() {
    assert_eq!(entity_layout_class(true, true, true), "");
}

#[test]
fn entity_layout_class_mirrors_the_base_grid_when_both_panels_share_the_right() {
    assert_eq!(entity_layout_class(false, false, true), "notes-layout-panel-right");
}

#[test]
fn entity_layout_class_dedicates_a_right_column_to_the_metadata_panel_when_the_folder_pane_stays_left() {
    assert_eq!(entity_layout_class(true, false, true), "notes-layout-metadata-right");
}

#[test]
fn entity_layout_class_dedicates_a_left_column_to_the_metadata_panel_when_the_folder_pane_moves_right() {
    assert_eq!(
        entity_layout_class(false, true, true),
        "notes-layout-metadata-left-panel-right"
    );
}
