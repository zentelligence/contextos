use super::*;

#[test]
fn bearer_token_matches_scheme_case_insensitively() {
    assert_eq!(bearer_token("Bearer abc"), Some("abc"));
    assert_eq!(bearer_token("bearer abc"), Some("abc"));
    assert_eq!(bearer_token("BEARER   abc  "), Some("abc"));
    assert_eq!(bearer_token("Basic abc"), None);
    assert_eq!(bearer_token("abc"), None);
}

#[test]
fn auth_guard_without_a_token_accepts_every_request() {
    let guard = AuthGuard::from_token("");
    let headers = axum::http::HeaderMap::new();
    assert!(guard.reject(&headers).is_none());
}

#[test]
fn auth_guard_rejects_a_wrong_token() {
    let guard = AuthGuard::from_token("correct-token");
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        axum::http::HeaderValue::from_static("Bearer wrong-token"),
    );
    assert!(guard.reject(&headers).is_some());
}

#[test]
fn auth_guard_accepts_the_configured_token() {
    let guard = AuthGuard::from_token("correct-token");
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        axum::http::HeaderValue::from_static("Bearer correct-token"),
    );
    assert!(guard.reject(&headers).is_none());
}

#[test]
fn validate_bind_accepts_loopback_without_a_token() -> Result<(), Box<dyn std::error::Error>> {
    validate_bind("127.0.0.1:3737", "")?;
    Ok(())
}

#[test]
fn validate_bind_refuses_non_loopback_without_a_token() {
    let result = validate_bind("0.0.0.0:3737", "");
    assert!(matches!(
        result,
        Err(HttpTransportError::NonLoopbackBindWithoutToken { .. })
    ));
}

#[test]
fn validate_bind_accepts_non_loopback_with_a_token() -> Result<(), Box<dyn std::error::Error>> {
    validate_bind("0.0.0.0:3737", "configured-token")?;
    Ok(())
}

#[test]
fn validate_bind_rejects_an_unparseable_bind() {
    let result = validate_bind("not-a-socket-address", "");
    assert!(matches!(result, Err(HttpTransportError::InvalidBindAddress { .. })));
}

#[tokio::test]
async fn listening_on_an_address_another_process_holds_says_so_and_how_to_find_it()
-> Result<(), Box<dyn std::error::Error>> {
    let held = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let bind = held.local_addr()?.to_string();

    let error = listen(&bind, "")
        .await
        .err()
        .ok_or("a held address should be refused")?;

    assert!(
        matches!(&error, HttpTransportError::AddressInUse { bind: refused } if *refused == bind),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(message.contains(&bind), "{message}");
    assert!(message.contains("contextos service status"), "{message}");
    assert!(message.contains("--http"), "{message}");
    Ok(())
}

#[tokio::test]
async fn listening_validates_the_bind_before_asking_the_system_for_it() -> Result<(), Box<dyn std::error::Error>> {
    let error = listen("0.0.0.0:0", "")
        .await
        .err()
        .ok_or("a non-loopback bind without a token should be refused")?;

    assert!(
        matches!(error, HttpTransportError::NonLoopbackBindWithoutToken { .. }),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn listening_on_a_free_loopback_address_returns_its_listener() -> Result<(), Box<dyn std::error::Error>> {
    let listener = listen("127.0.0.1:0", "").await?;

    assert!(listener.local_addr()?.ip().is_loopback());
    Ok(())
}
