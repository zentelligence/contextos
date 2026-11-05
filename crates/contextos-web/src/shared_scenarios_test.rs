use tempfile::tempdir;

use super::*;

const CALL: &str =
    "[[call]]\nserver = \"contextos\"\ntool = \"vault_info\"\nkind = \"valid\"\nresponse = { ok = true }\n";

fn ship(store: &std::path::Path, workflow: &str, name: &str, text: &str) -> std::io::Result<()> {
    let dir = store.join("acme").join("1.0.0").join(workflow).join("tests");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{name}.toml")), text)
}

fn load(
    store: &std::path::Path,
    package: &contextos_workflows::PackageName,
    requested: Option<&str>,
) -> Result<Scenario, SharedScenarioError> {
    load_shared_scenario(store, package, &Version::new(1, 0, 0), "sync", requested).map(|(_, scenario)| scenario)
}

fn acme() -> Result<contextos_workflows::PackageName, contextos_workflows::package::PackageError> {
    contextos_workflows::PackageName::try_from("acme")
}

#[test]
fn the_only_scenario_a_package_ships_for_a_workflow_is_used_when_none_is_named()
-> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    ship(store.path(), "sync", "happy-path", CALL)?;

    let scenario = load(store.path(), &acme()?, None)?;

    assert_eq!(scenario.calls.len(), 1);
    Ok(())
}

#[test]
fn a_named_scenario_is_loaded() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    ship(store.path(), "sync", "happy-path", CALL)?;
    ship(store.path(), "sync", "second", CALL)?;

    assert!(load(store.path(), &acme()?, Some("second")).is_ok());
    Ok(())
}

#[test]
fn a_workflow_with_no_scenario_is_refused_and_the_message_names_the_workflow_and_package()
-> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    std::fs::create_dir_all(store.path().join("acme").join("1.0.0"))?;
    ship(store.path(), "other", "happy-path", CALL)?;

    let refused = load(store.path(), &acme()?, None);

    let message = refused.as_ref().err().map(ToString::to_string).unwrap_or_default();
    assert!(
        matches!(refused, Err(SharedScenarioError::NoScenarios { .. })),
        "{refused:?}"
    );
    assert!(message.contains("sync") && message.contains("acme"), "{message}");
    Ok(())
}

#[test]
fn several_scenarios_without_a_name_are_ambiguous() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    ship(store.path(), "sync", "one", CALL)?;
    ship(store.path(), "sync", "two", CALL)?;

    assert!(matches!(
        load(store.path(), &acme()?, None),
        Err(SharedScenarioError::Ambiguous { count: 2, .. })
    ));
    Ok(())
}

#[test]
fn a_scenario_that_is_not_shipped_is_not_found() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    ship(store.path(), "sync", "happy-path", CALL)?;

    assert!(matches!(
        load(store.path(), &acme()?, Some("missing")),
        Err(SharedScenarioError::NotFound { .. })
    ));
    Ok(())
}

#[test]
fn a_scenario_name_cannot_leave_the_workflows_tests_folder() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    ship(store.path(), "sync", "happy-path", CALL)?;
    std::fs::write(store.path().join("acme").join("1.0.0").join("secret.toml"), CALL)?;

    for name in ["../../secret", "..", "a/b", "a\\b", ""] {
        assert!(
            matches!(
                load(store.path(), &acme()?, Some(name)),
                Err(SharedScenarioError::NotFound { .. })
            ),
            "{name:?}"
        );
    }
    Ok(())
}

#[test]
fn a_scenario_that_does_not_parse_is_reported_as_invalid() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    ship(store.path(), "sync", "broken", "not = [valid")?;

    assert!(matches!(
        load(store.path(), &acme()?, None),
        Err(SharedScenarioError::Invalid { .. })
    ));
    Ok(())
}

#[test]
fn scenarios_of_another_workflow_are_not_offered() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    ship(store.path(), "other", "happy-path", CALL)?;

    assert!(matches!(
        load(store.path(), &acme()?, None),
        Err(SharedScenarioError::NoScenarios { .. })
    ));
    Ok(())
}

#[test]
fn an_oversized_or_non_text_scenario_is_refused_without_naming_where_the_store_is()
-> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    ship(
        store.path(),
        "sync",
        "big",
        &"#".repeat(usize::try_from(MAX_SCENARIO_BYTES)? + 1),
    )?;
    let binary = store.path().join("acme/1.0.0/sync/tests/binary.toml");
    std::fs::write(binary, [0xff, 0xfe, 0xfd])?;
    let location = store.path().to_string_lossy().into_owned();

    for name in ["big", "binary"] {
        let error = load(store.path(), &acme()?, Some(name)).err();

        let message = error.map(|error| error.to_string()).unwrap_or_default();
        assert!(!message.is_empty(), "{name}: the scenario was loaded");
        assert!(
            !message.contains(&location),
            "{name}: the message holds the absolute location: {message}"
        );
    }
    Ok(())
}
