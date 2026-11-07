#![forbid(unsafe_code)]
// Standard output carries a run command's summary alone, written by the binary; the library only logs.
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod active_vault;
pub mod activity_log;
pub mod agents_liveness;
pub mod agents_load;
pub mod alerts;
pub mod app;
mod app_routers;
pub mod apps;
pub mod apps_favourites;
pub mod atomic_write;
pub mod bounded_read;
pub mod cli_jobs;
pub mod cli_run;
mod cli_run_once;
pub mod cli_shared;
mod cli_shared_record;
pub mod config;
pub mod config_interview;
pub mod config_writer;
pub mod credential_store;
pub mod date_structure;
pub mod dry_run_caller;
pub mod dry_run_scenario;
pub mod enabling_report;
pub mod entities;
pub mod entity_docs;
pub mod entity_docs_config;
pub mod events_delivered;
pub mod events_hub;
#[cfg(test)]
pub(crate) mod fake_vault;
pub mod file_view;
mod host_store;
pub mod inbox_outbox;
pub mod inbox_outbox_config;
pub mod insights;
pub mod insights_config;
pub mod job_attention;
mod job_chain;
pub mod job_checks;
pub mod job_discovery;
pub mod job_events;
pub mod job_events_marker;
pub mod job_index;
pub mod job_lifecycle;
pub mod job_operation;
#[cfg(test)]
pub(crate) mod job_operation_fake;
pub mod job_operation_shared;
pub mod job_operation_vault;
pub(crate) mod job_owner_check;
pub mod job_pages;
pub mod job_pass;
pub mod job_probation;
pub(crate) mod job_reconcile;
mod job_release_view;
pub mod job_status;
pub(crate) mod job_task_note;
pub(crate) mod job_team_gate;
pub mod job_validation;
pub mod jobs;
pub mod jobs_base;
pub mod journal;
pub mod journal_config;
pub mod journal_entry;
pub mod journal_template;
pub mod last_visited;
mod library_checks;
mod library_dependents;
pub mod library_document;
mod library_status;
mod library_views;
pub mod logging;
mod loopback_host;
pub mod mcp_client;
pub mod mcp_client_set;
mod mcp_http_auth;
pub mod mcp_readiness;
pub mod metadata_panel;
pub mod note_render;
pub mod notes;
pub mod notes_config;
pub mod notes_template;
pub mod oauth;
mod oauth_discovery;
mod oauth_http;
pub mod oauth_refresh;
pub mod overview_panels;
pub mod package_bundle;
pub mod package_files;
mod package_futures;
pub mod package_graph;
pub mod package_keep;
pub mod package_listing;
pub mod package_op_error;
pub mod package_release_manifest;
pub mod package_removal;
pub mod package_report;
pub mod package_retention;
pub mod package_standing;
pub mod package_store;
pub mod package_store_report;
pub mod package_stores;
pub(crate) mod page_job;
pub(crate) mod page_runs;
pub mod panel_side;
pub mod profile_config;
pub mod profile_docs;
pub mod profile_entities;
pub mod proxy;
pub mod recently_accessed;
pub mod register_records;
pub mod release_basis;
#[cfg(test)]
mod release_fixture;
mod release_record;
mod release_views;
pub mod rendering;
pub mod retention;
#[cfg(test)]
mod retention_fixture;
mod retention_job_files;
pub mod routes;
pub mod run_bookkeeping;
pub mod run_claim;
pub(crate) mod run_control;
pub(crate) mod run_control_shared;
pub(crate) mod run_control_vault;
pub(crate) mod run_records;
pub mod run_report;
mod run_scope;
pub mod run_settings;
mod run_snapshot;
pub mod run_summary;
pub mod saved_jobs;
pub mod scheduler;
pub mod screen_routes;
pub mod search;
pub mod service_identity;
pub mod shared_claim;
pub mod shared_index;
pub mod shared_job_authoring;
pub mod shared_jobs;
pub mod shared_ledger;
pub mod shared_lifecycle;
pub mod shared_run;
pub mod shared_runner;
pub mod shared_runs;
pub mod shared_scenarios;
pub mod shared_scheduler;
pub mod shared_snapshot;
pub mod shared_state;
pub mod shared_validation;
pub mod shared_version;
pub mod signals_attention;
pub mod signals_event;
pub mod signals_load;
pub(crate) mod signals_pager;
pub mod signals_query;
pub mod signals_view;
pub mod slug;
#[cfg(test)]
mod snapshot_hooks;
pub mod source_install_error;
#[cfg(test)]
pub(crate) mod source_install_probe;
pub mod source_install_report;
#[cfg(test)]
pub(crate) mod source_install_world;
pub(crate) mod source_overlay;
pub(crate) mod source_read;
pub(crate) mod source_release;
pub mod static_assets;
mod stdio_transport;
#[cfg(test)]
mod store_parity;
pub mod store_retention;
pub mod successor_interface;
pub mod successor_plan;
pub mod successor_report;
pub mod successor_service;
pub mod successor_stores;
pub mod task_folder;
pub mod task_sink;
#[cfg(test)]
pub(crate) mod task_sink_fake;
pub mod tasks_config;
pub mod team;
pub(crate) mod team_notice;
mod templater;
#[cfg(test)]
pub(crate) mod test_support;
pub mod token_exchange;
pub mod tool_result;
mod trailing_path;
pub mod tree_view;
pub mod urls;
pub mod vault_bundle_install;
pub mod vault_catalogue;
pub mod vault_content;
pub mod vault_info;
pub mod vault_job_authoring;
pub mod vault_job_release;
mod vault_library_source;
pub mod vault_links;
pub mod vault_management_config;
pub mod vault_package_store;
pub mod vault_packages;
pub mod vault_relative_path;
pub mod vault_release_snapshot;
pub mod vault_source_install;
pub(crate) mod vault_source_libraries;
pub(crate) mod vault_source_plan;
pub mod vault_source_read;
pub mod vault_time;
pub mod vault_tree;
pub mod vault_workflow_files;
mod vault_workflow_script;
pub mod vault_write;
pub mod wiki;
pub mod wiki_config;
pub mod workflow_releases;
pub mod workflow_releases_error;
pub(crate) mod workflow_releases_query;
pub mod workflow_releases_report;
pub mod workflow_releases_stores;

pub use app::{
    build_router, build_router_at, build_router_in, build_router_with, build_router_with_shutdown, connect,
    shared_router,
};
pub use apps::{AppKind, AppStatus, RegisteredApp, discover_apps};
pub use config::{McpServerConfig, WebConfig, WebConfigError, WebLogLevel, WebServerConfig, load_web_config};
pub use config_interview::{
    TerminalWebInterviewer, WebInterviewEnvironment, WebInterviewError, WebInterviewReport, WebInterviewer,
    run_web_config_interview,
};
pub use config_writer::{WebConfigDocument, WebConfigWriterError, WebServerSettingsInput, WebServerSettingsView};
pub use contextos_workflows::Clock;
pub use entities::{EntitiesState, EntityEntry, load_entities};
pub use inbox_outbox::{
    AncestorRow, DirectoryFolder, DirectoryItem, DirectoryListing, InboxLocation, ancestor_rows, format_modified,
    format_size, list_directory, list_existing_directory, pending_item_path, resolve_inbox_location,
    resolve_outbox_directory,
};
pub use inbox_outbox_config::{INBOX_OUTBOX_TOML_PATH, InboxOutboxConfig, load_inbox_outbox_config};
pub use jobs::{RegisteredJob, WebMcpCaller, discover_jobs};
pub use journal::{
    Day, LoadEntryError, SaveOutcome, WriteEntryError, append_task_event, append_task_event_best_effort, entry_dates,
    load_day, save_section,
};
pub use journal_config::{JOURNAL_TOML_PATH, JournalConfig, TaskEvent, load_journal_config};
pub use journal_entry::{
    EntryInput, EntrySection, JournalEntry, SectionEdit, SectionEditRefused, SectionFingerprint, same_heading,
};
pub use mcp_client::{McpCallError, McpClient, McpClientSet, McpConnectError};
pub use tasks_config::{HierarchyConfig, StageEntry, TasksConfig, load_tasks_config};
pub use team::{MemberState, TeamMember, TeamMemberKind, load_team};

mod job_runs;
mod job_runs_view;
mod nav_counts;
pub mod open_count_cache;
mod vault_health;
mod vault_health_cache;
mod vault_health_mcp;
mod vault_health_releases;
mod vault_health_view;
pub mod vault_runs;
mod vault_state;
pub use vault_state::FavouriteError;
mod view_favourites;
