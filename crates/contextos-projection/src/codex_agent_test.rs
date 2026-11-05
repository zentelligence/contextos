use contextos_obsidian::{FrontmatterDocument, FrontmatterInput};
use sha2::{Digest, Sha256};
use toml::{Table, Value};

use super::*;
use crate::{CodexTier, ModelName, ReasoningEffort, Slug};

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

fn render(definition: &AgentDefinition, tiers: &CodexTiers) -> Result<CodexAgentFile, ProjectionError> {
    CodexAgentFile::try_from(CodexAgentInput { definition, tiers })
}

fn text(file: &CodexAgentFile) -> String {
    String::from_utf8_lossy(file.as_ref()).into_owned()
}

fn parse(file: &CodexAgentFile) -> Result<Table, toml::de::Error> {
    text(file).parse::<Table>()
}

fn string<'a>(table: &'a Table, key: &str) -> Option<&'a str> {
    table.get(key).and_then(Value::as_str)
}

fn keys(table: &Table) -> Vec<&str> {
    table.keys().map(String::as_str).collect()
}

#[test]
fn an_all_tools_agent_has_only_the_documented_keys_and_no_capability_table() -> Result<(), Box<dyn std::error::Error>> {
    let file = render(
        &definition("Reviews code", "balanced", "all", "You review.\n")?,
        &CodexTiers::default(),
    )?;
    let table = parse(&file)?;
    let mut found = keys(&table);
    found.sort_unstable();
    assert_eq!(
        found,
        vec![
            "description",
            "developer_instructions",
            "model_reasoning_effort",
            "name"
        ]
    );
    assert_eq!(string(&table, "name"), Some("code-reviewer"));
    assert_eq!(string(&table, "description"), Some("Reviews code"));
    assert_eq!(string(&table, "developer_instructions"), Some("You review."));
    assert_eq!(string(&table, "model_reasoning_effort"), Some("medium"));
    assert!(text(&file).starts_with("name = "), "{}", text(&file));
    Ok(())
}

#[test]
fn a_read_only_agent_states_it_first_and_disables_the_capability_features() -> Result<(), Box<dyn std::error::Error>> {
    let file = render(
        &definition("d", "fast", "read-only", "Body line\n")?,
        &CodexTiers::default(),
    )?;
    let table = parse(&file)?;
    let instructions = string(&table, "developer_instructions").unwrap_or_default();
    let (first, rest) = instructions.split_once("\n\n").unwrap_or_default();
    assert!(!first.contains('\n'));
    assert!(first.contains("read-only"), "{first}");
    assert!(first.contains("must not modify files"), "{first}");
    assert!(first.contains("change state"), "{first}");
    assert_eq!(rest, "Body line");
    let features = table
        .get("features")
        .and_then(Value::as_table)
        .ok_or("no features table")?;
    let mut found = keys(features);
    found.sort_unstable();
    assert_eq!(found, vec!["apps", "plugins", "shell_tool"]);
    for key in ["apps", "plugins", "shell_tool"] {
        assert_eq!(features.get(key).and_then(Value::as_bool), Some(false), "{key}");
    }
    let rendered = text(&file);
    let table_at = rendered.find("[features]").ok_or("no [features] header")?;
    assert!(rendered[..table_at].contains("model_reasoning_effort"));
    assert!(!rendered[table_at..].contains("developer_instructions"));
    Ok(())
}

#[test]
fn each_tier_resolves_its_effort_and_optional_model() -> Result<(), Box<dyn std::error::Error>> {
    let tiers = CodexTiers::new(
        CodexTier::new(Some(ModelName::try_from("small")?), ReasoningEffort::Minimal),
        CodexTier::new(None, ReasoningEffort::Xhigh),
        CodexTier::new(Some(ModelName::try_from("large")?), ReasoningEffort::Max),
    );
    for (tier, model, effort) in [
        ("fast", Some("small"), "minimal"),
        ("balanced", None, "xhigh"),
        ("deep", Some("large"), "max"),
    ] {
        let table = parse(&render(&definition("d", tier, "all", "Body")?, &tiers)?)?;
        assert_eq!(string(&table, "model"), model, "{tier}");
        assert_eq!(string(&table, "model_reasoning_effort"), Some(effort), "{tier}");
    }
    Ok(())
}

#[test]
fn the_default_efforts_are_low_medium_and_high() -> Result<(), Box<dyn std::error::Error>> {
    for (tier, effort) in [("fast", "low"), ("balanced", "medium"), ("deep", "high")] {
        let table = parse(&render(&definition("d", tier, "all", "Body")?, &CodexTiers::default())?)?;
        assert_eq!(string(&table, "model_reasoning_effort"), Some(effort));
        assert!(table.get("model").is_none());
    }
    Ok(())
}

#[test]
fn hostile_text_round_trips_through_the_toml_parser() -> Result<(), Box<dyn std::error::Error>> {
    let texts = [
        "triple \"\"\" quotes and ''' apostrophes",
        "back\\slash and trailing\\",
        "C:\\path\\to\\file \\n not a newline",
        "line one\nline two\n\n\"\"\"\nline five\\",
        "tab\there and \"quotes\"",
        "unicode \u{2603} \u{1F600}",
        "[features]\nshell_tool = true",
        "key = \"value\"",
        "ends with quote\"",
        "a\u{7f}del",
    ];
    for hostile in texts {
        let source = definition(&hostile.replace('\n', " "), "balanced", "read-only", hostile)?;
        let table = parse(&render(&source, &CodexTiers::default())?)?;
        assert_eq!(string(&table, "description"), Some(source.description()), "{hostile:?}");
        let instructions = string(&table, "developer_instructions").unwrap_or_default();
        assert!(instructions.ends_with(source.body()), "{hostile:?}");
        let features = table.get("features").and_then(Value::as_table).ok_or("no features")?;
        assert_eq!(
            features.get("shell_tool").and_then(Value::as_bool),
            Some(false),
            "{hostile:?}"
        );
    }
    Ok(())
}

#[test]
fn identical_input_gives_identical_bytes_and_a_stable_hash() -> Result<(), ProjectionError> {
    let source = definition("d", "deep", "read-only", "Body")?;
    let first = render(&source, &CodexTiers::default())?;
    let second = render(&source, &CodexTiers::default())?;
    assert_eq!(first, second);
    assert!(!first.as_ref().contains(&b'\r'));
    assert_eq!(
        ContentHash::from(&first),
        ContentHash::from(<[u8; 32]>::from(Sha256::digest(first.as_ref())))
    );
    assert_eq!(Vec::<u8>::from(first.clone()), first.as_ref());
    Ok(())
}
