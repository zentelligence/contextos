//! The vault root's own screen: the Command Centre Overview dashboard, rendered once per vault root by
//! `routes::vault::render_directory` in place of any `index.md` fall through. A main column of Bridge (the chief of
//! staff), System status, Task lifecycle, and Needs a call from you, beside a rail of the Alert rail and Inputs.
//!
//! Task lifecycle renders with the page (`routes::tasks::dashboard_widgets_html`). Every other panel is a region that
//! loads after the page and reloads itself when the live stream says something may have changed, so one slow or
//! unreadable source never holds up the dashboard: Bridge, System status, and Inputs come from
//! `routes::overview_panels`, Needs a call from you is the Signals screen's attention region, and the Alert rail is the
//! alerts list the top-bar bell opens (`routes::alerts`).

use askama::Template;

use crate::mcp_client::McpClient;

#[derive(Template)]
#[template(path = "vault_overview.html")]
struct VaultOverviewTemplate {
    /// Every URL the template links to within the vault.
    urls: crate::urls::VaultUrls,
    task_lifecycle_html: String,
}

/// Renders the vault root's discrete Overview dashboard. Never fails outright: any downstream MCP failure inside the
/// Task lifecycle panel already renders that one panel empty rather than failing the whole page
/// (`tasks::dashboard_widgets_html`'s own contract); this function mirrors that by falling back to an empty page on
/// a template error rather than taking the vault root down over dashboard chrome.
pub(super) async fn render_html(
    client: &McpClient,
    vault_name: &str,
    entity: Option<&str>,
    vault_time: &crate::vault_time::VaultTime,
) -> String {
    let task_lifecycle_html = Box::pin(super::tasks::dashboard_widgets_html(
        client, vault_name, entity, vault_time,
    ))
    .await;
    let template = VaultOverviewTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        task_lifecycle_html,
    };
    template.render().unwrap_or_default()
}
