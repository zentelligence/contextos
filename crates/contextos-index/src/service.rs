use std::path::Path;

use contextos_core::{
    Clock, ENTITIES_PATH, EntitiesFile, EntityRoster, ListsVault, MaintainsIndexes, MoveMutation, MovesVault,
    NoCreationReports, OperationEvent, OperationWarning, Origin, ReadsVault, ReportsCreation, RoutesOperations,
    SystemClock, VaultChildInput, VaultEntry, VaultEntryKind, VaultPath, VaultPathInput, VaultRoot, VaultRootId,
    VaultSet, WriteMutation, WritesVault, frontmatter_timestamp,
};

use crate::document::{IndexDocument, IndexReconcileInput};
use crate::entry::{IndexEntry, IndexEntryKind, IndexSummary, IndexSummaryInput, IndexTitle, IndexTitleInput};
use crate::error::{IndexError, IndexServiceError};
use crate::folder_index::is_folder_index;
use crate::frontmatter::{IndexFrontmatterInput, IndexRepairAction, UpdatedStamp, needs_creation_date};

/// Auditable outcome of one recursive managed-index rebuild.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexRebuildResult {
    pub directories_scanned: usize,
    pub indexes_updated: usize,
    pub indexes_created: usize,
    pub skipped: usize,
    pub events: Vec<OperationEvent>,
    /// Every frontmatter value a repairing rebuild corrected (or, in a dry run, would correct); empty otherwise.
    pub repairs: Vec<IndexRepair>,
    /// Non-fatal conditions found along the way, such as an index whose frontmatter had to be left as found.
    pub warnings: Vec<IndexWarning>,
}

/// One non-fatal condition met while rebuilding a managed index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexWarning {
    /// The index's vault-relative path, forward-slash separated on every platform.
    pub path: String,
    pub message: String,
}

/// The frontmatter values corrected in one managed `index.md` by a repairing rebuild.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexRepair {
    /// The index's vault-relative path, forward-slash separated on every platform.
    pub path: String,
    pub actions: Vec<IndexRepairAction>,
}

/// How one rebuild runs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IndexRebuildOptions {
    /// Report what would change without writing anything.
    pub dry_run: bool,
    /// Also correct the frontmatter values an earlier defective rebuild wrote: see [`IndexRepairAction`].
    /// Off by default, since it overrides values an operator may have set on purpose.
    pub repair: bool,
}

#[derive(Debug, Default)]
struct RebuildState {
    directories_scanned: usize,
    indexes_updated: usize,
    indexes_created: usize,
    skipped: usize,
    events: Vec<OperationEvent>,
    repairs: Vec<IndexRepair>,
    warnings: Vec<IndexWarning>,
    /// Writes made but not yet handed to the router: routed in bounded batches, see [`ROUTING_BATCH`].
    unrouted: Vec<OperationEvent>,
}

/// Writes gathered before they are routed together. Search commits once per batch rather than once per file, which
/// is what keeps a whole-vault rebuild fast; the bound keeps the writes not yet staged in Git, should the process die
/// mid-rebuild, to a handful rather than the whole vault.
const ROUTING_BATCH: usize = 50;

/// Inputs shared by every directory of one rebuild or reconciliation.
struct Pass<'a> {
    /// `None` when the vault has no `.contextos/entities.toml`: no `entity` can then be derived.
    roster: Option<EntityRoster>,
    options: IndexRebuildOptions,
    /// Where each write is handed as it is made. `None` only when the router itself is calling
    /// [`MaintainsIndexes::reconcile`] and routes the returned events on its own account.
    router: Option<&'a dyn RoutesOperations>,
    /// Who the writes are attributed to: the tool that asked for the rebuild, so history names it, or the index
    /// service itself for maintenance the router triggered.
    writes_as: Origin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndexWrite {
    Unchanged,
    Created,
    Updated,
}

impl From<RebuildState> for IndexRebuildResult {
    fn from(value: RebuildState) -> Self {
        Self {
            directories_scanned: value.directories_scanned,
            indexes_updated: value.indexes_updated,
            indexes_created: value.indexes_created,
            skipped: value.skipped,
            events: value.events,
            repairs: value.repairs,
            warnings: value.warnings,
        }
    }
}

/// Dependencies and policy for managed-index persistence.
#[derive(Clone, Debug)]
pub struct IndexServiceConfig<R, W, Z = SystemClock, C = NoCreationReports> {
    pub root: VaultRoot,
    pub roots: VaultSet,
    pub reader: R,
    pub writer: W,
    pub excluded: Vec<String>,
    pub clock: Z,
    /// Dates a file with no `created` from outside its content (version history, filesystem birth time).
    pub creation: C,
}

/// Managed-index application service using only consumer-owned vault ports.
#[derive(Clone, Debug)]
pub struct IndexService<R, W, Z = SystemClock, C = NoCreationReports> {
    root: VaultRoot,
    root_id: VaultRootId,
    roots: VaultSet,
    reader: R,
    writer: W,
    excluded: Vec<String>,
    clock: Z,
    creation: C,
}

impl<R, W, Z, C> TryFrom<IndexServiceConfig<R, W, Z, C>> for IndexService<R, W, Z, C>
where
    R: ReadsVault + ListsVault<Error = <R as ReadsVault>::Error>,
    <R as ReadsVault>::Error: std::error::Error + 'static,
    W: WritesVault + MovesVault<Error = <W as WritesVault>::Error>,
    <W as WritesVault>::Error: std::error::Error + 'static,
    Z: Clock,
    C: ReportsCreation,
{
    type Error = IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>;

    fn try_from(value: IndexServiceConfig<R, W, Z, C>) -> Result<Self, Self::Error> {
        let root_index = value
            .roots
            .iter()
            .position(|candidate| candidate == &value.root)
            .ok_or(IndexServiceError::RootNotConfigured)?;
        let root_id = VaultRootId::try_from(root_index).map_err(IndexServiceError::Path)?;
        Ok(Self {
            root: value.root,
            root_id,
            roots: value.roots,
            reader: value.reader,
            writer: value.writer,
            excluded: value.excluded,
            clock: value.clock,
            creation: value.creation,
        })
    }
}

impl<R, W, Z, C> IndexService<R, W, Z, C>
where
    R: ReadsVault + ListsVault<Error = <R as ReadsVault>::Error>,
    <R as ReadsVault>::Error: std::error::Error + 'static,
    W: WritesVault + MovesVault<Error = <W as WritesVault>::Error>,
    <W as WritesVault>::Error: std::error::Error + 'static,
    Z: Clock,
    C: ReportsCreation,
{
    /// Rebuilds every non-excluded directory below the selected subtree, routing each write through `router`.
    ///
    /// # Errors
    ///
    /// Returns a typed discovery, parse, path, or persistence error.
    pub fn rebuild(
        &self,
        directory: &VaultPath,
        origin: Origin,
        router: &dyn RoutesOperations,
    ) -> Result<Vec<OperationEvent>, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        Ok(self.rebuild_report(directory, origin, false, router)?.events)
    }

    /// Rebuilds the selected subtree and reports exact scan and write counts, without repairing, routing each write
    /// through `router`.
    ///
    /// # Errors
    ///
    /// Returns a typed discovery, parse, path, or persistence error.
    pub fn rebuild_report(
        &self,
        directory: &VaultPath,
        origin: Origin,
        dry_run: bool,
        router: &dyn RoutesOperations,
    ) -> Result<IndexRebuildResult, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        self.rebuild_report_with(
            directory,
            origin,
            IndexRebuildOptions { dry_run, repair: false },
            router,
        )
    }

    /// Rebuilds the selected subtree as `options` direct, and reports exact scan and write counts and every
    /// repair made.
    ///
    /// A rebuild writes on its own account, so `router` is required: every index it writes (and every legacy index
    /// it moves) is handed to it as the write happens, which is how Git and search learn of the change. Warnings the
    /// router raises are reported in the result.
    ///
    /// # Errors
    ///
    /// Returns a typed discovery, parse, path, or persistence error; an `entities.toml` that is present but
    /// malformed fails the whole rebuild before anything is written.
    pub fn rebuild_report_with(
        &self,
        directory: &VaultPath,
        origin: Origin,
        options: IndexRebuildOptions,
        router: &dyn RoutesOperations,
    ) -> Result<IndexRebuildResult, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        if directory.root_id() != self.root_id {
            return Err(IndexServiceError::WrongRoot);
        }
        let pass = Pass {
            roster: self.load_roster()?,
            options,
            router: Some(router),
            writes_as: Origin::Internal(match origin {
                Origin::Tool(name) | Origin::Internal(name) => name,
            }),
        };
        let mut state = RebuildState::default();
        let rebuilt = self.rebuild_directory(directory, &mut state, &pass);
        // What was written before a failure is routed all the same, so a partial rebuild is never left unstaged.
        Self::flush_routing(&mut state, &pass);
        rebuilt?;
        Ok(IndexRebuildResult::from(state))
    }

    /// Reads the vault's configured entities: `None` when the vault has no `.contextos/entities.toml`, so no
    /// `entity` is derived at all (rather than a wrong one), and an error when the file is present but malformed.
    fn load_roster(
        &self,
    ) -> Result<Option<EntityRoster>, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        let path = self.path_from_relative(Path::new(ENTITIES_PATH))?;
        let Some(text) = self.reader.read_optional_text(&path).map_err(IndexServiceError::Read)? else {
            return Ok(None);
        };
        let file = toml::from_str::<EntitiesFile>(&text.content)
            .map_err(|source| IndexServiceError::EntityRoster { source })?;
        Ok(Some(EntityRoster::from(&file)))
    }

    fn rebuild_directory(
        &self,
        directory: &VaultPath,
        state: &mut RebuildState,
        pass: &Pass<'_>,
    ) -> Result<(), IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        let dry_run = pass.options.dry_run;
        state.directories_scanned = state.directories_scanned.saturating_add(1);
        let mut listing = self.reader.list(directory).map_err(IndexServiceError::Read)?;
        let mut moved = Vec::new();
        let has_legacy = self.migrate_legacy_index(directory, &listing, &mut moved, dry_run, &pass.writes_as)?;
        for event in moved {
            Self::route_written(state, pass, &event);
            state.events.push(event);
        }
        if has_legacy && !dry_run {
            listing = self.reader.list(directory).map_err(IndexServiceError::Read)?;
        }
        state.skipped = state.skipped.saturating_add(
            listing
                .iter()
                .filter(|entry| self.is_config_excluded(directory, entry))
                .count(),
        );
        let visible = listing
            .into_iter()
            .filter(|entry| self.includes(directory, entry))
            .collect::<Vec<_>>();
        match self.persist_directory(directory, &visible, state, pass, has_legacy)? {
            IndexWrite::Unchanged => {}
            IndexWrite::Created => {
                state.indexes_created = state.indexes_created.saturating_add(1);
            }
            IndexWrite::Updated => {
                state.indexes_updated = state.indexes_updated.saturating_add(1);
            }
        }
        for entry in visible.iter().filter(|entry| entry.kind == VaultEntryKind::Directory) {
            let child = self.child_path(directory, &entry.name)?;
            self.rebuild_directory(&child, state, pass)?;
        }
        Ok(())
    }

    fn persist_directory(
        &self,
        directory: &VaultPath,
        entries: &[VaultEntry],
        state: &mut RebuildState,
        pass: &Pass<'_>,
        legacy_preview: bool,
    ) -> Result<IndexWrite, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        let dry_run = pass.options.dry_run;
        let index_path = self.child_path(directory, "index.md")?;
        let existing_path = if legacy_preview && dry_run {
            self.child_path(directory, "_index.md")?
        } else {
            index_path.clone()
        };
        let existing = self
            .reader
            .read_optional_text(&existing_path)
            .map_err(IndexServiceError::Read)?;
        let existing_source = existing.as_ref().map(|text| text.content.as_str());
        let directory_path = directory.portable_relative();
        let mut rows = Vec::with_capacity(entries.len());
        for entry in entries.iter().filter(|entry| entry.name != "index.md") {
            rows.push(self.build_index_entry(directory, entry)?);
        }
        let directory_name = directory
            .relative()
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("Vault");
        let timestamp = frontmatter_timestamp(self.clock.now());
        let entity = pass
            .roster
            .as_ref()
            .map(|roster| <&str>::from(&roster.scope_of(directory)).to_owned());
        // A creation date is looked up only for a file that exists and would use one, so a rebuild of a vault whose
        // indexes are already complete never walks version history.
        let created = existing_source
            .filter(|source| needs_creation_date(source, pass.options.repair))
            .and_then(|_| self.creation.created_at(&existing_path))
            .map(|created| self.clock.localise(created));
        let reconcile = |updated: UpdatedStamp| {
            IndexDocument::try_from(IndexReconcileInput {
                source: existing_source,
                directory_path: &directory_path,
                directory_name,
                frontmatter: IndexFrontmatterInput {
                    entity: entity.as_deref(),
                    timestamp: &timestamp,
                    created,
                    updated,
                    repair: pass.options.repair,
                },
                entries: rows.clone(),
            })
        };
        // `updated` moves only when something else in the file does: reconcile without touching it first, and
        // restamp only if that differs from what is on disk, so a rebuild that changes nothing is byte-identical.
        let unstamped = reconcile(UpdatedStamp::Preserve)?;
        let repairs = unstamped.repairs().to_vec();
        if let Some(key) = unstamped.uneditable_frontmatter() {
            state.warnings.push(IndexWarning {
                path: index_path.portable_relative(),
                message: format!(
                    "frontmatter left as found: its layout cannot be edited line by line near {key}; \
                     rewrite it as plain block-style YAML to have it completed"
                ),
            });
        }
        if existing_source.is_some_and(|source| source == String::from(unstamped).as_str()) {
            return Ok(if dry_run && legacy_preview {
                IndexWrite::Updated
            } else {
                IndexWrite::Unchanged
            });
        }
        let content = String::from(reconcile(UpdatedStamp::Stamp)?);
        if !repairs.is_empty() {
            state.repairs.push(IndexRepair {
                path: index_path.portable_relative(),
                actions: repairs,
            });
        }
        let created = existing.is_none();
        if dry_run {
            return Ok(if created {
                IndexWrite::Created
            } else {
                IndexWrite::Updated
            });
        }
        let result = self
            .writer
            .persist(&WriteMutation {
                path: index_path,
                content: content.into(),
                expected_hash: existing.map(|text| text.content_hash),
                force: false,
                origin: pass.writes_as.clone(),
            })
            .map_err(IndexServiceError::Write)?;
        if let Some(event) = result.event {
            Self::route_written(state, pass, &event);
            state.events.push(event);
        }
        Ok(if created {
            IndexWrite::Created
        } else {
            IndexWrite::Updated
        })
    }

    /// Notes one write this rebuild made for routing, handing a full batch to the router as it fills.
    fn route_written(state: &mut RebuildState, pass: &Pass<'_>, event: &OperationEvent) {
        if pass.router.is_none() {
            return;
        }
        state.unrouted.push(event.clone());
        if state.unrouted.len() >= ROUTING_BATCH {
            Self::flush_routing(state, pass);
        }
    }

    /// Hands the writes gathered so far to the router, and keeps any warning it raises against the first index of
    /// the batch: a routing fault belongs to the batch, not to one file.
    fn flush_routing(state: &mut RebuildState, pass: &Pass<'_>) {
        let Some(router) = pass.router else {
            return;
        };
        if state.unrouted.is_empty() {
            return;
        }
        let events = std::mem::take(&mut state.unrouted);
        let path = events
            .first()
            .and_then(|event| event.paths.first())
            .map_or_else(String::new, VaultPath::portable_relative);
        for warning in router.route_all(events).warnings {
            state.warnings.push(IndexWarning {
                path: path.clone(),
                message: format!("{}: {}", warning.code, warning.message),
            });
        }
    }

    /// Resolves one directory entry's managed-row title and summary: a
    /// file's own frontmatter, or a sub-directory's own `index.md`
    /// frontmatter.
    fn build_index_entry(
        &self,
        directory: &VaultPath,
        entry: &VaultEntry,
    ) -> Result<IndexEntry, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        let is_markdown = Path::new(&entry.name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
        let entry_path = directory.relative().join(&entry.name);
        let source = match entry.kind {
            VaultEntryKind::File if is_markdown => {
                let child = self.child_path(directory, &entry.name)?;
                self.reader
                    .read_optional_text(&child)
                    .map_err(IndexServiceError::Read)?
                    .map(|text| text.content)
            }
            VaultEntryKind::Directory => {
                let child = self.child_path(directory, &entry.name)?;
                let child_index = self.child_path(&child, "index.md")?;
                self.reader
                    .read_optional_text(&child_index)
                    .map_err(IndexServiceError::Read)?
                    .map(|text| text.content)
            }
            VaultEntryKind::File => None,
        };
        let title = IndexTitle::try_from(IndexTitleInput {
            filename: &entry.name,
            source: source.as_deref(),
        })
        .map_err(|source| IndexError::Frontmatter {
            path: entry_path.clone(),
            source,
        })?;
        let summary = IndexSummary::try_from(IndexSummaryInput {
            source: source.as_deref(),
        })
        .map_err(|source| IndexError::Frontmatter {
            path: entry_path,
            source,
        })?;
        Ok(IndexEntry {
            name: entry.name.clone(),
            kind: match entry.kind {
                VaultEntryKind::File => IndexEntryKind::File,
                VaultEntryKind::Directory => IndexEntryKind::Directory,
            },
            title: <&str>::from(&title).to_owned(),
            suggested_summary: <&str>::from(&summary).to_owned(),
        })
    }

    fn reconcile_event(
        &self,
        event: &OperationEvent,
    ) -> Result<Vec<OperationEvent>, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        let mut directories = Vec::new();
        for path in &event.paths {
            if path.root_id() != self.root_id {
                continue;
            }
            let absolute = <&Path>::from(path);
            if absolute.is_dir() && !self.is_excluded(path.relative()) {
                directories.push(path.clone());
            }
            let mut ancestor = path.relative().parent().map(Path::to_path_buf);
            while let Some(relative) = ancestor {
                let next = relative.parent().map(Path::to_path_buf);
                let parent = self.path_from_relative(&relative)?;
                if !self.is_excluded(parent.relative()) && !directories.iter().any(|directory| directory == &parent) {
                    directories.push(parent);
                }
                ancestor = next;
            }
        }

        // Unlike an explicit rebuild, an ordinary write must not lose its index maintenance to an unreadable entity
        // list: without a roster no `entity` is derived (and none is guessed) until the file is fixed, and an
        // explicit rebuild reports the fault.
        let roster = self.load_roster().unwrap_or_else(|error| {
            tracing::warn!(
                code = error.code(),
                "entity list unusable; managed indexes are kept without an entity"
            );
            None
        });
        let pass = Pass {
            roster,
            options: IndexRebuildOptions::default(),
            router: None,
            writes_as: Origin::Internal("index".to_owned()),
        };
        let mut state = RebuildState::default();
        for directory in directories {
            let mut listing = self.reader.list(&directory).map_err(IndexServiceError::Read)?;
            if self.migrate_legacy_index(&directory, &listing, &mut state.events, false, &pass.writes_as)? {
                listing = self.reader.list(&directory).map_err(IndexServiceError::Read)?;
            }
            let visible = listing
                .into_iter()
                .filter(|entry| self.includes(&directory, entry))
                .collect::<Vec<_>>();
            self.persist_directory(&directory, &visible, &mut state, &pass, false)?;
        }
        if !state.warnings.is_empty() {
            tracing::warn!(
                indexes = state.warnings.len(),
                "managed index frontmatter left as found because its layout cannot be edited line by line"
            );
        }
        Ok(state.events)
    }

    fn includes(&self, directory: &VaultPath, entry: &VaultEntry) -> bool {
        if entry.name.starts_with('.') || is_folder_index(&entry.name) {
            return false;
        }
        let relative = directory.relative().join(&entry.name);
        !self.excluded.iter().any(|excluded| {
            let excluded = Path::new(excluded);
            relative == excluded || relative.starts_with(excluded)
        })
    }

    fn is_config_excluded(&self, directory: &VaultPath, entry: &VaultEntry) -> bool {
        let relative = directory.relative().join(&entry.name);
        self.excluded.iter().any(|excluded| {
            let excluded = Path::new(excluded);
            relative == excluded || relative.starts_with(excluded)
        })
    }

    /// The validated path of the entry `name` directly inside `directory`. Inspects only that entry: a directory's
    /// entries are listed and read on every reconciliation, and resolving each from the vault root would stat the
    /// directory's whole ancestry again per entry, which over a slow filesystem (WSL's mount of a Windows drive)
    /// costs far more than reading the entry.
    fn child_path(
        &self,
        directory: &VaultPath,
        name: &str,
    ) -> Result<VaultPath, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        VaultPath::try_from(VaultChildInput {
            roots: &self.roots,
            parent: directory,
            name,
        })
        .map_err(IndexServiceError::Path)
    }

    fn path_from_relative(
        &self,
        relative: &Path,
    ) -> Result<VaultPath, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        let absolute = self.root.path().join(relative);
        let raw = absolute
            .to_str()
            .ok_or_else(|| IndexServiceError::NonUtf8Path { path: absolute.clone() })?;
        VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw,
        })
        .map_err(IndexServiceError::Path)
    }

    fn is_excluded(&self, relative: &Path) -> bool {
        self.excluded.iter().any(|excluded| {
            let excluded = Path::new(excluded);
            relative == excluded || relative.starts_with(excluded)
        })
    }

    /// Reports whether this service would maintain `directory`'s managed
    /// `index.md`: the directory belongs to this service's own
    /// root and is not excluded from index maintenance. The guarded
    /// delete path uses this to decide whether a directory containing only
    /// `index.md`/`_index.md` may be treated as empty for deletion
    /// purposes; a directory this returns `false` for keeps its literal
    /// on-disk contents authoritative.
    #[must_use]
    pub fn manages_directory(&self, directory: &VaultPath) -> bool {
        directory.root_id() == self.root_id && !self.is_excluded(directory.relative())
    }

    fn migrate_legacy_index(
        &self,
        directory: &VaultPath,
        listing: &[VaultEntry],
        events: &mut Vec<OperationEvent>,
        dry_run: bool,
        writes_as: &Origin,
    ) -> Result<bool, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        let has_legacy = listing.iter().any(|entry| entry.name == "_index.md");
        if !has_legacy {
            return Ok(false);
        }
        if listing.iter().any(|entry| entry.name == "index.md") {
            return Err(IndexServiceError::LegacyIndexConflict {
                directory: directory.relative().to_path_buf(),
            });
        }

        if dry_run {
            return Ok(true);
        }

        let result = self
            .writer
            .move_path(&MoveMutation {
                source: self.child_path(directory, "_index.md")?,
                destination: self.child_path(directory, "index.md")?,
                origin: writes_as.clone(),
            })
            .map_err(IndexServiceError::Write)?;
        if let Some(event) = result.event {
            events.push(event);
        }
        Ok(true)
    }
}

impl<R, W, Z, C> MaintainsIndexes for IndexService<R, W, Z, C>
where
    R: ReadsVault + ListsVault<Error = <R as ReadsVault>::Error>,
    <R as ReadsVault>::Error: std::error::Error + 'static,
    W: WritesVault + MovesVault<Error = <W as WritesVault>::Error>,
    <W as WritesVault>::Error: std::error::Error + 'static,
    Z: Clock,
    C: ReportsCreation,
{
    fn reconcile(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning> {
        self.reconcile_event(event).map_err(OperationWarning::from)
    }
}
