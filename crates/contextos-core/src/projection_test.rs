use super::*;
use crate::{VaultPathInput, VaultRoot, VaultRootInput, VaultSet};

fn classify(raw: &str) -> Result<ProjectionScope, NotProjected> {
    let dir = tempfile::tempdir().map_err(|_| NotProjected)?;
    let roots = VaultSet::try_from(vec![
        VaultRoot::try_from(VaultRootInput {
            path: dir.path().to_path_buf(),
            managed: true,
            name: Some("vault".to_owned()),
        })
        .map_err(|_| NotProjected)?,
    ])
    .map_err(|_| NotProjected)?;
    let path = VaultPath::try_from(VaultPathInput { roots: &roots, raw }).map_err(|_| NotProjected)?;
    ProjectionScope::try_from(&path)
}

fn agent(name: &str) -> ProjectionScope {
    ProjectionScope::Agent(name.to_owned())
}

fn skill(name: &str) -> ProjectionScope {
    ProjectionScope::Skill(name.to_owned())
}

#[test]
fn a_direct_markdown_child_of_the_agents_folder_is_an_agent_named_without_its_extension() {
    assert_eq!(classify("registry/agents/critic.md"), Ok(agent("critic")));
}

#[test]
fn an_agent_name_is_the_raw_component_even_when_it_would_not_be_a_valid_slug() {
    assert_eq!(classify("registry/agents/Bad Name.md"), Ok(agent("Bad Name")));
    assert_eq!(classify("registry/agents/a.b.md"), Ok(agent("a.b")));
}

#[test]
fn agent_paths_that_are_not_direct_markdown_children_are_not_projected() {
    for raw in [
        "registry/agents/team/critic.md",
        "registry/agents/critic.txt",
        "registry/agents/critic",
        "registry/agents/critic.MD",
        "registry/agents/.md",
        "registry/agents/index.md",
    ] {
        assert_eq!(classify(raw), Err(NotProjected), "{raw}");
    }
}

#[test]
fn a_skill_folder_and_everything_beneath_it_is_that_skill() {
    for raw in [
        "registry/skills/triage",
        "registry/skills/triage/SKILL.md",
        "registry/skills/triage/scripts/run.sh",
    ] {
        assert_eq!(classify(raw), Ok(skill("triage")), "{raw}");
    }
}

#[test]
fn the_skills_folder_index_is_not_a_skill() {
    assert_eq!(classify("registry/skills/index.md"), Err(NotProjected));
}

#[test]
fn only_the_global_instructions_file_is_the_orientation() {
    assert_eq!(
        classify("registry/instructions/global.md"),
        Ok(ProjectionScope::Orientation)
    );
    for raw in [
        "registry/instructions/other.md",
        "registry/instructions/index.md",
        "registry/instructions/global.md/x.md",
        "registry/instructions/sub/global.md",
    ] {
        assert_eq!(classify(raw), Err(NotProjected), "{raw}");
    }
}

#[test]
fn a_registry_folder_moved_or_deleted_wholesale_affects_every_definition() {
    for raw in [
        "registry",
        "registry/agents",
        "registry/skills",
        "registry/instructions",
    ] {
        assert_eq!(classify(raw), Ok(ProjectionScope::AllDefinitions), "{raw}");
    }
}

#[test]
fn look_alike_prefixes_other_registry_folders_and_ordinary_notes_are_not_projected() {
    for raw in [
        "registry/agentsx/a.md",
        "registryx/agents/a.md",
        "xregistry/agents/a.md",
        "registry/roles/critic.md",
        "registry/hats",
        "registry/index.md",
        "notes/registry/agents/a.md",
        "notes/a.md",
    ] {
        assert_eq!(classify(raw), Err(NotProjected), "{raw}");
    }
}

#[test]
fn a_managed_index_at_any_depth_inside_a_skill_folder_is_not_part_of_the_skill() {
    for raw in [
        "registry/skills/triage/index.md",
        "registry/skills/triage/scripts/index.md",
        "registry/skills/triage/scripts/deep/er/index.md",
    ] {
        assert_eq!(classify(raw), Err(NotProjected), "{raw}");
    }
}

#[test]
fn a_file_that_only_resembles_the_managed_index_still_belongs_to_its_skill() {
    for raw in [
        "registry/skills/triage/my-index.md",
        "registry/skills/triage/index.markdown",
        "registry/skills/triage/scripts/index",
    ] {
        assert_eq!(classify(raw), Ok(skill("triage")), "{raw}");
    }
}

#[test]
fn the_registry_folder_names_and_the_orientation_file_are_matched_without_regard_to_case() {
    assert_eq!(classify("Registry/Agents/critic.md"), Ok(agent("critic")));
    assert_eq!(classify("Registry/agents/a.md"), Ok(agent("a")));
    assert_eq!(classify("registry/Agents/a.md"), Ok(agent("a")));
    assert_eq!(classify("REGISTRY/agents/critic.md"), Ok(agent("critic")));
    assert_eq!(classify("registry/AGENTS/critic.md"), Ok(agent("critic")));
    assert_eq!(classify("Registry/Skills/triage"), Ok(skill("triage")));
    assert_eq!(classify("registry/SKILLS/triage/SKILL.md"), Ok(skill("triage")));
    assert_eq!(
        classify("Registry/Instructions/Global.md"),
        Ok(ProjectionScope::Orientation)
    );
    assert_eq!(
        classify("registry/instructions/GLOBAL.MD"),
        Err(NotProjected),
        "the extension is not folded"
    );
}

#[test]
fn a_case_folded_registry_folder_moved_wholesale_affects_every_definition() {
    for raw in [
        "Registry",
        "registry/Agents",
        "REGISTRY/SKILLS",
        "Registry/INSTRUCTIONS",
    ] {
        assert_eq!(classify(raw), Ok(ProjectionScope::AllDefinitions), "{raw}");
    }
}

#[test]
fn the_definition_name_keeps_its_case_when_the_folders_are_matched_without_regard_to_case() {
    assert_eq!(classify("Registry/Agents/Critic.md"), Ok(agent("Critic")));
    assert_eq!(classify("registry/skills/Triage/SKILL.md"), Ok(skill("Triage")));
}

#[test]
fn case_folding_does_not_make_look_alike_folders_match() {
    for raw in [
        "Registry/Agentsx/a.md",
        "Registryx/Agents/a.md",
        "registry/Roles/critic.md",
        "registry/Instructions/other.md",
        "notes/Registry/Agents/a.md",
    ] {
        assert_eq!(classify(raw), Err(NotProjected), "{raw}");
    }
}
