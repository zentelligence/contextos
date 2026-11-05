//! Vault-scan orchestration for the `base_query` MCP tool
//! (`tools::obsidian::query_base`): resolves the scan root, enumerates
//! candidate notes, evaluates each against
//! `contextos_obsidian::base_query`'s pure filter evaluator, sorts and
//! limits the matches, and renders the requested representation. Kept
//! separate from `tools/obsidian.rs` so that file's tool handlers stay as
//! thin as its siblings.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use contextos_core::{VaultChildInput, VaultPath, VaultPathInput, VaultRootId, VaultSet};
use contextos_fs::{Filesystem, ReadTextRequest, SearchFilesRequest};
use contextos_obsidian::{
    FileMetadata, FrontmatterDocument, LinkCollection, QueryDefinition, QueryFormat, RowContext, ScanRootHint, SortKey,
    compare_values, evaluate_filters, records_folder, render, resolve_property, resolve_row, scan_root_hint,
    validate_filters,
};
use contextos_search::{SearchError, VaultSearchService};
use serde_json::{Map, Value};

use crate::tool_error::ToolError;

/// The `file.*` property name that, alone among the documented set,
/// structurally requires the link graph (backlinks cannot be derived from
/// a candidate's own content). Referenced by [`references_file_backlinks`].
const FILE_BACKLINKS_PROPERTY: &str = "file.backlinks";

/// Whether `definition` references [`FILE_BACKLINKS_PROPERTY`] anywhere a
/// row could need it: a display column, a sort key, or (via a conservative
/// substring check on the still-unparsed filter tree) a filter. Used to
/// decide, once per call rather than once per candidate, whether the link
/// graph must be resolved up front.
pub(crate) fn references_file_backlinks(definition: &QueryDefinition) -> bool {
    definition
        .columns
        .iter()
        .any(|column| column == FILE_BACKLINKS_PROPERTY)
        || definition
            .sort
            .iter()
            .any(|key| key.property == FILE_BACKLINKS_PROPERTY)
        || definition
            .filters
            .as_ref()
            .is_some_and(|filters| filters.to_string().contains(FILE_BACKLINKS_PROPERTY))
}

/// Safety cap on how many matched, filtered rows one `base_query` call
/// renders after evaluating every candidate under the scanned root.
/// Applied *after* filtering and sorting ([`run`]), never before: an
/// earlier revision capped the raw candidate list itself, in filesystem
/// enumeration order (lexicographic ascending), before either the filter
/// or the caller's own `sort` ran. Enumeration order has no relationship
/// to a query's `sort` key in general, so that earlier cap silently
/// discarded genuinely matching rows — most visibly, an ISO-date-
/// partitioned path convention (`YYYY/MM/...`) sorts oldest-first, so any
/// corpus exceeding the cap had its *newest* entries dropped before a
/// `sort: DESC` request ever saw them, no matter how small `limit` was.
/// Reaching this cap after sorting still sets `truncated` rather than
/// silently rendering only part of the match set without saying so, but
/// the rows actually rendered remain the true globally-sorted top ones.
const MAX_CANDIDATE_FILES: usize = 5_000;

/// Hard ceiling on how many candidate files one `base_query` call's raw
/// scan will enumerate, a last-resort circuit breaker against a single
/// call reading an unbounded number of files, not a normal-operation
/// limit: [`MAX_CANDIDATE_FILES`] is what actually bounds output size and
/// what a caller should expect to hit. Deliberately far above any
/// realistic vault or per-directory run-record volume so it does not
/// reintroduce [`MAX_CANDIDATE_FILES`]'s own former bug of truncating the
/// candidate set before the caller's `sort` ever runs.
const MAX_SCANNED_FILES: usize = 250_000;

const CANDIDATE_PATTERN: &str = "**/*.md";

/// The rendered outcome of one `base_query` call, ready to become
/// [`crate::tools::obsidian_types::BaseQueryToolResult`]. `diagnostics`
/// carries one `format/frontmatter` entry per candidate whose frontmatter
/// failed to parse (the row is still included, with every frontmatter-
/// backed column resolving to `null`, per [`load_row`]'s existing
/// graceful-degradation policy) — surfaced so a caller sees *why* those
/// columns are empty rather than silently getting `null` with no
/// explanation.
pub(crate) struct QueryOutcome {
    pub(crate) content: String,
    pub(crate) columns: Vec<String>,
    pub(crate) matched: usize,
    pub(crate) truncated: bool,
    pub(crate) diagnostics: Vec<crate::tools::diagnostics::StructuredDiagnosticToolResult>,
}

/// One matched row's owned data, kept alive across the scan so it can be
/// sorted and rendered once every candidate file has been evaluated.
struct MatchedRow {
    frontmatter: Map<String, Value>,
    content_hash: String,
    /// The body below the frontmatter, kept only when the scan was asked to keep it.
    body: Option<String>,
    name: String,
    basename: String,
    path: String,
    ext: String,
    folder: String,
    size: u64,
    ctime: Option<String>,
    mtime: Option<String>,
    tags: Vec<String>,
    links: Vec<String>,
    embeds: Vec<String>,
    backlinks: Vec<String>,
}

impl MatchedRow {
    fn row_context(&self) -> RowContext<'_> {
        RowContext {
            frontmatter: &self.frontmatter,
            file: FileMetadata {
                name: &self.name,
                basename: &self.basename,
                path: &self.path,
                ext: &self.ext,
                folder: &self.folder,
                size: self.size,
                ctime: self.ctime.as_deref(),
                mtime: self.mtime.as_deref(),
            },
            tags: &self.tags,
            links: &self.links,
            embeds: &self.embeds,
            backlinks: &self.backlinks,
        }
    }
}

/// Runs one `base_query`: resolves the scan root (narrowing it to a
/// `file.path == "..."` filter hint when present, per
/// [`scan_root_hint`]'s safety contract), enumerates markdown files,
/// evaluates each against `definition.filters`, sorts and limits the
/// matches, and renders `format`. `search` is the calling vault's
/// per-root search services (`ContextOsServer::search`); the link graph is
/// resolved from it, once, only when [`references_file_backlinks`] finds
/// `definition` actually needs it, and only for the same vault `root_id`
/// already resolves to. [`QueryOutcome::diagnostics`] carries one entry
/// per scanned candidate whose frontmatter failed to parse, see
/// [`load_row`].
///
/// # Errors
///
/// Returns a [`ToolError`] for any filesystem failure reading the vault
/// root itself, [`ToolError::SearchDisabled`] when `definition` references
/// `file.backlinks` but this vault has no search service configured, or
/// any [`contextos_obsidian::BaseQueryError`] raised while evaluating a
/// filter, sort key, or display column.
pub(crate) fn run(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root_id: VaultRootId,
    definition: &QueryDefinition,
    format: QueryFormat,
    search: &[Option<Arc<VaultSearchService>>],
) -> Result<QueryOutcome, ToolError> {
    let graph = if references_file_backlinks(definition) {
        Some(
            search
                .get(usize::try_from(root_id)?)
                .cloned()
                .flatten()
                .ok_or(ToolError::SearchDisabled)?,
        )
    } else {
        None
    };
    validate_filters(definition.filters.as_ref())?;
    let Scan {
        rows: mut matched,
        diagnostics,
        truncated: scan_truncated,
    } = scan(
        filesystem,
        roots,
        root_id,
        definition.filters.as_ref(),
        graph.as_deref(),
        false,
    )?;

    sort_rows(&definition.sort, &mut matched)?;
    let matched_count = matched.len();
    let truncated = scan_truncated || matched_count > MAX_CANDIDATE_FILES;
    matched.truncate(MAX_CANDIDATE_FILES);
    let limited = match definition.limit {
        Some(limit) => matched.into_iter().take(limit).collect::<Vec<_>>(),
        None => matched,
    };

    let mut rows = Vec::with_capacity(limited.len());
    for row in &limited {
        rows.push(resolve_row(
            &definition.columns,
            &row.row_context(),
            &definition.formulas,
        )?);
    }
    let content = render(&definition.columns, &rows, format);

    Ok(QueryOutcome {
        content,
        columns: definition.columns.clone(),
        matched: matched_count,
        truncated,
        diagnostics,
    })
}

/// Every note under the scanned root that `filters` matches, with the per-note diagnostics gathered while reading
/// them, and whether the scan reached [`MAX_SCANNED_FILES`] before enumerating everything.
struct Scan {
    rows: Vec<MatchedRow>,
    diagnostics: Vec<crate::tools::diagnostics::StructuredDiagnosticToolResult>,
    truncated: bool,
}

/// Enumerates markdown notes under the vault root, narrowed to a directory when [`scan_root_hint`] finds a safe one
/// in `filters`, and keeps those `filters` matches. `keep_body` retains each matched note's body for a caller that
/// returns it.
fn scan(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root_id: VaultRootId,
    filters: Option<&Value>,
    graph: Option<&VaultSearchService>,
    keep_body: bool,
) -> Result<Scan, ToolError> {
    let vault_root = join_relative(roots, root_id, "")?;
    let narrowing = scan_root_hint(filters).and_then(|hint| {
        let directory = match hint {
            ScanRootHint::Path(value) => candidate_directory(value),
            ScanRootHint::Folder(value) | ScanRootHint::InFolder(value) => (!value.is_empty()).then_some(value),
        }?;
        let narrowed_root = join_relative(roots, root_id, directory).ok()?;
        Some((directory.to_owned(), narrowed_root))
    });

    // A hinted directory that does not exist, is not a directory, or is otherwise unusable falls back to the
    // unnarrowed vault root: `scan_root_hint` is documented as optimisation-only, so a failed narrowing attempt must
    // never surface as a `base_query` error, only as a slower scan.
    let narrowed = narrowing.and_then(|(directory, narrowed_root)| {
        search_candidates(filesystem, &narrowed_root)
            .ok()
            .map(|candidates| (format!("{directory}/"), narrowed_root, candidates))
    });
    let (prefix, scan_root, candidates) = if let Some(narrowed) = narrowed {
        narrowed
    } else {
        let candidates = search_candidates(filesystem, &vault_root)?;
        (String::new(), vault_root, candidates)
    };
    let mut paths = ScanPaths {
        roots,
        scan_root,
        directories: HashMap::new(),
    };

    // `candidates` is bounded only by `MAX_SCANNED_FILES` (search_candidates'
    // own `max_results`), a DoS/memory safety valve far above any realistic
    // vault, never by `MAX_CANDIDATE_FILES`: every candidate the scan itself
    // found is filtered and sorted before any correctness-relevant cap
    // applies, so `sort`/`limit` are always evaluated against the complete
    // match set (see `MAX_CANDIDATE_FILES`'s own doc comment for why this
    // ordering matters).
    let truncated = candidates.len() > MAX_SCANNED_FILES;
    let candidates = &candidates[..candidates.len().min(MAX_SCANNED_FILES)];

    let mut rows = Vec::new();
    let mut diagnostics = Vec::new();
    for candidate in candidates {
        let relative = format!("{prefix}{candidate}");
        let path = paths.entry(candidate)?;
        let Some(row) = load_row(filesystem, &path, &relative, graph, keep_body, &mut diagnostics)? else {
            continue;
        };
        if evaluate_filters(filters, &row.row_context())? {
            rows.push(row);
        }
    }
    Ok(Scan {
        rows,
        diagnostics,
        truncated,
    })
}

/// The notes among which a base's record ids must be unique, as a filter: the folder its top-level filters name
/// (`file.inFolder("...")`, or `file.folder == "..."` for the notes directly in it), leaving out each folder's managed
/// `index` note, whatever else the filters say. A condition such as `status != "closed"` hides a record from the
/// base's views but never frees its id. Filters that name no folder define the base by its notes' properties, so the
/// notes they select are its records; filters that need the link graph (`file.backlinks`) to say which, and name no
/// folder, leave the whole vault (`None`), the one scope that cannot miss a record.
pub(crate) fn identity_scope(filters: Option<&Value>) -> Option<Value> {
    if let Some(folder) = records_folder(filters) {
        let quote = if folder.path.contains('"') { '\'' } else { '"' };
        let clause = if folder.includes_subfolders {
            format!("file.inFolder({quote}{}{quote})", folder.path)
        } else {
            format!("file.folder == {quote}{}{quote}", folder.path)
        };
        return Some(Value::String(format!("{clause} && file.basename != \"index\"")));
    }
    if filters.is_some_and(|filters| filters.to_string().contains(FILE_BACKLINKS_PROPERTY)) {
        return None;
    }
    filters.cloned()
}

/// One note a base's own top-level filters select: its vault-relative path and frontmatter.
pub(crate) struct ScopeRecord {
    pub(crate) path: String,
    pub(crate) frontmatter: Map<String, Value>,
}

/// Every note `scope` (a base's own top-level filters) selects, evaluated by the same scan and filter engine as a
/// view query and the id lookup, so "the records of a base" means one thing wherever it is asked: in a query, in a
/// write-time uniqueness check, and in the doctor.
///
/// # Errors
///
/// Returns [`ToolError::SearchDisabled`] when `scope` names `file.backlinks`, which no graph-free scan can evaluate
/// truthfully, [`ToolError::Invalid`] when the vault holds more notes than one scan reads (the records could then be
/// incomplete), or any filesystem or [`contextos_obsidian::BaseQueryError`] raised by the scan, with `scope` checked
/// against the filter grammar before any note is read.
pub(crate) fn scope_records(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root_id: VaultRootId,
    scope: Option<&Value>,
) -> Result<Vec<ScopeRecord>, ToolError> {
    if scope.is_some_and(|filters| filters.to_string().contains(FILE_BACKLINKS_PROPERTY)) {
        return Err(ToolError::SearchDisabled);
    }
    validate_filters(scope)?;
    let Scan { rows, truncated, .. } = scan(filesystem, roots, root_id, scope, None, false)?;
    if truncated {
        return Err(ToolError::Invalid(
            "the vault holds more notes than one base scan reads, so the base's records cannot be listed in full",
        ));
    }
    Ok(rows
        .into_iter()
        .map(|row| ScopeRecord {
            path: row.path,
            frontmatter: row.frontmatter,
        })
        .collect())
}

/// The most ids one lookup accepts.
const MAX_LOOKUP_IDS: usize = 100;

/// The ids one `base_query` lookup asks for: between one and [`MAX_LOOKUP_IDS`], each non-empty, none repeated, in
/// the order the caller gave them.
pub(crate) struct RecordIds(Vec<String>);

impl TryFrom<Vec<String>> for RecordIds {
    type Error = ToolError;

    fn try_from(ids: Vec<String>) -> Result<Self, Self::Error> {
        if ids.is_empty() {
            return Err(ToolError::Invalid("base_query ids must name at least one id"));
        }
        if ids.len() > MAX_LOOKUP_IDS {
            return Err(ToolError::Invalid("base_query ids accepts at most 100 ids in one call"));
        }
        if ids.iter().any(|id| id.trim().is_empty()) {
            return Err(ToolError::Invalid("base_query ids must not contain an empty id"));
        }
        let mut seen = std::collections::HashSet::new();
        if !ids.iter().all(|id| seen.insert(id.as_str())) {
            return Err(ToolError::Invalid("base_query ids must not repeat an id"));
        }
        Ok(Self(ids))
    }
}

/// One record a lookup found.
pub(crate) struct FoundRecord {
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) content_hash: String,
    pub(crate) frontmatter: Map<String, Value>,
    pub(crate) content: Option<String>,
}

/// The outcome of one lookup: the records found in the order their ids were asked for, the ids no record holds, the
/// diagnostics gathered (unparseable notes and ids held by more than one record), and whether the scan stopped short.
pub(crate) struct LookupOutcome {
    pub(crate) records: Vec<FoundRecord>,
    pub(crate) not_found: Vec<String>,
    pub(crate) diagnostics: Vec<crate::tools::diagnostics::StructuredDiagnosticToolResult>,
    pub(crate) truncated: bool,
}

/// Looks up records by their frontmatter `id` among the notes `scope` (a base's top-level filters) selects, so one
/// call replaces a query and a read per record.
///
/// # Errors
///
/// Returns a [`ToolError`] for a filesystem failure reading the vault root, or any
/// [`contextos_obsidian::BaseQueryError`] raised by `scope`, which is checked against the filter grammar before any
/// note is read.
pub(crate) fn lookup(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root_id: VaultRootId,
    scope: Option<&Value>,
    ids: &RecordIds,
    include_content: bool,
) -> Result<LookupOutcome, ToolError> {
    validate_filters(scope)?;
    let Scan {
        rows,
        mut diagnostics,
        truncated,
    } = scan(filesystem, roots, root_id, scope, None, include_content)?;
    let mut by_id: std::collections::HashMap<&str, Vec<MatchedRow>> =
        ids.0.iter().map(|id| (id.as_str(), Vec::new())).collect();
    for row in rows {
        let id = row.frontmatter.get("id").and_then(Value::as_str).map(str::to_owned);
        if let Some(matches) = id.as_deref().and_then(|id| by_id.get_mut(id)) {
            matches.push(row);
        }
    }
    let mut records = Vec::new();
    let mut not_found = Vec::new();
    for id in &ids.0 {
        let matches = by_id.remove(id.as_str()).unwrap_or_default();
        if matches.is_empty() {
            not_found.push(id.clone());
            continue;
        }
        if matches.len() > 1 {
            let paths: Vec<&str> = matches.iter().map(|row| row.path.as_str()).collect();
            diagnostics.push(crate::tools::diagnostics::StructuredDiagnosticToolResult {
                code: "query/base-query-duplicate-id".to_owned(),
                path: paths.join(", "),
                message: format!(
                    "{id} is held by {} records in this base's scope: {}",
                    matches.len(),
                    paths.join(", ")
                ),
            });
        }
        records.extend(matches.into_iter().map(|row| FoundRecord {
            id: id.clone(),
            path: row.path,
            content_hash: row.content_hash,
            frontmatter: row.frontmatter,
            content: row.body,
        }));
    }
    Ok(LookupOutcome {
        records,
        not_found,
        diagnostics,
        truncated,
    })
}

/// Answers a `base_query` call that asked for `ids`: checks the request carries `path` and nothing that only applies
/// to a view, reads the base's top-level filters as the lookup's scope, and looks the records up.
///
/// # Errors
///
/// Returns [`ToolError::Invalid`] for a request combining `ids` with `definition`, `view`, `limit`, or `format`, for
/// `include_content` without `ids`, or for ids [`RecordIds`] refuses; otherwise any error reading or parsing the base,
/// or from [`lookup`].
pub(crate) fn look_up_records(
    filesystem: &Filesystem,
    roots: &VaultSet,
    input: crate::tools::obsidian_types::BaseQueryToolInput,
) -> Result<crate::tools::obsidian_types::BaseQueryToolResult, ToolError> {
    let Some(ids) = input.ids else {
        return Err(ToolError::Invalid(
            "base_query include_content applies only to an ids lookup",
        ));
    };
    if input.definition.is_some() {
        return Err(ToolError::Invalid(
            "base_query ids look records up within an existing base, so they require path, not definition",
        ));
    }
    if input.view.is_some() || input.limit.is_some() || input.format.is_some() {
        return Err(ToolError::Invalid(
            "base_query ids are not accepted with view, limit, or format, which apply only to a view query",
        ));
    }
    let Some(raw) = input.path.as_deref() else {
        return Err(ToolError::Invalid(
            "base_query ids require path, the base to look records up in",
        ));
    };
    let ids = RecordIds::try_from(ids)?;
    let crate::tools::obsidian_types::BaseVaultPath(path) =
        crate::tools::obsidian_types::BaseVaultPath::try_from(crate::tools::obsidian_types::StructuredPathInput {
            roots,
            raw,
        })?;
    let source = filesystem.read_text(&ReadTextRequest {
        path: path.clone(),
        limit: None,
    })?;
    let document = contextos_obsidian::BaseDocument::try_from(source.content.as_str())?;
    let outcome = lookup(
        filesystem,
        roots,
        path.root_id(),
        document.definition().get("filters"),
        &ids,
        input.include_content.unwrap_or(false),
    )?;
    let content = std::iter::once("| id | path |\n| --- | --- |\n".to_owned())
        .chain(outcome.records.iter().map(|record| {
            format!(
                "| {} | {} |\n",
                record.id.replace('|', "\\|"),
                record.path.replace('|', "\\|")
            )
        }))
        .collect::<String>();
    let diagnostics = document
        .diagnostics()
        .into_iter()
        .map(crate::tools::diagnostics::StructuredDiagnosticToolResult::from)
        .chain(outcome.diagnostics)
        .collect();
    Ok(crate::tools::obsidian_types::BaseQueryToolResult {
        content,
        columns: Vec::new(),
        matched: outcome.records.len(),
        truncated: outcome.truncated,
        diagnostics,
        records: Some(outcome.records.into_iter().map(Into::into).collect()),
        not_found: Some(outcome.not_found),
    })
}

/// Resolves `suffix` (vault-root-relative, forward-slash separated; empty
/// for the vault root itself) as a [`VaultPath`], via the same "absolute
/// root path, then re-resolve" approach `links_read` already uses
/// (`tools/obsidian.rs::read_links`) to obtain an unambiguous whole-vault
/// `VaultPath` from a root id alone.
fn join_relative(roots: &VaultSet, root_id: VaultRootId, suffix: &str) -> Result<VaultPath, ToolError> {
    let root = roots
        .iter()
        .nth(usize::try_from(root_id)?)
        .ok_or(ToolError::Invalid("configured root not found"))?;
    let absolute = if suffix.is_empty() {
        root.path().to_path_buf()
    } else {
        root.path().join(suffix)
    };
    let raw = absolute
        .to_str()
        .ok_or(ToolError::Invalid("vault path must be valid UTF-8"))?;
    Ok(VaultPath::try_from(VaultPathInput { roots, raw })?)
}

/// The paths of one scan's candidates, built from the directory the scan walked: each directory on the way to a
/// candidate is validated once, and each candidate then costs one inspection of its own entry, rather than every
/// candidate resolving its whole ancestry from the vault root again.
struct ScanPaths<'a> {
    roots: &'a VaultSet,
    scan_root: VaultPath,
    /// Directories already validated, by their path relative to `scan_root`.
    directories: HashMap<PathBuf, VaultPath>,
}

impl ScanPaths<'_> {
    /// The validated path of `candidate`, a path relative to the scan root as the walk reported it.
    fn entry(&mut self, candidate: &str) -> Result<VaultPath, ToolError> {
        let candidate = Path::new(candidate);
        let mut directory = self.scan_root.clone();
        let mut walked = PathBuf::new();
        let mut names = candidate.components().peekable();
        while let Some(component) = names.next() {
            let Component::Normal(name) = component else {
                return Err(ToolError::Invalid("a scanned path is not a plain relative path"));
            };
            let name = name
                .to_str()
                .ok_or(ToolError::Invalid("vault path must be valid UTF-8"))?;
            if names.peek().is_none() {
                return Ok(VaultPath::try_from(VaultChildInput {
                    roots: self.roots,
                    parent: &directory,
                    name,
                })?);
            }
            walked.push(name);
            directory = if let Some(known) = self.directories.get(&walked) {
                known.clone()
            } else {
                let resolved = VaultPath::try_from(VaultChildInput {
                    roots: self.roots,
                    parent: &directory,
                    name,
                })?;
                self.directories.insert(walked.clone(), resolved.clone());
                resolved
            };
        }
        Err(ToolError::Invalid("a scanned path is empty"))
    }
}

fn search_candidates(filesystem: &Filesystem, root: &VaultPath) -> Result<Vec<String>, ToolError> {
    Ok(filesystem.search_files(&SearchFilesRequest {
        path: root.clone(),
        pattern: CANDIDATE_PATTERN.to_owned(),
        exclude_patterns: vec![".git/**".to_owned(), ".contextos/**".to_owned()],
        max_results: MAX_SCANNED_FILES.saturating_add(1),
    })?)
}

/// Returns the parent directory of a [`ScanRootHint::Path`] value, or
/// `None` when the hint has no `/` to derive a safe subdirectory from (a
/// bare filename hint is left unnarrowed rather than guessed at).
/// [`ScanRootHint::Folder`] values are already a directory and are used
/// directly by the caller instead.
fn candidate_directory(hint: &str) -> Option<&str> {
    if let Some(stripped) = hint.strip_suffix('/') {
        return (!stripped.is_empty()).then_some(stripped);
    }
    hint.rsplit_once('/')
        .map(|(directory, _)| directory)
        .filter(|directory| !directory.is_empty())
}

/// Reads one candidate file and builds its [`MatchedRow`], or `None` when
/// the file cannot be read (permissions, size limits, or a concurrent
/// deletion mid-scan): a single unreadable candidate is skipped rather
/// than failing the whole query, the same graceful-degradation discipline
/// `contextos_search::IndexedDocument` already applies to indexing.
/// Frontmatter that fails to parse degrades to an empty frontmatter map
/// with the whole file treated as body, appending a `format/frontmatter`
/// entry to `diagnostics` naming the candidate — every frontmatter-backed
/// column resolves to `null` for this row, and any filter referencing a
/// frontmatter property silently cannot match it, so the caller needs to
/// know why rather than read an empty result as "genuinely no matches".
/// Malformed wikilink syntax degrades `links`/`embeds` to empty lists
/// rather than failing the row (no diagnostic: less consequential, and
/// `links_read` already reports malformed-link errors directly for a
/// caller who cares); a candidate not yet present in the link graph
/// degrades `backlinks` to an empty list for the same reason, since
/// `base_query` runs a fresh filesystem scan independent of the graph's
/// own index state. Any other graph failure (the graph specifically
/// disabled for this vault, distinct from the whole-vault
/// [`ToolError::SearchDisabled`] gate `run` already applies before
/// scanning) fails the whole query rather than silently reporting empty
/// backlinks as a real answer.
fn load_row(
    filesystem: &Filesystem,
    path: &VaultPath,
    relative: &str,
    graph: Option<&VaultSearchService>,
    keep_body: bool,
    diagnostics: &mut Vec<crate::tools::diagnostics::StructuredDiagnosticToolResult>,
) -> Result<Option<MatchedRow>, ToolError> {
    let Ok(source) = filesystem.read_text(&ReadTextRequest {
        path: path.clone(),
        limit: None,
    }) else {
        return Ok(None);
    };
    let (frontmatter, body) = match FrontmatterDocument::try_from(source.content.as_str()) {
        Ok(parsed) => (parsed.frontmatter().clone(), parsed.body().to_owned()),
        Err(error) => {
            diagnostics.push(crate::tools::diagnostics::StructuredDiagnosticToolResult {
                code: error.code().to_owned(),
                path: relative.to_owned(),
                message: error.to_string(),
            });
            (Map::new(), source.content)
        }
    };
    let tags = contextos_core::extract_tags(&frontmatter, &body);
    let file_path = std::path::Path::new(relative);
    let name = file_path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_owned();
    let basename = file_path
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_owned();
    let ext = file_path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_owned();
    let folder = file_path
        .parent()
        .and_then(std::path::Path::to_str)
        .unwrap_or_default()
        .to_owned();
    let (links, embeds) = match LinkCollection::try_from(body.as_str()) {
        Ok(collection) => {
            let mut links = Vec::new();
            let mut embeds = Vec::new();
            for link in collection.outgoing() {
                if link.embed() {
                    embeds.push(link.target.clone());
                } else {
                    links.push(link.target.clone());
                }
            }
            (links, embeds)
        }
        Err(_) => (Vec::new(), Vec::new()),
    };
    let backlinks = match graph {
        Some(service) => match service.graph_backlinks(relative) {
            Ok(view) => {
                let mut paths: Vec<String> = view.edges.into_iter().map(|edge| edge.from).collect();
                paths.sort();
                paths.dedup();
                paths
            }
            // A candidate the fresh filesystem scan just found but the
            // graph's own index has not yet caught up to degrades to an
            // empty backlink list, the same tolerance this function already
            // gives frontmatter-parse and wikilink-parse failures. Any
            // other failure (graph disabled for this vault specifically,
            // even though some other search feature is on) is a whole-vault
            // capability gap, not a per-candidate one: propagate it rather
            // than silently reporting "no backlinks" as if it were a real
            // answer.
            Err(SearchError::UnknownNote { .. }) => Vec::new(),
            Err(error) => return Err(ToolError::from(error)),
        },
        None => Vec::new(),
    };
    let content_hash: &str = (&source.content_hash).into();
    Ok(Some(MatchedRow {
        frontmatter,
        content_hash: content_hash.to_owned(),
        body: keep_body.then_some(body),
        name,
        basename,
        path: relative.to_owned(),
        ext,
        folder,
        size: source.size,
        ctime: source.created.map(|value| value.to_string()),
        mtime: source.modified.map(|value| value.to_string()),
        tags,
        links,
        embeds,
        backlinks,
    }))
}

/// Stable multi-key sort applying each [`SortKey`] in reverse, the
/// standard technique for building a multi-key ordering from repeated
/// single-key stable sorts.
fn sort_rows(sort: &[SortKey], rows: &mut [MatchedRow]) -> Result<(), ToolError> {
    for key in sort.iter().rev() {
        let mut failure = None;
        rows.sort_by(|a, b| {
            if failure.is_some() {
                return Ordering::Equal;
            }
            let value_a = resolve_property(&key.property, &a.row_context(), "sort");
            let value_b = resolve_property(&key.property, &b.row_context(), "sort");
            match (value_a, value_b) {
                (Ok(value_a), Ok(value_b)) => {
                    let ordering = compare_values(value_a.as_ref(), value_b.as_ref());
                    if key.descending { ordering.reverse() } else { ordering }
                }
                (Err(error), _) | (_, Err(error)) => {
                    failure = Some(error);
                    Ordering::Equal
                }
            }
        });
        if let Some(error) = failure {
            return Err(ToolError::from(error));
        }
    }
    Ok(())
}
