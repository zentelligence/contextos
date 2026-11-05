//! Property tests: any valid agent definition renders host files that parse back to the same description and body.

use contextos_obsidian::FrontmatterDocument;
use contextos_projection::{
    AgentDefinition, ClaudeAgentFile, ClaudeAgentInput, ClaudeTiers, CodexAgentFile, CodexAgentInput, CodexTiers,
    ReadOnlyTools, Slug,
};
use proptest::prelude::*;
use serde_json::{Map, Value};

fn definition(description: &str, tier: &str, access: &str, body: &str) -> Option<AgentDefinition> {
    let mut frontmatter = Map::new();
    frontmatter.insert("description".into(), Value::String(description.into()));
    frontmatter.insert("tier".into(), Value::String(tier.into()));
    frontmatter.insert("access".into(), Value::String(access.into()));
    let document = FrontmatterDocument::from(contextos_obsidian::FrontmatterInput {
        frontmatter,
        body: body.into(),
    });
    let source = String::try_from(&document).ok()?;
    let slug = Slug::try_from("agent-one").ok()?;
    AgentDefinition::try_from((&slug, source.as_bytes())).ok()
}

proptest! {
    #[test]
    fn both_hosts_round_trip_description_and_body(
        description in "[a-z0-9:'\"#@&*!{}\\[\\]%,|>?-]{1,12}( [a-z0-9:'\"#-]{1,8}){0,3}(\n[a-z0-9: -]{1,10}){0,2}",
        body in "\\PC{1,30}(\n\\PC{0,30}){0,4}",
        tier in prop::sample::select(vec!["fast", "balanced", "deep"]),
        access in prop::sample::select(vec!["all", "read-only"]),
    ) {
        let Some(definition) = definition(&description, tier, access, &body) else {
            // Blank after trimming: the contract rejects it, which the unit tests cover.
            prop_assume!(false);
            return Ok(());
        };

        let claude = ClaudeAgentFile::try_from(ClaudeAgentInput {
            definition: &definition,
            tiers: &ClaudeTiers::default(),
            read_only_tools: &ReadOnlyTools::default(),
        }).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let text = String::from_utf8(claude.as_ref().to_vec()).map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert!(text.starts_with("---\n"));
        let parsed = FrontmatterDocument::try_from(text.as_str()).map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(parsed.frontmatter().get("description").and_then(Value::as_str), Some(definition.description()));
        prop_assert_eq!(parsed.frontmatter().get("name").and_then(Value::as_str), Some("agent-one"));
        let expected_body = format!("\n{}\n", definition.body());
        prop_assert_eq!(parsed.body(), expected_body.as_str());

        let codex = CodexAgentFile::try_from(CodexAgentInput { definition: &definition, tiers: &CodexTiers::default() })
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let text = String::from_utf8(codex.as_ref().to_vec()).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let table = text.parse::<toml::Table>().map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(table.get("description").and_then(toml::Value::as_str), Some(definition.description()));
        let instructions = table.get("developer_instructions").and_then(toml::Value::as_str).unwrap_or_default();
        prop_assert!(instructions.ends_with(definition.body()));
    }
}
