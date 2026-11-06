use axum::http::{HeaderMap, HeaderValue, header};

use super::*;

type BoxError = Box<dyn std::error::Error>;

fn headers_with_referer(referer: &str) -> Result<HeaderMap, BoxError> {
    let mut headers = HeaderMap::new();
    headers.insert(header::REFERER, HeaderValue::from_str(referer)?);
    Ok(headers)
}

fn header_str(response: &Response, name: axum::http::HeaderName) -> Result<&str, BoxError> {
    Ok(response
        .headers()
        .get(name)
        .ok_or("expected header missing")?
        .to_str()?)
}

#[tokio::test]
async fn toggling_from_wide_sets_the_cookie_to_narrow() -> Result<(), BoxError> {
    let response = toggle(HeaderMap::new()).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = header_str(&response, header::SET_COOKIE)?;
    assert!(cookie.starts_with("cc_sidebar=narrow"));
    Ok(())
}

#[tokio::test]
async fn toggling_from_narrow_sets_the_cookie_to_wide() -> Result<(), BoxError> {
    let mut headers = HeaderMap::new();
    headers.insert(header::COOKIE, HeaderValue::from_static("cc_sidebar=narrow"));

    let response = toggle(headers).await;

    let cookie = header_str(&response, header::SET_COOKIE)?;
    assert!(cookie.starts_with("cc_sidebar=wide"));
    Ok(())
}

#[tokio::test]
async fn redirects_back_to_a_same_origin_referer() -> Result<(), BoxError> {
    let response = toggle(headers_with_referer("/vault/docs/")?).await;

    assert_eq!(header_str(&response, header::LOCATION)?, "/vault/docs/");
    Ok(())
}

#[tokio::test]
async fn an_absolute_or_off_site_referer_falls_back_to_the_root() -> Result<(), BoxError> {
    let response = toggle(headers_with_referer("https://evil.example/vault/")?).await;

    assert_eq!(header_str(&response, header::LOCATION)?, "/");
    Ok(())
}

#[tokio::test]
async fn a_protocol_relative_referer_falls_back_to_the_root() -> Result<(), BoxError> {
    let response = toggle(headers_with_referer("//evil.example/vault/")?).await;

    assert_eq!(header_str(&response, header::LOCATION)?, "/");
    Ok(())
}

#[tokio::test]
async fn a_missing_referer_falls_back_to_the_root() -> Result<(), BoxError> {
    let response = toggle(HeaderMap::new()).await;

    assert_eq!(header_str(&response, header::LOCATION)?, "/");
    Ok(())
}

fn headers_from_a_browser(host: &str, referer: &str) -> Result<HeaderMap, BoxError> {
    let mut headers = headers_with_referer(referer)?;
    headers.insert(header::HOST, HeaderValue::from_str(host)?);
    Ok(headers)
}

// A browser's Referer is always a whole URL, so this is the form the toggle receives in use.
#[tokio::test]
async fn redirects_back_to_the_page_a_browser_toggled_from() -> Result<(), BoxError> {
    let headers = headers_from_a_browser("127.0.0.1:7399", "http://127.0.0.1:7399/server/jobs?state=draft")?;
    let response = toggle(headers).await;
    assert_eq!(header_str(&response, header::LOCATION)?, "/server/jobs?state=draft");
    Ok(())
}

#[tokio::test]
async fn a_referer_from_another_host_falls_back_to_the_root() -> Result<(), BoxError> {
    let headers = headers_from_a_browser("127.0.0.1:7399", "http://evil.example/server/jobs")?;
    let response = toggle(headers).await;
    assert_eq!(header_str(&response, header::LOCATION)?, "/");
    Ok(())
}

#[tokio::test]
async fn a_referer_on_another_port_falls_back_to_the_root() -> Result<(), BoxError> {
    let headers = headers_from_a_browser("127.0.0.1:7399", "http://127.0.0.1:8000/server/jobs")?;
    let response = toggle(headers).await;
    assert_eq!(header_str(&response, header::LOCATION)?, "/");
    Ok(())
}

#[tokio::test]
async fn a_same_host_referer_whose_path_leaves_the_site_falls_back_to_the_root() -> Result<(), BoxError> {
    for referer in [
        "http://127.0.0.1:7399//evil.example/",
        "http://127.0.0.1:7399/\\evil.example/",
    ] {
        let headers = headers_from_a_browser("127.0.0.1:7399", referer)?;
        let response = toggle(headers).await;
        assert_eq!(header_str(&response, header::LOCATION)?, "/", "{referer}");
    }
    Ok(())
}

#[tokio::test]
async fn a_referer_that_is_not_a_web_address_falls_back_to_the_root() -> Result<(), BoxError> {
    let headers = headers_from_a_browser("127.0.0.1:7399", "ftp://127.0.0.1:7399/server/jobs")?;
    let response = toggle(headers).await;
    assert_eq!(header_str(&response, header::LOCATION)?, "/");
    Ok(())
}
