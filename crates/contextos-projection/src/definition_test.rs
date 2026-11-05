use super::*;
use crate::SkillFilePath;

fn slug() -> Result<Slug, ProjectionError> {
    Slug::try_from("code-reviewer")
}

fn parse(source: &str) -> Result<AgentDefinition, ProjectionError> {
    AgentDefinition::try_from((&slug()?, source.as_bytes()))
}

fn rejection(source: &str) -> Option<(&'static str, DefinitionRejection)> {
    match parse(source) {
        Err(ProjectionError::InvalidDefinition { field, reason }) => Some((field, reason)),
        _ => None,
    }
}

const VALID: &str = "---\ndescription: Reviews code\ntier: balanced\naccess: read-only\n---\nYou review code.\n";

#[test]
fn a_valid_definition_exposes_each_field_and_the_slug() -> Result<(), ProjectionError> {
    let definition = parse(VALID)?;
    assert_eq!(<&str>::from(definition.slug()), "code-reviewer");
    assert_eq!(definition.description(), "Reviews code");
    assert_eq!(definition.tier(), Tier::Balanced);
    assert_eq!(definition.access(), &Access::ReadOnly);
    assert_eq!(definition.body(), "You review code.");
    Ok(())
}

#[test]
fn other_frontmatter_keys_are_ignored_and_the_slug_names_the_agent() -> Result<(), ProjectionError> {
    let source =
        "---\nname: something-else\ntype: agent\ntitle: A title\ndescription: d\ntier: deep\naccess: all\n---\nbody";
    let definition = parse(source)?;
    assert_eq!(<&str>::from(definition.slug()), "code-reviewer");
    assert_eq!(definition.tier(), Tier::Deep);
    assert_eq!(definition.access(), &Access::All);
    Ok(())
}

#[test]
fn the_body_loses_leading_blank_lines_and_trailing_whitespace_and_uses_unix_line_endings() -> Result<(), ProjectionError>
{
    let source = "---\r\ndescription: d\r\ntier: fast\r\naccess: all\r\n---\r\n\r\n\r\nLine one\r\n  indented\r\n\r\n";
    assert_eq!(parse(source)?.body(), "Line one\n  indented");
    Ok(())
}

#[test]
fn a_description_is_trimmed() -> Result<(), ProjectionError> {
    let source = "---\ndescription: |\n  Two\n  lines\ntier: fast\naccess: all\n---\nb";
    assert_eq!(parse(source)?.description(), "Two\nlines");
    Ok(())
}

#[test]
fn bytes_that_are_not_utf8_are_rejected() {
    let bytes: &[u8] = &[0xff, 0xfe, 0x00];
    let result = slug().and_then(|slug| AgentDefinition::try_from((&slug, bytes)));
    assert!(matches!(
        result,
        Err(ProjectionError::InvalidDefinition {
            field: "definition",
            reason: DefinitionRejection::NotUtf8
        })
    ));
}

#[test]
fn frontmatter_that_is_unclosed_or_invalid_yaml_is_rejected() {
    let malformed = Some(("frontmatter", DefinitionRejection::Malformed));
    assert_eq!(
        rejection("---\ndescription: d\ntier: fast\naccess: all\nbody"),
        malformed
    );
    assert_eq!(rejection("---\ndescription: [unclosed\n---\nbody"), malformed);
    assert_eq!(rejection("---\n- a\n- b\n---\nbody"), malformed);
}

#[test]
fn a_definition_without_frontmatter_lacks_its_description() {
    assert_eq!(
        rejection("just a body"),
        Some(("description", DefinitionRejection::Missing))
    );
}

#[test]
fn each_required_field_reports_missing_empty_or_wrong_type() {
    use DefinitionRejection::{Empty, Missing, WrongType};
    let cases = [
        ("tier: fast\naccess: all", "description", Missing),
        ("description: ''\ntier: fast\naccess: all", "description", Empty),
        ("description: '  '\ntier: fast\naccess: all", "description", Empty),
        ("description: ~\ntier: fast\naccess: all", "description", WrongType),
        ("description: 42\ntier: fast\naccess: all", "description", WrongType),
        ("description: [a]\ntier: fast\naccess: all", "description", WrongType),
        ("description: d\naccess: all", "tier", Missing),
        ("description: d\ntier: ''\naccess: all", "tier", Empty),
        ("description: d\ntier: 1\naccess: all", "tier", WrongType),
        ("description: d\ntier: fast\naccess: ''", "access", Empty),
        ("description: d\ntier: fast\naccess: true", "access", WrongType),
    ];
    for (frontmatter, field, reason) in cases {
        let source = format!("---\n{frontmatter}\n---\nbody");
        assert_eq!(rejection(&source), Some((field, reason)), "{frontmatter}");
    }
}

#[test]
fn a_vendor_model_or_unknown_tool_access_is_rejected_by_field() {
    let unrecognised = DefinitionRejection::Unrecognised;
    assert_eq!(
        rejection("---\ndescription: d\ntier: sonnet\naccess: all\n---\nb"),
        Some(("tier", unrecognised))
    );
    assert_eq!(
        rejection("---\ndescription: d\ntier: fast\naccess: Read\n---\nb"),
        Some(("access", unrecognised))
    );
}

#[test]
fn a_definition_without_access_defaults_to_all() -> Result<(), ProjectionError> {
    let definition = parse("---\ndescription: d\ntier: fast\n---\nbody")?;
    assert_eq!(definition.access(), &Access::All);
    Ok(())
}

#[test]
fn each_documented_access_is_read_from_the_access_field() -> Result<(), ProjectionError> {
    let all = parse("---\ndescription: d\ntier: fast\naccess: all\n---\nbody")?;
    let read_only = parse("---\ndescription: d\ntier: fast\naccess: read-only\n---\nbody")?;
    assert_eq!(all.access(), &Access::All);
    assert_eq!(read_only.access(), &Access::ReadOnly);
    Ok(())
}

#[test]
fn a_definition_carrying_a_host_model_field_is_refused_by_naming_it() {
    assert_eq!(
        rejection("---\ndescription: d\ntier: fast\nmodel: sonnet\n---\nbody"),
        Some(("model", DefinitionRejection::Reserved))
    );
    assert_eq!(
        rejection("---\ndescription: d\ntier: fast\nmodel: fast\n---\nbody"),
        Some(("model", DefinitionRejection::Reserved))
    );
}

#[test]
fn a_definition_carrying_a_host_tools_field_is_refused_by_naming_it() {
    assert_eq!(
        rejection("---\ndescription: d\ntier: fast\ntools: all\n---\nbody"),
        Some(("tools", DefinitionRejection::Reserved))
    );
    assert_eq!(
        rejection("---\ndescription: d\ntier: fast\ntools: [Read, Grep]\n---\nbody"),
        Some(("tools", DefinitionRejection::Reserved))
    );
}

#[test]
fn a_definition_without_a_tier_is_refused_by_naming_the_field() {
    assert_eq!(
        rejection("---\ndescription: d\naccess: all\n---\nbody"),
        Some(("tier", DefinitionRejection::Missing))
    );
}

#[test]
fn an_unknown_tier_or_access_value_is_refused_by_naming_its_field() {
    let unrecognised = DefinitionRejection::Unrecognised;
    assert_eq!(
        rejection("---\ndescription: d\ntier: medium\n---\nbody"),
        Some(("tier", unrecognised))
    );
    assert_eq!(
        rejection("---\ndescription: d\ntier: fast\naccess: write\n---\nbody"),
        Some(("access", unrecognised))
    );
}

#[test]
fn an_empty_or_whitespace_body_is_rejected() {
    for body in ["", "\n", "  \n\t\n"] {
        let source = format!("---\ndescription: d\ntier: fast\naccess: all\n---\n{body}");
        assert_eq!(
            rejection(&source),
            Some(("body", DefinitionRejection::Empty)),
            "{body:?}"
        );
    }
}

#[test]
fn the_error_never_echoes_definition_content() {
    let error = parse("---\ndescription: d\ntier: SECRET-VALUE\naccess: all\n---\nb").err();
    let message = error.map(|error| error.to_string()).unwrap_or_default();
    assert!(message.contains("tier"), "{message}");
    assert!(!message.contains("SECRET-VALUE"), "{message}");
}

#[test]
fn every_invalid_definition_has_the_stable_code() {
    let code = parse("x").err().map(|error| error.code());
    assert_eq!(code, Some("projection/invalid-definition"));
}

fn folder(files: &[(&str, &str)]) -> Result<SkillFolder, ProjectionError> {
    let mut entries = Vec::new();
    for (name, content) in files {
        entries.push((SkillFilePath::try_from(*name)?, content.as_bytes().to_vec()));
    }
    SkillFolder::try_from(entries)
}

fn skill_rejection(files: &[(&str, &str)]) -> Option<(&'static str, DefinitionRejection)> {
    let result = folder(files).and_then(|folder| SkillDefinition::try_from((slug()?, folder)));
    match result {
        Err(ProjectionError::InvalidDefinition { field, reason }) => Some((field, reason)),
        _ => None,
    }
}

#[test]
fn a_skill_with_name_and_description_keeps_its_files_unchanged() -> Result<(), ProjectionError> {
    let files = [
        ("SKILL.md", "---\nname: x\ndescription: Does x\n---\nSteps"),
        ("notes/a.txt", "A"),
    ];
    let skill = SkillDefinition::try_from((slug()?, folder(&files)?))?;
    assert_eq!(<&str>::from(skill.slug()), "code-reviewer");
    assert_eq!(skill.folder(), &folder(&files)?);
    Ok(())
}

#[test]
fn a_skill_needs_a_skill_file_with_name_and_description() {
    use DefinitionRejection::{Empty, Malformed, Missing, WrongType};
    assert_eq!(skill_rejection(&[("README.md", "x")]), Some(("SKILL.md", Missing)));
    assert_eq!(skill_rejection(&[("skill.md", "x")]), Some(("SKILL.md", Missing)));
    assert_eq!(
        skill_rejection(&[("SKILL.md", "---\nname: x\ndescription: [\n---\n")]),
        Some(("frontmatter", Malformed))
    );
    assert_eq!(
        skill_rejection(&[("SKILL.md", "no frontmatter")]),
        Some(("name", Missing))
    );
    assert_eq!(
        skill_rejection(&[("SKILL.md", "---\ndescription: d\n---\n")]),
        Some(("name", Missing))
    );
    assert_eq!(
        skill_rejection(&[("SKILL.md", "---\nname: ' '\ndescription: d\n---\n")]),
        Some(("name", Empty))
    );
    assert_eq!(
        skill_rejection(&[("SKILL.md", "---\nname: x\n---\n")]),
        Some(("description", Missing))
    );
    assert_eq!(
        skill_rejection(&[("SKILL.md", "---\nname: x\ndescription: 3\n---\n")]),
        Some(("description", WrongType))
    );
    assert_eq!(
        skill_rejection(&[("SKILL.md", "---\nname: x\ndescription: ''\n---\n")]),
        Some(("description", Empty))
    );
}

#[test]
fn a_skill_file_that_is_not_utf8_is_rejected() -> Result<(), ProjectionError> {
    let folder = SkillFolder::try_from(vec![(SkillFilePath::try_from("SKILL.md")?, vec![0xff, 0xfe])])?;
    let result = SkillDefinition::try_from((slug()?, folder));
    assert!(matches!(
        result,
        Err(ProjectionError::InvalidDefinition {
            field: "SKILL.md",
            reason: DefinitionRejection::NotUtf8
        })
    ));
    Ok(())
}
