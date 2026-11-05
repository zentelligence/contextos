//! Workflow execution and saved-job scheduling.
//!
//! A workflow is executable script logic; a job is its saved configuration,
//! and a run is one invocation. This crate owns the domain rules and
//! self-contained execution engine: cron-schedule evaluation
//! ([`schedule`]), the rule that decides whether a cron tick has
//! already fired ([`state`]), and workflow script execution
//! ([`script`]) dispatching through the host-supplied [`caller::McpCaller`]
//! trait. It has no dependency on `contextos-web`'s own types, and none on
//! any MCP transport crate: `contextos-web` depends on this crate, never
//! the reverse, and every vault-facing effect (reading
//! or writing `.contextos/jobs.toml`, `registry/workflows/` discovery, run
//! record files, the operation log) stays entirely in `contextos-web`.

// Standard output belongs to the command that runs a workflow: a script's `print` goes to its run log.
#![deny(clippy::print_stdout, clippy::print_stderr)]

mod attention;
pub mod bridge;
pub mod call_rules;
pub mod caller;
pub mod clock;
pub mod declared_inputs;
pub mod dependency;
pub mod fingerprint;
pub mod host_state;
pub mod import_rules;
pub mod inputs;
pub mod job_release;
pub mod journal;
pub mod library_lock;
pub mod library_manifest;
pub mod library_name;
pub mod library_requirements;
pub mod library_resolution;
pub mod library_tools;
pub mod lifecycle;
pub mod logging;
pub mod outputs;
pub mod package;
pub mod package_contents;
mod package_error;
pub mod package_name;
pub mod probation;
pub mod release_selection;
pub mod release_state;
pub mod retention;
pub mod run_record;
pub mod schedule;
pub mod scoped_caller;
pub mod script;
mod script_engine;
mod script_library;
pub mod script_modules;
pub mod state;
pub mod storage;
pub mod successor;
#[cfg(test)]
mod test_sources;
pub mod validation;
pub mod vault_libraries;
pub mod workflow_definition;
pub mod workflow_files;
pub mod workflow_name;
mod workflow_table;
pub mod workflows_file;

pub use attention::{
    Assignee, AttentionEvent, AttentionPurpose, Episode, FinishedRun, JobFacts, JobKey, Origin, events,
};
pub use caller::{McpCallFailure, McpCaller, PRE_START_CODES, WorkflowFailureKind, ends_before_the_script};
pub use clock::{Clock, SystemClock};
pub use declared_inputs::DeclaredInputs;
pub use dependency::{resolve_dependencies, validate_no_duplicate_identity};
pub use fingerprint::{FingerprintError, FingerprintLibrary, JobFingerprint, ReleaseFingerprintInput, StoreKind};
pub use host_state::JobsState;
pub use import_rules::{ImportRules, ModuleKey, ModuleOwner};
pub use inputs::{CommandLineInputs, InputOverrides, OverriddenInputs, SuppliedInputs, ValidatedInputs};
pub use job_release::{
    InstalledReleases, JobFileRelease, JobFileText, JobPlace, JobReleaseError, JobVersion, JobVersionNumbers,
    PredecessorKey, Presence, ReleaseRefusal, check_new_job, check_replacement,
};
pub use journal::{CallJournal, JournalledCaller};
pub use library_lock::{LibraryLock, LockedLibrary, VersionBasis};
pub use library_manifest::LibraryManifest;
pub use library_name::LibraryName;
pub use library_requirements::{LibraryRequirements, PackageFileOrigin, SourceText};
pub use library_resolution::{
    LibraryResolution, MAX_LIBRARY_DEPTH, MAX_LIBRARY_PACKAGES, MAX_SNAPSHOT_BYTES, MAX_SNAPSHOT_FILES,
    ReleaseCatalogue, ReleaseContents, ReleaseInventory, ReleaseRef, ReleaseRequirements, Requirement,
    ResolvedLibraries, SnapshotBudget, resolve_libraries,
};
pub use library_tools::{ReleaseToolFailure, check_release_library_tools};
pub use lifecycle::{
    Actor, Applied, EmptySuspensionReason, JobLifecycle, JobSchedule, OwnerIsMember, Suspension, SuspensionCause,
    SuspensionReason, Transition, TransitionError, apply,
};
pub use logging::{
    DegradedReasons, LogLevel, LogLevelError, RunLabel, ScriptLogSink, TracingLogSink, cut_to_bytes,
    effective_log_level,
};
pub use outputs::{DeclaredOutputs, MAX_OUTPUT_BYTES, OutputValues};
pub use package::{
    ContentDigest, FieldKind, PackageDependency, PackageError, PackageIdentity, PackageManifest, PackageProvenance,
    ReleaseFile, RequiredToolContract, TypedField, WorkflowDefinition,
};
pub use package_contents::{PackageFileText, PackageKind};
pub use package_name::{MAX_PACKAGE_NAME_LENGTH, PackageName};
pub use probation::{FirstRunVerdict, NewVersionVerdict, NoVerdict};
pub use release_selection::{OneOffRelease, ReleaseChoice, ReleaseChoiceError, select_release};
pub use release_state::{
    IdentityIssuer, JobIdentity, JobLink, JobRetirement, PackageReleases, RandomIdentityIssuer, ReleaseAllowance,
    ReleaseKind, ReleaseKinds, ReleaseState, ReleaseStateError, ReleaseUse, ReplacedBy, RetiredRelease,
    RetiredReleaseCause, RetirementCause, WorkflowReleaseState, WorkflowVersions, is_reserved_job_slug,
};
pub use retention::{
    JobFile, JobFileReading, JobNaming, KeepReason, LibraryKeep, Retention, RetentionInput, StoredRelease,
    WorkflowKeep, decide as decide_retention,
};
pub use run_record::{
    CallEntry, CallOutcome, INTERRUPTED_ERROR, MAX_ERROR_BYTES, MAX_RECORDED_CALLS, RecordedLibrary, RunHeader,
    RunRecord, RunRecordError, RunStatus, RunTrigger, mark_interrupted,
};
pub use schedule::{CronSchedule, CronScheduleError};
pub use scoped_caller::{ScopedMcpCaller, VaultScope};
pub use script::{RunFailure, RunOutcome, RunResult, RunSettings, run_workflow_declaring};
pub use script_modules::{
    DeclaredModules, DeclaredModulesError, MAX_MODULES, ModuleRead, ModuleSource, ModuleUnreadable, PackageCode,
    PackageToRead, ScriptParts, WorkflowScript,
};
pub use state::{
    ChainHold, ChainRelation, Changed, EnableRequest, Enabled, JobScope, JobState, LinkRefusal, LockOutcome,
    PackageJobsState, ReleaseContext, ReleaseMove, ReleaseRuleFailure, ReleaseRuleFindings, StoredFailure,
    StoredValidation, SuspensionRecord, TickDecision, ValidationKept, ValidationRecord, WorkflowJobsState,
    evaluate_tick,
};
pub use storage::{
    DEFAULT_DAILY_PARTITION_THRESHOLD, DEFAULT_RETENTION_DAYS, archive_ledger_path, archive_line, has_aged_out,
    job_records_folder, needs_daily_partition, parse_run_id, run_record_path,
};
pub use successor::{
    NotATarget, ReleaseStep, SuccessorCheck, SuccessorKey, SuccessorKeyInput, SuccessorRefusal, SuccessorTarget,
    check_successor, resolve_target, selected_for_all,
};
pub use validation::{
    NotADependencyFailure, RULES_REVISION, ScriptSyntaxError, Unverifiable, ValidationCode, ValidationFailure,
    ValidationReport, check_script,
};
pub use vault_libraries::{
    LibraryCode, LibraryError, LibraryReadFailure, LibrarySource, SourceFuture, VaultLibraries, read_libraries,
};
pub use workflow_files::{
    DeclaredFiles, MAX_LISTED_WORKFLOW_FILES, MAX_WORKFLOW_FILE_BYTES, WorkflowFile, WorkflowFileFailure,
    WorkflowFilePath, WorkflowFilePathError, WorkflowFileSource, WorkflowFiles,
};
pub use workflow_name::WorkflowName;
pub use workflows_file::WorkflowsFile;
