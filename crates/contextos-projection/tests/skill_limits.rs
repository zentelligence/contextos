//! A skill folder beyond the depth or size limits is an invalid definition: it is not projected, the warning names the
//! problem, and `doctor` reports it as such.

mod common;

use std::fs;

use common::{TestResult, codes, skill, world};
use contextos_projection::FindingKind;

#[test]
fn a_skill_nested_more_than_eight_levels_is_reported_invalid_and_not_projected() -> TestResult {
    let world = world(1)?;
    world.write_vault(0, "registry/skills/deep/SKILL.md", &skill("deep"))?;
    world.write_vault(0, "registry/skills/deep/a/b/c/d/e/f/g/h/i/note.md", "too deep")?;
    let service = world.service("config-one")?;

    let warnings = service.refresh();

    // One warning per definition, not one per host.
    assert_eq!(codes(&warnings), ["projection/invalid-definition"]);
    assert!(!world.claude_skill("deep").exists() && !world.codex_skill("deep").exists());
    let kinds: Vec<FindingKind> = service.assess().findings.iter().map(|finding| finding.kind).collect();
    assert!(
        kinds
            .iter()
            .all(|kind| *kind == FindingKind::ProjectionInvalidDefinition),
        "{kinds:?}"
    );
    assert_ne!(kinds, Vec::<FindingKind>::new());
    Ok(())
}

#[test]
fn a_skill_over_thirty_two_mebibytes_is_reported_invalid_and_not_projected() -> TestResult {
    let world = world(1)?;
    world.write_vault(0, "registry/skills/heavy/SKILL.md", &skill("heavy"))?;
    for index in 0..33 {
        let path = world.vault_file(0, &format!("registry/skills/heavy/part-{index}.bin"));
        fs::File::create(path)?.set_len((1 << 20) - 1)?;
    }
    let service = world.service("config-one")?;

    let warnings = service.refresh();

    // One warning per definition, not one per host.
    assert_eq!(codes(&warnings), ["projection/invalid-definition"]);
    assert!(!world.claude_skill("heavy").exists());
    Ok(())
}
