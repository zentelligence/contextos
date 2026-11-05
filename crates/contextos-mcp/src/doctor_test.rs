use tempfile::tempdir;

use crate::Config;

use contextos_core::TimeZoneSource;

use super::{DoctorReport, DoctorStatus, Remediation, configuration_check};
use crate::doctor_kind::DoctorKind;

/// As [`Config::try_from`]'s own `Vec<PathBuf>` conversion, with every
/// vault's `state_directory` pinned inside its own root rather than
/// left at that conversion's own real, platform-local app-data
/// default: `state_dir.rs`'s own doc covers why that default is right
/// for a real vault, but every one of these tests' own throwaway
/// `tempdir()` vault would otherwise hash to a distinct, permanent
/// directory under the operator's real home directory that nothing
/// ever cleans up.
fn scoped_config(paths: Vec<std::path::PathBuf>) -> Result<Config, Box<dyn std::error::Error>> {
    let mut config = Config::try_from(paths)?;
    for vault in &mut config.vaults {
        vault.state_directory = Some(std::path::PathBuf::from(".contextos"));
    }
    Ok(config)
}

#[test]
fn frontmatter_validity_reports_files_that_fail_strict_yaml_parsing() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    std::fs::write(vault.join("valid.md"), "---\ntitle: Fine\n---\nBody\n")?;
    std::fs::write(vault.join("no-frontmatter.md"), "# Just a heading\n")?;
    // The operator's original real-world case: an unquoted colon inside
    // a text value, which strict YAML treats as an unexpected nested
    // mapping rather than a plain scalar.
    std::fs::write(
        vault.join("broken.md"),
        "---\ntitle: Notes: something happened\n---\nBody\n",
    )?;
    let mut config = scoped_config(vec![vault])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].git.enabled = false;

    let report = DoctorReport::try_from(&config)?;

    let check = report
        .checks
        .iter()
        .find(|check| check.subject == "Frontmatter validity")
        .ok_or("no Frontmatter validity check reported")?;
    assert_eq!(check.status, super::DoctorStatus::Fail);
    assert!(!check.auto_fixable);
    assert_eq!(check.remediation, None);
    assert!(check.message.contains("broken.md"), "{}", check.message);
    assert!(!check.message.contains("valid.md"), "{}", check.message);
    assert!(!check.message.contains("no-frontmatter.md"), "{}", check.message);
    Ok(())
}

#[test]
fn frontmatter_validity_passes_a_vault_with_only_valid_or_absent_frontmatter() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    std::fs::write(vault.join("valid.md"), "---\ntitle: Fine\n---\nBody\n")?;
    std::fs::write(vault.join("no-frontmatter.md"), "# Just a heading\n")?;
    let mut config = scoped_config(vec![vault])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].git.enabled = false;

    let report = DoctorReport::try_from(&config)?;

    let check = report
        .checks
        .iter()
        .find(|check| check.subject == "Frontmatter validity")
        .ok_or("no Frontmatter validity check reported")?;
    assert_eq!(check.status, super::DoctorStatus::Pass);
    Ok(())
}

#[test]
fn a_double_moustache_placeholder_is_not_reported_as_invalid_frontmatter() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    // The real case found scanning an operator vault: an unquoted `{{`
    // is otherwise indistinguishable from opening a YAML flow mapping,
    // so a template using the `{{...}}` placeholder convention (the
    // same one the plugin's `vault_lint.py`
    // `UNRESOLVED_PLACEHOLDER_RE` recognises) fails strict parsing on
    // syntax alone, not on any real structural problem.
    std::fs::write(
        vault.join("template.md"),
        "---\ntitle: {{Human-readable claim title, stated as an assertion}}\nstatus: proposed\n---\nBody\n",
    )?;
    let mut config = scoped_config(vec![vault])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].git.enabled = false;

    let report = DoctorReport::try_from(&config)?;

    let check = report
        .checks
        .iter()
        .find(|check| check.subject == "Frontmatter validity")
        .ok_or("no Frontmatter validity check reported")?;
    assert_eq!(check.status, super::DoctorStatus::Pass, "{}", check.message);
    Ok(())
}

#[test]
fn a_genuine_yaml_error_beside_a_placeholder_is_still_reported() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    // The placeholder substitution must not blanket-forgive a file just
    // because it contains `{{...}}` somewhere: a genuinely broken,
    // unrelated line must still be caught.
    std::fs::write(
        vault.join("template.md"),
        "---\ntitle: {{Placeholder}}\nsummary: Notes: something happened\n---\nBody\n",
    )?;
    let mut config = scoped_config(vec![vault])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].git.enabled = false;

    let report = DoctorReport::try_from(&config)?;

    let check = report
        .checks
        .iter()
        .find(|check| check.subject == "Frontmatter validity")
        .ok_or("no Frontmatter validity check reported")?;
    assert_eq!(check.status, super::DoctorStatus::Fail);
    assert!(check.message.contains("template.md"), "{}", check.message);
    Ok(())
}

#[test]
fn a_placeholder_followed_by_trailing_literal_text_is_not_reported() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    // Real case found live against the operator's vault
    // (templates/registry-hat.md): the placeholder is not the whole
    // value, just part of it. A quoted replacement ("placeholder")
    // would leave trailing unquoted text after the closing quote,
    // which is itself invalid YAML.
    std::fs::write(vault.join("template.md"), "---\nname: {{Hat Name}} Hat\n---\nBody\n")?;
    let mut config = scoped_config(vec![vault])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].git.enabled = false;

    let report = DoctorReport::try_from(&config)?;

    let check = report
        .checks
        .iter()
        .find(|check| check.subject == "Frontmatter validity")
        .ok_or("no Frontmatter validity check reported")?;
    assert_eq!(check.status, super::DoctorStatus::Pass, "{}", check.message);
    Ok(())
}

#[test]
fn a_placeholder_nested_inside_an_already_quoted_string_is_not_reported() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    // Real case found live against the operator's vault
    // (templates/insight-note.md): the placeholder sits inside an
    // already-quoted wikilink string. A quoted replacement would
    // inject `"` characters mid-string, breaking the quoting.
    std::fs::write(
        vault.join("template.md"),
        "---\nrelated:\n  - \"[[{{related-insight-slug}}]]\"\n---\nBody\n",
    )?;
    let mut config = scoped_config(vec![vault])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].git.enabled = false;

    let report = DoctorReport::try_from(&config)?;

    let check = report
        .checks
        .iter()
        .find(|check| check.subject == "Frontmatter validity")
        .ok_or("no Frontmatter validity check reported")?;
    assert_eq!(check.status, super::DoctorStatus::Pass, "{}", check.message);
    Ok(())
}

#[test]
fn the_exact_real_vault_templates_that_first_exposed_this_are_now_valid() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    // Verbatim content of the two files that were still failing after
    // the first placeholder-substitution fix, fetched live from the
    // operator's vault (templates/insight-note.md,
    // templates/registry-hat.md), not a simplified reproduction.
    std::fs::write(
        vault.join("insight-note.md"),
        "---\ntitle: {{Human-readable claim title, stated as an assertion}}\ntype: insight\nstatus: proposed\nid: {{slug}}\ncreated: {{YYYY-MM-DD}}\nupdated: {{YYYY-MM-DD}}\ndomain:\n  - {{lowercase-hyphenated-domain}}\nkind: {{principle | pattern | heuristic | reframe}}\nconfidence: {{low | medium | high}}\nsources:\n  - {{memory/raw/YYYY/MM/YYYY-MM-DD-slug.md}}\nrelated:\n  - \"[[{{related-insight-slug}}]]\"\ntags:\n  - insight/{{domain}}\n---\n\nBody.\n",
    )?;
    std::fs::write(
        vault.join("registry-hat.md"),
        "---\nname: {{Hat Name}} Hat\ndescription: {{One sentence: what task phase this hat covers and what it produces.}}\n---\n\nBody.\n",
    )?;
    let mut config = scoped_config(vec![vault])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].git.enabled = false;

    let report = DoctorReport::try_from(&config)?;

    let check = report
        .checks
        .iter()
        .find(|check| check.subject == "Frontmatter validity")
        .ok_or("no Frontmatter validity check reported")?;
    assert_eq!(check.status, super::DoctorStatus::Pass, "{}", check.message);
    Ok(())
}

#[test]
fn stale_or_missing_managed_index_is_classified_auto_fixable() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    let mut config = scoped_config(vec![vault])?;
    config.vaults[0].git.enabled = false;

    let report = DoctorReport::try_from(&config)?;

    let check = report
        .checks
        .iter()
        .find(|check| check.subject == "Managed indexes")
        .ok_or("no Managed indexes check reported")?;
    assert_eq!(check.status, super::DoctorStatus::Fail);
    assert!(check.auto_fixable);
    assert_eq!(check.remediation, Some(Remediation::RebuildIndexes));
    Ok(())
}

#[test]
fn absent_git_repository_is_classified_auto_fixable() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    let mut config = scoped_config(vec![vault])?;
    config.vaults[0].index_md.enabled = false;

    let report = DoctorReport::try_from(&config)?;

    let check = report
        .checks
        .iter()
        .find(|check| check.subject == "Git recovery")
        .ok_or("no Git recovery check reported")?;
    assert_eq!(check.status, super::DoctorStatus::Fail);
    assert!(check.auto_fixable);
    assert_eq!(check.remediation, Some(Remediation::InitialiseGit));
    let action = check
        .action
        .as_deref()
        .ok_or("no action reported for the missing Git repository")?;
    assert!(
        action.contains("automatic commits"),
        "action must disclose that git_init starts auto-commits going forward, was: {action}"
    );
    Ok(())
}

#[test]
fn each_per_vault_check_is_tagged_with_its_configured_vault_index() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let healthy = fixture.path().join("healthy");
    let stale = fixture.path().join("stale");
    std::fs::create_dir(&healthy)?;
    std::fs::create_dir(&stale)?;
    let mut config = scoped_config(vec![healthy, stale])?;
    for vault in &mut config.vaults {
        vault.git.enabled = false;
    }
    config.vaults[0].index_md.enabled = false;

    let report = DoctorReport::try_from(&config)?;

    let healthy_managed = report
        .checks
        .iter()
        .find(|check| check.subject == "Managed indexes" && check.vault_index == Some(0))
        .ok_or("no vault-0 Managed indexes check reported")?;
    assert_eq!(healthy_managed.status, super::DoctorStatus::Pass);

    let stale_managed = report
        .checks
        .iter()
        .find(|check| check.subject == "Managed indexes" && check.vault_index == Some(1))
        .ok_or("no vault-1 Managed indexes check reported")?;
    assert_eq!(stale_managed.status, super::DoctorStatus::Fail);
    assert!(stale_managed.auto_fixable);

    let configuration = report
        .checks
        .iter()
        .find(|check| check.subject == "Configuration")
        .ok_or("no Configuration check reported")?;
    assert_eq!(configuration.vault_index, None);
    Ok(())
}

#[test]
fn semantic_search_misconfiguration_is_not_auto_fixable() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    let mut config = scoped_config(vec![vault])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].git.enabled = false;
    config.vaults[0].search.semantic = true;

    let report = DoctorReport::try_from(&config)?;

    let check = report
        .checks
        .iter()
        .find(|check| check.subject == "Semantic search")
        .ok_or("no Semantic search check reported")?;
    assert_eq!(check.status, super::DoctorStatus::Fail);
    assert!(!check.auto_fixable);
    assert_eq!(check.remediation, None);
    Ok(())
}

#[test]
fn a_lingering_pre_rename_agents_toml_with_no_team_toml_is_flagged_not_auto_fixable()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir_all(vault.join(".contextos"))?;
    std::fs::write(
        vault.join(".contextos/agents.toml"),
        "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\n",
    )?;
    let mut config = scoped_config(vec![vault])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].git.enabled = false;

    let report = DoctorReport::try_from(&config)?;

    let check = report
        .checks
        .iter()
        .find(|check| check.subject == "Team configuration")
        .ok_or("no Team configuration check reported")?;
    assert_eq!(check.status, super::DoctorStatus::Fail);
    assert!(!check.auto_fixable);
    assert_eq!(check.remediation, None);
    assert!(check.message.contains("agents.toml"), "{}", check.message);
    assert!(check.message.contains("team.toml"), "{}", check.message);
    Ok(())
}

#[test]
fn a_vault_with_only_team_toml_passes_the_migration_check() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir_all(vault.join(".contextos"))?;
    std::fs::write(
        vault.join(".contextos/team.toml"),
        "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\nchief_of_staff = true\n",
    )?;
    let mut config = scoped_config(vec![vault])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].git.enabled = false;

    let report = DoctorReport::try_from(&config)?;

    let check = report
        .checks
        .iter()
        .find(|check| check.subject == "Team configuration")
        .ok_or("no Team configuration check reported")?;
    assert_eq!(check.status, super::DoctorStatus::Pass);
    Ok(())
}

#[test]
fn a_vault_with_neither_file_passes_the_migration_check() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    let mut config = scoped_config(vec![vault])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].git.enabled = false;

    let report = DoctorReport::try_from(&config)?;

    let check = report
        .checks
        .iter()
        .find(|check| check.subject == "Team configuration")
        .ok_or("no Team configuration check reported")?;
    assert_eq!(check.status, super::DoctorStatus::Pass);
    Ok(())
}

#[test]
fn the_configuration_check_names_the_configured_time_zone_and_its_offset() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = scoped_config(vec![vault.path().to_path_buf()])?;
    config.server.timezone = Some("Australia/Sydney".to_owned());

    let report = DoctorReport::try_from(&config)?;
    let check = report
        .checks
        .iter()
        .find(|check| check.kind == DoctorKind::Configuration)
        .ok_or("no configuration check")?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    // Sydney keeps daylight saving, so the offset reported is whichever it has now.
    assert!(
        ["+10:00", "+11:00"].iter().any(|offset| check
            .message
            .contains(&format!("time zone Australia/Sydney (configured, {offset})"))),
        "{}",
        check.message
    );
    Ok(())
}

#[test]
fn an_undetectable_host_time_zone_fails_the_configuration_check_with_a_fix() {
    let check = configuration_check(TimeZoneSource::Undetected, Some("UTC"), time::UtcOffset::UTC);

    assert_eq!(check.status, DoctorStatus::Fail);
    assert!(check.message.contains("UTC"), "{}", check.message);
    assert!(
        check
            .action
            .as_deref()
            .is_some_and(|action| action.contains("[server] timezone")),
        "{:?}",
        check.action
    );
    assert!(!check.auto_fixable);
}

#[test]
fn a_host_time_zone_passes_the_configuration_check_and_says_where_it_came_from() {
    let offset = time::UtcOffset::from_hms(9, 30, 0).unwrap_or(time::UtcOffset::UTC);

    let check = configuration_check(TimeZoneSource::System, Some("Australia/Darwin"), offset);

    assert_eq!(check.status, DoctorStatus::Pass);
    assert!(
        check.message.contains("time zone Australia/Darwin (system, +09:30)"),
        "{}",
        check.message
    );
}

#[test]
fn each_tool_backed_remediation_advertises_the_mcp_tool_it_calls_and_uses_it_as_its_code() {
    let remediations = [
        Remediation::RebuildIndexes,
        Remediation::InitialiseGit,
        Remediation::CommitMcpChanges,
    ];

    let tools: Vec<Option<&'static str>> = remediations.into_iter().map(Remediation::tool).collect();
    let codes: Vec<&'static str> = remediations.into_iter().map(Remediation::code).collect();

    assert_eq!(
        tools,
        [Some("vault_index_rebuild"), Some("git_init"), Some("git_commit")]
    );
    assert_eq!(codes, ["vault_index_rebuild", "git_init", "git_commit"]);
}

#[test]
fn a_remediation_the_server_performs_has_a_code_and_no_tool() {
    for (remediation, code) in [
        (Remediation::Reproject, "reproject"),
        (Remediation::WithdrawProjection, "withdraw_projection"),
        (Remediation::RestoreOrientation, "restore_orientation"),
    ] {
        assert_eq!(remediation.code(), code);
        assert_eq!(
            remediation.tool(),
            None,
            "a client must never be handed something that is not a tool"
        );
        assert!(remediation.performed_by_server());
    }
    assert!(!Remediation::InitialiseGit.performed_by_server());
}

#[cfg(unix)]
#[test]
fn a_projection_configuration_error_is_a_failed_configuration_check_and_every_vault_check_still_runs()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let base = dunce::canonicalize(fixture.path())?;
    let home = base.join("home");
    let vault = base.join("vault");
    let elsewhere = base.join("elsewhere");
    for folder in [home.join(".claude"), vault.clone(), elsewhere.clone()] {
        std::fs::create_dir_all(folder)?;
    }
    let link = home.join(".claude/agents");
    std::os::unix::fs::symlink(&elsewhere, &link)?;
    let mut config = scoped_config(vec![vault])?;
    config.provenance = crate::ConfigProvenance {
        location: crate::ConfigLocation::PlatformDefault,
        path: Some(home.join(".config/contextos/config.toml")),
        home: Some(home),
    };

    let report = DoctorReport::try_from(&config)?;

    let configuration = report
        .checks
        .iter()
        .find(|check| check.kind == DoctorKind::Configuration)
        .ok_or("no configuration check")?;
    assert_eq!(configuration.status, DoctorStatus::Fail, "{}", configuration.message);
    assert!(
        configuration.message.contains(&link.display().to_string()),
        "{}",
        configuration.message
    );
    assert!(
        configuration.message.contains(&elsewhere.display().to_string()),
        "{}",
        configuration.message
    );
    assert!(
        configuration.message.contains("enabled = false"),
        "{}",
        configuration.message
    );
    assert!(
        report.checks.iter().any(|check| check.kind == DoctorKind::VaultAccess),
        "the vault checks did not run"
    );
    Ok(())
}

#[test]
fn the_configuration_check_names_the_projection_identity_only_when_projection_is_enabled()
-> Result<(), Box<dyn std::error::Error>> {
    use sha2::{Digest, Sha256};
    let fixture = tempdir()?;
    let base = dunce::canonicalize(fixture.path())?;
    let home = base.join("home");
    let vault = base.join("vault");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&vault)?;
    let file = home.join(".config").join("contextos").join("config.toml");
    let mut config = scoped_config(vec![vault])?;
    config.provenance = crate::ConfigProvenance {
        location: crate::ConfigLocation::PlatformDefault,
        path: Some(file.clone()),
        home: Some(home),
    };
    let identity: String =
        Sha256::digest(file.as_os_str().as_encoded_bytes())
            .iter()
            .take(6)
            .fold(String::new(), |mut text, byte| {
                use std::fmt::Write as _;
                let _ = write!(text, "{byte:02x}");
                text
            });

    let enabled = DoctorReport::try_from(&config)?;
    config.provenance.location = crate::ConfigLocation::Named;
    let disabled = DoctorReport::try_from(&config)?;

    let message = |report: &DoctorReport| {
        report
            .checks
            .iter()
            .find(|check| check.kind == DoctorKind::Configuration)
            .map(|check| check.message.clone())
            .unwrap_or_default()
    };
    assert!(message(&enabled).contains(&identity), "{}", message(&enabled));
    assert!(!message(&disabled).contains(&identity), "{}", message(&disabled));
    Ok(())
}
