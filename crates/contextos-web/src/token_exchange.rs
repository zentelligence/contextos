//! Shared RFC 6749 §5.1 token-endpoint HTTP exchange.
//!
//! Both grants this crate performs against a provider's `token_endpoint` — the authorization-code
//! grant (`routes::settings_oauth::exchange_token`) and the refresh-token grant
//! (`oauth_refresh::ensure_fresh`) — post a form body and parse an identically shaped JSON
//! response. This module is the one hand-rolled `reqwest` POST-and-parse implementation both
//! grants call with their own `grant_type` and fields, so the two never drift onto separately
//! maintained error-handling policies.

use serde::Deserialize;

/// The RFC 6749 §5.1 token response shape common to every grant type this crate performs. Every
/// other field a provider's response might carry is ignored.
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

/// Posts `form` to `token_endpoint`, returning `None` on any transport failure, non-2xx response,
/// or unparseable body: every one of those maps to the same "this grant failed" outcome at each
/// call site, so only the diagnostic detail (logged here, never returned to a browser or MCP
/// client) differs between them. `server_name` is carried only for that diagnostic; it never
/// appears in the returned value.
pub async fn request_token(token_endpoint: &str, form: &[(&str, &str)], server_name: &str) -> Option<TokenResponse> {
    let exchange = reqwest::Client::new().post(token_endpoint).form(form).send().await;
    match exchange {
        Ok(response) if response.status().is_success() => response.json::<TokenResponse>().await.ok(),
        Ok(response) => {
            tracing::warn!(
                server = %server_name,
                status = %response.status(),
                "OAuth2 token endpoint request rejected"
            );
            None
        }
        Err(source) => {
            tracing::warn!(server = %server_name, error = %source, "OAuth2 token endpoint request failed");
            None
        }
    }
}

#[cfg(test)]
#[path = "token_exchange_test.rs"]
mod tests;
