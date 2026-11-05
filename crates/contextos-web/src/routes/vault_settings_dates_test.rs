use super::*;

#[test]
fn parse_date_structure_side_accepts_left_and_right() {
    assert_eq!(parse_date_structure_side("left"), Ok(DateStructureSide::Left));
    assert_eq!(parse_date_structure_side("right"), Ok(DateStructureSide::Right));
}

#[test]
fn parse_date_structure_side_rejects_anything_else() {
    assert!(parse_date_structure_side("").is_err());
    assert!(parse_date_structure_side("Left").is_err());
    assert!(parse_date_structure_side("centre").is_err());
}
