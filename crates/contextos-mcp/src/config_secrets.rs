//! The secret-bearing parts of [`Config`](super::Config): a `Debug` that never prints a secret, and the HTTP
//! transport's choice between a token written in the file and one read from a named variable.

use super::{ConfigError, EmbeddingConfig, HttpConfig};

impl std::fmt::Debug for HttpConfig {
    /// Reports whether a token is set, never the token, so a configuration can be logged or asserted on safely.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpConfig")
            .field("bind", &self.bind)
            .field("token", &Redacted(&self.token))
            .field("token_env", &self.token_env)
            .field("max_body_kb", &self.max_body_kb)
            .finish()
    }
}

impl HttpConfig {
    /// Refuses a token written in the file beside a variable naming one: which was meant cannot be known.
    pub(crate) fn check_token_sources(&self) -> Result<(), ConfigError> {
        if !self.token.is_empty() && !self.token_env.is_empty() {
            return Err(ConfigError::HttpTokenConflict);
        }
        Ok(())
    }

    /// The bearer token the HTTP transport requires, empty when none is configured. A token written in the file (or
    /// supplied as `CONTEXTOS_MCP_TOKEN`, which replaces both file fields) is used as it is; otherwise the variable
    /// `token_env` names is read through `lookup`, so only the process about to serve HTTP reads it, from its own
    /// environment.
    ///
    /// # Errors
    ///
    /// [`ConfigError::HttpTokenVariableUnset`] when `token_env` names a variable that is unset or empty: serving
    /// without the token the configuration asks for would silently drop authentication.
    pub fn resolve_token(&self, lookup: impl Fn(&str) -> Option<String>) -> Result<String, ConfigError> {
        if !self.token.is_empty() || self.token_env.is_empty() {
            return Ok(self.token.clone());
        }
        match lookup(&self.token_env) {
            Some(token) if !token.is_empty() => Ok(token),
            _ => Err(ConfigError::HttpTokenVariableUnset {
                variable: self.token_env.clone(),
            }),
        }
    }
}

/// A secret's `Debug`: `"<redacted>"` when it is set, `""` when it is not, never the value.
struct Redacted<'a>(&'a str);

impl std::fmt::Debug for Redacted<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown = if self.0.is_empty() { "" } else { "<redacted>" };
        std::fmt::Debug::fmt(shown, formatter)
    }
}

impl std::fmt::Debug for EmbeddingConfig {
    /// Reports whether a key is written, never the key, so a configuration can be logged or asserted on safely.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmbeddingConfig")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("endpoint", &self.endpoint)
            .field("api_key", &Redacted(&self.api_key))
            .field("api_key_env", &self.api_key_env)
            .field("model_directory", &self.model_directory)
            .finish()
    }
}
