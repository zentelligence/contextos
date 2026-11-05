//! How the orientation block is repaired, left alone, or reported when it cannot be: real vaults and real
//! instruction files in temporary folders.

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

#[test]
fn resolving_restores_a_missing_or_differing_block_and_reports_it_as_auto_fixable() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    fs::write(&homes.claude, format!("top\n{}", block("stale\n")))?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;

    let report = service(&world, &homes)?.resolve();

    assert_eq!(codes(&report.warnings), Vec::<&str>::new());
    assert_eq!(
        report
            .findings
            .iter()
            .filter(|finding| finding.kind == FindingKind::OrientationBlock)
            .map(|finding| (finding.host, finding.auto_fixable()))
            .collect::<Vec<_>>(),
        vec![(Some(HostKind::ClaudeCode), true), (Some(HostKind::Codex), true)]
    );
    assert_eq!(
        fs::read_to_string(&homes.claude)?,
        format!("top\n{}", block("Orient.\n"))
    );
    assert_eq!(fs::read_to_string(&homes.codex)?, block("Orient.\n"));
    Ok(())
}

#[test]
fn a_malformed_finding_is_not_auto_fixable_and_resolving_leaves_the_file_alone() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    let malformed = format!("{END}\ntop\n");
    fs::write(&homes.claude, &malformed)?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;
    let service = service(&world, &homes)?;

    let assessed = service.assess();
    let resolved = service.resolve();

    for report in [&assessed, &resolved] {
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.kind == FindingKind::OrientationBlock && finding.host == Some(HostKind::ClaudeCode))
            .ok_or("no malformed finding")?;
        assert!(!finding.auto_fixable());
        assert!(finding.message.contains("malformed"), "{}", finding.message);
        assert!(
            !finding.message.contains(&*homes.claude.to_string_lossy()),
            "{}",
            finding.message
        );
    }
    assert_eq!(fs::read_to_string(&homes.claude)?, malformed);
    assert_eq!(fs::read_to_string(&homes.codex)?, block("Orient.\n"));
    Ok(())
}

#[test]
fn a_source_that_holds_a_marker_line_is_reported_not_fixable_and_written_nowhere() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    fs::write(&homes.claude, "# Mine\n")?;
    world.write_vault(0, GLOBAL, &format!("Hello\n{BEGIN}\n"))?;
    let service = service(&world, &homes)?;

    let warnings = world.project(&service, OpKind::Create, &[(0, GLOBAL)])?;
    let report = service.assess();

    assert_eq!(codes(&warnings), vec!["projection/invalid-definition"; 2]);
    assert!(
        report
            .findings
            .iter()
            .filter(|finding| finding.kind == FindingKind::OrientationBlock)
            .all(|finding| !finding.auto_fixable())
    );
    assert_eq!(fs::read_to_string(&homes.claude)?, "# Mine\n");
    assert!(!homes.codex.exists());
    Ok(())
}

#[test]
fn resolving_removes_a_block_no_vault_defines_but_never_with_zero_vaults() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    let existing = format!("top\n{}", block("Orient.\n"));
    fs::write(&homes.claude, &existing)?;

    let report = service(&world, &homes)?.assess();
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.kind == FindingKind::OrientationBlock && finding.host == Some(HostKind::ClaudeCode)),
        "{:?}",
        report.findings
    );
    let resolved = service(&world, &homes)?.resolve();

    assert_eq!(codes(&resolved.warnings), Vec::<&str>::new());
    assert_eq!(fs::read_to_string(&homes.claude)?, "top\n");

    fs::write(&homes.claude, &existing)?;
    let empty = world.service_with_instructions_over(&World::no_roots()?, "config-one", &homes.claude, &homes.codex)?;
    let report = empty.resolve();
    assert_eq!(fs::read_to_string(&homes.claude)?, existing);
    assert!(report.findings.iter().all(|f| f.kind != FindingKind::OrientationBlock));
    Ok(())
}

#[test]
fn an_unreadable_earlier_vault_leaves_the_blocks_as_they_are_and_warns() -> TestResult {
    let world = world(2)?;
    let homes = homes()?;
    let existing = block("Old.\n");
    fs::write(&homes.claude, &existing)?;
    // A folder where the file belongs makes the first vault's read fail, so which vault wins cannot be known.
    fs::create_dir_all(world.vault_file(0, GLOBAL))?;
    world.write_vault(1, GLOBAL, "From beta.\n")?;

    let warnings = service(&world, &homes)?.refresh();

    assert_eq!(codes(&warnings), vec!["projection/read-failed"]);
    assert_eq!(fs::read_to_string(&homes.claude)?, existing);
    assert!(!homes.codex.exists());
    Ok(())
}

#[test]
fn the_override_file_is_neither_created_nor_changed_by_any_run() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;
    let service = service(&world, &homes)?;
    assert_eq!(codes(&service.refresh()), Vec::<&str>::new());
    assert_eq!(codes(&service.resolve().warnings), Vec::<&str>::new());
    assert!(!homes.codex.with_file_name("AGENTS.override.md").exists());
    Ok(())
}

#[test]
fn the_override_finding_names_the_host_without_a_path_and_is_not_auto_fixable() -> TestResult {
    let world = world(1)?;
    let homes = homes()?;
    let override_file = homes.codex.with_file_name("AGENTS.override.md");
    fs::write(&override_file, "x\n")?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;

    let report = service(&world, &homes)?.resolve();

    let finding = report
        .findings
        .iter()
        .find(|finding| finding.kind == FindingKind::CodexOverrideHidesAgents)
        .ok_or("no override finding")?;
    assert!(!finding.auto_fixable());
    assert!(finding.message.contains("codex"), "{}", finding.message);
    assert!(
        !finding.message.contains(&*override_file.to_string_lossy()),
        "{}",
        finding.message
    );
    assert_eq!(fs::read_to_string(&override_file)?, "x\n");
    Ok(())
}

#[test]
fn a_host_without_instructions_configured_is_left_alone() -> TestResult {
    let world = world(1)?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;
    let service = world.service("config-one")?;
    assert_eq!(
        codes(&world.project(&service, OpKind::Create, &[(0, GLOBAL)])?),
        Vec::<&str>::new()
    );
    assert!(
        service
            .assess()
            .findings
            .iter()
            .all(|finding| finding.kind != FindingKind::OrientationBlock)
    );
    Ok(())
}
