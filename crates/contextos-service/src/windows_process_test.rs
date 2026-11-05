use std::path::{Path, PathBuf};

use tempfile::tempdir;

use super::*;
use crate::FakeCommandRunner;
use crate::process_record::ProcessRecord;

/// When the test records say the server wrote its record.
pub(crate) const WRITTEN_AT_MS: u64 = 1_790_000_000_000;
pub(crate) const POWERSHELL: &str = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";

pub(crate) fn windows_spec(dir: &Path) -> ServiceSpec {
    ServiceSpec {
        identity: crate::test_identities::WEB,
        run_args: Vec::new(),
        binary_path: dir.join("contextos-web.exe"),
        config_path: dir.join("web.toml"),
        home_dir: dir.join("home"),
        config_dir: dir.join("config"),
        process_record: dir.join("web.toml.process.json"),
        system_dir: PathBuf::from("C:\\Windows\\System32"),
    }
}

/// Writes a record for a server running `spec`'s executable as process `pid`.
pub(crate) fn write_record(spec: &ServiceSpec, pid: u32) -> Result<(), Box<dyn std::error::Error>> {
    write_record_for(spec, pid, &spec.binary_path)
}

fn write_record_for(spec: &ServiceSpec, pid: u32, exe: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let record = ProcessRecord {
        pid,
        exe: exe.to_path_buf(),
        written_at_ms: WRITTEN_AT_MS,
    };
    std::fs::write(&spec.process_record, serde_json::to_string(&record)?)?;
    Ok(())
}

/// What the process query prints for a live process: its executable and its start time.
pub(crate) fn process_line(executable: &Path, started_ms: u64) -> String {
    format!("{}\r\n{started_ms}\r\n", executable.display())
}

/// [`process_line`] for this service's server, started before it wrote its record.
pub(crate) fn server_line(spec: &ServiceSpec) -> String {
    process_line(&spec.binary_path, WRITTEN_AT_MS - 2_000)
}

/// What a registration query prints when the mechanism runs `spec`'s server.
pub(crate) fn registration_for(spec: &ServiceSpec) -> String {
    format!("{}\r\n", logon_command(spec))
}

/// What every PowerShell call runs before its script.
const SCRIPT_PREFIX: &str =
    "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $ErrorActionPreference = 'Stop'; try { ";
/// What every PowerShell call runs after its script.
const SCRIPT_SUFFIX: &str = " } catch { [Console]::Error.WriteLine($_.Exception.Message); exit 1 }; exit 0";

/// The script a PowerShell call ran, inside the wrapper every call carries.
pub(crate) fn script_of(call: &(String, Vec<String>)) -> &str {
    let script = call.1.last().map_or("", String::as_str);
    script
        .strip_prefix(SCRIPT_PREFIX)
        .and_then(|script| script.strip_suffix(SCRIPT_SUFFIX))
        .unwrap_or("")
}

#[test]
fn every_script_runs_in_utf8_with_an_exit_code_that_only_a_reported_failure_makes_non_zero()
-> Result<(), Box<dyn std::error::Error>> {
    // Windows PowerShell exits 1 after any error, even one a script silenced with `-ErrorAction SilentlyContinue`
    // (looking up a registration that does not exist), and prints nothing. The wrapper makes an unhandled error print
    // its message to standard error and exit 1, and everything else exit 0.
    let dir = tempdir()?;
    let runner = FakeCommandRunner::new();

    printed(&runner, &windows_spec(dir.path()), "'probe'")?;

    let call = &runner.calls()[0];
    assert_eq!(
        call.1.last().map(String::as_str),
        Some(format!("{SCRIPT_PREFIX}'probe'{SCRIPT_SUFFIX}").as_str())
    );
    Ok(())
}

/// Runs a registration lookup the way the Startup App does, through the real Windows PowerShell, for a value no
/// machine has: on a clean install the lookup must report nothing registered rather than fail.
#[cfg(windows)]
#[test]
fn looking_up_a_registration_that_does_not_exist_reports_none_through_the_real_powershell()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let mut spec = windows_spec(dir.path());
    spec.system_dir = std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32"))
        .ok_or("SystemRoot must be set")?;

    let found = printed(
        &crate::SystemCommandRunner,
        &spec,
        "$v = Get-ItemProperty -LiteralPath 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run' \
         -Name 'ContextOS Web test value that never exists' -ErrorAction SilentlyContinue; \
         if ($v) { 'registered' }",
    )?;

    assert_eq!(found, None);
    Ok(())
}

#[test]
fn no_record_means_the_server_is_stopped() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let runner = FakeCommandRunner::new();
    assert_eq!(server_state(&runner, &windows_spec(dir.path()))?, RunState::Stopped);
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
    Ok(())
}

#[test]
fn the_process_query_asks_powershell_in_utf8_for_its_executable_and_start_time()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    write_record(&spec, 4242)?;
    let runner = FakeCommandRunner::new();
    runner.push_success(&server_line(&spec));

    assert_eq!(server_state(&runner, &spec)?, RunState::Running);
    let call = &runner.calls()[0];
    assert_eq!(call.0, POWERSHELL, "by full path");
    assert_eq!(call.1[..3], ["-NoProfile", "-NonInteractive", "-Command"]);
    assert_eq!(
        script_of(call),
        "try { $p = Get-Process -Id 4242 -ErrorAction Stop; $p.Path; \
         [DateTimeOffset]::new($p.StartTime.ToUniversalTime()).ToUnixTimeMilliseconds() } catch { }"
    );
    Ok(())
}

#[test]
fn the_server_is_known_by_the_executable_it_recorded_whichever_copy_runs_the_command()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    let server_exe = PathBuf::from("C:\\Users\\José\\.cargo\\bin\\contextos-web.exe");
    write_record_for(&spec, 4242, &server_exe)?;
    let runner = FakeCommandRunner::new();
    // Reported in another case, and not ASCII: compared as Windows compares paths.
    runner.push_success(&process_line(
        Path::new("C:\\USERS\\JOSÉ\\.CARGO\\BIN\\CONTEXTOS-WEB.EXE"),
        WRITTEN_AT_MS,
    ));

    assert_eq!(
        server_state(&runner, &spec)?,
        RunState::Running,
        "started at the moment it wrote the record"
    );
    Ok(())
}

#[test]
fn a_record_whose_process_is_not_the_server_means_crashed() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    write_record(&spec, 4242)?;
    for (case, printed) in [
        ("gone, or not ours to inspect", String::new()),
        (
            "another program",
            process_line(Path::new("C:\\Windows\\notepad.exe"), WRITTEN_AT_MS - 2_000),
        ),
        (
            "another contextos-web started after the crash, such as this very command",
            process_line(&spec.binary_path, WRITTEN_AT_MS + 1),
        ),
        (
            "a path with no start time",
            format!("{}\r\n", spec.binary_path.display()),
        ),
        (
            "a start time that is not a number",
            format!("{}\r\nsoon\r\n", spec.binary_path.display()),
        ),
    ] {
        let runner = FakeCommandRunner::new();
        runner.push_success(&printed);
        assert_eq!(server_state(&runner, &spec)?, RunState::Crashed, "{case}");
    }
    Ok(())
}

#[test]
fn a_failed_or_missing_powershell_is_an_error_not_a_verdict() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    write_record(&spec, 4242)?;

    let runner = FakeCommandRunner::new();
    runner.push_failure("This language mode does not allow it.");
    let failed = server_state(&runner, &spec);
    assert!(
        matches!(&failed, Err(ServiceError::CommandFailed { program, .. }) if program == POWERSHELL),
        "{failed:?}"
    );

    let runner = FakeCommandRunner::new();
    runner.push_spawn_error();
    let missing = server_state(&runner, &spec);
    assert!(
        matches!(&missing, Err(ServiceError::Spawn { program, .. }) if program == POWERSHELL),
        "{missing:?}"
    );
    Ok(())
}

#[test]
fn a_malformed_record_is_an_error_not_a_stopped_server() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    std::fs::write(&spec.process_record, "not json")?;

    let result = server_state(&FakeCommandRunner::new(), &spec);

    assert!(matches!(result, Err(ServiceError::ProcessRecord(_))), "{result:?}");
    Ok(())
}

#[test]
fn ending_a_running_server_kills_it_waits_for_it_to_exit_and_clears_its_record()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    write_record(&spec, 4242)?;
    let runner = FakeCommandRunner::new();
    runner.push_success(&server_line(&spec));
    runner.push_success("SUCCESS"); // taskkill
    runner.push_success(&server_line(&spec)); // still exiting
    runner.push_success(""); // gone

    end_server(&runner, &spec)?;

    let calls = runner.calls();
    assert_eq!(
        calls[1],
        (
            "C:\\Windows\\System32\\taskkill.exe".to_owned(),
            ["/PID", "4242", "/F"].map(str::to_owned).to_vec()
        )
    );
    assert_eq!(calls.len(), 4, "checked again until it had exited");
    assert!(!spec.process_record.exists());
    Ok(())
}

#[test]
fn a_server_that_exits_before_taskkill_reaches_it_has_still_stopped() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    write_record(&spec, 4242)?;
    let runner = FakeCommandRunner::new();
    runner.push_success(&server_line(&spec));
    runner.push_failure("ERROR: The process \"4242\" not found."); // taskkill
    runner.push_success(""); // gone

    end_server(&runner, &spec)?;

    assert!(!spec.process_record.exists());
    Ok(())
}

#[test]
fn a_server_taskkill_cannot_end_keeps_its_record_and_says_so() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    write_record(&spec, 4242)?;
    let runner = FakeCommandRunner::new();
    runner.push_success(&server_line(&spec));
    runner.push_failure("ERROR: Access is denied.");
    runner.push_success(&server_line(&spec)); // still there

    let result = end_server(&runner, &spec);

    assert!(matches!(result, Err(ServiceError::CommandFailed { .. })), "{result:?}");
    assert!(
        spec.process_record.exists(),
        "the server is still running, so its record stays"
    );
    Ok(())
}

#[test]
fn a_server_that_will_not_exit_is_reported_rather_than_waited_on_for_ever() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    write_record(&spec, 4242)?;
    let runner = FakeCommandRunner::new();
    runner.push_success(&server_line(&spec));
    runner.push_success("SUCCESS"); // taskkill
    for _ in 0..30 {
        runner.push_success(&server_line(&spec));
    }

    let result = end_server(&runner, &spec);

    assert!(
        matches!(result, Err(ServiceError::ServerDidNotExit { pid: 4242, .. })),
        "{result:?}"
    );
    assert!(spec.process_record.exists());
    Ok(())
}

#[test]
fn ending_after_a_crash_only_clears_the_record() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    write_record(&spec, 4242)?;
    let runner = FakeCommandRunner::new();
    runner.push_success(&process_line(&spec.binary_path, WRITTEN_AT_MS + 60_000));

    end_server(&runner, &spec)?;

    let programs: Vec<String> = runner.calls().into_iter().map(|(program, _)| program).collect();
    assert_eq!(
        programs,
        [POWERSHELL],
        "no process to kill: the id belongs to something else by now"
    );
    assert!(!spec.process_record.exists());
    Ok(())
}

#[test]
fn launching_clears_a_stale_record_and_runs_the_server_detached_under_the_console_host()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    write_record(&spec, 4242)?;
    let runner = FakeCommandRunner::new();

    launch(
        &runner,
        &spec,
        &RegisteredCommand::try_from(logon_command(&spec).as_str())?,
    )?;

    assert!(!spec.process_record.exists());
    let binary = spec.binary_path.display().to_string();
    let config = spec.config_path.display().to_string();
    assert_eq!(
        runner.spawned(),
        vec![(
            "C:\\Windows\\System32\\conhost.exe".to_owned(),
            vec!["--headless".to_owned(), binary, "--config".to_owned(), config]
        )]
    );
    Ok(())
}

#[test]
fn a_server_that_cannot_be_launched_is_a_spawn_error_naming_the_console_host() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempdir()?;
    let runner = FakeCommandRunner::new();
    runner.fail_spawns();

    let spec = windows_spec(dir.path());
    let result = launch(
        &runner,
        &spec,
        &RegisteredCommand::try_from(logon_command(&spec).as_str())?,
    );

    assert!(
        matches!(&result, Err(ServiceError::Spawn { program, .. }) if program.ends_with("conhost.exe")),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn the_logon_command_quotes_each_path_as_one_argument_under_the_console_host() {
    let spec = ServiceSpec {
        identity: crate::test_identities::WEB,
        run_args: Vec::new(),
        binary_path: PathBuf::from("C:\\Program Files\\ContextOS\\contextos-web.exe"),
        config_path: PathBuf::from("C:\\Users\\pj\\ContextOS\\web.toml"),
        home_dir: PathBuf::from("C:\\Users\\pj"),
        config_dir: PathBuf::from("C:\\Users\\pj\\AppData\\Roaming"),
        process_record: PathBuf::from("C:\\Users\\pj\\ContextOS\\web.toml.process.json"),
        system_dir: PathBuf::from("C:\\Windows\\System32"),
    };
    assert_eq!(
        logon_command(&spec),
        "\"C:\\Windows\\System32\\conhost.exe\" --headless \"C:\\Program Files\\ContextOS\\contextos-web.exe\" \
         --config \"C:\\Users\\pj\\ContextOS\\web.toml\""
    );
}

#[test]
fn a_command_longer_than_the_limit_is_refused_naming_both_lengths() {
    let spec = windows_spec(Path::new("C:\\Temp"));
    assert!(within_limit(&spec, "short", 260).is_ok());
    let long = "x".repeat(261);
    assert!(matches!(
        within_limit(&spec, &long, 260),
        Err(ServiceError::CommandTooLong {
            length: 261,
            limit: 260,
            ..
        })
    ));
}

#[test]
fn a_registration_for_another_web_toml_is_refused_naming_it() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = ServiceSpec {
        identity: crate::test_identities::WEB,
        run_args: Vec::new(),
        config_path: PathBuf::from("C:\\Users\\José\\AppData\\Roaming\\contextos\\web.toml"),
        ..windows_spec(dir.path())
    };
    assert!(registered_for(&spec, &registration_for(&spec)).is_ok());
    let shouted = registration_for(&spec).to_uppercase();
    assert!(
        registered_for(&spec, &shouted).is_ok(),
        "Windows paths compare without case, accents included"
    );

    let other = ServiceSpec {
        identity: crate::test_identities::WEB,
        run_args: Vec::new(),
        config_path: dir.path().join("other.toml"),
        ..windows_spec(dir.path())
    };
    let result = registered_for(&spec, &registration_for(&other));
    assert!(
        matches!(
            &result,
            Err(ServiceError::RegisteredForAnotherConfig { registration, .. }) if registration.contains("other.toml")
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn quote_windows_arg_leaves_a_plain_value_wrapped_but_otherwise_unescaped() {
    assert_eq!(
        quote_windows_arg("C:\\Program Files\\ContextOS\\contextos-web.exe"),
        "\"C:\\Program Files\\ContextOS\\contextos-web.exe\""
    );
}

/// The inverse of [`quote_windows_arg`], applied only to a string this
/// module's own encoder produced: decodes one already-delimited quoted
/// argument back to the value it was built from, following the same
/// `CommandLineToArgvW` backslash-run rule in reverse. Used to prove a round
/// trip for inputs containing an embedded quote, instead of hand-transcribing
/// their doubly-escaped expected literal (itself an error-prone exercise the
/// encoder exists to spare a config file's *content* from).
fn decode_windows_arg(quoted: &str) -> String {
    // `quote_windows_arg` always wraps its result in a leading and trailing
    // quote; falling back to the untouched input on a malformed argument
    // (rather than panicking here) still fails the round-trip `assert_eq!`
    // this helper serves, just with a less specific message.
    let Some(inner) = quoted.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')) else {
        return quoted.to_owned();
    };
    let mut decoded = String::with_capacity(inner.len());
    let mut backslashes = 0usize;
    for character in inner.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                for _ in 0..backslashes / 2 {
                    decoded.push('\\');
                }
                decoded.push('"');
                backslashes = 0;
            }
            other => {
                for _ in 0..backslashes {
                    decoded.push('\\');
                }
                decoded.push(other);
                backslashes = 0;
            }
        }
    }
    // Trailing backslashes right before the (already-stripped) closing
    // delimiter were doubled by the encoder for the same reason as ones
    // before an embedded quote: halve them back, mirroring the `'"'` arm
    // above rather than the `other` arm's one-for-one count.
    for _ in 0..backslashes / 2 {
        decoded.push('\\');
    }
    decoded
}

#[test]
fn quote_windows_arg_round_trips_a_value_containing_an_embedded_quote() {
    // A value that already carries one layer of quoting (for example a
    // manually-typed path `parse_manual_path` failed to unwrap, or one
    // pasted from a source this crate does not control) must not corrupt
    // the argument boundary.
    let value = "C:\\Users\\pj\\\"quoted\"\\config.toml";
    assert_eq!(decode_windows_arg(&quote_windows_arg(value)), value);
}

#[test]
fn quote_windows_arg_round_trips_a_value_ending_in_a_backslash() {
    // The bug this replaces a naive `format!("\"{}\"", ...)` for: a
    // directory reference ending in `\` would otherwise produce `...\"`,
    // which `CommandLineToArgvW` reads as an escaped literal quote rather
    // than the argument's closing delimiter, silently merging the rest of
    // the command line into this one argument.
    let value = "C:\\Program Files\\ContextOS\\";
    assert_eq!(decode_windows_arg(&quote_windows_arg(value)), value);
}

#[test]
fn quote_windows_arg_round_trips_a_value_with_several_consecutive_embedded_quotes() {
    let value = "a\"\"b\\\"c";
    assert_eq!(decode_windows_arg(&quote_windows_arg(value)), value);
}

#[test]
fn a_logon_command_reads_back_as_the_program_and_arguments_it_was_built_from() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempdir()?;
    let awkward = [
        "--http",
        "127.0.0.1:7331",
        "--no-stdio",
        "with space",
        "trailing\\",
        "quote\"inside",
        "back\\\\\"slash",
        "",
    ];
    let mut spec = windows_spec(dir.path());
    spec.binary_path = PathBuf::from("C:\\Program Files\\ContextOS\\contextos.exe");
    spec.config_path = PathBuf::from("C:\\Users\\pj\\My Config\\");
    spec.run_args = awkward.iter().map(|arg| (*arg).to_owned()).collect();

    let registered = RegisteredCommand::try_from(logon_command(&spec).as_str())?;

    assert_eq!(registered.program, system_tool(&spec, "conhost.exe"));
    let mut expected = vec![
        "--headless".to_owned(),
        spec.binary_path.display().to_string(),
        "--config".to_owned(),
        spec.config_path.display().to_string(),
    ];
    expected.extend(spec.run_args.iter().cloned());
    assert_eq!(registered.args, expected);
    Ok(())
}

#[test]
fn a_logon_command_with_an_unclosed_quote_is_not_read_back() {
    for registration in [
        "",
        "   ",
        "\"C:\\Windows\\System32\\conhost.exe --headless",
        "conhost.exe \"--config",
    ] {
        assert!(
            RegisteredCommand::try_from(registration).is_err(),
            "{registration:?} should not read back"
        );
    }
}
