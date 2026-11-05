use crate::doctor::DoctorStatus;
use crate::doctor::fixture::check_over as doctor_check_over;

const DECISIONS_BASE: &str = "filters: file.inFolder(\"decisions\")\nviews:\n  - type: table\n    name: All\n";

fn check_over(files: &[(&str, &str)]) -> Result<crate::doctor::DoctorCheck, Box<dyn std::error::Error>> {
    doctor_check_over("Duplicate id detection", files)
}

fn record(id: &str) -> String {
    format!("---\nid: {id}\n---\nBody\n")
}

#[test]
fn the_same_id_in_two_different_bases_is_not_a_duplicate() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        (
            "alpha/decisions.base",
            DECISIONS_BASE.replace("\"decisions\"", "\"alpha/decisions\"").as_str(),
        ),
        (
            "beta/decisions.base",
            DECISIONS_BASE.replace("\"decisions\"", "\"beta/decisions\"").as_str(),
        ),
        ("alpha/decisions/item-1.md", &record("ITEM-1")),
        ("beta/decisions/item-1.md", &record("ITEM-1")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    Ok(())
}

#[test]
fn the_same_id_twice_within_one_bases_scope_is_reported_with_the_base() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        ("decisions.base", DECISIONS_BASE),
        ("decisions/item-1.md", &record("ITEM-1")),
        ("decisions/nested/copy.md", &record("ITEM-1")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Fail);
    assert!(check.message.contains("decisions.base"), "{}", check.message);
    assert!(check.message.contains("ITEM-1"), "{}", check.message);
    assert!(check.message.contains("decisions/item-1.md"), "{}", check.message);
    assert!(check.message.contains("decisions/nested/copy.md"), "{}", check.message);
    assert!(!check.auto_fixable);
    Ok(())
}

#[test]
fn ids_shared_by_notes_that_no_base_covers_are_not_duplicates() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        ("decisions.base", DECISIONS_BASE),
        ("elsewhere/one.md", &record("ITEM-1")),
        ("other/two.md", &record("ITEM-1")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    Ok(())
}

#[test]
fn templates_never_count_towards_a_duplicate() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        ("everything.base", "views:\n  - type: table\n    name: All\n"),
        ("templates/one.md", &record("<key>-<int>")),
        ("templates/two.md", &record("<key>-<int>")),
        ("real.md", &record("ITEM-2")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    Ok(())
}

#[test]
fn a_base_with_no_folder_scope_still_detects_vault_wide_duplicates() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        ("everything.base", "views:\n  - type: table\n    name: All\n"),
        ("one/a.md", &record("ITEM-3")),
        ("two/b.md", &record("ITEM-3")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Fail);
    assert!(check.message.contains("one/a.md"), "{}", check.message);
    Ok(())
}

#[test]
fn a_base_that_cannot_be_read_is_reported_rather_than_passed_and_invents_no_duplicates()
-> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        ("broken.base", "filters: [unterminated\n"),
        ("one/a.md", &record("ITEM-3")),
        ("two/b.md", &record("ITEM-3")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Fail, "{}", check.message);
    assert!(check.message.contains("broken.base"), "{}", check.message);
    assert!(!check.message.contains("ITEM-3"), "{}", check.message);
    Ok(())
}

#[test]
fn a_base_whose_filters_cannot_be_evaluated_is_reported_with_the_reason() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        ("odd.base", "filters: formula.score > 3\n"),
        ("one/a.md", &record("ITEM-4")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Fail, "{}", check.message);
    assert!(check.message.contains("odd.base"), "{}", check.message);
    Ok(())
}

#[test]
fn an_id_held_by_a_record_its_filters_hide_is_still_a_duplicate_within_the_folder()
-> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        (
            "decisions.base",
            "filters: file.inFolder(\"decisions\") && status != \"closed\"\n",
        ),
        ("decisions/open.md", "---\nid: ITEM-5\nstatus: open\n---\n"),
        ("decisions/closed.md", "---\nid: ITEM-5\nstatus: closed\n---\n"),
    ])?;

    assert_eq!(check.status, DoctorStatus::Fail, "{}", check.message);
    assert!(check.message.contains("decisions/closed.md"), "{}", check.message);
    Ok(())
}

#[test]
fn a_base_scoped_by_folder_equality_compares_only_the_notes_it_selects() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        (
            "memory/decisions.base",
            "filters: file.folder == \"memory/decisions\"\n",
        ),
        (
            "work/alpha/decisions.base",
            "filters: file.inFolder(\"work/alpha/decisions\")\n",
        ),
        (
            "work/beta/decisions.base",
            "filters: file.inFolder(\"work/beta/decisions\")\n",
        ),
        ("memory/decisions/vd-1.md", &record("VD-1")),
        ("work/alpha/decisions/d-001.md", &record("D-001")),
        ("work/beta/decisions/d-001.md", &record("D-001")),
        ("memory/tasks/d-001.md", &record("D-001")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    Ok(())
}

#[test]
fn a_base_scoped_by_folder_equality_still_reports_its_own_duplicates() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        (
            "memory/decisions.base",
            "filters: file.folder == \"memory/decisions\"\n",
        ),
        ("memory/decisions/one.md", &record("VD-1")),
        ("memory/decisions/two.md", &record("VD-1")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Fail);
    assert!(check.message.contains("memory/decisions.base"), "{}", check.message);
    assert!(check.message.contains("memory/decisions/two.md"), "{}", check.message);
    Ok(())
}

#[test]
fn a_base_scoped_by_properties_compares_only_the_records_its_filters_select() -> Result<(), Box<dyn std::error::Error>>
{
    let base = "filters:\n  and:\n    - type == \"task\"\n    - project == \"alpha\"\n";
    let task = |project: &str| format!("---\nid: T-1\ntype: task\nproject: {project}\n---\n");
    let selected_once = check_over(&[
        ("work/alpha/tasks/tasks.base", base),
        ("work/alpha/tasks/t-1.md", &task("alpha")),
        ("work/beta/tasks/t-1.md", &task("beta")),
    ])?;
    let selected_twice = check_over(&[
        ("work/alpha/tasks/tasks.base", base),
        ("work/alpha/tasks/t-1.md", &task("alpha")),
        ("archive/t-1.md", &task("alpha")),
    ])?;

    assert_eq!(selected_once.status, DoctorStatus::Pass, "{}", selected_once.message);
    assert_eq!(selected_twice.status, DoctorStatus::Fail);
    assert!(
        selected_twice.message.contains("archive/t-1.md"),
        "{}",
        selected_twice.message
    );
    Ok(())
}

#[test]
fn an_index_note_a_base_excludes_by_basename_is_not_one_of_its_records() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        (
            "decisions.base",
            "filters: file.inFolder(\"decisions\") && file.basename != \"index\"\n",
        ),
        ("decisions/index.md", &record("ITEM-1")),
        ("decisions/item-1.md", &record("ITEM-1")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    Ok(())
}
