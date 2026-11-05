use super::*;

fn reason(raw: &str) -> String {
    match WorkflowName::try_from(raw) {
        Err(error) => error.to_string(),
        Ok(name) => format!("accepted: {name}"),
    }
}

#[test]
fn a_workflow_name_is_lower_case_letters_digits_and_hyphens() -> Result<(), PackageError> {
    assert_eq!(WorkflowName::try_from("sync-2")?.as_str(), "sync-2");
    for bad in ["", "Sync", "sync_crm", "sync.crm", "sync/crm", "sync crm"] {
        assert!(reason(bad).contains("not valid"), "{bad:?}: {}", reason(bad));
    }
    Ok(())
}

#[test]
fn a_workflow_name_is_never_a_name_windows_reserves_for_a_device() {
    for device in ["con", "prn", "aux", "nul", "com1", "com9", "lpt1", "lpt9"] {
        assert!(
            reason(device).contains("Windows reserves"),
            "{device}: {}",
            reason(device)
        );
    }
    assert!(WorkflowName::try_from("com0").is_ok(), "only com1 to com9 are devices");
    assert!(WorkflowName::try_from("console").is_ok());
}

#[test]
fn a_workflow_name_is_at_most_the_length_a_package_name_may_be() {
    assert!(WorkflowName::try_from("a".repeat(MAX_PACKAGE_NAME_LENGTH).as_str()).is_ok());
    assert!(reason(&"a".repeat(MAX_PACKAGE_NAME_LENGTH + 1)).contains("longer than"));
}
