use super::*;

fn with_cookie(value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        HeaderValue::from_str(value).unwrap_or(HeaderValue::from_static("")),
    );
    headers
}

#[test]
fn the_cookie_is_found_among_the_others_and_decoded() {
    let headers = with_cookie("cc_sidebar=wide; cc_active_vault=my%20vault; cc_last_visited=x");
    assert_eq!(read_from_headers(&headers).as_deref(), Some("my vault"));
}

#[test]
fn no_cookie_or_an_empty_one_is_no_active_vault() {
    assert_eq!(read_from_headers(&HeaderMap::new()), None);
    assert_eq!(read_from_headers(&with_cookie("cc_active_vault=")), None);
    assert_eq!(read_from_headers(&with_cookie("other=alpha")), None);
}

#[test]
fn a_vaults_address_names_it_and_a_hosts_does_not() {
    assert_eq!(vault_of("/alpha/workflows").as_deref(), Some("alpha"));
    assert_eq!(vault_of("/alpha").as_deref(), Some("alpha"));
    assert_eq!(vault_of("/my%20vault/tasks").as_deref(), Some("my vault"));
    for host in [
        "/",
        "/server/settings/",
        "/server/workflows",
        "/shell/status",
        "/static/x.css",
        "/api/v1/x",
        "/mcp/a/b",
    ] {
        assert_eq!(vault_of(host), None, "{host}");
    }
}

#[test]
fn a_name_that_needs_encoding_survives_the_round_trip() {
    let header = set_cookie_header("my vault").unwrap_or(HeaderValue::from_static(""));
    let text = header.to_str().unwrap_or_default();
    assert!(text.ends_with("; Path=/; SameSite=Lax"), "{text}");
    let pair = text.split(';').next().unwrap_or_default();
    assert_eq!(read_from_headers(&with_cookie(pair)).as_deref(), Some("my vault"));
}

#[test]
fn a_vault_named_as_an_old_host_address_is_a_vault() {
    assert_eq!(vault_of("/shared/workflows").as_deref(), Some("shared"));
    assert_eq!(vault_of("/settings/").as_deref(), Some("settings"));
    assert_eq!(vault_of("/server/settings/"), None);
}
