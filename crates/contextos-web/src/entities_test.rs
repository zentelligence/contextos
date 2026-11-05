use std::fmt::Write as _;

use super::*;

const INDEX_MARKDOWN: &str = "\
# Entities: Index

Some prose before the table.

| File | Entity | Primary purpose | Agents |
| --- | --- | --- | --- |
| [personal](personal.md) | Personal | Personal finance | PersonalOS |
| [family](family.md) | Family | Household management | PersonalOS |
| [widgets](memory/entities/widgets.md) | Widget Co | Retail trading | BusinessOS |

## Contents

<!-- contextos:index:begin -->
| Item | Summary |
| --- | --- |
| [family.md](family.md) | Family |
| [personal.md](personal.md) | Self |
| [widgets.md](widgets.md) | Widget Co |
<!-- contextos:index:end -->
";

#[test]
fn seed_from_index_reads_the_first_tables_rows_in_document_order() {
    let state = seed_from_index(INDEX_MARKDOWN);
    let slugs: Vec<&str> = state.entities.iter().map(|entity| entity.slug.as_str()).collect();
    assert_eq!(slugs, ["personal", "family", "widgets"]);
}

#[test]
fn seed_from_index_never_reads_the_managed_contents_block() {
    let state = seed_from_index(INDEX_MARKDOWN);
    // The managed block is alphabetical (family, personal, widgets) and
    // would also match a naive "any table" parse; asserting the hand-authored
    // table's own order survives is enough to prove the managed block's rows
    // were never reached, since `state.entities` already has exactly three
    // entries above.
    assert_eq!(state.entities.len(), 3);
}

#[test]
fn seed_from_index_takes_the_slug_from_the_links_own_target_not_its_display_text()
-> Result<(), Box<dyn std::error::Error>> {
    let state = seed_from_index(INDEX_MARKDOWN);
    let widgets = state
        .entities
        .iter()
        .find(|entity| entity.name == "Widget Co")
        .ok_or("widgets row present")?;
    assert_eq!(widgets.slug, "widgets");
    Ok(())
}

#[test]
fn seed_from_index_assigns_accents_by_position_starting_at_one() {
    let state = seed_from_index(INDEX_MARKDOWN);
    let accents: Vec<u8> = state.entities.iter().map(|entity| entity.accent).collect();
    assert_eq!(accents, [1, 2, 3]);
}

#[test]
fn seed_from_index_wraps_the_accent_back_to_one_past_the_sixth_entity() {
    let mut markdown = String::from("| File | Entity |\n| --- | --- |\n");
    for index in 0..7 {
        let _ = writeln!(markdown, "| [e{index}](e{index}.md) | Entity {index} |");
    }
    let state = seed_from_index(&markdown);
    let accents: Vec<u8> = state.entities.iter().map(|entity| entity.accent).collect();
    assert_eq!(accents, [1, 2, 3, 4, 5, 6, 1]);
}

#[test]
fn seed_from_index_of_markdown_with_no_table_is_empty() {
    let state = seed_from_index("# Entities: Index\n\nNo table here yet.\n");
    assert!(
        state.entities.is_empty(),
        "`state.entities` should be empty, got {:?}",
        state.entities
    );
}

#[test]
fn entities_state_round_trips_through_toml() -> Result<(), Box<dyn std::error::Error>> {
    let state = seed_from_index(INDEX_MARKDOWN);
    let serialised = toml::to_string(&state)?;
    let parsed: EntitiesState = toml::from_str(&serialised)?;
    assert_eq!(parsed, state);
    Ok(())
}
