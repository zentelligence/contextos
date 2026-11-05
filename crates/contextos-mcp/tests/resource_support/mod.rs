//! Shared harness for the resource-capability contract tests
//! (`resource_contract.rs`, `resource_link_contract.rs`,
//! `resource_template_contract.rs`): the in-process MCP transport, URI
//! construction, and the vault configs each group builds on.
//!
//! Not itself a test target: as a `tests/` subdirectory module it is
//! compiled once per including test crate rather than as its own binary.
//! Some helpers are used by only one of the three crates, so each `use`
//! site imports what it needs; `#![allow(dead_code)]` keeps the unused-in-
//! this-crate items from warning.
#![allow(dead_code)]

use contextos_mcp::{Config, ContextOsServer};
use rmcp::ServiceExt;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Connects an in-process MCP client to `server` over a duplex pipe, framed
/// exactly like stdio, mirroring `module_contract.rs`'s own transport
/// harness.
pub async fn connect(
    server: ContextOsServer,
) -> Result<
    (
        rmcp::service::RunningService<rmcp::RoleClient, ()>,
        tokio::task::JoinHandle<Result<(), BoxError>>,
    ),
    BoxError,
> {
    let (server_transport, client_transport) = tokio::io::duplex(65536);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), BoxError>(())
    });
    let client = ().serve(client_transport).await?;
    Ok((client, server_handle))
}

/// Builds a `{name}://{absolute-path}` URI for a test that needs to
/// hand-construct a resource URI reaching outside what `resources/list`
/// would ever itself emit (an escape attempt, or a path the test controls
/// directly by absolute location rather than by vault-relative name). The
/// absolute remainder still has to resolve within the named root
/// (`resolve_within_named_root`), exactly like a plain absolute `path`
/// argument would on any other tool.
pub fn named_uri(name: &str, path: &std::path::Path) -> String {
    format!("{name}://{}", path.display())
}

/// A vault config with an explicit, known `name`, for a test that
/// needs to hand-build a `{name}://` URI deterministically rather than
/// discover the default name-from-basename at runtime. `resources_list_include`
/// is set to `**/*`: every test using this helper cares about
/// `resources/read`, `resources/templates/list`, or URI construction, never
/// about `resources/list`'s own narrowed enumeration scope, which has its
/// own dedicated tests using bespoke configs instead.
#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "Debug quoting/escaping is required to embed the path as a valid TOML string"
)]
pub fn named_vault_config(vault: &std::path::Path) -> Result<Config, BoxError> {
    Ok(Config::try_from(
        format!(
            "[[vault]]\npath = {vault:?}\nname = \"vault\"\nstate_directory = \".contextos\"\n\
             resources_list_include = [\"**/*\"]\n"
        )
        .as_str(),
    )?)
}

/// As [`named_vault_config`], but without an explicit `name` (so the
/// resource-URI scheme defaults to the root directory's basename), for a
/// test that only needs `resources/list` to actually enumerate files, not a
/// deterministic name.
#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "Debug quoting/escaping is required to embed the path as a valid TOML string"
)]
pub fn config_allowing_all_resources(vault: &std::path::Path) -> Result<Config, BoxError> {
    Ok(Config::try_from(
        format!(
            "[[vault]]\npath = {vault:?}\nstate_directory = \".contextos\"\n\
             resources_list_include = [\"**/*\"]\n"
        )
        .as_str(),
    )?)
}

/// As [`named_vault_config`], but without any `resources_list_include` (its
/// empty default left in place), for a test that needs a deterministic
/// `{name}://` URI while still proving the unconfigured `resources/list`
/// behaviour.
#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "Debug quoting/escaping is required to embed the path as a valid TOML string"
)]
pub fn named_vault_config_without_resources_list(vault: &std::path::Path) -> Result<Config, BoxError> {
    Ok(Config::try_from(
        format!("[[vault]]\npath = {vault:?}\nname = \"vault\"\nstate_directory = \".contextos\"\n").as_str(),
    )?)
}
