use std::fmt::Write as _;

use super::*;
use crate::call_rules::{CallLists, ToolRef};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn declared(servers: &[&str]) -> CallRules {
    CallRules::try_from(CallLists {
        server_allow: Some(servers.iter().map(|server| (*server).to_owned()).collect()),
        ..CallLists::default()
    })
    .unwrap_or_default()
}

mod releases {
    use std::collections::BTreeMap;

    use contextos_core::ContentHash;

    use super::*;
    use crate::library_manifest::LibraryManifest;
    use crate::package_contents::PackageKind;
    use crate::package_name::PackageName;
    use crate::script_modules::{PackageCode, PackageToRead, ScriptParts};
    use crate::workflow_files::WorkflowFile;
    use crate::workflows_file::WorkflowsFile;

    /// One package of a release: its modules with their text, what it exports, and what it depends on.
    struct Part<'a> {
        name: &'a str,
        modules: &'a [(&'a str, &'a str)],
        exports: &'a [&'a str],
        dependencies: &'a [&'a str],
    }

    fn quoted(items: &[&str]) -> String {
        format!(
            "[{}]",
            items
                .iter()
                .map(|item| format!("{item:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    fn code_of(part: &Part<'_>) -> Result<PackageCode, Box<dyn std::error::Error>> {
        let paths: Vec<&str> = part.modules.iter().map(|(path, _)| *path).collect();
        let mut libraries = String::new();
        for dependency in part.dependencies {
            let _ = write!(libraries, "[[libraries]]\nname = {dependency:?}\nrequirement = \"*\"\n");
        }
        let kind = if part.exports.is_empty() {
            PackageKind::Workflows(WorkflowsFile::try_from(
                format!("workflows = [\"sync\"]\nmodules = {}\n{libraries}", quoted(&paths)).as_str(),
            )?)
        } else {
            PackageKind::Library(LibraryManifest::try_from(
                format!(
                    "modules = {}\nexports = {}\n{libraries}",
                    quoted(&paths),
                    quoted(part.exports)
                )
                .as_str(),
            )?)
        };
        let held: BTreeMap<&str, &str> = part.modules.iter().copied().collect();
        let package = PackageToRead {
            name: PackageName::try_from(part.name)?,
            kind,
        };
        Ok(PackageCode::read(package, |path| {
            Ok(WorkflowFile {
                content: held.get(path.as_str()).copied().unwrap_or_default().to_owned(),
                content_hash: ContentHash::from([0; 32]),
            })
        })?)
    }

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
        }
    }

    fn library<'a>(name: &'a str, modules: &'a [(&'a str, &'a str)], dependencies: &'a [&'a str]) -> Part<'a> {
        Part {
            name,
            modules,
            exports: &["client.rhai"],
            dependencies,
        }
    }

    fn listed(
        libraries: &[(&str, &[&str])],
    ) -> Result<BTreeMap<PackageName, Vec<ToolRef>>, Box<dyn std::error::Error>> {
        libraries
            .iter()
            .map(|(name, servers)| {
                Ok((
                    PackageName::try_from(*name)?,
                    servers
                        .iter()
                        .map(|entry| {
                            let (server, tool) = entry.split_once('.').unwrap_or((entry, ""));
                            ToolRef {
                                server: server.to_owned(),
                                tool: tool.to_owned(),
                            }
                        })
                        .collect(),
                ))
            })
            .collect()
    }

    fn checked(
        script: &WorkflowScript,
        libraries: &[(&str, &[&str])],
        workflow_servers: &[&str],
    ) -> Result<Vec<ReleaseToolFailure>, Box<dyn std::error::Error>> {
        Ok(
            check_release_library_tools(script, &listed(libraries)?, &declared(workflow_servers))
                .map_err(|incomplete| incomplete.message)?,
        )
    }

    const CLIENT: &[(&str, &str)] = &[("client.rhai", "fn act() { 1 }")];

    #[test]
    fn a_library_imported_from_the_entry_script_whose_server_the_workflow_does_not_permit_is_reported() -> TestResult {
        let script = script_of(
            "import \"crm:client\" as c;",
            &[workflow(&[], &["crm"]), library("crm", CLIENT, &[])],
        )?;

        let failures = checked(&script, &[("crm", &["billing.charge"])], &["contextos"])?;

        let [failure] = failures.as_slice() else {
            return Err(format!("{failures:?}").into());
        };
        assert_eq!((failure.library.as_str(), failure.server.as_str()), ("crm", "billing"));
        assert_eq!(failure.artefact, "crm:library.toml");
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

        assert_eq!(checked(&script, &[("crm", &["billing.charge"])], &[])?.len(), 1);
        Ok(())
    }

    #[test]
    fn a_library_named_only_in_a_comment_or_a_string_is_not_imported_from() -> TestResult {
        let script = script_of(
            "// import \"crm:client\" as c;\nlet note = \"crm:client\";",
            &[workflow(&[], &["crm"]), library("crm", CLIENT, &[])],
        )?;

        assert_eq!(checked(&script, &[("crm", &["billing.charge"])], &[])?, []);
        Ok(())
    }

    #[test]
    fn a_library_a_module_nothing_imports_would_import_adds_nothing() -> TestResult {
        let unused = [("unused.rhai", "import \"crm:client\" as c;\nfn act() { c::act() }")];
        let script = script_of(
            "()",
            &[
                Part {
                    modules: &unused,
                    ..workflow(&[], &["crm"])
                },
                library("crm", CLIENT, &[]),
            ],
        )?;

        assert_eq!(checked(&script, &[("crm", &["billing.charge"])], &[])?, []);
        Ok(())
    }

    #[test]
    fn the_servers_of_a_library_a_library_imports_from_are_checked_in_turn() -> TestResult {
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

        let failures = checked(&script, &[("crm", &[]), ("http", &["web.get"])], &[])?;

        let [failure] = failures.as_slice() else {
            return Err(format!("{failures:?}").into());
        };
        assert_eq!((failure.library.as_str(), failure.server.as_str()), ("http", "web"));
        Ok(())
    }

    #[test]
    fn a_literal_call_to_a_tool_the_library_does_not_declare_is_reported_even_if_the_workflow_declares_it() -> TestResult
    {
        let calls = [("client.rhai", "fn act() { call_mcp(\"crm\", \"find\", #{}) }")];
        let script = script_of(
            "import \"crm:client\" as c;",
            &[workflow(&[], &["crm"]), library("crm", &calls, &[])],
        )?;

        let failures = checked(&script, &[("crm", &[])], &["crm"])?;

        let [failure] = failures.as_slice() else {
            return Err(format!("{failures:?}").into());
        };
        assert_eq!(failure.artefact, "crm:library.toml");
        assert_eq!(failure.server, "crm");
        assert_eq!(failure.tool.as_deref(), Some("find"));
        assert!(failure.message.contains("client.rhai"), "{}", failure.message);
        assert!(failure.message.contains("mcp_tools"), "{}", failure.message);
        Ok(())
    }

    #[test]
    fn a_computed_tool_is_accepted_only_when_the_library_declares_every_tool_of_that_server() -> TestResult {
        let calls = [("client.rhai", "fn act(tool) { call_mcp(\"crm\", tool, #{}) }")];
        let script = script_of(
            "import \"crm:client\" as c;",
            &[workflow(&[], &["crm"]), library("crm", &calls, &[])],
        )?;

        assert_eq!(checked(&script, &[("crm", &["crm.*"])], &["crm"])?, []);
        for declared in [&["crm.find"][..], &["mail.*"][..], &[][..]] {
            let failures = checked(&script, &[("crm", declared)], &["crm", "mail"])?;

            let [failure] = failures.as_slice() else {
                return Err(format!("{declared:?}: {failures:?}").into());
            };
            assert_eq!((failure.server.as_str(), failure.tool.as_deref()), ("crm", None));
        }
        Ok(())
    }

    #[test]
    fn a_wildcard_is_reported_when_the_workflow_restricts_the_server_and_accepted_otherwise() -> TestResult {
        let script = script_of(
            "import \"crm:client\" as c;",
            &[workflow(&[], &["crm"]), library("crm", CLIENT, &[])],
        )?;
        let declared_wildcard = listed(&[("crm", &["crm.*"])])?;
        let denying_a_tool = CallRules::try_from(CallLists {
            tool_deny: vec![ToolRef {
                server: "crm".to_owned(),
                tool: "delete".to_owned(),
            }],
            ..CallLists::default()
        })?;

        for (rules, accepted) in [
            (CallRules::default(), true),
            (declared(&["crm"]), true),
            (declared(&["mail"]), false),
            (denying_a_tool, false),
        ] {
            let failures = check_release_library_tools(&script, &declared_wildcard, &rules)
                .map_err(|incomplete| incomplete.message)?;

            assert_eq!(failures.is_empty(), accepted, "{rules:?}: {failures:?}");
            if let [failure] = failures.as_slice() {
                assert_eq!(failure.tool.as_deref(), Some("*"));
                assert_eq!(failure.artefact, "crm:library.toml");
            }
        }
        Ok(())
    }

    #[test]
    fn a_check_that_could_not_start_its_thread_is_an_error_and_never_a_pass() -> TestResult {
        let script = script_of(
            "import \"crm:client\" as c;",
            &[workflow(&[], &["crm"]), library("crm", CLIENT, &[])],
        )?;

        let outcome = check_release_with(
            &script,
            &listed(&[("crm", &["billing.charge"])])?,
            &declared(&[]),
            |_work| Err(std::io::Error::other("no more threads")),
        );

        let incomplete = outcome.err().ok_or("a check that did not run was taken for a pass")?;
        assert!(incomplete.message.contains("no more threads"), "{}", incomplete.message);
        Ok(())
    }

    #[test]
    fn a_workflow_with_no_server_allow_list_permits_the_library_server_unless_it_denies_it() -> TestResult {
        let script = script_of(
            "import \"crm:client\" as c;",
            &[workflow(&[], &["crm"]), library("crm", CLIENT, &[])],
        )?;
        let tools = listed(&[("crm", &["crm.find"])])?;
        let outcome = |rules: &CallRules| check_release_library_tools(&script, &tools, rules).map_err(|e| e.message);

        assert_eq!(outcome(&CallRules::default())?, []);

        let denying = CallRules::try_from(CallLists {
            server_deny: vec!["crm".to_owned()],
            ..CallLists::default()
        })?;
        let failures = outcome(&denying)?;
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].server, "crm");
        Ok(())
    }

    #[test]
    fn a_tool_the_workflow_denies_is_reported_even_when_its_server_is_allowed() -> TestResult {
        let script = script_of(
            "import \"crm:client\" as c;",
            &[workflow(&[], &["crm"]), library("crm", CLIENT, &[])],
        )?;
        let denying = CallRules::try_from(CallLists {
            tool_deny: vec![ToolRef {
                server: "crm".to_owned(),
                tool: "find".to_owned(),
            }],
            ..CallLists::default()
        })?;

        let failures = check_release_library_tools(&script, &listed(&[("crm", &["crm.find"])])?, &denying)
            .map_err(|incomplete| incomplete.message)?;

        let [failure] = failures.as_slice() else {
            return Err(format!("{failures:?}").into());
        };
        assert_eq!(failure.tool.as_deref(), Some("find"));
        assert!(failure.message.contains("find"), "{}", failure.message);
        Ok(())
    }

    #[test]
    fn a_tool_allow_that_names_other_tools_on_the_server_leaves_the_library_tool_out() -> TestResult {
        let script = script_of(
            "import \"crm:client\" as c;",
            &[workflow(&[], &["crm"]), library("crm", CLIENT, &[])],
        )?;
        let tools = listed(&[("crm", &["crm.find"])])?;
        let allowing = |tool: &str| {
            CallRules::try_from(CallLists {
                tool_allow: vec![ToolRef {
                    server: "crm".to_owned(),
                    tool: tool.to_owned(),
                }],
                ..CallLists::default()
            })
        };

        let outcome = |rules: &CallRules| check_release_library_tools(&script, &tools, rules).map_err(|e| e.message);
        assert_eq!(outcome(&allowing("find")?)?, []);
        assert_eq!(outcome(&allowing("list")?)?.len(), 1);
        Ok(())
    }

    #[test]
    fn a_call_whose_server_is_computed_is_left_to_the_run() -> TestResult {
        let calls = [("client.rhai", "fn act(server) { call_mcp(server, \"find\", #{}) }")];
        let script = script_of(
            "import \"crm:client\" as c;",
            &[workflow(&[], &["crm"]), library("crm", &calls, &[])],
        )?;

        assert_eq!(checked(&script, &[("crm", &[])], &[])?, []);
        Ok(())
    }

    #[test]
    fn a_check_that_panicked_is_an_error_and_never_a_pass() -> TestResult {
        let script = script_of(
            "import \"crm:client\" as c;",
            &[workflow(&[], &["crm"]), library("crm", CLIENT, &[])],
        )?;

        let outcome = check_release_with(
            &script,
            &listed(&[("crm", &["billing.charge"])])?,
            &declared(&[]),
            |_work| {
                std::thread::Builder::new()
                    .spawn(|| -> Vec<ReleaseToolFailure> { std::panic::resume_unwind(Box::new("boom")) })
            },
        );

        let incomplete = outcome.err().ok_or("a check that panicked was taken for a pass")?;
        assert!(
            incomplete.message.contains("did not complete"),
            "{}",
            incomplete.message
        );
        Ok(())
    }
}
