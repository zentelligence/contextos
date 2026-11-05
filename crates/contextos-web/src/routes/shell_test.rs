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
