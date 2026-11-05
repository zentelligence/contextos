//! Static validation of a shared job (`contextos_web::shared_validation::validate_shared_job`) against a real
//! `contextos-mcp` session and a real package store: every failure is reported with its code and the file it concerns, a
//! script is compiled and never run, a tool is checked by listing a server's tools, and an outage is unverifiable rather
//! than a defect in the job.

mod support;

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use contextos_web::McpClientSet;
use contextos_web::job_validation::JobValidation;
use contextos_web::shared_claim::SharedJobKey;
use contextos_web::shared_jobs::SharedJobFile;
use contextos_web::shared_validation::{SharedValidationHost, validate_shared_job};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const VAULT_NAME: &str = "contract-fixture";
const READS: &str = "\n[[workflow.permitted_effects]]\nmcp_server_name = \"contextos\"\ntool = \"vault_info\"\n";

struct Fixture {
    store: tempfile::TempDir,
    _config: tempfile::TempDir,
    _vault: tempfile::TempDir,
    clients: Arc<McpClientSet>,
}

/// A package `acme` 1.0.0 whose workflow `sync` has `workflow_extra` appended to its manifest entry and `script` as its
/// entry script.
async fn fixture(workflow_extra: &str, script: &str) -> Result<Fixture, BoxError> {
    let store = tempfile::tempdir()?;
    let dir = store.path().join("acme").join("1.0.0");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("manifest.toml"),
        format!(
            "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n{workflow_extra}"
        ),
    )?;
    std::fs::write(dir.join("sync.rhai"), script)?;
    let vault = tempfile::tempdir()?;
    std::fs::write(vault.path().join("index.md"), "# Root\n")?;
    let config = tempfile::tempdir()?;
    let config_path = support::write_vault_config(config.path(), vault.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(Fixture {
        store,
        _config: config,
        _vault: vault,
        clients,
    })
}

fn job(text: &str) -> SharedJobFile {
    SharedJobFile {
        key: SharedJobKey {
            package: "acme".to_owned(),
            workflow: "sync".to_owned(),
            job: "nightly".to_owned(),
        },
        text: text.to_owned(),
    }
}

async fn validate(fixture: &Fixture, file: &SharedJobFile) -> JobValidation {
    validate_with(fixture, file, &["contextos".to_owned()]).await
}

/// Makes every release in the store whole, as an install would leave it: the fixtures write their files by hand, and a
/// release is read only when its manifest lists exactly the files it holds and declares their digest.
fn seal_every_release(store: &Path) -> Result<(), BoxError> {
    for package in std::fs::read_dir(store)? {
        for release in std::fs::read_dir(package?.path())? {
            support::release_fixture::seal_io(&release?.path())?;
        }
    }
    Ok(())
}

async fn validate_with(fixture: &Fixture, file: &SharedJobFile, known_servers: &[String]) -> JobValidation {
    let sealed = seal_every_release(fixture.store.path());
    assert!(sealed.is_ok(), "the fixture releases could not be sealed: {sealed:?}");
    validate_shared_job(
        &SharedValidationHost {
            store_root: fixture.store.path(),
            clients: &fixture.clients,
            primary_server: "contextos",
            known_servers,
        },
        file,
    )
    .await
}

fn codes(validation: &JobValidation) -> Vec<&'static str> {
    validation
        .report
        .failures
        .iter()
        .map(|failure| failure.code.as_str())
        .collect()
}

#[tokio::test]
async fn a_complete_job_passes_and_has_a_version() -> Result<(), BoxError> {
    let fixture = fixture(READS, "log_info(\"hello\");").await?;

    let validation = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
    )
    .await;

    assert!(validation.report.passed(), "{:?}", validation.report);
    assert!(validation.fingerprint.is_some());
    assert!(!validation.ready);
    Ok(())
}

#[tokio::test]
async fn a_job_file_that_says_ready_is_reported_as_ready() -> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;

    let validation = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nready = true\n"),
    )
    .await;

    assert!(validation.ready);
    Ok(())
}

#[tokio::test]
async fn a_package_that_is_not_installed_is_reported_as_missing() -> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;
    let mut file = job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n");
    file.key.package = "absent".to_owned();

    let validation = validate(&fixture, &file).await;

    assert_eq!(codes(&validation), ["validation/artefact-missing"]);
    assert_eq!(validation.fingerprint, None);
    Ok(())
}

#[tokio::test]
async fn a_workflow_the_package_does_not_have_is_reported_as_missing() -> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;
    let mut file = job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n");
    file.key.workflow = "other".to_owned();

    let validation = validate(&fixture, &file).await;

    assert_eq!(codes(&validation), ["validation/artefact-missing"]);
    assert!(
        validation.fingerprint.is_some(),
        "the package resolved, so the job has a version"
    );
    Ok(())
}

#[tokio::test]
async fn a_job_file_that_does_not_match_its_schema_is_reported_as_invalid() -> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;

    let validation = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nowner = \"someone\"\n"),
    )
    .await;

    assert_eq!(codes(&validation), ["validation/artefact-invalid"]);
    assert_eq!(validation.report.failures[0].artefact, "nightly.toml");
    Ok(())
}

#[tokio::test]
async fn a_schedule_in_a_job_file_is_reported_as_authored_and_never_honoured() -> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;

    let validation = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nschedule = \"* * * * *\"\n"),
    )
    .await;

    assert_eq!(codes(&validation), ["validation/artefact-invalid"]);
    Ok(())
}

#[tokio::test]
async fn a_syntax_error_in_the_script_is_reported_with_its_file_and_line() -> Result<(), BoxError> {
    let fixture = fixture("", "let x = 1;\nlet y = (2 +;\n").await?;

    let validation = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
    )
    .await;

    assert_eq!(codes(&validation), ["validation/script-syntax"]);
    let failure = &validation.report.failures[0];
    assert_eq!(failure.artefact, "sync.rhai");
    assert!(failure.message.contains("line 2"), "{}", failure.message);
    Ok(())
}

#[tokio::test]
async fn a_missing_entry_script_is_reported_as_missing() -> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;
    // The release is laid out before the entry script is taken from it.
    seal_every_release(fixture.store.path())?;
    std::fs::remove_file(fixture.store.path().join("acme/1.0.0/sync/sync.rhai"))?;

    let validation = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
    )
    .await;

    assert_eq!(codes(&validation), ["validation/artefact-missing"]);
    assert_eq!(validation.report.failures[0].artefact, "sync/sync.rhai");
    Ok(())
}

#[tokio::test]
async fn values_that_do_not_satisfy_the_declared_inputs_are_reported() -> Result<(), BoxError> {
    let fixture = fixture("\n[[workflow.inputs]]\nname = \"since\"\nkind = \"date\"\n", "()").await?;

    let missing = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
    )
    .await;
    let wrong_kind = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n[inputs]\nsince = \"yesterday\"\n"),
    )
    .await;
    let fine = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n[inputs]\nsince = \"2026-01-01\"\n"),
    )
    .await;

    assert_eq!(codes(&missing), ["validation/input-invalid"]);
    assert_eq!(codes(&wrong_kind), ["validation/input-invalid"]);
    assert!(fine.report.passed(), "{:?}", fine.report);
    Ok(())
}

#[tokio::test]
async fn a_vault_input_must_name_a_vault_the_primary_server_reports() -> Result<(), BoxError> {
    let fixture = fixture("\n[[workflow.inputs]]\nname = \"source\"\nkind = \"vault\"\n", "()").await?;

    let configured = validate(
        &fixture,
        &job(&format!(
            "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n[inputs]\nsource = \"{VAULT_NAME}\"\n"
        )),
    )
    .await;
    let unknown = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n[inputs]\nsource = \"no-such-vault\"\n"),
    )
    .await;

    assert!(configured.report.passed(), "{:?}", configured.report);
    assert_eq!(codes(&unknown), ["validation/input-invalid"]);
    assert!(unknown.report.failures[0].message.contains("no-such-vault"));
    Ok(())
}

#[tokio::test]
async fn a_server_the_workflow_may_call_must_be_configured() -> Result<(), BoxError> {
    let fixture = fixture(
        "\n[[workflow.permitted_effects]]\nmcp_server_name = \"crm\"\ntool = \"list_contacts\"\n",
        "()",
    )
    .await?;

    let validation = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
    )
    .await;

    assert_eq!(codes(&validation), ["validation/server-not-configured"]);
    assert!(validation.report.failures[0].message.contains("crm"));
    Ok(())
}

#[tokio::test]
async fn a_tool_the_server_does_not_offer_is_reported_by_listing_the_servers_tools() -> Result<(), BoxError> {
    let fixture = fixture(
        "\n[[workflow.permitted_effects]]\nmcp_server_name = \"contextos\"\ntool = \"no_such_tool\"\n",
        "()",
    )
    .await?;

    let validation = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
    )
    .await;

    assert_eq!(codes(&validation), ["validation/tool-not-found"]);
    assert!(validation.report.failures[0].message.contains("no_such_tool"));
    Ok(())
}

#[tokio::test]
async fn a_server_that_is_configured_but_cannot_be_reached_is_unverifiable_not_a_defect() -> Result<(), BoxError> {
    let fixture = fixture(
        "\n[[workflow.permitted_effects]]\nmcp_server_name = \"crm\"\ntool = \"list_contacts\"\n",
        "()",
    )
    .await?;

    let validation = validate_with(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
        &["contextos".to_owned(), "crm".to_owned()],
    )
    .await;

    assert!(validation.report.failures.is_empty(), "{:?}", validation.report);
    assert_eq!(validation.report.unverifiable.len(), 1);
    assert!(!validation.report.passed(), "an unchecked tool is not a pass");
    Ok(())
}

#[tokio::test]
async fn every_failure_is_reported_together_not_just_the_first() -> Result<(), BoxError> {
    let fixture = fixture(
        "\n[[workflow.inputs]]\nname = \"since\"\nkind = \"date\"\n\n[[workflow.permitted_effects]]\nmcp_server_name = \"contextos\"\ntool = \"no_such_tool\"\n",
        "let y = (2 +;",
    )
    .await?;

    let validation = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
    )
    .await;

    let found = codes(&validation);
    for expected in [
        "validation/script-syntax",
        "validation/input-invalid",
        "validation/tool-not-found",
    ] {
        assert!(found.contains(&expected), "{expected} missing from {found:?}");
    }
    Ok(())
}

#[tokio::test]
async fn validation_never_runs_the_script() -> Result<(), BoxError> {
    let marker = tempfile::tempdir()?;
    let target = marker.path().join("written.md");
    let script = format!(
        "call_mcp(\"contextos\", \"fs_write_file\", #{{ path: {:?}, content: \"x\" }});",
        target.display().to_string()
    );
    let fixture = fixture(
        "\n[[workflow.permitted_effects]]\nmcp_server_name = \"contextos\"\ntool = \"fs_write_file\"\n",
        &script,
    )
    .await?;

    let validation = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
    )
    .await;

    assert!(validation.report.passed(), "{:?}", validation.report);
    assert!(!Path::new(&target).exists());
    Ok(())
}

#[tokio::test]
async fn the_version_changes_with_the_job_file_and_not_with_the_ready_key() -> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;

    let base = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
    )
    .await
    .fingerprint;
    let renamed = validate(
        &fixture,
        &job("name = \"Nightly, renamed\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
    )
    .await
    .fingerprint;
    let ready = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nready = true\n"),
    )
    .await
    .fingerprint;

    assert_ne!(base, renamed);
    assert_eq!(base, ready);
    Ok(())
}

/// Rewrites the installed `acme` 1.0.0 to depend on `acme-text` 1.x and declare one module of its own, and installs
/// `acme-text` 1.2.0 exporting `strings` and keeping `private`.
fn depend_on_a_library(fixture: &Fixture) -> Result<(), BoxError> {
    let dir = fixture.store.path().join("acme").join("1.0.0");
    std::fs::write(
        dir.join("manifest.toml"),
        format!(
            "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\nmodules = [\"util.rhai\"]\n[[dependencies]]\nname = \"acme-text\"\nrequirement = \"^1\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n"
        ),
    )?;
    std::fs::write(dir.join("util.rhai"), "fn who() { \"acme\" }")?;
    let library = fixture.store.path().join("acme-text").join("1.2.0");
    std::fs::create_dir_all(&library)?;
    std::fs::write(
        library.join("manifest.toml"),
        format!(
            "name = \"acme-text\"\nversion = \"1.2.0\"\ndigest = \"{DIGEST}\"\nmodules = [\"strings.rhai\", \"private.rhai\", \"util.rhai\"]\nexports = [\"strings.rhai\", \"util.rhai\"]\n"
        ),
    )?;
    std::fs::write(
        library.join("strings.rhai"),
        "import \"util\" as util;\nfn shout() { util::who() }",
    )?;
    std::fs::write(library.join("util.rhai"), "fn who() { \"text\" }")?;
    std::fs::write(library.join("private.rhai"), "fn hidden() { 1 }")?;
    Ok(())
}

#[tokio::test]
async fn a_script_importing_an_export_of_a_library_it_depends_on_passes_and_has_a_version() -> Result<(), BoxError> {
    let fixture = fixture(
        "",
        "import \"util\" as mine;\nimport \"acme-text:strings\" as s;\ns::shout() + mine::who()",
    )
    .await?;
    depend_on_a_library(&fixture)?;

    let validation = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
    )
    .await;

    assert!(validation.report.passed(), "{:?}", validation.report);
    assert!(validation.fingerprint.is_some());
    Ok(())
}

#[tokio::test]
async fn an_import_of_a_module_a_library_keeps_private_is_reported_at_its_line() -> Result<(), BoxError> {
    let fixture = fixture("", "let x = 1;\nimport \"acme-text:private\" as p;\np::hidden()").await?;
    depend_on_a_library(&fixture)?;

    let validation = validate(
        &fixture,
        &job("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
    )
    .await;

    assert_eq!(codes(&validation), ["validation/script-syntax"]);
    let failure = &validation.report.failures[0];
    assert_eq!(failure.artefact, "sync.rhai");
    assert!(failure.message.contains("does not export"), "{}", failure.message);
    assert!(failure.message.contains("line 2"), "{}", failure.message);
    Ok(())
}

const NIGHTLY: &str = "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";

/// Installs a library or package `name` `version` that exports `module` (and declares it), depending on each of
/// `dependencies` as `(name, requirement)`.
fn install_library(
    fixture: &Fixture,
    name: &str,
    version: &str,
    dependencies: &[(&str, &str)],
    module: (&str, &str),
) -> Result<(), BoxError> {
    let dir = fixture.store.path().join(name).join(version);
    std::fs::create_dir_all(&dir)?;
    let mut manifest = format!(
        "name = \"{name}\"\nversion = \"{version}\"\ndigest = \"{DIGEST}\"\nmodules = [\"{0}.rhai\"]\nexports = [\"{0}.rhai\"]\n",
        module.0
    );
    for (dependency, requirement) in dependencies {
        let _ = write!(
            manifest,
            "[[dependencies]]\nname = \"{dependency}\"\nrequirement = \"{requirement}\"\n"
        );
    }
    std::fs::write(dir.join("manifest.toml"), manifest)?;
    std::fs::write(dir.join(format!("{}.rhai", module.0)), module.1)?;
    Ok(())
}

/// Makes the installed `acme` 1.0.0 depend on each of `dependencies` as `(name, requirement)`.
fn acme_depends_on(fixture: &Fixture, dependencies: &[(&str, &str)]) -> Result<(), BoxError> {
    let dir = fixture.store.path().join("acme").join("1.0.0");
    let mut manifest = format!("name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\n");
    for (dependency, requirement) in dependencies {
        let _ = write!(
            manifest,
            "[[dependencies]]\nname = \"{dependency}\"\nrequirement = \"{requirement}\"\n"
        );
    }
    manifest.push_str("\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n");
    std::fs::write(dir.join("manifest.toml"), manifest)?;
    Ok(())
}

fn first_failure(validation: &JobValidation) -> Result<&contextos_workflows::ValidationFailure, BoxError> {
    validation
        .report
        .failures
        .first()
        .ok_or_else(|| "the validation should have failed".into())
}

#[tokio::test]
async fn a_library_that_is_not_installed_is_a_missing_artefact_and_leaves_the_job_without_a_version()
-> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;
    depend_on_a_library(&fixture)?;
    std::fs::remove_dir_all(fixture.store.path().join("acme-text"))?;

    let validation = validate(&fixture, &job(NIGHTLY)).await;

    assert_eq!(codes(&validation), ["validation/artefact-missing"]);
    let message = &first_failure(&validation)?.message;
    assert!(message.contains("acme-text"), "{message}");
    assert!(message.contains("is not installed"), "{message}");
    assert!(message.contains("acme 1.0.0 requires ^1"), "{message}");
    assert!(
        validation.fingerprint.is_none(),
        "a job whose libraries do not resolve has no version"
    );
    Ok(())
}

#[tokio::test]
async fn a_requirement_that_excludes_a_library_already_chosen_names_the_library_and_every_requirer()
-> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;
    acme_depends_on(&fixture, &[("a-text", "^1"), ("b-extra", "^1")])?;
    install_library(&fixture, "a-text", "1.0.0", &[], ("t", "fn t() { 1 }"))?;
    install_library(&fixture, "a-text", "1.2.0", &[], ("t", "fn t() { 2 }"))?;
    install_library(
        &fixture,
        "b-extra",
        "1.0.0",
        &[("a-text", "=1.0.0")],
        ("e", "fn e() { 1 }"),
    )?;

    let validation = validate(&fixture, &job(NIGHTLY)).await;

    assert_eq!(codes(&validation), ["validation/artefact-missing"]);
    let message = &first_failure(&validation)?.message;
    for expected in [
        "a-text",
        "already chosen at 1.2.0",
        "acme 1.0.0 requires ^1",
        "b-extra 1.0.0 requires =1.0.0",
    ] {
        assert!(message.contains(expected), "{expected}: {message}");
    }
    assert!(validation.fingerprint.is_none());
    Ok(())
}

#[tokio::test]
async fn a_library_release_whose_manifest_does_not_parse_is_an_invalid_artefact_naming_the_library()
-> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;
    depend_on_a_library(&fixture)?;
    std::fs::write(
        fixture
            .store
            .path()
            .join("acme-text")
            .join("1.2.0")
            .join("manifest.toml"),
        "this is not a manifest",
    )?;

    let validation = validate(&fixture, &job(NIGHTLY)).await;

    assert_eq!(codes(&validation), ["validation/artefact-invalid"]);
    let message = &first_failure(&validation)?.message;
    assert!(message.contains("acme-text"), "{message}");
    assert!(
        validation.fingerprint.is_none(),
        "a job whose libraries cannot be read has no version"
    );
    Ok(())
}

#[tokio::test]
async fn a_requirement_no_installed_release_satisfies_names_the_library_the_requirer_and_what_is_installed()
-> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;
    acme_depends_on(&fixture, &[("a-text", "^2")])?;
    install_library(&fixture, "a-text", "1.0.0", &[], ("t", "fn t() { 1 }"))?;

    let validation = validate(&fixture, &job(NIGHTLY)).await;

    assert_eq!(codes(&validation), ["validation/artefact-missing"]);
    let message = &first_failure(&validation)?.message;
    for expected in [
        "no installed release of library \"a-text\"",
        "acme 1.0.0 requires ^2",
        "installed: 1.0.0",
    ] {
        assert!(message.contains(expected), "{expected}: {message}");
    }
    assert!(validation.fingerprint.is_none());
    Ok(())
}

#[tokio::test]
async fn libraries_that_depend_on_each_other_are_a_dependency_cycle() -> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;
    acme_depends_on(&fixture, &[("lib-a", "^1")])?;
    install_library(&fixture, "lib-a", "1.0.0", &[("lib-b", "^1")], ("a", "fn a() { 1 }"))?;
    install_library(&fixture, "lib-b", "1.0.0", &[("lib-a", "^1")], ("b", "fn b() { 1 }"))?;

    let validation = validate(&fixture, &job(NIGHTLY)).await;

    assert_eq!(codes(&validation), ["validation/artefact-invalid"]);
    let message = &first_failure(&validation)?.message;
    assert!(message.contains("lib-a") && message.contains("lib-b"), "{message}");
    assert!(validation.fingerprint.is_none());
    Ok(())
}

#[tokio::test]
async fn a_chain_of_libraries_past_eight_levels_names_the_limit() -> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;
    acme_depends_on(&fixture, &[("lib-1", "^1")])?;
    for level in 1..=9 {
        let next = format!("lib-{}", level + 1);
        let dependencies: Vec<(&str, &str)> = if level == 9 {
            Vec::new()
        } else {
            vec![(next.as_str(), "^1")]
        };
        install_library(
            &fixture,
            &format!("lib-{level}"),
            "1.0.0",
            &dependencies,
            ("m", "fn m() { 1 }"),
        )?;
    }

    let validation = validate(&fixture, &job(NIGHTLY)).await;

    assert_eq!(codes(&validation), ["validation/artefact-invalid"]);
    let message = &first_failure(&validation)?.message;
    assert!(message.contains("8 levels"), "{message}");
    Ok(())
}

#[tokio::test]
async fn more_than_thirty_two_libraries_name_the_limit() -> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;
    let names: Vec<String> = (0..33).map(|index| format!("lib-{index:02}")).collect();
    let dependencies: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "^1")).collect();
    acme_depends_on(&fixture, &dependencies)?;
    for name in &names {
        install_library(&fixture, name, "1.0.0", &[], ("m", "fn m() { 1 }"))?;
    }

    let validation = validate(&fixture, &job(NIGHTLY)).await;

    assert_eq!(codes(&validation), ["validation/artefact-invalid"]);
    let message = &first_failure(&validation)?.message;
    assert!(message.contains("32 library packages"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_syntax_error_in_a_library_module_the_job_reaches_is_reported_against_that_library_file()
-> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;
    depend_on_a_library(&fixture)?;
    std::fs::write(
        fixture
            .store
            .path()
            .join("acme-text")
            .join("1.2.0")
            .join("strings.rhai"),
        "fn shout() {\n  let = ;\n}",
    )?;

    let validation = validate(&fixture, &job(NIGHTLY)).await;

    assert_eq!(codes(&validation), ["validation/script-syntax"]);
    let failure = first_failure(&validation)?;
    assert_eq!(failure.artefact, "acme-text:strings.rhai");
    assert!(failure.message.contains("line 2"), "{}", failure.message);
    Ok(())
}

#[tokio::test]
async fn a_library_module_importing_a_package_its_manifest_does_not_name_is_reported() -> Result<(), BoxError> {
    let fixture = fixture("", "()").await?;
    depend_on_a_library(&fixture)?;
    install_library(&fixture, "other", "1.0.0", &[], ("o", "fn o() { 1 }"))?;
    std::fs::write(
        fixture
            .store
            .path()
            .join("acme-text")
            .join("1.2.0")
            .join("strings.rhai"),
        "import \"other:o\" as o;\nfn shout() { o::o() }",
    )?;

    let validation = validate(&fixture, &job(NIGHTLY)).await;

    assert_eq!(codes(&validation), ["validation/script-syntax"]);
    let failure = first_failure(&validation)?;
    assert_eq!(failure.artefact, "acme-text:strings.rhai");
    assert!(failure.message.contains("libraries"), "{}", failure.message);
    Ok(())
}

/// Installs library `acme-text` 1.2.0 whose `strings` module holds `module` and whose `library.toml` declares `tools`
/// (the body of an `mcp_tools` array), and makes `acme` depend on it.
fn depend_on_a_library_declaring(
    fixture: &Fixture,
    workflow_extra: &str,
    tools: &str,
    module: &str,
) -> Result<(), BoxError> {
    std::fs::write(
        fixture.store.path().join("acme").join("1.0.0").join("manifest.toml"),
        format!(
            "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\n[[dependencies]]\nname = \"acme-text\"\nrequirement = \"^1\"\n\n\
             [[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n{workflow_extra}"
        ),
    )?;
    install_library(fixture, "acme-text", "1.2.0", &[], ("strings", module))?;
    let dir = fixture.store.path().join("acme-text").join("1.2.0");
    std::fs::write(
        dir.join("library.toml"),
        format!("modules = [\"strings.rhai\"]\nexports = [\"strings.rhai\"]\nmcp_tools = [{tools}]\n"),
    )?;
    Ok(())
}

const IMPORTS_STRINGS: &str = "import \"acme-text:strings\" as s;\ns::act()";

#[tokio::test]
async fn a_shared_workflow_is_failed_for_a_library_tool_its_lists_do_not_permit() -> Result<(), BoxError> {
    let fixture = fixture(READS, IMPORTS_STRINGS).await?;
    depend_on_a_library_declaring(
        &fixture,
        READS,
        "{ server = \"crm\", tool = \"find\" }",
        "fn act() { call_mcp(\"crm\", \"find\", #{}) }",
    )?;

    let validation = validate(&fixture, &job(NIGHTLY)).await;

    assert_eq!(codes(&validation), ["validation/artefact-invalid"]);
    let failure = first_failure(&validation)?;
    assert_eq!(failure.artefact, "acme-text:library.toml");
    assert!(
        failure.message.contains("crm") && failure.message.contains("find"),
        "{}",
        failure.message
    );
    Ok(())
}

#[tokio::test]
async fn a_shared_workflow_is_failed_for_a_literal_call_its_library_does_not_declare() -> Result<(), BoxError> {
    let permits_both = format!(
        "{READS}\n[[workflow.permitted_effects]]\nmcp_server_name = \"contextos\"\ntool = \"fs_list_allowed_directories\"\n"
    );
    let fixture = fixture(&permits_both, IMPORTS_STRINGS).await?;
    depend_on_a_library_declaring(
        &fixture,
        &permits_both,
        "{ server = \"contextos\", tool = \"vault_info\" }",
        "fn act() { call_mcp(\"contextos\", \"fs_list_allowed_directories\", #{}) }",
    )?;

    let validation = validate(&fixture, &job(NIGHTLY)).await;

    assert_eq!(
        codes(&validation),
        ["validation/artefact-invalid"],
        "{:?}",
        validation.report
    );
    let failure = first_failure(&validation)?;
    assert_eq!(failure.artefact, "acme-text:library.toml");
    assert!(
        failure.message.contains("fs_list_allowed_directories"),
        "{}",
        failure.message
    );
    Ok(())
}

#[tokio::test]
async fn a_shared_workflow_that_restricts_a_server_fails_a_library_that_declares_every_tool_on_it()
-> Result<(), BoxError> {
    let fixture = fixture(READS, IMPORTS_STRINGS).await?;
    depend_on_a_library_declaring(
        &fixture,
        READS,
        "{ server = \"contextos\", tool = \"*\" }",
        "fn act(tool) { call_mcp(\"contextos\", tool, #{}) }",
    )?;

    let validation = validate(&fixture, &job(NIGHTLY)).await;

    assert_eq!(codes(&validation), ["validation/artefact-invalid"]);
    let failure = first_failure(&validation)?;
    assert_eq!(failure.artefact, "acme-text:library.toml");
    assert!(failure.message.contains("every tool"), "{}", failure.message);
    Ok(())
}
