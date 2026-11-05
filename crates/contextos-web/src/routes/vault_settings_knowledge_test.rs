use super::*;

#[test]
fn parse_panel_side_accepts_left_and_right() {
    assert_eq!(parse_panel_side("left", "tree_side"), Ok(PanelSide::Left));
    assert_eq!(parse_panel_side("right", "tree_side"), Ok(PanelSide::Right));
}

#[test]
fn parse_panel_side_rejects_anything_else() {
    assert!(parse_panel_side("", "tree_side").is_err());
    assert!(parse_panel_side("Left", "tree_side").is_err());
    assert!(parse_panel_side("centre", "tree_side").is_err());
}

#[test]
fn parse_panel_side_names_the_field_it_was_actually_asked_to_validate() {
    let result = parse_panel_side("centre", "shard_side");
    assert!(
        matches!(&result, Err(error) if error.contains("shard_side")),
        "{result:?}"
    );
    let result = parse_panel_side("centre", "metadata_panel_side");
    assert!(
        matches!(&result, Err(error) if error.contains("metadata_panel_side")),
        "{result:?}"
    );
}
