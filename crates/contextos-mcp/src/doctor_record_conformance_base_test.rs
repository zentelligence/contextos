use crate::doctor::DoctorStatus;
use crate::doctor::fixture::check_over as doctor_check_over;

const TASKS_BASE: &str = "filters: file.inFolder(\"tasks\")\nviews:\n  - type: table\n    name: All\n";

/// The `Record conformance` check the doctor reports over a vault holding `files`.
fn check_over(files: &[(&str, &str)]) -> Result<crate::doctor::DoctorCheck, Box<dyn std::error::Error>> {
    doctor_check_over("Record conformance", files)
}

fn record(base: &str) -> String {
    format!("---\nid: T-1\nbase: '{base}'\n---\nBody\n")
}

#[test]
fn a_note_naming_a_base_that_contains_it_passes() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        ("tasks.base", TASKS_BASE),
        ("tasks/deep/one.md", &record("[[tasks.base]]")),
        ("plain.md", "---\ntitle: No base\n---\n"),
    ])?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    Ok(())
}

#[test]
fn a_note_outside_the_scope_of_the_base_it_names_is_reported_and_not_auto_fixable()
-> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        ("tasks.base", TASKS_BASE),
        ("elsewhere/moved.md", &record("[[tasks.base]]")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Fail);
    assert!(check.message.contains("elsewhere/moved.md"), "{}", check.message);
    assert!(check.message.contains("outside"), "{}", check.message);
    assert!(!check.auto_fixable);
    Ok(())
}

#[test]
fn a_note_naming_a_base_that_does_not_exist_is_reported() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[("tasks/one.md", &record("[[gone.base]]"))])?;

    assert_eq!(check.status, DoctorStatus::Fail);
    assert!(check.message.contains("tasks/one.md"), "{}", check.message);
    assert!(check.message.contains("gone.base"), "{}", check.message);
    Ok(())
}

#[test]
fn a_value_that_is_not_a_link_to_a_base_file_is_reported() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        ("tasks.base", TASKS_BASE),
        ("tasks/text.md", &record("tasks.base")),
        ("tasks/note.md", &record("[[tasks]]")),
        ("tasks/number.md", "---\nbase: 3\n---\n"),
    ])?;

    assert_eq!(check.status, DoctorStatus::Fail);
    for name in ["tasks/text.md", "tasks/note.md", "tasks/number.md"] {
        assert!(check.message.contains(name), "{name}: {}", check.message);
    }
    Ok(())
}

#[test]
fn a_base_with_no_folder_scope_contains_every_note() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        ("everything.base", "views:\n  - type: table\n    name: All\n"),
        ("anywhere/one.md", &record("[[everything.base]]")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    Ok(())
}

#[test]
fn a_bare_base_name_resolves_to_the_one_base_file_of_that_name() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        (
            "projects/alpha/tasks.base",
            "filters: file.inFolder(\"projects/alpha\")\n",
        ),
        ("projects/alpha/one.md", &record("[[tasks.base|Tasks]]")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    Ok(())
}

#[test]
fn a_bare_base_name_resolves_to_the_nearest_base_of_that_name() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        ("memory/tasks.base", "filters: file.inFolder(\"memory\")\n"),
        ("alpha/tasks.base", "filters: file.inFolder(\"alpha\")\n"),
        ("beta/tasks.base", "filters: file.inFolder(\"beta\")\n"),
        ("alpha/deep/one.md", &record("[[tasks.base]]")),
        ("beta/two.md", &record("[[tasks.base|Tasks]]")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    Ok(())
}

#[test]
fn a_bare_base_name_no_ancestor_holds_and_several_files_share_is_reported_as_ambiguous()
-> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        ("alpha/tasks.base", "filters: file.inFolder(\"alpha\")\n"),
        ("beta/tasks.base", "filters: file.inFolder(\"beta\")\n"),
        ("gamma/one.md", &record("[[tasks.base]]")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Fail);
    assert!(check.message.contains("gamma/one.md"), "{}", check.message);
    assert!(check.message.contains("more than one"), "{}", check.message);
    Ok(())
}

#[test]
fn a_note_the_base_filters_do_not_select_is_reported_even_inside_its_folder() -> Result<(), Box<dyn std::error::Error>>
{
    let check = check_over(&[
        (
            "decisions.base",
            "filters: file.folder == \"decisions\" && file.basename != \"index\"\n",
        ),
        ("decisions/one.md", &record("[[decisions.base]]")),
        ("decisions/deeper/two.md", &record("[[decisions.base]]")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Fail);
    assert!(check.message.contains("decisions/deeper/two.md"), "{}", check.message);
    assert!(!check.message.contains("decisions/one.md"), "{}", check.message);
    Ok(())
}

#[test]
fn templates_are_not_checked_for_a_base_property() -> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        ("tasks.base", TASKS_BASE),
        ("templates/task.md", &record("[[path_to_base|{{base_name}}]]")),
        ("templates/other.md", "---\nbase: placeholder\n---\n"),
    ])?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    Ok(())
}

#[test]
fn a_base_sidecar_beside_its_base_is_not_required_to_sit_inside_the_base_folder()
-> Result<(), Box<dyn std::error::Error>> {
    let check = check_over(&[
        ("tasks.base", TASKS_BASE),
        ("tasks.md", &record("[[tasks.base|Tasks]]")),
        ("tasks/one.md", &record("[[tasks.base]]")),
    ])?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    Ok(())
}
