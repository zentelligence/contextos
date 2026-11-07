use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;

use super::*;

type BoxError = Box<dyn std::error::Error>;

fn class(address: &str) -> Result<AddressClass, BoxError> {
    Ok(address.parse::<IpAddr>()?.into())
}

#[test]
fn addresses_are_classed_by_the_network_they_belong_to() -> Result<(), BoxError> {
    for (address, expected) in [
        ("8.8.8.8", AddressClass::Public),
        ("2606:4700:4700::1111", AddressClass::Public),
        ("127.0.0.1", AddressClass::Loopback),
        ("127.8.8.8", AddressClass::Loopback),
        ("::1", AddressClass::Loopback),
        ("10.0.0.1", AddressClass::Private),
        ("172.16.0.1", AddressClass::Private),
        ("192.168.1.1", AddressClass::Private),
        ("100.64.0.1", AddressClass::Private),
        ("198.18.0.1", AddressClass::Translated),
        ("fd00::1", AddressClass::Private),
        ("169.254.169.254", AddressClass::Reserved),
        ("0.0.0.0", AddressClass::Reserved),
        ("224.0.0.1", AddressClass::Reserved),
        ("255.255.255.255", AddressClass::Reserved),
        ("100.100.100.200", AddressClass::Reserved),
        ("168.63.129.16", AddressClass::Reserved),
        ("fd00:ec2::254", AddressClass::Reserved),
        ("::ffff:100.100.100.200", AddressClass::Reserved),
        ("192.0.2.1", AddressClass::Reserved),
        ("::", AddressClass::Reserved),
        ("fe80::1", AddressClass::Reserved),
        ("fec0::1", AddressClass::Reserved),
        ("ff02::1", AddressClass::Reserved),
        ("2001:db8::1", AddressClass::Reserved),
    ] {
        assert_eq!(class(address)?, expected, "{address}");
    }
    Ok(())
}

#[test]
fn an_ipv6_address_carrying_an_ipv4_one_is_classed_as_what_it_leads_to() -> Result<(), BoxError> {
    for (address, expected) in [
        ("::ffff:127.0.0.1", AddressClass::Loopback),
        ("::ffff:169.254.169.254", AddressClass::Reserved),
        ("::ffff:8.8.8.8", AddressClass::Public),
        ("64:ff9b::a00:1", AddressClass::Private),
        ("64:ff9b::a9fe:a9fe", AddressClass::Reserved),
        ("2002:7f00:1::", AddressClass::Loopback),
        ("64:ff9b::a83f:8110", AddressClass::Reserved),
        ("2002:a83f:8110::", AddressClass::Reserved),
        ("64:ff9b::6464:64c8", AddressClass::Reserved),
    ] {
        assert_eq!(class(address)?, expected, "{address}");
    }
    Ok(())
}

fn policy(endpoint_class: Option<AddressClass>, https_only: bool) -> AddressPolicy {
    AddressPolicy {
        endpoint_class,
        endpoint_domain: None,
        https_only,
    }
}

fn addresses(found: &[&str]) -> Result<Vec<IpAddr>, BoxError> {
    Ok(found.iter().map(|ip| ip.parse()).collect::<Result<_, _>>()?)
}

// The endpoint's name is the server's to answer, so a public answer beside local ones must grant nothing local.
#[test]
fn a_local_class_is_granted_only_when_the_endpoints_addresses_agree() -> Result<(), BoxError> {
    for (found, expected) in [
        (vec!["127.0.0.1"], Some(AddressClass::Loopback)),
        (vec!["127.0.0.1", "::1"], Some(AddressClass::Loopback)),
        (vec!["10.0.0.1", "192.168.1.1"], Some(AddressClass::Private)),
        (vec!["fe80::1", "192.168.1.1"], Some(AddressClass::Private)),
        (vec!["8.8.8.8"], None),
        (vec!["8.8.8.8", "127.0.0.1"], None),
        (vec!["127.0.0.1", "8.8.8.8"], None),
        (vec!["10.0.0.1", "127.0.0.1"], Some(AddressClass::Private)),
        (vec!["198.18.0.1"], Some(AddressClass::Translated)),
        (vec!["169.254.169.254"], None),
    ] {
        let policy = AddressPolicy::for_addresses(&addresses(&found)?, false);
        assert_eq!(policy.endpoint_class, expected, "{found:?}");
    }
    Ok(())
}

#[test]
fn an_endpoint_is_pinned_to_its_permitted_addresses_with_no_port() -> Result<(), BoxError> {
    for (found, expected) in [
        (vec!["127.0.0.1", "8.8.8.8"], vec!["8.8.8.8"]),
        (vec!["169.254.169.254", "8.8.8.8"], vec!["8.8.8.8"]),
        (vec!["10.0.0.1", "127.0.0.1"], vec!["10.0.0.1"]),
        (vec!["169.254.169.254"], vec![]),
    ] {
        let found = addresses(&found)?;
        let pinned = AddressPolicy::for_addresses(&found, false).pin(&found);
        assert_eq!(
            pinned.iter().map(SocketAddr::ip).collect::<Vec<_>>(),
            addresses(&expected)?
        );
        assert!(pinned.iter().all(|addr| addr.port() == 0), "{pinned:?}");
    }
    Ok(())
}

// A proxy's fake address says nothing about where a name really is, so it opens nothing but other fake addresses.
#[test]
fn an_endpoint_behind_fake_addresses_reaches_no_real_local_network() -> Result<(), BoxError> {
    let policy = AddressPolicy::for_addresses(&addresses(&["198.18.0.1"])?, false);
    assert!(policy.permits_address("198.18.0.2".parse()?));
    assert!(!policy.permits_address("10.0.0.1".parse()?));
    assert!(!policy.permits_address("127.0.0.1".parse()?));
    Ok(())
}

#[test]
fn a_public_endpoint_reaches_only_public_addresses() -> Result<(), BoxError> {
    let policy = policy(None, false);
    assert!(policy.permits_address("8.8.8.8".parse()?));
    for address in ["127.0.0.1", "10.0.0.1", "169.254.169.254", "::1", "fd00::1"] {
        assert!(!policy.permits_address(address.parse()?), "{address}");
    }
    Ok(())
}

#[test]
fn an_endpoint_reaches_its_own_kind_of_address_and_no_other() -> Result<(), BoxError> {
    let private = policy(Some(AddressClass::Private), false);
    assert!(private.permits_address("192.168.1.1".parse()?));
    assert!(private.permits_address("8.8.8.8".parse()?));
    assert!(!private.permits_address("127.0.0.1".parse()?));

    let loopback = policy(Some(AddressClass::Loopback), false);
    assert!(loopback.permits_address("127.0.0.1".parse()?));
    assert!(!loopback.permits_address("10.0.0.1".parse()?));
    Ok(())
}

#[test]
fn no_endpoint_opens_the_way_to_a_reserved_address() -> Result<(), BoxError> {
    let policy = policy(Some(AddressClass::Reserved), false);
    assert!(!policy.permits_address("169.254.169.254".parse()?));
    Ok(())
}

#[test]
fn a_url_is_judged_by_its_scheme_and_any_address_it_names() -> Result<(), BoxError> {
    let open = policy(None, false);
    let secure = policy(None, true);
    assert!(open.permits_url(&Url::parse("http://provider.example/x")?));
    assert!(secure.permits_url(&Url::parse("https://provider.example/x")?));
    assert!(!secure.permits_url(&Url::parse("http://provider.example/x")?));
    for url in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "ftp://provider.example/x",
        "http://127.0.0.1:8080/x",
        "http://[::1]:8080/x",
        "http://[::ffff:127.0.0.1]/x",
        "http://169.254.169.254/latest/meta-data/",
        "http://2130706433/x",
        "http://0x7f.1/x",
    ] {
        assert!(!open.permits_url(&Url::parse(url)?), "{url}");
    }
    Ok(())
}

/// Answers a fixed set of names, so a test never depends on a real resolver.
struct StaticLookup(HashMap<&'static str, Vec<&'static str>>);

impl StaticLookup {
    fn with(entries: &[(&'static str, &[&'static str])]) -> Arc<Self> {
        Arc::new(Self(
            entries.iter().map(|(name, found)| (*name, found.to_vec())).collect(),
        ))
    }
}

impl Resolve for StaticLookup {
    fn resolve(&self, name: Name) -> Resolving {
        let found: Vec<SocketAddr> = self
            .0
            .get(name.as_str())
            .into_iter()
            .flatten()
            .filter_map(|ip| ip.parse::<IpAddr>().ok())
            .map(|ip| SocketAddr::new(ip, 0))
            .collect();
        Box::pin(async move {
            let addrs: Addrs = Box::new(found.into_iter());
            Ok(addrs)
        })
    }
}

async fn guarded(lookup: Arc<StaticLookup>, name: &str) -> Result<Vec<IpAddr>, BoxError> {
    let guard = GuardedLookup {
        policy: Arc::new(policy(None, false)),
        lookup,
    };
    let found = guard
        .resolve(Name::from_str(name)?)
        .await
        .map_err(|source| source.to_string())?;
    Ok(found.map(|addr| addr.ip()).collect())
}

// A name is judged by where it leads as the connection is made, whatever it answered before.
#[tokio::test]
async fn a_connection_is_given_only_the_addresses_it_may_reach() -> Result<(), BoxError> {
    let lookup = StaticLookup::with(&[
        ("public.example", &["8.8.8.8"]),
        ("mixed.example", &["127.0.0.1", "8.8.8.8"]),
        ("internal.example", &["169.254.169.254", "10.0.0.1"]),
    ]);
    let public: IpAddr = "8.8.8.8".parse()?;
    assert_eq!(guarded(lookup.clone(), "public.example").await?, vec![public]);
    assert_eq!(guarded(lookup.clone(), "mixed.example").await?, vec![public]);
    assert!(guarded(lookup, "internal.example").await.is_err());
    Ok(())
}

fn fetcher(lookup: Arc<StaticLookup>, timeout: Duration) -> Result<OauthFetcher, BoxError> {
    Ok(OauthFetcher::with_policy(policy(None, false), lookup, None, timeout)?)
}

#[tokio::test]
async fn a_name_with_any_address_out_of_bounds_is_not_permitted() -> Result<(), BoxError> {
    let lookup = StaticLookup::with(&[
        ("public.example", &["8.8.8.8"]),
        ("mixed.example", &["127.0.0.1", "8.8.8.8"]),
    ]);
    let fetcher = fetcher(lookup, DISCOVERY_TIMEOUT)?;
    assert!(fetcher.permits("https://public.example/token").await);
    assert!(!fetcher.permits("https://mixed.example/token").await);
    assert!(!fetcher.permits("not a url").await);
    assert!(!fetcher.permits("https://unknown.example/token").await);
    Ok(())
}

async fn serve(router: Router) -> Result<String, BoxError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(origin)
}

fn loopback_fetcher(timeout: Duration) -> Result<OauthFetcher, BoxError> {
    Ok(OauthFetcher::with_policy(
        policy(Some(AddressClass::Loopback), false),
        StaticLookup::with(&[]),
        None,
        timeout,
    )?)
}

#[tokio::test]
async fn a_refused_address_is_never_contacted() -> Result<(), BoxError> {
    static HITS: AtomicUsize = AtomicUsize::new(0);
    let origin = serve(Router::new().fallback(|| async {
        HITS.fetch_add(1, Ordering::SeqCst);
        axum::Json(serde_json::json!({}))
    }))
    .await?;
    let fetcher = fetcher(StaticLookup::with(&[]), DISCOVERY_TIMEOUT)?;

    let outcome = fetcher.get_json::<serde_json::Value>(&format!("{origin}/secret")).await;

    assert!(outcome.is_err());
    assert_eq!(HITS.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn a_redirect_to_a_refused_address_is_not_followed() -> Result<(), BoxError> {
    static HITS: AtomicUsize = AtomicUsize::new(0);
    let internal = serve(Router::new().fallback(|| async {
        HITS.fetch_add(1, Ordering::SeqCst);
        axum::Json(serde_json::json!({}))
    }))
    .await?;
    let target = format!("{internal}/secret");
    let origin = serve(Router::new().fallback(move || {
        let target = target.clone();
        async move { (StatusCode::FOUND, [(header::LOCATION, target)]).into_response() }
    }))
    .await?;
    let fetcher = fetcher(StaticLookup::with(&[]), DISCOVERY_TIMEOUT)?;

    // The first request is the one to the configured endpoint, which is not judged, so only the redirect is.
    let followed = fetcher.http().get(format!("{origin}/start")).send().await;

    assert!(followed.is_err());
    assert_eq!(HITS.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn a_chain_of_redirects_ends() -> Result<(), BoxError> {
    static HITS: AtomicUsize = AtomicUsize::new(0);
    let origin = serve(Router::new().fallback(|| async {
        HITS.fetch_add(1, Ordering::SeqCst);
        (StatusCode::FOUND, [(header::LOCATION, "/again")]).into_response()
    }))
    .await?;
    let fetcher = loopback_fetcher(DISCOVERY_TIMEOUT)?;

    assert!(
        fetcher
            .get_json::<serde_json::Value>(&format!("{origin}/start"))
            .await
            .is_err()
    );
    assert_eq!(HITS.load(Ordering::SeqCst), MAX_REDIRECTS + 1);
    Ok(())
}

#[tokio::test]
async fn a_body_past_the_limit_is_refused() -> Result<(), BoxError> {
    let padded = format!("{{}}{}", " ".repeat(MAX_RESPONSE_BYTES));
    let origin = serve(Router::new().fallback(move || {
        let padded = padded.clone();
        async move { padded }
    }))
    .await?;
    let fetcher = loopback_fetcher(DISCOVERY_TIMEOUT)?;

    assert!(
        fetcher
            .get_json::<serde_json::Value>(&format!("{origin}/big"))
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn a_body_within_the_limit_is_read() -> Result<(), BoxError> {
    let origin = serve(Router::new().fallback(|| async { axum::Json(serde_json::json!({"read": true})) })).await?;
    let fetcher = loopback_fetcher(DISCOVERY_TIMEOUT)?;

    let document = fetcher.get_json::<serde_json::Value>(&format!("{origin}/small")).await;

    assert_eq!(document, Ok(serde_json::json!({"read": true})));
    Ok(())
}

#[tokio::test]
async fn a_server_that_never_answers_is_given_up_on() -> Result<(), BoxError> {
    let origin = serve(Router::new().fallback(std::future::pending::<StatusCode>)).await?;
    let fetcher = loopback_fetcher(Duration::from_millis(50))?;

    assert!(
        fetcher
            .get_json::<serde_json::Value>(&format!("{origin}/hang"))
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn an_endpoint_that_is_not_a_url_or_leads_nowhere_usable_has_no_fetcher() {
    let lookup = StaticLookup::with(&[("metadata.example", &["169.254.169.254"])]);
    for endpoint in ["not a url", "http://unknown.example/mcp", "http://metadata.example/mcp"] {
        assert!(
            OauthFetcher::judged(endpoint, lookup.clone(), DISCOVERY_TIMEOUT)
                .await
                .is_none(),
            "{endpoint}"
        );
    }
}

/// Answers one name differently each time, the way a server's own DNS can.
struct ChangingLookup(std::sync::Mutex<Vec<&'static str>>);

impl Resolve for ChangingLookup {
    fn resolve(&self, _name: Name) -> Resolving {
        let next = self.0.lock().ok().and_then(|mut answers| answers.pop());
        let found: Vec<SocketAddr> = next
            .and_then(|ip| ip.parse::<IpAddr>().ok())
            .map(|ip| SocketAddr::new(ip, 0))
            .into_iter()
            .collect();
        Box::pin(async move {
            let addrs: Addrs = Box::new(found.into_iter());
            Ok(addrs)
        })
    }
}

async fn counting_server(hits: &'static AtomicUsize) -> Result<u16, BoxError> {
    let origin = serve(Router::new().fallback(move || async move {
        hits.fetch_add(1, Ordering::SeqCst);
        axum::Json(serde_json::json!({"reached": true}))
    }))
    .await?;
    Ok(Url::parse(&origin)?.port().ok_or("no port")?)
}

// Proves requests really use the resolver: this name is known only to the lookup under test.
#[tokio::test]
async fn a_connection_by_name_uses_the_guarded_lookup() -> Result<(), BoxError> {
    static HITS: AtomicUsize = AtomicUsize::new(0);
    let port = counting_server(&HITS).await?;
    let lookup = StaticLookup::with(&[("service.example", &["127.0.0.1"])]);
    let url = format!("http://service.example:{port}/");

    let refused = fetcher(lookup.clone(), DISCOVERY_TIMEOUT)?;
    assert!(refused.http().get(&url).send().await.is_err());
    assert_eq!(HITS.load(Ordering::SeqCst), 0);

    let permitted = OauthFetcher::with_policy(
        policy(Some(AddressClass::Loopback), false),
        lookup,
        None,
        DISCOVERY_TIMEOUT,
    )?;
    assert!(permitted.http().get(&url).send().await.is_ok());
    assert_eq!(HITS.load(Ordering::SeqCst), 1);
    Ok(())
}

// The first answer sets the policy and the addresses, so a later one cannot lead the same name somewhere else.
#[tokio::test]
async fn the_endpoints_name_stays_at_the_addresses_it_was_judged_by() -> Result<(), BoxError> {
    static HITS: AtomicUsize = AtomicUsize::new(0);
    let port = counting_server(&HITS).await?;
    let lookup = Arc::new(ChangingLookup(std::sync::Mutex::new(vec!["8.8.8.8", "127.0.0.1"])));
    let endpoint = format!("http://mcp.example:{port}/mcp");

    let fetcher = OauthFetcher::judged(&endpoint, lookup, DISCOVERY_TIMEOUT)
        .await
        .ok_or("no fetcher")?;

    assert!(fetcher.permits(&format!("http://mcp.example:{port}/metadata")).await);
    let reached = fetcher
        .get_json::<serde_json::Value>(&format!("http://mcp.example:{port}/metadata"))
        .await;
    assert_eq!(reached, Ok(serde_json::json!({"reached": true})));
    assert_eq!(HITS.load(Ordering::SeqCst), 1);
    Ok(())
}

struct SilentLookup;

impl Resolve for SilentLookup {
    fn resolve(&self, _name: Name) -> Resolving {
        Box::pin(std::future::pending())
    }
}

#[tokio::test(start_paused = true)]
async fn a_name_that_never_resolves_is_given_up_on() -> Result<(), BoxError> {
    let url = Url::parse("http://silent.example/")?;

    assert_eq!(addresses_of(&url, &SilentLookup).await, Vec::<IpAddr>::new());
    Ok(())
}

#[tokio::test]
async fn a_loopback_endpoint_keeps_its_own_kind_of_address() -> Result<(), BoxError> {
    let fetcher = OauthFetcher::for_endpoint("http://127.0.0.1:9/mcp")
        .await
        .ok_or("no fetcher")?;
    assert!(fetcher.permits("http://127.0.0.1:9/.well-known/x").await);
    assert!(!fetcher.permits("http://10.0.0.1/.well-known/x").await);
    assert!(!fetcher.permits("http://169.254.169.254/latest/").await);
    Ok(())
}

#[tokio::test]
async fn an_https_endpoint_is_not_followed_onto_plain_http() -> Result<(), BoxError> {
    for (endpoint_address, other_address) in [("8.8.8.8", "8.8.4.4"), ("10.0.0.1", "10.0.0.2")] {
        let lookup = StaticLookup::with(&[
            ("mcp.example", &[endpoint_address][..]),
            ("other.example", &[other_address][..]),
        ]);
        let fetcher = OauthFetcher::judged("https://mcp.example/mcp", lookup, DISCOVERY_TIMEOUT)
            .await
            .ok_or("no fetcher")?;
        assert!(fetcher.permits("https://mcp.example/token").await, "{endpoint_address}");
        assert!(
            fetcher.permits("https://other.example/token").await,
            "{endpoint_address}"
        );
        assert!(!fetcher.permits("http://mcp.example/token").await, "{endpoint_address}");
        assert!(
            !fetcher.permits("http://other.example/token").await,
            "{endpoint_address}"
        );
    }
    Ok(())
}

// Loopback traffic never leaves the machine, so plain `http` exposes nothing there, whatever the endpoint uses.
#[tokio::test]
async fn an_https_loopback_endpoint_may_use_plain_http_on_loopback() -> Result<(), BoxError> {
    let lookup = StaticLookup::with(&[
        ("dev.example", &["127.0.0.1"][..]),
        ("localhost", &["127.0.0.1", "::1"][..]),
        ("auth.localhost", &["127.0.0.1"][..]),
    ]);
    let fetcher = OauthFetcher::judged("https://dev.example:8443/mcp", lookup, DISCOVERY_TIMEOUT)
        .await
        .ok_or("no fetcher")?;
    for url in [
        "http://127.0.0.1:9000/token",
        "http://[::1]:9000/token",
        "http://localhost:9000/token",
        "http://auth.localhost:9000/token",
        "http://dev.example:9000/token",
    ] {
        assert!(fetcher.permits(url).await, "{url}");
    }
    Ok(())
}

#[tokio::test]
async fn plain_http_stays_on_loopback_for_an_https_loopback_endpoint() -> Result<(), BoxError> {
    let lookup = StaticLookup::with(&[("localhost", &["127.0.0.1"][..]), ("public.example", &["8.8.8.8"][..])]);
    let fetcher = OauthFetcher::judged("https://localhost:8443/mcp", lookup, DISCOVERY_TIMEOUT)
        .await
        .ok_or("no fetcher")?;
    assert!(fetcher.permits("https://public.example/token").await);
    assert!(!fetcher.permits("http://public.example/token").await);
    assert!(!fetcher.permits("http://8.8.8.8/token").await);
    assert!(!fetcher.permits("http://notlocalhost/token").await);
    Ok(())
}

#[tokio::test]
async fn a_localhost_name_leading_off_the_machine_gets_no_plain_http() -> Result<(), BoxError> {
    let lookup = StaticLookup::with(&[("dev.example", &["127.0.0.1"][..]), ("localhost", &["8.8.8.8"][..])]);
    let fetcher = OauthFetcher::judged("https://dev.example/mcp", lookup, DISCOVERY_TIMEOUT)
        .await
        .ok_or("no fetcher")?;
    assert!(!fetcher.permits("http://localhost/token").await);
    Ok(())
}

// A public endpoint gets no such allowance, and cannot reach loopback over either scheme.
#[tokio::test]
async fn a_public_https_endpoint_gets_no_loopback_allowance() -> Result<(), BoxError> {
    let lookup = StaticLookup::with(&[("mcp.example", &["8.8.8.8"][..]), ("localhost", &["127.0.0.1"][..])]);
    let fetcher = OauthFetcher::judged("https://mcp.example/mcp", lookup, DISCOVERY_TIMEOUT)
        .await
        .ok_or("no fetcher")?;
    for url in [
        "http://127.0.0.1/token",
        "http://localhost/token",
        "https://localhost/token",
    ] {
        assert!(!fetcher.permits(url).await, "{url}");
    }
    Ok(())
}
