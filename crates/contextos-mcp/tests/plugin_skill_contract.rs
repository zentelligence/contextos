//! Keeps the `ContextOS` plugin's `mcp-tools` skill in step with this server's tool catalogue.
//!
//! The skill teaches an agent how to use the tools well, so a tool that is added, renamed, or removed here must be
//! reflected there in the same delivery. These checks catch the mechanical part of that drift: a shipped tool the skill
//! never mentions, and a tool the skill still names that no longer exists. They cannot judge whether the advice itself
//! is still right, which is what the review lenses are for.
//!
//! The matching logic is exercised here against in-memory and temporary-directory inputs, so its failure paths are
//! proven on every run. The skill itself lives in a separate repository, so the two tests that read it are ignored by
//! default and never run as part of a plain `cargo test`. `.claude/scripts/check-plugin-skill.sh` locates the plugin
//! checkout, points `CONTEXTOS_PLUGIN_SKILL_DIR` at its `skills/mcp-tools` directory, and runs them, reporting a skip
//! when no checkout is available.

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use contextos_mcp::ContextOsServer;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

/// Names the directory that holds the skill's Markdown files.
const SKILL_DIRECTORY_VARIABLE: &str = "CONTEXTOS_PLUGIN_SKILL_DIR";

/// The prefix of the optional tool family, which the operator enables per server and which the skill covers with one
/// wildcard mention rather than naming each tool.
const OPTIONAL_FAMILY_PREFIX: &str = "ephemeris_";

/// The leading word of every tool name. An identifier in the skill that starts with one of these is treated as a
/// reference to a tool, and so must exist in the catalogue. A test requires every catalogue tool to belong to one, so a
/// new family cannot be added without extending this list.
const TOOL_FAMILIES: &[&str] = &[
    "agent",
    "base",
    "canvas",
    "doctor",
    "ephemeris",
    "frontmatter",
    "fs",
    "git",
    "links",
    "mermaid",
    "note",
    "query",
    "vault",
];

/// Identifiers that share a tool family's leading word but are not tools: a frontmatter property name, the name the
/// skill uses for a fuller record tool it tells the agent to prefer if the catalogue ever lists it, the result
/// field `doctor` and `doctor_resolve` use to name the vault a finding concerns, and the result field
/// `fs_attach_file` uses to say its content has had its links expanded.
const NON_TOOL_IDENTIFIERS: &[&str] = &["note_type", "base_record", "vault_name", "vault_path", "links_expanded"];

fn catalogue_names() -> BTreeSet<String> {
    ContextOsServer::catalogue()
        .list_all()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect()
}

/// Every inline code span in every top-level Markdown file of `directory`.
fn code_spans_in(directory: &Path) -> Result<BTreeSet<String>, Box<dyn Error + Send + Sync>> {
    let mut spans = BTreeSet::new();
    let mut files = 0_usize;
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().and_then(OsStr::to_str) != Some("md") {
            continue;
        }
        files += 1;
        let text = std::fs::read_to_string(&path)?;
        // Splitting on backticks alternates prose and code, so the odd segments are the code spans.
        spans.extend(text.split('`').skip(1).step_by(2).map(str::to_owned));
    }
    if files == 0 {
        return Err(format!("{} holds no Markdown files", directory.display()).into());
    }
    Ok(spans)
}

fn configured_skill_spans() -> Result<BTreeSet<String>, Box<dyn Error + Send + Sync>> {
    let variable = std::env::var(SKILL_DIRECTORY_VARIABLE).map_err(|_| {
        format!(
            "{SKILL_DIRECTORY_VARIABLE} must name the plugin's skills/mcp-tools directory; run \
             .claude/scripts/check-plugin-skill.sh"
        )
    })?;
    code_spans_in(&PathBuf::from(variable))
}

fn is_mentioned(spans: &BTreeSet<String>, tool: &str) -> bool {
    spans.contains(tool)
        || (tool.starts_with(OPTIONAL_FAMILY_PREFIX) && spans.contains(&format!("{OPTIONAL_FAMILY_PREFIX}*")))
}

fn looks_like_tool_reference(span: &str) -> bool {
    span.contains('_')
        && span.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        && span
            .split('_')
            .next()
            .is_some_and(|family| TOOL_FAMILIES.contains(&family))
        && !NON_TOOL_IDENTIFIERS.contains(&span)
}

/// The catalogue tools the skill never names.
fn unmentioned_tools(catalogue: &BTreeSet<String>, spans: &BTreeSet<String>) -> Vec<String> {
    catalogue
        .iter()
        .filter(|tool| !is_mentioned(spans, tool))
        .cloned()
        .collect()
}

/// The tool-like identifiers the skill names that the catalogue does not ship.
fn stale_references(catalogue: &BTreeSet<String>, spans: &BTreeSet<String>) -> Vec<String> {
    spans
        .iter()
        .filter(|span| looks_like_tool_reference(span) && !catalogue.contains(span.as_str()))
        .cloned()
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[test]
fn a_tool_the_skill_never_names_is_reported() {
    let catalogue = set(&["fs_read_text_file", "fs_write_file"]);
    let spans = set(&["fs_read_text_file", "expected_hash"]);
    assert_eq!(unmentioned_tools(&catalogue, &spans), vec!["fs_write_file".to_owned()]);
}

#[test]
fn a_fully_covered_catalogue_reports_nothing_unmentioned() {
    let catalogue = set(&["fs_read_text_file", "fs_write_file"]);
    assert!(
        unmentioned_tools(&catalogue, &catalogue).is_empty(),
        "`unmentioned_tools(&catalogue, &catalogue)` should be empty, got {:?}",
        unmentioned_tools(&catalogue, &catalogue)
    );
}

#[test]
fn the_optional_family_wildcard_covers_only_that_family() {
    let catalogue = set(&["ephemeris_moon_phase", "ephemeris_solar_events", "fs_write_file"]);
    let spans = set(&["ephemeris_*"]);
    assert_eq!(unmentioned_tools(&catalogue, &spans), vec!["fs_write_file".to_owned()]);
}

#[test]
fn a_wildcard_for_a_required_family_does_not_cover_its_tools() {
    let catalogue = set(&["fs_write_file"]);
    let spans = set(&["fs_*"]);
    assert_eq!(unmentioned_tools(&catalogue, &spans), vec!["fs_write_file".to_owned()]);
}

#[test]
fn a_removed_or_renamed_tool_the_skill_still_names_is_reported() {
    let catalogue = set(&["fs_write_file"]);
    let spans = set(&["fs_write_file", "fs_overwrite_file", "git_initialise"]);
    assert_eq!(
        stale_references(&catalogue, &spans),
        vec!["fs_overwrite_file".to_owned(), "git_initialise".to_owned()]
    );
}

#[test]
fn identifiers_that_are_not_tool_references_are_ignored_as_stale() {
    let catalogue = set(&["fs_write_file"]);
    let spans = set(&[
        "expected_hash",
        "note_type",
        "base_record",
        "resource_link_threshold_kb",
        "FS_UPPER_CASE",
        "fs_write_file()",
        "fs",
    ]);
    assert!(
        stale_references(&catalogue, &spans).is_empty(),
        "`stale_references(&catalogue, &spans)` should be empty, got {:?}",
        stale_references(&catalogue, &spans)
    );
}

#[test]
fn only_code_spans_count_as_mentions_and_only_markdown_files_are_read() -> TestResult {
    let directory = tempfile::Builder::new().prefix("skill").tempdir()?;
    std::fs::write(
        directory.path().join("SKILL.md"),
        "Prose naming fs_read_text_file is ignored, but `fs_write_file` counts.\n\n`git_init` too.\n",
    )?;
    std::fs::write(directory.path().join("notes.txt"), "`vault_info`")?;
    let spans = code_spans_in(directory.path())?;
    assert_eq!(spans, set(&["fs_write_file", "git_init"]));
    Ok(())
}

#[test]
fn a_skill_directory_without_markdown_is_an_error_rather_than_an_empty_pass() -> TestResult {
    let directory = tempfile::Builder::new().prefix("skill").tempdir()?;
    std::fs::write(directory.path().join("notes.txt"), "`vault_info`")?;
    assert!(code_spans_in(directory.path()).is_err());
    Ok(())
}

#[test]
fn every_catalogue_tool_belongs_to_a_known_family() {
    let unknown = catalogue_names()
        .into_iter()
        .filter(|tool| {
            tool.split('_')
                .next()
                .is_none_or(|family| !TOOL_FAMILIES.contains(&family))
        })
        .collect::<Vec<_>>();
    assert!(
        unknown.is_empty(),
        "{unknown:?} start with a word missing from TOOL_FAMILIES, so a stale mention of them in the plugin skill \
         would go unnoticed; add the family"
    );
}

#[test]
#[ignore = "needs the plugin checkout; run through .claude/scripts/check-plugin-skill.sh"]
fn plugin_skill_mentions_every_tool_this_binary_ships() -> TestResult {
    let unmentioned = unmentioned_tools(&catalogue_names(), &configured_skill_spans()?);
    assert!(
        unmentioned.is_empty(),
        "the plugin's mcp-tools skill never mentions {unmentioned:?}; add guidance for each new tool, or for a renamed \
         tool replace the old name"
    );
    Ok(())
}

#[test]
#[ignore = "needs the plugin checkout; run through .claude/scripts/check-plugin-skill.sh"]
fn plugin_skill_names_only_tools_this_binary_ships() -> TestResult {
    let stale = stale_references(&catalogue_names(), &configured_skill_spans()?);
    assert!(
        stale.is_empty(),
        "the plugin's mcp-tools skill names {stale:?}, which this binary does not ship; remove or update the guidance, \
         or add a non-tool identifier to NON_TOOL_IDENTIFIERS"
    );
    Ok(())
}
