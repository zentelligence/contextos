//! Completion and repair of a managed `index.md`'s own frontmatter.
//!
//! Existing frontmatter is edited line by line, never re-serialised, so an operator's comments, quoting, key order,
//! and line endings survive and a rebuild that changes nothing is byte-identical. Only three things are ever
//! touched: keys the file lacks are appended, `updated` is restamped when the file's content really changed, and
//! (only when repairing) the three values an earlier defective rebuild wrote wrongly.

use std::ops::Range;
use std::path::PathBuf;

use contextos_core::frontmatter_timestamp;
use contextos_obsidian::{DefaultFrontmatter, DefaultFrontmatterInput, FrontmatterDocument, FrontmatterInput};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;

use crate::error::IndexError;

/// The `status` a defective rebuild stamped on every managed index; removed when repairing.
const STAMPED_STATUS: &str = "new";

/// Whether `updated` is left as found or set to the rebuild timestamp.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdatedStamp {
    /// Leave an existing `updated` untouched; an absent one is still added, as any missing key is.
    Preserve,
    /// The file's content changes in this rebuild, so `updated` becomes the rebuild timestamp.
    Stamp,
}

/// Values and policy for completing one managed `index.md`'s frontmatter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexFrontmatterInput<'a> {
    /// The `entity` derived from the directory's location; `None` when the vault's entities are not known, in
    /// which case no `entity` is written (and none is repaired), since a guess would be wrong more often than absent.
    pub entity: Option<&'a str>,
    /// The rebuild's own timestamp: `updated` when stamping, and `created` for a file no other source dates.
    pub timestamp: &'a str,
    /// A creation date reported by a source outside the file (version history, filesystem birth time).
    pub created: Option<OffsetDateTime>,
    pub updated: UpdatedStamp,
    /// Also correct the values a defective earlier rebuild wrote: see [`IndexRepairAction`].
    pub repair: bool,
}

/// One value corrected while repairing a managed index's frontmatter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IndexRepairAction {
    /// `entity` disagreed with the directory's location.
    EntityRederived { from: String, to: String },
    /// A `status: new` stamped by the defective rebuild was removed.
    StatusRemoved,
    /// `created` only repeated `updated` (the rebuild's own stamp) and a real source reports an earlier date.
    CreatedReset { from: String, to: String },
}

/// Whether reconciling `source` could use a creation date from outside the file: it has no `created` to keep, or
/// (when repairing) its `created` merely repeats `updated`. Malformed frontmatter reports `false`, as
/// reconciliation itself will reject it with the real error.
pub(crate) fn needs_creation_date(source: &str, repair: bool) -> bool {
    let Ok(document) = FrontmatterDocument::try_from(source) else {
        return false;
    };
    let properties = document.frontmatter();
    match string_property(properties, "created") {
        None => !properties.contains_key("created"),
        Some(created) => repair && string_property(properties, "updated").is_some_and(|updated| updated == created),
    }
}

impl IndexRepairAction {
    /// The frontmatter property this action corrected: `entity`, `status`, or `created`.
    #[must_use]
    pub const fn property(&self) -> &'static str {
        match self {
            Self::EntityRederived { .. } => "entity",
            Self::StatusRemoved => "status",
            Self::CreatedReset { .. } => "created",
        }
    }

    /// The value found before the correction; `None` for a removed `status`, which is always `new`.
    #[must_use]
    pub fn from_value(&self) -> Option<&str> {
        match self {
            Self::EntityRederived { from, .. } | Self::CreatedReset { from, .. } => Some(from),
            Self::StatusRemoved => Some(STAMPED_STATUS),
        }
    }

    /// The value written by the correction; `None` when the property was removed.
    #[must_use]
    pub fn to_value(&self) -> Option<&str> {
        match self {
            Self::EntityRederived { to, .. } | Self::CreatedReset { to, .. } => Some(to),
            Self::StatusRemoved => None,
        }
    }
}

pub(crate) struct FrontmatterReconcileInput<'a> {
    /// The frontmatter block as found, opening delimiter through closing delimiter; `None` when the file has none.
    pub prefix: Option<&'a str>,
    pub title: &'a str,
    /// The index's own vault-relative path, for error reporting only.
    pub path: &'a str,
    pub settings: &'a IndexFrontmatterInput<'a>,
}

pub(crate) struct FrontmatterReconciliation {
    pub block: String,
    pub repairs: Vec<IndexRepairAction>,
}

impl TryFrom<FrontmatterReconcileInput<'_>> for FrontmatterReconciliation {
    type Error = IndexError;

    fn try_from(value: FrontmatterReconcileInput<'_>) -> Result<Self, Self::Error> {
        let path = || PathBuf::from(value.path);
        let defaults = default_properties(value.title, value.settings)?;
        let Some(prefix) = value.prefix else {
            let mut block =
                render_block(&defaults).map_err(|source| IndexError::DirectoryFrontmatter { path: path(), source })?;
            block.push('\n');
            return Ok(Self {
                block,
                repairs: Vec::new(),
            });
        };
        let existing = FrontmatterDocument::try_from(prefix)
            .map_err(|source| IndexError::DirectoryFrontmatter { path: path(), source })?;
        let mut editor = BlockEditor::try_from(prefix).map_err(|Uneditable| IndexError::FrontmatterLayout {
            path: path(),
            key: "frontmatter delimiters".to_owned(),
        })?;
        let mut expected = existing.frontmatter().clone();
        let mut repairs = Vec::new();
        let layout = |key: &str| IndexError::FrontmatterLayout {
            path: path(),
            key: key.to_owned(),
        };

        if value.settings.repair {
            repair_entity(&mut editor, &mut expected, &mut repairs, value.settings).map_err(layout)?;
            repair_status(&mut editor, &mut expected, &mut repairs).map_err(layout)?;
            repair_created(&mut editor, &mut expected, &mut repairs, value.settings).map_err(layout)?;
        }
        let had_updated = expected.contains_key("updated");
        for (key, property) in &defaults {
            if !expected.contains_key(key) {
                editor.append(key, property).map_err(|Uneditable| layout(key))?;
                expected.insert(key.clone(), property.clone());
            }
        }
        if value.settings.updated == UpdatedStamp::Stamp && had_updated {
            let stamp = Value::String(value.settings.timestamp.to_owned());
            editor
                .replace("updated", &stamp)
                .map_err(|Uneditable| layout("updated"))?;
            expected.insert("updated".to_owned(), stamp);
        }

        let block = String::from(editor);
        // A line edit that produced anything other than exactly the intended properties (flow-style or otherwise
        // unusual layouts) is rejected, never left as a silently different document.
        let reparsed = FrontmatterDocument::try_from(block.as_str())
            .ok()
            .filter(|document| document.frontmatter() == &expected);
        if reparsed.is_none() {
            return Err(layout("frontmatter layout"));
        }
        Ok(Self { block, repairs })
    }
}

fn default_properties(title: &str, settings: &IndexFrontmatterInput<'_>) -> Result<Map<String, Value>, IndexError> {
    let mut properties = Map::<String, Value>::from(DefaultFrontmatter::try_from(DefaultFrontmatterInput {
        kind: "index",
        title,
        timestamp: settings.timestamp,
        entity: settings.entity,
        supplied: Map::new(),
    })?);
    // A rebuild is not a workflow state: no status is written, so none can churn.
    properties.shift_remove("status");
    if let Some(created) = settings.created {
        properties.insert("created".to_owned(), Value::String(frontmatter_timestamp(created)));
    }
    Ok(properties)
}

fn repair_entity(
    editor: &mut BlockEditor,
    expected: &mut Map<String, Value>,
    repairs: &mut Vec<IndexRepairAction>,
    settings: &IndexFrontmatterInput<'_>,
) -> Result<(), &'static str> {
    let (Some(wanted), Some(current)) = (settings.entity, string_property(expected, "entity")) else {
        return Ok(());
    };
    if current == wanted {
        return Ok(());
    }
    let replacement = Value::String(wanted.to_owned());
    editor.replace("entity", &replacement).map_err(|Uneditable| "entity")?;
    expected.insert("entity".to_owned(), replacement);
    repairs.push(IndexRepairAction::EntityRederived {
        from: current,
        to: wanted.to_owned(),
    });
    Ok(())
}

fn repair_status(
    editor: &mut BlockEditor,
    expected: &mut Map<String, Value>,
    repairs: &mut Vec<IndexRepairAction>,
) -> Result<(), &'static str> {
    if string_property(expected, "status").as_deref() != Some(STAMPED_STATUS) {
        return Ok(());
    }
    editor.remove("status").map_err(|Uneditable| "status")?;
    expected.shift_remove("status");
    repairs.push(IndexRepairAction::StatusRemoved);
    Ok(())
}

fn repair_created(
    editor: &mut BlockEditor,
    expected: &mut Map<String, Value>,
    repairs: &mut Vec<IndexRepairAction>,
    settings: &IndexFrontmatterInput<'_>,
) -> Result<(), &'static str> {
    let (Some(source), Some(created), Some(updated)) = (
        settings.created,
        string_property(expected, "created"),
        string_property(expected, "updated"),
    ) else {
        return Ok(());
    };
    // Only a `created` that merely repeats `updated` is suspect, and only an earlier reported date can correct it:
    // a source that reports a later one (filesystem birth time after a copy or an atomic rewrite, say) would replace
    // one wrong value with another. A `created` that is not a readable date is left for its owner.
    let Ok(stored) = StoredTimestamp::try_from(created.as_str()) else {
        return Ok(());
    };
    if created != updated || source >= OffsetDateTime::from(stored) {
        return Ok(());
    }
    let replacement = Value::String(frontmatter_timestamp(source));
    editor
        .replace("created", &replacement)
        .map_err(|Uneditable| "created")?;
    expected.insert("created".to_owned(), replacement);
    repairs.push(IndexRepairAction::CreatedReset {
        from: created,
        to: frontmatter_timestamp(source),
    });
    Ok(())
}

/// A timestamp as this crate and its callers write it into frontmatter: ISO 8601 (`2026-09-28T09:00:00+10:00`) or
/// the `OffsetDateTime` display form (`2026-09-28 9:00:00.0 +10:00:00`) that earlier rebuilds wrote.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StoredTimestamp(OffsetDateTime);

impl TryFrom<&str> for StoredTimestamp {
    type Error = time::error::Parse;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let display_unpadded = format_description!(
            "[year]-[month]-[day] [hour padding:none]:[minute]:[second].[subsecond] \
             [offset_hour sign:mandatory]:[offset_minute]:[offset_second]"
        );
        let display_padded = format_description!(
            "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond] \
             [offset_hour sign:mandatory]:[offset_minute]:[offset_second]"
        );
        OffsetDateTime::parse(value, &Rfc3339)
            .or_else(|_| OffsetDateTime::parse(value, &display_unpadded))
            .or_else(|_| OffsetDateTime::parse(value, &display_padded))
            .map(Self)
    }
}

impl From<StoredTimestamp> for OffsetDateTime {
    fn from(value: StoredTimestamp) -> Self {
        value.0
    }
}

fn string_property(properties: &Map<String, Value>, key: &str) -> Option<String> {
    properties.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Renders `properties` as a complete `---` delimited block.
fn render_block(properties: &Map<String, Value>) -> Result<String, contextos_obsidian::FrontmatterError> {
    String::try_from(&FrontmatterDocument::from(FrontmatterInput {
        frontmatter: properties.clone(),
        body: String::new(),
    }))
}

/// The lines of one key's `key: value` rendering, each ending in `eol`.
fn render_lines(key: &str, value: &Value, eol: &str) -> Result<Vec<String>, Uneditable> {
    let mut single = Map::new();
    single.insert(key.to_owned(), value.clone());
    let block = render_block(&single).map_err(|_| Uneditable)?;
    let mut lines = block.lines();
    lines.next();
    let body = lines.collect::<Vec<_>>();
    let (_closing, properties) = body.split_last().ok_or(Uneditable)?;
    Ok(properties.iter().map(|line| format!("{line}{eol}")).collect())
}

/// A block that cannot be edited line by line: its delimiters are absent, or a key is not laid out as `key: value`
/// lines the editor can address.
#[derive(Clone, Copy, Debug)]
struct Uneditable;

/// A frontmatter block held as its own lines (each with its original terminator) so an edit changes only the lines
/// it names.
struct BlockEditor {
    opening: String,
    yaml: Vec<String>,
    closing_and_after: String,
    eol: &'static str,
}

impl TryFrom<&str> for BlockEditor {
    type Error = Uneditable;

    fn try_from(prefix: &str) -> Result<Self, Self::Error> {
        let mut segments = prefix.split_inclusive('\n');
        let opening = segments.next().ok_or(Uneditable)?.to_owned();
        let eol = if opening.ends_with("\r\n") { "\r\n" } else { "\n" };
        let mut yaml = Vec::new();
        let mut consumed = opening.len();
        for segment in segments {
            if segment.trim_end_matches(['\r', '\n']) == "---" {
                return Ok(Self {
                    opening,
                    yaml,
                    closing_and_after: prefix[consumed..].to_owned(),
                    eol,
                });
            }
            consumed = consumed.saturating_add(segment.len());
            yaml.push(segment.to_owned());
        }
        Err(Uneditable)
    }
}

impl From<BlockEditor> for String {
    fn from(value: BlockEditor) -> Self {
        let mut block = value.opening;
        block.extend(value.yaml);
        block.push_str(&value.closing_and_after);
        block
    }
}

impl BlockEditor {
    fn append(&mut self, key: &str, value: &Value) -> Result<(), Uneditable> {
        self.yaml.extend(render_lines(key, value, self.eol)?);
        Ok(())
    }

    fn replace(&mut self, key: &str, value: &Value) -> Result<(), Uneditable> {
        let extent = self.extent_of(key).ok_or(Uneditable)?;
        let lines = render_lines(key, value, self.eol)?;
        self.yaml.splice(extent, lines);
        Ok(())
    }

    fn remove(&mut self, key: &str) -> Result<(), Uneditable> {
        let extent = self.extent_of(key).ok_or(Uneditable)?;
        self.yaml.drain(extent);
        Ok(())
    }

    /// The `key:` line and every line that continues its value: indented lines, and the unindented `- item` lines
    /// this codec itself writes for sequences.
    fn extent_of(&self, key: &str) -> Option<Range<usize>> {
        let start = self.yaml.iter().position(|line| is_key_line(line, key))?;
        let continued = self.yaml[start.saturating_add(1)..]
            .iter()
            .take_while(|line| is_continuation(line))
            .count();
        Some(start..start.saturating_add(1).saturating_add(continued))
    }
}

fn is_key_line(line: &str, key: &str) -> bool {
    line.strip_prefix(key).is_some_and(|rest| rest.starts_with(':'))
}

fn is_continuation(line: &str) -> bool {
    let body = line.trim_end_matches(['\r', '\n']);
    body.starts_with([' ', '\t']) || body == "-" || body.starts_with("- ")
}

#[cfg(test)]
#[path = "frontmatter_test.rs"]
mod tests;
