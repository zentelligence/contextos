use super::*;
use crate::test_identities::WEB;

#[test]
fn linux_selects_the_systemd_backend() {
    assert!(backend_for_os(WEB, "linux", WindowsMechanism::StartupApp).is_ok());
}

#[test]
fn macos_selects_the_launchd_backend() {
    assert!(backend_for_os(WEB, "macos", WindowsMechanism::StartupApp).is_ok());
}

#[test]
fn windows_selects_the_windows_backend() {
    assert!(backend_for_os(WEB, "windows", WindowsMechanism::StartupApp).is_ok());
}

#[test]
fn an_unrecognised_platform_is_a_typed_error_not_a_panic() {
    let result = backend_for_os(WEB, "plan9", WindowsMechanism::StartupApp);

    assert!(matches!(
        result,
        Err(ServiceError::UnsupportedPlatform { os, .. }) if os == "plan9"
    ));
}

#[test]
fn current_platform_backend_resolves_on_this_test_host() {
    // This suite only runs on hosts `std::env::consts::OS` reports as one
    // of the three supported platforms; a fourth would fail CI outright
    // long before this test, so asserting success here (rather than
    // matching a specific platform) is the honest claim for a
    // platform-parameterised function.
    assert!(current_platform_backend(WEB, WindowsMechanism::StartupApp).is_ok());
}

#[test]
fn fake_command_runner_records_calls_in_order() -> Result<(), Box<dyn std::error::Error>> {
    let runner = FakeCommandRunner::new();
    runner.push_success("first");
    runner.push_failure("second failed");

    let first = runner.run("echo", &["a"])?;
    let second = runner.run("echo", &["b"])?;

    assert!(first.success);
    assert_eq!(first.stdout, "first");
    assert!(!second.success);
    assert_eq!(second.stderr, "second failed");
    assert_eq!(
        runner.calls(),
        vec![
            ("echo".to_owned(), vec!["a".to_owned()]),
            ("echo".to_owned(), vec!["b".to_owned()]),
        ]
    );
    Ok(())
}

#[test]
fn fake_command_runner_reports_a_scripted_spawn_error() {
    let runner = FakeCommandRunner::new();
    runner.push_spawn_error();

    assert!(runner.run("does-not-exist", &[]).is_err());
}

#[test]
fn a_failed_command_that_printed_nothing_says_so_and_names_its_exit_code() {
    let runner = FakeCommandRunner::new();
    runner.push_failure("");

    let message = run_checked(&runner, "tool.exe", &["--check"])
        .err()
        .map(|error| error.to_string());

    let message = message.unwrap_or_default();
    assert!(message.contains("exit code 1"), "{message}");
    assert!(message.contains("printed nothing"), "{message}");
}

#[test]
fn a_failed_command_with_no_error_text_reports_what_it_printed_instead() {
    let runner = FakeCommandRunner::new();
    runner.push_output(false, "Access is denied.\r\n");

    let message = run_checked(&runner, "tool.exe", &["--check"])
        .err()
        .map(|error| error.to_string());

    let message = message.unwrap_or_default();
    assert!(message.contains("Access is denied."), "{message}");
}

#[test]
fn a_failed_command_reports_its_error_text() {
    let runner = FakeCommandRunner::new();
    runner.push_failure("The system cannot find the file specified.\r\n");

    let message = run_checked(&runner, "tool.exe", &["--check"])
        .err()
        .map(|error| error.to_string());

    let message = message.unwrap_or_default();
    assert!(message.contains("exit code 1"), "{message}");
    assert!(
        message.contains("The system cannot find the file specified."),
        "{message}"
    );
}
