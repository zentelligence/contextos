//! The `contextos-web jobs run`, `jobs dry-run`, and `workflows run` commands as a real process: what is on standard
//! output (the run summary and nothing else), the graded exit status, and what is on standard error. The host's data
//! directory is redirected to a temporary directory, so nothing here reads or writes the operator's own.
#![cfg(target_os = "linux")]

#[path = "support/run_host.rs"]
mod run_host;
mod support;

use run_host::{
    BoxError, DEBUGGED, DEGRADED_REASON, PRINTED, RunHost, SECRET_OUTPUT, args, jobs_dry_run, jobs_run, stderr, stdout,
    workflows_run,
};
use serde_json::{Value, json};

fn field<'a>(document: &'a Value, name: &str) -> &'a Value {
    document.get(name).unwrap_or(&Value::Null)
}

/// Every command, for a vault and a shared workflow, live and dry, that ends with a run summary.
fn every_run_command(workflow: &str) -> Vec<(String, Vec<String>)> {
    let mut commands = Vec::new();
    for shared in [false, true] {
        let scope = if shared { "shared" } else { "vault" };
        commands.push((
            format!("jobs run, {scope}"),
            jobs_run(shared, workflow, &["--format", "json"]),
        ));
        commands.push((
            format!("jobs run --dry-run, {scope}"),
            jobs_run(shared, workflow, &["--dry-run", "--format", "json"]),
        ));
        commands.push((
            format!("jobs dry-run, {scope}"),
            jobs_dry_run(shared, workflow, &["--format", "json"]),
        ));
        commands.push((
            format!("workflows run, {scope}"),
            workflows_run(shared, workflow, &["--format", "json"]),
        ));
        commands.push((
            format!("workflows run --dry-run, {scope}"),
            workflows_run(shared, workflow, &["--dry-run", "--format", "json"]),
        ));
    }
    commands
}

#[tokio::test]
async fn with_format_json_every_run_command_prints_exactly_one_document_on_stdout() -> Result<(), BoxError> {
    let host = RunHost::new()?;

    for (name, command) in every_run_command("emit") {
        let (output, document) = host.json(&command).await?;

        assert_eq!(output.status.code(), Some(0), "{name}: {}", stderr(&output));
        assert_eq!(field(&document, "summary_version"), &json!(1), "{name}");
        assert_eq!(field(&document, "outcome"), &json!("succeeded"), "{name}: {document}");
        assert_eq!(field(&document, "exit_code"), &json!(0), "{name}");
        assert_eq!(
            field(&document, "outputs"),
            &json!({ "created": 3, "label": SECRET_OUTPUT }),
            "{name}: the declared outputs, in declaration order"
        );
        let expected_command = if command[0] == "workflows" {
            "workflows run"
        } else if command[1] == "dry-run" {
            "jobs dry-run"
        } else {
            "jobs run"
        };
        assert_eq!(field(&document, "command"), &json!(expected_command), "{name}");
        if command[0] == "jobs" && !command.contains(&"--dry-run".to_owned()) && command[1] == "run" {
            assert_eq!(
                field(&document["run"], "id"),
                &json!("R-1"),
                "{name}: a saved job's run has its number"
            );
            assert!(field(&document["run"], "record").is_string(), "{name}: and its record");
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_script_that_prints_and_debugs_leaves_stdout_holding_only_the_summary() -> Result<(), BoxError> {
    let host = RunHost::new()?;

    for shared in [false, true] {
        let scope = if shared { "shared" } else { "vault" };
        let (json_output, document) = host.json(&jobs_run(shared, "chatty", &["--format", "json"])).await?;
        assert_eq!(json_output.status.code(), Some(0), "{scope}: {}", stderr(&json_output));
        assert_eq!(
            field(&document, "summary_version"),
            &json!(1),
            "{scope}: stdout parses whole as one summary"
        );

        let text_output = host.run(&jobs_run(shared, "chatty", &["--format", "text"])).await?;
        let text = stdout(&text_output);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.first(), Some(&"Run succeeded."), "{scope}: {text}");
        let allowed = ["Run succeeded.", "Job: ", "Run: ", "Record: ", "Duration: "];
        for line in &lines {
            assert!(
                allowed.iter().any(|prefix| line.starts_with(prefix)),
                "{scope}: a line that is not part of the summary: {line:?}"
            );
        }
        for output in [&json_output, &text_output] {
            assert!(
                !stdout(output).contains(PRINTED),
                "{scope}: the printed text reached stdout"
            );
            assert!(
                !stdout(output).contains(DEBUGGED),
                "{scope}: the debugged text reached stdout"
            );
            assert!(
                stderr(output).contains(PRINTED),
                "{scope}: the printed text is in the log at info: {}",
                stderr(output)
            );
            assert!(
                !stderr(output).contains(DEBUGGED),
                "{scope}: debug is below the default log level: {}",
                stderr(output)
            );
        }
    }
    Ok(())
}

/// One refusal: the command, the code its summary carries, and a name for the failure message.
struct Refusal {
    name: &'static str,
    command: Vec<String>,
    code: &'static str,
}

fn refusals() -> Vec<Refusal> {
    let mut cases = Vec::new();
    for shared in [false, true] {
        cases.push(Refusal {
            name: if shared {
                "job not found, shared"
            } else {
                "job not found, vault"
            },
            command: {
                let mut command = jobs_run(shared, "emit", &["--format", "json"]);
                if let Some(job) = command.iter_mut().find(|part| *part == "nightly") {
                    "no-such-job".clone_into(job);
                }
                command
            },
            code: "workflow/job-not-found",
        });
        cases.push(Refusal {
            name: if shared {
                "not runnable, shared"
            } else {
                "not runnable, vault"
            },
            command: {
                let mut command = jobs_run(shared, "emit", &["--format", "json"]);
                if let Some(job) = command.iter_mut().find(|part| *part == "nightly") {
                    "draft".clone_into(job);
                }
                command
            },
            code: "workflow/job-not-runnable",
        });
        cases.push(Refusal {
            name: if shared {
                "input invalid, shared"
            } else {
                "input invalid, vault"
            },
            command: workflows_run(shared, "echo", &["--format", "json"]),
            code: "workflow/input-invalid",
        });
        cases.push(Refusal {
            name: if shared {
                "scenario missing, shared"
            } else {
                "scenario missing, vault"
            },
            command: jobs_run(
                shared,
                "emit",
                &["--dry-run", "--scenario", "no-such-scenario", "--format", "json"],
            ),
            code: "workflow/scenario-unavailable",
        });
    }
    cases
}

#[tokio::test]
async fn a_refused_run_prints_one_summary_and_exits_3_for_each_refusal_kind() -> Result<(), BoxError> {
    let host = RunHost::new()?;

    for case in refusals() {
        let (output, document) = host.json(&case.command).await?;

        assert_eq!(output.status.code(), Some(3), "{}: {}", case.name, stderr(&output));
        assert_eq!(
            field(&document, "outcome"),
            &json!("refused"),
            "{}: {document}",
            case.name
        );
        assert_eq!(field(&document, "code"), &json!(case.code), "{}: {document}", case.name);
        assert_eq!(field(&document, "exit_code"), &json!(3), "{}", case.name);
        assert_eq!(field(&document, "outputs"), &Value::Null, "{}", case.name);
    }

    let broken = host.root.path().join("broken-web.toml");
    run_host::write(&broken, "this is = not [ valid")?;
    let output = host
        .command_with(&broken, &jobs_run(false, "emit", &["--format", "json"]))
        .output()
        .await?;
    let document = run_host::parse_whole(&output)?;
    assert_eq!(output.status.code(), Some(3), "config invalid: {}", stderr(&output));
    assert_eq!(field(&document, "outcome"), &json!("refused"), "{document}");
    assert_eq!(
        field(&document, "code"),
        &json!("workflow/config-invalid"),
        "{document}"
    );
    assert_eq!(
        field(&document["scope"], "workflow"),
        &json!("emit"),
        "the scope is built from the arguments as given"
    );
    Ok(())
}

#[tokio::test]
async fn a_busy_job_exits_75_with_job_busy_for_vault_and_shared_jobs() -> Result<(), BoxError> {
    let host = RunHost::new()?;
    let _shared = host.hold_shared_claim("emit")?;
    let _vault = host.hold_vault_claim("emit")?;

    for shared in [false, true] {
        let scope = if shared { "shared" } else { "vault" };
        let (output, document) = host.json(&jobs_run(shared, "emit", &["--format", "json"])).await?;

        assert_eq!(output.status.code(), Some(75), "{scope}: {}", stderr(&output));
        assert_eq!(field(&document, "outcome"), &json!("refused"), "{scope}: {document}");
        assert_eq!(
            field(&document, "code"),
            &json!("workflow/job-busy"),
            "{scope}: {document}"
        );
        assert!(
            !stdout(&output).contains(".lock"),
            "{scope}: no output names the claim: {}",
            stdout(&output)
        );
        assert!(
            !stderr(&output).contains(".lock"),
            "{scope}: no output names the claim: {}",
            stderr(&output)
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_timed_out_run_exits_124() -> Result<(), BoxError> {
    let host = RunHost::new()?;

    for shared in [false, true] {
        let scope = if shared { "shared" } else { "vault" };
        let (output, document) = host.json(&jobs_run(shared, "slow", &["--format", "json"])).await?;

        assert_eq!(output.status.code(), Some(124), "{scope}: {}", stderr(&output));
        assert_eq!(field(&document, "outcome"), &json!("failed"), "{scope}: {document}");
        assert_eq!(
            field(&document, "code"),
            &json!("workflow/timed-out"),
            "{scope}: {document}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_failed_run_exits_1_with_its_code_in_the_summary_and_in_one_error_log_event() -> Result<(), BoxError> {
    let host = RunHost::new()?;

    for shared in [false, true] {
        let scope = if shared { "shared" } else { "vault" };
        let (output, document) = host.json(&jobs_run(shared, "fail", &["--format", "json"])).await?;

        assert_eq!(output.status.code(), Some(1), "{scope}: {}", stderr(&output));
        assert_eq!(field(&document, "outcome"), &json!("failed"), "{scope}: {document}");
        assert_eq!(
            field(&document, "code"),
            &json!("workflow/script-error"),
            "{scope}: {document}"
        );
        let log = stderr(&output);
        // Only the command's own event is counted: the MCP library can log its own shutdown as an error (a task it was
        // closing was cancelled), which is no business of the command and comes and goes with timing.
        let errors: Vec<&str> = log
            .lines()
            .filter(|line| line.contains(" ERROR contextos_web::run_commands:"))
            .collect();
        assert_eq!(errors.len(), 1, "{scope}: exactly one error event: {log}");
        assert!(
            errors[0].contains("workflow/script-error"),
            "{scope}: the event carries the code: {}",
            errors[0]
        );
        assert!(
            !log.lines().any(|line| line.starts_with("Error:")),
            "{scope}: the command writes no `Error:` line of its own: {log}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_usage_error_exits_2_and_prints_nothing_on_stdout() -> Result<(), BoxError> {
    let host = RunHost::new()?;
    let cases = [
        jobs_run(false, "emit", &["--scenario", "offline"]),
        jobs_run(true, "emit", &["--format", "yaml"]),
        workflows_run(false, "emit", &["--release", "current", "--source"]),
        workflows_run(true, "emit", &["--release", "not-a-version"]),
        workflows_run(true, "emit", &["--release", "1.0.0", "--source"]),
        workflows_run(true, "emit", &["--scenario", "offline"]),
    ];

    for command in cases {
        let output = host.run(&command).await?;

        assert_eq!(output.status.code(), Some(2), "{command:?}: {}", stderr(&output));
        assert_eq!(stdout(&output), "", "{command:?}");
    }
    Ok(())
}

#[tokio::test]
async fn the_text_summary_prints_the_documented_lines_in_order() -> Result<(), BoxError> {
    let host = RunHost::new()?;

    let live = host.run(&jobs_run(true, "emit", &[])).await?;
    let dry = host
        .run(&jobs_run(false, "emit", &["--dry-run", "--format", "text"]))
        .await?;

    let live_text = stdout(&live);
    let live_lines: Vec<&str> = live_text.lines().collect();
    assert_eq!(live.status.code(), Some(0), "{}", stderr(&live));
    assert_eq!(live_lines.first(), Some(&"Run succeeded."), "{live_text}");
    assert_eq!(
        live_lines.get(1),
        Some(&"Job: nightly (workflow emit, package acme 1.0.0)"),
        "{live_text}"
    );
    assert_eq!(live_lines.get(2), Some(&"Run: R-1"), "{live_text}");
    assert!(
        live_lines
            .get(3)
            .is_some_and(|line| line.starts_with("Record: runs/acme/emit/nightly/")),
        "{live_text}"
    );
    assert_eq!(live_lines.get(4), Some(&"Output created: 3"), "{live_text}");
    assert_eq!(
        live_lines.get(5),
        Some(&format!("Output label: \"{SECRET_OUTPUT}\"").as_str()),
        "{live_text}"
    );
    assert!(
        live_lines
            .get(6)
            .is_some_and(|line| line.starts_with("Duration: ") && line.ends_with(" s")),
        "{live_text}"
    );
    assert_eq!(live_lines.len(), 7, "{live_text}");

    let dry_text = stdout(&dry);
    let dry_lines: Vec<&str> = dry_text.lines().collect();
    assert_eq!(dry_lines.first(), Some(&"Dry run succeeded."), "{dry_text}");
    assert_eq!(
        dry_lines.get(1),
        Some(&"Job: nightly (workflow emit, package emit, vault va)"),
        "{dry_text}"
    );
    assert_eq!(
        dry_lines.get(2),
        Some(&"Scenario: offline (call results were mocked, so nothing was written)"),
        "{dry_text}"
    );
    assert!(
        dry_lines.iter().any(|line| line.starts_with("Registration: ")),
        "{dry_text}"
    );
    Ok(())
}

#[tokio::test]
async fn a_degraded_run_exits_0_with_its_warnings_in_the_summary_and_none_on_stderr() -> Result<(), BoxError> {
    let host = RunHost::new()?;

    for shared in [false, true] {
        let scope = if shared { "shared" } else { "vault" };
        let (output, document) = host.json(&jobs_run(shared, "degrade", &["--format", "json"])).await?;

        assert_eq!(output.status.code(), Some(0), "{scope}: {}", stderr(&output));
        assert_eq!(field(&document, "outcome"), &json!("degraded"), "{scope}: {document}");
        assert_eq!(
            field(&document, "warnings"),
            &json!({ "kept": [DEGRADED_REASON], "omitted": 0 }),
            "{scope}"
        );
        assert!(
            !stderr(&output).contains(DEGRADED_REASON),
            "{scope}: reasons are no longer written to stderr: {}",
            stderr(&output)
        );
    }
    Ok(())
}

#[tokio::test]
async fn outputs_reach_the_summary_and_never_the_record_or_the_log() -> Result<(), BoxError> {
    let host = RunHost::new()?;

    for shared in [false, true] {
        let scope = if shared { "shared" } else { "vault" };
        let (output, document) = host.json(&jobs_run(shared, "emit", &["--format", "json"])).await?;

        assert_eq!(
            field(&document, "outputs"),
            &json!({ "created": 3, "label": SECRET_OUTPUT }),
            "{scope}"
        );
        let record = if shared {
            host.shared_record("emit", 1)
        } else {
            host.vault_record("emit", 1)
        }
        .ok_or("the run left no record")?;
        // A shared record writes its lists inline, and a vault note's frontmatter is written as YAML block lists.
        let names = if shared {
            "outputs: [\"created\",\"label\"]\n"
        } else {
            "outputs:\n- created\n- label\n"
        };
        assert!(record.contains(names), "{scope}: names only: {record}");
        assert!(!record.contains(SECRET_OUTPUT), "{scope}: {record}");
        assert!(!stderr(&output).contains(SECRET_OUTPUT), "{scope}: {}", stderr(&output));
    }
    Ok(())
}

#[tokio::test]
async fn jobs_run_and_dry_run_take_input_overrides_for_vault_and_shared_jobs() -> Result<(), BoxError> {
    let host = RunHost::new()?;

    for shared in [false, true] {
        let scope = if shared { "shared" } else { "vault" };
        let commands = [
            (
                "jobs run",
                jobs_run(shared, "echo", &["--input", "target=one-off-value", "--format", "json"]),
            ),
            (
                "jobs run --dry-run",
                jobs_run(
                    shared,
                    "echo",
                    &["--dry-run", "--input", "target=one-off-value", "--format", "json"],
                ),
            ),
            (
                "jobs dry-run",
                jobs_dry_run(shared, "echo", &["--input", "target=one-off-value", "--format", "json"]),
            ),
        ];
        for (name, command) in commands {
            let (output, document) = host.json(&command).await?;

            assert_eq!(output.status.code(), Some(0), "{scope} {name}: {}", stderr(&output));
            assert_eq!(
                field(&document["outputs"], "seen"),
                &json!("one-off-value"),
                "{scope} {name}: {document}"
            );
            assert_eq!(
                field(&document, "overridden_inputs"),
                &json!(["target"]),
                "{scope} {name}"
            );
        }
        let (_, plain) = host.json(&jobs_run(shared, "echo", &["--format", "json"])).await?;
        assert_eq!(
            field(&plain["outputs"], "seen"),
            &json!("from-the-job"),
            "{scope}: without an override"
        );
        assert_eq!(field(&plain, "overridden_inputs"), &json!([]), "{scope}");
    }
    Ok(())
}

#[tokio::test]
async fn no_run_command_prints_the_host_data_folder_on_stdout_or_stderr() -> Result<(), BoxError> {
    let host = RunHost::new()?;
    let _shared = host.hold_shared_claim("emit")?;
    let _vault = host.hold_vault_claim("emit")?;
    let data = host.data.to_string_lossy().into_owned();
    let mut commands: Vec<Vec<String>> = refusals().into_iter().map(|case| case.command).collect();
    commands.push(jobs_run(false, "emit", &["--format", "json"]));
    commands.push(jobs_run(true, "emit", &["--format", "json"]));
    commands.push(jobs_run(false, "slow", &["--format", "json"]));
    commands.push(jobs_run(true, "slow", &["--format", "json"]));
    commands.push(jobs_run(false, "fail", &["--format", "json"]));
    commands.push(jobs_run(true, "fail", &["--format", "json"]));
    commands.push(args(&[
        "jobs",
        "run",
        "--shared",
        "acme",
        "emit",
        "missing-job",
        "--format",
        "text",
    ]));
    commands.push(args(&[
        "jobs",
        "run",
        "--shared",
        "acme",
        "no-such-workflow",
        "nightly",
        "--format",
        "json",
    ]));

    // Each runs as given, and again without `--format`, in the default text form at the default log level.
    let mut every = commands.clone();
    every.extend(commands.into_iter().map(|command| {
        let mut plain = Vec::new();
        let mut skip = false;
        for part in command {
            if skip {
                skip = false;
            } else if part == "--format" {
                skip = true;
            } else {
                plain.push(part);
            }
        }
        plain
    }));

    for command in every {
        let output = host.run(&command).await?;

        assert!(
            !stdout(&output).contains(&data),
            "{command:?} prints the data folder: {}",
            stdout(&output)
        );
        assert!(
            !stderr(&output).contains(&data),
            "{command:?} logs the data folder: {}",
            stderr(&output)
        );
    }
    Ok(())
}
