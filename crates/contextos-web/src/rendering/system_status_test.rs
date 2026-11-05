use super::*;

fn status(connected: usize, total: usize, tools: usize) -> McpStatus {
    McpStatus {
        connected,
        total,
        tools,
    }
}

#[test]
fn every_server_connected_is_operational() {
    assert_eq!(Connectivity::from(status(2, 2, 40)), Connectivity::Operational);
    assert_eq!(Connectivity::from(status(1, 1, 20)), Connectivity::Operational);
}

#[test]
fn some_but_not_all_servers_connected_is_degraded() {
    assert_eq!(Connectivity::from(status(1, 3, 20)), Connectivity::Degraded);
}

#[test]
fn no_server_connected_is_offline_and_so_is_having_none_configured() {
    assert_eq!(Connectivity::from(status(0, 2, 0)), Connectivity::Offline);
    assert_eq!(Connectivity::from(status(0, 0, 0)), Connectivity::Offline);
}

#[test]
fn the_badge_and_dot_change_with_the_state() {
    assert_eq!(Connectivity::Operational.badge_label(), "LIVE");
    assert_eq!(Connectivity::Degraded.badge_label(), "DEGRADED");
    assert_eq!(Connectivity::Offline.badge_label(), "OFFLINE");
    assert_eq!(Connectivity::Operational.dot_class(), "");
    assert_eq!(Connectivity::Degraded.dot_class(), "attention");
    assert_eq!(Connectivity::Offline.dot_class(), "error");
}

#[test]
fn the_summary_says_what_is_wrong_in_words() {
    assert_eq!(status(1, 1, 5).summary(), "1 MCP server connected");
    assert_eq!(status(3, 3, 5).summary(), "All 3 MCP servers connected");
    assert_eq!(status(1, 3, 5).summary(), "Only 1 of 3 MCP servers connected");
    assert_eq!(status(0, 2, 0).summary(), "No MCP servers connected (0 of 2)");
    assert_eq!(status(0, 0, 0).summary(), "No MCP servers configured");
}

#[test]
fn the_badge_carries_its_state_and_an_accessible_summary() {
    let html = render_badge(status(1, 3, 5), false);

    assert!(html.contains("live-badge status-degraded"), "{html}");
    assert!(html.contains("DEGRADED"), "{html}");
    assert!(
        html.contains("System status: Only 1 of 3 MCP servers connected"),
        "{html}"
    );
    assert!(html.contains("id=\"system-status-badge\""), "{html}");
    assert!(!html.contains("hx-swap-oob"), "{html}");
}

#[test]
fn the_footer_item_shows_the_dot_for_its_state() {
    let ok = render_footer_item(status(2, 2, 10), false);
    let down = render_footer_item(status(0, 2, 0), false);

    assert!(!ok.contains("attention") && !ok.contains("error"), "{ok}");
    assert!(ok.contains("2 of 2 connected"), "{ok}");
    assert!(down.contains("status-dot error"), "{down}");
    assert!(down.contains("0 of 2 connected"), "{down}");
    assert!(down.contains("id=\"system-status-mcp\""), "{down}");
}

#[test]
fn a_polled_update_replaces_both_elements_out_of_band() {
    let html = render_fragment(status(0, 1, 0));

    assert_eq!(html.matches("hx-swap-oob=\"outerHTML\"").count(), 2, "{html}");
    assert!(html.contains("OFFLINE"), "{html}");
    assert!(html.contains("status-dot error"), "{html}");
}
