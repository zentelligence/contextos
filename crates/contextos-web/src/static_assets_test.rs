use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use super::service;

fn bundled_static_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/static"))
}

#[tokio::test]
async fn serves_a_file_that_exists_under_a_configured_static_directory() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("hello.txt"), b"hello")?;
    let router = Router::new().nest_service("/static", service(Some(dir.path())));

    let response = router
        .oneshot(Request::builder().uri("/static/hello.txt").body(Body::empty())?)
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn a_traversal_attempt_never_escapes_a_configured_static_directory() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let static_dir = root.path().join("static");
    std::fs::create_dir_all(&static_dir)?;
    std::fs::write(root.path().join("secret.txt"), b"outside the static directory")?;
    let router = Router::new().nest_service("/static", service(Some(&static_dir)));

    let response = router
        .oneshot(Request::builder().uri("/static/../secret.txt").body(Body::empty())?)
        .await?;

    assert_ne!(response.status(), StatusCode::OK);
    Ok(())
}

/// The crate's own bundled assets ship embedded in the binary, so
/// they are servable with no `static_dir` configured at all, matching the
/// v0.20.2 Windows install failure this behaviour fixes (a `cargo install`
/// layout never carries a `static/` directory next to the binary).
#[tokio::test]
async fn serves_a_bundled_asset_with_no_static_dir_configured() -> Result<(), Box<dyn std::error::Error>> {
    let router = Router::new().nest_service("/static", service(None));

    let response = router
        .oneshot(
            Request::builder()
                .uri("/static/contextos-web-client.js")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let expected = std::fs::read(bundled_static_dir().join("contextos-web-client.js"))?;
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(body.as_ref(), expected.as_slice());
    Ok(())
}

/// Every asset a rendered page references unconditionally (the favicon, the
/// stylesheet and its imported token layer, the self-hosted Inter faces and
/// the Lucide icons the stylesheet pulls, their licences, and the default
/// header logo mark) must ship in the bundled set so a fresh install
/// renders a working page rather than broken glyphs and `404`s.
#[tokio::test]
async fn every_unconditionally_referenced_bundled_asset_is_servable() -> Result<(), Box<dyn std::error::Error>> {
    let router = Router::new().nest_service("/static", service(None));

    let paths = [
        "/static/favicon.ico",
        "/static/contextos-web.css",
        "/static/contextos-shell.css",
        "/static/contextos-vault-management.css",
        "/static/contextos-tokens.css",
        "/static/htmx.min.js",
        "/static/htmx.min.js.LICENSE",
        "/static/htmx-ext-sse.js",
        "/static/htmx-ext-sse.js.LICENSE",
        "/static/fonts/inter-latin-400-normal.woff2",
        "/static/fonts/inter-latin-600-normal.woff2",
        "/static/fonts/OFL.txt",
        "/static/icons/folder.svg",
        "/static/icons/settings.svg",
        "/static/icons/LICENSE",
        // The header's default logo mark (`config::DEFAULT_LOGO_FILENAME`):
        // an operator overrides it by adding a differently named file to a
        // configured `static_dir` and pointing `[server.ui] logo` at it,
        // the same pattern every other static-folder override follows.
        "/static/logo.png",
    ];
    for path in paths {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty())?)
            .await?;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{path} should be servable from the bundled set"
        );
    }
    Ok(())
}

/// A served `woff2` face carries the `font/woff2` content type and an
/// `ETag`, and revalidates to `304`, the same cache contract every bundled
/// asset already carries.
#[tokio::test]
async fn a_bundled_font_serves_as_woff2_and_revalidates() -> Result<(), Box<dyn std::error::Error>> {
    let router = Router::new().nest_service("/static", service(None));
    let path = "/static/fonts/inter-latin-400-normal.woff2";

    let first = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    assert_eq!(first.status(), StatusCode::OK);
    let content_type = first
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(content_type.contains("woff2"), "content type was {content_type}");
    let etag = first
        .headers()
        .get(axum::http::header::ETAG)
        .ok_or("a served woff2 must carry an ETag")?
        .clone();

    let second = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(axum::http::header::IF_NONE_MATCH, etag)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    Ok(())
}

/// A bundled asset carries a content-hash `ETag`, and a matching
/// `If-None-Match` on a later request gets a bodyless `304`, not a full
/// re-download: the same cache-revalidation contract a browser gets from
/// `ServeDir` for a configured `static_dir`.
#[tokio::test]
async fn a_bundled_asset_revalidates_via_etag() -> Result<(), Box<dyn std::error::Error>> {
    let router = Router::new().nest_service("/static", service(None));

    let first = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/static/contextos-web-client.js")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first
        .headers()
        .get(axum::http::header::ETAG)
        .ok_or("expected an ETag header on the first response")?
        .clone();

    let second = router
        .oneshot(
            Request::builder()
                .uri("/static/contextos-web-client.js")
                .header(axum::http::header::IF_NONE_MATCH, etag)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    let body = to_bytes(second.into_body(), usize::MAX).await?;
    assert!(body.is_empty());
    Ok(())
}

/// A path neither a configured `static_dir` nor the bundled set contains
/// is a plain `404`, not a panic or an empty-body success.
#[tokio::test]
async fn a_path_absent_from_both_the_static_dir_and_the_bundled_set_is_not_found()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let router = Router::new().nest_service("/static", service(Some(dir.path())));

    let response = router
        .oneshot(
            Request::builder()
                .uri("/static/does-not-exist.txt")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

/// A configured `static_dir` is consulted first: an operator can override a
/// bundled asset (a custom `contextos-web.css`, say) without rebuilding.
#[tokio::test]
async fn a_configured_static_dir_overrides_a_bundled_asset_of_the_same_name() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("contextos-web.css"), b"body { color: red; }")?;
    let router = Router::new().nest_service("/static", service(Some(dir.path())));

    let response = router
        .oneshot(
            Request::builder()
                .uri("/static/contextos-web.css")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(body.as_ref(), b"body { color: red; }");
    Ok(())
}

/// A configured `static_dir` that does not itself carry a bundled asset
/// (`htmx.min.js`, say, an operator only overrode the crate's own CSS)
/// still serves that asset from the embedded fallback, not a `404`.
#[tokio::test]
async fn a_configured_static_dir_falls_back_to_a_bundled_asset_it_does_not_contain()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("contextos-web.css"), b"body { color: red; }")?;
    let router = Router::new().nest_service("/static", service(Some(dir.path())));

    let response = router
        .oneshot(Request::builder().uri("/static/htmx.min.js").body(Body::empty())?)
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let expected = std::fs::read(bundled_static_dir().join("htmx.min.js"))?;
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(body.as_ref(), expected.as_slice());
    Ok(())
}

#[test]
fn the_offered_icon_names_are_exactly_those_the_stylesheet_renders() {
    let names = crate::static_assets::icon_names();
    for expected in ["inbox", "loader", "check", "scale", "history"] {
        assert!(
            names.iter().any(|name| name == expected),
            "{expected} missing from {names:?}"
        );
    }
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]), "sorted and unique");
    assert!(names.iter().all(|name| !name.contains(['{', ' ', '.'])));
}
