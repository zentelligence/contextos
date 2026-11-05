use crate::{OperationEvent, OperationWarning, Origin, ProjectionScope, VaultRootId};

/// Port that dispatches a completed mutation to non-primary services.
pub trait RoutesOperations: Send + Sync {
    /// Returns every non-fatal service warning in dispatch order.
    fn route(&self, event: &OperationEvent) -> Vec<OperationWarning>;

    /// Routes each event of a batch in order and returns them with every warning raised.
    ///
    /// The way for a service that writes on its own account (a whole-vault index rebuild, a manual log entry, a
    /// flush of buffered log entries) to hand every write it made to Git and search: a caller that must supply a
    /// router cannot drop the events, and cannot lose the warnings either.
    fn route_all(&self, events: Vec<OperationEvent>) -> RoutedEvents {
        let warnings = events.iter().flat_map(|event| self.route(event)).collect();
        RoutedEvents { events, warnings }
    }
}

/// Events a service wrote on its own account, all of which have been routed, with the warnings routing raised.
#[derive(Clone, Debug)]
pub struct RoutedEvents {
    pub events: Vec<OperationEvent>,
    pub warnings: Vec<OperationWarning>,
}

/// Routing adapter used before substrate services are configured.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoSubstrateServices;

impl RoutesOperations for NoSubstrateServices {
    fn route(&self, _event: &OperationEvent) -> Vec<OperationWarning> {
        Vec::new()
    }
}

/// Port for managed `index.md` reconciliation after a completed mutation.
pub trait MaintainsIndexes: Send + Sync {
    /// Reconciles directories affected by the event.
    ///
    /// # Errors
    ///
    /// Returns a non-fatal warning when reconciliation must be healed later.
    fn reconcile(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning>;
}

/// Port for append-only operation logging after a completed mutation.
pub trait LogsOperations: Send + Sync {
    /// Appends one compatible operation-log entry.
    ///
    /// # Errors
    ///
    /// Returns a non-fatal warning when the entry must be retried later.
    fn append(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning>;
}

/// Port for staging completed operations in local version control.
pub trait VersionsVault: Send + Sync {
    /// Stages only the paths owned by this operation.
    ///
    /// # Errors
    ///
    /// Returns a non-fatal warning when versioning is degraded.
    fn stage(&self, event: &OperationEvent) -> Result<(), OperationWarning>;
}

/// Port for incremental search-index updates after a completed mutation.
///
/// Search receives tool and internal events alike: managed index blocks and
/// operation-log entries are vault markdown, so they remain searchable.
pub trait UpdatesSearch: Send + Sync {
    /// Applies the event to the derived search state.
    ///
    /// # Errors
    ///
    /// Returns a non-fatal warning when search state must be healed later.
    fn update(&self, event: &OperationEvent) -> Result<(), OperationWarning>;

    /// Applies a batch of events in order.
    ///
    /// The default applies them one by one, attempting every event and reporting the first failure. An
    /// implementation whose storage commits per call (a text index, say) should override this to commit once per
    /// run of compatible changes, which is what keeps a whole-vault rebuild from costing one commit per file.
    ///
    /// # Errors
    ///
    /// Returns the first non-fatal warning when search state must be healed later.
    fn update_all(&self, events: &[OperationEvent]) -> Result<(), OperationWarning> {
        let mut first = None;
        for event in events {
            if let Err(warning) = self.update(event) {
                first.get_or_insert(warning);
            }
        }
        first.map_or(Ok(()), Err)
    }
}

/// Search adapter used before search services are configured.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoSearchUpdates;

impl UpdatesSearch for NoSearchUpdates {
    fn update(&self, _event: &OperationEvent) -> Result<(), OperationWarning> {
        Ok(())
    }
}

/// Port for re-projecting agent definitions into agent hosts after a completed mutation.
///
/// Implementations re-project every definition the event touched into each enabled agent host, and return every
/// non-fatal warning they raise. One event may warn once per host, so the result can hold several warnings.
pub trait ProjectsDefinitions: Send + Sync {
    /// Re-projects the definitions the event touched and returns every non-fatal warning.
    fn project(&self, event: &OperationEvent) -> Vec<OperationWarning>;
}

/// Projection adapter used when no agent host is configured: it projects nothing and never warns.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoProjection;

impl ProjectsDefinitions for NoProjection {
    fn project(&self, _event: &OperationEvent) -> Vec<OperationWarning> {
        Vec::new()
    }
}

/// A warning with the vault whose write raised it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopedWarning {
    pub warning: OperationWarning,
    /// The vault of the event that raised the warning. `None` for a warning raised once for the whole batch, such as
    /// the search update, which belongs to every vault the batch wrote to.
    pub root: Option<VaultRootId>,
}

/// A service a batch was delivered to, with the vault it was delivered for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScopedService {
    pub service: OperationService,
    /// The vault of the event the service was given. `None` for a service given the whole batch (search).
    pub root: Option<VaultRootId>,
}

/// What one routed batch looked like once every other service had finished with it, handed to [`RecordsEvents`].
///
/// A batch can write to more than one vault (a move between vaults, a delete naming paths in several). Warnings and
/// attempted services are therefore recorded with the vault they concern; read them for one vault through
/// [`Self::warnings_for`] and [`Self::attempted_for`], which also include what applies to the whole batch.
#[derive(Clone, Copy, Debug)]
pub struct EventsBatch<'a> {
    /// The batch's tool-origin events, in arrival order. Internal events never appear.
    pub events: &'a [OperationEvent],
    /// Every warning the other services raised for the batch, in dispatch order.
    pub warnings: &'a [ScopedWarning],
    /// The services the batch was delivered to, in dispatch order and without repeats. A service the routing policy
    /// or the batch's paths ruled out is absent, so a consumer can tell "did not warn" from "was not asked".
    pub attempted: &'a [ScopedService],
}

impl EventsBatch<'_> {
    /// The warnings raised by writes to `root`, and those raised once for the whole batch, in dispatch order.
    #[must_use]
    pub fn warnings_for(&self, root: VaultRootId) -> Vec<OperationWarning> {
        self.warnings
            .iter()
            .filter(|scoped| scoped.root.is_none_or(|owner| owner == root))
            .map(|scoped| scoped.warning.clone())
            .collect()
    }

    /// The services delivered an event of `root`, and those delivered the whole batch, in dispatch order.
    #[must_use]
    pub fn attempted_for(&self, root: VaultRootId) -> Vec<OperationService> {
        let mut services = Vec::new();
        for scoped in self
            .attempted
            .iter()
            .filter(|scoped| scoped.root.is_none_or(|owner| owner == root))
        {
            if !services.contains(&scoped.service) {
                services.push(scoped.service);
            }
        }
        services
    }
}

/// What a [`RecordsEvents`] consumer wrote and what went wrong while writing it.
#[derive(Clone, Debug, Default)]
pub struct EventsOutcome {
    /// The internal events for the notes the consumer wrote. The router stages them in Git, as it does the events a
    /// service derives, and offers them to nothing else.
    pub events: Vec<OperationEvent>,
    /// Non-fatal failures of the consumer itself, reported after every other warning of the batch.
    pub warnings: Vec<OperationWarning>,
}

/// Port for recording vault events about a completed batch of tool writes.
///
/// Called once per batch, after every other service (search included), and only when the batch holds at least one
/// tool-origin event: an internal write never raises an event, so the notes the consumer writes cannot raise more.
pub trait RecordsEvents: Send + Sync {
    /// Records whatever events the batch warrants and returns the internal events for the notes written.
    fn record(&self, batch: &EventsBatch<'_>) -> EventsOutcome;
}

/// Events adapter used when events are not configured: it records nothing and never warns.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoEvents;

impl RecordsEvents for NoEvents {
    fn record(&self, _batch: &EventsBatch<'_>) -> EventsOutcome {
        EventsOutcome::default()
    }
}

/// A downstream service class that may observe a completed mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationService {
    Index,
    OperationLog,
    Git,
    Search,
    /// Agent host projection: derived data, so, like Git, it follows internal writes as well as tool writes.
    Projection,
    /// Vault events: raised only by tool writes, so an event note can never raise another event.
    Events,
}

/// Immutable routing policy derived from an operation's trusted origin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationRoute {
    kind: OperationRouteKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationRouteKind {
    Tool,
    Internal,
}

impl From<&Origin> for OperationRoute {
    fn from(value: &Origin) -> Self {
        match value {
            Origin::Tool(_) => Self {
                kind: OperationRouteKind::Tool,
            },
            Origin::Internal(_) => Self {
                kind: OperationRouteKind::Internal,
            },
        }
    }
}

impl OperationRoute {
    /// Reports whether this route delivers to the selected service class.
    #[must_use]
    pub const fn includes(self, service: OperationService) -> bool {
        match (self.kind, service) {
            (
                OperationRouteKind::Internal,
                OperationService::Index | OperationService::OperationLog | OperationService::Events,
            ) => false,
            (
                OperationRouteKind::Tool | OperationRouteKind::Internal,
                OperationService::Index
                | OperationService::OperationLog
                | OperationService::Git
                | OperationService::Search
                | OperationService::Projection
                | OperationService::Events,
            ) => true,
        }
    }
}

/// Trusted dependencies for routing completed operation events.
#[derive(Clone, Debug)]
pub struct OperationRouterConfig<I, L, V, S, P = NoProjection, E = NoEvents> {
    pub indexes: I,
    pub operation_log: L,
    pub versions: V,
    pub search: S,
    pub projection: P,
    pub events: E,
}

/// Central dispatcher that makes internal index/log recursion impossible.
#[derive(Clone, Debug)]
pub struct OperationRouter<I, L, V, S, P = NoProjection, E = NoEvents> {
    indexes: I,
    operation_log: L,
    versions: V,
    search: S,
    projection: P,
    events: E,
}

impl<I, L, V, S, P, E> From<OperationRouterConfig<I, L, V, S, P, E>> for OperationRouter<I, L, V, S, P, E> {
    fn from(value: OperationRouterConfig<I, L, V, S, P, E>) -> Self {
        Self {
            indexes: value.indexes,
            operation_log: value.operation_log,
            versions: value.versions,
            search: value.search,
            projection: value.projection,
            events: value.events,
        }
    }
}

impl<I, L, V, S, P, E> OperationRouter<I, L, V, S, P, E>
where
    I: MaintainsIndexes,
    L: LogsOperations,
    V: VersionsVault,
    S: UpdatesSearch,
    P: ProjectsDefinitions,
    E: RecordsEvents,
{
    /// Delivers an event to its allowed services and collects non-fatal failures.
    #[must_use]
    pub fn route(&self, event: &OperationEvent) -> Vec<OperationWarning> {
        self.route_batch(std::slice::from_ref(event))
    }

    /// Delivers a batch of events, in order, and collects non-fatal failures.
    ///
    /// Every event is routed to the index, operation log, and Git exactly as a single event is. Search, whose storage
    /// commits per call, instead receives all of the batch's events (and every event they caused) together, in
    /// arrival order, so a whole-vault rebuild costs one commit rather than one per file.
    #[must_use]
    pub fn route_batch(&self, events: &[OperationEvent]) -> Vec<OperationWarning> {
        let mut collected = Collected::default();
        let mut for_search = Vec::new();
        for event in events {
            let route = OperationRoute::from(&event.origin);
            let mut internal_events = Vec::new();

            if route.includes(OperationService::Index) {
                collected.attempt(event, OperationService::Index);
                match self.indexes.reconcile(event) {
                    Ok(events) => internal_events.extend(events),
                    Err(warning) => collected.warn(event, warning),
                }
            }
            if route.includes(OperationService::OperationLog) {
                collected.attempt(event, OperationService::OperationLog);
                match self.operation_log.append(event) {
                    Ok(events) => internal_events.extend(events),
                    Err(warning) => collected.warn(event, warning),
                }
            }
            if route.includes(OperationService::Git) {
                collected.attempt(event, OperationService::Git);
                self.stage_per_vault(event, &mut collected);
            }
            self.project_definitions(event, route, &mut collected);
            if route.includes(OperationService::Search) {
                for_search.push(event.clone());
            }
            for internal_event in internal_events {
                self.stage_per_vault(&internal_event, &mut collected);
                self.project_definitions(
                    &internal_event,
                    OperationRoute::from(&internal_event.origin),
                    &mut collected,
                );
                for_search.push(internal_event);
            }
        }
        if !for_search.is_empty() {
            collected.attempt_batch(OperationService::Search);
            if let Err(warning) = self.search.update_all(&for_search) {
                collected.warn_batch(warning);
            }
        }
        let raised = self.record_events(events, &collected);
        collected.flat.extend(raised);
        collected.flat
    }

    /// Stages the event in Git once for each vault it touches, so a failure is attributed to the vault whose
    /// repository failed and one failing repository does not leave another unstaged.
    fn stage_per_vault(&self, event: &OperationEvent, collected: &mut Collected) {
        let roots = distinct_roots(event);
        if roots.len() <= 1 {
            if let Err(warning) = self.versions.stage(event) {
                collected.warn(event, warning);
            }
            return;
        }
        for root in roots {
            let mut scoped = event.clone();
            scoped.paths.retain(|path| path.root_id() == root);
            if let Err(warning) = self.versions.stage(&scoped) {
                collected.warn(&scoped, warning);
            }
        }
    }

    /// Hands the batch's tool-origin events to the events consumer, after every other service has finished.
    ///
    /// The notes the consumer wrote are staged in Git, so one commit holds the write and the events it raised. The
    /// consumer is not called when the batch holds no tool-origin event, which is what keeps an event note from
    /// raising an event.
    fn record_events(&self, events: &[OperationEvent], collected: &Collected) -> Vec<OperationWarning> {
        let tool_events: Vec<OperationEvent> = events
            .iter()
            .filter(|event| OperationRoute::from(&event.origin).includes(OperationService::Events))
            .cloned()
            .collect();
        if tool_events.is_empty() {
            return Vec::new();
        }
        let outcome = self.events.record(&EventsBatch {
            events: &tool_events,
            warnings: &collected.scoped,
            attempted: &collected.attempted,
        });
        let mut raised = Vec::new();
        for event in &outcome.events {
            if let Err(warning) = self.versions.stage(event) {
                raised.push(warning);
            }
        }
        raised.extend(outcome.warnings);
        raised
    }

    /// Hands the event to projection when its route allows it and any of its paths is a projected definition.
    ///
    /// A move names two paths but is still one event, so projection sees it once and re-projects both ends.
    fn project_definitions(&self, event: &OperationEvent, route: OperationRoute, collected: &mut Collected) {
        let touches_definitions = event.paths.iter().any(|path| ProjectionScope::try_from(path).is_ok());
        if route.includes(OperationService::Projection) && touches_definitions {
            collected.attempt(event, OperationService::Projection);
            for warning in self.projection.project(event) {
                collected.warn(event, warning);
            }
        }
    }
}

/// The vaults an event touches, in path order and without repeats.
fn distinct_roots(event: &OperationEvent) -> Vec<VaultRootId> {
    let mut roots = Vec::new();
    for path in &event.paths {
        let root = path.root_id();
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

/// What a batch's services raised and reached, kept flat for the caller and by vault for the events consumer.
#[derive(Default)]
struct Collected {
    flat: Vec<OperationWarning>,
    scoped: Vec<ScopedWarning>,
    attempted: Vec<ScopedService>,
}

impl Collected {
    /// Records a warning against each vault the event touches, or against the whole batch when it names no path.
    fn warn(&mut self, event: &OperationEvent, warning: OperationWarning) {
        self.flat.push(warning.clone());
        let roots = distinct_roots(event);
        if roots.is_empty() {
            self.scoped.push(ScopedWarning { warning, root: None });
            return;
        }
        for root in roots {
            self.scoped.push(ScopedWarning {
                warning: warning.clone(),
                root: Some(root),
            });
        }
    }

    /// Records a warning raised once for the whole batch.
    fn warn_batch(&mut self, warning: OperationWarning) {
        self.flat.push(warning.clone());
        self.scoped.push(ScopedWarning { warning, root: None });
    }

    /// Notes that the service was given the event, for each vault the event touches.
    fn attempt(&mut self, event: &OperationEvent, service: OperationService) {
        let roots = distinct_roots(event);
        if roots.is_empty() {
            self.attempt_batch(service);
        }
        for root in roots {
            self.note(ScopedService {
                service,
                root: Some(root),
            });
        }
    }

    /// Notes that the service was given the whole batch.
    fn attempt_batch(&mut self, service: OperationService) {
        self.note(ScopedService { service, root: None });
    }

    fn note(&mut self, scoped: ScopedService) {
        if !self.attempted.contains(&scoped) {
            self.attempted.push(scoped);
        }
    }
}

impl<I, L, V, S, P, E> RoutesOperations for OperationRouter<I, L, V, S, P, E>
where
    I: MaintainsIndexes,
    L: LogsOperations,
    V: VersionsVault,
    S: UpdatesSearch,
    P: ProjectsDefinitions,
    E: RecordsEvents,
{
    fn route(&self, event: &OperationEvent) -> Vec<OperationWarning> {
        Self::route(self, event)
    }

    fn route_all(&self, events: Vec<OperationEvent>) -> RoutedEvents {
        let warnings = self.route_batch(&events);
        RoutedEvents { events, warnings }
    }
}
