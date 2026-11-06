use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use super::*;
use crate::app::build_router;
use crate::mcp_client::McpClientSet;

#[test]
fn loopback_names_with_or_without_a_port_are_accepted() {
    for host in [
        "localhost",
        "LOCALHOST:7373",
        "127.0.0.1",
        "127.0.0.1:7373",
        "127.1.2.3:80",
        "[::1]",
        "[::1]:7373",
    ] {
        assert!(LoopbackHost::try_from(host).is_ok(), "{host}");
    }
}

#[test]
fn every_other_name_is_refused() {
    for host in [
        "attacker.example",
        "attacker.example:7373",
        "localhost.attacker.example",
        "192.168.1.10:7373",
        "0.0.0.0:7373",
        "[::2]:7373",
        "[::1",
        "",
    ] {
        assert!(LoopbackHost::try_from(host).is_err(), "{host}");
    }
}

async fn settings_status(host: Option<&str>) -> Result<(StatusCode, String), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    std::fs::write(
        dir.path().join("web.toml"),
        "[server]\nbind = \"127.0.0.1:7373\"\n\n[[mcp_server]]\nname = \"contextos\"\ntransport = \"stdio\"\ncommand = \"contextos\"\n",
    )?;
    let clients = Arc::new(McpClientSet::connect(&[]).await?);
    let router = build_router(clients, Some(dir.path()), &dir.path().join("web.toml"));
    let mut request = Request::builder().uri("/server/settings/");
    if let Some(host) = host {
        request = request.header("host", host);
    }
    let response = router.oneshot(request.body(Body::empty())?).await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

#[tokio::test]
async fn a_request_naming_another_site_is_refused_before_any_route_runs() -> Result<(), Box<dyn std::error::Error>> {
    let (status, body) = settings_status(Some("attacker.example:7373")).await?;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("route/host-not-allowed"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_request_naming_a_loopback_host_or_none_is_served() -> Result<(), Box<dyn std::error::Error>> {
    for host in [Some("127.0.0.1:7373"), Some("localhost:7373"), None] {
        let (status, body) = settings_status(host).await?;

        assert_eq!(status, StatusCode::OK, "{host:?}: {body}");
    }
    Ok(())
}
