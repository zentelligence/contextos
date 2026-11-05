use std::fmt::Write as _;

use super::*;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn manifest(body: &str, files: &[&str]) -> String {
    let listed = files.iter().fold(String::new(), |mut text, path| {
        let _ = write!(text, "\n[[files]]\npath = {path:?}\nhash = {HASH:?}\n");
        text
    });
    format!("name = \"sync-crm\"\nversion = \"1.2.0\"\ndigest = {DIGEST:?}\n{body}{listed}")
}

fn refusal(text: &str) -> String {
    match PackageManifest::parse(text) {
        Err(error) => error.to_string(),
        Ok(parsed) => format!("accepted: {parsed:?}"),
    }
}

#[test]
fn a_minimal_manifest_holds_its_identity_and_its_files() -> Result<(), Box<dyn std::error::Error>> {
    let parsed = PackageManifest::parse(&manifest("", &["workflows.toml"]))?;
    assert_eq!(parsed.identity.name, "sync-crm");
    assert_eq!(parsed.identity.version, Version::parse("1.2.0")?);
    assert_eq!(parsed.identity.digest.as_str(), DIGEST);
    assert_eq!(
        parsed.provenance,
        PackageProvenance::Uploaded {
            digest: ContentDigest::try_new(DIGEST)?
        }
    );
    Ok(())
}

#[test]
fn an_invalid_semver_version_is_rejected() {
    let text = manifest("", &["workflows.toml"]).replace("1.2.0", "not-a-version");
    let result = PackageManifest::parse(&text);
    assert!(matches!(result, Err(PackageError::InvalidVersion { .. })), "{result:?}");
}

#[test]
fn an_unknown_top_level_field_is_a_schema_violation() {
    let result = PackageManifest::parse(&manifest("surprise = true\n", &["workflows.toml"]));
    assert!(matches!(result, Err(PackageError::Malformed(_))), "{result:?}");
}

#[test]
fn manifest_keys_that_belong_to_other_files_are_unknown_keys() {
    for body in [
        "modules = [\"client.rhai\"]\n",
        "templates = [\"scaffold\"]\n",
        "exports = [\"client.rhai\"]\n",
        "[[dependencies]]\nname = \"shared-lib\"\nrequirement = \"^2.0\"\n",
        "[[libraries]]\nname = \"shared-lib\"\nrequirement = \"^2.0\"\n",
        "[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n",
        "binding_scopes = [\"single-vault\"]\n",
    ] {
        let found = refusal(&manifest(body, &["workflows.toml", "sync/workflow.toml"]));
        assert!(found.contains("unknown field"), "{body}: {found}");
    }
}

#[test]
fn a_release_that_lists_a_library_toml_is_a_library() -> Result<(), PackageError> {
    let parsed = PackageManifest::parse(&manifest("", &["library.toml", "client.rhai"]))?;
    assert!(parsed.is_library());
    Ok(())
}

#[test]
fn a_release_lists_exactly_one_of_workflows_toml_and_library_toml() {
    let neither = PackageManifest::parse(&manifest("", &["sync/workflow.toml", "notes.md"]));
    assert!(matches!(neither, Err(PackageError::NothingDeclared)), "{neither:?}");
    let both = PackageManifest::parse(&manifest("", &["workflows.toml", "library.toml"]));
    assert!(matches!(both, Err(PackageError::LibraryAndWorkflow)), "{both:?}");
}

#[test]
fn a_workflows_toml_below_the_top_level_does_not_make_a_workflow_package() {
    let found = PackageManifest::parse(&manifest("", &["sync/workflows.toml", "sync/library.toml"]));
    assert!(matches!(found, Err(PackageError::NothingDeclared)), "{found:?}");
}

#[test]
fn a_release_that_lists_a_workflows_toml_is_a_workflow_package() -> Result<(), PackageError> {
    let parsed = PackageManifest::parse(&manifest("", &["workflows.toml", "sync/workflow.toml"]))?;
    assert!(!parsed.is_library());
    Ok(())
}

#[test]
fn the_package_file_a_release_lists_is_named_by_its_kind() -> Result<(), PackageError> {
    let library = PackageManifest::parse(&manifest("", &["library.toml", "client.rhai"]))?;
    let workflows = PackageManifest::parse(&manifest("", &["workflows.toml", "sync/workflow.toml"]))?;
    assert_eq!(library.package_file_name(), "library.toml");
    assert_eq!(workflows.package_file_name(), "workflows.toml");
    Ok(())
}
