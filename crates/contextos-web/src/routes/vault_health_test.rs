use axum::http::HeaderValue;

use super::*;

fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(*name, HeaderValue::from_static(value));
    }
    map
}

#[test]
fn a_request_with_no_origin_information_is_not_from_a_browser_page_and_is_allowed() {
    assert!(from_own_origin(&headers(&[("host", "localhost:8080")])));
}

#[test]
fn a_same_origin_fetch_is_allowed() {
    assert!(from_own_origin(&headers(&[
        ("host", "localhost:8080"),
        ("sec-fetch-site", "same-origin"),
    ])));
}

#[test]
fn a_cross_site_or_same_site_fetch_is_refused_even_with_a_matching_origin() {
    for site in ["cross-site", "same-site", "none", ""] {
        let mut map = headers(&[("host", "localhost:8080"), ("origin", "http://localhost:8080")]);
        map.insert("sec-fetch-site", HeaderValue::from_static(site));

        assert!(!from_own_origin(&map), "{site:?} must be refused");
    }
}

#[test]
fn an_origin_matching_the_host_is_allowed_whatever_its_case() {
    assert!(from_own_origin(&headers(&[
        ("host", "Localhost:8080"),
        ("origin", "http://localhost:8080"),
    ])));
}

#[test]
fn an_origin_naming_another_site_is_refused() {
    assert!(!from_own_origin(&headers(&[
        ("host", "localhost:8080"),
        ("origin", "https://evil.example"),
    ])));
    assert!(!from_own_origin(&headers(&[
        ("host", "localhost:8080"),
        ("origin", "http://localhost:9090"),
    ])));
}

#[test]
fn an_opaque_or_malformed_origin_is_refused() {
    assert!(!from_own_origin(&headers(&[
        ("host", "localhost:8080"),
        ("origin", "null"),
    ])));
    assert!(!from_own_origin(&headers(&[("origin", "http://localhost:8080")])));
}
