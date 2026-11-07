//! The HTTP this crate uses against `OAuth2` discovery documents and token endpoints, where the address to reach is
//! often chosen by a remote server rather than by the operator.
//!
//! An MCP server's `401` and metadata name further URLs, and fetching them blindly lets that server point this process
//! at whatever it can reach and the server cannot: the operator's loopback services, their network, or a cloud
//! instance's metadata address. [`OauthFetcher`] therefore lets a URL reach a public address, or the same kind of
//! address as the endpoint the operator configured, so a provider on the operator's own network keeps working. It
//! applies this to the addresses a name resolves to as a connection is made, so a name cannot answer one way when
//! checked and another when used.
//!
//! These requests go direct, ignoring any configured proxy, which would resolve names itself and so out of this check's
//! sight. Behind a proxy with no direct route, Detect fails and the endpoints are entered by hand.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use serde::de::DeserializeOwned;

/// The most a discovery document or token response may hold. Each is a small JSON object.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a token request may take, which also bounds a proxied call waiting on a refresh.
pub(crate) const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);

const MAX_REDIRECTS: usize = 5;

/// Cloud instance metadata services outside the link-local range, which addressing alone would class as ordinary.
const METADATA_IPV4: [Ipv4Addr; 2] = [Ipv4Addr::new(100, 100, 100, 200), Ipv4Addr::new(168, 63, 129, 16)];
const METADATA_IPV6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);

/// What kind of network an address belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddressClass {
    Public,
    Private,
    Loopback,
    /// The benchmarking range, which proxies that give out fake addresses use for names that may really be anywhere.
    Translated,
    /// Link-local (which holds the cloud metadata addresses), multicast, documentation, and every other address no
    /// provider is reached at.
    Reserved,
}

impl From<Ipv4Addr> for AddressClass {
    fn from(address: Ipv4Addr) -> Self {
        let [first, second, third, _fourth] = address.octets();
        let shared = first == 100 && (64..=127).contains(&second);
        let reserved = METADATA_IPV4.contains(&address)
            || first == 0
            || first >= 224
            || address.is_link_local()
            || address.is_documentation()
            || (first == 192 && second == 0 && third == 0);
        if address.is_loopback() {
            Self::Loopback
        } else if reserved {
            Self::Reserved
        } else if address.is_private() || shared {
            Self::Private
        } else if first == 198 && (18..=19).contains(&second) {
            Self::Translated
        } else {
            Self::Public
        }
    }
}

impl From<Ipv6Addr> for AddressClass {
    fn from(address: Ipv6Addr) -> Self {
        let segments = address.segments();
        let embedded = |high: u16, low: u16| {
            let [a, b] = high.to_be_bytes();
            let [c, d] = low.to_be_bytes();
            Self::from(Ipv4Addr::new(a, b, c, d))
        };
        match segments {
            _ if address.is_loopback() => Self::Loopback,
            _ if address == METADATA_IPV6 => Self::Reserved,
            // NAT64 and 6to4 lead to the IPv4 address inside them.
            [0x0064, 0xff9b, 0, 0, 0, 0, high, low] | [0x2002, high, low, ..] => embedded(high, low),
            [first, ..] if first & 0xfe00 == 0xfc00 => Self::Private,
            [0, ..] | [0x2001, 0x0db8, ..] => Self::Reserved,
            [first, ..] if first & 0xff00 == 0xff00 || first & 0xffc0 == 0xfe80 || first & 0xffc0 == 0xfec0 => {
                Self::Reserved
            }
            _ => Self::Public,
        }
    }
}

impl From<IpAddr> for AddressClass {
    fn from(address: IpAddr) -> Self {
        match address.to_canonical() {
            IpAddr::V4(address) => address.into(),
            IpAddr::V6(address) => address.into(),
        }
    }
}

/// What a URL named by a remote server may reach, judged against the endpoint the operator configured.
#[derive(Debug)]
struct AddressPolicy {
    /// The local class a URL may reach besides public addresses, when the endpoint is on one.
    endpoint_class: Option<AddressClass>,
    /// The name in the endpoint's URL, when it has one.
    endpoint_domain: Option<String>,
    /// An `https` endpoint is not followed onto plain `http`, except on loopback, where nothing leaves the machine.
    https_only: bool,
}

impl AddressPolicy {
    /// The policy for an endpoint at `addresses`. The name's owner chooses these, so a public address among them grants
    /// nothing local: only a name with no public answer, whose owner is then never contacted, counts as local.
    fn for_addresses(addresses: &[IpAddr], https_only: bool) -> Self {
        let classes: Vec<AddressClass> = addresses
            .iter()
            .map(|address| AddressClass::from(*address))
            .filter(|class| *class != AddressClass::Reserved)
            .collect();
        let local = !classes.contains(&AddressClass::Public);
        Self {
            endpoint_class: classes.first().copied().filter(|_class| local),
            endpoint_domain: None,
            https_only,
        }
    }

    /// The addresses of `addresses` an endpoint's name may be reached at. A port here would override the one each URL
    /// asks for, so it says nothing.
    fn pin(&self, addresses: &[IpAddr]) -> Vec<SocketAddr> {
        addresses
            .iter()
            .filter(|address| self.permits_address(**address))
            .map(|address| SocketAddr::new(*address, 0))
            .collect()
    }

    fn permits_address(&self, address: IpAddr) -> bool {
        match AddressClass::from(address) {
            AddressClass::Public => true,
            AddressClass::Reserved => false,
            class => self.endpoint_class == Some(class),
        }
    }

    /// Whether a loopback endpoint names a `url` that is on loopback too: an address, `localhost` or a name under it, or
    /// the endpoint's own name, which is held to its loopback addresses.
    fn stays_on_loopback(&self, url: &Url) -> bool {
        let local_name = url.domain().is_some_and(|domain| {
            domain == "localhost" || domain.ends_with(".localhost") || Some(domain) == self.endpoint_domain.as_deref()
        });
        self.endpoint_class == Some(AddressClass::Loopback)
            && (local_name || literal_address(url).is_some_and(|ip| AddressClass::from(ip) == AddressClass::Loopback))
    }

    /// Whether `url`'s scheme is acceptable and, when its host is an address rather than a name, that address is.
    fn permits_url(&self, url: &Url) -> bool {
        let scheme =
            url.scheme() == "https" || (url.scheme() == "http" && (!self.https_only || self.stays_on_loopback(url)));
        scheme && (url.domain().is_some() || literal_address(url).is_some_and(|ip| self.permits_address(ip)))
    }
}

/// The address `url` names directly, when its host is an address rather than a name.
fn literal_address(url: &Url) -> Option<IpAddr> {
    url.host_str()?.trim_matches(['[', ']']).parse().ok()
}

/// The operating system's name lookup.
struct SystemLookup;

impl Resolve for SystemLookup {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let found = tokio::net::lookup_host((name.as_str(), 0)).await?;
            let addrs: Addrs = Box::new(found.collect::<Vec<_>>().into_iter());
            Ok(addrs)
        })
    }
}

/// A resolver that hands a connection only the addresses [`AddressPolicy`] permits.
struct GuardedLookup {
    policy: Arc<AddressPolicy>,
    lookup: Arc<dyn Resolve>,
}

impl Resolve for GuardedLookup {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = Arc::clone(&self.policy);
        let found = self.lookup.resolve(name);
        Box::pin(async move {
            let permitted: Vec<SocketAddr> = found.await?.filter(|addr| policy.permits_address(addr.ip())).collect();
            if permitted.is_empty() {
                return Err("the name resolves to no address this request may reach".into());
            }
            let addrs: Addrs = Box::new(permitted.into_iter());
            Ok(addrs)
        })
    }
}

/// Every address `url` leads to. Empty when it names none, or its name does not resolve in time.
async fn addresses_of(url: &Url, lookup: &dyn Resolve) -> Vec<IpAddr> {
    if let Some(address) = literal_address(url) {
        return vec![address];
    }
    let Some(name) = url.domain().and_then(|domain| Name::from_str(domain).ok()) else {
        return Vec::new();
    };
    match tokio::time::timeout(DISCOVERY_TIMEOUT, lookup.resolve(name)).await {
        Ok(Ok(found)) => found.map(|addr| addr.ip()).collect(),
        Ok(Err(_)) | Err(_) => Vec::new(),
    }
}

/// Reaches what a remote server names, within what the operator's endpoint allows.
pub(crate) struct OauthFetcher {
    http: reqwest::Client,
    policy: Arc<AddressPolicy>,
    lookup: Arc<dyn Resolve>,
    /// The endpoint's own name, whose addresses were judged once and are the only ones it is reached at.
    pinned_domain: Option<String>,
}

impl OauthFetcher {
    /// A fetcher judged against `endpoint`, or `None` when it is not a URL, or names no address a request may reach.
    pub(crate) async fn for_endpoint(endpoint: &str) -> Option<Self> {
        Self::judged(endpoint, Arc::new(SystemLookup), DISCOVERY_TIMEOUT).await
    }

    /// Requests to `endpoint`'s own name go only to the permitted addresses judged here, so the name cannot be judged
    /// local and then answer from somewhere else, or lead anywhere the policy refuses. An address written in the URL
    /// is the operator's own choice and goes as written.
    async fn judged(endpoint: &str, lookup: Arc<dyn Resolve>, timeout: Duration) -> Option<Self> {
        let url = Url::parse(endpoint).ok()?;
        let addresses = addresses_of(&url, lookup.as_ref()).await;
        let mut policy = AddressPolicy::for_addresses(&addresses, url.scheme() == "https");
        policy.endpoint_domain = url.domain().map(str::to_owned);
        let pinned = match url.domain() {
            Some(domain) => {
                let addrs = policy.pin(&addresses);
                if addrs.is_empty() {
                    return None;
                }
                Some((domain.to_owned(), addrs))
            }
            None if addresses.is_empty() => return None,
            None => None,
        };
        Self::with_policy(policy, lookup, pinned, timeout).ok()
    }

    /// A fetcher for an endpoint on a public address, so a test's own loopback stub counts as out of bounds.
    #[cfg(test)]
    pub(crate) fn public_only() -> Result<Self, reqwest::Error> {
        let policy = AddressPolicy {
            endpoint_class: None,
            endpoint_domain: None,
            https_only: false,
        };
        Self::with_policy(policy, Arc::new(SystemLookup), None, DISCOVERY_TIMEOUT)
    }

    fn with_policy(
        policy: AddressPolicy,
        lookup: Arc<dyn Resolve>,
        pinned: Option<(String, Vec<SocketAddr>)>,
        timeout: Duration,
    ) -> Result<Self, reqwest::Error> {
        let policy = Arc::new(policy);
        let redirects = Arc::clone(&policy);
        let mut builder = reqwest::Client::builder()
            .timeout(timeout)
            .no_proxy()
            .dns_resolver(Arc::new(GuardedLookup {
                policy: Arc::clone(&policy),
                lookup: Arc::clone(&lookup),
            }))
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() > MAX_REDIRECTS {
                    attempt.error("too many redirects")
                } else if redirects.permits_url(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.error("redirected to an address this request may not reach")
                }
            }));
        if let Some((domain, addrs)) = &pinned {
            builder = builder.resolve_to_addrs(domain, addrs);
        }
        Ok(Self {
            http: builder.build()?,
            policy,
            lookup,
            pinned_domain: pinned.map(|(domain, _addrs)| domain),
        })
    }

    /// The client for the request to the configured endpoint itself, which the operator chose and so needs no check.
    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Whether a URL a remote server named may be used: fetched here, or offered to the operator as an endpoint.
    pub(crate) async fn permits(&self, url: &str) -> bool {
        let Ok(parsed) = Url::parse(url) else {
            return false;
        };
        if !self.policy.permits_url(&parsed) {
            return false;
        }
        if parsed.domain().is_some() && parsed.domain() == self.pinned_domain.as_deref() {
            return true;
        }
        let addresses = addresses_of(&parsed, self.lookup.as_ref()).await;
        // A name allowed plain `http` for being on loopback has to lead there.
        let loopback_only = self.policy.https_only && parsed.scheme() == "http";
        !addresses.is_empty()
            && addresses.into_iter().all(|address| {
                self.policy.permits_address(address)
                    && (!loopback_only || AddressClass::from(address) == AddressClass::Loopback)
            })
    }

    /// `GET url`, collapsing a refused address, transport failure, non-2xx status, oversized body, or body `T` cannot
    /// be read from into one `Err`: a best-effort assist treats them alike.
    pub(crate) async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, ()> {
        if !self.permits(url).await {
            tracing::info!("auth detection refused a URL the server named, because of the address it leads to");
            return Err(());
        }
        let response = self.http.get(url).send().await.map_err(|_source| ())?;
        if !response.status().is_success() {
            return Err(());
        }
        let body = bounded_body(response).await.ok_or(())?;
        serde_json::from_slice(&body).map_err(|_source| ())
    }
}

/// `response`'s body, or `None` when it fails or passes [`MAX_RESPONSE_BYTES`]. Stops reading at the limit, so a server
/// cannot fill this process's memory by never ending its body.
pub(crate) async fn bounded_body(mut response: reqwest::Response) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

#[cfg(test)]
#[path = "oauth_http_test.rs"]
mod tests;
