use tempfile::tempdir;

use crate::Config;
use crate::doctor::{DoctorCheck, DoctorReport, DoctorStatus};

const ONE_CHIEF: &str = "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\nchief_of_staff = true\n\
                         [[member]]\nslug = \"vela\"\nname = \"Vela\"\n";

/// As `Config::try_from`'s own conversion, with every vault's state directory inside its own root, so a throwaway vault
/// never leaves state under the operator's home directory.
fn scoped_config(paths: Vec<std::path::PathBuf>) -> Result<Config, Box<dyn std::error::Error>> {
    let mut config = Config::try_from(paths)?;
    for vault in &mut config.vaults {
        vault.state_directory = Some(std::path::PathBuf::from(".contextos"));
    }
    Ok(config)
}

/// What the vault's registry holds, which decides whether the vault's jobs depend on a team.
#[derive(Clone, Copy)]
enum Registry {
    /// No workflow at all.
    Empty,
    /// A workflow with no saved job.
    WorkflowOnly,
    /// A workflow with one saved job.
    WithAJob,
}

fn team_check(team: Option<&str>, agents: Option<&str>) -> Result<DoctorCheck, Box<dyn std::error::Error>> {
    team_check_in(team, agents, Registry::Empty)
}

/// The team check of a vault with one saved job, which depends on the team.
fn team_check_with_a_job(team: Option<&str>) -> Result<DoctorCheck, Box<dyn std::error::Error>> {
    team_check_in(team, None, Registry::WithAJob)
}

fn team_check_in(
    team: Option<&str>,
    agents: Option<&str>,
    registry: Registry,
) -> Result<DoctorCheck, Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir_all(vault.join(".contextos"))?;
    if !matches!(registry, Registry::Empty) {
        std::fs::create_dir_all(vault.join("registry/workflows/crm/sync/jobs"))?;
        std::fs::write(
            vault.join("registry/workflows/crm/workflows.toml"),
            "workflows = [\"sync\"]\n",
        )?;
        std::fs::write(
            vault.join("registry/workflows/crm/sync/workflow.toml"),
            "name = \"Sync\"\nentry = \"sync.rhai\"\n",
        )?;
    }
    if matches!(registry, Registry::WithAJob) {
        std::fs::write(
            vault.join("registry/workflows/crm/sync/jobs/nightly.toml"),
            "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        )?;
    }
    if let Some(team) = team {
        std::fs::write(vault.join(".contextos/team.toml"), team)?;
    }
    if let Some(agents) = agents {
        std::fs::write(vault.join(".contextos/agents.toml"), agents)?;
    }
    let mut config = scoped_config(vec![vault])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].git.enabled = false;
    let report = DoctorReport::try_from(&config)?;
    report
        .checks
        .into_iter()
        .find(|check| check.subject == "Team configuration")
        .ok_or_else(|| "no Team configuration check reported".into())
}

#[test]
fn a_team_without_exactly_one_chief_of_staff_fails_team_config_naming_the_rule()
-> Result<(), Box<dyn std::error::Error>> {
    let none = team_check(
        Some("[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\n[[member]]\nslug = \"vela\"\nname = \"Vela\"\n"),
        None,
    )?;
    let several = team_check(
        Some(
            "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\nchief_of_staff = true\n\
             [[member]]\nslug = \"vela\"\nname = \"Vela\"\nchief_of_staff = true\n",
        ),
        None,
    )?;

    assert_eq!(none.status, DoctorStatus::Fail, "{}", none.message);
    assert!(none.message.contains("no-chief-of-staff"), "{}", none.message);
    assert_eq!(several.status, DoctorStatus::Fail, "{}", several.message);
    assert!(
        several.message.contains("several-chiefs-of-staff"),
        "{}",
        several.message
    );
    assert!(!several.auto_fixable, "the team is the operator's to correct");
    assert!(
        several
            .action
            .as_deref()
            .unwrap_or_default()
            .contains(".contextos/team.toml"),
        "{:?}",
        several.action
    );
    Ok(())
}

#[test]
fn a_team_that_does_not_parse_fails_without_quoting_member_content() -> Result<(), Box<dyn std::error::Error>> {
    let check = team_check(
        Some("[[member]]\nslug = \"secret-member-slug\"\nname = \"Secret Member Name\"\nchief_of_staff = [oops\n"),
        None,
    )?;

    assert_eq!(check.status, DoctorStatus::Fail, "{}", check.message);
    assert!(check.message.contains(".contextos/team.toml"), "{}", check.message);
    for private in ["secret-member-slug", "Secret Member Name"] {
        assert!(!check.message.contains(private), "{}", check.message);
        assert!(!check.action.as_deref().unwrap_or_default().contains(private));
    }
    Ok(())
}

#[test]
fn an_absent_team_passes_with_an_advisory_in_a_vault_without_jobs_and_a_valid_team_passes()
-> Result<(), Box<dyn std::error::Error>> {
    let absent = team_check(None, None)?;
    let valid = team_check(Some(ONE_CHIEF), None)?;

    // The doctor reports Pass or Fail only, so an absent team is a pass that says what a vault with jobs needs.
    assert_eq!(absent.status, DoctorStatus::Pass, "{}", absent.message);
    assert!(absent.message.contains("no .contextos/team.toml"), "{}", absent.message);
    assert!(
        absent.action.is_some(),
        "an advisory says to add one before the vault has jobs"
    );
    assert_eq!(valid.status, DoctorStatus::Pass, "{}", valid.message);
    assert!(valid.message.contains("chief of staff"), "{}", valid.message);
    Ok(())
}

#[test]
fn a_vault_with_a_job_and_no_team_file_fails_team_config() -> Result<(), Box<dyn std::error::Error>> {
    // A valid team first, so the failure below is the missing file and not the job.
    let valid = team_check_with_a_job(Some(ONE_CHIEF))?;
    assert_eq!(valid.status, DoctorStatus::Pass, "{}", valid.message);

    let absent = team_check_with_a_job(None)?;

    assert_eq!(absent.status, DoctorStatus::Fail, "{}", absent.message);
    assert!(absent.message.contains("file-absent"), "{}", absent.message);
    assert!(!absent.auto_fixable, "the team is the operator's to write");
    assert!(
        absent
            .action
            .as_deref()
            .unwrap_or_default()
            .contains(".contextos/team.toml"),
        "{:?}",
        absent.action
    );
    Ok(())
}

#[test]
fn a_vault_with_a_job_and_an_invalid_team_file_fails_team_config() -> Result<(), Box<dyn std::error::Error>> {
    let no_chief = team_check_with_a_job(Some(
        "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\n[[member]]\nslug = \"vela\"\nname = \"Vela\"\n",
    ))?;
    let unparsable = team_check_with_a_job(Some("[[member]\nslug = "))?;

    assert_eq!(no_chief.status, DoctorStatus::Fail, "{}", no_chief.message);
    assert!(no_chief.message.contains("no-chief-of-staff"), "{}", no_chief.message);
    assert_eq!(unparsable.status, DoctorStatus::Fail, "{}", unparsable.message);
    assert!(unparsable.message.contains("file-unparsable"), "{}", unparsable.message);
    Ok(())
}

#[test]
fn the_agents_toml_migration_finding_is_unchanged() -> Result<(), Box<dyn std::error::Error>> {
    let check = team_check(None, Some("[[agent]]\nslug = \"atlas\"\n"))?;

    assert_eq!(check.status, DoctorStatus::Fail);
    assert!(check.message.contains("agents.toml"), "{}", check.message);
    assert!(check.message.contains("team.toml"), "{}", check.message);
    assert!(!check.auto_fixable);
    Ok(())
}

#[test]
fn a_team_file_with_no_members_fails_team_config_naming_the_empty_team_and_not_the_missing_chief()
-> Result<(), Box<dyn std::error::Error>> {
    let check = team_check(Some("# nobody yet\n"), None)?;

    assert_eq!(check.status, DoctorStatus::Fail, "{}", check.message);
    assert!(check.message.contains("team-empty"), "{}", check.message);
    assert!(!check.message.contains("no-chief-of-staff"), "{}", check.message);
    Ok(())
}

#[test]
fn a_vault_with_a_workflow_but_no_saved_job_and_no_team_file_passes_with_the_advisory()
-> Result<(), Box<dyn std::error::Error>> {
    let check = team_check_in(None, None, Registry::WorkflowOnly)?;

    assert_eq!(
        check.status,
        DoctorStatus::Pass,
        "a team is needed once there is a job, not a workflow: {}",
        check.message
    );
    assert!(check.message.contains("no .contextos/team.toml"), "{}", check.message);
    assert!(check.action.is_some(), "the advisory says when a team is needed");
    Ok(())
}
