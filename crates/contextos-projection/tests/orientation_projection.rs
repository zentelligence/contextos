//! The orientation block in each host's global instructions file: real vaults and real instruction files in
//! temporary folders, driven through write events, start-up refresh, and assessment.

mod common;

use std::fs;
use std::path::PathBuf;

use common::{TestResult, World, codes, world};
use contextos_core::OpKind;
use contextos_projection::{FindingKind, HostKind, ProjectionService};
use tempfile::{TempDir, tempdir};

const GLOBAL: &str = "registry/instructions/global.md";
const BEGIN: &str = "<!-- contextos:orientation:begin -->";
const END: &str = "<!-- contextos:orientation:end -->";

struct Homes {
    _dir: TempDir,
    claude: PathBuf,
    codex: PathBuf,
}

fn homes() -> Result<Homes, Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    fs::create_dir_all(dir.path().join(".claude"))?;
    fs::create_dir_all(dir.path().join(".codex"))?;
    Ok(Homes {
        claude: dir.path().join(".claude/CLAUDE.md"),
        codex: dir.path().join(".codex/AGENTS.md"),
        _dir: dir,
    })
}

fn service(world: &World, homes: &Homes) -> Result<ProjectionService, Box<dyn std::error::Error>> {
    world.service_with_instructions("config-one", &homes.claude, &homes.codex)
}

fn block(content: &str) -> String {
    format!("{BEGIN}\n{content}{END}\n")
}

fn read(path: &PathBuf) -> Result<String, String> {
    fs::read_to_string(path)
        .map_err(|error| format!("could not read the instructions file {}: {error}", path.display()))
}

#[test]
fn a_write_event_for_the_instructions_file_puts_the_block_in_each_hosts_file() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    let service = service(&world, &homes)?;
    world.write_vault(0, GLOBAL, "Use vault_info first.\n")?;

    let warnings = world.project(&service, OpKind::Create, &[(0, GLOBAL)])?;

    assert_eq!(codes(&warnings), Vec::<&str>::new());
    assert_eq!(read(&homes.claude)?, block("Use vault_info first.\n"));
    assert_eq!(read(&homes.codex)?, block("Use vault_info first.\n"));
    Ok(())
}

#[test]
fn the_block_is_appended_to_an_operators_existing_instructions_without_touching_them() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    fs::write(&homes.claude, "# My rules\nBe brief.\n")?;
    fs::write(&homes.codex, "Codex rules, no final newline")?;
    let service = service(&world, &homes)?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;

    world.project(&service, OpKind::Create, &[(0, GLOBAL)])?;

    assert_eq!(
        read(&homes.claude)?,
        format!("# My rules\nBe brief.\n\n{}", block("Orient.\n"))
    );
    assert_eq!(
        read(&homes.codex)?,
        format!("Codex rules, no final newline\n\n{}", block("Orient.\n"))
    );
    Ok(())
}

#[test]
fn the_source_is_the_first_vault_in_configuration_order_that_has_the_file() -> TestResult {
    let world = world(3)?;
    let homes = homes()?;
    let service = service(&world, &homes)?;
    world.write_vault(1, GLOBAL, "From beta.\n")?;
    world.write_vault(2, GLOBAL, "From gamma.\n")?;

    world.project(&service, OpKind::Create, &[(2, GLOBAL)])?;

    assert_eq!(read(&homes.claude)?, block("From beta.\n"));
    assert_eq!(read(&homes.codex)?, block("From beta.\n"));
    Ok(())
}

#[test]
fn deleting_the_file_from_the_only_vault_that_has_it_removes_the_block() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    fs::write(&homes.claude, "# Mine\n")?;
    let service = service(&world, &homes)?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;
    world.project(&service, OpKind::Create, &[(0, GLOBAL)])?;
    assert!(
        read(&homes.claude)?.contains(BEGIN),
        "the block must have been written first"
    );
    world.remove_vault(0, GLOBAL)?;

    let warnings = world.project(&service, OpKind::Delete, &[(0, GLOBAL)])?;

    assert_eq!(codes(&warnings), Vec::<&str>::new());
    assert_eq!(read(&homes.claude)?, "# Mine\n\n");
    assert_eq!(read(&homes.codex)?, "");
    Ok(())
}

#[test]
fn deleting_the_file_from_the_first_vault_switches_the_block_to_the_next_vaults_content() -> TestResult {
    let world = world(2)?;
    let homes = homes()?;
    let service = service(&world, &homes)?;
    world.write_vault(0, GLOBAL, "From alpha.\n")?;
    world.write_vault(1, GLOBAL, "From beta.\n")?;
    world.project(&service, OpKind::Create, &[(0, GLOBAL)])?;
    assert_eq!(read(&homes.claude)?, block("From alpha.\n"));
    world.remove_vault(0, GLOBAL)?;

    world.project(&service, OpKind::Delete, &[(0, GLOBAL)])?;

    assert_eq!(read(&homes.claude)?, block("From beta.\n"));
    assert_eq!(read(&homes.codex)?, block("From beta.\n"));
    Ok(())
}

#[test]
fn refresh_restores_a_block_that_differs_from_its_source() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    fs::write(&homes.claude, format!("top\n{}bottom\n", block("hand edited\n")))?;
    fs::write(&homes.codex, block("also edited\n"))?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;

    let warnings = service(&world, &homes)?.refresh();

    assert_eq!(codes(&warnings), Vec::<&str>::new());
    assert_eq!(read(&homes.claude)?, format!("top\n{}bottom\n", block("Orient.\n")));
    assert_eq!(read(&homes.codex)?, block("Orient.\n"));
    Ok(())
}

#[test]
fn refresh_inserts_a_missing_block() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    fs::write(&homes.claude, "# Mine\n")?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;

    let warnings = service(&world, &homes)?.refresh();

    assert_eq!(codes(&warnings), Vec::<&str>::new());
    assert_eq!(read(&homes.claude)?, format!("# Mine\n\n{}", block("Orient.\n")));
    assert_eq!(read(&homes.codex)?, block("Orient.\n"));
    Ok(())
}

#[test]
fn refresh_with_no_vault_configured_leaves_an_existing_block_untouched() -> TestResult {
    let world = world(0)?;
    let homes = homes()?;
    let existing = format!("# Mine\n\n{}", block("Orient.\n"));
    fs::write(&homes.claude, &existing)?;
    fs::write(&homes.codex, &existing)?;

    let service =
        world.service_with_instructions_over(&World::no_roots()?, "config-one", &homes.claude, &homes.codex)?;
    let warnings = service.refresh();

    assert_eq!(codes(&warnings), Vec::<&str>::new());
    assert_eq!(read(&homes.claude)?, existing);
    assert_eq!(read(&homes.codex)?, existing);
    Ok(())
}

#[test]
fn a_malformed_instructions_file_is_left_untouched_and_the_other_host_is_still_updated() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    let malformed = format!("top\n{BEGIN}\nnever closed\n");
    fs::write(&homes.claude, &malformed)?;
    let service = service(&world, &homes)?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;

    let warnings = world.project(&service, OpKind::Create, &[(0, GLOBAL)])?;

    assert_eq!(codes(&warnings), vec!["projection/malformed-block"]);
    assert_eq!(read(&homes.claude)?, malformed);
    assert_eq!(read(&homes.codex)?, block("Orient.\n"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_instructions_file_that_is_an_operator_made_link_is_written_through_and_stays_a_link() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    let dotfiles = tempdir()?;
    let real = dotfiles.path().join("claude-instructions.md");
    fs::write(&real, "# Mine, kept in dotfiles\n")?;
    std::os::unix::fs::symlink(&real, &homes.claude)?;
    let service = service(&world, &homes)?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;

    let warnings = world.project(&service, OpKind::Create, &[(0, GLOBAL)])?;

    assert_eq!(codes(&warnings), Vec::<&str>::new());
    assert!(
        fs::symlink_metadata(&homes.claude)?.file_type().is_symlink(),
        "the link must be preserved"
    );
    assert_eq!(fs::read_link(&homes.claude)?, real);
    assert_eq!(
        fs::read_to_string(&real)?,
        format!("# Mine, kept in dotfiles\n\n{}", block("Orient.\n"))
    );
    Ok(())
}

#[test]
fn assess_reports_a_missing_block_as_auto_fixable_and_changes_nothing() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    fs::write(&homes.claude, "# Mine\n")?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;

    let report = service(&world, &homes)?.assess();

    let findings: Vec<_> = report
        .findings
        .iter()
        .filter(|finding| finding.kind == FindingKind::OrientationBlock)
        .collect();
    assert_eq!(findings.len(), 2, "{:?}", report.findings);
    assert!(findings.iter().all(|finding| finding.auto_fixable()));
    assert!(
        findings
            .iter()
            .any(|finding| finding.host == Some(HostKind::ClaudeCode))
    );
    assert!(findings.iter().any(|finding| finding.host == Some(HostKind::Codex)));
    assert_eq!(<&str>::from(FindingKind::OrientationBlock), "orientation_block");
    assert_eq!(read(&homes.claude)?, "# Mine\n");
    assert!(!homes.codex.exists());
    Ok(())
}

#[test]
fn assess_reports_a_differing_block_and_a_current_block_is_clean() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    fs::write(&homes.claude, block("stale\n"))?;
    fs::write(&homes.codex, block("Orient.\n"))?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;

    let report = service(&world, &homes)?.assess();

    let hosts: Vec<_> = report
        .findings
        .iter()
        .filter(|finding| finding.kind == FindingKind::OrientationBlock)
        .map(|finding| finding.host)
        .collect();
    assert_eq!(hosts, vec![Some(HostKind::ClaudeCode)]);
    assert_eq!(read(&homes.claude)?, block("stale\n"));
    Ok(())
}

#[test]
fn assess_reports_a_malformed_block_as_not_auto_fixable() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    let malformed = format!("{END}\ntop\n");
    fs::write(&homes.claude, &malformed)?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;

    let report = service(&world, &homes)?.assess();

    let finding = report
        .findings
        .iter()
        .find(|finding| finding.kind == FindingKind::OrientationBlock && finding.host == Some(HostKind::ClaudeCode))
        .ok_or_else(|| format!("no orientation_block finding for Claude in {:?}", report.findings))?;
    assert!(!finding.auto_fixable());
    assert_eq!(read(&homes.claude)?, malformed);
    Ok(())
}

#[test]
fn assess_reports_a_codex_override_file_and_never_touches_it() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    let override_file = homes.codex.with_file_name("AGENTS.override.md");
    fs::write(&override_file, "Operator override.\n")?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;
    let service = service(&world, &homes)?;

    let report = service.assess();
    assert_eq!(codes(&service.refresh()), Vec::<&str>::new());

    let finding = report
        .findings
        .iter()
        .find(|finding| finding.kind == FindingKind::CodexOverrideHidesAgents)
        .ok_or_else(|| format!("no codex_override_hides_agents finding in {:?}", report.findings))?;
    assert_eq!(finding.host, Some(HostKind::Codex));
    assert_eq!(<&str>::from(finding.kind), "codex_override_hides_agents");
    assert_eq!(fs::read_to_string(&override_file)?, "Operator override.\n");
    Ok(())
}
