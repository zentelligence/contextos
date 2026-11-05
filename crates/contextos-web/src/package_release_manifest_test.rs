use super::*;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn manifest_text(name: &str, version: &str, extra: &str) -> String {
    format!(
        r#"
            name    = "{name}"
            version = "{version}"
            digest  = "{DIGEST}"

            [[files]]
            path = "workflows.toml"
            hash = "{HASH}"

            [[files]]
            path = "sync/workflow.toml"
            hash = "{HASH}"

            [[files]]
            path = "sync/sync.rhai"
            hash = "{HASH}"

            [[files]]
            path = "lib/util.rhai"
            hash = "{HASH}"
            {extra}
        "#
    )
}

const WORKFLOW: &str = r#"
name = "Sync"
entry = "sync.rhai"

[mcp_servers]
allow = ["crm"]
deny = ["mail"]

[mcp_tools]
allow = [{ server = "crm", tool = "update" }]
deny = [{ server = "crm", tool = "delete" }]

[[input]]
name = "when"
kind = "date"

[[output]]
name = "items"
kind = "array"
items = { kind = "string" }

[[required_tool_contracts]]
mcp_server_name = "crm"
tool = "list"
contract_version = "1"
"#;

const PACKAGE: &str = "workflows = [\"sync\"]\nmodules = [\"lib/util.rhai\"]\n";

fn files_of(workflow_toml: &str) -> ReleaseFiles {
    files_with(PACKAGE, workflow_toml)
}

fn files_with(package_toml: &str, workflow_toml: &str) -> ReleaseFiles {
    let held = |path: &str, text: &str| (ReleaseFilePath::try_from(path), text.as_bytes().to_vec());
    ReleaseFiles(
        [
            held("workflows.toml", package_toml),
            held("sync/workflow.toml", workflow_toml),
            held("sync/sync.rhai", "()"),
            held("lib/util.rhai", "fn x() { 1 }"),
        ]
        .into_iter()
        .filter_map(|(path, bytes)| path.ok().map(|path| (path, bytes)))
        .collect(),
    )
}

fn release() -> Result<PackageRelease, ManifestRejection> {
    PackageRelease::try_from(manifest_text("sync-crm", "1.2.0", "").as_str())
}

fn rejection(text: &str) -> Result<ManifestRejection, String> {
    match PackageRelease::try_from(text) {
        Ok(release) => Err(format!("expected a rejection, parsed {:?}", release.release())),
        Err(rejection) => Ok(rejection),
    }
}

#[test]
fn a_manifest_reads_as_the_release_it_names_with_its_listed_files() -> Result<(), ManifestRejection> {
    let release = PackageRelease::try_from(manifest_text("sync-crm", "1.2.0", "").as_str())?;

    assert_eq!(release.release().key.name.as_str(), "sync-crm");
    assert_eq!(release.release().key.version.to_string(), "1.2.0");
    assert_eq!(release.release().digest.as_str(), DIGEST);
    let listed: Vec<(String, &str)> = release
        .listed_files()
        .iter()
        .map(|file| (file.path.segments().join("/"), <&str>::from(&file.hash)))
        .collect();
    assert_eq!(
        listed,
        [
            ("workflows.toml".to_owned(), HASH),
            ("sync/workflow.toml".to_owned(), HASH),
            ("sync/sync.rhai".to_owned(), HASH),
            ("lib/util.rhai".to_owned(), HASH)
        ]
    );
    Ok(())
}

#[test]
fn the_interface_records_each_workflows_fields_tools_and_call_lists() -> Result<(), ManifestRejection> {
    let interface = release()?.interface(&files_of(WORKFLOW));

    assert_eq!(interface.exports, Vec::<String>::new());
    assert_eq!(interface.workflows.len(), 1);
    let workflow = &interface.workflows[0];
    assert_eq!(workflow.slug, "sync");
    assert_eq!(workflow.inputs[0].name, "when");
    assert_eq!(workflow.inputs[0].kind, InterfaceKind::Date);
    assert_eq!(
        workflow.outputs[0].kind,
        InterfaceKind::Array {
            items: Box::new(InterfaceKind::String)
        }
    );
    assert_eq!(workflow.tools[0].contract_version, "1");
    assert_eq!(workflow.server_allow.as_deref(), Some(["crm".to_owned()].as_slice()));
    assert_eq!(workflow.server_deny, ["mail"]);
    assert_eq!(workflow.tool_allow[0].tool, "update");
    assert_eq!(workflow.tool_deny[0].tool, "delete");
    Ok(())
}

#[test]
fn the_interface_survives_a_round_trip_through_the_ledger_encoding() -> Result<(), Box<dyn std::error::Error>> {
    let interface = release()
        .map_err(|rejection| rejection.message)?
        .interface(&files_of(WORKFLOW));

    let written = toml::to_string(&interface)?;
    let read: PackageInterface = toml::from_str(&written)?;

    assert_eq!(read, interface);
    Ok(())
}

#[test]
fn a_workflow_that_restricts_no_server_round_trips_with_the_allow_list_absent() -> Result<(), Box<dyn std::error::Error>>
{
    let interface = release()
        .map_err(|rejection| rejection.message)?
        .interface(&files_of("name = \"Sync\"\nentry = \"sync.rhai\"\n"));
    assert_eq!(interface.workflows[0].server_allow, None);

    let read: PackageInterface = toml::from_str(&toml::to_string(&interface)?)?;

    assert_eq!(read, interface);
    Ok(())
}

fn refusal_for(workflow_toml: &str, home: StoreHome) -> Result<ManifestRejection, String> {
    let release = release().map_err(|rejection| rejection.message)?;
    release
        .check_files(&files_of(workflow_toml), home)
        .err()
        .ok_or_else(|| "the files were accepted".to_owned())
}

#[test]
fn a_release_whose_workflow_toml_does_not_parse_is_refused_naming_the_file() -> Result<(), String> {
    let refused = refusal_for("name = \"Sync\"\n", StoreHome::Host)?;

    assert_eq!(refused.code, contextos_packages::codes::INVALID_MANIFEST);
    assert!(refused.message.contains("sync/workflow.toml"), "{}", refused.message);
    Ok(())
}

#[test]
fn a_workflow_toml_naming_a_slug_is_refused_as_an_unknown_key() -> Result<(), String> {
    let refused = refusal_for(
        "name = \"Sync\"\nentry = \"sync.rhai\"\nslug = \"other\"\n",
        StoreHome::Host,
    )?;

    assert!(refused.message.contains("unknown field `slug`"), "{}", refused.message);
    Ok(())
}

#[test]
fn a_workflow_with_a_vault_input_goes_into_the_host_store_and_is_refused_by_a_vault_store() -> Result<(), String> {
    let text = "name = \"Sync\"\nentry = \"sync.rhai\"\n[[input]]\nname = \"target\"\nkind = \"vault\"\n";
    let release = release().map_err(|rejection| rejection.message)?;
    release
        .check_files(&files_of(text), StoreHome::Host)
        .map_err(|rejection| rejection.message)?;

    let refused = refusal_for(text, StoreHome::Vault)?;

    assert_eq!(refused.code, VAULT_INPUT_IN_VAULT_STORE);
    assert!(refused.message.contains("sync/workflow.toml"), "{}", refused.message);
    assert!(refused.message.contains("target"), "{}", refused.message);
    Ok(())
}

#[test]
fn a_nested_vault_input_is_refused_by_a_vault_store_too() -> Result<(), String> {
    let text = "name = \"Sync\"\nentry = \"sync.rhai\"\n[[input]]\nname = \"targets\"\nkind = \"array\"\n[input.items]\nkind = \"vault\"\n";

    assert_eq!(refusal_for(text, StoreHome::Vault)?.code, VAULT_INPUT_IN_VAULT_STORE);
    Ok(())
}

fn refusal_with(package_toml: &str, workflow_toml: &str) -> Result<ManifestRejection, String> {
    let release = release().map_err(|rejection| rejection.message)?;
    release
        .check_files(&files_with(package_toml, workflow_toml), StoreHome::Host)
        .err()
        .ok_or_else(|| "the files were accepted".to_owned())
}

const SYNC: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\n";

#[test]
fn a_module_or_template_the_package_declares_and_the_release_does_not_list_is_refused() -> Result<(), String> {
    let refused = refusal_with("workflows = [\"sync\"]\nmodules = [\"helpers.rhai\"]\n", SYNC)?;
    assert!(refused.message.contains("workflows.toml"), "{}", refused.message);
    assert!(refused.message.contains("helpers.rhai"), "{}", refused.message);

    let refused = refusal_with("workflows = [\"sync\"]\ntemplates = [\"notes\"]\n", SYNC)?;
    assert!(refused.message.contains("notes"), "{}", refused.message);
    Ok(())
}

#[test]
fn a_package_file_that_does_not_match_its_schema_is_refused_naming_the_file() -> Result<(), String> {
    let refused = refusal_with("workflows = [\"sync\"]\nexports = [\"lib/util.rhai\"]\n", SYNC)?;

    assert_eq!(refused.code, contextos_packages::codes::INVALID_MANIFEST);
    assert!(refused.message.contains("workflows.toml"), "{}", refused.message);
    assert!(
        refused.message.contains("unknown field `exports`"),
        "{}",
        refused.message
    );
    Ok(())
}

#[test]
fn a_listed_workflow_without_a_workflow_toml_in_the_release_is_refused_naming_the_file() -> Result<(), String> {
    let refused = refusal_with("workflows = [\"sync\", \"report\"]\n", SYNC)?;

    assert_eq!(refused.code, contextos_packages::codes::INVALID_MANIFEST);
    assert!(refused.message.contains("report/workflow.toml"), "{}", refused.message);
    Ok(())
}

#[test]
fn a_workflow_toml_in_a_folder_the_package_does_not_list_is_refused() -> Result<(), String> {
    let release = release().map_err(|rejection| rejection.message)?;
    let mut files = files_with("workflows = [\"sync\"]\n", SYNC);
    if let Ok(path) = ReleaseFilePath::try_from("extra/workflow.toml") {
        files.0.insert(path, SYNC.as_bytes().to_vec());
    }

    let refused = release
        .check_files(&files, StoreHome::Host)
        .err()
        .ok_or("the files were accepted")?;

    assert!(refused.message.contains("extra/workflow.toml"), "{}", refused.message);
    assert!(refused.message.contains("does not list"), "{}", refused.message);
    Ok(())
}

#[test]
fn a_workflow_toml_declaring_modules_or_templates_or_libraries_is_refused_as_an_unknown_key() -> Result<(), String> {
    for key in ["modules", "templates", "libraries"] {
        let refused = refusal_with(PACKAGE, &format!("{SYNC}{key} = []\n"))?;
        assert!(
            refused.message.contains(&format!("unknown field `{key}`")),
            "{key}: {}",
            refused.message
        );
    }
    Ok(())
}

#[test]
fn the_interface_comes_from_the_package_file_and_each_workflow_toml() -> Result<(), ManifestRejection> {
    let release = release()?;
    let listed_twice = files_with("workflows = [\"sync\", \"report\"]\n", SYNC);
    let mut files = listed_twice;
    if let Ok(path) = ReleaseFilePath::try_from("report/workflow.toml") {
        files.0.insert(path, SYNC.as_bytes().to_vec());
    }

    let slugs: Vec<String> = release
        .interface(&files)
        .workflows
        .into_iter()
        .map(|workflow| workflow.slug)
        .collect();

    assert_eq!(slugs, ["sync", "report"]);
    Ok(())
}

#[test]
fn an_entry_the_release_does_not_list_is_refused() -> Result<(), String> {
    let refused = refusal_for("name = \"Sync\"\nentry = \"missing.rhai\"\n", StoreHome::Host)?;

    assert!(refused.message.contains("sync/missing.rhai"), "{}", refused.message);
    Ok(())
}

fn interface_of(workflow_toml: &str) -> Result<PackageInterface, ManifestRejection> {
    Ok(release()?.interface(&files_of(workflow_toml)))
}

const BASE: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\n[[input]]\nname = \"a\"\nkind = \"string\"\n[[output]]\nname = \"r\"\nkind = \"string\"\n";

fn step_after(base: &str, changed: &str) -> Result<VersionStep, ManifestRejection> {
    Ok(interface_of(changed)?.required_step(&interface_of(base)?))
}

#[test]
fn an_unchanged_interface_is_a_patch() -> Result<(), ManifestRejection> {
    assert_eq!(step_after(BASE, BASE)?, VersionStep::Patch);
    Ok(())
}

#[test]
fn adding_or_removing_an_entry_in_any_of_the_four_lists_is_a_minor_step() -> Result<(), ManifestRejection> {
    let lists = [
        "[mcp_servers]\nallow = [\"crm\"]\n",
        "[mcp_servers]\ndeny = [\"mail\"]\n",
        "[mcp_tools]\nallow = [{ server = \"crm\", tool = \"t\" }]\n",
        "[mcp_tools]\ndeny = [{ server = \"crm\", tool = \"t\" }]\n",
    ];
    for list in lists {
        let with_list = format!("{BASE}{list}");
        assert_eq!(step_after(BASE, &with_list)?, VersionStep::Minor, "adding {list}");
        assert_eq!(step_after(&with_list, BASE)?, VersionStep::Minor, "removing {list}");
    }
    let wider = format!("{BASE}[mcp_servers]\nallow = [\"crm\", \"mail\"]\n");
    let narrower = format!("{BASE}[mcp_servers]\nallow = [\"crm\"]\n");
    assert_eq!(step_after(&narrower, &wider)?, VersionStep::Minor);
    Ok(())
}

#[test]
fn reordering_a_list_is_not_a_change() -> Result<(), ManifestRejection> {
    let one = format!("{BASE}[mcp_servers]\nallow = [\"crm\", \"mail\"]\n");
    let other = format!("{BASE}[mcp_servers]\nallow = [\"mail\", \"crm\"]\n");
    assert_eq!(step_after(&one, &other)?, VersionStep::Patch);
    Ok(())
}

#[test]
fn a_workflow_removed_is_major_and_a_workflow_added_is_minor() -> Result<(), ManifestRejection> {
    let release = release()?;
    let mut both = release.interface(&files_of(BASE));
    let mut second = both.workflows[0].clone();
    second.slug = "report".to_owned();
    both.workflows.push(second);
    let one = release.interface(&files_of(BASE));

    assert_eq!(both.required_step(&one), VersionStep::Minor);
    assert_eq!(one.required_step(&both), VersionStep::Major);
    Ok(())
}

#[test]
fn the_package_takes_the_largest_step_any_workflow_requires() -> Result<(), ManifestRejection> {
    let release = release()?;
    let before = {
        let mut interface = release.interface(&files_of(BASE));
        let mut second = interface.workflows[0].clone();
        second.slug = "report".to_owned();
        interface.workflows.push(second);
        interface
    };
    let mut after = before.clone();
    after.workflows[0].server_deny.push("mail".to_owned());
    assert_eq!(after.required_step(&before), VersionStep::Minor);

    after.workflows[1].inputs.clear();
    assert_eq!(after.required_step(&before), VersionStep::Major);
    Ok(())
}

#[test]
fn input_and_output_changes_step_by_what_a_job_sees() -> Result<(), ManifestRejection> {
    let cases = [
        (
            "a new optional input",
            "[[input]]\nname = \"b\"\nkind = \"string\"\nrequired = false\n",
            VersionStep::Minor,
        ),
        (
            "a new required input",
            "[[input]]\nname = \"b\"\nkind = \"string\"\n",
            VersionStep::Major,
        ),
        (
            "a new output",
            "[[output]]\nname = \"s\"\nkind = \"string\"\n",
            VersionStep::Minor,
        ),
    ];
    for (name, addition, expected) in cases {
        assert_eq!(step_after(BASE, &format!("{BASE}{addition}"))?, expected, "{name}");
    }
    let without_input = "name = \"Sync\"\nentry = \"sync.rhai\"\n[[output]]\nname = \"r\"\nkind = \"string\"\n";
    assert_eq!(step_after(BASE, without_input)?, VersionStep::Major, "an input removed");
    let retyped = BASE.replace("name = \"a\"\nkind = \"string\"", "name = \"a\"\nkind = \"number\"");
    assert_eq!(step_after(BASE, &retyped)?, VersionStep::Major, "an input retyped");
    let optional = BASE.replace(
        "name = \"a\"\nkind = \"string\"",
        "name = \"a\"\nkind = \"string\"\nrequired = false",
    );
    assert_eq!(
        step_after(BASE, &optional)?,
        VersionStep::Minor,
        "an input made optional"
    );
    assert_eq!(
        step_after(&optional, BASE)?,
        VersionStep::Major,
        "an input made required"
    );
    let without_output = "name = \"Sync\"\nentry = \"sync.rhai\"\n[[input]]\nname = \"a\"\nkind = \"string\"\n";
    assert_eq!(
        step_after(BASE, without_output)?,
        VersionStep::Major,
        "an output removed"
    );
    let output_retyped = BASE.replace("name = \"r\"\nkind = \"string\"", "name = \"r\"\nkind = \"number\"");
    assert_eq!(
        step_after(BASE, &output_retyped)?,
        VersionStep::Major,
        "an output changed"
    );
    Ok(())
}

#[test]
fn exports_removed_are_major_and_exports_added_are_minor() {
    let with = PackageInterface {
        workflows: Vec::new(),
        exports: vec!["a.rhai".to_owned(), "b.rhai".to_owned()],
    };
    let without = PackageInterface {
        workflows: Vec::new(),
        exports: vec!["a.rhai".to_owned()],
    };
    assert_eq!(without.required_step(&with), VersionStep::Major);
    assert_eq!(with.required_step(&without), VersionStep::Minor);
}

#[test]
fn a_manifest_naming_a_bad_package_name_is_rejected_with_the_invalid_name_code() -> Result<(), String> {
    let rejected = rejection(&manifest_text("../escape", "1.2.0", ""))?;

    assert_eq!(rejected.code, contextos_packages::codes::INVALID_NAME);
    Ok(())
}

#[test]
fn a_manifest_with_a_bad_version_is_rejected_with_the_invalid_version_code() -> Result<(), String> {
    let rejected = rejection(&manifest_text("sync-crm", "one", ""))?;

    assert_eq!(rejected.code, contextos_packages::codes::INVALID_VERSION);
    Ok(())
}

#[test]
fn a_manifest_with_a_pre_release_version_is_rejected_with_the_invalid_version_code() -> Result<(), String> {
    let rejected = rejection(&manifest_text("sync-crm", "1.2.0-beta.1", ""))?;

    assert_eq!(rejected.code, contextos_packages::codes::INVALID_VERSION);
    Ok(())
}

#[test]
fn any_other_schema_failure_is_rejected_with_the_invalid_manifest_code() -> Result<(), String> {
    let rejected = rejection("name = \"sync-crm\"\nunknown = 1\n")?;

    assert_eq!(rejected.code, contextos_packages::codes::INVALID_MANIFEST);
    assert_ne!(rejected.message, "");
    Ok(())
}

#[test]
fn a_listed_file_with_a_malformed_hash_is_rejected_with_the_invalid_manifest_code() -> Result<(), String> {
    let text = manifest_text("sync-crm", "1.2.0", "").replace(HASH, "short");

    assert_eq!(rejection(&text)?.code, contextos_packages::codes::INVALID_MANIFEST);
    Ok(())
}

#[test]
fn the_interface_of_a_library_records_the_modules_it_exports() -> Result<(), ManifestRejection> {
    let library = PackageRelease::try_from(
        format!(
            "name = \"acme-text\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\n\
             [[files]]\npath = \"library.toml\"\nhash = \"{HASH}\"\n\n\
             [[files]]\npath = \"lib/util.rhai\"\nhash = \"{HASH}\"\n"
        )
        .as_str(),
    )?;

    let held = |path: &str, text: &str| (ReleaseFilePath::try_from(path), text.as_bytes().to_vec());
    let files = ReleaseFiles(
        [held(
            "library.toml",
            "modules = [\"lib/util.rhai\"]\nexports = [\"lib/util.rhai\"]\n",
        )]
        .into_iter()
        .filter_map(|(path, bytes)| path.ok().map(|path| (path, bytes)))
        .collect(),
    );

    let interface = library.interface(&files);

    assert_eq!(interface.exports, ["lib/util.rhai"]);
    assert_eq!(interface.workflows.len(), 0);
    Ok(())
}

/// A library release that lists its `library.toml` and one module, with `library_toml` as that file's text.
fn library_checked_with(library_toml: &str) -> Result<Result<(), ManifestRejection>, ManifestRejection> {
    let release = PackageRelease::try_from(
        format!(
            "name = \"acme-text\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\n\
             [[files]]\npath = \"library.toml\"\nhash = \"{HASH}\"\n\n\
             [[files]]\npath = \"lib/util.rhai\"\nhash = \"{HASH}\"\n"
        )
        .as_str(),
    )?;
    let held = |path: &str, text: &str| (ReleaseFilePath::try_from(path), text.as_bytes().to_vec());
    let files = ReleaseFiles(
        [
            held("library.toml", library_toml),
            held("lib/util.rhai", "fn x() { 1 }"),
        ]
        .into_iter()
        .filter_map(|(path, bytes)| path.ok().map(|path| (path, bytes)))
        .collect(),
    );
    Ok(release.check_files(&files, StoreHome::Host))
}

#[test]
fn a_library_whose_library_toml_declares_what_it_ships_is_accepted() -> Result<(), ManifestRejection> {
    let checked = library_checked_with(
        "modules = [\"lib/util.rhai\"]\nexports = [\"lib/util.rhai\"]\n\
         mcp_tools = [{ server = \"crm\", tool = \"*\" }]\n",
    )?;

    assert!(checked.is_ok(), "{checked:?}");
    Ok(())
}

#[test]
fn a_library_whose_library_toml_does_not_match_its_schema_is_refused_naming_the_file() -> Result<(), ManifestRejection>
{
    for invalid in [
        "modules = [\"lib/util.rhai\"]\nexports = [\"lib/util.rhai\"]\nmcp_servers = [\"crm\"]\n",
        "name = \"Text\"\nmodules = [\"lib/util.rhai\"]\nexports = [\"lib/util.rhai\"]\n",
        "modules = [\"lib/util.rhai\"]\nexports = [\"lib/util.rhai\"]\n\
         mcp_tools = [{ server = \"crm\", tool = \"*\" }, { server = \"crm\", tool = \"find\" }]\n",
        "not toml at all [",
    ] {
        let refused = library_checked_with(invalid)?.err();

        let refused = refused.ok_or_else(|| ManifestRejection {
            code: "test",
            message: format!("accepted: {invalid}"),
        })?;
        assert_eq!(refused.code, contextos_packages::codes::INVALID_MANIFEST);
        assert!(refused.message.contains("library.toml"), "{}", refused.message);
    }
    Ok(())
}

// The tests below describe behaviour that already existed when they were written, so none was seen failing first.

fn release_with(files: &[(&str, &[u8])]) -> ReleaseFiles {
    ReleaseFiles(
        files
            .iter()
            .filter_map(|(path, bytes)| ReleaseFilePath::try_from(*path).ok().map(|path| (path, bytes.to_vec())))
            .collect(),
    )
}

#[test]
fn a_library_release_holding_a_workflow_toml_is_refused() -> Result<(), ManifestRejection> {
    let release = PackageRelease::try_from(
        format!(
            "name = \"acme-text\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\n\
             [[files]]\npath = \"library.toml\"\nhash = \"{HASH}\"\n\n\
             [[files]]\npath = \"lib/util.rhai\"\nhash = \"{HASH}\"\n"
        )
        .as_str(),
    )?;
    let files = release_with(&[
        (
            "library.toml",
            b"modules = [\"lib/util.rhai\"]\nexports = [\"lib/util.rhai\"]\n",
        ),
        ("lib/util.rhai", b"fn x() { 1 }"),
        ("x/workflow.toml", SYNC.as_bytes()),
    ]);

    let refused = release.check_files(&files, StoreHome::Host).err();

    let refused = refused.map(|found| found.message).unwrap_or_default();
    assert!(
        refused.contains("x/workflow.toml") && refused.contains("library.toml"),
        "{refused}"
    );
    Ok(())
}

#[test]
fn a_workflow_toml_nested_deeper_than_one_folder_is_not_a_workflow() -> Result<(), ManifestRejection> {
    let release = release()?;
    let mut files = files_with("workflows = [\"sync\"]\nmodules = [\"lib/util.rhai\"]\n", SYNC);
    if let Ok(path) = ReleaseFilePath::try_from("sync/fixtures/deep/workflow.toml") {
        files.0.insert(path, b"not even toml [".to_vec());
    }

    let checked = release.check_files(&files, StoreHome::Host);

    assert!(checked.is_ok(), "{checked:?}");
    Ok(())
}

#[test]
fn a_package_file_that_is_not_utf_8_is_refused_as_unreadable() -> Result<(), ManifestRejection> {
    let release = release()?;
    let files = release_with(&[
        ("workflows.toml", &[0xff, 0xfe, 0x00]),
        ("sync/workflow.toml", SYNC.as_bytes()),
        ("sync/sync.rhai", b"()"),
        ("lib/util.rhai", b"fn x() { 1 }"),
    ]);

    let refused = release
        .check_files(&files, StoreHome::Host)
        .err()
        .map(|found| found.message);

    let refused = refused.unwrap_or_default();
    assert!(
        refused.contains("workflows.toml") && refused.contains("UTF-8"),
        "{refused}"
    );
    Ok(())
}

#[test]
fn a_workflow_toml_that_is_not_utf_8_is_refused_naming_the_file() -> Result<(), ManifestRejection> {
    let release = release()?;
    let files = release_with(&[
        ("workflows.toml", PACKAGE.as_bytes()),
        ("sync/workflow.toml", &[0xff, 0xfe]),
        ("sync/sync.rhai", b"()"),
        ("lib/util.rhai", b"fn x() { 1 }"),
    ]);

    let refused = release
        .check_files(&files, StoreHome::Host)
        .err()
        .map(|found| found.message);

    let refused = refused.unwrap_or_default();
    assert!(
        refused.contains("sync/workflow.toml") && refused.contains("UTF-8"),
        "{refused}"
    );
    Ok(())
}

#[test]
fn a_template_that_is_a_folder_is_held_when_the_release_lists_a_file_beneath_it() -> Result<(), ManifestRejection> {
    let release = release()?;
    let mut files = files_with(
        "workflows = [\"sync\"]\nmodules = [\"lib/util.rhai\"]\ntemplates = [\"notes\"]\n",
        SYNC,
    );
    if let Ok(path) = ReleaseFilePath::try_from("notes/profile.md") {
        files.0.insert(path, b"# Profile".to_vec());
    }

    let checked = release.check_files(&files, StoreHome::Host);

    assert!(checked.is_ok(), "{checked:?}");
    Ok(())
}

fn changes_after(base: &str, changed: &str) -> Result<Vec<String>, ManifestRejection> {
    Ok(interface_of(changed)?
        .changes_from(&interface_of(base)?)
        .into_iter()
        .map(|change| format!("{}: {}", change.step, change.what))
        .collect())
}

#[test]
fn each_interface_change_is_named_with_the_step_it_needs() -> Result<(), ManifestRejection> {
    let without_input = "name = \"Sync\"\nentry = \"sync.rhai\"\n[[output]]\nname = \"r\"\nkind = \"string\"\n";
    assert_eq!(changes_after(BASE, without_input)?, ["major: sync: input a removed"]);
    let more = format!(
        "{BASE}[[input]]\nname = \"b\"\nkind = \"string\"\nrequired = false\n[[output]]\nname = \"s\"\nkind = \"string\"\n[mcp_servers]\ndeny = [\"mail\"]\n"
    );
    assert_eq!(
        changes_after(BASE, &more)?,
        [
            "minor: sync: optional input b added",
            "minor: sync: output s added",
            "minor: sync: the allowed servers or tools changed"
        ]
    );
    assert_eq!(changes_after(BASE, BASE)?, Vec::<String>::new());
    Ok(())
}

#[test]
fn a_workflow_and_an_export_changing_are_named() {
    let with = PackageInterface {
        workflows: Vec::new(),
        exports: vec!["a.rhai".to_owned(), "b.rhai".to_owned()],
    };
    let without = PackageInterface {
        workflows: Vec::new(),
        exports: vec!["a.rhai".to_owned()],
    };
    let named = |now: &PackageInterface, before: &PackageInterface| -> Vec<String> {
        now.changes_from(before)
            .into_iter()
            .map(|change| format!("{}: {}", change.step, change.what))
            .collect()
    };
    assert_eq!(named(&without, &with), ["major: export b.rhai removed"]);
    assert_eq!(named(&with, &without), ["minor: export b.rhai added"]);
}

fn with_tool_contract(version: &str) -> String {
    format!(
        "{BASE}[[required_tool_contracts]]\nmcp_server_name = \"crm\"\ntool = \"list\"\ncontract_version = \"{version}\"\n"
    )
}

#[test]
fn a_required_tool_contract_version_change_needs_a_minor_step_and_is_named() -> Result<(), ManifestRejection> {
    let changes = changes_after(&with_tool_contract("1"), &with_tool_contract("2"))?;

    assert_eq!(
        changes,
        ["minor: sync: tool crm.list contract version changed from 1 to 2"]
    );
    assert_eq!(
        step_after(&with_tool_contract("1"), &with_tool_contract("2"))?,
        VersionStep::Minor
    );
    assert_eq!(
        changes_after(&with_tool_contract("1"), &with_tool_contract("1"))?,
        Vec::<String>::new()
    );
    Ok(())
}

#[test]
fn a_required_tool_added_or_removed_needs_a_minor_step() -> Result<(), ManifestRejection> {
    assert_eq!(
        changes_after(BASE, &with_tool_contract("1"))?,
        ["minor: sync: required tool crm.list added"]
    );
    assert_eq!(
        changes_after(&with_tool_contract("1"), BASE)?,
        ["minor: sync: required tool crm.list removed"]
    );
    Ok(())
}
