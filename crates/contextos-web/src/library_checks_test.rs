use std::collections::BTreeMap;
use std::fmt::Write as _;

use contextos_core::ContentHash;
use contextos_workflows::{
    LibraryManifest, PackageCode, PackageKind, PackageName, PackageToRead, ScriptParts, WorkflowFile, WorkflowScript,
    WorkflowsFile,
};

use super::*;

/// One package of a release: its name, the modules it holds with their text, what it exports, and what it depends on.
struct Part<'a> {
    name: &'a str,
    modules: &'a [(&'a str, &'a str)],
    exports: &'a [&'a str],
    dependencies: &'a [&'a str],
    /// Whether it is the workflow's own package.
    workflow: bool,
}

fn quoted(items: impl IntoIterator<Item = String>) -> String {
    format!(
        "[{}]",
        items
            .into_iter()
            .map(|item| format!("{item:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn code_of(part: &Part<'_>) -> Result<PackageCode, Box<dyn std::error::Error>> {
    let mut libraries = String::new();
    for dependency in part.dependencies {
        let _ = write!(libraries, "[[libraries]]\nname = {dependency:?}\nrequirement = \"*\"\n");
    }
    let modules = quoted(part.modules.iter().map(|(path, _)| (*path).to_owned()));
    // A package that exports is a library; the workflow's own package holds a workflow.
    let kind = if part.workflow {
        PackageKind::Workflows(WorkflowsFile::try_from(
            format!("workflows = [\"sync\"]\nmodules = {modules}\n{libraries}").as_str(),
        )?)
    } else {
        let exports = quoted(part.exports.iter().map(|path| (*path).to_owned()));
        PackageKind::Library(LibraryManifest::try_from(
            format!("modules = {modules}\nexports = {exports}\n{libraries}").as_str(),
        )?)
    };
    let manifest = PackageToRead {
        name: PackageName::try_from(part.name)?,
        kind,
    };
    let held: BTreeMap<String, &str> = part
        .modules
        .iter()
        .map(|(path, module)| ((*path).to_owned(), *module))
        .collect();
    Ok(PackageCode::read(manifest, |path| {
        let content = held.get(path.as_str()).copied().unwrap_or_default().to_owned();
        Ok(WorkflowFile {
            content_hash: ContentHash::from([0; 32]),
            content,
        })
    })?)
}

/// The code of a workflow release named `sync` with the entry script `entry`, and the libraries `parts` hold.
fn script_of(entry: &str, parts: &[Part<'_>]) -> Result<WorkflowScript, Box<dyn std::error::Error>> {
    let mut packages = BTreeMap::new();
    for part in parts {
        let code = code_of(part)?;
        packages.insert(code.name().clone(), code);
    }
    Ok(WorkflowScript::from(ScriptParts {
        entry: entry.to_owned(),
        package: Some(PackageName::try_from("sync")?),
        packages,
    }))
}

fn workflow<'a>(modules: &'a [(&'a str, &'a str)], dependencies: &'a [&'a str]) -> Part<'a> {
    Part {
        name: "sync",
        modules,
        exports: &[],
        dependencies,
        workflow: true,
    }
}

fn library<'a>(name: &'a str, modules: &'a [(&'a str, &'a str)], dependencies: &'a [&'a str]) -> Part<'a> {
    Part {
        name,
        modules,
        exports: &["client.rhai"],
        dependencies,
        workflow: false,
    }
}

/// The tools `library` declares, each written `server.tool`.
fn servers_of(library: &str, tools: &[&str]) -> BTreeMap<PackageName, Vec<ToolRef>> {
    PackageName::try_from(library)
        .into_iter()
        .map(|name| {
            let declared = tools
                .iter()
                .map(|entry| {
                    let (server, tool) = entry.split_once('.').unwrap_or((entry, ""));
                    ToolRef {
                        server: server.to_owned(),
                        tool: tool.to_owned(),
                    }
                })
                .collect();
            (name, declared)
        })
        .collect()
}

fn declared(servers: &[&str]) -> CallRules {
    CallRules::try_from(contextos_workflows::call_rules::CallLists {
        server_allow: Some(servers.iter().map(|server| (*server).to_owned()).collect()),
        ..Default::default()
    })
    .unwrap_or_default()
}

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CLIENT: &[(&str, &str)] = &[("client.rhai", "fn act() { 1 }")];

#[test]
fn a_tool_an_imported_library_declares_and_the_workflow_does_not_permit_is_a_failure_against_the_library() -> TestResult
{
    let script = script_of(
        "import \"crm:client\" as c;",
        &[workflow(&[], &["crm"]), library("crm", CLIENT, &[])],
    )?;

    let failures = release_library_tool_failures(
        &script,
        "sync.rhai",
        &servers_of("crm", &["billing.charge"]),
        &declared(&["contextos"]),
    );

    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].artefact, "crm:library.toml");
    assert!(failures[0].message.contains("crm") && failures[0].message.contains("billing"));
    Ok(())
}

#[test]
fn a_server_the_workflow_declares_is_no_failure() -> TestResult {
    let script = script_of(
        "import \"crm:client\" as c;",
        &[workflow(&[], &["crm"]), library("crm", CLIENT, &[])],
    )?;

    let failures = release_library_tool_failures(
        &script,
        "sync.rhai",
        &servers_of("crm", &["billing.charge"]),
        &declared(&["billing"]),
    );

    assert_eq!(failures.len(), 0);
    Ok(())
}

#[test]
fn a_library_nothing_imports_from_adds_nothing() -> TestResult {
    let script = script_of("()", &[workflow(&[], &["crm"]), library("crm", CLIENT, &[])])?;

    let failures = release_library_tool_failures(
        &script,
        "sync.rhai",
        &servers_of("crm", &["billing.charge"]),
        &declared(&[]),
    );

    assert_eq!(failures.len(), 0);
    Ok(())
}

#[test]
fn a_library_imported_only_from_a_module_of_the_workflow_is_found() -> TestResult {
    let helpers = [("helpers.rhai", "import \"crm:client\" as c;\nfn act() { c::act() }")];
    let script = script_of(
        "import \"helpers\" as h;",
        &[
            Part {
                modules: &helpers,
                ..workflow(&[], &["crm"])
            },
            library("crm", CLIENT, &[]),
        ],
    )?;

    let failures = release_library_tool_failures(
        &script,
        "sync.rhai",
        &servers_of("crm", &["billing.charge"]),
        &declared(&[]),
    );

    assert_eq!(failures.len(), 1, "{failures:?}");
    Ok(())
}

#[test]
fn a_library_named_only_in_a_comment_or_a_string_is_not_imported_from() -> TestResult {
    let script = script_of(
        "// import \"crm:client\" as c;\nlet note = \"crm:client\";",
        &[workflow(&[], &["crm"]), library("crm", CLIENT, &[])],
    )?;

    let failures = release_library_tool_failures(
        &script,
        "sync.rhai",
        &servers_of("crm", &["billing.charge"]),
        &declared(&[]),
    );

    assert_eq!(failures.len(), 0, "{failures:?}");
    Ok(())
}

#[test]
fn a_library_a_library_imports_from_is_checked_in_turn() -> TestResult {
    let crm = [(
        "client.rhai",
        "import \"http:client\" as http;\nfn act() { http::act() }",
    )];
    let script = script_of(
        "import \"crm:client\" as c;",
        &[
            workflow(&[], &["crm"]),
            library("crm", &crm, &["http"]),
            library("http", CLIENT, &[]),
        ],
    )?;
    let mut servers = servers_of("crm", &[]);
    servers.extend(servers_of("http", &["web.get"]));

    let failures = release_library_tool_failures(&script, "sync.rhai", &servers, &declared(&[]));

    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].artefact, "http:library.toml");
    Ok(())
}

#[test]
fn a_call_to_a_tool_the_library_does_not_declare_is_a_failure_against_the_library_file() -> TestResult {
    let calls = [("client.rhai", "fn act() { call_mcp(\"crm\", \"find\", #{}) }")];
    let script = script_of(
        "import \"crm:client\" as c;",
        &[workflow(&[], &["crm"]), library("crm", &calls, &[])],
    )?;

    let failures = release_library_tool_failures(&script, "sync.rhai", &servers_of("crm", &[]), &declared(&["crm"]));

    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].artefact, "crm:library.toml");
    Ok(())
}
