//! Per-domain tool-router modules. Each declares its own
//! `#[tool_router(router = <name>)]` on an `impl ContextOsServer` block;
//! [`crate::server::ContextOsServer::effective_catalogue`] merges every
//! named router into the one dispatch table.

pub(crate) mod agents;
pub(crate) mod base_query;
pub(crate) mod base_record;
pub(crate) mod base_record_template;
pub(crate) mod base_sidecar;
pub(crate) mod diagnostics;
pub(crate) mod doctor;
mod doctor_projection;
pub(crate) mod ephemeris;
pub(crate) mod frontmatter_link;
pub(crate) mod frontmatter_link_types;
pub(crate) mod fs;
pub(crate) mod fs_expand;
pub(crate) mod fs_read_types;
pub(crate) mod fs_types;
pub(crate) mod git;
pub(crate) mod index_status;
pub(crate) mod links_read;
pub(crate) mod mermaid;
pub(crate) mod note_body;
pub(crate) mod note_entity;
pub(crate) mod obsidian;
pub(crate) mod obsidian_types;
pub(crate) mod query;
pub(crate) mod query_blended;
pub(crate) mod query_controls;
pub(crate) mod query_links;
pub(crate) mod vault;
