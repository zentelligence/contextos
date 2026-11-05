use super::*;

#[test]
fn extension_of_returns_the_lowercase_extension() {
    assert_eq!(extension_of("notes/example.md"), "md");
    assert_eq!(extension_of("registry/apps/tasks.base"), "base");
}

#[test]
fn extension_of_returns_empty_for_a_path_with_no_extension() {
    assert_eq!(extension_of("notes/example"), "");
}

#[test]
fn extension_of_ignores_dots_in_earlier_path_segments() {
    assert_eq!(extension_of("v1.2/notes/example.canvas"), "canvas");
}

fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.headers().get(name).and_then(|value| value.to_str().ok())
}

#[test]
fn a_vault_file_that_cannot_run_script_is_not_sandboxed() {
    for content_type in [
        "image/png",
        "application/pdf",
        "text/plain",
        "application/json",
        "text/csv",
    ] {
        let response = asset_response(content_type.to_owned(), AttachedBody::Bytes(vec![0, 1]));
        assert_eq!(header(&response, "content-security-policy"), None, "{content_type}");
    }
}

#[test]
fn a_vault_file_is_never_sniffed_as_another_type() {
    let response = asset_response("image/png".to_owned(), AttachedBody::Bytes(vec![0, 1]));
    assert_eq!(header(&response, "x-content-type-options"), Some("nosniff"));
    assert_eq!(header(&response, "content-security-policy"), None);
}

#[test]
fn a_vault_file_that_could_run_script_is_served_sandboxed() {
    for content_type in [
        "image/svg+xml",
        "text/html; charset=utf-8",
        "application/xhtml+xml",
        "application/xml",
        "text/xml",
        "IMAGE/SVG+XML",
        // Every XML document type a browser renders as XML, which runs XHTML-namespaced script.
        "application/rss+xml",
        "application/atom+xml",
        "application/xslt+xml",
        "application/mathml+xml",
        "text/xsl",
    ] {
        let response = asset_response(content_type.to_owned(), AttachedBody::Text("<svg/>".to_owned()));
        let policy = header(&response, "content-security-policy").unwrap_or_default();
        assert!(policy.starts_with("sandbox;"), "{content_type}: {policy}");
        assert!(policy.contains("default-src 'none'"), "{content_type}: {policy}");
        assert_eq!(
            header(&response, "x-content-type-options"),
            Some("nosniff"),
            "{content_type}"
        );
    }
}
