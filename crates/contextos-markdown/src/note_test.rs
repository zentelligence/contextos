use super::*;

#[test]
fn a_note_splits_into_ordered_frontmatter_and_the_body_after_it() -> Result<(), NoteError> {
    let note = Note::try_from("---\nzeta: 1\nalpha: [a, b]\n---\n# Title\n\nText\n")?;
    let keys: Vec<&String> = note.frontmatter().properties().keys().collect();
    assert_eq!(keys, ["zeta", "alpha"]);
    assert_eq!(note.body().text(), "# Title\n\nText\n");
    assert_eq!(note.body().first_line(), 5);
    Ok(())
}

#[test]
fn a_note_without_frontmatter_is_all_body_from_line_one() -> Result<(), NoteError> {
    let note = Note::try_from("Just text\n")?;
    assert!(note.frontmatter().properties().is_empty());
    assert_eq!(note.body().text(), "Just text\n");
    assert_eq!(note.body().first_line(), 1);
    Ok(())
}

#[test]
fn malformed_frontmatter_is_refused_not_repaired() {
    assert!(Note::try_from("---\nkey: [unclosed\n---\nbody\n").is_err());
    assert!(Note::try_from("---\nkey: value\nbody with no close\n").is_err());
}

#[test]
fn a_piece_of_a_note_is_all_body_even_when_it_opens_with_a_rule() {
    let body = Body::from("---\nAfter a rule.\n");
    assert_eq!(body.text(), "---\nAfter a rule.\n");
    assert_eq!(body.first_line(), 1);
}
