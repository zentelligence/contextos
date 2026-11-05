use super::*;

use contextos_packages::ReleaseFilePath;
use contextos_workflows::ValidationCode;

use crate::source_install_world::World;

type BoxError = Box<dyn std::error::Error>;

const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\n";

fn workflows(name: &str) -> Result<SourceTarget, BoxError> {
    Ok(SourceTarget {
        kind: SourceKind::Workflows,
        name: PackageName::try_from(name)?,
    })
}

fn library(name: &str) -> Result<SourceTarget, BoxError> {
    Ok(SourceTarget {
        kind: SourceKind::Library,
        name: PackageName::try_from(name)?,
    })
}

fn paths(package: &SourcePackage) -> Vec<String> {
    package.files.0.keys().map(ToString::to_string).collect()
}

fn text_of(package: &SourcePackage, path: &str) -> Result<String, BoxError> {
    let key = ReleaseFilePath::try_from(path)?;
    let bytes = package.files.0.get(&key).ok_or("no such file")?;
    Ok(String::from_utf8(bytes.clone())?)
}

async fn refusal(world: &World, target: &SourceTarget) -> Result<(String, String), BoxError> {
    let client = world.session().await?;
    match read_source(&client, "vault", target).await {
        Err(error @ SourceInstallError::Invalid { .. }) => {
            let SourceInstallError::Invalid { failures } = &error else {
                return Err("not invalid".into());
            };
            Ok((failures[0].code.as_str().to_owned(), failures[0].artefact.clone()))
        }
        Err(other) => Err(other.into()),
        Ok(_) => Err("the source was read".into()),
    }
}

fn lay_out_package(world: &World) -> Result<(), BoxError> {
    world.write(
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\"]\nmodules = [\"shared/dates.rhai\"]\ntemplates = [\"templates\"]\n",
    )?;
    world.write("registry/workflows/ops/sync/workflow.toml", WORKFLOW)?;
    world.write("registry/workflows/ops/sync/sync.rhai", "let x = 1;")?;
    world.write("registry/workflows/ops/shared/dates.rhai", "fn today() { 1 }")?;
    world.write("registry/workflows/ops/templates/report.md", "# Report")?;
    world.write("registry/workflows/ops/templates/more/extra.md", "extra")?;
    world.write("registry/workflows/ops/sync/jobs/weekly.toml", "version = \"1.0.0\"\n")?;
    world.write("registry/workflows/ops/sync/tests/one.toml", "x = 1\n")?;
    Ok(())
}

#[tokio::test]
async fn a_workflow_package_is_read_with_its_workflows_scripts_modules_and_templates() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_package(&world)?;
    let client = world.session().await?;

    let snapshot = read_source(&client, "vault", &workflows("ops")?).await?;

    assert_eq!(
        paths(&snapshot.root),
        [
            "shared/dates.rhai",
            "sync/sync.rhai",
            "sync/workflow.toml",
            "templates/more/extra.md",
            "templates/report.md",
            "workflows.toml"
        ]
    );
    assert_eq!(text_of(&snapshot.root, "sync/sync.rhai")?, "let x = 1;");
    assert!(snapshot.libraries.is_empty());
    Ok(())
}

#[tokio::test]
async fn a_library_that_states_a_requirement_is_not_read_from_source() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_package(&world)?;
    world.write(
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\"]\n\n[[libraries]]\nname = \"crm\"\nrequirement = \"^1.0\"\n",
    )?;
    let client = world.session().await?;

    let snapshot = read_source(&client, "vault", &workflows("ops")?).await?;

    assert!(
        snapshot.libraries.is_empty(),
        "no registry/libraries/crm exists, and none was asked for"
    );
    assert_eq!(
        snapshot.root.libraries,
        [SourceLibrary {
            name: PackageName::try_from("crm")?,
            requirement: Some(VersionReq::parse("^1.0")?),
        }]
    );
    Ok(())
}

#[tokio::test]
async fn a_library_without_a_requirement_is_read_from_source_and_so_are_the_libraries_it_names() -> Result<(), BoxError>
{
    let world = World::new()?;
    lay_out_package(&world)?;
    world.write(
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\"]\n\n[[libraries]]\nname = \"crm\"\n",
    )?;
    world.write(
        "registry/libraries/crm/library.toml",
        "modules = [\"crm.rhai\"]\nexports = [\"crm.rhai\"]\n\n[[libraries]]\nname = \"base\"\n",
    )?;
    world.write("registry/libraries/crm/crm.rhai", "fn contacts() { 1 }")?;
    world.write(
        "registry/libraries/base/library.toml",
        "modules = [\"base.rhai\"]\nexports = [\"base.rhai\"]\n",
    )?;
    world.write("registry/libraries/base/base.rhai", "fn id() { 1 }")?;
    let client = world.session().await?;

    let snapshot = read_source(&client, "vault", &workflows("ops")?).await?;

    let names: Vec<String> = snapshot.libraries.keys().map(ToString::to_string).collect();
    assert_eq!(names, ["base", "crm"]);
    let crm = &snapshot.libraries[&PackageName::try_from("crm")?];
    assert_eq!(paths(crm), ["crm.rhai", "library.toml"]);
    assert_eq!(crm.libraries[0].requirement, None);
    Ok(())
}

#[tokio::test]
async fn a_library_is_read_on_its_own() -> Result<(), BoxError> {
    let world = World::new()?;
    world.write(
        "registry/libraries/crm/library.toml",
        "modules = [\"crm.rhai\"]\nexports = [\"crm.rhai\"]\n",
    )?;
    world.write("registry/libraries/crm/crm.rhai", "fn contacts() { 1 }")?;
    let client = world.session().await?;

    let snapshot = read_source(&client, "vault", &library("crm")?).await?;

    assert_eq!(snapshot.root.kind, SourceKind::Library);
    assert_eq!(paths(&snapshot.root), ["crm.rhai", "library.toml"]);
    Ok(())
}

#[tokio::test]
async fn a_package_without_a_workflows_file_is_refused_as_a_missing_artefact() -> Result<(), BoxError> {
    let world = World::new()?;
    world.write("registry/workflows/ops/sync/workflow.toml", WORKFLOW)?;

    let (code, artefact) = refusal(&world, &workflows("ops")?).await?;

    assert_eq!(code, ValidationCode::ArtefactMissing.as_str());
    assert_eq!(artefact, "workflows.toml");
    Ok(())
}

#[tokio::test]
async fn a_listed_workflow_without_a_workflow_file_is_refused_naming_the_file() -> Result<(), BoxError> {
    let world = World::new()?;
    world.write(
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\", \"update\"]\n",
    )?;
    world.write("registry/workflows/ops/sync/workflow.toml", WORKFLOW)?;
    world.write("registry/workflows/ops/sync/sync.rhai", "let x = 1;")?;

    let (code, artefact) = refusal(&world, &workflows("ops")?).await?;

    assert_eq!(code, ValidationCode::ArtefactMissing.as_str());
    assert_eq!(artefact, "update/workflow.toml");
    Ok(())
}

#[tokio::test]
async fn a_missing_entry_script_is_refused_naming_it() -> Result<(), BoxError> {
    let world = World::new()?;
    world.write("registry/workflows/ops/workflows.toml", "workflows = [\"sync\"]\n")?;
    world.write("registry/workflows/ops/sync/workflow.toml", WORKFLOW)?;

    let (code, artefact) = refusal(&world, &workflows("ops")?).await?;

    assert_eq!(code, ValidationCode::ArtefactMissing.as_str());
    assert_eq!(artefact, "sync/sync.rhai");
    Ok(())
}

#[tokio::test]
async fn a_declared_module_that_is_missing_is_refused_naming_it() -> Result<(), BoxError> {
    let world = World::new()?;
    world.write(
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\"]\nmodules = [\"lib/gone.rhai\"]\n",
    )?;
    world.write("registry/workflows/ops/sync/workflow.toml", WORKFLOW)?;
    world.write("registry/workflows/ops/sync/sync.rhai", "let x = 1;")?;

    let (code, artefact) = refusal(&world, &workflows("ops")?).await?;

    assert_eq!(code, ValidationCode::ArtefactMissing.as_str());
    assert_eq!(artefact, "lib/gone.rhai");
    Ok(())
}

#[tokio::test]
async fn a_declared_template_that_is_missing_is_refused_naming_it() -> Result<(), BoxError> {
    let world = World::new()?;
    world.write(
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\"]\ntemplates = [\"templates\"]\n",
    )?;
    world.write("registry/workflows/ops/sync/workflow.toml", WORKFLOW)?;
    world.write("registry/workflows/ops/sync/sync.rhai", "let x = 1;")?;

    let (code, artefact) = refusal(&world, &workflows("ops")?).await?;

    assert_eq!(code, ValidationCode::ArtefactMissing.as_str());
    assert_eq!(artefact, "templates");
    Ok(())
}

#[tokio::test]
async fn a_workflows_file_that_does_not_parse_is_refused_as_an_invalid_artefact() -> Result<(), BoxError> {
    let world = World::new()?;
    world.write("registry/workflows/ops/workflows.toml", "workflows = [")?;

    let (code, artefact) = refusal(&world, &workflows("ops")?).await?;

    assert_eq!(code, ValidationCode::ArtefactInvalid.as_str());
    assert_eq!(artefact, "workflows.toml");
    Ok(())
}

#[tokio::test]
async fn a_source_library_with_no_folder_is_refused_naming_its_library_file() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_package(&world)?;
    world.write(
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\"]\n\n[[libraries]]\nname = \"crm\"\n",
    )?;

    let (code, artefact) = refusal(&world, &workflows("ops")?).await?;

    assert_eq!(code, ValidationCode::ArtefactMissing.as_str());
    assert_eq!(artefact, "crm:library.toml");
    Ok(())
}

#[tokio::test]
async fn source_libraries_that_name_each_other_are_refused_as_a_loop() -> Result<(), BoxError> {
    let world = World::new()?;
    world.write(
        "registry/libraries/a/library.toml",
        "modules = [\"a.rhai\"]\nexports = [\"a.rhai\"]\n\n[[libraries]]\nname = \"b\"\n",
    )?;
    world.write("registry/libraries/a/a.rhai", "fn a() { 1 }")?;
    world.write(
        "registry/libraries/b/library.toml",
        "modules = [\"b.rhai\"]\nexports = [\"b.rhai\"]\n\n[[libraries]]\nname = \"a\"\n",
    )?;
    world.write("registry/libraries/b/b.rhai", "fn b() { 1 }")?;

    let (code, _) = refusal(&world, &library("a")?).await?;

    assert_eq!(code, ValidationCode::ArtefactInvalid.as_str());
    Ok(())
}

#[tokio::test]
async fn a_template_folder_holding_more_files_than_a_snapshot_allows_is_refused_before_any_is_read()
-> Result<(), BoxError> {
    let world = World::new()?;
    world.write(
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\"]\ntemplates = [\"templates\"]\n",
    )?;
    world.write("registry/workflows/ops/sync/workflow.toml", WORKFLOW)?;
    world.write("registry/workflows/ops/sync/sync.rhai", "let x = 1;")?;
    for number in 0..501 {
        world.write(&format!("registry/workflows/ops/templates/t{number}.md"), "x")?;
    }

    let (code, artefact) = refusal(&world, &workflows("ops")?).await?;

    assert_eq!(code, ValidationCode::ArtefactInvalid.as_str());
    assert_eq!(artefact, "templates");
    Ok(())
}
