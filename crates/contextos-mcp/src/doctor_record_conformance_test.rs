use crate::doctor::DoctorStatus;
use crate::doctor::fixture::check_over as doctor_check_over;

const TASKS_BASE: &str = "filters: file.inFolder(\"tasks\")\nviews:\n  - type: table\n    name: All\n";
const TASK_TEMPLATE: &str =
    "---\ntype: task\ntask_type: action\nstate: next\nentity: \"{{entity}}\"\n---\nTemplate body.\n";
const DECISION_TEMPLATE: &str = "---\ntype: task\ntask_type: decision\nstate: open\n---\nTemplate body.\n";
const DECLARATION: &str = "template: '[[templates/task|Task]]'\nseries_templates:\n  D: '[[templates/task-decision]]'\nfixed_fields: [task_type]\n";

type TestResult = Result<(), Box<dyn std::error::Error>>;
/// A named vault fixture: its files and the fragments the check's message must contain.
type Case<'a> = (&'a str, Vec<(&'a str, String)>, Vec<&'a str>);

/// The `Record conformance` check the doctor reports over a vault holding `files`.
fn check_over(files: &[(&str, &str)]) -> Result<crate::doctor::DoctorCheck, Box<dyn std::error::Error>> {
    doctor_check_over("Record conformance", files)
}

fn sidecar(base: &str, declaration: &str) -> String {
    format!(
        "---\nkey_prefix: A\nkey_format: \"{{key_prefix}}-{{key_id}}\"\nkey_prefixes:\n  A: 0\n  D: 0\nlast_create: t0\nlast_update: t0\n{declaration}---\n![[{base}|tasks]]\n"
    )
}

fn record(id: &str, base: Option<&str>, fields: &str) -> String {
    let base = base.map_or_else(String::new, |base| format!("base: '{base}'\n"));
    format!("---\nid: {id}\n{base}{fields}---\nBody\n")
}

fn matching(id: &str) -> String {
    record(id, Some("[[tasks.base]]"), "type: task\ntask_type: action\n")
}

/// A vault with the task base, its sidecar declaring [`DECLARATION`], and both templates, plus `extra` files.
fn tasks_vault<'a>(extra: &[(&'a str, &'a str)], sidecar_text: &'a str) -> Vec<(&'a str, &'a str)> {
    let mut files = vec![
        ("tasks.base", TASKS_BASE),
        ("tasks.md", sidecar_text),
        ("templates/task.md", TASK_TEMPLATE),
        ("templates/task-decision.md", DECISION_TEMPLATE),
    ];
    files.extend_from_slice(extra);
    files
}

#[test]
fn a_record_whose_fixed_fields_differ_from_its_template_fails_naming_fields_series_and_template_without_values()
-> TestResult {
    let sidecar_text = sidecar("tasks.base", DECLARATION);
    let differing = record("A-005", Some("[[tasks.base]]"), "type: ZZ-SECRET\n");
    let check = check_over(&tasks_vault(&[("tasks/a-005.md", &differing)], &sidecar_text))?;

    assert_eq!(check.status, DoctorStatus::Fail, "{}", check.message);
    for needle in [
        "1 of 1 records differ from their series template",
        "tasks/a-005.md",
        "type, task_type differ from the template of series A (templates/task.md)",
    ] {
        assert!(check.message.contains(needle), "{needle}: {}", check.message);
    }
    assert!(
        !check.message.contains("ZZ-SECRET"),
        "no value is shown: {}",
        check.message
    );
    assert!(!check.auto_fixable);
    assert!(check.remediation.is_none());
    assert!(
        check
            .action
            .as_deref()
            .is_some_and(|action| action.contains("Correct each listed note's fields to its series template")),
        "{:?}",
        check.action
    );
    Ok(())
}

#[test]
fn a_record_matching_its_template_is_not_listed() -> TestResult {
    let sidecar_text = sidecar("tasks.base", DECLARATION);
    let ok = matching("A-001");
    let decision = record("D-001", Some("[[tasks.base]]"), "type: task\ntask_type: decision\n");
    let bad = record("A-002", Some("[[tasks.base]]"), "type: note\n");
    let check = check_over(&tasks_vault(
        &[
            ("tasks/a-001.md", &ok),
            ("tasks/d-001.md", &decision),
            ("tasks/a-002.md", &bad),
        ],
        &sidecar_text,
    ))?;

    assert_eq!(check.status, DoctorStatus::Fail, "{}", check.message);
    assert!(check.message.contains("1 of 3 records differ"), "{}", check.message);
    assert!(!check.message.contains("a-001.md"), "{}", check.message);
    assert!(!check.message.contains("d-001.md"), "{}", check.message);
    Ok(())
}

#[test]
fn a_record_whose_id_belongs_to_no_declared_series_is_not_checked() -> TestResult {
    let sidecar_text = sidecar("tasks.base", DECLARATION);
    let stray = record("XA-5", Some("[[tasks.base]]"), "type: note\n");
    let check = check_over(&tasks_vault(&[("tasks/xa-5.md", &stray)], &sidecar_text))?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    Ok(())
}

#[test]
fn a_record_without_a_base_property_is_checked_against_the_one_templated_base_selecting_it_and_skipped_when_several_do()
-> TestResult {
    let sidecar_text = sidecar("tasks.base", DECLARATION);
    let bare = record("A-001", None, "type: note\n");

    // One templated base selects it: checked.
    let check = check_over(&tasks_vault(&[("tasks/a-001.md", &bare)], &sidecar_text))?;
    assert_eq!(check.status, DoctorStatus::Fail, "{}", check.message);
    assert!(check.message.contains("tasks/a-001.md"), "{}", check.message);

    // A second base that selects it but declares no template does not govern it.
    let plain_sidecar = sidecar("everything.base", "");
    let check = check_over(&tasks_vault(
        &[
            ("tasks/a-001.md", &bare),
            ("everything.base", "views:\n  - type: table\n    name: All\n"),
            ("everything.md", &plain_sidecar),
        ],
        &sidecar_text,
    ))?;
    assert_eq!(check.status, DoctorStatus::Fail, "{}", check.message);
    assert!(check.message.contains("tasks/a-001.md"), "{}", check.message);

    // Two templated bases select it: not checked.
    let second_sidecar = sidecar("second.base", DECLARATION);
    let check = check_over(&tasks_vault(
        &[
            ("tasks/a-001.md", &bare),
            ("second.base", TASKS_BASE),
            ("second.md", &second_sidecar),
        ],
        &sidecar_text,
    ))?;
    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    Ok(())
}

fn meridian_vault<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut files = vec![
        (
            "meridian/decisions.base",
            "filters: file.inFolder(\"meridian/decisions\")\n",
        ),
        (
            "meridian/registry.base",
            "filters: file.inFolder(\"meridian/registry\")\n",
        ),
    ];
    files.extend_from_slice(extra);
    files
}

#[test]
fn a_base_property_naming_a_base_that_does_not_select_the_note_names_the_one_base_that_does() -> TestResult {
    let moved = record("D-1", Some("[[meridian/decisions.base]]"), "");
    let check = check_over(&meridian_vault(&[("meridian/registry/d-1.md", &moved)]))?;

    assert_eq!(check.status, DoctorStatus::Fail, "{}", check.message);
    assert!(check.message.contains("meridian/registry/d-1.md"), "{}", check.message);
    assert!(
        check
            .message
            .contains("meridian/registry.base is the one base that selects it"),
        "{}",
        check.message
    );
    assert!(!check.auto_fixable);
    Ok(())
}

#[test]
fn a_base_property_naming_a_base_that_does_not_select_the_note_says_no_base_selects_it_or_counts_several_without_naming_them()
-> TestResult {
    let orphan = record("D-1", Some("[[meridian/decisions.base]]"), "");

    let none = check_over(&meridian_vault(&[("meridian/elsewhere/d-1.md", &orphan)]))?;
    assert_eq!(none.status, DoctorStatus::Fail, "{}", none.message);
    assert!(none.message.contains("no base selects it"), "{}", none.message);

    let several = check_over(&meridian_vault(&[
        ("meridian/registry/d-1.md", &orphan),
        (
            "meridian/second-selector.base",
            "filters: file.inFolder(\"meridian/registry\")\n",
        ),
    ]))?;
    assert_eq!(several.status, DoctorStatus::Fail, "{}", several.message);
    assert!(several.message.contains("2 bases select it"), "{}", several.message);
    assert!(!several.message.contains("registry.base"), "{}", several.message);
    assert!(!several.message.contains("second-selector"), "{}", several.message);
    Ok(())
}

#[test]
fn every_case_the_base_property_check_reported_is_still_reported() -> TestResult {
    let cases: Vec<Case> = vec![
        (
            "outside the base's scope",
            vec![
                ("tasks.base", TASKS_BASE.to_owned()),
                ("elsewhere/moved.md", record("T-1", Some("[[tasks.base]]"), "")),
            ],
            vec!["elsewhere/moved.md", "outside"],
        ),
        (
            "a base that does not exist",
            vec![("tasks/one.md", record("T-1", Some("[[gone.base]]"), ""))],
            vec!["tasks/one.md", "gone.base"],
        ),
        (
            "not a link to a base file",
            vec![
                ("tasks.base", TASKS_BASE.to_owned()),
                ("tasks/text.md", record("T-1", Some("tasks.base"), "")),
                ("tasks/note.md", record("T-2", Some("[[tasks]]"), "")),
                ("tasks/number.md", "---\nbase: 3\n---\n".to_owned()),
            ],
            vec!["tasks/text.md", "tasks/note.md", "tasks/number.md"],
        ),
        (
            "ambiguous bare name",
            vec![
                ("alpha/tasks.base", "filters: file.inFolder(\"alpha\")\n".to_owned()),
                ("beta/tasks.base", "filters: file.inFolder(\"beta\")\n".to_owned()),
                ("gamma/one.md", record("T-1", Some("[[tasks.base]]"), "")),
            ],
            vec!["gamma/one.md", "more than one"],
        ),
        (
            "filters that exclude a note inside the folder",
            vec![
                (
                    "decisions.base",
                    "filters: file.folder == \"decisions\" && file.basename != \"index\"\n".to_owned(),
                ),
                ("decisions/deeper/two.md", record("T-1", Some("[[decisions.base]]"), "")),
            ],
            vec!["decisions/deeper/two.md"],
        ),
    ];
    for (name, files, expected) in cases {
        let borrowed = files
            .iter()
            .map(|(path, content)| (*path, content.as_str()))
            .collect::<Vec<_>>();
        let check = check_over(&borrowed)?;

        assert_eq!(check.status, DoctorStatus::Fail, "{name}: {}", check.message);
        for needle in expected {
            assert!(check.message.contains(needle), "{name}: {needle}: {}", check.message);
        }
    }
    Ok(())
}

#[test]
fn a_sidecar_whose_template_cannot_be_used_is_one_entry_and_its_records_are_not_checked() -> TestResult {
    let sidecar_text = sidecar(
        "tasks.base",
        "template: '[[templates/gone|Gone]]'\nfixed_fields: [task_type]\n",
    );
    let bad = record("A-001", Some("[[tasks.base]]"), "type: note\n");
    let check = check_over(&[
        ("tasks.base", TASKS_BASE),
        ("tasks.md", &sidecar_text),
        ("tasks/a-001.md", &bad),
    ])?;

    assert_eq!(check.status, DoctorStatus::Fail, "{}", check.message);
    assert!(
        check
            .message
            .contains("1 sidecars declare a template that cannot be used"),
        "{}",
        check.message
    );
    for needle in ["tasks.md", "templates/gone", "not found"] {
        assert!(check.message.contains(needle), "{needle}: {}", check.message);
    }
    assert!(!check.message.contains("tasks/a-001.md"), "{}", check.message);
    Ok(())
}

#[test]
fn a_sidecar_whose_template_link_resolves_to_a_base_sidecar_is_reported_as_a_template_that_cannot_be_used() -> TestResult
{
    for (reason, link, extra) in [
        ("its own sidecar", "[[tasks]]", Vec::new()),
        (
            "another base's sidecar",
            "[[other]]",
            vec![
                ("other.base", TASKS_BASE),
                ("other.md", "---\ntype: task\n---\n![[other.base|other]]\n"),
            ],
        ),
    ] {
        let sidecar_text = sidecar("tasks.base", &format!("template: '{link}'\n"));
        let mut files = vec![("tasks.base", TASKS_BASE), ("tasks.md", sidecar_text.as_str())];
        files.extend(extra);

        let check = check_over(&files)?;

        assert_eq!(check.status, DoctorStatus::Fail, "{reason}: {}", check.message);
        for needle in [
            "1 sidecars declare a template that cannot be used",
            "tasks.md",
            "resolves to a base sidecar",
        ] {
            assert!(check.message.contains(needle), "{reason}: {needle}: {}", check.message);
        }
    }
    Ok(())
}

#[test]
fn a_template_that_is_not_literal_for_a_fixed_field_is_reported_against_its_sidecar() -> TestResult {
    let sidecar_text = sidecar(
        "tasks.base",
        "template: '[[templates/menu]]'\nfixed_fields: [task_type]\n",
    );
    let check = check_over(&[
        ("tasks.base", TASKS_BASE),
        ("tasks.md", &sidecar_text),
        (
            "templates/menu.md",
            "---\ntype: task\ntask_type: action|decision\n---\nBody\n",
        ),
    ])?;

    assert_eq!(check.status, DoctorStatus::Fail, "{}", check.message);
    assert!(check.message.contains("tasks.md"), "{}", check.message);
    assert!(check.message.contains("task_type"), "{}", check.message);
    Ok(())
}

#[test]
fn a_template_list_of_several_links_without_series_entries_is_reported_once_per_sidecar() -> TestResult {
    let sidecar_text = sidecar(
        "tasks.base",
        "template:\n- '[[templates/task]]'\n- '[[templates/task-decision]]'\n",
    );
    let check = check_over(&tasks_vault(&[], &sidecar_text))?;

    assert_eq!(check.status, DoctorStatus::Fail, "{}", check.message);
    assert_eq!(check.message.matches("tasks.md").count(), 1, "{}", check.message);
    assert!(
        check
            .message
            .contains("1 sidecars declare a template that cannot be used"),
        "{}",
        check.message
    );
    Ok(())
}

#[test]
fn at_most_ten_entries_per_finding_are_listed_in_path_order_and_the_rest_are_counted() -> TestResult {
    let sidecar_text = sidecar("tasks.base", DECLARATION);
    let names = (1..=25)
        .map(|index| format!("tasks/a-{index:03}.md"))
        .collect::<Vec<_>>();
    let contents = (1..=25)
        .map(|index| record(&format!("A-{index:03}"), Some("[[tasks.base]]"), "type: note\n"))
        .collect::<Vec<_>>();
    let extra = names
        .iter()
        .zip(&contents)
        .map(|(name, content)| (name.as_str(), content.as_str()))
        .collect::<Vec<_>>();
    let check = check_over(&tasks_vault(&extra, &sidecar_text))?;

    assert_eq!(check.status, DoctorStatus::Fail, "{}", check.message);
    assert!(check.message.contains("25 of 25 records differ"), "{}", check.message);
    assert!(check.message.contains("tasks/a-001.md"), "{}", check.message);
    assert!(check.message.contains("tasks/a-010.md"), "{}", check.message);
    assert!(!check.message.contains("tasks/a-011.md"), "{}", check.message);
    assert!(check.message.contains(", and 15 more"), "{}", check.message);
    assert!(
        check.message.find("a-001.md") < check.message.find("a-002.md"),
        "{}",
        check.message
    );
    Ok(())
}

#[test]
fn a_vault_with_nothing_to_report_passes_with_its_counts() -> TestResult {
    let sidecar_text = sidecar("tasks.base", DECLARATION);
    let first = matching("A-001");
    let second = matching("A-002");
    let check = check_over(&tasks_vault(
        &[("tasks/a-001.md", &first), ("tasks/a-002.md", &second)],
        &sidecar_text,
    ))?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    assert_eq!(
        check.message,
        "2 records match their series template, and 2 notes name a base that selects them"
    );
    Ok(())
}

#[test]
fn a_vault_declaring_no_template_and_with_consistent_bases_passes() -> TestResult {
    let plain = sidecar("tasks.base", "");
    let one = record("A-001", Some("[[tasks.base]]"), "type: note\n");
    let check = check_over(&[
        ("tasks.base", TASKS_BASE),
        ("tasks.md", &plain),
        ("tasks/a-001.md", &one),
    ])?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    assert!(
        check.message.contains("0 records match their series template"),
        "{}",
        check.message
    );
    Ok(())
}

#[test]
fn templates_and_base_sidecars_never_take_part_as_records() -> TestResult {
    let sidecar_text = sidecar("tasks.base", DECLARATION);
    let placeholder = "---\nid: A-9\nbase: '[[path_to_base|{{base_name}}]]'\ntype: placeholder\n---\n";
    let check = check_over(&tasks_vault(&[("templates/other.md", placeholder)], &sidecar_text))?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    Ok(())
}
