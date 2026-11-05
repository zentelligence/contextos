use clap::Parser;

use super::*;
use crate::job_commands::JobsAction;

fn parse(arguments: &[&str]) -> Result<SuccessorArguments, clap::Error> {
    let cli = crate::Cli::try_parse_from(
        ["contextos-web", "jobs", "successor"]
            .into_iter()
            .chain(arguments.iter().copied()),
    )?;
    match cli.command {
        Some(crate::Command::Jobs {
            action: JobsAction::Successor(arguments),
        }) => Ok(arguments),
        other => Err(clap::Error::raw(
            clap::error::ErrorKind::InvalidValue,
            format!("not a successor command: {other:?}"),
        )),
    }
}

fn asked(arguments: &[&str]) -> Result<Asked, Box<dyn std::error::Error>> {
    Ok(Asked::try_from(&parse(arguments)?)?)
}

fn one(job: &str, new_job: &str) -> SuccessorScope {
    SuccessorScope::One {
        job: job.to_owned(),
        new_job: new_job.to_owned(),
    }
}

#[test]
fn a_vault_successor_names_the_vault_package_workflow_job_and_new_job() -> Result<(), Box<dyn std::error::Error>> {
    let found = asked(&["va", "ops", "sync", "nightly", "nightly-next"])?;
    assert_eq!(found.place, Where::Vault("va".to_owned()));
    assert_eq!(found.request.package, "ops");
    assert_eq!(found.request.workflow, "sync");
    assert_eq!(found.request.scope, one("nightly", "nightly-next"));
    assert_eq!(found.request.to, SuccessorTarget::Future);
    Ok(())
}

#[test]
fn a_shared_successor_leaves_out_the_vault() -> Result<(), Box<dyn std::error::Error>> {
    let found = asked(&["--shared", "ops", "sync", "nightly", "nightly-next"])?;
    assert_eq!(found.place, Where::Shared);
    assert_eq!(found.request.package, "ops");
    assert_eq!(found.request.scope, one("nightly", "nightly-next"));
    Ok(())
}

#[test]
fn all_leaves_out_the_job_and_the_new_job() -> Result<(), Box<dyn std::error::Error>> {
    let vault = asked(&["va", "ops", "sync", "--all"])?;
    assert_eq!(vault.place, Where::Vault("va".to_owned()));
    assert_eq!(vault.request.scope, SuccessorScope::All);
    let shared = asked(&["--shared", "ops", "sync", "--all"])?;
    assert_eq!(shared.place, Where::Shared);
    assert_eq!(shared.request.workflow, "sync");
    assert_eq!(shared.request.scope, SuccessorScope::All);
    Ok(())
}

#[test]
fn to_names_future_previous_or_an_exact_version() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        asked(&["va", "ops", "sync", "--all", "--to", "previous"])?.request.to,
        SuccessorTarget::Previous
    );
    assert_eq!(
        asked(&["va", "ops", "sync", "--all", "--to", "2.1.0"])?.request.to,
        SuccessorTarget::Version(semver::Version::new(2, 1, 0))
    );
    assert!(parse(&["va", "ops", "sync", "--all", "--to", "^2"]).is_err());
    Ok(())
}

#[test]
fn the_wrong_number_of_words_is_not_understood() {
    for (words, flags) in [
        (&["va", "ops", "sync", "nightly"][..], &[][..]),
        (&["ops", "sync", "nightly", "next", "extra"][..], &["--shared"][..]),
        (&["ops", "sync", "nightly"][..], &["--shared"][..]),
        (&["va", "ops", "sync", "nightly"][..], &["--all"][..]),
        (&["ops", "sync", "nightly"][..], &["--shared", "--all"][..]),
    ] {
        let arguments: Vec<&str> = words.iter().chain(flags).copied().collect();
        let parsed = parse(&arguments);
        let error = parsed.map(|parsed| Asked::try_from(&parsed));
        assert!(matches!(error, Ok(Err(_))), "{arguments:?} was accepted: {error:?}");
    }
}

#[test]
fn more_words_than_any_form_takes_is_refused_by_the_parser() {
    assert!(parse(&["va", "ops", "sync", "nightly", "next", "extra"]).is_err());
    assert!(parse(&["only-one"]).is_err());
}
