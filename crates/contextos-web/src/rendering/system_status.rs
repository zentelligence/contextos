//! The shell's MCP connectivity indicators: the header badge and the footer's MCP servers item. Both come from one
//! [`McpStatus`], so they always agree, and both are the same markup whether they are rendered with the page or
//! pushed later by the poll (`GET /shell/status`), so a server that drops after the page loaded is shown without a
//! reload. Not being able to reach a server is the state they exist to show, so nothing here reads as healthy unless
//! every configured server answered, and having none configured is offline, not healthy.

use askama::Template;

use super::page::McpStatus;

/// How much of the configured MCP servers is answering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Connectivity {
    Operational,
    Degraded,
    Offline,
}

impl From<McpStatus> for Connectivity {
    fn from(status: McpStatus) -> Self {
        if status.connected == 0 {
            Self::Offline
        } else if status.connected < status.total {
            Self::Degraded
        } else {
            Self::Operational
        }
    }
}

impl Connectivity {
    /// The word on the header badge.
    #[must_use]
    pub fn badge_label(self) -> &'static str {
        match self {
            Self::Operational => "LIVE",
            Self::Degraded => "DEGRADED",
            Self::Offline => "OFFLINE",
        }
    }

    /// The colour class of the header badge.
    #[must_use]
    pub fn badge_class(self) -> &'static str {
        match self {
            Self::Operational => "status-ok",
            Self::Degraded => "status-degraded",
            Self::Offline => "status-offline",
        }
    }

    /// The extra class on the footer's status dot; none while everything is well.
    #[must_use]
    pub fn dot_class(self) -> &'static str {
        match self {
            Self::Operational => "",
            Self::Degraded => "attention",
            Self::Offline => "error",
        }
    }
}

impl McpStatus {
    #[must_use]
    pub fn state(self) -> Connectivity {
        Connectivity::from(self)
    }

    /// What the connectivity is, in words.
    #[must_use]
    pub fn summary(self) -> String {
        let plural = |count: usize| if count == 1 { "server" } else { "servers" };
        match self.state() {
            Connectivity::Operational if self.total == 1 => "1 MCP server connected".to_owned(),
            Connectivity::Operational => format!("All {} MCP servers connected", self.total),
            Connectivity::Degraded => {
                format!(
                    "Only {} of {} MCP {} connected",
                    self.connected,
                    self.total,
                    plural(self.total)
                )
            }
            Connectivity::Offline if self.total == 0 => "No MCP servers configured".to_owned(),
            Connectivity::Offline => format!("No MCP servers connected (0 of {})", self.total),
        }
    }
}

#[derive(Template)]
#[template(path = "system_status_badge.html")]
struct BadgeTemplate {
    status: McpStatus,
    oob: bool,
}

#[derive(Template)]
#[template(path = "system_status_footer_item.html")]
struct FooterItemTemplate {
    status: McpStatus,
    oob: bool,
}

const RENDER_FAILED: &str = "<span class=\"hint error\">System status could not be displayed.</span>";

/// The header badge. `oob` marks it to replace the element already on the page.
#[must_use]
pub fn render_badge(status: McpStatus, oob: bool) -> String {
    BadgeTemplate { status, oob }
        .render()
        .unwrap_or_else(|_| RENDER_FAILED.to_owned())
}

/// The footer's MCP servers item. `oob` marks it to replace the element already on the page.
#[must_use]
pub fn render_footer_item(status: McpStatus, oob: bool) -> String {
    FooterItemTemplate { status, oob }
        .render()
        .unwrap_or_else(|_| RENDER_FAILED.to_owned())
}

/// Both elements, each replacing its counterpart on the page, for the poll's response.
#[must_use]
pub fn render_fragment(status: McpStatus) -> String {
    format!("{}{}", render_badge(status, true), render_footer_item(status, true))
}

#[cfg(test)]
#[path = "system_status_test.rs"]
mod tests;
