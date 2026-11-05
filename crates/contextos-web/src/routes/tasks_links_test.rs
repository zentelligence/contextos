use serde_json::{Map, Value, json};

use super::*;
use crate::routes::tasks::TaskRow;

type BoxError = Box<dyn std::error::Error>;

fn frontmatter(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

fn row(id: &str, title: &str) -> TaskRow {
    TaskRow {
        id: id.to_owned(),
        title: title.to_owned(),
        ..TaskRow::default()
    }
}

fn rows_by_id(rows: &[TaskRow]) -> HashMap<String, &TaskRow> {
    rows.iter().map(|row| (row.id.to_ascii_lowercase(), row)).collect()
}

#[test]
fn a_single_valued_wikilink_resolves_to_a_chip_carrying_the_matching_rows_own_title() -> Result<(), BoxError> {
    let rows = [row("T-060", "Ship the widget")];
    let fields = build_link_fields(
        &frontmatter(&[("parent", json!("[[t-060|T-060]]"))]),
        &rows_by_id(&rows),
    );
    let parent = fields
        .iter()
        .find(|f| f.key == "parent")
        .ok_or("parent field present")?;
    assert!(parent.single);
    assert_eq!(parent.chips.len(), 1);
    assert_eq!(parent.chips[0].target, "t-060");
    assert_eq!(parent.chips[0].display_id, "T-060");
    assert_eq!(parent.chips[0].title.as_deref(), Some("Ship the widget"));
    Ok(())
}

#[test]
fn a_list_valued_wikilink_field_resolves_every_entry_independently() -> Result<(), BoxError> {
    let rows = [row("T-060", "Ship the widget"), row("T-061", "Renew the licence")];
    let related = json!(["[[t-060|T-060]]", "[[t-061|T-061]]"]);
    let fields = build_link_fields(&frontmatter(&[("related", related)]), &rows_by_id(&rows));
    let related = fields
        .iter()
        .find(|f| f.key == "related")
        .ok_or("related field present")?;
    assert!(!related.single);
    assert_eq!(related.chips.len(), 2);
    assert_eq!(related.chips[0].display_id, "T-060");
    assert_eq!(related.chips[1].display_id, "T-061");
    Ok(())
}

#[test]
fn a_wikilink_target_with_no_matching_row_still_renders_a_chip_with_no_title() -> Result<(), BoxError> {
    let rows: [TaskRow; 0] = [];
    let fields = build_link_fields(
        &frontmatter(&[("parent", json!("[[t-999|T-999]]"))]),
        &rows_by_id(&rows),
    );
    let parent = fields
        .iter()
        .find(|f| f.key == "parent")
        .ok_or("parent field present")?;
    assert_eq!(
        parent.chips.len(),
        1,
        "a dangling reference must still render as a chip"
    );
    assert_eq!(parent.chips[0].target, "t-999");
    assert_eq!(parent.chips[0].display_id, "T-999");
    assert!(parent.chips[0].title.is_none());
    Ok(())
}

#[test]
fn an_absent_link_field_renders_with_no_chips_rather_than_erroring() {
    let rows: [TaskRow; 0] = [];
    let fields = build_link_fields(&frontmatter(&[]), &rows_by_id(&rows));
    for field in &fields {
        assert!(field.chips.is_empty(), "field {} should have no chips", field.key);
    }
}

#[test]
fn a_null_link_field_renders_with_no_chips() -> Result<(), BoxError> {
    let rows: [TaskRow; 0] = [];
    let fields = build_link_fields(&frontmatter(&[("parent", Value::Null)]), &rows_by_id(&rows));
    let parent = fields
        .iter()
        .find(|f| f.key == "parent")
        .ok_or("parent field present")?;
    assert!(parent.chips.is_empty());
    Ok(())
}

#[test]
fn every_documented_link_field_is_exposed_for_the_generic_field_builders_own_exclusion() {
    let keys: Vec<&str> = link_field_keys().collect();
    assert_eq!(keys, ["parent", "depends_on", "blocks", "related"]);
}

#[test]
fn resolution_is_case_insensitive_against_a_rows_own_id() -> Result<(), BoxError> {
    let rows = [row("T-060", "Ship the widget")];
    // A note might spell its own wikilink target in a different case than
    // the referenced row's own `id` frontmatter value; matching must not
    // depend on the two agreeing on case.
    let fields = build_link_fields(
        &frontmatter(&[("parent", json!("[[T-060|T-060]]"))]),
        &rows_by_id(&rows),
    );
    let parent = fields
        .iter()
        .find(|f| f.key == "parent")
        .ok_or("parent field present")?;
    assert_eq!(parent.chips[0].title.as_deref(), Some("Ship the widget"));
    Ok(())
}

fn row_at(path: &str, id: &str, title: &str) -> TaskRow {
    TaskRow {
        path: path.to_owned(),
        ..row(id, title)
    }
}

#[test]
fn a_link_written_as_a_path_within_the_tasks_base_resolves_to_the_row_and_round_trips_as_its_id() -> Result<(), BoxError>
{
    let rows = [row_at("memory/tasks/zenith/mcp/T-060.md", "T-060", "Ship the widget")];
    let fields = build_link_fields(
        &frontmatter(&[("parent", json!("[[zenith/mcp/t-060]]"))]),
        &rows_by_id(&rows),
    );
    let parent = fields
        .iter()
        .find(|f| f.key == "parent")
        .ok_or("parent field present")?;
    assert_eq!(parent.chips[0].target, "t-060");
    assert_eq!(parent.chips[0].display_id, "T-060");
    assert_eq!(parent.chips[0].title.as_deref(), Some("Ship the widget"));
    Ok(())
}

#[test]
fn a_link_written_as_a_full_vault_path_resolves_to_the_row() -> Result<(), BoxError> {
    let rows = [row_at("memory/tasks/zenith/mcp/T-060.md", "T-060", "Ship the widget")];
    let fields = build_link_fields(
        &frontmatter(&[("related", json!(["[[memory/tasks/zenith/mcp/T-060|T-060]]"]))]),
        &rows_by_id(&rows),
    );
    let related = fields
        .iter()
        .find(|f| f.key == "related")
        .ok_or("related field present")?;
    assert_eq!(related.chips[0].target, "t-060");
    assert_eq!(related.chips[0].title.as_deref(), Some("Ship the widget"));
    Ok(())
}

#[test]
fn a_path_link_to_no_known_row_stays_a_dangling_chip() -> Result<(), BoxError> {
    let rows = [row_at("memory/tasks/a/T-060.md", "T-060", "Ship the widget")];
    let fields = build_link_fields(
        &frontmatter(&[("parent", json!("[[elsewhere/t-999]]"))]),
        &rows_by_id(&rows),
    );
    let parent = fields
        .iter()
        .find(|f| f.key == "parent")
        .ok_or("parent field present")?;
    assert_eq!(parent.chips[0].title, None);
    assert_eq!(parent.chips[0].target, "elsewhere/t-999");
    Ok(())
}
