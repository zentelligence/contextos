#![forbid(unsafe_code)]

mod commit_schedule;
mod config;
mod config_io;
mod config_load;
mod config_writer;
mod doctor;
mod doctor_bases;
mod doctor_events;
mod doctor_fences;
mod doctor_kind;
mod doctor_projection;
mod doctor_team;
mod event_kinds;
mod events_folder;
mod events_record;
mod events_writer;
mod host_paths;
mod host_registration;
mod http;
mod index_cli;
mod interview;
mod logging;
mod model_cli;
mod module;
mod packages_folder;
mod projection_config;
mod projection_lock;
mod projection_setup;
mod projection_wiring;
mod resource_support;
mod resources;
mod semantic_drain;
mod server;
mod server_clock;
mod server_compose;
mod server_error;
mod server_events;
mod server_owned_folders;
mod service_command;
mod state_dir;
mod tool_effect;
mod tool_error;
mod tools;
mod update_cli;
mod vault_events;
mod vault_events_scope;
mod write_coordinator;

pub use config::{
    Config, ConfigError, ConfigLocation, ConfigProvenance, EmbeddingConfig, EmbeddingProvider, EventsConfig, GitConfig,
    GraphBackendConfig, HttpConfig, IndexMdConfig, LimitsConfig, LinksConfig, LogLevel, OplogConfig, SearchConfig,
    ServerConfig, Transport, VaultConfig,
};
pub use config_io::{ConfigIoError, load_config_document, write_config_document};
pub use config_load::{CliHttp, ConfigEnvironment, ConfigLoadInput};
pub use config_writer::{ConfigDocument, ConfigWriterError, LogSettings, ServerSettingsSummary, VaultSummary};
pub use doctor::{DoctorError, DoctorReport};
pub use doctor_kind::DoctorKind;
pub use host_paths::{
    HostPathError, HostPathResolution, default_claude_desktop_config_path, resolve_linux_config_path,
    resolve_macos_config_path, resolve_windows_config_path, resolve_windows_config_paths,
    resolve_windows_roaming_config_path,
};
pub use host_registration::{
    DeregisterOutcome, DetectsRunningProcesses, HostRegistrationError, RegisteredServer, RegistrationStatus,
    SystemProcessDetector, deregister, is_claude_desktop_running, register, status,
};
pub use http::{HttpTransportError, MOUNT_PATH as HTTP_MOUNT_PATH, build_router, listen, validate_bind};
pub use index_cli::{IndexCliError, IndexReport};
pub use interview::{
    HostRegistrationOutcome, InterviewEnvironment, InterviewError, InterviewReport, Interviewer, TerminalInterviewer,
    default_log_file, run_interview,
};
pub use logging::open as open_log;
pub use model_cli::{ModelCliError, ModelReport, default_model_cache_dir, download_default_model};
pub use module::{
    ModuleCall, ModuleContext, ModuleContextError, ModuleManifest, ModuleNamespace, ModuleRegistry,
    ModuleRegistryError, ServerModule, ServerModuleFuture,
};
pub use projection_config::{
    ClaudeProjectionConfig, ClaudeTiersConfig, CodexProjectionConfig, CodexTierConfig, CodexTiersConfig,
    ProjectionConfig,
};
pub use server::{ContextOsServer, ServerBuildConfig, ServerBuildError, ShutdownFlushError};
pub use server_clock::ServerClock;
pub use server_events::{EventSweepReport, EventsSweepError};
pub use service_command::{
    SERVICE_IDENTITY, ServiceCommand, ServiceCommandError, claim_process_record, run_service_command,
    run_service_command_on,
};
pub use state_dir::StateDirError;
pub use tool_effect::{ToolEffect, tool_names};
pub use tools::doctor::{
    DoctorResolveCliError, DoctorResolveCliReport, DoctorResolveOutcome, SearchSkipped, preview_resolution_for_cli,
    resolve_for_cli,
};
pub use update_cli::{run_check, run_cleanup, run_diagnostics, run_install, run_rollback, run_update};
