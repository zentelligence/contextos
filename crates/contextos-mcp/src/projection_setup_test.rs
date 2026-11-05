use std::path::Path;

use contextos_projection::{HostKind, HostRender, Tier};

use super::*;
use crate::{ConfigLocation, ConfigProvenance};

struct Fixture {
    _directory: tempfile::TempDir,
    home: PathBuf,
    vault: PathBuf,
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let home = dunce::canonicalize(directory.path())?.join("home");
    let vault = dunce::canonicalize(directory.path())?.join("vault");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&vault)?;
    Ok(Fixture {
        _directory: directory,
        home,
        vault,
    })
}

fn config(
    fixture: &Fixture,
    location: ConfigLocation,
    projection_toml: &str,
) -> Result<Config, Box<dyn std::error::Error>> {
    let source = format!(
        "[[vault]]\npath = {:?}\n\n{projection_toml}",
        fixture.vault.display().to_string()
    );
    let mut config = Config::try_from(source.as_str())?;
    config.provenance = ConfigProvenance {
        location,
        path: Some(fixture.home.join(".config/contextos/config.toml")),
        home: Some(fixture.home.clone()),
    };
    Ok(config)
}

fn settings(setup: ProjectionSetup) -> Result<ProjectionSettings, Box<dyn std::error::Error>> {
    match setup {
        ProjectionSetup::Enabled(settings) => Ok(settings),
        ProjectionSetup::Disabled => Err("projection was disabled".into()),
    }
}

fn host(settings: &ProjectionSettings, kind: HostKind) -> Result<&HostSettings, Box<dyn std::error::Error>> {
    settings
        .hosts
        .iter()
        .find(|host| HostKind::from(&host.render) == kind)
        .ok_or_else(|| format!("no {kind} host").into())
}

#[test]
fn a_default_location_configuration_without_a_section_projects_into_both_hosts_with_the_documented_defaults()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let config = config(&fixture, ConfigLocation::PlatformDefault, "")?;

    let settings = settings(ProjectionSetup::try_from(&config)?)?;

    let claude = host(&settings, HostKind::ClaudeCode)?;
    assert_eq!(claude.agents, fixture.home.join(".claude/agents"));
    assert_eq!(claude.skills, fixture.home.join(".claude/skills"));
    assert_eq!(claude.instructions, fixture.home.join(".claude/CLAUDE.md"));
    let codex = host(&settings, HostKind::Codex)?;
    assert_eq!(codex.agents, fixture.home.join(".codex/agents"));
    assert_eq!(codex.skills, fixture.home.join(".agents/skills"));
    assert_eq!(codex.instructions, fixture.home.join(".codex/AGENTS.md"));
    assert_eq!(settings.state_root, fixture.home.join(".contextos"));
    Ok(())
}

#[test]
fn default_tiers_map_to_claude_models_and_codex_reasoning_efforts() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let config = config(&fixture, ConfigLocation::PlatformDefault, "")?;

    let settings = settings(ProjectionSetup::try_from(&config)?)?;

    let HostRender::Claude { tiers, read_only_tools } = &host(&settings, HostKind::ClaudeCode)?.render else {
        return Err("not a Claude host".into());
    };
    assert_eq!(<&str>::from(tiers.model(Tier::Fast)), "haiku");
    assert_eq!(<&str>::from(tiers.model(Tier::Balanced)), "sonnet");
    assert_eq!(<&str>::from(tiers.model(Tier::Deep)), "opus");
    assert_eq!(read_only_tools.server(), "contextos");
    let HostRender::Codex { tiers } = &host(&settings, HostKind::Codex)?.render else {
        return Err("not a Codex host".into());
    };
    let efforts = [Tier::Fast, Tier::Balanced, Tier::Deep].map(|tier| {
        let codex = tiers.tier(tier);
        (codex.model().is_none(), <&str>::from(codex.effort()))
    });
    assert_eq!(efforts, [(true, "low"), (true, "medium"), (true, "high")]);
    Ok(())
}

#[test]
fn the_read_only_allowlist_holds_every_non_mutating_tool_under_the_configured_server_name()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let config = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        "[projection.claude]\nmcp_server = \"vault-os\"\n",
    )?;

    let settings = settings(ProjectionSetup::try_from(&config)?)?;

    let HostRender::Claude { read_only_tools, .. } = &host(&settings, HostKind::ClaudeCode)?.render else {
        return Err("not a Claude host".into());
    };
    let allowlist = read_only_tools.allowlist();
    assert!(allowlist.contains(&"mcp__vault-os__fs_read_text_file".to_owned()));
    assert!(allowlist.contains(&"mcp__vault-os__vault_info".to_owned()));
    assert!(!allowlist.contains(&"mcp__vault-os__fs_write_file".to_owned()));
    assert_eq!(
        allowlist.len(),
        crate::tool_names(crate::ToolEffect::ReadOnly).len(),
        "the allowlist is built from the single production classification"
    );
    Ok(())
}

#[test]
fn a_named_configuration_without_a_section_projects_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let config = config(&fixture, ConfigLocation::Named, "")?;

    assert!(matches!(ProjectionSetup::try_from(&config)?, ProjectionSetup::Disabled));
    Ok(())
}

#[test]
fn a_configuration_loaded_from_no_file_projects_nothing_unless_it_says_so() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let config = config(&fixture, ConfigLocation::Unspecified, "")?;

    assert!(matches!(ProjectionSetup::try_from(&config)?, ProjectionSetup::Disabled));
    Ok(())
}

#[test]
fn a_named_configuration_projects_when_it_sets_enabled() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let config = config(&fixture, ConfigLocation::Named, "[projection]\nenabled = true\n")?;

    let settings = settings(ProjectionSetup::try_from(&config)?)?;

    assert_eq!(settings.hosts.len(), 2);
    Ok(())
}

#[test]
fn a_default_location_configuration_can_turn_projection_off() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let config = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        "[projection]\nenabled = false\n",
    )?;

    assert!(matches!(ProjectionSetup::try_from(&config)?, ProjectionSetup::Disabled));
    Ok(())
}

#[test]
fn a_section_without_enabled_follows_the_source_rule() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let body = "[projection.claude]\nenabled = true\n";

    let named = config(&fixture, ConfigLocation::Named, body)?;
    let default = config(&fixture, ConfigLocation::PlatformDefault, body)?;

    assert!(matches!(ProjectionSetup::try_from(&named)?, ProjectionSetup::Disabled));
    assert!(matches!(
        ProjectionSetup::try_from(&default)?,
        ProjectionSetup::Enabled(_)
    ));
    Ok(())
}

#[test]
fn a_disabled_host_is_left_out_and_with_both_off_nothing_is_projected() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let one_off = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        "[projection.codex]\nenabled = false\n",
    )?;
    let both_off = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        "[projection.codex]\nenabled = false\n[projection.claude]\nenabled = false\n",
    )?;

    let settings = settings(ProjectionSetup::try_from(&one_off)?)?;

    assert_eq!(settings.hosts.len(), 1);
    assert!(matches!(settings.hosts[0].render, HostRender::Claude { .. }));
    assert!(matches!(
        ProjectionSetup::try_from(&both_off)?,
        ProjectionSetup::Disabled
    ));
    Ok(())
}

#[test]
fn a_tilde_expands_from_the_injected_home_not_the_environment() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let config = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        "[projection.claude]\nagents = \"~/agents-here\"\nskills = \"~/skills-here\"\n",
    )?;

    let settings = settings(ProjectionSetup::try_from(&config)?)?;

    let claude = host(&settings, HostKind::ClaudeCode)?;
    assert_eq!(claude.agents, fixture.home.join("agents-here"));
    assert_eq!(claude.skills, fixture.home.join("skills-here"));
    Ok(())
}

#[test]
fn an_absolute_target_is_used_as_written() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let elsewhere = fixture.home.join("elsewhere");
    let config = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        &format!("[projection.codex]\nagents = {:?}\n", elsewhere.display().to_string()),
    )?;

    let settings = settings(ProjectionSetup::try_from(&config)?)?;

    assert_eq!(host(&settings, HostKind::Codex)?.agents, elsewhere);
    Ok(())
}

#[test]
fn a_relative_target_is_refused_with_its_key_and_a_stable_code() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let config = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        "[projection.claude]\nagents = \"agents\"\n",
    )?;

    let error = ProjectionSetup::try_from(&config)
        .err()
        .ok_or("accepted a relative target")?;

    assert_eq!(error.code(), "projection/target-not-absolute");
    assert!(error.to_string().contains("projection.claude.agents"), "{error}");
    Ok(())
}

#[test]
fn a_user_qualified_tilde_is_refused_as_not_absolute() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let config = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        "[projection.codex]\nskills = \"~other/skills\"\n",
    )?;

    let error = ProjectionSetup::try_from(&config).err().ok_or("accepted ~other")?;

    assert_eq!(error.code(), "projection/target-not-absolute");
    Ok(())
}

#[test]
fn a_target_inside_a_vault_root_is_refused_even_when_it_does_not_exist_yet() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let inside = fixture.vault.join("registry/agents-out");
    let config = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        &format!("[projection.claude]\nagents = {:?}\n", inside.display().to_string()),
    )?;

    let error = ProjectionSetup::try_from(&config)
        .err()
        .ok_or("accepted a target in a vault")?;

    assert_eq!(error.code(), "projection/target-in-vault");
    assert!(error.to_string().contains("projection.claude.agents"), "{error}");
    Ok(())
}

#[test]
fn an_instructions_file_inside_a_vault_root_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let inside = fixture.vault.join("CLAUDE.md");
    let config = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        &format!(
            "[projection.codex]\ninstructions = {:?}\n",
            inside.display().to_string()
        ),
    )?;

    let error = ProjectionSetup::try_from(&config)
        .err()
        .ok_or("accepted instructions in a vault")?;

    assert_eq!(error.code(), "projection/target-in-vault");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_target_that_reaches_a_vault_through_a_link_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let link = fixture.home.join("shortcut");
    std::os::unix::fs::symlink(&fixture.vault, &link)?;
    let config = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        &format!(
            "[projection.claude]\nagents = {:?}\n",
            link.join("agents").display().to_string()
        ),
    )?;

    let error = ProjectionSetup::try_from(&config)
        .err()
        .ok_or("followed a link into a vault")?;

    assert_eq!(error.code(), "projection/target-in-vault");
    Ok(())
}

#[test]
fn a_target_folder_that_contains_a_vault_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let parent = fixture.vault.parent().ok_or("vault has no parent")?;
    let config = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        &format!("[projection.claude]\nskills = {:?}\n", parent.display().to_string()),
    )?;

    let error = ProjectionSetup::try_from(&config)
        .err()
        .ok_or("accepted a skills folder that holds a vault")?;

    assert_eq!(error.code(), "projection/vault-in-target");
    Ok(())
}

#[test]
fn an_instructions_file_whose_folder_contains_a_vault_is_accepted() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let shared = fixture.home.join("shared");
    let vault = shared.join("vault");
    std::fs::create_dir_all(&vault)?;
    let mut config = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        &format!(
            "[projection.codex]\ninstructions = {:?}\n",
            shared.join("AGENTS.md").display().to_string()
        ),
    )?;
    config.vaults[0].path = vault;

    assert!(matches!(
        ProjectionSetup::try_from(&config)?,
        ProjectionSetup::Enabled(_)
    ));
    Ok(())
}

#[test]
fn a_target_folder_several_levels_above_a_vault_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let nested = fixture.home.join("projects");
    std::fs::create_dir_all(&nested)?;
    let vault = nested.join("agents/vault");
    std::fs::create_dir_all(&vault)?;
    let mut config = config(&fixture, ConfigLocation::PlatformDefault, "")?;
    config.vaults[0].path = vault;
    config.projection = Some(toml::from_str(&format!(
        "[claude]\nagents = {:?}\n",
        nested.join("agents").display().to_string()
    ))?);

    let error = ProjectionSetup::try_from(&config)
        .err()
        .ok_or("accepted an agents folder that holds a vault")?;

    assert_eq!(error.code(), "projection/vault-in-target");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_target_that_reaches_a_folder_holding_a_vault_through_a_link_is_refused() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = fixture()?;
    let parent = fixture.vault.parent().ok_or("vault has no parent")?;
    let link = fixture.home.join("shortcut");
    std::os::unix::fs::symlink(parent, &link)?;
    let config = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        &format!("[projection.codex]\nagents = {:?}\n", link.display().to_string()),
    )?;

    let error = ProjectionSetup::try_from(&config)
        .err()
        .ok_or("followed a link to a folder holding a vault")?;

    assert_eq!(error.code(), "projection/vault-in-target");
    Ok(())
}

#[test]
fn a_disabled_host_with_a_bad_target_is_not_validated() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let config = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        "[projection.codex]\nenabled = false\nagents = \"relative\"\n",
    )?;

    assert!(matches!(
        ProjectionSetup::try_from(&config)?,
        ProjectionSetup::Enabled(_)
    ));
    Ok(())
}

#[test]
fn an_unknown_projection_key_is_a_configuration_error() {
    for body in [
        "[projection]\nenable = true\n",
        "[projection.claude]\nagent = \"x\"\n",
        "[projection.claude.tiers]\nmedium = \"x\"\n",
        "[projection.codex.tiers.fast]\nmodell = \"x\"\n",
    ] {
        let error = Config::try_from(body);
        assert!(error.is_err(), "accepted {body:?}");
        assert_eq!(error.err().map(|error| error.code()), Some("config/invalid-toml"));
    }
}

#[test]
fn invalid_tier_values_are_refused_with_the_key_that_holds_them() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    for (body, key) in [
        (
            "[projection.claude.tiers]\nfast = \"two words\"\n",
            "projection.claude.tiers.fast",
        ),
        (
            "[projection.claude.tiers]\ndeep = \"\"\n",
            "projection.claude.tiers.deep",
        ),
        (
            "[projection.claude]\nmcp_server = \"Bad Name\"\n",
            "projection.claude.mcp_server",
        ),
        (
            "[projection.codex.tiers.balanced]\neffort = \"extreme\"\n",
            "projection.codex.tiers.balanced.effort",
        ),
        (
            "[projection.codex.tiers.deep]\nmodel = \"has space\"\n",
            "projection.codex.tiers.deep.model",
        ),
    ] {
        let config = config(&fixture, ConfigLocation::PlatformDefault, body)?;

        let error = ProjectionSetup::try_from(&config).err().ok_or("accepted a bad value")?;

        assert_eq!(error.code(), "projection/invalid-configuration", "{body}");
        assert!(error.to_string().contains(key), "{error}");
    }
    Ok(())
}

#[test]
fn a_codex_tier_may_name_a_model_and_any_documented_effort() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let config = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        "[projection.codex.tiers.deep]\nmodel = \"gpt-x\"\neffort = \"xhigh\"\n[projection.codex.tiers.fast]\neffort = \"minimal\"\n",
    )?;

    let settings = settings(ProjectionSetup::try_from(&config)?)?;

    let HostRender::Codex { tiers } = &host(&settings, HostKind::Codex)?.render else {
        return Err("not a Codex host".into());
    };
    assert_eq!(tiers.tier(Tier::Deep).model().map(<&str>::from), Some("gpt-x"));
    assert_eq!(<&str>::from(tiers.tier(Tier::Deep).effort()), "xhigh");
    assert_eq!(<&str>::from(tiers.tier(Tier::Fast).effort()), "minimal");
    assert_eq!(<&str>::from(tiers.tier(Tier::Balanced).effort()), "medium");
    Ok(())
}

#[test]
fn enabled_projection_needs_a_home_and_a_configuration_file() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let mut homeless = config(&fixture, ConfigLocation::PlatformDefault, "")?;
    homeless.provenance.home = None;
    let mut unnamed = config(&fixture, ConfigLocation::PlatformDefault, "")?;
    unnamed.provenance.path = None;

    let homeless = ProjectionSetup::try_from(&homeless)
        .err()
        .ok_or("projected without a home")?;
    let unnamed = ProjectionSetup::try_from(&unnamed)
        .err()
        .ok_or("projected without a file")?;

    assert_eq!(homeless.code(), "projection/home-unavailable");
    assert_eq!(unnamed.code(), "projection/configuration-unnamed");
    Ok(())
}

#[test]
fn the_configuration_identity_is_stable_for_a_path_and_differs_between_paths() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = fixture()?;
    let first = config(&fixture, ConfigLocation::PlatformDefault, "")?;
    let mut other = config(&fixture, ConfigLocation::PlatformDefault, "")?;
    other.provenance.path = Some(Path::new("/elsewhere/config.toml").to_path_buf());

    let one = settings(ProjectionSetup::try_from(&first)?)?.configuration;
    let again = settings(ProjectionSetup::try_from(&first)?)?.configuration;
    let two = settings(ProjectionSetup::try_from(&other)?)?.configuration;

    assert_eq!(one, again);
    assert_ne!(one, two);
    Ok(())
}

fn refusal(fixture: &Fixture, projection_toml: &str) -> Result<ConfigError, Box<dyn std::error::Error>> {
    let config = config(fixture, ConfigLocation::PlatformDefault, projection_toml)?;
    ProjectionSetup::try_from(&config)
        .err()
        .ok_or_else(|| format!("accepted: {projection_toml}").into())
}

#[test]
fn a_target_that_is_or_lies_inside_the_state_root_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    for configured in ["~/.contextos", "~/.contextos/agents", "~/.contextos/deeper/still"] {
        let error = refusal(&fixture, &format!("[projection.claude]\nagents = {configured:?}\n"))?;
        assert_eq!(error.code(), "projection/target-in-state-root", "{configured}: {error}");
        assert!(error.to_string().contains("projection.claude.agents"), "{error}");
    }
    Ok(())
}

#[test]
fn a_target_that_contains_the_state_root_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;

    let error = refusal(&fixture, "[projection.codex]\nskills = \"~\"\n")?;

    assert_eq!(error.code(), "projection/state-root-in-target", "{error}");
    assert!(error.to_string().contains("projection.codex.skills"), "{error}");
    Ok(())
}

#[test]
fn an_instructions_file_inside_the_state_root_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;

    let error = refusal(
        &fixture,
        "[projection.claude]\ninstructions = \"~/.contextos/CLAUDE.md\"\n",
    )?;

    assert_eq!(error.code(), "projection/target-in-state-root", "{error}");
    assert!(error.to_string().contains("projection.claude.instructions"), "{error}");
    Ok(())
}

#[test]
fn two_hosts_may_not_share_a_folder() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;

    let same_agents = refusal(&fixture, "[projection.codex]\nagents = \"~/.claude/agents\"\n")?;
    let same_skills = refusal(&fixture, "[projection.codex]\nskills = \"~/.claude/skills\"\n")?;

    assert_eq!(same_agents.code(), "projection/targets-overlap", "{same_agents}");
    assert_eq!(same_skills.code(), "projection/targets-overlap", "{same_skills}");
    Ok(())
}

#[test]
fn a_target_nested_inside_or_around_another_targets_folder_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    for projection in [
        "[projection.codex]\nagents = \"~/.claude/agents/codex\"\n",
        "[projection.codex]\nskills = \"~/.claude\"\n",
        "[projection.claude]\nskills = \"~/.claude/agents\"\n",
    ] {
        let error = refusal(&fixture, projection)?;
        assert_eq!(error.code(), "projection/targets-overlap", "{projection}: {error}");
    }
    Ok(())
}

#[test]
fn targets_beside_each_other_with_a_shared_name_prefix_are_accepted() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let config = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        "[projection.claude]\nagents = \"~/agents\"\nskills = \"~/agents-extra\"\n",
    )?;

    assert!(ProjectionSetup::try_from(&config).is_ok());
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_target_reaching_the_state_root_through_a_link_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    std::fs::create_dir_all(fixture.home.join(".contextos"))?;
    std::os::unix::fs::symlink(fixture.home.join(".contextos"), fixture.home.join("alias"))?;

    let error = refusal(&fixture, "[projection.claude]\nagents = \"~/alias/agents\"\n")?;

    assert_eq!(error.code(), "projection/target-in-state-root", "{error}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_target_reaching_another_targets_folder_through_a_link_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    std::fs::create_dir_all(fixture.home.join(".claude/agents"))?;
    std::os::unix::fs::symlink(fixture.home.join(".claude/agents"), fixture.home.join("alias"))?;

    let error = refusal(&fixture, "[projection.codex]\nagents = \"~/alias\"\n")?;

    assert_eq!(error.code(), "projection/targets-overlap", "{error}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_instructions_file_reaching_a_vault_through_a_link_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    std::os::unix::fs::symlink(&fixture.vault, fixture.home.join("alias"))?;

    let error = refusal(&fixture, "[projection.claude]\ninstructions = \"~/alias/CLAUDE.md\"\n")?;

    assert_eq!(error.code(), "projection/target-in-vault", "{error}");
    assert!(error.to_string().contains("projection.claude.instructions"), "{error}");
    Ok(())
}

#[test]
fn the_one_resolution_policy_gives_a_target_and_a_vault_root_naming_the_same_folder_equal_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    std::fs::create_dir(fixture.vault.join("inner"))?;
    let roots = contextos_core::VaultSet::try_from(&config(&fixture, ConfigLocation::PlatformDefault, "")?)?;
    let root = roots.iter().next().ok_or("no vault root")?;
    let spelt_differently = fixture.vault.join("inner").join("..");

    assert_eq!(
        contextos_core::ResolvedPath::try_from(root.path())?,
        contextos_core::ResolvedPath::try_from(spelt_differently.as_path())?
    );
    Ok(())
}

#[cfg(windows)]
#[test]
fn a_verbatim_spelling_of_a_target_inside_a_vault_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let verbatim = format!(r"\\?\{}", fixture.vault.join("agents").display());

    let error = refusal(&fixture, &format!("[projection.claude]\nagents = {verbatim:?}\n"))?;

    assert_eq!(error.code(), "projection/target-in-vault", "{error}");
    Ok(())
}

#[cfg(windows)]
#[test]
fn a_vault_root_reached_through_a_verbatim_target_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let parent = fixture.vault.parent().ok_or("no parent")?.display().to_string();
    let verbatim = format!(r"\\?\{parent}");

    let error = refusal(&fixture, &format!("[projection.claude]\nagents = {verbatim:?}\n"))?;

    assert_eq!(error.code(), "projection/vault-in-target", "{error}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn the_configuration_identity_is_the_same_before_and_after_the_file_is_created()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let real = fixture.home.join("real-config");
    std::fs::create_dir_all(&real)?;
    std::os::unix::fs::symlink(&real, fixture.home.join("linked-config"))?;
    let through_the_link = fixture.home.join("linked-config/config.toml");

    let before = ConfigurationId::try_from(ConfigFilePath(&through_the_link))?;
    std::fs::write(&through_the_link, "")?;
    let after = ConfigurationId::try_from(ConfigFilePath(&through_the_link))?;

    assert_eq!(before, after);
    Ok(())
}

#[cfg(unix)]
#[test]
fn the_configuration_identity_is_the_same_however_the_path_is_linked() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let real = fixture.home.join("real-config");
    std::fs::create_dir_all(&real)?;
    std::os::unix::fs::symlink(&real, fixture.home.join("linked-config"))?;
    let direct = real.join("config.toml");
    let through_the_link = fixture.home.join("linked-config/config.toml");

    // Neither file exists yet.
    assert_eq!(
        ConfigurationId::try_from(ConfigFilePath(&direct))?,
        ConfigurationId::try_from(ConfigFilePath(&through_the_link))?
    );
    std::fs::write(&direct, "")?;
    // And once it does.
    assert_eq!(
        ConfigurationId::try_from(ConfigFilePath(&direct))?,
        ConfigurationId::try_from(ConfigFilePath(&through_the_link))?
    );
    Ok(())
}

/// Makes `home/<host_home>` and a symbolic link at `home/<host_home>/<name>` pointing at `destination`.
#[cfg(unix)]
fn link_in_host(
    fixture: &Fixture,
    host_home: &str,
    name: &str,
    destination: &Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let folder = fixture.home.join(host_home);
    std::fs::create_dir_all(&folder)?;
    let link = folder.join(name);
    std::os::unix::fs::symlink(destination, &link)?;
    Ok(link)
}

#[cfg(unix)]
#[test]
fn a_default_agents_folder_that_is_a_link_stops_the_configuration_loading_and_names_the_link_and_its_target()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let elsewhere = fixture.home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere)?;
    let link = link_in_host(&fixture, ".claude", "agents", &elsewhere)?;

    let error = refusal(&fixture, "")?;

    let message = error.to_string();
    assert!(error.code().starts_with("projection/"), "{}", error.code());
    assert!(message.contains(&link.display().to_string()), "{message}");
    assert!(message.contains(&elsewhere.display().to_string()), "{message}");
    assert!(message.contains("enabled = false"), "{message}");
    assert!(message.contains("[projection]"), "{message}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_dangling_link_in_place_of_a_skills_folder_stops_the_configuration_loading()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let missing = fixture.home.join("not-there");
    let link = link_in_host(&fixture, ".claude", "skills", &missing)?;

    let error = refusal(&fixture, "")?;

    let message = error.to_string();
    assert!(message.contains(&link.display().to_string()), "{message}");
    assert!(message.contains(&missing.display().to_string()), "{message}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn every_linked_folder_is_named_in_one_error_across_both_hosts() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let one = fixture.home.join("one");
    let two = fixture.home.join("two");
    let three = fixture.home.join("three");
    for folder in [&one, &two, &three] {
        std::fs::create_dir_all(folder)?;
    }
    let claude_agents = link_in_host(&fixture, ".claude", "agents", &one)?;
    let codex_agents = link_in_host(&fixture, ".codex", "agents", &two)?;
    let codex_skills = link_in_host(&fixture, ".agents", "skills", &three)?;

    let message = refusal(&fixture, "")?.to_string();

    for (link, destination) in [(claude_agents, one), (codex_agents, two), (codex_skills, three)] {
        assert!(message.contains(&link.display().to_string()), "{message}");
        assert!(message.contains(&destination.display().to_string()), "{message}");
    }
    assert!(message.contains("enabled = false"), "{message}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_configured_target_that_is_a_link_is_refused_the_same_way_as_a_default() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let elsewhere = fixture.home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere)?;
    let link = link_in_host(&fixture, "custom", "my-agents", &elsewhere)?;

    let message = refusal(
        &fixture,
        &format!("[projection.claude]\nagents = {:?}\n", link.display().to_string()),
    )?
    .to_string();

    assert!(message.contains(&link.display().to_string()), "{message}");
    assert!(message.contains(&elsewhere.display().to_string()), "{message}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_link_in_an_ancestor_of_a_target_is_still_followed_and_accepted() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let real = fixture.home.join("real-claude");
    std::fs::create_dir_all(real.join("agents"))?;
    std::os::unix::fs::symlink(&real, fixture.home.join(".claude"))?;
    let config = config(&fixture, ConfigLocation::PlatformDefault, "")?;

    assert!(matches!(
        ProjectionSetup::try_from(&config)?,
        ProjectionSetup::Enabled(_)
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_instructions_file_that_is_a_link_is_still_accepted() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let shared = fixture.home.join("shared");
    std::fs::create_dir_all(&shared)?;
    let real = shared.join("claude.md");
    std::fs::write(&real, "kept elsewhere\n")?;
    link_in_host(&fixture, ".claude", "CLAUDE.md", &real)?;
    let config = config(&fixture, ConfigLocation::PlatformDefault, "")?;

    assert!(matches!(
        ProjectionSetup::try_from(&config)?,
        ProjectionSetup::Enabled(_)
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_linked_folder_of_a_host_that_is_switched_off_does_not_stop_the_configuration_loading()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let elsewhere = fixture.home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere)?;
    link_in_host(&fixture, ".claude", "agents", &elsewhere)?;
    let off = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        "[projection.claude]\nenabled = false\n",
    )?;
    let all_off = config(
        &fixture,
        ConfigLocation::PlatformDefault,
        "[projection]\nenabled = false\n",
    )?;

    assert!(matches!(ProjectionSetup::try_from(&off)?, ProjectionSetup::Enabled(_)));
    assert!(matches!(
        ProjectionSetup::try_from(&all_off)?,
        ProjectionSetup::Disabled
    ));
    Ok(())
}

#[test]
fn a_vault_in_the_folder_that_holds_an_instructions_file_but_in_no_target_is_accepted()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let notes = fixture.home.join(".codex/notes");
    std::fs::create_dir_all(&notes)?;
    let mut config = config(&fixture, ConfigLocation::PlatformDefault, "")?;
    config.vaults[0].path = notes;

    assert!(matches!(
        ProjectionSetup::try_from(&config)?,
        ProjectionSetup::Enabled(_)
    ));
    Ok(())
}

#[test]
fn a_vault_inside_an_agents_target_is_still_refused_beside_an_instructions_file()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let inside = fixture.home.join(".codex/agents/notes");
    std::fs::create_dir_all(&inside)?;
    let mut config = config(&fixture, ConfigLocation::PlatformDefault, "")?;
    config.vaults[0].path = inside;

    let error = ProjectionSetup::try_from(&config)
        .err()
        .ok_or("accepted a vault inside a target")?;

    assert_eq!(error.code(), "projection/vault-in-target");
    Ok(())
}
