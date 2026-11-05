//! Refuses a request that names any host but a loopback one, `contextos-web`'s defence against DNS rebinding.
//!
//! `contextos-web` binds only to loopback and has no authentication, so a page on another site that makes its own
//! name resolve to `127.0.0.1` would otherwise reach every route as same-origin: read `/settings/`, reveal a stored
//! token, or rewrite an `[[mcp_server]]` command. The browser still sends that site's name in `Host`, so naming a
//! loopback host (`localhost`, a `127.0.0.0/8` address, or `[::1]`, with any port) is required. A request with no
//! `Host` at all is let through: every browser sends one, so only a local non-browser client can omit it.

use std::net::IpAddr;

use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

/// A `Host` header value naming a loopback host, with or without a port.
pub(crate) struct LoopbackHost;

impl TryFrom<&str> for LoopbackHost {
    type Error = ();

    fn try_from(host: &str) -> Result<Self, Self::Error> {
        let name = match host.strip_prefix('[') {
            // `[::1]` or `[::1]:7373`: the address sits between the brackets.
            Some(bracketed) => bracketed.split_once(']').map(|(address, _port)| address).ok_or(())?,
            None => host.rsplit_once(':').map_or(host, |(name, _port)| name),
        };
        let loopback =
            name.eq_ignore_ascii_case("localhost") || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
        if loopback { Ok(Self) } else { Err(()) }
    }
}

/// Whole-router middleware: answers `403` `route/host-not-allowed` for a `Host` that is not a loopback name.
pub(crate) async fn refuse_other_hosts(request: Request, next: Next) -> Response {
    let refused = request
        .headers()
        .get(header::HOST)
        .is_some_and(|host| host.to_str().map_or(true, |host| LoopbackHost::try_from(host).is_err()));
    if refused {
        tracing::warn!(
            "refused a request naming a host other than loopback; a page on another site may be rebinding its \
             name to this address"
        );
        return crate::routes::vault::error_response(StatusCode::FORBIDDEN, "route/host-not-allowed");
    }
    next.run(request).await
}

#[cfg(test)]
#[path = "loopback_host_test.rs"]
mod tests;
