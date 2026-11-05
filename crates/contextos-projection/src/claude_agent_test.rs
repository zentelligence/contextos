use contextos_obsidian::{FrontmatterDocument, FrontmatterInput};
use sha2::{Digest, Sha256};

use super::*;
use crate::{ModelName, Slug, Tier};

fn definition(description: &str, tier: &str, access: &str, body: &str) -> Result<AgentDefinition, ProjectionError> {
    let mut frontmatter = serde_json::Map::new();
    for (key, value) in [("description", description), ("tier", tier), ("access", access)] {
        frontmatter.insert(key.to_owned(), serde_json::Value::String(value.to_owned()));
    }
    let document = FrontmatterDocument::from(FrontmatterInput {
        frontmatter,
        body: body.to_owned(),
    });
    let source = String::try_from(&document).map_err(|source| ProjectionError::RenderYaml { source })?;
    AgentDefinition::try_from((&Slug::try_from("code-reviewer")?, source.as_bytes()))
}

fn render(definition: &AgentDefinition, tools: &ReadOnlyTools) -> Result<ClaudeAgentFile, ProjectionError> {
    ClaudeAgentFile::try_from(ClaudeAgentInput {
        definition,
        tiers: &ClaudeTiers::default(),
        read_only_tools: tools,
    })
}

fn text(file: &ClaudeAgentFile) -> String {
    String::from_utf8_lossy(file.as_ref()).into_owned()
}

#[test]
fn a_read_only_agent_renders_the_exact_documented_file() -> Result<(), ProjectionError> {
    let tools = ReadOnlyTools::try_from(("contextos", ["fs_read_text_file", "base_query"].as_slice()))?;
    let file = render(
        &definition("Reviews code", "balanced", "read-only", "You review code.\n")?,
        &tools,
    )?;
    assert_eq!(
        text(&file),
        "---\nname: code-reviewer\ndescription: Reviews code\nmodel: sonnet\n\
         tools: Read, Grep, Glob, WebFetch, WebSearch, mcp__contextos__base_query, mcp__contextos__fs_read_text_file\n\
         ---\n\nYou review code.\n"
    );
    Ok(())
}

#[test]
fn an_agent_with_all_tools_has_no_tools_key() -> Result<(), ProjectionError> {
    let tools = ReadOnlyTools::try_from(("contextos", ["base_query"].as_slice()))?;
    let file = render(&definition("d", "fast", "all", "Body")?, &tools)?;
    assert_eq!(
        text(&file),
        "---\nname: code-reviewer\ndescription: d\nmodel: haiku\n---\n\nBody\n"
    );
    Ok(())
}

#[test]
fn a_read_only_agent_with_no_contextos_tools_lists_only_the_base_tools() -> Result<(), ProjectionError> {
    let file = render(
        &definition("d", "fast", "read-only", "Body")?,
        &ReadOnlyTools::default(),
    )?;
    assert!(
        text(&file).contains("\ntools: Read, Grep, Glob, WebFetch, WebSearch\n"),
        "{}",
        text(&file)
    );
    Ok(())
}

#[test]
fn each_tier_resolves_through_the_configured_mapping() -> Result<(), ProjectionError> {
    let tiers = ClaudeTiers::new(
        ModelName::try_from("m-fast")?,
        ModelName::try_from("m-bal")?,
        ModelName::try_from("m-deep")?,
    );
    for (tier, expected) in [("fast", "m-fast"), ("balanced", "m-bal"), ("deep", "m-deep")] {
        let definition = definition("d", tier, "all", "Body")?;
        let file = ClaudeAgentFile::try_from(ClaudeAgentInput {
            definition: &definition,
            tiers: &tiers,
            read_only_tools: &ReadOnlyTools::default(),
        })?;
        let parsed =
            FrontmatterDocument::try_from(text(&file)).map_err(|source| ProjectionError::RenderYaml { source })?;
        assert_eq!(
            parsed.frontmatter().get("model").and_then(|value| value.as_str()),
            Some(expected)
        );
    }
    assert_eq!(definition("d", "fast", "all", "b")?.tier(), Tier::Fast);
    Ok(())
}

#[test]
fn hostile_descriptions_round_trip_through_the_workspace_yaml_parser() -> Result<(), ProjectionError> {
    let descriptions = [
        "Has: a colon",
        "Quote \" and ' marks",
        "- leading dash",
        "# not a comment",
        "{braces} and [brackets]",
        "line one\nline two\n\nline four",
        "yes",
        "null",
        "123",
        "@at and `tick` and *star* and &amp !bang",
        "ends with colon:",
        "  inner   spacing  kept",
        "unicode \u{2603} and emoji \u{1F600}",
        "---",
        "a --- b\n--- c",
    ];
    for description in descriptions {
        let source = definition(description, "balanced", "read-only", "Body text")?;
        let file = render(&source, &ReadOnlyTools::default())?;
        let rendered = text(&file);
        assert!(rendered.starts_with("---\n"), "{rendered}");
        let parsed = FrontmatterDocument::try_from(rendered.as_str())
            .map_err(|source| ProjectionError::RenderYaml { source })?;
        let get = |key: &str| {
            parsed
                .frontmatter()
                .get(key)
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        };
        assert_eq!(get("name").as_deref(), Some("code-reviewer"), "{rendered}");
        assert_eq!(get("description").as_deref(), Some(source.description()), "{rendered}");
        assert_eq!(get("model").as_deref(), Some("sonnet"), "{rendered}");
        assert_eq!(parsed.body(), "\nBody text\n", "{rendered}");
        assert_eq!(parsed.frontmatter().len(), 4, "{rendered}");
    }
    Ok(())
}

#[test]
fn the_body_is_carried_verbatim_including_lookalike_delimiters() -> Result<(), ProjectionError> {
    let body = "# Heading\n\n---\n\n    indented code\n\n```\n---\n```\n";
    let source = definition("d", "fast", "all", body)?;
    let rendered = text(&render(&source, &ReadOnlyTools::default())?);
    let parsed =
        FrontmatterDocument::try_from(rendered.as_str()).map_err(|source| ProjectionError::RenderYaml { source })?;
    assert_eq!(parsed.body(), format!("\n{}\n", source.body()));
    Ok(())
}

#[test]
fn identical_input_gives_identical_bytes_and_a_stable_hash() -> Result<(), ProjectionError> {
    let source = definition("Has: colon", "deep", "read-only", "Body")?;
    let tools = ReadOnlyTools::try_from(("contextos", ["b", "a"].as_slice()))?;
    let first = render(&source, &tools)?;
    let second = render(&source, &tools)?;
    assert_eq!(first, second);
    assert!(!first.as_ref().contains(&b'\r'));
    assert_eq!(ContentHash::from(&first), ContentHash::from(&second));
    let expected = ContentHash::from(<[u8; 32]>::from(Sha256::digest(first.as_ref())));
    assert_eq!(ContentHash::from(&first), expected);
    assert_eq!(Vec::<u8>::from(first.clone()), first.as_ref());
    Ok(())
}
