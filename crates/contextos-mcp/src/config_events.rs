//! `[vault.events]`: how long events are kept.

use serde::Deserialize;

/// How long a vault keeps the events the server writes under `memory/events/`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EventsConfig {
    /// Days an `info` event is kept before the sweep deletes it. A kind's own `retention_days` overrides this for its
    /// events. Zero is refused at load.
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
    /// Whether `warning` and `error` events are kept until deleted by hand. When `false` they age out like `info`
    /// events.
    #[serde(default = "default_keep_attention")]
    pub keep_attention: bool,
}

impl Default for EventsConfig {
    fn default() -> Self {
        Self {
            retention_days: default_retention_days(),
            keep_attention: default_keep_attention(),
        }
    }
}

const fn default_retention_days() -> u32 {
    90
}

const fn default_keep_attention() -> bool {
    true
}
