use super::*;

#[test]
fn parse_panel_side_accepts_left_and_right() {
    assert_eq!(parse_panel_side("left"), Ok(PanelSide::Left));
    assert_eq!(parse_panel_side("right"), Ok(PanelSide::Right));
}

#[test]
fn parse_panel_side_rejects_anything_else() {
    assert!(parse_panel_side("").is_err());
    assert!(parse_panel_side("Left").is_err());
    assert!(parse_panel_side("centre").is_err());
}
