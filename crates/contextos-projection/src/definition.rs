use contextos_obsidian::FrontmatterDocument;
use serde_json::{Map, Value};

use crate::{Access, DefinitionRejection, ProjectionError, SkillFolder, Slug, Tier};

/// The file that makes a folder a skill.
const SKILL_FILE: &str = "SKILL.md";

const fn rejected(field: &'static str, reason: DefinitionRejection) -> ProjectionError {
    ProjectionError::InvalidDefinition { field, reason }
}

/// Splits `source` into frontmatter and body. `file` names the field reported when the bytes are not UTF-8.
fn frontmatter_document(file: &'static str, source: &[u8]) -> Result<FrontmatterDocument, ProjectionError> {
    let text = std::str::from_utf8(source).map_err(|_| rejected(file, DefinitionRejection::NotUtf8))?;
    FrontmatterDocument::try_from(text).map_err(|_| rejected("frontmatter", DefinitionRejection::Malformed))
}

/// A frontmatter field that must be text with something in it.
fn required_text<'a>(frontmatter: &'a Map<String, Value>, field: &'static str) -> Result<&'a str, ProjectionError> {
    match frontmatter.get(field) {
        None => Err(rejected(field, DefinitionRejection::Missing)),
        Some(Value::String(text)) if text.trim().is_empty() => Err(rejected(field, DefinitionRejection::Empty)),
        Some(Value::String(text)) => Ok(text),
        Some(_) => Err(rejected(field, DefinitionRejection::WrongType)),
    }
}

/// Host settings a definition must not carry: each host chooses its own model and tool names.
const RESERVED_FIELDS: [&str; 2] = ["model", "tools"];

/// The `access` field; absent means every tool.
fn optional_access(frontmatter: &Map<String, Value>) -> Result<Access, ProjectionError> {
    if frontmatter.contains_key("access") {
        Access::try_from(required_text(frontmatter, "access")?)
    } else {
        Ok(Access::All)
    }
}

/// The body with Unix line endings, without leading blank lines or trailing whitespace; `None` when nothing is left.
/// The first content line keeps its own indentation, because leading spaces can be meaningful Markdown.
fn clean_body(body: &str) -> Option<String> {
    let unix = body.replace("\r\n", "\n");
    let first_content = unix.find(|character: char| !character.is_whitespace())?;
    let line_start = unix[..first_content]
        .rfind('\n')
        .map_or(0, |index| index.saturating_add(1));
    Some(unix[line_start..].trim_end().to_owned())
}

/// An agent definition that satisfies the projection contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentDefinition {
    slug: Slug,
    description: String,
    tier: Tier,
    access: Access,
    body: String,
}

impl AgentDefinition {
    /// The name every host projects the agent under.
    #[must_use]
    pub const fn slug(&self) -> &Slug {
        &self.slug
    }

    /// The one-line summary a host shows and uses to choose the agent.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The model class the agent asks for.
    #[must_use]
    pub const fn tier(&self) -> Tier {
        self.tier
    }

    /// Which tools the agent may use.
    #[must_use]
    pub const fn access(&self) -> &Access {
        &self.access
    }

    /// The agent's prompt.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

impl TryFrom<(&Slug, &[u8])> for AgentDefinition {
    type Error = ProjectionError;

    fn try_from((slug, source): (&Slug, &[u8])) -> Result<Self, Self::Error> {
        let document = frontmatter_document("definition", source)?;
        let frontmatter = document.frontmatter();
        let description = required_text(frontmatter, "description")?.trim().to_owned();
        for reserved in RESERVED_FIELDS {
            if frontmatter.contains_key(reserved) {
                return Err(rejected(reserved, DefinitionRejection::Reserved));
            }
        }
        let tier = Tier::try_from(required_text(frontmatter, "tier")?)?;
        let access = optional_access(frontmatter)?;
        let body = clean_body(document.body()).ok_or_else(|| rejected("body", DefinitionRejection::Empty))?;
        Ok(Self {
            slug: slug.clone(),
            description,
            tier,
            access,
            body,
        })
    }
}

/// A skill definition that satisfies the projection contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillDefinition {
    slug: Slug,
    folder: SkillFolder,
}

impl SkillDefinition {
    /// The folder name every host projects the skill under.
    #[must_use]
    pub const fn slug(&self) -> &Slug {
        &self.slug
    }

    /// The skill's files, projected unchanged.
    #[must_use]
    pub const fn folder(&self) -> &SkillFolder {
        &self.folder
    }
}

impl TryFrom<(Slug, SkillFolder)> for SkillDefinition {
    type Error = ProjectionError;

    fn try_from((slug, folder): (Slug, SkillFolder)) -> Result<Self, Self::Error> {
        let source = folder
            .files()
            .find_map(|(path, bytes)| (<&str>::from(path) == SKILL_FILE).then_some(bytes))
            .ok_or_else(|| rejected(SKILL_FILE, DefinitionRejection::Missing))?;
        let document = frontmatter_document(SKILL_FILE, source)?;
        required_text(document.frontmatter(), "name")?;
        required_text(document.frontmatter(), "description")?;
        Ok(Self { slug, folder })
    }
}

#[cfg(test)]
#[path = "definition_test.rs"]
mod tests;
