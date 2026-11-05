//! The operator's local time for every date contextos-web writes into a vault: a new note's file name and template
//! dates, the journal's "today", a task's or decision's date. The zone is the one the contextos server reports in
//! `vault_info` (its `[server] timezone`, else its host's zone), so the web dates a note exactly as the server stamps
//! it, never in UTC and never in the browser's zone: by its IANA name, or, for a host zone with none, by the POSIX
//! rules the server reports. The server is asked once and its answer kept for the life of the process. Should it not
//! be reachable, this host's own zone stands in for that request and the server is asked again next time, so a
//! transient failure is never remembered; should it answer with a zone this process cannot build, this host's zone
//! is kept and the mismatch logged once.

use std::sync::Arc;

use contextos_core::{OperatorTimeZone, ReportedTimeZone};
use contextos_workflows::Clock;
use std::sync::OnceLock;
use time::{Date, OffsetDateTime};

use tokio::sync::OnceCell;

use crate::mcp_client::McpClientSet;
use crate::vault_info::ReportedZone;

/// How long the server is given to report its zone before this host's stands in for that reading.
const ZONE_QUESTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// What [`VaultTime`] is built from.
pub struct VaultTimeInput {
    /// The instant source; fixed in a test.
    pub clock: Arc<dyn Clock>,
    /// The sessions whose primary server, the contextos server, reports the zone the web follows.
    pub clients: Arc<McpClientSet>,
}

/// Local time in the zone the contextos server reports.
pub struct VaultTime {
    clock: Arc<dyn Clock>,
    clients: Arc<McpClientSet>,
    zone: OnceCell<OperatorTimeZone>,
    /// This host's own zone, detected once, for a reading taken before the server's zone is known.
    host_zone: OnceLock<OperatorTimeZone>,
}

impl From<VaultTimeInput> for VaultTime {
    fn from(input: VaultTimeInput) -> Self {
        Self {
            clock: input.clock,
            clients: input.clients,
            zone: OnceCell::new(),
            host_zone: OnceLock::new(),
        }
    }
}

impl VaultTime {
    /// The local time for `clients`' contextos server, read from the system clock, with its zone already resolved
    /// (or, for a server that does not answer in time, this host's standing in until it does).
    pub async fn warmed(clients: Arc<McpClientSet>) -> Arc<Self> {
        let vault_time = Self::unwarmed(clients);
        vault_time.warm().await;
        vault_time
    }

    /// The local time for `clients`' contextos server, read from the system clock, its zone resolved on the first
    /// asynchronous reading or [`Self::warm`], so nothing waits on the server to build it.
    #[must_use]
    pub fn unwarmed(clients: Arc<McpClientSet>) -> Arc<Self> {
        Arc::new(Self::from(VaultTimeInput {
            clock: Arc::new(contextos_workflows::SystemClock),
            clients,
        }))
    }

    /// The current instant at the operator's local offset.
    pub async fn instant(&self) -> OffsetDateTime {
        self.zone().await.localise(self.clock.now())
    }

    /// The operator's local date.
    pub async fn today(&self) -> Date {
        self.instant().await.date()
    }

    /// Resolves the server's zone now, so a later synchronous reading ([`Clock::now`]) is already in it. Called
    /// once at startup by everything that reads this clock synchronously.
    pub async fn warm(&self) {
        let _ = self.zone().await;
    }

    async fn zone(&self) -> OperatorTimeZone {
        if let Some(zone) = self.zone.get() {
            return zone.clone();
        }
        let Some(reported) = self.server_zone().await else {
            return self.host_zone().clone();
        };
        let zone = match OperatorTimeZone::try_from(ReportedTimeZone {
            name: reported.name.as_deref(),
            rules: reported.rules.as_deref(),
        }) {
            Ok(zone) => zone,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "the contextos server's time zone cannot be used here; dating vault writes in this host's zone, \
                     which may differ: set [server] timezone in its config.toml"
                );
                self.host_zone().clone()
            }
        };
        self.zone.get_or_init(|| async { zone }).await.clone()
    }

    fn host_zone(&self) -> &OperatorTimeZone {
        self.host_zone.get_or_init(|| OperatorTimeZone::from(None))
    }

    /// The zone the server reports, or `None` when it cannot be asked, or does not answer within
    /// [`ZONE_QUESTION_TIMEOUT`]: a hung server never holds up a page or a run, and is asked again next time.
    async fn server_zone(&self) -> Option<ReportedZone> {
        let client = self.clients.primary()?;
        tokio::time::timeout(ZONE_QUESTION_TIMEOUT, client.vault_info())
            .await
            .ok()?
            .ok()?
            .time_zone
    }
}

/// The same local time read synchronously, for the workflow runtime (run records, claims, ledgers, and a script's
/// own clock): the server's zone once [`VaultTime::warm`] or any asynchronous reading has resolved it, this host's
/// zone before then. An instant is the same whichever offset it is shown at, so timing, ordering, and cron
/// evaluation (which normalises to UTC) are unaffected; only how the instant is written changes.
impl Clock for VaultTime {
    fn now(&self) -> OffsetDateTime {
        self.localise(self.clock.now())
    }

    fn time_zone(&self) -> OperatorTimeZone {
        self.zone.get().unwrap_or_else(|| self.host_zone()).clone()
    }

    fn localise(&self, instant: OffsetDateTime) -> OffsetDateTime {
        self.zone.get().unwrap_or_else(|| self.host_zone()).localise(instant)
    }
}

impl std::fmt::Debug for VaultTime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VaultTime")
            .field("zone", &self.zone.get())
            .finish_non_exhaustive()
    }
}

/// The system clock, shown in the zone another clock reports in: for a workflow run's own engine, whose timeouts must
/// follow real time whatever clock its host is given, while the times the run writes are still the operator's.
pub struct LocalisedSystemClock {
    /// Supplies only the zone ([`Clock::localise`]); never read for the instant itself.
    pub zone: Arc<dyn Clock>,
}

impl Clock for LocalisedSystemClock {
    fn now(&self) -> OffsetDateTime {
        self.zone.localise(contextos_workflows::SystemClock.now())
    }

    fn time_zone(&self) -> OperatorTimeZone {
        self.zone.time_zone()
    }

    fn localise(&self, instant: OffsetDateTime) -> OffsetDateTime {
        self.zone.localise(instant)
    }
}
