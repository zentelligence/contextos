use super::*;

async fn parsed(uri: &str, htmx: bool) -> Result<FileRequest, MalformedQuery> {
    let mut builder = axum::http::Request::builder().uri(uri);
    if htmx {
        builder = builder.header("hx-request", "true");
    }
    let (mut parts, ()) = builder.body(()).unwrap_or_default().into_parts();
    FileRequest::from_request_parts(&mut parts, &()).await
}

async fn request_of(uri: &str, htmx: bool) -> FileRequest {
    parsed(uri, htmx).await.unwrap_or_default()
}

#[tokio::test]
async fn only_raw_equal_to_one_asks_for_the_file() {
    for (uri, raw) in [
        ("/v/wiki/a.md?raw=1", true),
        ("/v/wiki/a.md?view=x&raw=1", true),
        ("/v/wiki/a.md", false),
        ("/v/wiki/a.md?draw=1", false),
    ] {
        assert_eq!(
            parsed(uri, false).await.map(|request| request.raw).ok(),
            Some(Raw(raw)),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn a_raw_other_than_one_or_a_repeated_parameter_is_refused_not_ignored() {
    for uri in [
        "/v/wiki/a.md?raw=0",
        "/v/wiki/a.md?raw=true",
        "/v/wiki/a.md?raw=",
        "/v/wiki/a.md?raw=1&raw=1",
        "/v/notes/a.base?view=A&view=B",
    ] {
        assert_eq!(parsed(uri, true).await.err(), Some(MalformedQuery), "{uri}");
    }
}

#[tokio::test]
async fn only_an_htmx_request_naming_a_view_asks_for_a_base_view() {
    assert_eq!(
        request_of("/v/notes/a.base?view=Open%20now", true)
            .await
            .base_view
            .as_deref(),
        Some("Open now")
    );
    assert_eq!(request_of("/v/notes/a.base?view=Open", false).await.base_view, None);
    assert_eq!(request_of("/v/notes/a.base?view=", true).await.base_view, None);
    assert_eq!(request_of("/v/notes/a.base", true).await.base_view, None);
}

#[tokio::test]
async fn a_query_the_screen_owns_is_left_to_the_screen() {
    assert_eq!(
        request_of("/v/tasks/?q=x&entity=y", false).await,
        FileRequest::default()
    );
}

#[test]
fn raw_text_is_plain_text_that_is_never_sniffed() {
    let response = found(
        "text/html".to_owned(),
        AttachedBody::Text("<script></script>".to_owned()),
        Raw(true),
    );
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(
        response
            .headers()
            .get("x-content-type-options")
            .and_then(|value| value.to_str().ok()),
        Some("nosniff")
    );
}

#[test]
fn a_file_that_is_not_asked_for_raw_keeps_its_type_under_the_sandbox() {
    let response = found(
        "text/html".to_owned(),
        AttachedBody::Text("<p>x</p>".to_owned()),
        Raw(false),
    );
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/html")
    );
    assert!(response.headers().contains_key("content-security-policy"));
}

#[test]
fn a_malformed_query_is_answered_with_a_bad_request() {
    assert_eq!(
        MalformedQuery.into_response().status(),
        axum::http::StatusCode::BAD_REQUEST
    );
}
