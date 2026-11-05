use super::*;

fn definition(text: &str) -> Result<WorkflowDefinition, PackageError> {
    WorkflowDefinition::try_from(WorkflowText { text, folder: "sync" })
}

fn reason(text: &str) -> String {
    match definition(text) {
        Err(error) => error.to_string(),
        Ok(parsed) => format!("accepted: {parsed:?}"),
    }
}

const MINIMAL: &str = "name = \"Sync\"\nentry = \"main.rhai\"\n";

#[test]
fn the_smallest_workflow_declares_a_name_and_an_entry_and_is_named_by_its_folder() -> Result<(), PackageError> {
    let parsed = definition(MINIMAL)?;
    assert_eq!(parsed.folder, "sync");
    assert_eq!(parsed.name, "Sync");
    assert_eq!(parsed.entry, "main.rhai");
    assert!(
        parsed.calls.permits("any", "tool"),
        "nothing declared permits every call"
    );
    Ok(())
}

#[test]
fn the_keys_that_belong_to_the_package_or_were_replaced_are_unknown_keys() {
    for extra in [
        "slug = \"nightly-sync\"\n",
        "modules = [\"helpers.rhai\"]\n",
        "templates = [\"t\"]\n",
        "libraries = [\"crm\"]\n",
        "mcp_servers = [\"crm\"]\n",
        "binding_scopes = [\"single-vault\"]\n",
        "[[permitted_effects]]\nmcp_server_name = \"crm\"\ntool = \"list_contacts\"\n",
    ] {
        let found = reason(&format!("{MINIMAL}{extra}"));
        assert!(!found.starts_with("accepted"), "{extra}: {found}");
    }
}

#[test]
fn modules_templates_and_libraries_are_refused_as_unknown_keys_naming_the_key() {
    for key in ["modules", "templates", "libraries"] {
        let found = reason(&format!("{MINIMAL}{key} = []\n"));
        assert!(found.contains(&format!("unknown field `{key}`")), "{found}");
    }
}

#[test]
fn the_four_call_lists_are_read_from_the_two_tables() -> Result<(), PackageError> {
    let parsed = definition(&format!(
        "{MINIMAL}\n[mcp_servers]\nallow = [\"contextos\", \"crm\"]\ndeny = [\"mail\"]\n\n[mcp_tools]\nallow = [{{ server = \"crm\", tool = \"list_contacts\" }}]\ndeny = [{{ server = \"contextos\", tool = \"fs_delete_file\" }}]\n"
    ))?;
    assert!(parsed.calls.permits("crm", "list_contacts"));
    assert!(!parsed.calls.permits("crm", "delete_contact"));
    assert!(!parsed.calls.permits("mail", "send"));
    assert!(!parsed.calls.permits("contextos", "fs_delete_file"));
    assert!(parsed.calls.permits("contextos", "fs_write_file"));
    assert!(
        !parsed.calls.permits("other", "anything"),
        "a server off the allow list is refused"
    );
    Ok(())
}

#[test]
fn an_unknown_key_in_a_call_table_is_refused() {
    let found = reason(&format!(
        "{MINIMAL}\n[mcp_servers]\nallow = [\"crm\"]\nblock = [\"mail\"]\n"
    ));
    assert!(found.contains("block"), "{found}");
    let found = reason(&format!(
        "{MINIMAL}\n[mcp_tools]\nallow = [{{ server = \"crm\", name = \"x\" }}]\n"
    ));
    assert!(found.contains("name") || found.contains("tool"), "{found}");
}

#[test]
fn contradictory_call_lists_are_refused_when_the_file_is_read() {
    for text in [
        "[mcp_servers]\nallow = [\"mail\"]\ndeny = [\"mail\"]\n",
        "[mcp_tools]\nallow = [{ server = \"crm\", tool = \"t\" }]\ndeny = [{ server = \"crm\", tool = \"t\" }]\n",
        "[mcp_servers]\ndeny = [\"crm\"]\n[mcp_tools]\nallow = [{ server = \"crm\", tool = \"t\" }]\n",
    ] {
        let found = reason(&format!("{MINIMAL}{text}"));
        assert!(found.contains("contradict"), "{found}");
    }
}

#[test]
fn inputs_outputs_and_required_tools_are_read() -> Result<(), PackageError> {
    let parsed = definition(&format!(
        "{MINIMAL}log_level = \"debug\"\n\n[[input]]\nname = \"vault\"\nkind = \"vault\"\n\n[[output]]\nname = \"result\"\nkind = \"string\"\n\n[[required_tool_contracts]]\nmcp_server_name = \"crm\"\ntool = \"list_contacts\"\ncontract_version = \"v1\"\n"
    ))?;
    assert_eq!(parsed.inputs.fields().len(), 1);
    assert_eq!(parsed.outputs.fields().len(), 1);
    assert_eq!(parsed.required_tool_contracts.len(), 1);
    assert!(parsed.log_level.is_some());
    Ok(())
}

#[test]
fn a_blank_name_an_empty_entry_and_an_unknown_key_are_each_refused() {
    assert!(reason("name = \" \"\nentry = \"main.rhai\"\n").contains("name must not be empty"));
    assert!(reason("name = \"Sync\"\nentry = \" \"\n").contains("entry"));
}
