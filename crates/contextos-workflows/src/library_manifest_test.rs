use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const MINIMAL: &str = "modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n";

fn refused(text: &str) -> Result<String, String> {
    match LibraryManifest::try_from(text) {
        Ok(_) => Err(format!("a manifest that should be refused was accepted: {text}")),
        Err(PackageError::InvalidLibraryManifest { reason }) => Ok(reason),
        Err(other) => Err(format!("refused with the wrong error: {other:?}")),
    }
}

#[test]
fn the_smallest_manifest_exports_a_module() -> TestResult {
    let manifest = LibraryManifest::try_from(MINIMAL)?;

    assert_eq!(manifest.modules().entries().len(), 1);
    assert_eq!(manifest.exports().entries()[0].as_str(), "client.rhai");
    assert_eq!(manifest.libraries().entries().len(), 0);
    assert_eq!(manifest.mcp_tools().len(), 0);
    Ok(())
}

#[test]
fn a_full_manifest_names_its_modules_exports_libraries_and_tools() -> TestResult {
    let manifest = LibraryManifest::try_from(
        "modules = [\"client.rhai\", \"lib/paging.rhai\"]\nexports = [\"client.rhai\"]\n\
         mcp_tools = [{ server = \"crm\", tool = \"list_contacts\" }, { server = \"mail\", tool = \"send\" }]\n\
         [[libraries]]\nname = \"http-helpers\"\nrequirement = \"^1\"\n",
    )?;

    assert_eq!(manifest.modules().entries().len(), 2);
    assert_eq!(manifest.libraries().entries()[0].name, "http-helpers");
    let declared: Vec<(&str, &str)> = manifest
        .mcp_tools()
        .iter()
        .map(|entry| (entry.server.as_str(), entry.tool.as_str()))
        .collect();
    assert_eq!(declared, [("crm", "list_contacts"), ("mail", "send")]);
    Ok(())
}

#[test]
fn an_unknown_key_is_refused_rather_than_ignored() -> TestResult {
    let reason = refused(&format!("{MINIMAL}slug = \"crm\"\n"))?;

    assert!(reason.contains("slug"), "{reason}");
    Ok(())
}

#[test]
fn a_library_that_exports_nothing_is_refused_because_nothing_could_use_it() -> TestResult {
    let reason = refused("modules = [\"client.rhai\"]\nexports = []\n")?;

    assert!(reason.contains("exports"), "{reason}");
    Ok(())
}

#[test]
fn an_export_that_is_not_a_declared_module_is_refused() -> TestResult {
    let reason = refused("modules = [\"client.rhai\"]\nexports = [\"other.rhai\"]\n")?;

    assert!(reason.contains("other.rhai"), "{reason}");
    Ok(())
}

#[test]
fn a_module_path_that_could_leave_the_library_or_shadow_the_built_ins_is_refused() -> TestResult {
    for path in [
        "../x.rhai",
        "/abs.rhai",
        "a\\\\b.rhai",
        "notes.txt",
        "contextos/text.rhai",
        "c:x.rhai",
    ] {
        let text = format!("modules = [\"{path}\"]\nexports = [\"{path}\"]\n");
        refused(&text)?;
    }
    Ok(())
}

#[test]
fn a_module_declared_twice_is_refused() -> TestResult {
    refused("modules = [\"a.rhai\", \"a.rhai\"]\nexports = [\"a.rhai\"]\n")?;
    Ok(())
}

#[test]
fn more_than_a_hundred_modules_are_refused() -> TestResult {
    let modules: Vec<String> = (0..101).map(|index| format!("\"m{index}.rhai\"")).collect();
    let text = format!("modules = [{}]\nexports = [\"m0.rhai\"]\n", modules.join(", "));

    let reason = refused(&text)?;

    assert!(reason.contains("100"), "{reason}");
    Ok(())
}

#[test]
fn a_library_named_in_libraries_must_be_a_valid_name_with_a_valid_requirement_and_named_once() -> TestResult {
    let entry =
        |name: &str, requirement: &str| format!("[[libraries]]\nname = {name:?}\nrequirement = {requirement:?}\n");
    for libraries in [
        entry("Not-Valid", "1"),
        entry("../up", "1"),
        entry("contextos", "1"),
        entry("crm", "not-a-requirement"),
        format!("{}{}", entry("a", "1"), entry("a", "2")),
        "libraries = [\"crm\"]\n".to_owned(),
    ] {
        refused(&format!("{MINIMAL}{libraries}"))?;
    }
    Ok(())
}

#[test]
fn a_bare_library_version_is_a_caret_requirement() -> TestResult {
    let manifest = LibraryManifest::try_from(
        format!("{MINIMAL}[[libraries]]\nname = \"crm\"\nrequirement = \"1.4.2\"\n").as_str(),
    )?;

    assert_eq!(manifest.libraries().entries()[0].requirement.to_string(), "^1.4.2");
    Ok(())
}

#[test]
fn text_that_is_not_toml_or_has_the_wrong_types_is_refused() -> TestResult {
    refused("modules = \n")?;
    refused("modules = 4\nexports = []\n")?;
    Ok(())
}

#[test]
fn the_name_of_a_library_is_not_a_key_of_its_manifest() -> TestResult {
    let reason = refused(&format!("name = \"CRM\"\n{MINIMAL}"))?;

    assert!(reason.contains("unknown field `name`"), "{reason}");
    Ok(())
}

#[test]
fn the_old_server_list_is_an_unknown_key() -> TestResult {
    let reason = refused(&format!("mcp_servers = [\"crm\"]\n{MINIMAL}"))?;

    assert!(reason.contains("unknown field `mcp_servers`"), "{reason}");
    Ok(())
}

#[test]
fn a_tool_declared_twice_or_with_a_blank_name_is_refused() -> TestResult {
    let twice = refused(&format!(
        "{MINIMAL}mcp_tools = [{{ server = \"crm\", tool = \"find\" }}, {{ server = \"crm\", tool = \"find\" }}]\n"
    ))?;
    assert!(twice.contains("twice"), "{twice}");
    let blank = refused(&format!(
        "{MINIMAL}mcp_tools = [{{ server = \"crm\", tool = \" \" }}]\n"
    ))?;
    assert!(blank.contains("blank"), "{blank}");
    Ok(())
}

#[test]
fn a_tool_entry_takes_a_server_and_a_tool_and_nothing_else() -> TestResult {
    let reason = refused(&format!(
        "{MINIMAL}mcp_tools = [{{ server = \"crm\", tool = \"find\", allow = true }}]\n"
    ))?;

    assert!(reason.contains("allow"), "{reason}");
    Ok(())
}

#[test]
fn a_wildcard_declares_every_tool_of_its_server_and_sits_beside_exact_tools_of_other_servers() -> TestResult {
    let manifest = LibraryManifest::try_from(&format!(
        "{MINIMAL}mcp_tools = [{{ server = \"contextos\", tool = \"*\" }}, {{ server = \"mail\", tool = \"send\" }}]\n"
    ) as &str)?;

    let declared: Vec<(&str, &str)> = manifest
        .mcp_tools()
        .iter()
        .map(|entry| (entry.server.as_str(), entry.tool.as_str()))
        .collect();
    assert_eq!(declared, [("contextos", "*"), ("mail", "send")]);
    Ok(())
}

#[test]
fn a_wildcard_beside_an_exact_tool_of_the_same_server_is_refused_as_redundant_in_either_order() -> TestResult {
    for tools in [
        "{ server = \"crm\", tool = \"*\" }, { server = \"crm\", tool = \"find\" }",
        "{ server = \"crm\", tool = \"find\" }, { server = \"crm\", tool = \"*\" }",
    ] {
        let reason = refused(&format!("{MINIMAL}mcp_tools = [{tools}]\n"))?;

        assert!(reason.contains("crm") && reason.contains("redundant"), "{reason}");
    }
    Ok(())
}

#[test]
fn an_installed_library_manifest_states_a_requirement_for_every_library_it_uses() -> TestResult {
    let reason = refused(&format!("{MINIMAL}[[libraries]]\nname = \"crm\"\n"))?;

    assert!(reason.contains("requirement"), "{reason}");
    Ok(())
}

#[test]
fn a_library_manifest_in_source_may_leave_a_library_requirement_out() -> TestResult {
    let manifest = LibraryManifest::try_from(SourceText(&format!(
        "{MINIMAL}[[libraries]]\nname = \"crm\"\n[[libraries]]\nname = \"http\"\nrequirement = \"^1\"\n"
    )))?;

    let names: Vec<String> = manifest.libraries().names().iter().map(ToString::to_string).collect();
    assert_eq!(names, ["crm", "http"]);
    assert_eq!(
        manifest.libraries().entries().len(),
        1,
        "only the stated requirement is an entry"
    );
    assert_eq!(manifest.libraries().entries()[0].name, "http");
    Ok(())
}

#[test]
fn a_library_manifest_in_source_is_otherwise_held_to_the_same_rules() {
    let found = LibraryManifest::try_from(SourceText(&format!(
        "{MINIMAL}[[libraries]]\nname = \"crm\"\nrequirement = \"not-a-requirement\"\n"
    )));
    assert!(
        matches!(found, Err(PackageError::InvalidLibraryManifest { .. })),
        "{found:?}"
    );
    let twice = LibraryManifest::try_from(SourceText(&format!(
        "{MINIMAL}[[libraries]]\nname = \"crm\"\n[[libraries]]\nname = \"crm\"\n"
    )));
    assert!(
        matches!(twice, Err(PackageError::InvalidLibraryManifest { .. })),
        "{twice:?}"
    );
}
