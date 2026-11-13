//! `note_create`, `note_update`, `frontmatter_read`, `frontmatter_update`, `base_*`,
//! `canvas_*`, and `links_read`: Obsidian-flavoured Markdown, Bases, and
//! JSON Canvas document support layered over the plain filesystem.

use std::path::PathBuf;
use std::sync::Arc;

use contextos_core::{Clock, ContentHash, Origin, VaultPath, VaultPathInput, WriteMutation, frontmatter_timestamp};
use contextos_fs::{FsError, ReadTextRequest};
use contextos_obsidian::{
    BaseDocument, BaseOperation, BaseSidecar, BaseSidecarInput, CanvasCreateInput, CanvasDocument, CanvasOperation,
    FrontmatterDocument, NoteCreateInput, NoteDocument, NoteUpdate, NoteUpdateInput, QueryDefinition, QueryFormat,
};
use contextos_search::IncomingLink;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::tool;

use crate::resource_support::fallible_output_schema_for;
use crate::server::{ContextOsServer, PathInput};
use crate::tool_error::{ToolError, ToolFailure, evaluate, execute};
use crate::tools::diagnostics::StructuredDiagnosticToolResult;
use crate::tools::frontmatter_link_types::{FrontmatterLinkToolInput, FrontmatterLinkToolResult};
use crate::tools::fs_expand::{ExpandedText, NoteExpansion, require_markdown_note};
use crate::tools::links_read::{LinksReadToolResult, LinksReading};
use crate::tools::note_body::{NoteBody, NoteBodyInput};
use crate::tools::note_entity::NoteEntities;
use crate::tools::obsidian_types::{
    BaseApplyToolInput, BaseCreateToolInput, BaseCreateToolResult, BaseQueryToolInput, BaseQueryToolResult,
    BaseReadSource, BaseReadToolResult, BaseRecordCreateToolInput, BaseRecordCreateToolResult, BaseVaultPath,
    CanvasApplyToolInput, CanvasCreateToolInput, CanvasReadSource, CanvasReadToolResult, CanvasVaultPath,
    ExpandedFrontmatter, FrontmatterReadToolInput, FrontmatterReadToolResult, FrontmatterUpdateInput,
    FrontmatterUpdateToolResult, LinkDirectionInput, LinksReadInput, NoteCreateToolInput, NoteCreateToolResult,
    NoteUpdateToolInput, NoteUpdateToolResult, StructuredPathInput, StructuredWriteToolResult,
};

#[rmcp::tool_router(router = obsidian_tool_router, vis = "pub(crate)")]
impl ContextOsServer {
    #[tool(
        name = "note_create",
        description = "Create a validated Obsidian Markdown note with standard frontmatter defaults. Give the body as content, or as content_from: a vault file whose body the server copies in verbatim, so content that only needs copying never passes through you. A Markdown source's frontmatter is left behind (the note's properties come from frontmatter and the defaults); any other text file is copied whole. content_from.expected_hash refuses a source that has changed since you read it (io/conflict naming the source). The result's content_hash is the written note's",
        output_schema = fallible_output_schema_for::<NoteCreateToolResult>()
    )]
    async fn create_note(
        &self,
        Parameters(input): Parameters<NoteCreateToolInput>,
    ) -> Result<Json<NoteCreateToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let NoteCreateToolInput {
            path: raw_path,
            frontmatter,
            content,
            content_from,
            references,
        } = input;
        let (path, frontmatter, title, entity, body, references) = evaluate(move || {
            let path = VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &raw_path,
            })?;
            if path.relative().extension().and_then(std::ffi::OsStr::to_str) != Some("md") {
                return Err(ToolError::Invalid("note path must end in .md"));
            }
            let body = NoteBody::try_from(NoteBodyInput {
                roots: &roots,
                content,
                content_from,
            })?;
            let entity = NoteEntities::read(&filesystem, &roots, &path)?.entity_of(&path);
            let title = frontmatter
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    path.relative()
                        .file_stem()
                        .and_then(std::ffi::OsStr::to_str)
                        .map(|stem| stem.replace(['-', '_'], " "))
                })
                .ok_or(ToolError::Invalid("note path must have a UTF-8 filename"))?;
            // References are judged before the vault is locked, so a malformed one never holds the lock while a
            // copied source is read.
            let references = if references.is_empty() {
                None
            } else {
                let mut section = String::from("\n# References\n");
                for reference in references {
                    if reference.target.trim().is_empty()
                        || reference.target.contains("[[")
                        || reference.target.contains("]]")
                    {
                        return Err(ToolError::Invalid("reference target is invalid"));
                    }
                    section.push_str("\n- [[");
                    section.push_str(reference.target.trim());
                    section.push_str("]]: ");
                    section.push_str(&reference.summary.split_whitespace().collect::<Vec<_>>().join(" "));
                    section.push('\n');
                }
                Some(section)
            };
            Ok((path, frontmatter, title, entity, body, references))
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        let filesystem = Arc::clone(&self.filesystem);
        let clock = self.clock.clone();
        let service = Arc::clone(&self.mutations);
        execute(move || {
            let _guards = guards;
            // A copied source is read under the note's write lock, so within one vault the copy cannot interleave
            // with another write to the source.
            let mut content = body.resolve(&filesystem)?;
            if let Some(references) = references {
                if !content.ends_with('\n') {
                    content.push('\n');
                }
                content.push_str(&references);
            }
            let note = NoteDocument::try_from(NoteCreateInput {
                title: &title,
                frontmatter,
                content: &content,
                timestamp: &frontmatter_timestamp(clock.now()),
                entity: entity.as_deref(),
            })?;
            Ok(NoteCreateToolResult::from(service.write_file(&WriteMutation {
                path,
                content: String::try_from(note)?.into(),
                expected_hash: None,
                force: false,
                origin: Origin::Tool("note_create".to_owned()),
            })?))
        })
        .await
    }

    #[tool(
        name = "note_update",
        description = "Update an existing Markdown note's frontmatter and body in one atomic, conflict-checked write. patch is an RFC 7386 merge patch for the frontmatter (a value sets a field, null removes it) and body replaces everything after the frontmatter's closing --- line; give either or both. expected_hash (from frontmatter_read or a previous write) is required. created is kept and updated is stamped, so the patch may not name either. The title rules note_create applies hold for anything the patch changes, the body is validated as Obsidian Markdown, and it must use the note's own line endings",
        output_schema = fallible_output_schema_for::<NoteUpdateToolResult>()
    )]
    async fn update_note(
        &self,
        Parameters(input): Parameters<NoteUpdateToolInput>,
    ) -> Result<Json<NoteUpdateToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let clock = self.clock.clone();
        let raw_path = input.path.clone();
        let nothing_to_change = input.patch.is_empty() && input.body.is_none();
        let path = evaluate(move || {
            if nothing_to_change {
                return Err(ToolError::Invalid("note_update needs a patch, a body, or both"));
            }
            let path = VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &raw_path,
            })?;
            if path.relative().extension().and_then(std::ffi::OsStr::to_str) != Some("md") {
                return Err(ToolError::Invalid("note path must end in .md"));
            }
            Ok(path)
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        let filesystem = Arc::clone(&self.filesystem);
        let service = Arc::clone(&self.mutations);
        execute(move || {
            let _guards = guards;
            let expected_hash = ContentHash::try_from(input.expected_hash)?;
            let source = filesystem.read_text(&ReadTextRequest {
                path: path.clone(),
                limit: None,
            })?;
            // A stale caller is told so before its patch is judged against fields it never saw.
            if source.content_hash != expected_hash {
                return Err(FsError::Conflict {
                    path: PathBuf::from(path),
                    current_hash: Some(source.content_hash),
                }
                .into());
            }
            let note = NoteUpdate::try_from(NoteUpdateInput {
                document: FrontmatterDocument::try_from(source.content.as_str())?,
                patch: input.patch,
                body: input.body.as_deref(),
                timestamp: &frontmatter_timestamp(clock.now()),
            })?;
            let result = service.write_file(&WriteMutation {
                path,
                content: String::try_from(note)?.into(),
                expected_hash: Some(expected_hash),
                force: false,
                origin: Origin::Tool("note_update".to_owned()),
            })?;
            Ok(NoteUpdateToolResult::from(result))
        })
        .await
    }

    #[tool(
        name = "frontmatter_read",
        description = "Read ordered YAML frontmatter from an Obsidian Markdown note; with expand_links, every link value is resolved to its vault-rooted path and no content_hash is returned",
        output_schema = fallible_output_schema_for::<FrontmatterReadToolResult>()
    )]
    async fn read_frontmatter(
        &self,
        Parameters(input): Parameters<FrontmatterReadToolInput>,
    ) -> Result<Json<FrontmatterReadToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let file_names = Arc::clone(&self.file_names);
        execute(move || {
            let path = VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            if input.expand_links {
                require_markdown_note(&path)?;
                let names = file_names
                    .get(usize::try_from(path.root_id())?)
                    .ok_or(ToolError::Invalid("the vault has no file name index"))?;
                let source = filesystem.read_text(&ReadTextRequest {
                    path: path.clone(),
                    limit: None,
                })?;
                let document = FrontmatterDocument::try_from(source.content.as_str())?;
                // Only the frontmatter block is expanded: the body's links, and the limits and lookups they would cost,
                // play no part in reading the properties.
                let block: String = source
                    .content
                    .split_inclusive('\n')
                    .take(document.body_start_line().saturating_sub(1))
                    .collect();
                let ExpandedText(expanded) = ExpandedText::try_from(NoteExpansion {
                    source: &block,
                    note: &path,
                    filesystem: &filesystem,
                    roots: &roots,
                    names,
                })?;
                let expanded = FrontmatterDocument::try_from(expanded.as_str())?;
                return Ok(FrontmatterReadToolResult::from(ExpandedFrontmatter(expanded)));
            }
            let source = filesystem.read_text(&ReadTextRequest { path, limit: None })?;
            let document = FrontmatterDocument::try_from(source.content.as_str())?;
            Ok(FrontmatterReadToolResult::from((document, source.content_hash)))
        })
        .await
    }

    #[tool(
        name = "frontmatter_update",
        description = "Apply an atomic JSON merge patch to note frontmatter while preserving its body",
        output_schema = fallible_output_schema_for::<FrontmatterUpdateToolResult>()
    )]
    async fn update_frontmatter(
        &self,
        Parameters(input): Parameters<FrontmatterUpdateInput>,
    ) -> Result<Json<FrontmatterUpdateToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let clock = self.clock.clone();
        let path = evaluate(move || {
            VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &input.path,
            })
            .map_err(ToolError::from)
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        let filesystem = Arc::clone(&self.filesystem);
        let service = Arc::clone(&self.mutations);
        execute(move || {
            let _guards = guards;
            let source = filesystem.read_text(&ReadTextRequest {
                path: path.clone(),
                limit: None,
            })?;
            let mut document = FrontmatterDocument::try_from(source.content.as_str())?;
            document.apply_merge_patch(input.patch, &frontmatter_timestamp(clock.now()));
            let content = String::try_from(&document)?;
            let result = service.write_file(&WriteMutation {
                path,
                content: content.into(),
                expected_hash: input
                    .expected_hash
                    .map(ContentHash::try_from)
                    .transpose()?
                    .or(Some(source.content_hash)),
                force: false,
                origin: Origin::Tool("frontmatter_update".to_owned()),
            })?;
            Ok(FrontmatterUpdateToolResult::from(result))
        })
        .await
    }

    #[tool(
        name = "base_create",
        description = "Create a validated Obsidian Bases YAML definition, and ensure its paired <name>.md sidecar (key_format, key_prefix and key_prefixes with an independent counter per prefix, last_create, last_update) used by base_record_create. key_prefix names the default id series, key_prefixes declares further series (for example A for actions and D for decisions); omit both for a FILEPATH-keyed base",
        output_schema = fallible_output_schema_for::<BaseCreateToolResult>()
    )]
    async fn create_base(
        &self,
        Parameters(input): Parameters<BaseCreateToolInput>,
    ) -> Result<Json<BaseCreateToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let clock = self.clock.clone();
        let key_prefix = input.key_prefix.clone();
        let key_prefixes = input.key_prefixes.clone().unwrap_or_default();
        let (request, sidecar_path, requested_sidecar) = evaluate(move || {
            let BaseVaultPath(path) = BaseVaultPath::try_from(StructuredPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            let document = BaseDocument::try_from(input.definition)?;
            let sidecar_path = crate::tools::base_sidecar::sidecar_path(&roots, &path)?;
            // Validated before anything is written: an unusable prefix is a
            // request error, not a secondary problem to degrade to a warning.
            let requested_sidecar = BaseSidecar::try_from(BaseSidecarInput {
                key_prefix: key_prefix.as_deref(),
                key_prefixes: &key_prefixes,
                timestamp: &frontmatter_timestamp(clock.now()),
            })?;
            Ok((
                WriteMutation {
                    path,
                    content: String::try_from(&document)?.into(),
                    expected_hash: None,
                    force: false,
                    origin: Origin::Tool("base_create".to_owned()),
                },
                sidecar_path,
                requested_sidecar,
            ))
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[request.path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        let service = Arc::clone(&self.mutations);
        let filesystem = Arc::clone(&self.filesystem);
        execute(move || {
            let _guards = guards;
            let base_result = service.write_file(&request)?;
            let base_stem = sidecar_path
                .relative()
                .file_stem()
                .and_then(std::ffi::OsStr::to_str)
                .ok_or(ToolError::Invalid("base path must have a UTF-8 filename"))?
                .to_owned();
            let (sidecar_path_text, sidecar_warnings) = crate::tools::base_sidecar::ensure_sidecar_on_create(
                &filesystem,
                &service,
                &sidecar_path,
                &base_stem,
                &requested_sidecar,
            );
            Ok(BaseCreateToolResult::new(
                base_result,
                sidecar_path_text,
                sidecar_warnings,
            ))
        })
        .await
    }

    #[tool(
        name = "base_read",
        description = "Read an ordered Obsidian Bases definition with schema diagnostics; a file that fails to parse at all is reported as a diagnostic, not a tool error",
        output_schema = fallible_output_schema_for::<BaseReadToolResult>()
    )]
    async fn read_base(
        &self,
        Parameters(input): Parameters<PathInput>,
    ) -> Result<Json<BaseReadToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        execute(move || {
            let BaseVaultPath(path) = BaseVaultPath::try_from(StructuredPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            let source = filesystem.read_text(&ReadTextRequest { path, limit: None })?;
            Ok(BaseReadToolResult::from(BaseReadSource {
                document: BaseDocument::try_from(source.content.as_str()),
                content_hash: source.content_hash,
            }))
        })
        .await
    }

    #[tool(
        name = "base_apply",
        description = "Apply ordered Base operations atomically after validating the complete result",
        output_schema = fallible_output_schema_for::<StructuredWriteToolResult>()
    )]
    async fn apply_base(
        &self,
        Parameters(input): Parameters<BaseApplyToolInput>,
    ) -> Result<Json<StructuredWriteToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let BaseVaultPath(path) = evaluate(move || {
            BaseVaultPath::try_from(StructuredPathInput {
                roots: &roots,
                raw: &input.path,
            })
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        let filesystem = Arc::clone(&self.filesystem);
        let service = Arc::clone(&self.mutations);
        let roots = Arc::clone(&self.roots);
        let clock = self.clock.clone();
        execute(move || {
            let _guards = guards;
            let source = filesystem.read_text(&ReadTextRequest {
                path: path.clone(),
                limit: None,
            })?;
            let mut document = BaseDocument::try_from(source.content.as_str())?;
            document.apply(input.operations.into_iter().map(BaseOperation::from).collect())?;
            let sidecar_path = crate::tools::base_sidecar::sidecar_path(&roots, &path)?;
            let result = service.write_file(&WriteMutation {
                path,
                content: String::try_from(&document)?.into(),
                expected_hash: input
                    .expected_hash
                    .map(ContentHash::try_from)
                    .transpose()?
                    .or(Some(source.content_hash)),
                force: false,
                origin: Origin::Tool("base_apply".to_owned()),
            })?;
            let timestamp = frontmatter_timestamp(clock.now());
            let sidecar_warnings =
                crate::tools::base_sidecar::touch_sidecar_on_apply(&filesystem, &service, &sidecar_path, &timestamp);
            let mut result = StructuredWriteToolResult::from(result);
            result.warnings_mut().extend(sidecar_warnings);
            Ok(result)
        })
        .await
    }

    #[tool(
        name = "base_record_create",
        description = "Create a keyed record note within a .base's scope, stamping its sidecar's next id/key and advancing that series' counter (or, for a FILEPATH-keyed base with no id of its own to assign, accepting one supplied in frontmatter instead). prefix chooses which of the sidecar's declared id series to assign from, each with its own counter; omit it to use the default series. Refuses an undeclared prefix and a caller-supplied id that already exists elsewhere in the base's own scope. When the series' next key is already held by a note written outside this tool, the series counter is caught up to the highest key of that series in scope, the next free key is assigned, and a warning says so. unique_fields names frontmatter fields whose values together must not already be held by a record in scope; a match is refused with io/duplicate-record naming that record, so a raiser that runs again finds the record it already raised. When the sidecar declares a template for the record's series (template, series_templates), the record carries that template's fixed fields (type always, plus those the sidecar names in fixed_fields) instead of type: note; omit them to have them stamped. A supplied value that differs from a fixed one is refused with io/fixed-field-conflict, and a template that cannot be used (not found, ambiguous, not Markdown, too large, malformed, or without a literal value for a fixed field) with format/series-template, both before any key is reserved. When the base's sidecar names a kind registry (event_kinds), the record is an event of a kind the vault declares there: give kind, optionally severity, subject, subject_id, entity, and content, and the kind's own fields in frontmatter; the server stamps type, at, and the path, assigns the kind's key only when the kind declares a key field (returned as key), in which case prefix is required and must be a series the sidecar's series_kinds maps to that kind (prefix for any other kind is refused with events/invalid-series), and refuses an undeclared or server-fired kind, a severity, field, or subject the kind does not allow, and any server-stamped value with events/unknown-kind, events/server-fired-kind, events/invalid-severity, events/invalid-fields, events/invalid-subject, events/invalid-series, or events/server-stamped before reserving a key or writing anything",
        output_schema = fallible_output_schema_for::<BaseRecordCreateToolResult>()
    )]
    async fn create_base_record(
        &self,
        Parameters(input): Parameters<BaseRecordCreateToolInput>,
    ) -> Result<Json<BaseRecordCreateToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let raw_path = input.path.clone();
        let BaseVaultPath(base_path) = evaluate(move || {
            BaseVaultPath::try_from(StructuredPathInput {
                roots: &roots,
                raw: &raw_path,
            })
        })
        .await?;
        // An event's subject is looked up before the write lock is taken, so a walk of the vault's file names never
        // holds up other writers; what it found is checked again under the lock.
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let file_names = Arc::clone(&self.file_names);
        let lookup_path = base_path.clone();
        let lookup_frontmatter = input.frontmatter.clone();
        let subject = evaluate(move || {
            let names = file_names
                .get(usize::try_from(lookup_path.root_id())?)
                .ok_or(ToolError::Invalid("the vault has no file name index"))?;
            crate::events_record::look_up_subject(&filesystem, &roots, names, &lookup_path, &lookup_frontmatter)
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[base_path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        let roots = Arc::clone(&self.roots);
        let clock = self.clock.clone();
        let filesystem = Arc::clone(&self.filesystem);
        let service = Arc::clone(&self.mutations);
        execute(move || {
            let _guards = guards;
            let now = clock.now();
            let timestamp = frontmatter_timestamp(now);
            let request = crate::tools::base_record::RecordCreateRequest {
                base_path,
                roots: &roots,
                frontmatter: input.frontmatter,
                content: input.content,
                folder: input.folder.as_deref(),
                filename: input.filename.as_deref(),
                prefix: input.prefix.as_deref(),
                unique_fields: input
                    .unique_fields
                    .map(crate::tools::base_record::UniqueFields::try_from)
                    .transpose()?,
                timestamp: &timestamp,
            };
            let outcome = match crate::events_record::kind_registry_folder(&filesystem, &roots, &request.base_path)? {
                Some(kinds_folder) => crate::events_record::record_event(
                    &filesystem,
                    service.as_ref(),
                    &request,
                    &kinds_folder,
                    subject,
                    now,
                )?,
                None => crate::tools::base_record::create_record(&filesystem, service.as_ref(), &request)?,
            };
            Ok(BaseRecordCreateToolResult::from(outcome))
        })
        .await
    }

    #[tool(
        name = "base_query",
        description = "Execute a Base view's filter tree against the vault and return matching rows as a Markdown table (default), JSON, or CSV. Evaluates a documented subset of the Bases filter grammar (==, !=, .contains(), .containsAny(...), .containsAll(...), file.hasTag(), file.inFolder(), and/or/not, and every documented file.* property: ext/name/basename/path/folder/size/ctime/mtime/tags/links/embeds/backlinks/properties). A display column naming a formula evaluates it only when the formula body is a bare property reference (shown as that property's own value) or file.asLink(display?) (a link to the row's own file, display defaulting to its basename); every other formula (arithmetic, if(), date functions, etc.) is shown as an unevaluated marker, and a formula.* reference in a filter or sort key is always rejected. Prefer file.inFolder(\"...\") over file.folder == \"...\" || file.folder.contains(\".../\") to scope a query to a directory and its subdirectories: file.inFolder() also lets the scan itself narrow to that directory, where the .contains() form cannot. A filter outside the grammar is refused before any note is read. An inline definition is one view with its keys at the top level (filters, order, sort, limit, properties, formulas), not the views list of a .base file; any other key, and view alongside it, is refused. Pass ids (with path) instead of a view to fetch records by id from the base's scope in one call: each comes back with its path, frontmatter, and a content_hash usable as a later write's expected_hash, so a task can be claimed by looking it up and then updating it conditionally.",
        output_schema = fallible_output_schema_for::<BaseQueryToolResult>()
    )]
    async fn query_base(
        &self,
        Parameters(input): Parameters<BaseQueryToolInput>,
    ) -> Result<Json<BaseQueryToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let search = Arc::clone(&self.search);
        execute(move || {
            if input.ids.is_some() || input.include_content.is_some() {
                return crate::tools::base_query::look_up_records(&filesystem, &roots, input);
            }
            let format = QueryFormat::from(input.format.unwrap_or_default());
            let (root_id, mut definition, document_diagnostics) = match (&input.path, &input.definition) {
                (Some(_), Some(_)) => {
                    return Err(ToolError::Invalid(
                        "base_query accepts exactly one of path or definition, not both",
                    ));
                }
                (None, None) => {
                    return Err(ToolError::Invalid("base_query requires either path or definition"));
                }
                (Some(raw), None) => {
                    let BaseVaultPath(path) = BaseVaultPath::try_from(StructuredPathInput { roots: &roots, raw })?;
                    let source = filesystem.read_text(&ReadTextRequest {
                        path: path.clone(),
                        limit: None,
                    })?;
                    let document = BaseDocument::try_from(source.content.as_str())?;
                    let definition = QueryDefinition::from_document(&document, input.view.as_deref())?;
                    let diagnostics = document
                        .diagnostics()
                        .into_iter()
                        .map(StructuredDiagnosticToolResult::from)
                        .collect();
                    (path.root_id(), definition, diagnostics)
                }
                (None, Some(inline)) => {
                    if input.view.is_some() {
                        return Err(ToolError::Invalid(
                            "base_query's view names a view of a .base file, so it requires path; an inline definition is itself one view",
                        ));
                    }
                    let definition = QueryDefinition::from_inline(inline)?;
                    let raw = input.vault.clone().unwrap_or_else(|| ".".to_owned());
                    let vault_path = VaultPath::try_from(VaultPathInput {
                        roots: &roots,
                        raw: &raw,
                    })?;
                    (vault_path.root_id(), definition, Vec::new())
                }
            };
            if let Some(limit) = input.limit {
                definition.limit = Some(usize::try_from(limit)?);
            }
            let outcome = crate::tools::base_query::run(&filesystem, &roots, root_id, &definition, format, &search)?;
            let diagnostics = document_diagnostics.into_iter().chain(outcome.diagnostics).collect();
            Ok(BaseQueryToolResult {
                content: outcome.content,
                columns: outcome.columns,
                matched: outcome.matched,
                truncated: outcome.truncated,
                diagnostics,
                records: None,
                not_found: None,
            })
        })
        .await
    }

    #[tool(
        name = "canvas_create",
        description = "Create a validated JSON Canvas 1.0 document and generate omitted identifiers",
        output_schema = fallible_output_schema_for::<StructuredWriteToolResult>()
    )]
    async fn create_canvas(
        &self,
        Parameters(input): Parameters<CanvasCreateToolInput>,
    ) -> Result<Json<StructuredWriteToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let request = evaluate(move || {
            let CanvasVaultPath(path) = CanvasVaultPath::try_from(StructuredPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            let document = CanvasDocument::try_from(CanvasCreateInput {
                nodes: input.nodes,
                edges: input.edges,
            })?;
            Ok(WriteMutation {
                path,
                content: String::try_from(&document)?.into(),
                expected_hash: None,
                force: false,
                origin: Origin::Tool("canvas_create".to_owned()),
            })
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[request.path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        let service = Arc::clone(&self.mutations);
        execute(move || {
            let _guards = guards;
            Ok(StructuredWriteToolResult::from(service.write_file(&request)?))
        })
        .await
    }

    #[tool(
        name = "canvas_read",
        description = "Read JSON Canvas nodes and edges with schema diagnostics; a file that fails to parse at all is reported as a diagnostic, not a tool error",
        output_schema = fallible_output_schema_for::<CanvasReadToolResult>()
    )]
    async fn read_canvas(
        &self,
        Parameters(input): Parameters<PathInput>,
    ) -> Result<Json<CanvasReadToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        execute(move || {
            let CanvasVaultPath(path) = CanvasVaultPath::try_from(StructuredPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            let source = filesystem.read_text(&ReadTextRequest { path, limit: None })?;
            Ok(CanvasReadToolResult::from(CanvasReadSource {
                document: CanvasDocument::try_from(source.content.as_str()),
                content_hash: source.content_hash,
            }))
        })
        .await
    }

    #[tool(
        name = "canvas_apply",
        description = "Apply ordered JSON Canvas operations atomically after full validation",
        output_schema = fallible_output_schema_for::<StructuredWriteToolResult>()
    )]
    async fn apply_canvas(
        &self,
        Parameters(input): Parameters<CanvasApplyToolInput>,
    ) -> Result<Json<StructuredWriteToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let CanvasVaultPath(path) = evaluate(move || {
            CanvasVaultPath::try_from(StructuredPathInput {
                roots: &roots,
                raw: &input.path,
            })
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        let filesystem = Arc::clone(&self.filesystem);
        let service = Arc::clone(&self.mutations);
        execute(move || {
            let _guards = guards;
            let source = filesystem.read_text(&ReadTextRequest {
                path: path.clone(),
                limit: None,
            })?;
            let mut document = CanvasDocument::try_from(source.content.as_str())?;
            document.apply(input.operations.into_iter().map(CanvasOperation::from).collect())?;
            let result = service.write_file(&WriteMutation {
                path,
                content: String::try_from(&document)?.into(),
                expected_hash: input
                    .expected_hash
                    .map(ContentHash::try_from)
                    .transpose()?
                    .or(Some(source.content_hash)),
                force: false,
                origin: Origin::Tool("canvas_apply".to_owned()),
            })?;
            Ok(StructuredWriteToolResult::from(result))
        })
        .await
    }

    #[tool(
        name = "frontmatter_link",
        description = "Add or remove wikilinks in the link fields (depends_on, blocks, related, parent, and so on) that a base's sidecar declares under link_fields, validating that each target exists (a remove also accepts the vault path of a note that is gone, to clear a dangling link) and writing the reciprocal link on the target note where the sidecar declares an inverse field. Links are written as quoted wikilinks whose display text is the target's id, else its title, else its file name, relative to the governing base when the target is in its scope (the bare key for a keyed base, else the path within the base's folder), otherwise by full vault path. Add and remove are idempotent, so repeating a call repairs a reciprocal whose write failed. Between 1 and 100 operations apply in order in one atomic write to the note, guarded by expected_hash even when nothing would change",
        output_schema = fallible_output_schema_for::<FrontmatterLinkToolResult>()
    )]
    async fn link_frontmatter(
        &self,
        Parameters(input): Parameters<FrontmatterLinkToolInput>,
    ) -> Result<Json<FrontmatterLinkToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let raw_path = input.path.clone();
        let path = evaluate(move || {
            VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &raw_path,
            })
            .map_err(ToolError::from)
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        let roots = Arc::clone(&self.roots);
        let clock = self.clock.clone();
        let filesystem = Arc::clone(&self.filesystem);
        let service = Arc::clone(&self.mutations);
        execute(move || {
            let _guards = guards;
            let expected_hash = input.expected_hash.map(ContentHash::try_from).transpose()?;
            let timestamp = frontmatter_timestamp(clock.now());
            crate::tools::frontmatter_link::edit_links(
                &filesystem,
                &service,
                &crate::tools::frontmatter_link::LinkRequest {
                    note: path,
                    roots: &roots,
                    operations: input.operations,
                    expected_hash,
                    timestamp: &timestamp,
                },
            )
        })
        .await
    }

    #[tool(
        name = "links_read",
        description = "Read the links of a note or file. direction out (the default) returns outgoing, the note's wikilinks, embeds, and Markdown links, each with the form it was written in and, when it resolves, the vault file it reaches, and unresolved, the links that find no file (not-found) or several (ambiguous, with candidates). Links resolve as Obsidian resolves them, against every file in the vault (attachments, canvases, bases, and notes in excluded folders included). direction in returns incoming instead: every link to the file, one entry for each, with its source, line, and form, sorted by source then line, read from the link graph after it is brought up to date with outside edits (a link from an excluded folder still counts); both returns all three. in and both fail with index/disabled when the vault's link graph is off",
        output_schema = fallible_output_schema_for::<LinksReadToolResult>()
    )]
    async fn read_links(
        &self,
        Parameters(input): Parameters<LinksReadInput>,
    ) -> Result<Json<LinksReadToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let file_names = Arc::clone(&self.file_names);
        let search = Arc::clone(&self.search);
        execute(move || {
            let path = VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            let read_outgoing = || -> Result<LinksReadToolResult, ToolError> {
                let names = file_names
                    .get(usize::try_from(path.root_id())?)
                    .ok_or(ToolError::Invalid("the vault has no file name index"))?;
                let source = filesystem.read_text(&ReadTextRequest {
                    path: path.clone(),
                    limit: None,
                })?;
                LinksReadToolResult::try_from(LinksReading {
                    source: &source.content,
                    note: &path,
                    filesystem: &filesystem,
                    roots: &roots,
                    names,
                })
            };
            let read_incoming = || -> Result<Vec<IncomingLink>, ToolError> {
                let service = search
                    .get(usize::try_from(path.root_id())?)
                    .and_then(Option::as_ref)
                    .ok_or(ToolError::SearchDisabled)?;
                Ok(service.incoming_links(&path)?)
            };
            match input.direction {
                LinkDirectionInput::Out => read_outgoing(),
                LinkDirectionInput::Both => Ok(read_outgoing()?.with_incoming(read_incoming()?)),
                LinkDirectionInput::In => {
                    if !filesystem.is_file(&path)? {
                        return Err(ToolError::from(FsError::NotFound {
                            path: PathBuf::from(path.clone()),
                        }));
                    }
                    Ok(LinksReadToolResult::from(read_incoming()?))
                }
            }
        })
        .await
    }
}
