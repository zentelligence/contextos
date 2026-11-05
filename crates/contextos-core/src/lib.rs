#![forbid(unsafe_code)]

mod creation;
mod entity;
mod limits;
mod local_time;
mod path;
mod pipeline;
mod projection;
mod resolved;
mod routing;
mod tags;
mod team;
mod vault;

pub use creation::{FirstReportedCreation, NoCreationReports, ReportsCreation};
pub use entity::{ENTITIES_PATH, EntitiesFile, EntityRecord, EntityRoster, EntityScope, VAULT_SCOPE};
pub use limits::{MAX_FILE_BYTES, MAX_REQUEST_BODY_BYTES};
pub use local_time::{
    ConfiguredTimeZone, LocalClock, LocalClockInput, LocalResolution, OperatorTimeZone, ReportedTimeZone,
    TimeZoneError, TimeZoneSource,
};
pub use tags::extract_tags;
pub use team::{
    DeclaredOwner, InvalidMemberSlug, MemberKind, MemberSlug, OwnerStanding, TEAM_PATH, TeamFile, TeamMember,
    TeamMemberRecord, TeamRoster, TeamRosterError, TeamRule,
};

pub use path::{
    PathError, PortableRelativePath, VaultChildInput, VaultPath, VaultPathInput, VaultRoot, VaultRootId,
    VaultRootInput, VaultSet,
};
pub use pipeline::{
    AppendMutation, AppendOutcome, AppliesMutations, Clock, ContentHash, ContentHashError, CreateDirectoryMutation,
    CreateDirectoryOutcome, DeleteMode, DeleteMutation, DeleteOutcome, LINE_ENDINGS_CONVERTED, LineEndingsConverted,
    MoveMutation, MoveOutcome, OpKind, OperationEvent, OperationWarning, Origin, PipelineConfig, PipelineResult,
    RestoreMutation, RoutedPipelineConfig, RoutedWritePipeline, SystemClock, UnixText, WriteContent, WriteMutation,
    WriteOutcome, WritePipeline, frontmatter_timestamp,
};
pub use projection::{NotProjected, ProjectionScope, registry_layout};
pub use resolved::ResolvedPath;
pub use routing::{
    EventsBatch, EventsOutcome, LogsOperations, MaintainsIndexes, NoEvents, NoProjection, NoSearchUpdates,
    NoSubstrateServices, OperationRoute, OperationRouter, OperationRouterConfig, OperationService, ProjectsDefinitions,
    RecordsEvents, RoutedEvents, RoutesOperations, ScopedService, ScopedWarning, UpdatesSearch, VersionsVault,
};
pub use vault::{AppendsVault, ListsVault, MovesVault, ReadsVault, VaultEntry, VaultEntryKind, VaultText, WritesVault};
