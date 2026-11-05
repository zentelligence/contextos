use super::*;
use crate::workflow_files::WorkflowFilePath;

fn refusal(text: &str) -> String {
    match WorkflowsFile::try_from(text) {
        Err(error) => error.to_string(),
        Ok(parsed) => format!("accepted: {parsed:?}"),
    }
}

#[test]
fn a_workflows_file_names_its_workflows_and_declares_the_package_level_files() -> Result<(), Box<dyn std::error::Error>>
{
    let parsed = WorkflowsFile::try_from(
        "workflows = [\"scaffold\", \"update\"]\n\
         modules = [\"modules/helpers.rhai\"]\n\
         templates = [\"templates\", \"rules/lint.toml\"]\n\
         [[libraries]]\nname = \"crm\"\nrequirement = \"1.4.2\"\n",
    )?;
    assert_eq!(parsed.workflows(), ["scaffold", "update"]);
    assert_eq!(parsed.modules().entries().len(), 1);
    assert!(
        parsed
            .templates()
            .admits(&WorkflowFilePath::try_from("templates/entity/profile.md")?)
    );
    assert!(
        !parsed
            .templates()
            .admits(&WorkflowFilePath::try_from("scaffold/scaffold.rhai")?)
    );
    let libraries = parsed.libraries().entries();
    assert_eq!(libraries.len(), 1);
    assert_eq!(libraries[0].name, "crm");
    assert_eq!(libraries[0].requirement, semver::VersionReq::parse("^1.4.2")?);
    Ok(())
}

#[test]
fn a_bare_library_version_means_the_caret_requirement() -> Result<(), Box<dyn std::error::Error>> {
    let parsed =
        WorkflowsFile::try_from("workflows = [\"a\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \"1.4.2\"\n")?;
    let requirement = &parsed.libraries().entries()[0].requirement;
    assert!(requirement.matches(&semver::Version::parse("1.9.0")?));
    assert!(!requirement.matches(&semver::Version::parse("2.0.0")?));
    Ok(())
}

#[test]
fn modules_templates_and_libraries_are_optional() -> Result<(), PackageError> {
    let parsed = WorkflowsFile::try_from("workflows = [\"a\"]\n")?;
    assert!(parsed.modules().is_empty());
    assert!(parsed.templates().is_empty());
    assert!(matches!(parsed.libraries().entries(), []));
    Ok(())
}

#[test]
fn a_workflows_file_without_workflows_is_refused() {
    assert!(refusal("modules = []\n").contains("workflows"));
    assert!(refusal("workflows = []\n").contains("at least one workflow"));
}

#[test]
fn a_workflow_name_that_is_not_a_slug_or_is_repeated_is_refused() {
    for names in ["[\"Sync\"]", "[\"a/b\"]", "[\"\"]", "[\"sync\", \"sync\"]"] {
        let found = refusal(&format!("workflows = {names}\n"));
        assert!(found.contains("workflows"), "{names}: {found}");
        assert!(!found.starts_with("accepted"), "{names}: {found}");
    }
}

#[test]
fn keys_other_than_the_four_are_unknown_keys() {
    for body in [
        "exports = [\"a.rhai\"]\n",
        "mcp_tools = []\n",
        "dependencies = []\n",
        "name = \"x\"\n",
        "[[workflow]]\nslug = \"a\"\n",
        "binding_scopes = []\n",
    ] {
        let found = refusal(&format!("workflows = [\"a\"]\n{body}"));
        assert!(found.contains("unknown field"), "{body}: {found}");
    }
}

#[test]
fn a_library_named_twice_or_with_a_bad_requirement_is_refused() {
    let twice = refusal(
        "workflows = [\"a\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \"1\"\n[[libraries]]\nname = \"crm\"\nrequirement = \"2\"\n",
    );
    assert!(twice.contains("twice"), "{twice}");
    let bad = refusal("workflows = [\"a\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \"not-a-req\"\n");
    assert!(bad.contains("not a valid version requirement"), "{bad}");
}

#[test]
fn a_module_or_template_path_that_leaves_the_package_is_refused() {
    assert!(!refusal("workflows = [\"a\"]\nmodules = [\"../x.rhai\"]\n").starts_with("accepted"));
    assert!(!refusal("workflows = [\"a\"]\ntemplates = [\"/etc\"]\n").starts_with("accepted"));
}

#[test]
fn a_workflow_named_like_a_windows_device_or_too_long_is_refused() {
    assert!(refusal("workflows = [\"con\"]\n").contains("Windows reserves"));
    assert!(refusal("workflows = [\"ok\", \"lpt3\"]\n").contains("Windows reserves"));
    let long = "a".repeat(65);
    assert!(refusal(&format!("workflows = [\"{long}\"]\n")).contains("longer than"));
}

#[test]
fn an_installed_workflows_file_states_a_requirement_for_every_library_it_uses() {
    let found = refusal("workflows = [\"a\"]\n[[libraries]]\nname = \"crm\"\n");
    assert!(found.contains("requirement"), "{found}");
}

#[test]
fn a_workflows_file_in_source_may_leave_a_library_requirement_out() -> Result<(), PackageError> {
    let parsed = WorkflowsFile::try_from(SourceText(
        "workflows = [\"a\"]\n[[libraries]]\nname = \"crm\"\n[[libraries]]\nname = \"http\"\nrequirement = \"1.2.0\"\n",
    ))?;
    let names: Vec<String> = parsed.libraries().names().iter().map(ToString::to_string).collect();
    assert_eq!(names, ["crm", "http"]);
    assert_eq!(parsed.libraries().entries().len(), 1);
    Ok(())
}
