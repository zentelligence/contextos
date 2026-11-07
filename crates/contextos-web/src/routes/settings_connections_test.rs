use super::*;

fn view(readiness: Readiness) -> ConnectionView {
    ConnectionView::from(&ServerReadiness {
        name: "my server".to_owned(),
        readiness,
    })
}

fn rendered(readiness: Readiness) -> Result<String, askama::Error> {
    let view = view(readiness);
    ConnectionTemplate { connection: &view }.render()
}

#[test]
fn a_connected_server_shows_its_tool_count_and_offers_reconnect() -> Result<(), askama::Error> {
    let html = rendered(Readiness::Connected { tool_count: 46 })?;

    assert!(html.contains("conn-dot conn-ok"), "{html}");
    assert!(html.contains("Connected, 46 tools"), "{html}");
    assert!(
        html.contains(r#"hx-post="/server/settings/mcp-servers/my%20server/reconnect""#),
        "{html}"
    );
    assert!(!html.contains("hx-trigger"), "a settled panel does not poll: {html}");
    Ok(())
}

#[test]
fn a_connecting_server_polls_its_panel_and_offers_no_reconnect_meanwhile() -> Result<(), askama::Error> {
    let html = rendered(Readiness::Connecting)?;

    assert!(html.contains("conn-dot conn-pending"), "{html}");
    assert!(
        html.contains(r#"hx-get="/server/settings/mcp-servers/my%20server/connection""#),
        "{html}"
    );
    assert!(html.contains(r#"hx-trigger="load delay:1s""#), "{html}");
    assert!(!html.contains(">Reconnect<"), "{html}");
    Ok(())
}

#[test]
fn a_failed_server_shows_its_cause_and_what_to_do() -> Result<(), askama::Error> {
    let html = rendered(Readiness::Failed(ConnectionFailure {
        cause: FailureCause::NotStarted,
        detail: "failed to spawn MCP server \"my server\" (command \"my-command\"): no such file".to_owned(),
    }))?;

    assert!(html.contains("conn-dot conn-down"), "{html}");
    assert!(html.contains("Not connected"), "{html}");
    assert!(html.contains("no such file"), "{html}");
    assert!(html.contains("Check the command and its arguments"), "{html}");
    assert!(html.contains(">Reconnect<"), "{html}");
    Ok(())
}

#[test]
fn an_unset_token_variable_says_a_restart_is_needed_for_it_or_a_pasted_token_instead() {
    let view = view(Readiness::Failed(ConnectionFailure {
        cause: FailureCause::TokenEnvUnset {
            variable: "REMOTE_TOKEN".to_owned(),
        },
        detail: String::new(),
    }));

    let remedy = view.remedy.unwrap_or_default();
    assert!(remedy.contains("REMOTE_TOKEN"), "{remedy}");
    assert!(remedy.contains("restart contextos-web"), "{remedy}");
    assert!(remedy.contains("paste a token"), "{remedy}");
}

#[test]
fn an_oauth2_server_needing_a_login_offers_sign_in() -> Result<(), askama::Error> {
    let html = rendered(Readiness::LoginRequired(LoginRecovery::OauthConnect))?;

    assert!(html.contains("conn-dot conn-attention"), "{html}");
    assert!(html.contains("Login required"), "{html}");
    assert!(
        html.contains(r#"hx-post="/server/settings/oauth/my%20server/connect">Sign in<"#),
        "{html}"
    );
    Ok(())
}

#[test]
fn a_bearer_server_needing_a_token_points_at_the_entry_rather_than_a_sign_in() -> Result<(), askama::Error> {
    let html = rendered(Readiness::LoginRequired(LoginRecovery::ReplaceBearerToken))?;

    assert!(html.contains("Paste a new token under Edit"), "{html}");
    assert!(!html.contains(">Sign in<"), "{html}");
    Ok(())
}

#[test]
fn an_unreachable_server_is_told_to_reconnect() {
    let view = view(Readiness::Unreachable);

    assert_eq!(view.label, "Unreachable");
    assert!(view.remedy.unwrap_or_default().contains("Reconnect"));
}

#[test]
fn an_entry_the_sessions_do_not_know_reads_as_not_connected_with_reconnect_to_connect_it() {
    let view = ConnectionView::unknown_to_set("added by hand");

    assert_eq!(view.label, "Not connected");
    assert!(!view.connecting);
    assert!(view.remedy.unwrap_or_default().contains("Reconnect to connect it"));
}

#[test]
fn a_server_name_is_one_path_segment_however_it_is_spelt() {
    assert_eq!(ServerSegment::from("contextos").to_string(), "contextos");
    assert_eq!(ServerSegment::from("docs.local").to_string(), "docs.local");
    assert_eq!(ServerSegment::from("team/a b").to_string(), "team%2Fa%20b");
}

#[test]
fn the_reconnect_button_replaces_its_own_panel_whatever_the_server_is_called() -> Result<(), askama::Error> {
    let view = ConnectionView::from(&ServerReadiness {
        name: "docs.local/a b".to_owned(),
        readiness: Readiness::Unreachable,
    });
    let html = ConnectionTemplate { connection: &view }.render()?;

    assert!(html.contains(r#"hx-target="closest .mcp-server-connection""#), "{html}");
    assert!(
        html.contains(r#"hx-post="/server/settings/mcp-servers/docs.local%2Fa%20b/reconnect""#),
        "{html}"
    );
    Ok(())
}
