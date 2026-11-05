use tempfile::tempdir;

use clap::Parser;

use super::{Cli, PathBuf, default_web_config_path, resolve_web_config_path};

fn parse(arguments: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once("contextos-web").chain(arguments.iter().copied()))
}

#[test]
fn default_web_config_path_is_under_the_home_directory_config_folder()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let resolved = default_web_config_path().ok_or("home directory unavailable")?;

    let expected_suffix = std::path::Path::new(".config/contextos/web.toml");
    assert!(
        resolved.ends_with(expected_suffix),
        "expected {resolved:?} to end with {expected_suffix:?}"
    );
    assert_eq!(
        resolved,
        contextos_host::HostFolders::detect()
            .ok_or("home directory unavailable")?
            .home()
            .join(".config")
            .join("contextos")
            .join("web.toml")
    );
    Ok(())
}

#[test]
fn resolve_web_config_path_prefers_the_explicit_cli_flag_over_every_fallback()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let explicit = tempdir()?.path().join("chosen.toml");
    let environment = PathBuf::from("/from-env/web.toml");
    let default = PathBuf::from("/default/web.toml");

    let resolved = resolve_web_config_path(Some(explicit.clone()), Some(environment), Some(default))?;

    assert_eq!(resolved, explicit);
    Ok(())
}

#[test]
fn resolve_web_config_path_prefers_the_environment_variable_over_the_default()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let environment = PathBuf::from("/from-env/web.toml");
    let default = PathBuf::from("/default/web.toml");

    let resolved = resolve_web_config_path(None, Some(environment.clone()), Some(default))?;

    assert_eq!(resolved, environment);
    Ok(())
}

#[test]
fn resolve_web_config_path_falls_back_to_the_default_location_with_neither_flag_nor_environment()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let default = PathBuf::from("/default/web.toml");

    let resolved = resolve_web_config_path(None, None, Some(default.clone()))?;

    assert_eq!(resolved, default);
    Ok(())
}

#[test]
fn resolve_web_config_path_errors_when_no_source_determines_a_path() {
    let result = resolve_web_config_path(None, None, None);
    assert!(result.is_err());
}

#[test]
fn a_scenario_without_a_dry_run_is_a_usage_error() {
    for arguments in [
        &["workflows", "run", "va", "sync", "--scenario", "offline"][..],
        &["workflows", "run", "--shared", "acme", "sync", "--scenario", "offline"][..],
    ] {
        let parsed = parse(arguments);

        assert!(
            parsed.as_ref().is_err_and(|error| error.exit_code() == 2),
            "{arguments:?} must be a usage error, clap's own exit status 2: {parsed:?}"
        );
    }
}

#[test]
fn a_one_off_run_takes_a_release_choice_in_either_store() {
    for choice in ["current", "future", "previous", "1.2.3"] {
        for store in [&["va", "sync"][..], &["--shared", "acme", "sync"][..]] {
            let mut arguments = vec!["workflows", "run"];
            arguments.extend_from_slice(store);
            arguments.extend_from_slice(&["--release", choice]);
            let parsed = parse(&arguments);
            assert!(parsed.is_ok(), "{arguments:?}: {parsed:?}");
        }
    }
    let defaulted = parse(&["workflows", "run", "--shared", "acme", "sync"]);
    assert!(defaulted.is_ok(), "the release defaults to current: {defaulted:?}");
    let with_dry_run = parse(&[
        "workflows",
        "run",
        "--shared",
        "acme",
        "sync",
        "--release",
        "1.0.0",
        "--dry-run",
    ]);
    assert!(with_dry_run.is_ok(), "{with_dry_run:?}");
}

#[test]
fn a_release_that_is_not_a_choice_is_a_usage_error() {
    for choice in ["newest", "1.2", "^1.0.0", "1.2.x", "1.2.3-rc.1", ""] {
        let parsed = parse(&["workflows", "run", "va", "sync", "--release", choice]);

        assert!(
            parsed.as_ref().is_err_and(|error| error.exit_code() == 2),
            "{choice:?} must be a usage error: {parsed:?}"
        );
    }
}

#[test]
fn source_runs_a_vault_workflow_and_cannot_be_combined_with_a_release_or_a_shared_package() {
    assert!(parse(&["workflows", "run", "va", "sync", "--source"]).is_ok());
    assert!(parse(&["workflows", "run", "va", "sync", "--source", "--dry-run"]).is_ok());
    for arguments in [
        &["workflows", "run", "va", "sync", "--source", "--release", "current"][..],
        &["workflows", "run", "--shared", "acme", "sync", "--source"][..],
    ] {
        let parsed = parse(arguments);
        assert!(
            parsed.as_ref().is_err_and(|error| error.exit_code() == 2),
            "{arguments:?} must be a usage error: {parsed:?}"
        );
    }
}

#[test]
fn the_retired_package_version_option_is_a_usage_error() {
    let parsed = parse(&[
        "workflows",
        "run",
        "--shared",
        "acme",
        "sync",
        "--package-version",
        "1.0.0",
    ]);

    assert!(parsed.as_ref().is_err_and(|error| error.exit_code() == 2), "{parsed:?}");
}
