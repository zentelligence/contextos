//! Helpers shared by this crate's own unit tests.

/// A loopback URL nothing is listening on: a port just bound and released, which the OS refuses at once. A fixed low
/// port such as 1 is silently dropped by some sandboxed networks instead, turning a fast refusal into a long connect
/// timeout. Another process could take the port in the moment between, which no test here depends on.
pub(crate) fn unreachable_loopback_url() -> std::io::Result<String> {
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    Ok(format!("http://127.0.0.1:{port}"))
}
