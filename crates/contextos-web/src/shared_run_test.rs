use serde_json::json;
use tempfile::tempdir;

use super::*;
use crate::release_fixture::{seal, seal_io};

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Installs a package version straight into the store layout, with `script` as its entry.
fn install(store: &std::path::Path, version: &str, extra: &str, script: &str) -> std::io::Result<()> {
    let dir = store.join("acme").join(version);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("manifest.toml"),
        format!(
            "name = \"acme\"\nversion = \"{version}\"\ndigest = \"{DIGEST}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n{extra}"
        ),
    )?;
    std::fs::write(dir.join("sync.rhai"), script)?;
    seal_io(&dir)
}

/// The release most tests install and name.
static ONE: Version = Version::new(1, 0, 0);

fn request<'a>(
    store: &'a std::path::Path,
    workflow: &'a str,
    version: &'a Version,
    values: serde_json::Value,
) -> SharedRunRequest<'a> {
    SharedRunRequest {
        store_root: store,
        package: "acme",
        workflow,
        version,
        lock: None,
        values: RunValues::Structured(match values {
            Value::Object(map) => map,
            _ => Map::new(),
        }),
    }
}

#[test]
fn the_named_release_and_its_own_script_are_used() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install(store.path(), "1.2.0", "", "old")?;
    install(store.path(), "1.10.0", "", "new")?;
    let named = Version::parse("1.10.0")?;

    let run = SharedRun::try_from(request(store.path(), "sync", &named, json!({})))?;

    assert_eq!(run.identity.version, Version::parse("1.10.0")?);
    assert_eq!(run.script.entry(), "new");
    Ok(())
}

#[test]
fn a_run_uses_the_named_version_even_when_a_newer_one_is_installed() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install(store.path(), "1.2.0", "", "old")?;
    install(store.path(), "1.10.0", "", "new")?;
    let named = Version::parse("1.2.0")?;

    let run = SharedRun::try_from(request(store.path(), "sync", &named, json!({})))?;

    assert_eq!(run.script.entry(), "old");
    Ok(())
}

#[test]
fn a_named_release_that_is_not_installed_ends_the_run_as_package_unavailable() -> Result<(), Box<dyn std::error::Error>>
{
    let store = tempdir()?;
    install(store.path(), "1.2.0", "", "old")?;
    let named = Version::parse("9.9.9")?;

    let error = SharedRun::try_from(request(store.path(), "sync", &named, json!({}))).err();

    assert_eq!(
        error.map(|error| error.kind()),
        Some(WorkflowFailureKind::PackageUnavailable)
    );
    Ok(())
}

#[test]
fn a_package_that_is_not_installed_is_package_unavailable() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;

    let error = SharedRun::try_from(request(store.path(), "sync", &ONE, json!({}))).err();

    assert_eq!(
        error.map(|error| error.kind()),
        Some(WorkflowFailureKind::PackageUnavailable)
    );
    Ok(())
}

#[test]
fn a_workflow_the_release_does_not_contain_is_package_unavailable() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install(store.path(), "1.0.0", "", "s")?;

    let error = SharedRun::try_from(request(store.path(), "other", &ONE, json!({}))).err();

    assert_eq!(
        error.map(|error| error.kind()),
        Some(WorkflowFailureKind::PackageUnavailable)
    );
    Ok(())
}

#[test]
fn values_are_validated_against_the_workflows_inputs_before_anything_runs() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install(
        store.path(),
        "1.0.0",
        "\n[[workflow.inputs]]\nname = \"since\"\nkind = \"date\"\n",
        "s",
    )?;

    let missing = SharedRun::try_from(request(store.path(), "sync", &ONE, json!({}))).err();
    let undeclared = SharedRun::try_from(request(
        store.path(),
        "sync",
        &ONE,
        json!({ "since": "2026-01-01", "x": 1 }),
    ))
    .err();
    let valid = SharedRun::try_from(request(store.path(), "sync", &ONE, json!({ "since": "2026-01-01" })))?;

    assert_eq!(
        missing.map(|error| error.kind()),
        Some(WorkflowFailureKind::InputInvalid)
    );
    assert_eq!(
        undeclared.map(|error| error.kind()),
        Some(WorkflowFailureKind::InputInvalid)
    );
    assert_eq!(valid.inputs.values().get("since"), Some(&json!("2026-01-01")));
    Ok(())
}

#[test]
fn the_call_rules_of_the_workflow_are_carried_for_scoping() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install(
        store.path(),
        "1.0.0",
        "\n[[workflow.permitted_effects]]\nmcp_server_name = \"contextos\"\ntool = \"fs_read_text_file\"\n",
        "s",
    )?;

    let run = SharedRun::try_from(request(store.path(), "sync", &ONE, json!({})))?;

    assert!(run.calls.permits("contextos", "fs_read_text_file"));
    assert!(!run.calls.permits("contextos", "fs_delete_file"));
    Ok(())
}

#[test]
fn a_package_name_that_could_leave_the_store_is_never_looked_up() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    let outside = store.path().join("outside");
    install(&outside, "1.0.0", "", "s")?;
    let mut escaping = request(&outside, "sync", &ONE, json!({}));
    escaping.package = "../outside/acme";

    let error = SharedRun::try_from(escaping).err();

    assert_eq!(
        error.map(|error| error.kind()),
        Some(WorkflowFailureKind::PackageUnavailable)
    );
    Ok(())
}

#[test]
fn command_line_arguments_are_read_by_each_inputs_declared_kind() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install(
        store.path(),
        "1.0.0",
        "\n[[workflow.inputs]]\nname = \"count\"\nkind = \"number\"\n",
        "s",
    )?;
    let mut from_text = request(store.path(), "sync", &ONE, json!({}));
    from_text.values = RunValues::CommandLine(vec!["count=3".to_owned()]);

    let run = SharedRun::try_from(from_text)?;

    assert_eq!(run.inputs.values().get("count"), Some(&json!(3)));
    Ok(())
}

#[test]
fn a_bad_command_line_argument_ends_the_run_as_input_invalid() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install(store.path(), "1.0.0", "", "s")?;
    let mut bad = request(store.path(), "sync", &ONE, json!({}));
    bad.values = RunValues::CommandLine(vec!["no-equals".to_owned()]);

    let error = SharedRun::try_from(bad).err();

    assert_eq!(error.map(|error| error.kind()), Some(WorkflowFailureKind::InputInvalid));
    Ok(())
}

#[test]
fn an_entry_script_over_the_size_limit_is_refused_as_unavailable() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install(
        store.path(),
        "1.0.0",
        "",
        &"#".repeat(usize::try_from(MAX_SCRIPT_BYTES)? + 1),
    )?;

    let error = SharedRun::try_from(request(store.path(), "sync", &ONE, json!({}))).err();

    assert_eq!(
        error.map(|error| error.kind()),
        Some(WorkflowFailureKind::PackageUnavailable)
    );
    Ok(())
}

#[test]
fn an_unreadable_entry_script_is_refused_without_naming_where_the_store_is() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install(store.path(), "1.0.0", "", "")?;
    let script = store.path().join("acme/1.0.0/sync/sync.rhai");
    let location = store.path().to_string_lossy().into_owned();

    for content in [
        vec![b'#'; usize::try_from(MAX_SCRIPT_BYTES)? + 1],
        vec![0xff_u8, 0xfe, 0xfd],
    ] {
        std::fs::write(&script, content)?;

        let error = SharedRun::try_from(request(store.path(), "sync", &ONE, json!({}))).err();

        let message = error.map(|error| error.to_string()).unwrap_or_default();
        assert!(!message.is_empty(), "the script was read");
        assert!(
            !message.contains(&location),
            "the message holds the absolute location: {message}"
        );
    }
    Ok(())
}

/// Installs a release whose manifest declares `t.md` as a template holding `text`.
fn install_with_template(store: &std::path::Path, version: &str, text: &str) -> std::io::Result<()> {
    let dir = store.join("acme").join(version);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("manifest.toml"),
        format!(
            "name = \"acme\"\nversion = \"{version}\"\ndigest = \"{DIGEST}\"\ntemplates = [\"t.md\"]\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n"
        ),
    )?;
    std::fs::write(dir.join("sync.rhai"), "")?;
    std::fs::write(dir.join("t.md"), text)?;
    seal_io(&dir)
}

#[tokio::test]
async fn a_run_reads_the_declared_files_of_the_release_it_resolved_and_no_other()
-> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install_with_template(store.path(), "1.0.0", "one")?;
    let first = SharedRun::try_from(request(store.path(), "sync", &ONE, json!({})))?;
    // An upgrade installed after the first run resolved its release, as between two runs.
    install_with_template(store.path(), "2.0.0", "two")?;
    let two = Version::parse("2.0.0")?;
    let second = SharedRun::try_from(request(store.path(), "sync", &two, json!({})))?;

    assert_eq!(first.files.read("t.md").await?.content, "one");
    assert_eq!(second.files.read("t.md").await?.content, "two");
    assert_eq!(
        first.files.read("t.md").await?.content,
        "one",
        "a release installed after the first run resolved does not change what it reads"
    );
    assert_eq!(
        second.files.read("sync.rhai").await.err().map(|failure| failure.kind),
        Some(WorkflowFailureKind::FileUndeclared)
    );
    Ok(())
}

/// Installs a release whose manifest declares `modules` before its workflow, with each of `files` beside the entry.
fn install_with_modules(store: &std::path::Path, modules: &str, files: &[(&str, &str)]) -> std::io::Result<()> {
    let dir = store.join("acme").join("1.0.0");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("manifest.toml"),
        format!(
            "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\nmodules = {modules}\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n"
        ),
    )?;
    std::fs::write(dir.join("sync.rhai"), "import \"lib/vault\" as vault;")?;
    for (path, text) in files {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(dir.join(parent))?;
        }
        std::fs::write(dir.join(path), text)?;
    }
    seal_io(&dir)
}

#[test]
fn a_run_takes_every_module_the_package_declares_from_the_release_it_resolved() -> Result<(), Box<dyn std::error::Error>>
{
    let store = tempdir()?;
    install_with_modules(
        store.path(),
        "[\"lib/vault.rhai\"]",
        &[("lib/vault.rhai", "fn info() { 1 }")],
    )?;

    let run = SharedRun::try_from(request(store.path(), "sync", &ONE, json!({})))?;

    let modules: Vec<(&str, &str)> = run
        .script
        .modules()
        .map(|module| (module.path().as_str(), module.text()))
        .collect();
    assert_eq!(modules, [("lib/vault.rhai", "fn info() { 1 }")]);
    Ok(())
}

#[test]
fn a_release_missing_a_declared_module_is_refused_as_unavailable_naming_the_module()
-> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install_with_modules(store.path(), "[\"lib/vault.rhai\"]", &[])?;

    let refused = SharedRun::try_from(request(store.path(), "sync", &ONE, json!({})));

    assert!(
        matches!(refused, Err(SharedRunError::Unavailable(ref message)) if message.contains("lib/vault.rhai")),
        "{refused:?}"
    );
    Ok(())
}

/// Installs a library package straight into the store layout: every file in `files` is a declared module and an export.
fn install_library(store: &std::path::Path, name: &str, version: &str, files: &[(&str, &str)]) -> std::io::Result<()> {
    let dir = store.join(name).join(version);
    std::fs::create_dir_all(&dir)?;
    let listed: Vec<String> = files.iter().map(|(path, _)| format!("\"{path}\"")).collect();
    std::fs::write(
        dir.join("manifest.toml"),
        format!(
            "name = \"{name}\"\nversion = \"{version}\"\ndigest = \"{DIGEST}\"\nmodules = [{0}]\nexports = [{0}]\n",
            listed.join(", ")
        ),
    )?;
    for (path, text) in files {
        std::fs::write(dir.join(path), text)?;
    }
    seal_io(&dir)
}

#[test]
fn a_run_carries_the_code_of_its_library_releases_and_names_them() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install(
        store.path(),
        "1.0.0",
        "",
        "import \"acme-text:strings\" as s;\ns::shout()",
    )?;
    // The package under test needs the dependency and a module of its own, which `install` does not write.
    let dir = store.path().join("acme").join("1.0.0");
    std::fs::write(
        dir.join("manifest.toml"),
        format!(
            "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\nmodules = [\"util.rhai\"]\n[[dependencies]]\nname = \"acme-text\"\nrequirement = \"^1\"\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n"
        ),
    )?;
    std::fs::write(dir.join("util.rhai"), "fn who() { \"acme\" }")?;
    seal(&dir)?;
    install_library(
        store.path(),
        "acme-text",
        "1.0.0",
        &[("strings.rhai", "fn shout() { \"old\" }")],
    )?;
    install_library(
        store.path(),
        "acme-text",
        "1.3.0",
        &[("strings.rhai", "fn shout() { \"new\" }")],
    )?;

    let run = SharedRun::try_from(request(store.path(), "sync", &ONE, json!({})))?;

    let libraries: Vec<String> = run
        .libraries
        .iter()
        .map(|library| format!("{} {}", library.name, library.version))
        .collect();
    assert_eq!(libraries, ["acme-text 1.3.0"]);
    let keys: Vec<String> = run.script.modules().map(|module| module.key().to_string()).collect();
    assert_eq!(keys, ["acme:util", "acme-text:strings"]);
    assert_eq!(
        run.script
            .modules()
            .find(|module| module.key().to_string() == "acme-text:strings")
            .map(contextos_workflows::ModuleSource::text),
        Some("fn shout() { \"new\" }")
    );
    Ok(())
}

#[test]
fn a_run_whose_library_is_not_installed_ends_as_package_unavailable_naming_it() -> Result<(), Box<dyn std::error::Error>>
{
    let store = tempdir()?;
    install(store.path(), "1.0.0", "", "()")?;
    let dir = store.path().join("acme").join("1.0.0");
    std::fs::write(
        dir.join("manifest.toml"),
        format!(
            "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\n[[dependencies]]\nname = \"acme-text\"\nrequirement = \"^1\"\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n"
        ),
    )?;
    seal(&dir)?;

    let error = SharedRun::try_from(request(store.path(), "sync", &ONE, json!({}))).err();

    let Some(error) = error else {
        return Err("the run should not have been ready".into());
    };
    assert_eq!(error.kind(), WorkflowFailureKind::PackageUnavailable);
    assert!(error.to_string().contains("acme-text"), "{error}");
    Ok(())
}
